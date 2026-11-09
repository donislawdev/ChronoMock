//! Job objects a launch puts its process in (R4-S1, R4-N15, R4-N30).
//!
//! A job can end every process in it when its last handle closes, and the kernel closes a process's
//! handles however that process ends. That makes a job the one tie between a process we start and our
//! own life that still holds when we are killed: a killed process runs nothing on its way out, so no
//! code of ours could do the same job.
//!
//! Two uses. A target is in a job only while it is being started (`prepare`): a core that dies while
//! the target is still suspended takes it along, and once the target runs the job lets go of it, so a
//! core that dies later leaves the application running as ADR-14 promises. The job starts with the
//! target and holds nothing else, because its children are let out of it (`SILENT_BREAKAWAY_OK`). A
//! process started without the hook (a probe host, a browser in the CDP mode) lives in its job for
//! good, with everything it starts, and ends with it.
//!
//! Measured before this was written (`tools/probes/r4-8`): a job the core itself runs in keeps the
//! target and its children exactly as it did without ours, whichever breakaway it allows.

use std::time::Instant;

use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, HANDLE};
use windows::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject, JOBOBJECT_BASIC_LIMIT_INFORMATION,
    JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE};

use crate::win32_detail;

/// How many times the process list is asked for, each time with room for the count the last answer
/// gave. A job whose processes multiply faster than that is listed in part.
const LIST_TRIES: usize = 4;

/// A job object this process made, closed when the value drops. While it is still set to end its
/// processes, the drop ends them.
pub(crate) struct Job(HANDLE);

// SAFETY: a job handle is a kernel object, usable from any thread.
unsafe impl Send for Job {}

impl Job {
    /// A job that ends every process in it when its last handle closes. With `children_stay_out`, the
    /// processes in it start their children outside it.
    pub(crate) fn ending_with_us(children_stay_out: bool) -> Result<Job, String> {
        // SAFETY: a new unnamed job, owned by the value from here on, so every way out closes it.
        let job = Job(unsafe { CreateJobObjectW(None, None) }.map_err(|e| win32_detail("CreateJobObjectW", &e))?);
        let stay_out = if children_stay_out { JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK } else { JOB_OBJECT_LIMIT(0) };
        job.set_limits(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | stay_out)?;
        Ok(job)
    }

    /// The handle, for a launch to name the job the new process starts in.
    pub(crate) fn handle(&self) -> HANDLE {
        self.0
    }

    /// Stop ending the processes in the job, then close it. They run on in a job that holds no limit
    /// they could notice, and their children stay out of it as before. When the job cannot be changed it
    /// comes back unclosed, because closing it would end them.
    pub(crate) fn let_go(self) -> Result<(), (Job, String)> {
        match self.set_limits(JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) {
            Ok(()) => Ok(()),
            Err(why) => Err((self, why)),
        }
    }

    /// End every process in the job now. A job already empty is not an error here.
    pub(crate) fn end_all(&self, code: u32) {
        // SAFETY: the handle is ours until drop.
        unsafe {
            let _ = TerminateJobObject(self.0, code);
        }
    }

    /// The processes in the job and in the jobs nested in it now, each opened to be waited on, and how
    /// many of its processes could not be: one the system would not open, or all of them when it would
    /// not list them. A pid from the list can name another process by the time it is opened - one that
    /// ended and gave its number away - so a process is kept only when the system says it is in this
    /// job, and one that has ended by then is not there to wait for.
    pub(crate) fn members(&self) -> (Vec<Member>, usize) {
        let Some(pids) = self.process_ids() else {
            return (Vec::new(), 1);
        };
        let mut held = Vec::new();
        let mut unconfirmed = 0;
        for pid in pids {
            match self.open_member(pid) {
                Opened::Member(member) => held.push(member),
                Opened::NotHere => {}
                Opened::Refused => unconfirmed += 1,
            }
        }
        (held, unconfirmed)
    }

