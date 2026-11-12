//! What `prepare` holds between creating a session and handing it back (R5-A6).
//!
//! `prepare` used to release these by hand on each of its six ways out - unmap the view, close the
//! mapping, end the still-suspended target, close its two handles - and every new resource meant a
//! new line on every one of them. A way out that missed one left a target suspended for good, holding
//! its executable and the hook library open, or a view of the control block mapped in the core. Here
//! each owner releases what it holds when it goes out of scope, so a way out is a plain `return Err`,
//! and the session takes the resources over only once nothing can fail any more.

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{UnmapViewOfFile, MEMORY_MAPPED_VIEW_ADDRESS};
use windows::Win32::System::Threading::{TerminateProcess, PROCESS_INFORMATION};

use chrono_ctl::Ctl;

use crate::JobOrWhy;

/// The control block's mapping and this process's view of it.
pub(crate) struct ControlView {
    hmap: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl ControlView {
    /// Take over a mapping and the view made of it. Neither may be released by anyone else from here.
    pub(crate) fn new(hmap: HANDLE, view: MEMORY_MAPPED_VIEW_ADDRESS) -> Self {
        ControlView { hmap, view }
    }

    /// The block, as the view maps it.
    pub(crate) fn ctl(&self) -> *mut Ctl {
        self.view.Value as *mut Ctl
    }

    /// Hand the view's address and the mapping to the session, which releases them when it ends.
    pub(crate) fn into_session(self) -> (usize, HANDLE) {
        let held = ManuallyDrop::new(self);
        (held.view.Value as usize, held.hmap)
    }
}

impl Drop for ControlView {
    fn drop(&mut self) {
        // SAFETY: both came from `prepare` and are released exactly once, here, in the order the
        // view needs: the view goes before the mapping it was made of.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.hmap);
        }
    }
}

/// A target created suspended, whose program has not run - its main thread is suspended, or parked
/// before the entry point once it has started the process (ADR-25) - with the job it was started in.
///
/// Dropped before it runs, it is ended: letting it go on would run an application that reads the real
/// clock as a session that substituted nothing (H-2), and leaving it held would keep its executable
/// and the hook library open for good. Ending it costs the tester nothing, since its program never
/// ran.
pub(crate) struct SuspendedTarget {
    pi: PROCESS_INFORMATION,
    job: JobOrWhy,
}

impl SuspendedTarget {
    pub(crate) fn new(pi: PROCESS_INFORMATION, job: JobOrWhy) -> Self {
        SuspendedTarget { pi, job }
    }

    pub(crate) fn process(&self) -> HANDLE {
        self.pi.hProcess
    }

    pub(crate) fn thread(&self) -> HANDLE {
        self.pi.hThread
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pi.dwProcessId
    }

    /// The target runs now: hand back its handles and its job, which nothing here ends any more.
    pub(crate) fn into_running(self) -> (PROCESS_INFORMATION, JobOrWhy) {
        let held = ManuallyDrop::new(self);
        // SAFETY: `held` is never dropped, so the job is moved out of it exactly once.
        (held.pi, unsafe { std::ptr::read(&held.job) })
    }
}