    /// The pids the job lists now. `None` when the system would not list them.
    fn process_ids(&self) -> Option<Vec<u32>> {
        let header = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList);
        let mut room = 64usize;
        for _ in 0..LIST_TRIES {
            let bytes = header + room * size_of::<usize>();
            let length = u32::try_from(bytes).ok()?;
            // Eight-byte units, so the structure is aligned on either bitness.
            let mut buffer = vec![0u64; bytes.div_ceil(8)];
            let list = buffer.as_mut_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
            // SAFETY: a zeroed buffer of `length` bytes, aligned for the structure, written during the call.
            let asked = unsafe { QueryInformationJobObject(Some(self.0), JobObjectBasicProcessIdList, list.cast(), length, None) };
            // SAFETY: the header lies inside the buffer, and holds zeros where the call wrote nothing.
            let (assigned, listed) = unsafe { ((*list).NumberOfAssignedProcesses, (*list).NumberOfProcessIdsInList) };
            if listed < assigned {
                room = assigned as usize + 16;
                continue;
            }
            if asked.is_err() {
                return None;
            }
            // SAFETY: `listed` ids follow the header, and the call wrote no more than the room it was given.
            let ids = unsafe {
                std::slice::from_raw_parts(list.cast::<u8>().add(header).cast::<usize>(), (listed as usize).min(room))
            };
            return Some(ids.iter().filter_map(|&id| u32::try_from(id).ok()).collect());
        }
        None
    }

    /// The process `pid`, opened to be waited on, when it is in this job.
    fn open_member(&self, pid: u32) -> Opened {
        // SAFETY: a handle that opens belongs to the Member from the next statement, so every way out closes it.
        let handle = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(handle) => handle,
            // No process under that number any more - it ended (also what the system answers for pid 0).
            Err(e) if e.code() == ERROR_INVALID_PARAMETER.to_hresult() => return Opened::NotHere,
            Err(_) => return Opened::Refused,
        };
        let member = Member { handle };
        let mut inside = BOOL(0);
        // SAFETY: both handles are open, and the answer is written during the call only.
        if unsafe { IsProcessInJob(member.handle, Some(self.0), &raw mut inside) }.is_err() {
            return Opened::Refused;
        }
        if inside.as_bool() { Opened::Member(member) } else { Opened::NotHere }
    }

    fn set_limits(&self, limits: JOB_OBJECT_LIMIT) -> Result<(), String> {
        let info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION { LimitFlags: limits, ..Default::default() },
            ..Default::default()
        };
        // SAFETY: the structure the information class names, at its own size, read during the call only.
        unsafe {
            SetInformationJobObject(
                self.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(|e| win32_detail("SetInformationJobObject", &e))
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateJobObjectW and is closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// What opening a pid from the job's list gave.
enum Opened {
    /// A process of the job, held to be waited on.
    Member(Member),
    /// No process of the job under that number: it ended, or the number is another process's now.
    NotHere,
    /// A process the system would not open or place in a job - it cannot be confirmed to have ended.
    Refused,
}

/// A process of a job, opened to be waited on, closed when the value drops.
pub(crate) struct Member {
    handle: HANDLE,
}

impl Member {
    /// Whether the process has ended by `deadline`, waiting for it until then. Its handles are closed
    /// by the time it counts as ended, so the files it held are free.
    pub(crate) fn ended_by(&self, deadline: Instant) -> bool {
        !crate::ending::waits_out(self.handle, deadline)
    }
}

impl Drop for Member {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess and is closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Opened;

    /// The job lists the process a launcher handed its work to and not the launcher, which has ended:
    /// `cmd.exe` that starts `PING.EXE` with `start` and ends at once, the shape of an installed
    /// Chromium browser.
    #[test]
    fn the_job_lists_the_process_a_launcher_handed_over_to_and_not_the_launcher() {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let path = format!(r"{system_root}\System32\cmd.exe");
        let ping = format!(r"{system_root}\System32\PING.EXE");
        let args = ["/c".to_string(), "start".into(), "\"\"".into(), "/b".into(), ping, "-n".into(), "30".into(), "127.0.0.1".into()];
        let target = crate::Target { path: &path, args: &args, cwd: None, env: &[], stdio: crate::TargetStdio::Discarded };
        let child = crate::launch_plain(&target).expect("cmd.exe launches");
        assert!(child.wait_exit(5_000).is_some(), "cmd.exe hands over and ends");
        let handed_to = crate::tree::tests::started_by(child.pid, "PING.EXE").expect("cmd.exe started ping within five seconds");

        let listed = child.job.as_ref().expect("the child runs in a job").process_ids();
        child.terminate();
        let listed = listed.expect("the system lists the job's processes");
        assert!(listed.contains(&handed_to), "the job lists the process it handed over to: {listed:?}");
        assert!(!listed.contains(&child.pid), "and not the launcher, which has ended: {listed:?}");
    }

    /// A pid from the job's list can name another process by the time it is opened, so a member is a
    /// process the system says is in the job: the child is, and this test's own process, which is not
    /// in the job of the child it started, is not there. Neither is a number no process has - pid 0,
    /// which the system never opens - and neither is waited for or counted as unconfirmed.
    #[test]
    fn only_a_process_in_the_job_is_a_member() {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let path = format!(r"{system_root}\System32\PING.EXE");
        let args = ["-n".to_string(), "30".into(), "127.0.0.1".into()];
        let target = crate::Target { path: &path, args: &args, cwd: None, env: &[], stdio: crate::TargetStdio::Discarded };
        let child = crate::launch_plain(&target).expect("ping launches");
        let job = child.job.as_ref().expect("the child runs in a job");
        assert!(matches!(job.open_member(child.pid), Opened::Member(_)), "the child is in its job");
        assert!(matches!(job.open_member(std::process::id()), Opened::NotHere), "this test's own process is not");
        assert!(matches!(job.open_member(0), Opened::NotHere), "a number no process has is not there");
        let (members, unconfirmed) = job.members();
        assert_eq!((members.len(), unconfirmed), (1, 0), "the child alone, confirmed");
        child.terminate();
    }
}