impl Drop for SuspendedTarget {
    fn drop(&mut self) {
        // SAFETY: the handles came from the create call and are released exactly once, here. The job
        // is dropped after this body, as a field, so the process is ended by name before its job closes.
        unsafe {
            let _ = TerminateProcess(self.pi.hProcess, 1);
            let _ = CloseHandle(self.pi.hThread);
            let _ = CloseHandle(self.pi.hProcess);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{
        DuplicateHandle, DUPLICATE_SAME_ACCESS, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFileEx, VirtualQuery, FILE_MAP_ALL_ACCESS, MEMORY_BASIC_INFORMATION, MEM_COMMIT,
        MEM_MAPPED, PAGE_READWRITE,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, WaitForSingleObject, CREATE_SUSPENDED,
    };

    /// A ping started suspended, and a second handle to it that the guard does not own, so the test
    /// can still look at the process after the guard is gone. Started outside any job: the job would
    /// end it on its own, and the guard is what is under test.
    fn suspended_ping() -> (SuspendedTarget, HANDLE) {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let path = format!(r"{system_root}\System32\PING.EXE");
        let args = ["-n".to_string(), "30".into(), "127.0.0.1".into()];
        let target = crate::Target { path: &path, args: &args, cwd: None, env: &[], stdio: crate::TargetStdio::Discarded };
        let (app, line) = crate::launch_line(&target).expect("a command line");
        // SAFETY: the buffers come from `launch_line` for this target, and the handles are owned below.
        unsafe {
            let pi = crate::create_process(&app, &line, &target, CREATE_SUSPENDED, None).expect("ping starts");
            let mut watch = HANDLE::default();
            DuplicateHandle(GetCurrentProcess(), pi.hProcess, GetCurrentProcess(), &mut watch, 0, false, DUPLICATE_SAME_ACCESS)
                .expect("a second handle");
            (SuspendedTarget::new(pi, Err("no job in this test".into())), watch)
        }
    }

    /// A target dropped before it ran is ended, with the code the start-up refusals have always used. When
    /// the guard fails, the test ends the target itself, so a red run leaves no suspended process behind.
    #[test]
    fn a_target_dropped_before_it_runs_is_ended() {
        let (suspended, watch) = suspended_ping();
        drop(suspended);
        // SAFETY: `watch` is this test's own handle, closed at the end.
        unsafe {
            let ended = WaitForSingleObject(watch, 5_000);
            if ended != WAIT_OBJECT_0 {
                let _ = TerminateProcess(watch, 0);
            }
            assert_eq!(ended, WAIT_OBJECT_0, "the target outlived its guard");
            let mut code = 0u32;
            GetExitCodeProcess(watch, &mut code).expect("an exit code");
            assert_eq!(code, 1);
            let _ = CloseHandle(watch);
        }
    }

    /// A target handed over is left alone: `into_running` disarms the guard.
    #[test]
    fn a_target_handed_over_is_left_running() {
        let (suspended, watch) = suspended_ping();
        let (pi, _job) = suspended.into_running();
        // SAFETY: every handle here is this test's own, and each is closed once.
        unsafe {
            assert_eq!(WaitForSingleObject(watch, 300), WAIT_TIMEOUT, "the guard ended a target it handed over");
            let _ = TerminateProcess(pi.hProcess, 0);
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
            let _ = CloseHandle(watch);
        }
    }

    /// A value no fresh view holds, different in every run. A fresh view reads as zeros, so it is never zero.
    fn marker() -> u64 {
        use std::hash::{BuildHasher, RandomState};
        RandomState::new().hash_one(0u8) | 1
    }

    /// Whether this process still maps, at `address`, the view `marker` was written into. The question is
    /// put to the view, not to the address: the tests of this binary map memory in parallel, and memory one
    /// of them took there after ours was gone made an address query answer "mapped" in CI.
    fn holds(address: *const std::ffi::c_void, marker: u64) -> bool {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: a query about an address, which reads nothing at it.
        let n = unsafe { VirtualQuery(Some(address), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
        // Only a committed mapped view, which is what ours was, is read, so the test never touches a
        // stack or a heap another test is using.
        if n == 0 || info.State != MEM_COMMIT || info.Type != MEM_MAPPED {
            return false;
        }
        let mut found = 0u64;
        // SAFETY: the read goes through the kernel, which reports a page gone since the query as an error
        // instead of faulting, into a local of the size asked for.
        let read = unsafe {
            ReadProcessMemory(GetCurrentProcess(), address, (&raw mut found).cast(), size_of::<u64>(), None)
        };
        read.is_ok() && found == marker
    }

    /// A view of a fresh one-page section, where the system places it, or at `at`. `None` when `at` is taken.
    fn page_view(at: Option<*const std::ffi::c_void>) -> Option<ControlView> {
        // SAFETY: an anonymous section of one page, owned by the guard from here.
        unsafe {
            let hmap = CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, 4096, None).expect("a section");
            let view = MapViewOfFileEx(hmap, FILE_MAP_ALL_ACCESS, 0, 0, 4096, at);
            if view.Value.is_null() {
                let _ = CloseHandle(hmap);
                return None;
            }
            Some(ControlView::new(hmap, view))
        }
    }

    /// A view with `marker` written at its start, and its address.
    fn marked_view(marker: u64) -> (ControlView, *const std::ffi::c_void) {
        let view = page_view(None).expect("a view");
        let address = view.ctl() as *const std::ffi::c_void;
        // SAFETY: the view is mapped, writable and a page long.
        unsafe { address.cast_mut().cast::<u64>().write(marker) };
        (view, address)
    }

    /// How many views the test drops, at most, before one of them gets a new view in its place.
    const PLACE_TRIES: usize = 16;

    /// A view dropped on a way out is unmapped, and one handed to the session stays. Before the question
    /// about the dropped one, a new view is made at its address, the way a test running beside this one
    /// can make one, so the question meets that case in every run and not only when CI happens on it. A
    /// test beside this one can take the address first, and then the try is repeated with a new view.
    #[test]
    fn a_view_is_unmapped_unless_the_session_takes_it() {
        let marker = marker();
        let replaced = (0..PLACE_TRIES).any(|_| {
            let (dropped, address) = marked_view(marker);
            assert!(holds(address, marker), "the question does not see a view that is there");
            drop(dropped);
            let in_its_place = page_view(Some(address));
            assert!(!holds(address, marker), "the view outlived its guard");
            in_its_place.is_some()
        });
        assert!(replaced, "no try in {PLACE_TRIES} put a new view in a dropped one's place");

        let (kept, _) = marked_view(marker);
        let (address, hmap) = kept.into_session();
        assert!(holds(address as *const std::ffi::c_void, marker), "the guard unmapped a view it handed over");
        // SAFETY: the test owns both now, as the session would.
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: address as *mut std::ffi::c_void });
            let _ = CloseHandle(hmap);
        }
    }
}
