//! Chrono Mock mechanism layer - the only place that touches the OS and the target
//! process. This is the rewrite of the throwaway spike injector/hook plumbing as
//! product code.
//!
//! Stage 4: substitute the full set of wall-clock channels and report the session
//! zone. `prepare` creates the session control memory, computes the fake anchor and
//! the session zone bias from the moment, launches the target SUSPENDED (so the hook
//! is installed before the target's entry point runs), injects the hook DLL, reads back
//! which channels were covered, then resumes. One thing runs before the hook all the
//! same (R4-N20): the remote thread that loads it does the target's own loading first,
//! so the start-up code of the target's static imports (`DllMain`) and its TLS
//! callbacks read the real clock. The README says so under its limits.
//!
//! The tool injects its OWN probes on the host - injecting into third-party or system
//! processes stays on the VM or requires explicit consent.

mod batch;
mod ending;
mod environment;
mod family;
mod handoff;
mod held;
mod host_zone;
mod job;
mod listeners;
mod loader;
mod policy;
mod policy_value;
mod process_facts;
#[cfg(test)]
mod search;
mod stdio;
mod tree;

pub use batch::{batch_launch_problem, is_batch_script};
pub use ending::FamilyEnd;
pub use environment::{current_environment, environment_block};
pub use family::FamilyMember;
pub use handoff::RunningCopy;
pub use host_zone::{host_zone_name, host_zone_sample, ZoneSample};
pub use listeners::{listening_sockets, Listener, ListenerTable, IPV4_ANY_ADDR, IPV4_LOOPBACK_ADDR};
pub use policy::webview2_arguments_policy_present;
pub use policy_value::{
    recover_stale, set_for_session, stale_value_present, PolicyRemoval, PolicySet, PolicyValue, Recovery,
};
pub use process_facts::{
    process_elevated, process_has_any_module, process_has_module, process_image_name, process_image_path, ModuleProbe,
};
pub use stdio::TargetStdio;
pub use tree::{descendants_of, family_of};

use std::collections::HashSet;
use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

use chrono_core::{ChannelCoverage, Coverage, SessionSpec, TimeMode};
use chrono_ctl::{
    cov_at, ctl_size, header_is_ours, read_anchor, read_calls,
    find_pid_slot, read_core_pid, read_created, read_dur, read_ended, read_failed_channels, read_installed,
    read_late_installed, read_pid,
    read_pid_count, read_subsystem, rebase, SUBSYSTEM_CONSOLE,
    read_uncovered_child, read_uncovered_children_count, read_uninjected_children, read_waits_at_floor,
    read_timers_at_floor, read_absolute_dues, read_calls_without_storage, calls_capped, clock_fence,
    mark_ended, write_anchor, write_anchor_full, write_core_created, write_header, FullAnchor,
    write_core_pid, write_scale_dur, write_scale_qpc, write_tz_bias, ChannelCategory,
    Cov, Ctl, CHANNELS, CH_GTC, CH_GTC64, CTL_LAYOUT_VERSION, CTL_MAGIC, DEADLINE_CHANNELS, IDX_TIMEGETTIME,
    MAX_COV_PIDS,
};
// The bound on the remote `LoadLibraryW` thread is the one the hook keeps for a child (R5-A7): a thread
// still stuck past it means the target's loader deadlocked, an injection failure rather than a
// `prepare` that hangs forever (M-1).
use chrono_ctl::inject::INJECT_TIMEOUT_MS;
use windows::core::{s, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, FILETIME, HANDLE, INVALID_HANDLE_VALUE, UNICODE_STRING,
    WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualAllocEx,
    VirtualFreeEx, VirtualQuery, FILE_MAP_ALL_ACCESS, FILE_MAP_READ, MEMORY_BASIC_INFORMATION,
    MEMORY_MAPPED_VIEW_ADDRESS, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::SystemInformation::{
    GetSystemTimeAsFileTime, GetTickCount64, IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64,
    IMAGE_FILE_MACHINE_ARM64, IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_UNKNOWN,
};
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
use windows::Win32::System::Threading::{
    CreateMutexW, CreateProcessW, CreateRemoteThread, GetCurrentProcess, GetCurrentProcessId,
    GetExitCodeProcess, GetExitCodeThread, GetProcessTimes, IsWow64Process2, OpenProcess,
    QueryFullProcessImageNameW,
    ResumeThread, TerminateProcess, WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    LPTHREAD_START_ROUTINE,
    PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE,
};
use windows::Win32::System::Performance::QueryPerformanceCounter;
use windows::Win32::System::WindowsProgramming::{
    QueryInterruptTime, QueryUnbiasedInterruptTime, QueryUnbiasedInterruptTimePrecise,
};

/// What to launch.
pub struct Target<'a> {
    pub path: &'a str,
    pub args: &'a [String],
    pub cwd: Option<&'a str>,
    /// Variables the target gets on top of this process's environment (a name already present is
    /// replaced). Empty means the child inherits the environment untouched, through a null
    /// `lpEnvironment` - byte for byte what every session did before the embedded-engine channel
    /// (docs/09) had two variables to add. Non-empty means a block built by `environment_block`.
    pub env: &'a [(String, String)],
    /// The standard handles it starts with (R4-W1, ADR-17) - never this process's stdin or stdout.
    pub stdio: TargetStdio,
}

/// Why preparing a session failed. Each variant carries a message from the point of
/// origin (zasady/06 section 9) - the caller maps it to a protocol error key.
#[derive(Debug)]
pub enum PrepareError {
    /// The moment or its zone cannot be a session clock (`chrono_core::session_instant`).
    Moment(chrono_core::SessionMomentError),
    Control(String),
    Launch(String),
    Inject(String),
    /// The target ended while Windows was loading it, before its entry point, with this exit
    /// code - the loader's status when a static import was missing, of the other bitness, lacked a
    /// function or would not initialise (R4-S6). Nothing about the hook is known: it never got its turn.
    EndedLoading(u32),
    /// Another session's core (this pid) is already running - single-session limit (fixed section
    /// name). The caller refuses rather than sharing one control block between two sessions.
    SessionActive(u32),
    /// The target runs at a different bitness than this core, so `CreateRemoteThread` +
    /// `LoadLibraryW` cannot reach it - a known impossibility, declared before the attempt rather
    /// than after (R2-S1). Carries the two machine labels, target first.
    BitnessMismatch(&'static str, &'static str),
}

/// The outcome of `prepare`: the parent's audit coverage plus the live session.
pub struct Prepared {
    /// The PARENT process's coverage (its pid is `session.pid`), each covered channel
    /// with a live call count sampled shortly after resume. Children that join later
    /// (ADR-3) are reported separately via `Session::poll_new_coverage`, each with its
    /// OWN pid and counts - never summed into this one.
    pub coverage: Coverage,
    /// The running session: read its clocks, poll child coverage, check the target, end it.
    pub session: Session,
    /// Set when the target exited within the guard window right after injection - a
    /// suspected single-instance vanish (ADR-4). Carries how long it lived, in ms.
    pub vanished_lived_ms: Option<u64>,
    /// Whether a previous session's control block was still there and was taken over, and how that
    /// session had ended. The caller surfaces this so the takeover is not silent.
    pub reclaimed: Reclaimed,
    /// What went wrong with the job the target was started in (R4-S1), for the caller to say. `None`
    /// on every ordinary launch.
    pub start_job_note: Option<String>,
}

/// What `prepare` found under the control block's name before it started.
///
/// A block survives its core for as long as any process of the session keeps it mapped. It used to be
/// reported as "a previous core had died" whatever had happened, which was false after every session
/// that ended in order with its application still running (R4-W2, measured in `tools/probes/r4-6`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reclaimed {
    /// Nothing survived: this session made a fresh block.
    Nothing,
    /// The previous session had ended in order, and a process of it still held the block - an
    /// application that session left running, on the real clock since its end (ADR-14).
    EndedSession,
    /// The previous core died without ending its session.
    DeadCore,
}

/// A live, running session. Keeps the control memory mapped and the target handle
/// open so the core can read state (and later re-anchor) until the session ends.
pub struct Session {
    pub pid: u32,
    ctl_addr: usize,
    hmap: HANDLE,
    hprocess: HANDLE,
    /// The real (QUIT) anchor captured at session start, so real elapsed stays measured from the start
    /// even after a later re-anchor. The fake side no longer has a matching field: it is integrated
    /// below rather than read off the wall anchor, which is what a jump used to corrupt.
    start_real: i64,
    /// Fake time elapsed BEFORE the current rate segment, in 100 ns ticks, plus the real instant that
    /// segment began. Together they make "how much fake time this session has run" an integral of the
    /// rate over real time - which is what the phrase means, and what the CDP clock already computed.
    /// Reading it off the wall anchor instead made a backward jump report NEGATIVE elapsed (the anchor
    /// moves, the start does not), and that number goes into `ended`, the session report and the
    /// exported evidence. A jump deliberately does not touch these: it moves the wall, it does not
    /// unspend time already spent.
    fake_elapsed_before: std::cell::Cell<i64>,
    rate_segment_real0: std::cell::Cell<i64>,
    /// The sleep bias when the session started (100 ns), so `state` can say how long the machine has
    /// slept since (R5-P1).
    sleep_bias0: i64,
    tz_bias: i32,
    /// The duration axis is opt-in - coverage gathering needs it to know whether the
    /// Duration channels are expected.
    scale_duration: bool,
    /// The QPC axis is a SEPARATE opt-in from the duration axis, so coverage gathering needs its own
    /// flag to know whether the QueryPerformanceCounter channel is expected (R2-S3).
    scale_qpc: bool,
    /// Which registry slots have already been reported, so `poll_new_coverage` emits each process
    /// exactly once. Indexed by SLOT, not by pid: Windows recycles pids, and two processes in one
    /// session can carry the same one - keyed by pid, the second would have been silently swallowed
    /// as a duplicate. The coverage itself needs no bookkeeping here, since it lives in the control
    /// block this session already holds mapped and so outlives every process that writes it (S-9).
    reported_slots: Vec<bool>,
    /// How many entries of each slot's `uncovered_children` ring have been handed out already, so
    /// `poll_uncovered_children` names each child exactly once. Indexed by SLOT like `reported_slots`,
    /// and for the same reason. An entry claimed but not yet written (pid 0) stops the walk for that
    /// slot until the next poll - the writer fills it within a few instructions of claiming it.
    consumed_children: Vec<u32>,
    /// The other processes on the session clock, watched so the session lasts as long as any of them
    /// runs and not only as long as the one it launched (ADR-16).
    family: family::Family,
    /// The full path of the launched process's executable, read while it runs: once it has ended the
    /// system may no longer say (ADR-4, 2026-11-11). `None` when it would not say even then.
    root_image: Option<String>,
    /// The session lock, held for as long as the session lives. Dropped last, so a second core
    /// cannot start until this one has released the control block it was using.
    _lock: SessionLock,
}

/// A process this session's family spawned and the hook did not follow into: it ran on the real
/// clock for as long as it lived, whatever it read. `image` is the executable's file name and
/// `command_line` what it was started with, both when the child was still there to ask and `None`
/// when it had already gone. The command line is raw text from the target's world - the core reads
/// the one token it needs out of it and never forwards the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncoveredChild {
    pub pid: u32,
    pub parent_pid: u32,
    pub image: Option<String>,
    pub command_line: Option<String>,
}

/// A process nothing reported, named the way the hook's own unfollowed children are: its image and
/// command line as they stand now, `None` for whatever a process that has gone will not say. The
/// session names the processes of an elevated WebView2 host's engine this way, because the hook never
/// saw them start (docs/09 section 12.19).
pub fn name_unhooked(pid: u32, parent_pid: u32) -> UncoveredChild {
    let (image, command_line) = describe_process(pid);
    UncoveredChild { pid, parent_pid, image, command_line }
}

/// The file name of a running process's executable and its command line, or `(None, None)` when
/// the process cannot be opened - which is the ordinary outcome for a child that has already exited,
/// and the honest one for a process this user's token may not query. Limited-information access is
/// enough for both and is granted for a lower-integrity child (a sandboxed renderer) by a
/// medium-integrity parent. One open for the two questions, because the answers describe the same
/// instant: a child found alive for its name is asked for its command line in the same breath.
fn describe_process(pid: u32) -> (Option<String>, Option<String>) {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return (None, None);
        };
        let image = image_name_of(h);
        let command_line = command_line_of(h);
        let _ = CloseHandle(h);
        (image, command_line)
    }
}

/// # Safety
/// `h` must be an open process handle with at least limited query access.
unsafe fn image_name_of(h: HANDLE) -> Option<String> { unsafe {
    let full = image_path_of(h)?;
    let name = full.rsplit(['\\', '/']).next().unwrap_or(&full);
    Some(name.to_string())
}}

/// # Safety
/// `h` must be an open process handle with at least limited query access.
pub(crate) unsafe fn image_path_of(h: HANDLE) -> Option<String> { unsafe {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut len).ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}}

/// `NtQueryInformationProcess` as ntdll exports it. Resolved by name at the call, like the hook
/// resolves the Nt entry points it detours: the function is documented, the information class used
/// below is not, and a build must not fail on either.
type NtQueryInformationProcessFn =
    unsafe extern "system" fn(HANDLE, u32, *mut c_void, u32, *mut u32) -> i32;

/// `ProcessCommandLineInformation`: the information class that answers with the process's command
/// line as a `UNICODE_STRING` whose buffer follows it in the same block. Not on the documented list
/// for `NtQueryInformationProcess` - it is the phnt / ReactOS number, an assessment rather than a
/// source (zasady/03 section 4), chosen over the documented road because that road reads the PEB
/// of another process and a 32-bit core cannot read a 64-bit PEB. The failure mode of a wrong number
/// is a status the kernel returns, never a fault: an unknown class is `STATUS_INVALID_INFO_CLASS`,
/// a short buffer `STATUS_INFO_LENGTH_MISMATCH`, and both read here as "no command line".
const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;

/// The command line the process was started with, or `None` when the system will not say (an
/// unknown class, a command line longer than the buffer, a process that is gone). The buffer is
/// sized for the longest command line Windows allows plus the string header, so the second case is
/// a hard ceiling rather than an ordinary one.
///
/// # Safety
/// `h` must be an open process handle with at least limited query access.
unsafe fn command_line_of(h: HANDLE) -> Option<String> { unsafe {
    let ntdll = GetModuleHandleA(s!("ntdll.dll")).ok()?;
    let entry = GetProcAddress(ntdll, s!("NtQueryInformationProcess"))?;
    let query: NtQueryInformationProcessFn = std::mem::transmute(entry);
    // The longest command line Windows accepts is 32 767 UTF-16 units. The header is the
    // UNICODE_STRING, then the characters. Aligned to the header by being a Vec of u64.
    const HEADER: usize = std::mem::size_of::<UNICODE_STRING>();
    let bytes = HEADER + 32_768 * 2;
    let mut block: Vec<u64> = vec![0; bytes.div_ceil(8)];
    let base = block.as_mut_ptr() as *mut u8;
    let mut returned: u32 = 0;
    let status = query(h, PROCESS_COMMAND_LINE_INFORMATION, base as *mut c_void, bytes as u32, &mut returned);
    if status < 0 {
        return None;
    }
    let header = std::ptr::read_unaligned(base as *const UNICODE_STRING);
    let start = header.Buffer.0 as *const u8;
    let len = header.Length as usize;
    // The string buffer has to lie inside the block the kernel filled - anything else is not a
    // command line we read, whatever the header claims.
    let first = base.add(HEADER) as usize;
    let end = base.add(bytes) as usize;
    let s = start as usize;
    if s < first || s.checked_add(len)? > end {
        return None;
    }
    let units: Vec<u16> = (0..len / 2).map(|i| std::ptr::read_unaligned((start as *const u16).add(i))).collect();
    Some(String::from_utf16_lossy(&units))
}}

/// `PROCESS_BASIC_INFORMATION` as Microsoft Learn lays it out for `NtQueryInformationProcess` with
/// `ProcessBasicInformation` (class 0): `InheritedFromUniqueProcessId` is the parent process.
#[repr(C)]
struct ProcessBasicInformation {
    exit_status: i32,
    peb_base_address: *mut c_void,
    affinity_mask: usize,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
}

/// The pid of the process that started `pid` - its declared parent - or `None` when it cannot be
/// asked (the process is gone, or does not grant even a limited query). One open and one query, with
/// no snapshot of the system's processes: a question asked of one process, about one process.
pub fn process_parent(pid: u32) -> Option<u32> {
    // SAFETY: the handle is closed on every path out, the entry point is checked before it is called,
    // and the buffer is the structure the information class writes, at its own size.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let parent = parent_of(process);
        let _ = CloseHandle(process);
        parent
    }
}

/// # Safety
/// `h` must be an open process handle with at least limited query access.
unsafe fn parent_of(h: HANDLE) -> Option<u32> { unsafe {
    let ntdll = GetModuleHandleA(s!("ntdll.dll")).ok()?;
    let entry = GetProcAddress(ntdll, s!("NtQueryInformationProcess"))?;
    let query: NtQueryInformationProcessFn = std::mem::transmute(entry);
    let mut info = std::mem::MaybeUninit::<ProcessBasicInformation>::zeroed();
    let size = std::mem::size_of::<ProcessBasicInformation>() as u32;
    let mut returned = 0u32;
    if query(h, 0, info.as_mut_ptr().cast(), size, &mut returned) < 0 {
        return None;
    }
    u32::try_from(info.assume_init().inherited_from_unique_process_id).ok()
}}

/// Both clocks at one instant, in raw UTC FILETIME ticks. The core formats them.
pub struct SessionState {
    pub fake_ft: i64,
    pub real_ft: i64,
    pub multiplier: i64,
    pub tz_bias: i32,
    pub elapsed_fake_ms: i64,
    pub elapsed_real_ms: i64,
    /// How long the machine has slept since the session started, in milliseconds (R5-P1, R5-D1). The
    /// session clock stands still while the machine sleeps (ADR-5) - its real base is the unbiased
    /// interrupt time, which leaves sleep out - so this is the time the session did not count, and the
    /// core says so. Measured as the growth of the sleep bias (`chrono_ctl::sleep_bias`), which Windows
    /// moves only when the machine wakes, so it is exact rather than a difference of two clocks.
    pub host_slept_ms: i64,
    /// Whether the duration axes are standing at the end of their range - see
    /// [`SessionState::duration_at_range_end`]. Carried on the snapshot rather than recomputed by the
    /// reader, because it depends on the same `now_real` the rest of this snapshot was taken at.
    duration_saturated: bool,
}

/// The fake wall instant at `now_real`, from the anchor and the rate.
///
/// This is the SAME projection the hook computes inside the target (`chrono_hook::compute_fake`),
/// clamp included, and it has to stay that way: this one is what the session REPORTS about itself,
/// so a difference between them is the tool lying about its own clock. Wrapping here did exactly
/// that at the end of the range - measured at 30828-09-01 under the maximum multiplier, the target
/// held at the clamp while `state` announced the year -27627, a date no channel ever showed anyone,
/// in the event the panel and the evidence export are built from (R2-X2).
pub fn project_fake_ft(anchor_fake: i64, anchor_real: i64, now_real: i64, multiplier: i64) -> i64 {
    chrono_ctl::fake_wall_at(anchor_fake, anchor_real, now_real, multiplier)
}

/// Fake ticks elapsed since the session started: what was banked in earlier rate segments (`before`),
/// plus the current segment, which began at `segment_real0`, at the rate in force. Frozen (m = 0)
/// contributes nothing, which is correct - a frozen clock spends no fake time.
///
/// Saturating, and never below the banked amount (R5-P17). It wrapped until R5: at the maximum multiplier
/// the product passed the top of an i64 about 10.7 days in, and `state`, `ended` and the report announced a
/// NEGATIVE fake elapsed time of some nine hundred million seconds. Standing at the top is still not a true
/// number, which is why it never comes alone: the duration axis runs at a rate at least this one from a base
/// at zero or above, so it reaches the top first and `time.duration_axis_clamped` says so
/// (`chrono_ctl::dur_axis_at_range_end`).
fn fake_elapsed_ticks(before: i64, segment_real0: i64, now_real: i64, m: i64) -> i64 {
    let segment = now_real.saturating_sub(segment_real0).max(0).saturating_mul(m.max(0));
    before.saturating_add(segment)
}

// The session range is stated twice: in `chrono-core`, which admits a moment and depends on nothing,
// and in `chrono-ctl`, whose clamp the hook holds the clock to. This crate sees both, so it checks
// them when it compiles - the gate that lets a moment in and the clamp that keeps it cannot drift
// apart without the build failing (R4-S7, R4-S9).
const _: () = {
    assert!(
        chrono_core::FAKE_WALL_MAX == chrono_ctl::FAKE_WALL_MAX,
        "the last instant a session may start at must be the instant the hook clamps to"
    );
    assert!(
        (chrono_core::ZONE_BIAS_MAX_MIN as i64) * 60 * 10_000_000 <= chrono_ctl::MAX_ZONE_BIAS_TICKS,
        "every zone the session accepts must fit the headroom the clamp leaves for the local-time channels"
    );
};

impl SessionState {
    /// Whether the fake wall clock is standing on the last instant this build can represent. The
    /// session is then doing less than it promised - the clock has stopped while fake time is still
    /// being counted - so the core says so rather than let the reader infer it from a still picture
    /// (untouchable rule 6, R2-X2).
    pub fn clock_at_range_end(&self) -> bool {
        self.fake_ft >= chrono_ctl::FAKE_WALL_MAX
    }

    /// Whether the monotonic DURATION axes have reached the end of their range and are standing.
    ///
    /// The wall clock has had `clock_at_range_end` since R2-X2. The three duration axes - the ones
    /// untouchable rule 3 is actually about - had nothing: they wrapped, which sent a clock the rule
    /// says never rewinds backwards by centuries in a single step, invisibly. They saturate now, and
    /// this is what makes the standing detectable, so the session can say it rather than let a reader
    /// discover a frozen elapsed counter on their own (rule 6).
    pub fn duration_at_range_end(&self) -> bool {
        self.duration_saturated
    }
}

/// The pids a parent's ring holds past what was already taken, and how far the taking got (R4-N18).
/// `read(at)` is entry `at`, 0 for one claimed and not written yet. A parent that may still run writes it
/// soon, so the walk stops there and the next poll reads on. One that has ended never will, and its
/// hole used to hold back every entry after it for the rest of the session - those children were then
/// only counted, never named. It is stepped over now, and the child it stood for stays in the count.
fn ring_pids(taken: usize, named: usize, writer_may_run: bool, read: impl Fn(usize) -> u32) -> (Vec<u32>, usize) {
    let mut pids = Vec::new();
    let mut at = taken;
    while at < named {
        let pid = read(at);
        if pid == 0 && writer_may_run {
            break;
        }
        if pid != 0 {
            pids.push(pid);
        }
        at += 1;
    }
    (pids, at)
}

/// An uncovered child, named only when the process under its pid is one the family started (R4-N13).
/// The hook wrote the pid at the spawn, and the name is asked up to a poll later, so a child that ended
/// in between left its pid to anybody. The snapshot's parent decides, as it does for a family member
/// (`family::member_of_family`): a process started from outside the family is a stranger, and the child
/// is reported the way an ended one is, by its pid alone. A snapshot that could not be read names it as
/// before. Between the snapshot and the open behind `describe` the pid would have to be freed and taken
/// again, which is the window of a few instructions.
fn name_if_ours(
    pid: u32,
    parent_pid: u32,
    known: &HashSet<u32>,
    entries: Option<&[tree::ProcessEntry]>,
    describe: impl FnOnce(u32) -> (Option<String>, Option<String>),
) -> UncoveredChild {
    let (image, command_line) = match family::member_of_family(pid, known, entries) {
        Some(_) => describe(pid),
        None => (None, None),
    };
    UncoveredChild { pid, parent_pid, image, command_line }
}

/// The family is alive when the launched process is, or any other process on its clock - asked in that
/// order (R4-N11). A child the hook follows signs in to the registry while its parent is still inside
/// the call that creates it, so a launched process that has ended left every child it started in the
/// registry already. Asked the other way round, a launched process that started a child and ended
/// between the two looks was seen ended next to a registry read from before the child, and the session
/// closed under a running application. Both looks always happen: the registry read also keeps the
/// family's own list current.
fn alive_in_order<S>(session: &mut S, launched: fn(&S) -> bool, others: fn(&mut S) -> bool) -> bool {
    let launched = launched(session);
    others(session) || launched
}

/// Window after resume within which a target exit is read as a single-instance vanish
/// (ADR-4). Reuses the coverage sample window, so a healthy target pays no extra wait.
const GUARD_MS: u32 = 300;

fn real_system_filetime() -> i64 {
    // windows 0.62: GetSystemTimeAsFileTime takes no argument and returns FILETIME.
    let ft = unsafe { GetSystemTimeAsFileTime() };
    (((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64) as i64
}

impl Session {
    fn ctl(&self) -> *const Ctl {
        self.ctl_addr as *const Ctl
    }

    /// Read both clocks now. Fake = the anchor projected by the real elapsed time,
    /// real = the actual system time (the core is not hooked, so this is genuine).
    pub fn state(&self) -> SessionState {
        let (a_fake, a_real, m) = unsafe { read_anchor(self.ctl()) };
        let now_real = quitp_now();
        let fake_ft = project_fake_ft(a_fake, a_real, now_real, m);
        // Read from the SAME anchor snapshot and the same `now_real` as the projection above, so the
        // flag describes the numbers beside it rather than a moment slightly later.
        let dur = unsafe { read_dur(self.ctl()) };
        SessionState {
            fake_ft,
            real_ft: real_system_filetime(),
            multiplier: m,
            tz_bias: self.tz_bias,
            elapsed_fake_ms: self.fake_elapsed_ticks(now_real, m) / 10_000,
            elapsed_real_ms: now_real.saturating_sub(self.start_real).max(0) / 10_000,
            host_slept_ms: sleep_bias_now().saturating_sub(self.sleep_bias0).max(0) / 10_000,
            duration_saturated: chrono_ctl::dur_axis_at_range_end(dur.quit_c0, dur.q0, dur.m, now_real),
        }
    }

    /// Fake ticks elapsed since the session started - see [`fake_elapsed_ticks`].
    fn fake_elapsed_ticks(&self, now_real: i64, m: i64) -> i64 {
        fake_elapsed_ticks(self.fake_elapsed_before.get(), self.rate_segment_real0.get(), now_real, m)
    }

    /// End the current rate segment at `now_real`, banking its fake time.
    fn close_rate_segment(&self, now_real: i64, m: i64) {
        self.fake_elapsed_before.set(self.fake_elapsed_ticks(now_real, m));
        self.rate_segment_real0.set(now_real);
    }

    /// Whether the target process is still running.
    pub fn is_alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.hprocess, 0) == WAIT_TIMEOUT }
    }

    /// Start watching the processes that joined the session since the last call, and stop watching
    /// the ones that ended. Cheap enough for the child poll: a new member costs one open and one
    /// process snapshot, a known one a zero-length wait.
    pub fn refresh_family(&mut self) {
        // The creation time rides along with the pid (R4-N12), read after `read_pid` saw the pid.
        let published: Vec<family::SignedIn> = (0..MAX_COV_PIDS)
            .map(|slot| (slot, unsafe { read_pid(self.ctl(), slot) }))
            .filter(|&(_, pid)| pid != 0)
            .map(|(slot, pid)| family::SignedIn {
                slot,
                pid,
                created: unsafe { read_created(cov_at(self.ctl(), slot)) },
                windowed: unsafe { read_subsystem(cov_at(self.ctl(), slot)) } != SUBSYSTEM_CONSOLE,
            })
            .collect();
        self.family.refresh(self.pid, &published);
    }

    /// Whether any process on the session clock is still running: the launched one, or any the hook
    /// followed into. A launcher that started the application and ended leaves the session running
    /// for the application (ADR-16).
    pub fn family_alive(&mut self) -> bool {
        alive_in_order(self, Self::is_alive, Self::others_alive)
    }

    /// Whether a process besides the launched one is on the session clock, from the registry as it
    /// stands now.
    fn others_alive(&mut self) -> bool {
        self.refresh_family();
        !self.family.living().is_empty()
    }

    /// Whether every process that signed in to this session is a console program, by the subsystem each
    /// hook recorded in its slot (ADR-4, owner decision 2026-10-27). A family that ended inside the guard
    /// window is a suspected hand-off only when it held a windowed program, because a hand-off is what the
    /// second instance of a windowed application does. A family of console tools that ended there simply
    /// did its work quickly. See `only_console` for every doubt that keeps the suspicion.
    pub fn family_only_console(&self) -> bool {
        let claims = unsafe { read_pid_count(self.ctl()) };
        let slots: Vec<Option<u32>> = (0..(claims as usize).min(MAX_COV_PIDS))
            .map(|slot| {
                let published = unsafe { read_pid(self.ctl(), slot) } != 0;
                published.then(|| unsafe { read_subsystem(cov_at(self.ctl(), slot)) })
            })
            .collect();
        only_console(claims, &slots)
    }

    /// The processes besides the launched one that were running when the family was last refreshed.
    pub fn living_family(&self) -> Vec<FamilyMember> {
        self.family.living()
    }

    /// The executables of the family's windowed processes, the launched one first. The launched process
    /// counts as windowed unless its hook recorded the console subsystem - a process that never signed
    /// in recorded nothing, and the doubt keeps it, as `only_console` does.
    fn windowed_images(&self) -> Vec<String> {
        let root_windowed = match self.family.root_slot() {
            Some(slot) => (unsafe { read_subsystem(cov_at(self.ctl(), slot)) }) != SUBSYSTEM_CONSOLE,
            None => true,
        };
        let root = self.root_image.clone().filter(|_| root_windowed);
        root.into_iter().chain(self.family.windowed_images().iter().cloned()).collect()
    }

    /// The pids this session holds as its own: the launched process, whose handle it keeps for its whole
    /// life, and the members it watches, each through a handle of its own. A held handle keeps the system
    /// from handing the pid on, so each of these still names the process it was taken for - which a pid
    /// noted once and kept does not (ADR-4, 2026-11-11). The watch is as fresh as the last `family_alive`.
    fn held_pids(&self) -> HashSet<u32> {
        std::iter::once(self.pid).chain(self.family.living().into_iter().map(|m| m.pid)).collect()
    }

    /// Whether a process of this session shows a visible window now (ADR-4, 2026-11-11): one the session
    /// holds, or a child the hook never entered (`unhooked`) that still runs the executable it was named
    /// with. A pid the session neither holds nor can confirm may belong to another program by now, and that
    /// program's window is not one the tester saw of this application.
    pub fn family_shows_window(&self, unhooked: &[UncoveredChild]) -> bool {
        let owners = handoff::visible_window_owners();
        !owners.is_empty() && handoff::owned_by_member(&owners, &self.held_pids(), unhooked, handoff::full_path_of)
    }

    /// The processes outside this session that run the executable of a windowed process of it: the copies
    /// a single-instance application may have handed its work to (ADR-4, 2026-11-11). The session's own
    /// processes are told apart the way `family_shows_window` tells them - held, or a child the hook never
    /// entered (`unhooked`) that still runs the file it was named with - so a copy that came to hold the
    /// pid of a child gone long ago is still a copy. An error is the process list's own.
    pub fn running_copies(&self, unhooked: &[UncoveredChild]) -> Result<Vec<RunningCopy>, String> {
        let mut members = self.held_pids();
        members.extend(
            unhooked.iter().filter(|child| handoff::still_the_child(child, &handoff::full_path_of)).map(|child| child.pid),
        );
        handoff::running_copies(&self.windowed_images(), &members)
    }

    /// The target's exit code, once it has exited. Whether it has is the signalled process object's to
    /// say, not the code's (R4-N19): 259 is also a code a program can end with, and read as "still
    /// running" it took the launched application's real exit code off the report.
    pub fn exit_code(&self) -> Option<i32> {
        if self.is_alive() {
            return None;
        }
        let mut code: u32 = 0;
        unsafe { GetExitCodeProcess(self.hprocess, &mut code).is_ok().then_some(code as i32) }
    }

    fn ctl_mut(&self) -> *mut Ctl {
        self.ctl_addr as *mut Ctl
    }

    /// Change the multiplier in flight, re-anchoring from the current clock so the
    /// fake time is continuous across the change (ADR-5): the fake instant now becomes
    /// the new fake anchor and the real clock now the new real anchor.
    ///
    /// The duration axes are re-anchored the SAME way, in the same seqlock transaction. Without it, a
    /// smaller multiplier would retroactively rescale the whole `(real_now - dur_q0)` history and rewind
    /// GetTickCount64/QUIT (H-1) - a violation of untouchable rule 3. `chrono_ctl::rebase` freezes each
    /// axis where it stands under the OLD multiplier and re-bases it at `now`, so it continues from there
    /// at the new speed without ever going backward. It reads `now` and the QPC inside its own write
    /// (R4-S3, F3): read here, before the write opened, they left a reader of the old anchor room to take
    /// a later clock and answer above where the axis then carried on. The wall anchor goes through the
    /// same clamped projection the hook serves and `state` reports (R4-S7a).
    pub fn set_multiplier(&self, m: i64) {
        let done = unsafe { rebase(self.ctl_mut(), m, quitp_now, qpc_now) };
        // Bank the fake time spent at the OLD rate before the new one starts, so elapsed stays an
        // integral over the whole session instead of being rescaled by whatever the latest rate is.
        self.close_rate_segment(done.now, done.old_m);
    }

    /// Jump the wall clock to `to_ft` (UTC FILETIME), keeping the current multiplier.
    /// The duration axis anchors separately in the hook, so it is not affected - a
    /// backward jump never rewinds it (untouchable rule 3).
    pub fn jump(&self, to_ft: i64) {
        let now = quitp_now();
        let (_, _, cur_m) = unsafe { read_anchor(self.ctl()) };
        unsafe { write_anchor(self.ctl_mut(), to_ft, now, cur_m) };
    }

    /// The session zone bias in minutes (UTC = local + bias) - the zone a jump that names none is
    /// read in, like every other moment of this session (untouchable rule 2).
    pub fn tz_bias(&self) -> i32 {
        self.tz_bias
    }

    /// Jump the wall clock by ONE shift step from its CURRENT fake value, keeping the
    /// multiplier. Fixed-length units add a tick delta (sub-second precision preserved) -
    /// calendar units (months/quarters/years) fold through the civil date in the session
    /// zone. Computed under ONE anchor read so no real time leaks between reading the
    /// current fake and re-anchoring - a calendar jump is as race-free as a fixed one
    /// (ADR-5). Returns Err if the step is unsupported (business days) or overflows.
    pub fn jump_step(
        &self,
        step: &chrono_core::calc::Step,
    ) -> Result<(), chrono_core::calc::EvalError> {
        let now = quitp_now();
        let (a_fake, a_real, cur_m) = unsafe { read_anchor(self.ctl()) };
        // From the clock the target actually reads, clamp included - see `set_multiplier` (R4-S7a).
        let fake_now = project_fake_ft(a_fake, a_real, now, cur_m);
        let target = chrono_core::calc::step_target(fake_now, self.tz_bias, step)?;
        unsafe { write_anchor(self.ctl_mut(), target, now, cur_m) };
        Ok(())
    }

    /// How many absolute deadlines the whole family has had converted so far (R5-D2). The core asks it at
    /// every jump and speed change: a deadline converted before the clock moved fires where the old clock
    /// would have reached it, which the audit says (`timer.absolute_due_converted`). Every slot is read,
    /// published or not - a slot reserved by a process whose hook has not finished yet counts zero, and one
    /// whose process has ended still holds what it counted (S-9).
    pub fn absolute_dues_total(&self) -> u64 {
        (0..MAX_COV_PIDS).fold(0u64, |sum, slot| {
            sum.saturating_add(unsafe { read_absolute_dues(cov_at(self.ctl(), slot)) })
        })
    }

    /// Scan the PID registry for processes not yet reported (children that joined the
    /// session after `prepare`, ADR-3) and return each one's OWN coverage. Idempotent:
    /// a slot is returned exactly once across calls.
    ///
    /// A child's evidence cannot be lost here any more, however briefly it lived: its coverage sits
    /// in the control block this session holds mapped from `prepare` to `end`, so the poll reads
    /// what the child wrote whether or not the child is still alive. That is the whole of S-9 - the
    /// coverage used to live in a section the child's own handle kept alive, and a helper shorter
    /// than the poll interval took its evidence with it every single time.
    pub fn poll_new_coverage(&mut self) -> Vec<(u32, Coverage)> {
        let mut out = Vec::new();
        unsafe {
            for i in 0..MAX_COV_PIDS {
                let pid = read_pid(self.ctl(), i);
                // 0 = empty or reserved-but-not-yet-published - skip and retry later.
                if pid == 0 || self.reported_slots[i] {
                    continue;
                }
                let cov = cov_at(self.ctl(), i);
                let coverage =
                    gather_coverage(cov, read_installed(cov), self.scale_duration, self.scale_qpc);
                self.reported_slots[i] = true;
                out.push((pid, coverage));
            }
        }
        out
    }

    /// Children that hooked processes spawned and the hook did not follow into, not seen before -
    /// each named once, with the image name resolved NOW, while the child is most likely still alive.
    ///
    /// The pid comes from the parent's slot (the hook wrote it at the spawn, see
    /// `chrono_ctl::record_uncovered_child`), and only the pid: a detour must not allocate, so naming
    /// happens here, on the mechanism side, from a snapshot the OS still holds. A child that has
    /// already exited keeps its pid and loses its name - the report says so rather than guessing - and
    /// so does one whose pid a process from outside the family has taken since (R4-N13).
    ///
    /// Called at the child poll cadence (about every 100 ms) so a short-lived child is usually still
    /// there to be named, and once more at the end so a late spawn is not lost. A parent whose ring
    /// overflowed still reports every pid it managed to record, and `uncovered_children_total` (the
    /// sum of the counters) carries the ones it could not.
    pub fn poll_uncovered_children(&mut self) -> Vec<UncoveredChild> {
        // The family as it stands now, so a parent that has ended is known to have ended (R4-N18).
        self.refresh_family();
        let mut found: Vec<(u32, u32)> = Vec::new();
        for slot in 0..MAX_COV_PIDS {
            // SAFETY: the control block stays mapped for the whole session, and each read is of one
            // published field.
            let parent = unsafe { read_pid(self.ctl(), slot) };
            if parent == 0 {
                continue;
            }
            let cov = unsafe { cov_at(self.ctl(), slot) };
            let count = unsafe { read_uncovered_children_count(cov) } as usize;
            let named = count.min(chrono_ctl::UNCOVERED_CHILDREN_MAX);
            let writer_may_run = self.slot_may_run(slot);
            let (pids, taken) = ring_pids(self.consumed_children[slot] as usize, named, writer_may_run, |at| {
                // SAFETY: as above, `at` is below the ring's length.
                unsafe { read_uncovered_child(cov, at) }
            });
            self.consumed_children[slot] = taken as u32;
            found.extend(pids.into_iter().map(|pid| (parent, pid)));
        }
        if found.is_empty() {
            return Vec::new();
        }
        let known: HashSet<u32> = std::iter::once(self.pid)
            .chain((0..MAX_COV_PIDS).map(|slot| unsafe { read_pid(self.ctl(), slot) }))
            .filter(|&pid| pid != 0)
            .collect();
        let entries = tree::process_entries().ok();
        found
            .into_iter()
            .map(|(parent, pid)| name_if_ours(pid, parent, &known, entries.as_deref(), describe_process))
            .collect()
    }

    /// Whether the process in sign-in slot `slot` may still write to its ring: running when the family
    /// was last refreshed, or not looked at yet. The launched process is asked through the session's own
    /// handle.
    fn slot_may_run(&self, slot: usize) -> bool {
        if self.family.is_root(slot) {
            return self.is_alive();
        }
        self.family.may_run(slot)
    }

    /// Every uncovered child every hooked process reported, ring overflow included - the number the
    /// audit owes when the named list is shorter than the truth.
    pub fn uncovered_children_total(&self) -> u32 {
        let mut total: u32 = 0;
        unsafe {
            for i in 0..MAX_COV_PIDS {
                if read_pid(self.ctl(), i) == 0 {
                    continue;
                }
                total = total.saturating_add(read_uncovered_children_count(cov_at(self.ctl(), i)));
            }
        }
        total
    }

    /// Every published process's coverage as it stands NOW, reported or not.
    ///
    /// `poll_new_coverage` answers "who joined since last time" and hands each process out exactly
    /// once, which is right for discovery and wrong for counts: the parent's one event is emitted
    /// about 300 ms after resume, inside the ADR-4 guard window, so its call counts are the first
    /// blink of the session and never move again. Measured - a probe that read the clock 25 times
    /// over five seconds was reported as "2 calls", and that number is what the report, the panel and
    /// the exported evidence all showed under "how many times" (R2-X8). The caller emits this at the
    /// end so the counts describe the session that ran.
    pub fn read_all_coverage(&self) -> Vec<(u32, Coverage)> {
        let mut out = Vec::new();
        unsafe {
            for i in 0..MAX_COV_PIDS {
                let pid = read_pid(self.ctl(), i);
                if pid == 0 {
                    continue; // empty, or reserved but not yet published
                }
                let cov = cov_at(self.ctl(), i);
                out.push((
                    pid,
                    gather_coverage(cov, read_installed(cov), self.scale_duration, self.scale_qpc),
                ));
            }
        }
        out
    }

    /// How many processes of this session ran with NO coverage slot, because the registry was already
    /// full. Zero for every ordinary session - `MAX_COV_PIDS` is 4096 and slots are never reused, so a
    /// long session whose application keeps starting short helpers is the realistic way past it (R5-P26).
    ///
    /// These processes are invisible to `poll_new_coverage` - they published no pid, because they had
    /// nowhere to publish one - so the family count and the coverage list are both short by this many,
    /// and nothing else in the audit can say so. Read it BEFORE `end`, while the block is still mapped.
    pub fn uncovered_process_count(&self) -> u32 {
        uncovered_from_attempts(unsafe { read_pid_count(self.ctl()) })
    }

    /// How many processes that signed in to this session would not let the core wait on them (R5-D3).
    /// The session lasts while a process on its clock runs (ADR-16), and these it cannot see close, so it
    /// may have ended while one of them still ran - which from then on ran on the real clock.
    pub fn unwatchable_member_count(&self) -> u32 {
        self.family.unwatchable()
    }

    /// Stop the application, for the one case where letting it run would be worse than not running it:
    /// the opening verdict says the substitution did not take effect, so every minute the tester spends
    /// in it produces evidence about the REAL clock while looking like a time-shifted run. Coverage
    /// already gathered stays valid and is still reported - this ends processes, not the audit.
    ///
    /// The whole family, not the launched process alone (R4-S5): whatever it had started in its first
    /// moments used to run on. The registry and the ring are read here, while the block is mapped, and
    /// `ending` confirms every process by its creation time before ending it. What is still running
    /// afterwards is named in the answer, and a family that could be looked for only in part says why.
    pub fn terminate_family(&self) -> FamilyEnd {
        let ctl = self.ctl();
        let slots: Vec<ending::Known> = (0..MAX_COV_PIDS)
            .map(|slot| (slot, unsafe { read_pid(ctl, slot) }))
            .filter(|&(_, pid)| pid != 0)
            .map(|(slot, pid)| ending::Known { pid, born: unsafe { read_created(cov_at(ctl, slot)) } })
            .collect();
        let ring: Vec<u32> = (0..MAX_COV_PIDS)
            .filter(|&slot| unsafe { read_pid(ctl, slot) } != 0)
            .flat_map(|slot| {
                let cov = unsafe { cov_at(ctl, slot) };
                let named = unsafe { read_uncovered_children_count(cov) } as usize;
                (0..named.min(chrono_ctl::UNCOVERED_CHILDREN_MAX)).map(move |i| unsafe { read_uncovered_child(cov, i) })
            })
            .filter(|&pid| pid != 0)
            .collect();
        ending::end_family(self.hprocess, self.pid, &slots, &ring)
    }

    /// Release our own handles. The target keeps its own mapped view of the control
    /// block, so its hooks keep working after we detach (full residue cleanup is a
    /// later slice).
    ///
    /// Kept as the explicit way to end a session (it reads as one at every call site), but the release
    /// itself lives in `Drop`: every path out of the core called this today, and the first `?` or early
    /// `return` added next to one would have leaked the process handle, the control mapping and every
    /// coverage section - silently, since nothing observes a leaked handle until the machine is short of
    /// them.
    pub fn end(self) {
        drop(self);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            // Every ordered way out of a session passes here, so this is where the block learns the
            // session is over (R4-W2): a process of it that starts a child later does not hand that
            // child a clock nobody drives, and the next core can say what it took over. A core that is
            // killed never gets here, which the session's identity in the block covers instead.
            mark_ended(self.ctl_addr as *mut Ctl);
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.ctl_addr as *mut c_void,
            });
            let _ = CloseHandle(self.hmap);
            let _ = CloseHandle(self.hprocess);
        }
    }
}

/// A process launched WITHOUT the hook, for a probe that needs a host running with the session's
/// environment and nothing else - the embedded-engine channel proves its port discovery and its
/// attach on a host it started this way (docs/09) - and for the browser of the Chromium mechanism. The
/// process is the owner's for as long as this value lives and is TERMINATED when it drops, whichever
/// way the owner leaves - an early return included. A probe that launched an application must never
/// leave it running with the session's variables in its environment. Nothing today needs to keep such
/// a process alive past its owner, so there is no way to detach one - the day a caller needs that, it
/// gets a method, not a default.
///
/// It runs in a job from its first instruction, with everything it starts (R4-N15, R4-N30): ending
/// only the process we started left its children running, and a killed owner ended nothing at all.
/// The job ends them all together, the second case included, because the kernel closes a killed
/// process's handles.
pub struct PlainChild {
    pub pid: u32,
    handle: HANDLE,
    /// `None` only when the system would not give it one, which `job_note` then says.
    job: Option<job::Job>,
    job_note: Option<String>,
}

impl PlainChild {
    /// Whether the process is still running.
    pub fn is_alive(&self) -> bool {
        // SAFETY: the handle is ours until drop.
        unsafe { WaitForSingleObject(self.handle, 0) == WAIT_TIMEOUT }
    }

    /// Wait up to `timeout_ms` for the process to end and return its exit code, or None if it is
    /// still running when the wait runs out.
    pub fn wait_exit(&self, timeout_ms: u32) -> Option<i32> {
        // SAFETY: the handle is ours until drop, and the exit code is read only after the wait
        // said the process signalled.
        unsafe {
            if WaitForSingleObject(self.handle, timeout_ms) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code: u32 = 0;
            GetExitCodeProcess(self.handle, &mut code).ok()?;
            Some(code as i32)
        }
    }

    /// End the process now, with everything it started. Idempotent: a process already gone is not an
    /// error here.
    pub fn terminate(&self) {
        if let Some(job) = &self.job {
            job.end_all(1);
        }
        // SAFETY: the handle is ours until drop, and terminating a process that already exited fails
        // harmlessly.
        unsafe {
            let _ = TerminateProcess(self.handle, 1);
        }
    }

    /// End the process with everything it started, as [`terminate`](Self::terminate) does, and wait up
    /// to `timeout_ms` for every one of them to have ended - by then the files they held are closed.
    /// True when all ended in time. False also when a process of the job could not be opened to be
    /// waited on, or the job's processes could not be listed: those cannot be confirmed to have ended,
    /// and the files they held may still be held.
    ///
    /// The processes of the job are opened before it is ended, because a launcher that handed its work
    /// to a process of its own has ended already: waiting for it alone waited for nothing while the
    /// process it handed over to was still closing its files. An installed Chromium browser does that as
    /// it starts (measured on two, 2026-11-09), and the profile its session removes next was left behind
    /// in about one session of eight. Without a job, the launched process alone is waited for.
    pub fn terminate_and_wait(&self, timeout_ms: u32) -> bool {
        let (members, unconfirmed) = self.job.as_ref().map(job::Job::members).unwrap_or_default();
        self.terminate();
        let ended = self.ended_with(&members, Instant::now() + std::time::Duration::from_millis(u64::from(timeout_ms)));
        ended && unconfirmed == 0
    }

    /// Whether the process and `members` have all ended by `deadline`, waiting for them until then.
    /// Every one is waited for, whatever the ones before it did, so the wait covers them all.
    fn ended_with(&self, members: &[job::Member], deadline: Instant) -> bool {
        let launched = !ending::waits_out(self.handle, deadline);
        members.iter().fold(launched, |all, member| member.ended_by(deadline) && all)
    }

    /// Why the process runs without a job, when it does - its children then outlive it, and a killed
    /// owner leaves it running. The owner says so rather than let it pass (rule 6).
    pub fn job_note(&self) -> Option<&str> {
        self.job_note.as_deref()
    }
}

impl Drop for PlainChild {
    fn drop(&mut self) {
        self.terminate();
        // SAFETY: closing the one process handle this struct opened, once. The job closes after this,
        // with its own drop.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Launch a target without suspending or injecting it: `CreateProcessW` with the command line the
/// hooked launch builds, the working folder, and the same environment treatment, in a job that ends
/// it and its children with the value (R4-N15). The one way a probe starts a host the channel then
/// has to find, and the way the Chromium mechanism starts its browser (R4-N30).
pub fn launch_plain(target: &Target) -> Result<PlainChild, String> {
    let (app, cmdline) = launch_line(target)?;
    // SAFETY: the buffers `launch_line` made for this target. The thread handle is closed at once -
    // nothing here resumes or inspects the thread.
    unsafe {
        let (pi, job) = create_in_job(&app, &cmdline, target, PROCESS_CREATION_FLAGS(0), false)?;
        let _ = CloseHandle(pi.hThread);
        let (job, job_note) = match job {
            Ok(job) => (Some(job), None),
            Err(why) => (None, Some(why)),
        };
        Ok(PlainChild { pid: pi.dwProcessId, handle: pi.hProcess, job, job_note })
    }
}

/// The job a process was started in, or why it runs without one.
type JobOrWhy = Result<job::Job, String>;

/// Start `target` in a job that ends it when the job's last handle closes. With `children_stay_out`
/// the children it starts stay outside that job, which is how `prepare` holds a target only while it
/// starts it. A job the system will not start a process in (this process runs in one that forbids
/// nesting, say) is not a reason to refuse the launch: the process then starts without one, and the
/// answer says why. A launch that fails for another reason fails the same way the second time.
///
/// # Safety
/// `app` and `cmdline` are the NUL-terminated buffers `launch_line` made for `target`.
unsafe fn create_in_job(
    app: &[u16],
    cmdline: &[u16],
    target: &Target,
    flags: PROCESS_CREATION_FLAGS,
    children_stay_out: bool,
) -> Result<(PROCESS_INFORMATION, JobOrWhy), String> { unsafe {
    let job = job::Job::ending_with_us(children_stay_out);
    if let Ok(held) = &job {
        match create_process(app, cmdline, target, flags, Some(held.handle())) {
            Ok(pi) => return Ok((pi, job)),
            Err(in_job) => {
                let pi = create_process(app, cmdline, target, flags, None)?;
                return Ok((pi, Err(format!("the system would not start it in a job ({in_job})"))));
            }
        }
    }
    let pi = create_process(app, cmdline, target, flags, None)?;
    Ok((pi, job))
}}

/// One `CreateProcessW` for `target`: its environment, its standard handles and, when named, the job
/// it starts in. The command line is copied for the call, because `CreateProcessW` may write to it.
///
/// # Safety
/// `app` and `cmdline` are NUL-terminated.
unsafe fn create_process(
    app: &[u16],
    cmdline: &[u16],
    target: &Target,
    flags: PROCESS_CREATION_FLAGS,
    job: Option<HANDLE>,
) -> Result<PROCESS_INFORMATION, String> { unsafe {
    let stdio = stdio::launch_stdio(target.stdio, job)?;
    let mut line = cmdline.to_vec();
    let cwd_wide = target.cwd.map(to_wide);
    let cwd_ptr = cwd_wide
        .as_ref()
        .map(|w| PCWSTR(w.as_ptr()))
        .unwrap_or(PCWSTR::null());
    let mut pi = PROCESS_INFORMATION::default();
    let (block, env_ptr, env_flag) = environment_for(target.env);
    let launched = CreateProcessW(
        PCWSTR(app.as_ptr()),
        Some(PWSTR(line.as_mut_ptr())),
        None,
        None,
        stdio.inherit,
        flags | env_flag | stdio.flags,
        env_ptr,
        cwd_ptr,
        stdio.startup_info(),
        &mut pi,
    );
    // The child holds its own copies now, so this process's are closed at once.
    drop(block);
    drop(stdio);
    launched.map_err(|e| win32_detail("CreateProcessW", &e))?;
    Ok(pi)
}}

/// A zero character ends the string `CreateProcessW` reads, so a path, an argument or a working folder
/// holding one lost whatever followed it without a word - for a program, while the batch path already
/// refused it (R4-N16). No command line typed in a shell can carry one, but the protocol can, so it is
/// refused here, before anything starts, for both kinds of target.
fn refuse_zero_character(target: &Target) -> Result<(), String> {
    let fields = std::iter::once(("the target path", target.path))
        .chain(target.cwd.map(|cwd| ("the working folder", cwd)))
        .chain(target.args.iter().map(|arg| ("the argument", arg.as_str())));
    for (what, text) in fields {
        if text.contains('\0') {
            return Err(format!(
                "{what} {text:?} holds a zero character, where Windows would cut it short - remove that character"
            ));
        }
    }
    Ok(())
}

/// The application and the command line `CreateProcessW` is given for a target: the target itself
/// with its arguments quoted for the C runtime, or, for a batch script, the command interpreter with
/// the script and its arguments quoted for it (see `batch`). One definition for both launches.
fn launch_line(target: &Target) -> Result<(Vec<u16>, Vec<u16>), String> {
    refuse_zero_character(target)?;
    if is_batch_script(Path::new(target.path)) {
        let (app, line) = batch::interpreter_line(target.path, target.args)?;
        return Ok((to_wide(&app), to_wide(&line)));
    }
    Ok((to_wide(target.path), build_command_line(target.path, target.args)))
}

/// The `lpEnvironment` argument and its creation flag for a launch: null and no flag when there is
/// nothing to add (the child inherits, exactly as before), or a Unicode block with its flag. The
/// block is returned so it outlives the call - the pointer is into it.
fn environment_for(
    extra: &[(String, String)],
) -> (Option<Vec<u16>>, Option<*const c_void>, windows::Win32::System::Threading::PROCESS_CREATION_FLAGS) {
    if extra.is_empty() {
        return (None, None, windows::Win32::System::Threading::PROCESS_CREATION_FLAGS(0));
    }
    let block = environment_block(extra);
    let ptr = block.as_ptr().cast::<c_void>();
    (Some(block), Some(ptr), CREATE_UNICODE_ENVIRONMENT)
}

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// The real clock every anchor of the session is read from: `QueryUnbiasedInterruptTimePrecise`, the
/// unbiased interrupt time read from the timer hardware (R5-P2, R5-D31, ADR-24).
///
/// The same epoch and the same sleep semantics as `QueryUnbiasedInterruptTime` - disassembled on both
/// bitnesses (2026-10-14), it is the precise interrupt time minus the same sleep bias - so ADR-5 holds. What
/// changes is the resolution: the plain one stands on the last timer interrupt, up to 15.6 ms at the default
/// timer and multiplied by the speed in the fake clock, and a fake `GetSystemTimePreciseAsFileTime` that
/// projected from it stood still for 47 ms at a time at x60 (measured). The precise channels in the hook read
/// this clock too, the others the plain one, which is never ahead of it - `chrono_ctl::fake_wall_at` holds
/// those at the anchor until their clock catches up.
///
/// Between two fences because the clock may come from the processor's time-stamp counter, which is ordered
/// with nothing around it (`chrono_ctl::clock_fence`). The core is never hooked, so the call goes straight
/// to the system.
fn quitp_now() -> i64 {
    clock_fence();
    let t = unsafe { QueryUnbiasedInterruptTimePrecise() } as i64;
    clock_fence();
    t
}

/// The clock a session with no hook of its own runs on (R5-F6, R5-D42): the same unbiased interrupt time
/// the native anchors read ([`quitp_now`]), in 100 ns. It stands still while the machine sleeps and takes no
/// correction of the system clock, so a Chromium session's fake clock stands still across a sleep as the
/// native one does (ADR-5, docs/01 3.0a).
pub fn unbiased_interrupt_time() -> i64 {
    quitp_now()
}

/// How long the machine has slept since it started (100 ns) - what the native session announces as
/// `host_slept`, for a session that reads it without a `Session` (R5-F6).
pub fn slept_since_boot() -> i64 {
    sleep_bias_now()
}

/// How long the machine has slept since it started (100 ns), read as `chrono_ctl::sleep_bias` describes.
fn sleep_bias_now() -> i64 {
    chrono_ctl::sleep_bias(
        || {
            let mut t: u64 = 0;
            unsafe {
                let _ = QueryUnbiasedInterruptTime(&mut t);
            }
            t as i64
        },
        || unsafe { QueryInterruptTime() } as i64,
    )
}

/// Current raw QueryPerformanceCounter value. QPC is a system-wide counter, so the core reads the same
/// value the target's hook does - the QPC axis anchor (ADR-2 reversal) rides on that.
fn qpc_now() -> i64 {
    let mut t: i64 = 0;
    unsafe {
        let _ = QueryPerformanceCounter(&mut t);
    }
    t
}

/// The host's CURRENT effective UTC offset, in the Win32 sense (UTC = local + bias), daylight saving
/// included as it stands right now. Used for the one case where the session must not shift anything:
/// a `run` with no moment asked for, where the session clock is meant to be the real one - reading the
/// moment as UTC there would hand the target a local time off by the host's own offset, the silent
/// "wrong by N hours" untouchable rule 2 exists to prevent. An entered moment is unaffected: it stays
/// in the session zone the caller names.
pub fn host_tz_bias_min() -> i32 {
    // 0 = TIME_ZONE_ID_UNKNOWN (the zone has no DST rules), 1 = STANDARD, 2 = DAYLIGHT,
    // TIME_ZONE_ID_INVALID (u32::MAX) on failure. Anything but DAYLIGHT/STANDARD leaves the base bias,
    // which is the honest answer when the season is unknown and 0 when the call failed - never a guess.
    const TZ_STANDARD: u32 = 1;
    const TZ_DAYLIGHT: u32 = 2;
    let mut tzi = TIME_ZONE_INFORMATION::default();
    unsafe {
        match GetTimeZoneInformation(&mut tzi) {
            TZ_DAYLIGHT => tzi.Bias + tzi.DaylightBias,
            TZ_STANDARD => tzi.Bias + tzi.StandardBias,
            _ => tzi.Bias,
        }
    }
}

/// Whether a process with this pid is still running. "Already signalled" means gone, and so does a pid
/// nothing answers to. A process that refuses the open is running: it exists and will not let this one
/// wait on it - one that runs as administrator, or as another user (R5-P24). It used to read as gone,
/// so the orphan sweep removed the profile of a live elevated session and a session took an elevated
/// member of its family for ended. PID reuse is a hazard in the usual direction: a recycled pid reads
/// as alive, so a caller deciding whether to clean something up errs toward leaving it - never toward
/// destroying something that belongs to a live process. The mechanism layer owns this because it is
/// the only layer that touches the OS (docs/07), so the CDP driver in `cli` borrows it rather than
/// growing its own Win32 dependency.
pub fn process_is_alive(pid: u32) -> bool {
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(h) => {
                let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
                let _ = CloseHandle(h);
                alive
            }
            Err(e) => policy_value::open_error_means_alive(e.code()),
        }
    }
}

/// Exclusive right to run a session, held for the session's whole life. Closing the handle is
/// enough to give the lock up - deliberately no `ReleaseMutex`, because mutex ownership is per
/// THREAD and the core does not promise that `prepare` and `end` run on the same one, whereas
/// closing a handle is safe from any thread and the kernel drops ownership when the last handle
/// goes (a killed core included). Held in `Session` so every early `return Err` in `prepare`
/// releases it by `Drop` - a lock that leaked on an error path would lock the machine out of
/// starting a session until the core exits.
struct SessionLock(HANDLE);

impl Drop for SessionLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// The pid of the core running the session named in the control block, or 0 when there is none to
/// read (no section yet, or a core that has not published its pid). Used only to name the other
/// session in a refusal message - the refusal itself comes from the lock, never from this value.
///
/// # Safety
/// Maps and unmaps the control section - safe to call with no session running.
unsafe fn read_active_core_pid() -> u32 { unsafe {
    // Read access only. This function reads a single u32 and has never written anything, so asking
    // for write was a right we could not use - and the block it opens belongs to a session that is
    // still running.
    let Ok(hmap) = OpenFileMappingW(FILE_MAP_READ.0, false, windows::core::PCWSTR(chrono_ctl::CTL_SECTION_NAME_W.as_ptr()))
    else {
        return 0;
    };
    let view = MapViewOfFile(hmap, FILE_MAP_READ, 0, 0, ctl_size());
    // A block we do not recognise has no pid of ours in it, whatever sits at that offset. Returning
    // 0 makes the caller say "another session's core is starting or running" instead of naming a
    // number read out of a stranger's memory as though it were a Chrono Mock process (rule 6).
    let pid = if view.Value.is_null() || !header_is_ours(view.Value as *const Ctl) {
        0
    } else {
        read_core_pid(view.Value as *const Ctl)
    };
    if !view.Value.is_null() {
        let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
    }
    let _ = CloseHandle(hmap);
    pid
}}

/// Take the session lock, or report who holds it. A named mutex rather than inference from the
/// control block's contents: the previous design read `core_pid`, which `prepare` writes LAST, so
/// a core still initializing published 0 and a second core read that as "orphan" and zeroed a LIVE
/// session's anchor - the target then saw 1601-01-01, frozen, with no error at all (untouchable
/// rules 2 and 3). The kernel releases a mutex when its owner dies, kill -9 included, so an
/// abandoned lock is exactly the orphan signal, with no pid guessing and no pid-reuse hazard.
///
/// # Safety
/// Calls the Win32 synchronization APIs.
unsafe fn take_session_lock() -> Result<SessionLock, PrepareError> { unsafe {
    let h = CreateMutexW(None, false, windows::core::PCWSTR(chrono_ctl::CTL_LOCK_NAME_W.as_ptr()))
        .map_err(|e| PrepareError::Control(win32_detail("CreateMutexW", &e)))?;
    let lock = SessionLock(h);
    if lock_is_ours(WaitForSingleObject(h, 0)) {
        Ok(lock)
    } else {
        Err(PrepareError::SessionActive(read_active_core_pid()))
    }
}}

/// Whether a zero-timeout wait on the session mutex left US holding it. `WAIT_OBJECT_0` is the
/// plain case. `WAIT_ABANDONED` also means ours: the previous owner died without releasing, which
/// is precisely the orphan case we want to reclaim rather than refuse - the whole reason the lock
/// beats reading a pid out of shared memory. Anything else (`WAIT_TIMEOUT`, a failed wait) means a
/// live core holds it, and refusing is the safe direction.
fn lock_is_ours(state: windows::Win32::Foundation::WAIT_EVENT) -> bool {
    state == WAIT_OBJECT_0 || state == WAIT_ABANDONED
}

/// Whether the audit should say that network timeouts inside this process follow the session speed.
///
/// Since the tick count is detoured in kernelbase as well (2026-09-23), a network library that measures
/// its own timeout from it runs that timeout on the session's duration axis, while the socket wait
/// underneath stays real (ADR-7 class B). Measured on WinHTTP at x60: a 60 s receive timeout against a
/// server answering after 4 s gave up after 1.06 s real with ERROR_WINHTTP_TIMEOUT, where the build
/// before the move got the response. Without this line the report would say, one row above, that an
/// I/O timeout is not shortened.
///
/// The signal is a connection counted by this process. The connection observer sees every Winsock
/// path, WinHTTP's `ConnectEx` included, so the count is the one the warning needs. It used to be
/// "ws2_32 loaded", read off the observer's install bit while the observer sat on ws2_32 and missed
/// WinHTTP - which also made a failed hook in a loaded ws2_32 look like no network stack at all. A
/// connection is the narrower signal on purpose: plenty of applications load ws2_32 through another
/// library and never open a socket, and their timeouts have nothing to follow.
///
/// A tick count channel has to be installed as well, because that is what the timeout follows: with
/// both of them failed the tick count stays real, and the sentence would describe a session that did
/// not happen. One of the two is enough for the caution to be true for a library reading that one, and
/// the one that failed is listed as uncovered already.
fn network_timeouts_follow_session(installed: u64, connected: bool, scale_duration: bool) -> bool {
    scale_duration && connected && installed & (CH_GTC64 | CH_GTC) != 0
}

/// The warnings for scaled waits and timers that could not be divided by the full multiplier and ran
/// at a floor instead, so part of this application did NOT accelerate: a wait at this tool's 1 ms
/// (`wait.timeout_collapsed`), a timer at Windows' 10 ms window-timer minimum or at the 1 ms period
/// floor (`timer.interval_floored`, R5 D8). Said out loud rather than left as an unexplained
/// difference in behaviour - the honest half of the floors themselves, which exist because the
/// alternative (an interval truncated to zero) turned a polling loop into a spin and a periodic timer
/// into a one-shot. Coverage first, and name what is left over (rule 27, rule 4).
///
/// Its own function so `gather_coverage` stays under its pinned complexity ceiling.
///
/// # Safety
/// `cov` must point to a live, correctly aligned `Cov`.
unsafe fn floor_warning_keys(cov: *const Cov) -> Vec<String> { unsafe {
    let mut keys = Vec::new();
    if read_waits_at_floor(cov) > 0 {
        keys.push("wait.timeout_collapsed".to_string());
    }
    if read_timers_at_floor(cov) > 0 {
        keys.push("timer.interval_floored".to_string());
    }
    // The same kind of leftover from another cause: waits and timers on a thread without thread-local
    // storage, which the hook passes through unscaled because it cannot tell a cascade there (2026-10-28).
    if read_calls_without_storage(cov) > 0 {
        keys.push("wait.thread_without_storage".to_string());
    }
    keys
}}

/// Whether `ch` is a deadline channel in a session that does not scale durations (R5-D2). The deadline
/// channels stand in EVERY session, but without scale_duration they convert only an absolute deadline:
/// their calls are mostly relative waits and timers the session leaves real, so listing them as substituted
/// would claim every `Sleep` was. They are reported only when they did NOT stand (`deadline_gaps`).
fn deadline_only(ch: &chrono_ctl::ChannelDef, scale_duration: bool) -> bool {
    !scale_duration && DEADLINE_CHANNELS & ch.bit != 0
}

/// The deadline channels that did not stand in a session that does not scale durations - its absolute
/// deadlines then ran on the real clock, a gap the audit names in every session (R5-D2). Its own function
/// so `gather_coverage` stays under its pinned complexity ceiling.
fn deadline_gaps(installed: u64, scale_duration: bool) -> Vec<String> {
    CHANNELS
        .iter()
        .filter(|ch| deadline_only(ch, scale_duration) && installed & ch.bit == 0)
        .map(|ch| ch.name.to_string())
        .collect()
}

/// Build one process's coverage from its `Cov` section: the install bitmask and the
/// live per-channel call counters. Iterates the single-source `CHANNELS` table so the
/// report names exactly what the hook installs.
///
/// # Safety
/// `cov` must point to a live, correctly aligned `Cov`.
unsafe fn gather_coverage(
    cov: *const Cov,
    installed: u64,
    scale_duration: bool,
    scale_qpc: bool,
) -> Coverage { unsafe {
    let mut out = Coverage::default();
    // Track which KIND of observed channel actually ran, so the audit names the right reason: an
    // object wait left real (class B), a multimedia timer left real (class C, winmm/ADR-2), or a
    // direct NtCreateUserProcess left un-injected (ADR-3).
    let mut any_wait_observed = false;
    let mut any_timer_observed = false;
    let mut any_spawn_observed = false;
    let mut any_source_observed = false;
    // Channels the hook tried and could not switch on - the one way to tell a failed detour in an
    // optional module from a module the application never loaded. Read only for a channel that is not
    // installed, so one that failed at startup and went live on the late scan is simply covered.
    let failed = read_failed_channels(cov);
    out.uncovered.extend(deadline_gaps(installed, scale_duration));
    for (idx, ch) in CHANNELS.iter().enumerate().filter(|(_, ch)| !deadline_only(ch, scale_duration)) {
        // The duration axis and the TIME observers are opt-in: with scale_duration off, their channels
        // are not expected. The spawn observer (NtCreateUserProcess) is NOT opt-in - process creation
        // is watched regardless - so it is absent from this gate.
        if matches!(
            ch.category,
            ChannelCategory::Duration | ChannelCategory::WaitObserved | ChannelCategory::TimerObserved
        ) && !scale_duration
        {
            continue;
        }
        // The QPC axis rides its OWN opt-in, not scale_duration (the two carry different risks), so it
        // is expected only when the session asked for it. Off, it is not a gap - the channel is
        // deliberately left real, which is the ADR-2 default.
        if ch.category == ChannelCategory::Qpc && !scale_qpc {
            continue;
        }
        // Observed channels (class B object waits, class C multimedia timer, direct NtCreateUserProcess)
        // are counted but never modified: their own bucket, so they never sway the verdict. A failed
        // install just means we are not observing it - not a verdict-affecting gap, so it goes nowhere.
        if ch.category.observed() {
            if installed & ch.bit != 0 {
                let calls = read_calls(cov, idx);
                if calls > 0 {
                    match ch.category {
                        ChannelCategory::WaitObserved => any_wait_observed = true,
                        ChannelCategory::TimerObserved => any_timer_observed = true,
                        ChannelCategory::SpawnObserved => any_spawn_observed = true,
                        ChannelCategory::SourceObserved => any_source_observed = true,
                        _ => {}
                    }
                }
                out.observed.push(ChannelCoverage { channel: ch.name.to_string(), calls, capped: calls_capped(calls) });
            } else if ch.module.in_every_process() || failed & ch.bit != 0 {
                // "Wanted and not installed" in a module every process has, or one the hook says it
                // could not switch on: either way the hook failed. That used to produce no line at
                // all: not covered, not uncovered, absent - so a watch the session promised was
                // quietly not running while the report read as complete. It goes in its own bucket
                // rather than `uncovered`, which is a verdict input and would turn a failed observer
                // into a Partial session.
                out.unobserved.push(ch.name.to_string());
            }
            continue;
        }
        if installed & ch.bit != 0 {
            let calls = read_calls(cov, idx);
            out.covered.push(ChannelCoverage { channel: ch.name.to_string(), calls, capped: calls_capped(calls) });
        } else if ch.module.in_every_process() || failed & ch.bit != 0 {
            // Failed install, and a real coverage gap: the module is in every process (kernelbase
            // included, since kernel32 is built on it), or it is an optional one the hook found loaded
            // and could not detour. An optional module with no failure recorded is simply not loaded
            // in this process (a console app or service never loads user32), so the app cannot call
            // the channel at all - not a gap, so it goes nowhere rather than faking a partial verdict
            // (rule 4: never claim a gap the target could not hit). One that arrives after startup is
            // the late scan's to find (ADR-10), and a detour that fails there is recorded the same way.
            out.uncovered.push(ch.name.to_string());
        }
    }
    // Separate warnings by observed kind, so the tester knows WHICH time source ran real: an object
    // wait (a real timeout left intact, class B) or a multimedia timer (winmm/audio left unscaled per
    // ADR-2, class C). Each fires only when that kind actually ran under acceleration (calls > 0).
    if any_wait_observed {
        out.warning_keys.push("wait.object_waits_not_scaled".to_string());
    }
    out.warning_keys.extend(floor_warning_keys(cov));
    if any_timer_observed {
        out.warning_keys.push("timer.multimedia_not_scaled".to_string());
    }
    // The target READ the winmm millisecond clock and we SCALED it (partial ADR-2 reversal). Named for
    // the same reason the QPC opt-in is named: the coverage has a cost. winmm is also the audio path, and
    // a media application that positions its buffers from this clock can shift under acceleration. The
    // scheduler timeSetEvent is untouched, so this is about reading, not about when sound is queued.
    if scale_duration && read_calls(cov, IDX_TIMEGETTIME) > 0 {
        out.warning_keys.push("clock.timegettime_scaled_audio_may_shift".to_string());
    }
    // A child this process spawned through the COVERED CreateProcess* path could not be followed into
    // (R2-S2). Distinct from the observed NtCreateUserProcess warning below, which is about a spawn path
    // we deliberately do not inject through: here we tried and it did not take, so one process of this
    // family really did run on the real clock, and the family count is one short of the truth.
    if read_uninjected_children(cov) > 0 {
        out.warning_keys.push("inheritance.child_not_injected".to_string());
    }
    // A direct NtCreateUserProcess ran: its child was NOT injected (ADR-3, observed), so warn that the
    // child may run with real time - honest, since real targets spawn through the covered CreateProcess*.
    if any_spawn_observed {
        out.warning_keys
            .push("inheritance.ntcreateuserprocess_child_maybe_uncovered".to_string());
    }
    // A network connection ran: the target may read time from a SERVER, which no local hook covers. Warn
    // honestly (rule 4) - the local-coverage verdict can be `works` while the real time source is remote
    // and unsubstituted.
    if any_source_observed {
        out.warning_keys.push("source.network_at_start".to_string());
    }
    if network_timeouts_follow_session(installed, any_source_observed, scale_duration) {
        out.warning_keys.push("wait.network_timeouts_scaled".to_string());
    }
    // At least one channel was hooked only after its module turned up, which for a runtime that pulls
    // in winmm or ws2_32 during startup is the ordinary case rather than the exotic one - measured on
    // four of nine processes across six real targets.
    //
    // The INTERSECTION, not the late mask alone. The hook writes the two masks in sequence and this is
    // what makes any order of those two stores safe to read: a channel counts as late only once it is
    // also installed, so a half-written pair can under-report for one sample but can never warn about
    // a channel the report does not list.
    //
    // Warned even when the counter is zero, and that is the point rather than an oversight: calls made
    // before the hook landed were never counted, so we cannot tell an idle channel from one that was
    // busy in the window. Claiming otherwise is exactly what untouchable rule 4 forbids.
    //
    // The channels are NAMED as well. The warning alone said "a channel" while a runtime that loads
    // winmm or ws2_32 brings several at once, and the reader had every row of the audit to guess from.
    let late = read_late_installed(cov) & installed;
    if late != 0 {
        out.warning_keys.push("coverage.channel_installed_late".to_string());
        out.installed_late = CHANNELS
            .iter()
            .filter(|ch| late & ch.bit != 0)
            .map(|ch| ch.name.to_string())
            .collect();
    }
    out
}}

// --- Target bitness (R2-S1) ----------------------------------------------------
//
// A 64-bit core cannot inject into a 32-bit target and the other way round: the remote LoadLibraryW
// simply returns NULL, and the only word we had for that was `target.inject_failed` - the same word
// an antivirus block, a corrupt DLL and (until R2-W3) a missing one all produced. The tester was left
// to guess, when the one fact that would have told them to run the other chrono.exe was knowable.
//
// We ASK THE OS rather than read the target's PE header. A .NET Framework AnyCPU executable carries
// IMAGE_FILE_MACHINE_I386 in its file header yet runs 64-bit on 64-bit Windows unless
// COMIMAGE_FLAGS_32BITREQUIRED is set, so a header-reading gate would refuse a target that works
// today. `IsWow64Process2` reports what the loader actually decided, and stays exact on an ARM64 host
// where an emulated x64 process would fool the older `IsWow64Process`.

/// A machine constant as a label for the report. Unknown values are shown as their raw value rather
/// than guessed at - we never name a machine we do not recognise.
fn machine_label(m: u16) -> &'static str {
    match m {
        x if x == IMAGE_FILE_MACHINE_I386.0 => "x86",
        x if x == IMAGE_FILE_MACHINE_AMD64.0 => "x64",
        x if x == IMAGE_FILE_MACHINE_ARM64.0 => "arm64",
        _ => "an unrecognised machine",
    }
}

/// The machine a live process actually runs as. `IsWow64Process2` reports `IMAGE_FILE_MACHINE_UNKNOWN`
/// for a process that is NOT emulated, in which case the native machine is the answer.
///
/// # Safety
/// `hproc` must be a valid process handle with QUERY_LIMITED_INFORMATION rights.
unsafe fn process_machine(hproc: HANDLE) -> Option<u16> { unsafe {
    let mut process = IMAGE_FILE_MACHINE(0);
    let mut native = IMAGE_FILE_MACHINE(0);
    IsWow64Process2(hproc, &mut process, Some(&mut native)).ok()?;
    Some(if process == IMAGE_FILE_MACHINE_UNKNOWN { native.0 } else { process.0 })
}}

/// Whether this core can reach `hproc` at all. `None` means "go ahead": either the bitness matches, or
/// the query failed and we do not know - and a guess is not grounds for refusing a session (rule 6 cuts
/// both ways, we only declare what we actually established).
///
/// # Safety
/// `hproc` must be a valid process handle with QUERY_LIMITED_INFORMATION rights.
unsafe fn bitness_mismatch(hproc: HANDLE) -> Option<(&'static str, &'static str)> { unsafe {
    let target = process_machine(hproc)?;
    let core = process_machine(GetCurrentProcess())?;
    (target != core).then(|| (machine_label(target), machine_label(core)))
}}

/// How many slot claims went unserved, given the number of claims made. Pure, so the arithmetic is
/// testable without a live session: the registry counter only ever increases and counts ATTEMPTS, so
/// everything past its capacity is a process that ran with nowhere to report coverage into (R2-S9).
fn uncovered_from_attempts(attempts: u32) -> u32 {
    attempts.saturating_sub(MAX_COV_PIDS as u32)
}

/// Whether a family is console programs only, from the number of slot claims made and what each claimed
/// slot says: `None` for a slot whose pid was never published, otherwise the subsystem its hook recorded.
/// Every doubt answers no, which keeps the hand-off suspicion: no claim at all (the launched process never
/// signed in), a claim past the registry's capacity (a process with nowhere to say what it was), a slot
/// never published (a hook that failed, or one still inside `DllMain`), and a slot with no subsystem
/// recorded. Pure, so each of those is tested without a session.
fn only_console(claims: u32, slots: &[Option<u32>]) -> bool {
    claims != 0
        && uncovered_from_attempts(claims) == 0
        && slots.len() == claims as usize
        && slots.iter().all(|s| *s == Some(SUBSYSTEM_CONSOLE))
}

/// When the process behind `handle` was created, as one FILETIME number, or `None` when the handle
/// does not allow the question (it needs `PROCESS_QUERY_LIMITED_INFORMATION`). The system time at
/// creation, unaffected by later changes to the system clock (MS Learn, `GetProcessTimes` and
/// `PsGetProcessCreateTimeQuadPart`), which is what makes it a process's identity next to a pid that
/// the system recycles.
///
/// # Safety
/// `handle` must be a process handle, or the pseudo-handle of this process.
pub(crate) unsafe fn process_created(handle: HANDLE) -> Option<u64> { unsafe {
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user).ok()?;
    Some(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
}}

/// What a control block found under the session's name turned out to be (R5-D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeldBlock {
    /// Written by this build: an orphan to reclaim.
    Ours,
    /// Written by another version of this tool - another layout, or another size - and still held by a
    /// process that version started.
    OtherVersion,
    /// Not written by this tool at all.
    Foreign,
}

/// Read a surviving block from what can be read of any view, whatever its size: the bytes mapped, and
/// the magic and layout at its start (the first page is always mapped, so both are there to read).
fn held_block(mapped: usize, magic: u64, layout: u32) -> HeldBlock {
    if magic != CTL_MAGIC {
        HeldBlock::Foreign
    } else if layout != CTL_LAYOUT_VERSION || mapped < ctl_size() {
        HeldBlock::OtherVersion
    } else {
        HeldBlock::Ours
    }
}

/// How many bytes the view at `view` maps, as one region of pages, or 0 when the system will not say.
fn mapped_size(view: *const c_void) -> usize {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: a query about an address, which reads nothing at it.
    let n = unsafe { VirtualQuery(Some(view), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
    if n == 0 { 0 } else { info.RegionSize }
}

/// Prepare and start a session on `target` using `spec`, injecting `hook_dll`.
pub fn prepare(spec: &SessionSpec, target: &Target, hook_dll: &Path) -> Result<Prepared, PrepareError> {
    // The session gate, not the plain conversion: a zone the hook's local channels cannot carry, or an
    // instant outside the clock's range, is refused here as well as by the caller (R4-S7, R4-S9). The
    // zone comes back from the same check, so there is no default left to fall back on.
    let (a_fake, tz_bias) = chrono_core::session_instant(&spec.moment).map_err(PrepareError::Moment)?;
    let multiplier = match spec.mode {
        TimeMode::Flow => 1,
        TimeMode::Frozen => 0, // M = 0 holds the wall clock at a_fake
        TimeMode::Multiplier(m) => m,
    };
    let dll_wide = to_wide(&hook_dll.to_string_lossy());
    // Before the lock and the control block, so a target that cannot be given its command line leaves
    // nothing behind to undo.
    let (app, cmdline) = launch_line(target).map_err(PrepareError::Launch)?;

    unsafe {
        // 0. Session lock, before anything shared is touched. Everything below - the decision to
        // reclaim, the zeroing, the anchor writes - happens under it, so no second core can observe
        // a half-initialized session and mistake it for an orphan.
        let lock = take_session_lock()?;

        // 1. Session control memory. CreateFileMapping zero-initializes it.
        let hmap = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE,
            0,
            ctl_size() as u32,
            windows::core::PCWSTR(chrono_ctl::CTL_SECTION_NAME_W.as_ptr()),
        )
        .map_err(|e| PrepareError::Control(win32_detail("CreateFileMappingW", &e)))?;
        // The section name is fixed, so it may survive a prior session. GetLastError right after the
        // create says whether it pre-existed - read it before anything else can reset it.
        let already_existed = GetLastError() == ERROR_ALREADY_EXISTS;
        // The whole section, whatever its size (R5-D3). A section that survived from another version of
        // the tool keeps the size that version gave it, and a view of this build's size used to fail on
        // a smaller one as a bare "MapViewOfFile returned null", before anything could say whose it was.
        let view = MapViewOfFile(hmap, FILE_MAP_ALL_ACCESS, 0, 0, 0);
        if view.Value.is_null() {
            let failed = windows::core::Error::from_thread();
            let _ = CloseHandle(hmap);
            return Err(PrepareError::Control(win32_detail("MapViewOfFile", &failed)));
        }
        // From here every way out releases the view and the mapping on its own (R5-A6).
        let control = held::ControlView::new(hmap, view);
        let ctl = control.ctl();

        // A surviving section can only be an orphan here: we hold the session lock, so no live core
        // owns it - it was left by a killed core whose target self-detached to real time, or by one
        // that exited while the target still held the mapping alive. Reclaim it by zeroing the whole
        // block so no stale PID registry or anchor leaks into the new session. The refusal for a real
        // second session happened at the lock, not here - reading `core_pid` to decide would race
        // with a core that has not written it yet.
        let mut reclaimed = Reclaimed::Nothing;
        if already_existed {
            // ...but "surviving" is not the same as "ours". The name is fixed and any process in
            // this session can create it, so a section already sitting on it was zeroed and used as
            // if we had made it - handing whoever made it a mapping of the session's anchor and its
            // coverage counters for the whole run. The audit is evidence (untouchable rule 4) and
            // the anchor decides what the target sees (rule 2) - neither may come from a block we
            // cannot recognise. A section the OS just created is zero-filled, so a bare name-squat
            // fails this, and so does a block written by a build with a different layout - which is
            // told apart, because it has an ordinary cause and an ordinary cure (R5-D3).
            let magic = std::ptr::read_volatile(std::ptr::addr_of!((*ctl).magic));
            let layout = std::ptr::read_volatile(std::ptr::addr_of!((*ctl).layout));
            match held_block(mapped_size(view.Value), magic, layout) {
                HeldBlock::Ours => {}
                HeldBlock::OtherVersion => {
                    return Err(PrepareError::Control(
                        "the session control block is held by a process started by another version of \
                         Chrono Mock - close the applications that version started, then try again"
                            .into(),
                    ));
                }
                HeldBlock::Foreign => {
                    return Err(PrepareError::Control(
                        "the session control block is held by something that is not this tool - \
                         refusing to reuse it"
                            .into(),
                    ));
                }
            }
            // Read BEFORE the zeroing, which is what makes it the previous session's answer.
            reclaimed = if read_ended(ctl) { Reclaimed::EndedSession } else { Reclaimed::DeadCore };
            std::ptr::write_bytes(ctl as *mut u8, 0, ctl_size());
        }
        // Stamped before the anchor, so anything that outlives this core is recognisable as ours.
        write_header(ctl);

        let start_real = quitp_now();
        let sleep_bias0 = sleep_bias_now();
        // Initialize the duration anchor from the REAL clock (the core is not hooked, so GetTickCount64 and
        // QUIT are genuine). The fake-QUIT base and the real base both start at `start_real`, and the tick
        // count is that QUIT in milliseconds plus an offset chosen so it starts at the real GetTickCount64
        // (R4/10a, F4) - a target's tick count starts near the real uptime. The axis is re-anchored on
        // every set_multiplier so it never rewinds (H-1). Written in the wall anchor's seqlock. The QPC
        // axis (ADR-2 reversal, opt-in) starts fake == real at the current QPC, so elapsed begins at 0.
        let dur_tick_offset = (GetTickCount64() as i64).saturating_sub(start_real.div_euclid(10_000));
        let start_qpc = qpc_now();
        write_anchor_full(
            ctl,
            &FullAnchor {
                a_fake,
                a_real: start_real,
                multiplier,
                dur_tick_offset,
                dur_quit_c0: start_real,
                dur_q0: start_real,
                dur_qpc_c0: start_qpc,
                dur_qpc_q0: start_qpc,
            },
        );
        write_tz_bias(ctl, tz_bias);
        write_scale_dur(ctl, spec.scale_duration);
        write_scale_qpc(ctl, spec.scale_qpc);
        // The pair that names this session (R4-W2), the pid LAST. Reading our own creation time cannot
        // realistically fail, and if it did the 0 would make every hook refuse to join rather than join
        // a session it cannot tell from another - the target's injection then fails, loudly.
        write_core_created(ctl, process_created(GetCurrentProcess()).unwrap_or(0));
        write_core_pid(ctl, GetCurrentProcessId());

        // 2. Launch SUSPENDED so the hook lands before the entry point (the loading the remote thread does
        // for the target still comes first, with its imports' start-up code, R4-N20), and in a job that ends the
        // target with this core until it runs (R4-S1). A core that died while the target was still
        // suspended - killed by the window after its patience ran out, Ctrl+C, a crash - left it
        // suspended for good, holding its executable and the hook library open. Its children stay out
        // of the job.
        let (pi, start_job) = create_in_job(&app, &cmdline, target, CREATE_SUSPENDED, true).map_err(PrepareError::Launch)?;
        // Until it runs, every way out ends it, closes its handles and drops its job (R5-A6, H-2).
        let suspended = held::SuspendedTarget::new(pi, start_job);

        // 2a. Refuse a target this core cannot reach, before trying (R2-S1). The process exists but is
        // still suspended - it has not run an instruction - so ending it here costs the tester
        // nothing and leaves no half-started application behind.
        if let Some((target_bits, core_bits)) = bitness_mismatch(suspended.process()) {
            return Err(PrepareError::BitnessMismatch(target_bits, core_bits));
        }

        // 3. Inject the hook into the suspended target. A failed injection leaves it UNHOOKED and still
        // SUSPENDED, and it is ended on the way out (H-2): resuming - the old "let it die naturally" -
        // would run an unhooked process to completion reading REAL time, a silent orphan the caller
        // never asked to run, and hand back a "session" that substituted nothing. The view goes too (M-2).
        inject(suspended.process(), &dll_wide)?;

        // 4. Find the parent's OWN coverage slot (it published its pid in DllMain, before resume, so
        // this is deterministic) and read the install bitmask. If the hook could not claim a slot
        // (best-effort failure in the target), report no coverage rather than guessing - honest.
        let parent_pid = suspended.pid();
        let parent_slot = find_pid_slot(ctl, parent_pid, None);
        let installed = match parent_slot {
            Some(slot) => read_installed(cov_at(ctl, slot)),
            None => 0,
        };

        // 5. Resume, then wait up to the guard window. WaitForSingleObject returns
        // early if the target exits - the single-instance vanish signal (ADR-4). The
        // install bits are already set in DllMain, so without this a target that
        // vanished right after injection would look like a false "works".
        let t0 = Instant::now();
        // ResumeThread returns the thread's previous suspend count, or u32::MAX (-1) on failure. A failed
        // resume would leave the target SUSPENDED forever while WaitForSingleObject below reads it as
        // "still alive/healthy" (M-3) - a frozen process handed back as a running session that never does
        // anything. It is ended on the way out instead, loudly (the hook's mapped section dies with it).
        if ResumeThread(suspended.thread()) == u32::MAX {
            return Err(PrepareError::Inject("ResumeThread failed - target left suspended".into()));
        }
        // It runs now, so the job lets go of it before the guard window: from here a core that dies
        // leaves the application running, as ADR-14 promises.
        let (pi, start_job) = suspended.into_running();
        let start_job_note = let_go_of(start_job);
        let waited = WaitForSingleObject(pi.hProcess, GUARD_MS);
        let coverage = match parent_slot {
            Some(slot) => {
                gather_coverage(cov_at(ctl, slot), installed, spec.scale_duration, spec.scale_qpc)
            }
            None => Coverage::default(),
        };
        let vanished_lived_ms = if waited == WAIT_TIMEOUT {
            None
        } else {
            Some(t0.elapsed().as_millis() as u64)
        };

        // 6. Hand back a live session. We keep the control section mapped and the process handle
        // open - only the thread handle is released here. Session::end releases the rest - the
        // target's own mapped view keeps the control section alive regardless. The parent's slot is
        // marked reported, since its coverage is handed back right here in `Prepared` - without that
        // the first child poll would emit the parent a second time.
        let _ = CloseHandle(pi.hThread);
        let root_image = image_path_of(pi.hProcess);
        let mut reported_slots = vec![false; MAX_COV_PIDS];
        if let Some(slot) = parent_slot {
            reported_slots[slot] = true;
        }
        let (ctl_addr, hmap) = control.into_session();
        let session = Session {
            pid: parent_pid,
            ctl_addr,
            hmap,
            hprocess: pi.hProcess,
            start_real,
            fake_elapsed_before: std::cell::Cell::new(0),
            rate_segment_real0: std::cell::Cell::new(start_real),
            sleep_bias0,
            tz_bias,
            scale_duration: spec.scale_duration,
            scale_qpc: spec.scale_qpc,
            reported_slots,
            consumed_children: vec![0; MAX_COV_PIDS],
            family: family::Family::new(MAX_COV_PIDS, parent_slot),
            root_image,
            _lock: lock,
        };
        Ok(Prepared { coverage, session, vanished_lived_ms, reclaimed, start_job_note })
    }
}

/// Let the start-up job go of a target that runs (R4-S1), and say what the caller has to know when it
/// could not: the target started outside a job, or it could not be let out of it and ends with this
/// core. Such a job is kept open rather than closed, because closing it would end the application now.
fn let_go_of(job: JobOrWhy) -> Option<String> {
    match job {
        Ok(job) => match job.let_go() {
            Ok(()) => None,
            Err((held, why)) => {
                std::mem::forget(held);
                Some(format!(
                    "the application could not be let out of the job it started in ({why}), so it closes when this core exits"
                ))
            }
        },
        Err(why) => Some(format!(
            "the application started outside a job ({why}) - had this core stopped before it ran, it would have been left suspended"
        )),
    }
}

/// A Win32 failure as a sentence a person can act on: what was being done, why it failed when we can
/// say, and the raw code for a bug report.
///
/// Written because the alternative was on screen. These failures used to be formatted with `{e:?}`,
/// which is Rust's DEBUG rendering of `windows::core::Error` - so `chrono run` on a path that does not
/// exist answered with
///
/// ```text
/// chrono core: CreateProcessW: Error { code: HRESULT(0x80070003), message: "..." }
/// ```
///
/// a struct dump rather than a sentence. Worse for a tool whose CLI is English without exception
/// (rule 15): the `message` field is filled in by Windows IN THE SYSTEM'S LANGUAGE, so on this
/// machine an English-only command line printed a Polish sentence. Measured, not supposed.
///
/// So the OS text is dropped and the code is kept. The handful of codes a tester actually meets get a
/// clause of our own - the rest are named by their number, which is what a bug report needs anyway.
fn win32_detail(operation: &str, e: &windows::core::Error) -> String {
    format!("{operation} failed{}", win32_reason(e))
}

/// The tail of `win32_detail` alone - the clause of ours when there is one, and always the code - for a
/// sentence that names what failed in its own words.
fn win32_reason(e: &windows::core::Error) -> String {
    let code = e.code().0 as u32;
    let because = match code {
        0x8007_0002 => " - the file does not exist",
        0x8007_0003 => " - the path does not exist",
        0x8007_0005 => " - access was denied",
        0x8007_0020 => " - the file is in use by another process",
        // ERROR_BAD_EXE_FORMAT. Worth naming: it is what a corrupt executable and a 16-bit or
        // non-native binary both come back as, and neither reads as "bad format" to a tester.
        0x8007_00C1 => " - the file is not a runnable executable for this machine",
        // The refusals a tester meets most and could not read from a bare number (R5-D3). Meanings from
        // MS Learn "System Error Codes": ERROR_ELEVATION_REQUIRED, ERROR_FILENAME_EXCED_RANGE,
        // ERROR_EXE_MACHINE_TYPE_MISMATCH, ERROR_VIRUS_INFECTED, ERROR_VIRUS_DELETED and
        // ERROR_ACCESS_DISABLED_BY_POLICY. An installer with a requireAdministrator manifest is the
        // ordinary way to the first.
        0x8007_02E4 => " - the program asks to run as administrator, so start Chrono Mock as administrator to test it",
        0x8007_00CE => " - the path or command line is too long",
        0x8007_00D8 => " - the program is built for a different kind of processor or Windows",
        0x8007_00E1 => " - antivirus blocked the file as a virus or unwanted software",
        0x8007_00E2 => " - antivirus removed the file as a virus or unwanted software",
        0x8007_04EC => " - a system policy blocks this program",
        _ => "",
    };
    format!("{because} (0x{code:08X})")
}

/// Quote one argument so the target's CRT (CommandLineToArgvW) parses it back verbatim, per the
/// standard Windows rule: double-quote when the argument is empty or holds whitespace or a quote,
/// and double the run of backslashes that precedes a quote or the closing quote. Without this an
/// `--args` value containing a space or quote would reach the target split or mangled (P9).
fn quote_arg(arg: &str) -> String {
    let needs_quotes = arg.is_empty()
        || arg
            .chars()
            .any(|c| c == ' ' || c == '\t' || c == '\n' || c == '\x0b' || c == '"');
    if !needs_quotes {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Backslashes before a quote must be doubled to stay literal, then escape the quote.
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // Trailing backslashes precede the closing quote, so double them too.
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
    out
}

/// Quote a target path for the command line. Reuses `quote_arg` - one escaping rule for the whole
/// line, not two - but ALWAYS ends up wrapped: `CreateProcessW` resolves an unquoted path containing
/// spaces ambiguously, so the quotes are not optional here even when the escaping is a no-op.
fn quote_path(path: &str) -> String {
    let quoted = quote_arg(path);
    if quoted.starts_with('"') {
        return quoted; // already wrapped and escaped
    }

    // quote_arg leaves a token with no space or quote bare, so the wrapping is ours to add - and a
    // trailing backslash would then escape the closing quote (`"C:\dir\"` reads as a path plus an
    // unterminated quote). Double that run, the same rule quote_arg applies inside a wrapped argument.
    let trailing = quoted.chars().rev().take_while(|&c| c == '\\').count();
    let mut out = String::with_capacity(quoted.len() + trailing + 2);
    out.push('"');
    out.push_str(&quoted);
    for _ in 0..trailing {
        out.push('\\');
    }

    out.push('"');
    out
}

/// The `"path" arg1 arg2` command line as text, each argument quoted with `quote_arg` so a target
/// argument with spaces or quotes survives the round trip through CreateProcess.
fn command_line_string(path: &str, args: &[String]) -> String {
    // The path goes through the same quoting as the arguments. It was wrapped in bare quotes, so a path
    // containing one would have ended the quoted section early and split the command line somewhere the
    // caller never intended. Windows does not allow a quote in a file name, so this is defence rather
    // than a fix - but one rule for every part of the line beats two, and quote_arg leaves an ordinary
    // path exactly as the bare wrapping did.
    let mut s = String::with_capacity(path.len() + 2);
    s.push_str(&quote_path(path));
    for a in args {
        s.push(' ');
        s.push_str(&quote_arg(a));
    }
    s
}

/// Build a mutable command line: `"path" arg1 arg2`, wide-encoded for CreateProcessW.
fn build_command_line(path: &str, args: &[String]) -> Vec<u16> {
    to_wide(&command_line_string(path, args))
}

/// Inject the hook with Windows's error window off while the target loads, and back the way the target
/// inherited it before its entry point (R4-S6, `loader::QuietLoader`). On `Err` the caller ends the
/// target, which has not reached its entry point, so the mode is not given back on that road.
unsafe fn inject(hproc: HANDLE, dll_wide: &[u16]) -> Result<(), PrepareError> { unsafe {
    let quiet = loader::QuietLoader::begin(hproc);
    load_hook(hproc, dll_wide)?;
    match quiet {
        Some(quiet) => quiet.end().map_err(PrepareError::Inject),
        None => Ok(()),
    }
}}

/// Manual LoadLibrary injection: write the DLL path into the target and run `LoadLibraryW` there on a
/// remote thread. Returns `Err` on any failure, INCLUDING a `LoadLibraryW` that returned NULL in the
/// target - the caller then terminates the still-suspended target rather than resume an unhooked process
/// reading real time (H-2) - and a target the loader ended before the hook got its turn (R4-S6).
unsafe fn load_hook(hproc: HANDLE, dll_wide: &[u16]) -> Result<(), PrepareError> { unsafe {
    let bytes = dll_wide.len() * 2;
    let remote = VirtualAllocEx(hproc, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if remote.is_null() {
        return Err(PrepareError::Inject(win32_detail("VirtualAllocEx", &windows::core::Error::from_thread())));
    }
    // Free the remote page on EVERY failure below, not just the write error (L-6). The target is still
    // alive here (the caller terminates it on `Err`), so an un-freed page would leak in its address space.
    let fail = |msg: String| -> PrepareError {
        let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
        PrepareError::Inject(msg)
    };
    if let Err(e) = WriteProcessMemory(hproc, remote, dll_wide.as_ptr() as *const c_void, bytes, None) {
        return Err(fail(win32_detail("WriteProcessMemory", &e)));
    }
    let k32 = match GetModuleHandleA(s!("kernel32.dll")) {
        Ok(h) => h,
        Err(e) => return Err(fail(win32_detail("GetModuleHandleA", &e))),
    };
    let loadlib = match GetProcAddress(k32, s!("LoadLibraryW")) {
        Some(f) => f,
        None => return Err(fail("no LoadLibraryW export".into())),
    };
    let start: LPTHREAD_START_ROUTINE = Some(std::mem::transmute::<
        unsafe extern "system" fn() -> isize,
        unsafe extern "system" fn(*mut c_void) -> u32,
    >(loadlib));
    let hthread = match CreateRemoteThread(hproc, None, 0, start, Some(remote as *const c_void), 0, None) {
        Ok(h) => h,
        Err(e) => return Err(fail(win32_detail("CreateRemoteThread", &e))),
    };

    // Bounded wait (M-1): a hung DllMain (loader lock) must not hang prepare forever. The thread alone is
    // enough to wait on: a target that ends takes its threads with it, so the thread is signalled no
    // later than the process.
    let waited = WaitForSingleObject(hthread, INJECT_TIMEOUT_MS);
    let wait_error = GetLastError();
    let wait = match waited {
        WAIT_OBJECT_0 => loader::Wait::ThreadDone,
        WAIT_TIMEOUT => loader::Wait::TimedOut,
        _ => loader::Wait::Failed,
    };
    // Signalled first, then the code: read the other way round, a target that ends between the two
    // reads would be a signalled process with the code of a live one.
    let signalled = WaitForSingleObject(hproc, 0) == WAIT_OBJECT_0;
    let mut process_code: u32 = 0;
    let process = loader::ProcessAfter {
        signalled,
        code: GetExitCodeProcess(hproc, &mut process_code).is_ok().then_some(process_code),
    };
    // Only a finished thread's code is a result - a running one reads 259.
    let mut thread_code: u32 = 0;
    let thread_code = (wait == loader::Wait::ThreadDone && GetExitCodeThread(hthread, &mut thread_code).is_ok())
        .then_some(thread_code);
    let outcome = loader::outcome(wait, process, thread_code);
    if loader::page_released_safely(outcome) {
        let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
    }
    let _ = CloseHandle(hthread);

    match outcome {
        loader::Outcome::Loaded => Ok(()),
        // Two causes give the same NULL since R4-D18: the library did not load, or it loaded, found a
        // control block it could not confirm as this live session's, and unloaded itself. The target
        // keeps the loader's reason to itself, so the two this side can establish are asked here, on
        // this path only (R5-D3): a file the core cannot open, and a file built for another machine.
        loader::Outcome::LibraryNotLoaded => Err(PrepareError::Inject(match hook_library_problem(dll_wide) {
            Some(problem) => format!("LoadLibraryW returned NULL in the target - {problem}"),
            None => "LoadLibraryW returned NULL in the target (the hook DLL failed to load, or would not join a \
                     session it could not confirm)"
                .into(),
        })),
        loader::Outcome::TargetEnded(code) => Err(PrepareError::EndedLoading(code)),
        loader::Outcome::TargetEndedUnread => Err(PrepareError::Inject(
            "the target ended while Windows was loading it, and its exit code could not be read".into(),
        )),
        loader::Outcome::TimedOut => Err(PrepareError::Inject(format!(
            "LoadLibraryW did not return within {INJECT_TIMEOUT_MS} ms - the target's loading is stuck (a \
             library's start-up code waiting, or a loader lock)"
        ))),
        loader::Outcome::WaitFailed => Err(PrepareError::Inject(win32_detail(
            "waiting for the remote LoadLibraryW thread",
            &windows::core::Error::from_hresult(wait_error.to_hresult()),
        ))),
    }
}}

/// What this side can establish about a hook library `LoadLibraryW` would not load in the target: that
/// the core cannot open it either (removed, blocked by antivirus, access denied), or that it is built
/// for another machine than this core and its target run as - a package with mixed files. `None` when
/// neither holds, and the plain message stands.
fn hook_library_problem(dll_wide: &[u16]) -> Option<String> {
    use std::io::Read;
    use std::os::windows::ffi::OsStringExt;
    let path = std::ffi::OsString::from_wide(dll_wide.strip_suffix(&[0]).unwrap_or(dll_wide));
    let mut header = [0u8; 1024];
    let read = std::fs::File::open(&path).and_then(|mut file| file.read(&mut header));
    let read = match read {
        Ok(n) => n,
        Err(e) => {
            let code = e.raw_os_error().unwrap_or(0) as u32;
            let error = windows::core::Error::from_hresult(windows::Win32::Foundation::WIN32_ERROR(code).to_hresult());
            return Some(format!("the hook library could not be opened{}", win32_reason(&error)));
        }
    };
    let library = pe_machine(&header[..read])?;
    // SAFETY: the pseudo-handle of this process.
    let core = unsafe { process_machine(GetCurrentProcess()) }?;
    (library != core).then(|| {
        format!(
            "the hook library is built for {}, but this core runs as {} - the files of the package are mixed",
            machine_label(library),
            machine_label(core)
        )
    })
}

/// The machine a PE file is built for, read from its headers, or `None` when `bytes` does not hold them.
fn pe_machine(bytes: &[u8]) -> Option<u16> {
    if bytes.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32::from_le_bytes(bytes.get(0x3C..0x40)?.try_into().ok()?) as usize;
    if bytes.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let at = pe.checked_add(4)?;
    Some(u16::from_le_bytes(bytes.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    /// A process started here names this one as its parent, and a pid nobody holds names none.
    #[test]
    fn a_process_names_the_one_that_started_it_as_its_parent() {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let path = format!(r"{system_root}\System32\PING.EXE");
        let args = ["-n".to_string(), "30".into(), "127.0.0.1".into()];
        let target = crate::Target { path: &path, args: &args, cwd: None, env: &[], stdio: crate::TargetStdio::Discarded };
        let child = crate::launch_plain(&target).expect("ping launches");
        assert_eq!(super::process_parent(child.pid), Some(std::process::id()));
        assert_eq!(super::process_parent(u32::MAX - 5), None);
    }

    /// A launcher that signs a child in and ends between the two looks: running and alone at the first
    /// look, ended with the child in the registry from the second on. Each look counts itself.
    struct LauncherWorld {
        looks: std::cell::Cell<u32>,
    }

    fn launcher_runs(world: &LauncherWorld) -> bool {
        let look = world.looks.get();
        world.looks.set(look + 1);
        look == 0
    }

    fn child_signed_in(world: &mut LauncherWorld) -> bool {
        let look = world.looks.get();
        world.looks.set(look + 1);
        look >= 1
    }

    /// A ring entry claimed and never written stops the walk while its writer may still run, and is
    /// stepped over once it cannot (R4-N18): the children after it used to wait for it for good.
    #[test]
    fn a_hole_left_by_a_parent_that_ended_does_not_hold_back_the_children_after_it() {
        let ring = [5, 0, 7];
        let read = |at: usize| ring[at];
        assert_eq!(super::ring_pids(0, 3, true, read), (vec![5], 1), "a running writer is waited for");
        assert_eq!(super::ring_pids(1, 3, true, read), (vec![], 1), "and waited for on the next poll too");
        assert_eq!(super::ring_pids(0, 3, false, read), (vec![5, 7], 3), "an ended writer's hole is stepped over");
        assert_eq!(super::ring_pids(3, 3, false, read), (vec![], 3), "nothing past what the ring names");
        assert_eq!(super::ring_pids(0, 2, true, |at| [9, 8][at]), (vec![9, 8], 2));
    }

    fn entry(pid: u32, parent: u32, image: &str) -> super::tree::ProcessEntry {
        super::tree::ProcessEntry { pid, parent, image: image.to_string() }
    }

    fn described(pid: u32) -> (Option<String>, Option<String>) {
        (Some(format!("p{pid}.exe")), Some(format!("p{pid}.exe --flag")))
    }

    /// An uncovered child is named only while the snapshot shows the family started it (R4-N13). A
    /// stranger under its pid is never asked, so its name and command line cannot stand in for the
    /// child's.
    #[test]
    fn a_stranger_under_an_uncovered_child_s_pid_does_not_lend_it_a_name() {
        let known: HashSet<u32> = [10, 11].into_iter().collect();
        let list = [entry(20, 11, "child.exe"), entry(21, 4, "stranger.exe")];
        let named = super::name_if_ours(20, 11, &known, Some(&list), described);
        assert_eq!(named.image.as_deref(), Some("p20.exe"));
        assert_eq!(named.command_line.as_deref(), Some("p20.exe --flag"));
        let stranger = super::name_if_ours(21, 11, &known, Some(&list), |_| panic!("a stranger was asked"));
        assert_eq!((stranger.pid, stranger.parent_pid, stranger.image, stranger.command_line), (21, 11, None, None));
        let ended = super::name_if_ours(22, 11, &known, Some(&list), |_| panic!("an ended child was asked"));
        assert_eq!((ended.image, ended.command_line), (None, None));
        let unread = super::name_if_ours(23, 11, &known, None, described);
        assert_eq!(unread.image.as_deref(), Some("p23.exe"), "an unreadable snapshot names as before");
    }

    /// The family is asked about in the order that cannot miss a child (R4-N11), and the registry is read
    /// every time. The same world asked registry first reads as a family that has ended.
    #[test]
    fn a_child_signed_in_just_before_its_launcher_ended_keeps_the_family_alive() {
        let mut world = LauncherWorld { looks: std::cell::Cell::new(0) };
        assert!(super::alive_in_order(&mut world, launcher_runs, child_signed_in));
        assert_eq!(world.looks.get(), 2, "the registry was not read while the launcher ran");

        let mut world = LauncherWorld { looks: std::cell::Cell::new(0) };
        let others = child_signed_in(&mut world);
        let registry_first = others || launcher_runs(&world);
        assert!(!registry_first, "this world does not reproduce the race the order is there for");
    }

    /// A zero character anywhere in what `CreateProcessW` reads is refused before anything starts, for
    /// a program as for a script (R4-N16): the program used to get the line up to it and nothing after.
    /// Nothing is launched here - the refusal comes first, and the control proves the same target
    /// without the character gets past it.
    #[test]
    fn a_zero_character_in_a_launch_is_refused_before_anything_starts() {
        let program = r"C:\no-such-dir\app.exe";
        let clean = ["one".to_string(), "two".to_string()];
        let cut = ["one".to_string(), "tw\0o".to_string()];
        let target = |path, args, cwd| super::Target { path, args, cwd, env: &[], stdio: super::TargetStdio::Shared };

        for (label, t) in [
            ("argument", target(program, &cut[..], None)),
            ("path", target("C:\\app\0.exe", &clean[..], None)),
            ("working folder", target(program, &clean[..], Some("C:\\work\0dir"))),
            ("script argument", target(r"C:\no-such-dir\run.bat", &cut[..], None)),
        ] {
            let e = super::launch_line(&t).expect_err(label);
            assert!(e.contains("zero character"), "{label}: {e}");
        }
        assert!(super::launch_line(&target(program, &clean[..], Some(r"C:\work"))).is_ok());
    }

    /// The shell every Windows has, asked one question about its environment and answering with
    /// its exit code - no pipes, no output, the one oracle a plain launch offers.
    fn cmd_exit_if_defined(name: &str, env: &[(String, String)]) -> i32 {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let path = format!(r"{system_root}\System32\cmd.exe");
        let args = vec!["/c".to_string(), format!("if defined {name} (exit 3) else (exit 0)")];
        let target = super::Target { path: &path, args: &args, cwd: None, env, stdio: super::TargetStdio::Shared };
        let child = super::launch_plain(&target)
            .expect("cmd.exe launches");
        child.wait_exit(10_000).expect("cmd.exe exits within ten seconds")
    }

    /// This thread without the debug privilege, until dropped: an elevated shell enables it, and a
    /// thread that holds it opens any process whatever its DACL. A copy of the process token on this
    /// thread alone, so the tests running beside it keep theirs.
    struct NoDebugPrivilege(HANDLE);

    impl NoDebugPrivilege {
        fn on_this_thread() -> NoDebugPrivilege {
            use windows::Win32::Foundation::LUID;
            use windows::Win32::Security::{
                AdjustTokenPrivileges, ImpersonateSelf, LookupPrivilegeValueW, SecurityImpersonation, LUID_AND_ATTRIBUTES,
                SE_DEBUG_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_PRIVILEGES_ATTRIBUTES, TOKEN_QUERY,
            };
            use windows::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};
            let mut luid = LUID::default();
            // SAFETY: every out-parameter is a local of this frame or the guard's own handle. The guard
            // exists from the moment the thread impersonates, so a call that fails after it - and
            // panics the test - still sends the thread back to the process token and closes the handle
            // (CodeRabbit on #110).
            unsafe {
                ImpersonateSelf(SecurityImpersonation).expect("a token of this thread's own");
                let mut guard = NoDebugPrivilege(HANDLE::default());
                OpenThreadToken(GetCurrentThread(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, true, &mut guard.0).expect("that token");
                LookupPrivilegeValueW(None, SE_DEBUG_NAME, &mut luid).expect("the debug privilege's id");
                let off = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: TOKEN_PRIVILEGES_ATTRIBUTES(0) }],
                };
                AdjustTokenPrivileges(guard.0, false, Some(&off), 0, None, None).expect("the privilege switched off");
                guard
            }
        }
    }

    impl Drop for NoDebugPrivilege {
        fn drop(&mut self) {
            // SAFETY: the thread goes back to the process token, then the copy's handle, when there is
            // one, is closed once.
            unsafe {
                let _ = windows::Win32::Security::RevertToSelf();
                if !self.0.is_invalid() {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    /// R5-P24: a process that refuses to be opened is running, not gone. The child here is started
    /// with an empty DACL, so nobody without the debug privilege enabled may open it - the shape of an
    /// elevated or another user's process seen from an ordinary one. The open was taken for "gone",
    /// which removed the profile of a live elevated Chromium session and ended a session's watch on an
    /// elevated member of its family. The handle `CreateProcessW` hands back keeps full access, so the
    /// test ends its own child.
    #[test]
    fn a_process_that_refuses_to_be_opened_is_alive_until_it_ends() {
        use windows::core::{PCWSTR, PWSTR};
        use windows::Win32::Security::{
            InitializeAcl, InitializeSecurityDescriptor, SetSecurityDescriptorDacl, ACL, ACL_REVISION, PSECURITY_DESCRIPTOR,
            SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
        };
        use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
        use windows::Win32::System::Threading::{
            CreateProcessW, TerminateProcess, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
        };

        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let app: Vec<u16> = format!(r"{system_root}\System32\PING.EXE").encode_utf16().chain([0]).collect();
        let mut line: Vec<u16> = "PING.EXE -n 30 127.0.0.1".encode_utf16().chain([0]).collect();
        let mut acl = ACL::default();
        let mut sd = SECURITY_DESCRIPTOR::default();
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: the descriptor and the empty ACL live on this frame for the whole call, the ACL is
        // given its own size, and the process handles are closed below on every path that has them.
        unsafe {
            InitializeAcl(&mut acl, std::mem::size_of::<ACL>() as u32, ACL_REVISION).expect("an empty ACL");
            let psd = PSECURITY_DESCRIPTOR(std::ptr::from_mut(&mut sd).cast());
            InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION).expect("a security descriptor");
            SetSecurityDescriptorDacl(psd, true, Some(&acl), false).expect("the empty DACL set on it");
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: psd.0,
                bInheritHandle: false.into(),
            };
            let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
            CreateProcessW(PCWSTR(app.as_ptr()), Some(PWSTR(line.as_mut_ptr())), Some(&sa), None, false, CREATE_NO_WINDOW, None, PCWSTR::null(), &si, &mut pi)
                .expect("ping starts");
            let _ordinary = NoDebugPrivilege::on_this_thread();
            let opened = OpenProcess(PROCESS_SYNCHRONIZE, false, pi.dwProcessId);
            let refused = match opened {
                Ok(h) => {
                    let _ = CloseHandle(h);
                    false
                }
                Err(_) => true,
            };
            let alive_while_running = super::process_is_alive(pi.dwProcessId);
            let _ = TerminateProcess(pi.hProcess, 1);
            let _ = WaitForSingleObject(pi.hProcess, 5_000);
            // The process object, and its empty DACL, lives as long as a handle to it does - the console
            // host of a console program holds one for a moment after the end - and the pid is free once
            // the last one closes. Until then the refusal reads as running, the safe side.
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
            let alive_after = !gone_soon(pi.dwProcessId);
            assert!(refused, "the empty DACL must refuse the open, or this test proves nothing");
            assert!(alive_while_running, "a process that refuses the open is running, not gone");
            assert!(!alive_after, "and once it has ended it is gone");
        }
    }

    /// Whether a process is gone within five seconds - ending a job is the kernel's to schedule.
    fn gone_soon(pid: u32) -> bool {
        (0..50).any(|_| {
            let gone = !super::process_is_alive(pid);
            if !gone {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            gone
        })
    }

    /// R4-N15: a plain child and what it started end together, and without any code of ours running.
    /// Closing the job's handle is what the kernel does for an owner that was killed, so the job is
    /// dropped on its own here, the process untouched. Ending only the process we started used to leave
    /// its children running, and a killed owner ended nothing at all.
    #[test]
    fn a_plain_child_and_what_it_started_end_when_its_job_closes() {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let path = format!(r"{system_root}\System32\cmd.exe");
        let ping = format!(r"{system_root}\System32\PING.EXE");
        let args = ["/c".to_string(), ping, "-n".into(), "30".into(), "127.0.0.1".into()];
        let target = super::Target { path: &path, args: &args, cwd: None, env: &[], stdio: super::TargetStdio::Discarded };
        let mut child = super::launch_plain(&target).expect("cmd.exe launches");
        assert!(child.job_note().is_none(), "the child runs without a job: {:?}", child.job_note());

        // The grandchild, once cmd.exe has started it.
        let Some(grandchild) = tree::tests::started_by(child.pid, "PING.EXE") else {
            panic!("cmd.exe did not start ping within five seconds");
        };

        drop(child.job.take());
        let (root_gone, grandchild_gone) = (gone_soon(child.pid), gone_soon(grandchild));
        child.terminate();
        assert!(root_gone, "closing the job left the process we started running");
        assert!(grandchild_gone, "closing the job left the process it started running");
    }

    /// `cmd.exe` that starts `PING.EXE` with `start` and ends at once: a launcher that handed its work
    /// over, the shape of an installed Chromium browser.
    fn handing_over_to_ping(count: &str) -> PlainChild {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let path = format!(r"{system_root}\System32\cmd.exe");
        let ping = format!(r"{system_root}\System32\PING.EXE");
        let args = ["/c".to_string(), "start".into(), "\"\"".into(), "/b".into(), ping, "-n".into(), count.into(), "127.0.0.1".into()];
        let target = super::Target { path: &path, args: &args, cwd: None, env: &[], stdio: super::TargetStdio::Discarded };
        let child = super::launch_plain(&target).expect("cmd.exe launches");
        assert!(child.wait_exit(5_000).is_some(), "cmd.exe hands over and ends");
        child
    }

    /// The process a launcher handed its work to, found by its parent.
    fn handed_to(child: &PlainChild) -> u32 {
        tree::tests::started_by(child.pid, "PING.EXE").expect("cmd.exe started ping within five seconds")
    }

    /// The wait covers the process a launcher handed over to, not only the launcher. Without the end, so
    /// the wait is all there is: `PING.EXE -n 2` ends on its own after about a second, and the wait
    /// returns once it has, not at once because the launcher had ended. Waiting for the launcher alone
    /// waited for nothing, and a browser's profile was removed while the browser still held its files.
    #[test]
    fn the_wait_covers_the_process_a_launcher_handed_over_to() {
        let child = handing_over_to_ping("2");
        let handed_to = handed_to(&child);
        let (members, unconfirmed) = child.job.as_ref().expect("the child runs in a job").members();
        assert_eq!(unconfirmed, 0, "every process of the job could be opened");
        assert!(super::process_is_alive(handed_to), "ping ended before the wait began, so the wait proves nothing");

        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        assert!(child.ended_with(&members, deadline), "ping ends on its own well inside ten seconds");
        assert!(!super::process_is_alive(handed_to), "the wait returned while the process it handed over to still ran");
    }

    /// Ending a launcher that handed over ends the process it handed over to and waits for it.
    #[test]
    fn ending_a_launcher_that_handed_over_waits_for_the_process_it_handed_over_to() {
        let child = handing_over_to_ping("30");
        let handed_to = handed_to(&child);
        assert!(child.terminate_and_wait(5_000), "the wait ran out with a process of the job still running");
        assert!(!super::process_is_alive(handed_to), "the end left the process it handed over to running");
    }
    /// R4-S5: a refusal ends the family, not the launched process alone. A live tree started without the
    /// hook stands in for it - `cmd.exe` and the `PING.EXE` it started - so the family can only be found
    /// the way a process the hook never saw is found, through the process list. A process started beside
    /// it, by the same parent, has to survive: it names this test process as its parent, as the tree's
    /// root does, and ending it would be ending somebody else's work.
    #[test]
    fn a_refused_family_ends_with_what_it_started_and_nothing_beside_it() {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let path = format!(r"{system_root}\System32\cmd.exe");
        let ping = format!(r"{system_root}\System32\PING.EXE");
        let args = ["/c".to_string(), ping.clone(), "-n".into(), "30".into(), "127.0.0.1".into()];
        let target = super::Target { path: &path, args: &args, cwd: None, env: &[], stdio: super::TargetStdio::Discarded };
        let child = super::launch_plain(&target).expect("cmd.exe launches");
        let beside_args = ["-n".to_string(), "30".into(), "127.0.0.1".into()];
        let beside_target =
            super::Target { path: &ping, args: &beside_args, cwd: None, env: &[], stdio: super::TargetStdio::Discarded };
        let beside = super::launch_plain(&beside_target).expect("ping launches");

        let Some(grandchild) = tree::tests::started_by(child.pid, "PING.EXE") else {
            panic!("cmd.exe did not start ping within five seconds");
        };
        // The list shows a process from the moment its object exists, before its creator has let its
        // first thread run. Ending the creator in that stretch makes the system end the half-made child
        // too (exit status 0xC000010A, measured), so ending the root alone passed this test now and then.
        // A second later the child runs on its own, and only what ends it on purpose can end it.
        std::thread::sleep(std::time::Duration::from_secs(1));
        assert!(super::process_is_alive(grandchild), "ping ended on its own before the family was ended");

        let end = ending::end_family(child.handle, child.pid, &[], &[]);
        assert!(end.incomplete.is_none(), "{:?}", end.incomplete);
        assert!(end.left_running.is_empty(), "the refusal left these running: {:?}", end.left_running);
        assert!(gone_soon(child.pid), "the launched process was not ended");
        assert!(gone_soon(grandchild), "the process it started was not ended");
        assert!(beside.is_alive(), "a process beside the family was ended");
    }

    #[test]
    fn a_launched_child_sees_the_variable_the_block_added_and_the_environment_it_inherited() {
        let extra = vec![("CHRONO_MECH_PROBE".to_string(), "1".to_string())];
        // The block carries the new name...
        assert_eq!(cmd_exit_if_defined("CHRONO_MECH_PROBE", &extra), 3);
        // ...and everything the parent had - a block that dropped SystemRoot would start a child
        // that cannot find its own system folder.
        assert_eq!(cmd_exit_if_defined("SystemRoot", &extra), 3);
        // Reversal: without the block the name is not there, so the variable is the block's doing.
        assert_eq!(cmd_exit_if_defined("CHRONO_MECH_PROBE", &[]), 0);
    }

    use super::*;
    // Only the QPC-channel test needs this bit, so it is imported here rather than in the lib.
    use chrono_ctl::CH_QPC;
    // Same for the winmm-clock test: the bit, its counter index, and the counter writer.
    use chrono_ctl::{bump_calls, set_late_installed, CH_TIMEGETTIME, IDX_CONNECT, IDX_TIMEGETTIME};
    // The failed-detour test pairs the winmm clock with winmm's observed timer.
    use chrono_ctl::CH_TIMESETEVENT;

    /// R2-X2. The projection the core reports has to be the one the target sees - the hook clamps at
    /// the end of the range, so this must clamp there too. Before it did, a session at the edge showed
    /// the tester a date fifty thousand years away from the one the app was reading.
    #[test]
    fn the_reported_clock_clamps_where_the_hook_clamps_and_never_wraps() {
        let max = chrono_ctl::FAKE_WALL_MAX;

        // Ordinary session: the anchor plus the elapsed real time times the rate, untouched.
        assert_eq!(project_fake_ft(1_000, 100, 160, 60), 1_000 + 60 * 60);
        // Frozen: the rate is zero, so the clock stands at the anchor.
        assert_eq!(project_fake_ft(1_000, 100, 10_000_000, 0), 1_000);

        // A rate big enough to run past the end of the range holds AT the edge - never a wrapped
        // number, and above all never a NEGATIVE one, which is what a wrapping multiply produced.
        let far = project_fake_ft(max - 10_000_000, 0, 1_000_000, chrono_core::MULTIPLIER_MAX);
        assert_eq!(far, max);
        assert!(far > 0, "a clock that wrapped past i64::MAX reads as a date before year one");

        // Starting ON the edge stays on it rather than stepping off the end.
        assert_eq!(project_fake_ft(max, 0, 10_000_000, 1), max);

        // And the clamp leaves room for the largest zone bias, so the local-time channels can express
        // the same instant without overflowing (R2-X7 - that overflow sent GetLocalTime back to the
        // REAL clock while the UTC channels stayed fake).
        assert!(i64::MAX - max >= chrono_ctl::MAX_ZONE_BIAS_TICKS);
    }

    #[test]
    fn this_core_reports_its_own_machine_and_matches_itself() {
        // The gate compares the target's machine with our own, so our own has to be knowable at all -
        // and comparing this process against itself must never read as a mismatch (R2-S1).
        let me = unsafe { process_machine(GetCurrentProcess()) };
        let me = me.expect("IsWow64Process2 must answer for our own process");
        let expected = if cfg!(target_pointer_width = "64") { "x64" } else { "x86" };
        assert_eq!(machine_label(me), expected);
        assert!(unsafe { bitness_mismatch(GetCurrentProcess()) }.is_none());
    }

    #[test]
    fn machine_labels_never_guess_at_an_unknown_machine() {
        assert_eq!(machine_label(IMAGE_FILE_MACHINE_I386.0), "x86");
        assert_eq!(machine_label(IMAGE_FILE_MACHINE_AMD64.0), "x64");
        assert_eq!(machine_label(IMAGE_FILE_MACHINE_ARM64.0), "arm64");
        assert_eq!(machine_label(0xBEEF), "an unrecognised machine");
    }

    #[test]
    fn quote_arg_leaves_simple_tokens_bare() {
        assert_eq!(quote_arg("simple"), "simple");
        assert_eq!(quote_arg("C:\\path\\file"), "C:\\path\\file"); // backslashes without a quote stay
        assert_eq!(quote_arg("a-b_c.txt"), "a-b_c.txt");
    }

    #[test]
    fn quote_arg_wraps_and_escapes() {
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a\"b"), "\"a\\\"b\""); // an embedded quote is escaped
        // A trailing backslash is doubled before the closing quote so it stays literal.
        assert_eq!(quote_arg("with space\\"), "\"with space\\\\\"");
    }

    #[test]
    fn command_line_quotes_each_arg() {
        let cl = command_line_string("C:\\dir\\app.exe", &["a b".to_string(), "c".to_string()]);
        assert_eq!(cl, "\"C:\\dir\\app.exe\" \"a b\" c");
    }

    /// S-36. The path was wrapped in bare quotes, so one inside it ended the quoted section early and
    /// split the command line somewhere the caller never meant. Windows forbids a quote in a file name,
    /// so this is defence - but the path now goes through the same escaping as every argument, and an
    /// ordinary path still comes out exactly as the bare wrapping produced it.
    #[test]
    fn a_quote_in_the_target_path_cannot_break_the_command_line() {
        assert_eq!(quote_path("C:\\dir\\app.exe"), "\"C:\\dir\\app.exe\"");
        assert_eq!(quote_path("C:\\my apps\\app.exe"), "\"C:\\my apps\\app.exe\"");
        // The quote is escaped rather than closing the wrapper.
        assert_eq!(quote_path("C:\\od\"d\\app.exe"), "\"C:\\od\\\"d\\app.exe\"");
        // A trailing backslash is doubled so it cannot escape the closing quote.
        assert_eq!(quote_path("C:\\dir\\"), "\"C:\\dir\\\\\"");
    }

    fn zeroed_cov() -> Cov {
        Cov::ZEROED
    }

    /// S-2 regression. A hook that PREPARED its detours but never enabled them (an AV blocking
    /// the code-section write, a CFG conflict) publishes an empty mask. That must read as a
    /// failing verdict - never as a session reported "works" while nothing was substituted.
    /// The old order set the bits at `create_hook` time, so this exact case produced a full
    /// covered list with zero calls and a Works verdict (untouchable rule 4).
    #[test]
    fn empty_install_mask_reads_as_a_failing_verdict() {
        let cov = zeroed_cov();
        let gathered = unsafe { gather_coverage(&cov as *const Cov, 0, false, false) };
        assert!(gathered.covered.is_empty(), "nothing may be claimed as covered");
        assert!(!gathered.uncovered.is_empty(), "always-present channels are a real gap");
        assert_eq!(chrono_core::verdict_from_coverage(&gathered), chrono_core::Verdict::Fails);
    }

    /// A Win32 failure has to read as a sentence, not as a struct dump, and it has to stay ENGLISH.
    ///
    /// Both halves were broken by the same `{e:?}`: it printed `Error { code: HRESULT(0x80070003),
    /// message: "..." }`, and the message field is filled in by Windows in the SYSTEM's language - so
    /// an English-only CLI (rule 15) printed a Polish sentence on this machine.
    #[test]
    fn a_win32_failure_reads_as_a_sentence_and_keeps_its_code() {
        let missing_path = windows::core::Error::from_hresult(windows::core::HRESULT(0x8007_0003u32 as i32));
        let detail = win32_detail("CreateProcessW", &missing_path);

        assert_eq!(detail, "CreateProcessW failed - the path does not exist (0x80070003)");
        // No struct dump and no OS sentence: the two things that made this unreadable.
        assert!(!detail.contains("HRESULT("), "the Debug rendering is what this replaced: {detail}");
        assert!(!detail.contains("message:"), "the OS message is language-dependent: {detail}");
    }

    /// The refusals a tester meets most say what to do or what happened, beside the number (R5-D3): a
    /// program that wants administrator rights used to read "CreateProcessW failed (0x800702E4)".
    #[test]
    fn the_common_refusals_are_named() {
        let named = |code: u32| win32_detail("CreateProcessW", &windows::core::Error::from_hresult(windows::core::HRESULT(code as i32)));
        assert_eq!(
            named(0x8007_02E4),
            "CreateProcessW failed - the program asks to run as administrator, so start Chrono Mock as administrator to test it (0x800702E4)"
        );
        for (code, clause) in [
            (0x8007_00CEu32, "too long"),
            (0x8007_00D8, "different kind of processor"),
            (0x8007_00E1, "antivirus blocked"),
            (0x8007_00E2, "antivirus removed"),
            (0x8007_04EC, "system policy"),
        ] {
            let detail = named(code);
            assert!(detail.contains(clause) && detail.ends_with(&format!("(0x{code:08X})")), "{detail}");
        }
    }

    /// A block left by another version is told from a foreign one, whatever size it was mapped at (R5-D3).
    /// A smaller old block used to fail before its header could be read, as a bare "MapViewOfFile
    /// returned null".
    #[test]
    fn a_block_of_another_version_is_told_from_a_foreign_one() {
        use super::{held_block, HeldBlock};
        let full = chrono_ctl::ctl_size();
        assert_eq!(held_block(full, chrono_ctl::CTL_MAGIC, chrono_ctl::CTL_LAYOUT_VERSION), HeldBlock::Ours);
        assert_eq!(held_block(full, chrono_ctl::CTL_MAGIC, chrono_ctl::CTL_LAYOUT_VERSION - 1), HeldBlock::OtherVersion);
        assert_eq!(held_block(full / 16, chrono_ctl::CTL_MAGIC, chrono_ctl::CTL_LAYOUT_VERSION), HeldBlock::OtherVersion);
        assert_eq!(held_block(full, 0, chrono_ctl::CTL_LAYOUT_VERSION), HeldBlock::Foreign);
        assert_eq!(held_block(4096, chrono_ctl::CTL_MAGIC ^ 1, 11), HeldBlock::Foreign);
    }

    /// The machine comes out of a PE header, and anything that is not one gives none.
    #[test]
    fn the_machine_is_read_from_a_pe_header() {
        use super::pe_machine;
        let own = std::fs::read(std::env::current_exe().expect("the test binary")).expect("readable");
        // SAFETY: the pseudo-handle of this process.
        let core = unsafe { super::process_machine(GetCurrentProcess()) }.expect("this process's machine");
        assert_eq!(pe_machine(&own), Some(core));
        assert_eq!(pe_machine(b"MZ"), None);
        assert_eq!(pe_machine(b"not a program at all, long enough to hold an offset........"), None);
        let mut header = vec![0u8; 0x48];
        header[..2].copy_from_slice(b"MZ");
        header[0x3C] = 0x40;
        header[0x40..0x44].copy_from_slice(b"PE\0\0");
        header[0x44..0x46].copy_from_slice(&0x14Cu16.to_le_bytes());
        assert_eq!(pe_machine(&header), Some(0x14C));
        // An offset that leads to anything but the PE signature is no header either: a 16-bit program
        // starts with MZ too, and its machine field would be two bytes of something else.
        header[0x40..0x44].copy_from_slice(b"NE\0\0");
        assert_eq!(pe_machine(&header), None, "a header without the PE signature was read");
        header[0x40..0x44].copy_from_slice(b"PE\0\0");
        header[0x3C] = 0xFF;
        assert_eq!(pe_machine(&header), None, "an offset past the bytes is no header");
    }

    /// A hook library `LoadLibraryW` would not load is examined this side (R5-D3): a file the core
    /// cannot open is named with its reason, one built for another machine is called a mixed package,
    /// and a library that is fine leaves the plain message standing.
    #[test]
    fn a_hook_library_that_would_not_load_is_examined() {
        use super::{hook_library_problem, to_wide};
        let dir = std::env::temp_dir().join(format!("chrono-hooklib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder");

        let missing = dir.join("absent.dll");
        let said = hook_library_problem(&to_wide(&missing.to_string_lossy())).expect("a missing file is a problem");
        assert_eq!(said, "the hook library could not be opened - the file does not exist (0x80070002)");

        // SAFETY: the pseudo-handle of this process.
        let core = unsafe { super::process_machine(GetCurrentProcess()) }.expect("this process's machine");
        let other: u16 = if core == IMAGE_FILE_MACHINE_I386.0 { IMAGE_FILE_MACHINE_AMD64.0 } else { IMAGE_FILE_MACHINE_I386.0 };
        let mut header = vec![0u8; 0x48];
        header[..2].copy_from_slice(b"MZ");
        header[0x3C] = 0x40;
        header[0x40..0x44].copy_from_slice(b"PE\0\0");
        header[0x44..0x46].copy_from_slice(&other.to_le_bytes());
        let foreign = dir.join("foreign.dll");
        std::fs::write(&foreign, &header).expect("written");
        let said = hook_library_problem(&to_wide(&foreign.to_string_lossy())).expect("another machine is a problem");
        assert!(said.contains("the files of the package are mixed"), "{said}");

        let own = std::env::current_exe().expect("the test binary");
        assert_eq!(hook_library_problem(&to_wide(&own.to_string_lossy())), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A code with no clause of ours still names the operation and the number - which is what a bug
    /// report needs. Silence about the code would be worse than the struct dump this replaced.
    #[test]
    fn an_unnamed_win32_code_still_carries_the_operation_and_the_number() {
        let odd = windows::core::Error::from_hresult(windows::core::HRESULT(0x8007_1234u32 as i32));

        assert_eq!(win32_detail("CreateRemoteThread", &odd), "CreateRemoteThread failed (0x80071234)");
    }

    /// S-3 regression. The session lock decides orphan-versus-live, and an abandoned mutex (the
    /// owner died, kill -9 included) must read as OURS - the old design inferred this from a
    /// `core_pid` that `prepare` writes LAST, so a core still initializing looked like an orphan
    /// and a second core zeroed its anchor mid-session (the target then read 1601-01-01, frozen,
    /// with no error - untouchable rules 2 and 3).
    #[test]
    fn abandoned_lock_is_ours_and_a_held_one_is_not() {
        assert!(lock_is_ours(WAIT_OBJECT_0), "a free lock is ours");
        assert!(lock_is_ours(WAIT_ABANDONED), "a dead owner's lock is ours - that is the orphan");
        assert!(!lock_is_ours(WAIT_TIMEOUT), "a live core holding it must refuse us");
        assert!(!lock_is_ours(windows::Win32::Foundation::WAIT_FAILED), "a failed wait refuses too");
    }

    /// R2-S2. A child the hook could not follow into is a process of this family running on the REAL
    /// clock. It leaves no slot of its own, so without the parent counting it the audit reports a
    /// smaller family and a `works` that never mentions it (untouchable rule 4).
    #[test]
    fn an_uninjected_child_warns_and_does_not_touch_the_verdict() {
        let all = CHANNELS.iter().fold(0u64, |acc, ch| acc | ch.bit);

        let mut cov = zeroed_cov();
        cov.uninjected_children = 1;
        let gathered = unsafe { gather_coverage(&cov as *const Cov, all, false, false) };
        assert!(gathered.warning_keys.iter().any(|k| k == "inheritance.child_not_injected"));
        // The parent's OWN coverage is untouched: this says something about a different process, and
        // the two are never merged (rule 4). The family count is what shrinks, and the warning says so.
        assert_eq!(chrono_core::verdict_from_coverage(&gathered), chrono_core::Verdict::Works);

        // And silence when every child was followed - the warning must mean something.
        let quiet = zeroed_cov();
        let gathered = unsafe { gather_coverage(&quiet as *const Cov, all, false, false) };
        assert!(!gathered.warning_keys.iter().any(|k| k == "inheritance.child_not_injected"));
    }

    /// R2-S9. Everything past the registry's capacity is a process the audit could not see. Exactly at
    /// capacity is not an overflow - the 256th process got the last slot - and the saturating subtraction
    /// is what keeps an ordinary session from reporting a negative-turned-huge count.
    #[test]
    fn only_claims_past_the_registrys_capacity_count_as_uncovered() {
        assert_eq!(uncovered_from_attempts(0), 0);
        assert_eq!(uncovered_from_attempts(1), 0);
        assert_eq!(uncovered_from_attempts(MAX_COV_PIDS as u32), 0, "the last slot is still a slot");
        assert_eq!(uncovered_from_attempts(MAX_COV_PIDS as u32 + 1), 1);
        assert_eq!(uncovered_from_attempts(MAX_COV_PIDS as u32 + 44), 44);
        assert_eq!(uncovered_from_attempts(u32::MAX), u32::MAX - MAX_COV_PIDS as u32);
    }

    /// ADR-4, 2026-10-27. Only a family every member of which said it is a console program loses the
    /// hand-off suspicion. One windowed member, or one whose kind is not known, keeps it.
    #[test]
    fn only_a_family_of_known_console_programs_is_not_a_suspected_hand_off() {
        const CONSOLE: Option<u32> = Some(chrono_ctl::SUBSYSTEM_CONSOLE);
        const WINDOWED: Option<u32> = Some(chrono_ctl::SUBSYSTEM_WINDOWS);
        assert!(only_console(1, &[CONSOLE]));
        assert!(only_console(3, &[CONSOLE, CONSOLE, CONSOLE]));
        assert!(!only_console(1, &[WINDOWED]), "the second instance of a windowed application");
        assert!(!only_console(2, &[CONSOLE, WINDOWED]), "a console starter of a windowed application");
        assert!(!only_console(2, &[CONSOLE, None]), "a member whose hook never published");
        assert!(!only_console(1, &[Some(0)]), "a member that could not read its own headers");
        assert!(!only_console(0, &[]), "a launched process that never signed in");
        let full = vec![CONSOLE; MAX_COV_PIDS];
        assert!(only_console(MAX_COV_PIDS as u32, &full), "the last slot is still a slot");
        assert!(!only_console(MAX_COV_PIDS as u32 + 1, &full), "a process with nowhere to say what it was");
    }

    /// R2-S3. The QPC channel rides its OWN opt-in. Off, it is not a gap - leaving QPC real is the
    /// ADR-2 default, and reporting it as missing would make every ordinary session look partial. On,
    /// it must be reported both ways: covered when the detour is live, and an honest gap when it is
    /// not, because "did the QPC axis actually scale" is the only question the person who turned that
    /// flag on is asking (untouchable rule 4).
    #[test]
    fn the_qpc_channel_is_reported_only_under_its_own_opt_in() {
        let cov = zeroed_cov();
        let all = CHANNELS.iter().fold(0u64, |acc, ch| acc | ch.bit);
        let is_qpc = |name: &String| name == "QueryPerformanceCounter";

        // scale_qpc off: absent from every bucket, whether or not the detour happened to install.
        let off = unsafe { gather_coverage(&cov as *const Cov, all, false, false) };
        assert!(!off.covered.iter().any(|c| is_qpc(&c.channel)));
        assert!(!off.uncovered.iter().any(is_qpc));

        // scale_qpc on and installed: a covered channel like any other.
        let on = unsafe { gather_coverage(&cov as *const Cov, all, false, true) };
        assert!(on.covered.iter().any(|c| is_qpc(&c.channel)), "the scaled axis must be reported");

        // scale_qpc on and NOT installed: a real gap, and one that moves the verdict off `works` -
        // the session was asked to scale QPC and did not (it used to say nothing at all).
        let failed = unsafe { gather_coverage(&cov as *const Cov, all & !CH_QPC, false, true) };
        assert!(failed.uncovered.iter().any(is_qpc), "a QPC detour that did not install is a gap");
        assert_eq!(chrono_core::verdict_from_coverage(&failed), chrono_core::Verdict::Partial);
    }

    /// `timeGetTime` rides the DURATION axis (partial ADR-2 reversal, 2026-09-07): covered like
    /// GetTickCount when the session asked for a scaled duration, absent entirely when it did not - the
    /// same shape as every other member of that axis, because it is the same quantity. The warning is what
    /// the coverage costs, so it fires only when the target actually read the clock: a caution about audio
    /// drift on a target that never touched winmm would be noise.
    #[test]
    fn the_winmm_clock_rides_the_duration_axis_and_warns_about_what_that_costs() {
        let all = CHANNELS.iter().fold(0u64, |acc, ch| acc | ch.bit);
        let is_tgt = |name: &String| name == "timeGetTime";
        let warned =
            |c: &Coverage| c.warning_keys.iter().any(|k| k == "clock.timegettime_scaled_audio_may_shift");

        // Installed but never called: covered with a zero count, and NO warning.
        let quiet = zeroed_cov();
        let idle = unsafe { gather_coverage(&quiet as *const Cov, all, true, false) };
        let seen = idle.covered.iter().find(|c| is_tgt(&c.channel)).expect("must be covered, not observed");
        assert_eq!(seen.calls, 0);
        assert!(!idle.observed.iter().any(|c| is_tgt(&c.channel)), "it is scaled now, not observed");
        assert!(!warned(&idle), "a clock nobody read costs nothing");

        // Called: same bucket, real count, and the caution about what scaling it can shift.
        let mut busy = zeroed_cov();
        unsafe { bump_calls(&mut busy as *mut Cov, IDX_TIMEGETTIME) };
        let hot = unsafe { gather_coverage(&busy as *const Cov, all, true, false) };
        assert_eq!(hot.covered.iter().find(|c| is_tgt(&c.channel)).map(|c| c.calls), Some(1));
        assert!(warned(&hot), "scaling the winmm clock has a cost and the audit names it");
        assert_eq!(chrono_core::verdict_from_coverage(&hot), chrono_core::Verdict::Works);

        // A winmm channel that did not install, with no failure recorded by the hook, is still not a gap -
        // a target without winmm loaded cannot call it, so it goes nowhere rather than faking a partial
        // verdict (the User32/Winmm rule).
        let absent = unsafe { gather_coverage(&busy as *const Cov, all & !CH_TIMEGETTIME, true, false) };
        assert!(!absent.uncovered.iter().any(is_tgt), "winmm absent is not a coverage gap");

        // Without the duration opt-in the whole axis is left real on purpose, so this channel is not
        // reported at all - exactly like GetTickCount, which is the same quantity.
        let off = unsafe { gather_coverage(&busy as *const Cov, all, false, false) };
        assert!(!off.covered.iter().any(|c| is_tgt(&c.channel)));
        assert!(!off.observed.iter().any(|c| is_tgt(&c.channel)));
        assert!(!warned(&off));
    }

    /// A detour the hook could not switch on in an optional module it found loaded is a failed hook,
    /// not a module the application never loaded, and the report has to say so the way it does for
    /// kernel32 (untouchable rule 4). A scaled channel is a gap that moves the verdict, an observed one
    /// goes to `unobserved`, and one that failed and then went live on the late scan is covered.
    #[test]
    fn a_detour_that_failed_in_a_loaded_optional_module_is_reported() {
        let all = CHANNELS.iter().fold(0u64, |acc, ch| acc | ch.bit);
        let both = CH_TIMEGETTIME | CH_TIMESETEVENT;
        let named = |list: &[String], name: &str| list.iter().any(|n| n == name);

        // Nothing recorded: the module was not loaded, so neither channel is a gap and the verdict holds.
        let absent = zeroed_cov();
        let quiet = unsafe { gather_coverage(&absent as *const Cov, all & !both, true, false) };
        assert!(!named(&quiet.uncovered, "timeGetTime"));
        assert!(!named(&quiet.unobserved, "timeSetEvent"));
        assert_eq!(chrono_core::verdict_from_coverage(&quiet), chrono_core::Verdict::Works);

        // The hook recorded both as failed: the scaled clock is uncovered, the observed timer unobserved.
        let mut failed = zeroed_cov();
        failed.failed_channels = both;
        let gap = unsafe { gather_coverage(&failed as *const Cov, all & !both, true, false) };
        assert!(named(&gap.uncovered, "timeGetTime"), "a failed scaled detour is a gap");
        assert!(named(&gap.unobserved, "timeSetEvent"), "a failed observer is not running");
        assert!(!named(&gap.uncovered, "timeSetEvent"), "an observer never moves the verdict");
        assert_eq!(chrono_core::verdict_from_coverage(&gap), chrono_core::Verdict::Partial);

        // Failed at startup, live after the late scan: covered, and no gap left behind.
        let retried = unsafe { gather_coverage(&failed as *const Cov, all, true, false) };
        assert!(retried.covered.iter().any(|c| c.channel == "timeGetTime"));
        assert!(retried.uncovered.is_empty() && retried.unobserved.is_empty());
        assert_eq!(chrono_core::verdict_from_coverage(&retried), chrono_core::Verdict::Works);

        // Without the duration opt-in these channels are not wanted, so a stale bit says nothing.
        let off = unsafe { gather_coverage(&failed as *const Cov, all & !both, false, false) };
        assert!(!named(&off.uncovered, "timeGetTime") && !named(&off.unobserved, "timeSetEvent"));
    }

    /// Network timeouts follow the session once the tick count is detoured in kernelbase, and the audit
    /// says so when the process opened a connection under a scaled duration axis. The connection is
    /// counted, not inferred from the observer being installed: the observer sits in ntdll and is
    /// installed in every process, network or not.
    #[test]
    fn a_connection_under_a_scaled_duration_axis_warns_that_network_timeouts_follow_the_session() {
        let all = CHANNELS.iter().fold(0u64, |acc, ch| acc | ch.bit);
        let warned = |c: &Coverage| c.warning_keys.iter().any(|k| k == "wait.network_timeouts_scaled");
        let quiet = zeroed_cov();
        let mut connected = zeroed_cov();
        unsafe { bump_calls(&mut connected as *mut Cov, IDX_CONNECT) };

        let with_connection = unsafe { gather_coverage(&connected as *const Cov, all, true, false) };
        assert!(warned(&with_connection), "a connection counted and the axis scaled");
        assert_eq!(chrono_core::verdict_from_coverage(&with_connection), chrono_core::Verdict::Works, "a caution, never a verdict");

        let no_connection = unsafe { gather_coverage(&quiet as *const Cov, all, true, false) };
        assert!(!warned(&no_connection), "the observer is installed everywhere, so no connection means nothing to time out");

        let axis_real = unsafe { gather_coverage(&connected as *const Cov, all, false, false) };
        assert!(!warned(&axis_real), "without the duration opt-in the tick count stays real");

        // The timeout follows the tick count, so the tick count has to be on the session's axis.
        let no_tick = unsafe { gather_coverage(&connected as *const Cov, all & !(CH_GTC64 | CH_GTC), true, false) };
        assert!(!warned(&no_tick), "both tick count channels failed, so the timeouts stay real");
        let one_tick = unsafe { gather_coverage(&connected as *const Cov, all & !CH_GTC, true, false) };
        assert!(warned(&one_tick), "a library reading the installed tick count still follows the session");
    }

    /// A wait held at the scaling floor is reported, because it is partial coverage.
    ///
    /// The floor exists so a sub-multiplier timeout does not truncate to zero and turn a polling loop
    /// into a spin - but a wait at the floor did NOT accelerate by the session's multiplier, and a
    /// difference in behaviour the tester cannot account for is what rule 27 forbids leaving silent.
    #[test]
    fn a_wait_held_at_the_scaling_floor_is_reported() {
        let says_collapsed =
            |c: &Coverage| c.warning_keys.iter().any(|k| k == "wait.timeout_collapsed");

        let mut cov = zeroed_cov();
        cov.waits_at_floor = 4;
        let gathered = unsafe { gather_coverage(&cov as *const Cov, 0, true, false) };
        assert!(says_collapsed(&gathered), "four waits ran at the floor and nothing said so");

        // No such wait, no warning - a warning on every session would say nothing at all.
        let quiet = zeroed_cov();
        let gathered = unsafe { gather_coverage(&quiet as *const Cov, 0, true, false) };
        assert!(!says_collapsed(&gathered));
    }

    /// A timer held at a floor is reported under its own key, and a wait at the floor does not stand
    /// in for it: the two name different parts of the application and different floors (R5, D8).
    #[test]
    fn a_timer_held_at_a_floor_is_reported_apart_from_the_waits() {
        let says = |c: &Coverage, key: &str| c.warning_keys.iter().any(|k| k == key);

        let mut timers = zeroed_cov();
        timers.timers_at_floor = 3;
        let gathered = unsafe { gather_coverage(&timers as *const Cov, 0, true, false) };
        assert!(says(&gathered, "timer.interval_floored"), "three timers ran at a floor and nothing said so");
        assert!(!says(&gathered, "wait.timeout_collapsed"), "no wait was held, yet the wait warning came");

        let mut waits = zeroed_cov();
        waits.waits_at_floor = 3;
        let gathered = unsafe { gather_coverage(&waits as *const Cov, 0, true, false) };
        assert!(!says(&gathered, "timer.interval_floored"), "no timer was held, yet the timer warning came");

        let quiet = zeroed_cov();
        let gathered = unsafe { gather_coverage(&quiet as *const Cov, 0, true, false) };
        assert!(!says(&gathered, "timer.interval_floored"));
    }

    /// 2026-10-28: a wait or timer on a thread without thread-local storage runs unscaled, because the
    /// hook cannot tell a cascade there. The report names it, under its own key, and only when it happened.
    #[test]
    fn a_call_on_a_thread_without_storage_is_reported() {
        let says = |c: &Coverage, key: &str| c.warning_keys.iter().any(|k| k == key);

        let mut cov = zeroed_cov();
        cov.calls_without_storage = 2;
        let gathered = unsafe { gather_coverage(&cov as *const Cov, 0, true, false) };
        assert!(says(&gathered, "wait.thread_without_storage"), "two calls ran unscaled and nothing said so");
        assert!(!says(&gathered, "wait.timeout_collapsed"), "no wait was held at the floor");

        let quiet = zeroed_cov();
        let gathered = unsafe { gather_coverage(&quiet as *const Cov, 0, true, false) };
        assert!(!says(&gathered, "wait.thread_without_storage"), "no such call, yet the warning came");
    }

    /// A channel hooked late is covered, and the audit says the count is a floor.
    ///
    /// The interesting case is the third one. The hook writes `late_installed` and
    /// `installed_channels` as two separate stores, so a reader can catch the pair half-written -
    /// and the intersection is what makes either order safe. Without it, a late bit arriving first
    /// would warn about a channel the report does not list, which is the audit describing something
    /// that is not there (untouchable rule 4).
    #[test]
    fn a_channel_installed_late_is_covered_and_the_count_is_called_a_floor() {
        let all = CHANNELS.iter().fold(0u64, |acc, ch| acc | ch.bit);
        let warned =
            |c: &Coverage| c.warning_keys.iter().any(|k| k == "coverage.channel_installed_late");

        // Nothing late: the ordinary session, and no warning to make the reader doubt the counts.
        let plain = zeroed_cov();
        let early = unsafe { gather_coverage(&plain as *const Cov, all, true, true) };
        assert!(!warned(&early), "a session with nothing late must not carry the caution");
        assert!(early.installed_late.is_empty(), "and names no channel as late");

        // Late AND installed: covered like any other channel, plus the warning. The channel keeps its
        // place in `covered` on purpose - it WAS substituted, just not for the whole session.
        let mut late = zeroed_cov();
        unsafe { set_late_installed(&mut late as *mut Cov, CH_TIMEGETTIME) };
        let hooked_late = unsafe { gather_coverage(&late as *const Cov, all, true, true) };
        assert!(warned(&hooked_late), "a late channel makes its call count a floor, and that is said");
        assert!(
            hooked_late.covered.iter().any(|c| c.channel == "timeGetTime"),
            "late is still covered - it is not a gap"
        );
        assert_eq!(
            hooked_late.installed_late,
            vec!["timeGetTime".to_string()],
            "the warning is not enough on its own - the channel it is about is named"
        );

        // Late but NOT in the installed mask: the half-written pair. Silence, because the report does
        // not list this channel at all and a warning about it would point at nothing.
        let torn = unsafe { gather_coverage(&late as *const Cov, all & !CH_TIMEGETTIME, true, true) };
        assert!(
            !warned(&torn),
            "the masks are read as an intersection - a late bit alone must never warn"
        );
        assert!(torn.installed_late.is_empty(), "nor name a channel the report does not list");
    }

    /// The paired direction, so the guard above cannot pass by reporting nothing at all: a full
    /// mask still yields covered channels and a Works verdict.
    #[test]
    fn full_install_mask_reads_as_works() {
        let cov = zeroed_cov();
        let all = CHANNELS.iter().fold(0u64, |acc, ch| acc | ch.bit);
        let gathered = unsafe { gather_coverage(&cov as *const Cov, all, false, false) };
        assert!(!gathered.covered.is_empty(), "a live mask must report covered channels");
        assert!(gathered.uncovered.is_empty(), "nothing is missing when every bit is set");
        assert_eq!(chrono_core::verdict_from_coverage(&gathered), chrono_core::Verdict::Works);
    }

    /// R5-D2. The deadline channels stand in every session. Without `--scale-duration` they convert only
    /// an absolute deadline, so they are not listed as substituted (their calls are mostly waits the
    /// session leaves real) - but a deadline channel that did NOT stand is a gap in any session.
    #[test]
    fn the_deadline_channels_are_expected_in_every_session_and_listed_only_when_they_scale() {
        let cov = zeroed_cov();
        let all = CHANNELS.iter().fold(0u64, |acc, ch| acc | ch.bit);
        let names = |list: &[chrono_core::ChannelCoverage]| list.iter().map(|c| c.channel.clone()).collect::<Vec<_>>();

        let plain = unsafe { gather_coverage(&cov as *const Cov, all, false, false) };
        for name in ["SetWaitableTimer", "SetWaitableTimerEx", "SetThreadpoolTimer", "SetThreadpoolTimerEx", "NtDelayExecution"] {
            assert!(!names(&plain.covered).contains(&name.to_string()), "{name} listed as substituted without the duration axis");
            assert!(!plain.uncovered.contains(&name.to_string()), "{name} called a gap while it stood");
        }

        let failed = unsafe { gather_coverage(&cov as *const Cov, all & !chrono_ctl::CH_SWT, false, false) };
        assert_eq!(failed.uncovered, vec!["SetWaitableTimer".to_string()], "a deadline channel that did not stand is a gap");

        let scaled = unsafe { gather_coverage(&cov as *const Cov, all, true, false) };
        assert!(names(&scaled.covered).contains(&"NtDelayExecution".to_string()), "under the duration axis it is listed");
    }

    /// R5-WY11. A count that stopped at the hook's ceiling travels as "at least", covered and observed alike.
    #[test]
    fn a_count_at_the_ceiling_is_marked_as_a_floor() {
        let mut cov = zeroed_cov();
        cov.calls[chrono_ctl::IDX_GSTAFT] = chrono_ctl::CALLS_CAP;
        cov.calls[chrono_ctl::IDX_GST] = chrono_ctl::CALLS_CAP - 1;
        cov.calls[chrono_ctl::IDX_WFSO] = chrono_ctl::CALLS_CAP;
        let all = CHANNELS.iter().fold(0u64, |acc, ch| acc | ch.bit);
        let gathered = unsafe { gather_coverage(&cov as *const Cov, all, true, false) };
        let find = |list: &[chrono_core::ChannelCoverage], name: &str| {
            list.iter().find(|c| c.channel == name).cloned().expect("channel listed")
        };
        assert!(find(&gathered.covered, "GetSystemTimeAsFileTime").capped);
        assert!(!find(&gathered.covered, "GetSystemTime").capped, "one under the ceiling is exact");
        assert!(find(&gathered.observed, "WaitForSingleObject").capped);
    }

    /// R5-P17. The fake elapsed time stands at the top of its range instead of wrapping to a negative
    /// number, and a reading behind the segment's start adds nothing.
    #[test]
    fn the_fake_elapsed_time_saturates_and_never_goes_negative() {
        let m = chrono_core::MULTIPLIER_MAX;
        // Eleven days of real time at the maximum multiplier: the product passes the top of an i64.
        let eleven_days = 10_000_000i64 * 86_400 * 11;
        assert_eq!(fake_elapsed_ticks(0, 0, eleven_days, m), i64::MAX);
        // Banked time plus a segment that would carry it over the top.
        assert_eq!(fake_elapsed_ticks(i64::MAX - 5, 0, 10_000_000, 60), i64::MAX);
        // An ordinary segment.
        assert_eq!(fake_elapsed_ticks(1_000, 10_000_000, 20_000_000, 60), 1_000 + 10_000_000 * 60);
        // Frozen spends nothing, and a clock behind the segment's start adds nothing.
        assert_eq!(fake_elapsed_ticks(1_000, 0, 10_000_000, 0), 1_000);
        assert_eq!(fake_elapsed_ticks(1_000, 10_000_000, 9_000_000, 60), 1_000);
    }

    /// The random half of what a target is handed, each read back by the system itself rather than by
    /// anything of this crate's. The command line: whatever the path and the arguments hold, a program
    /// reads back exactly what it was started with, read by `CommandLineToArgvW`, the rule a C runtime
    /// parses its command line by (P9). The clock: the moment a session reports is the one Windows shows
    /// the target for it.
    mod properties {
        use proptest::prelude::*;
        use proptest::sample::select;

        #[cfg_attr(target_arch = "x86", link(name = "shell32", kind = "raw-dylib", import_name_type = "undecorated"))]
        #[cfg_attr(not(target_arch = "x86"), link(name = "shell32", kind = "raw-dylib"))]
        unsafe extern "system" {
            fn CommandLineToArgvW(line: *const u16, count: *mut i32) -> *mut *mut u16;
        }

        /// The arguments `CommandLineToArgvW` reads out of a zero-terminated command line, the program first.
        fn argv(line: &[u16]) -> Vec<String> {
            assert_eq!(line.last(), Some(&0), "a command line ends in a zero");
            let mut count = 0i32;
            // SAFETY: the line ends in a zero, every argument is copied out before the array is let go,
            // and each one the call returns ends in a zero of its own.
            unsafe {
                let array = CommandLineToArgvW(line.as_ptr(), &raw mut count);
                assert!(!array.is_null(), "CommandLineToArgvW read nothing");
                let read = (0..usize::try_from(count).unwrap())
                    .map(|i| {
                        let arg = *array.add(i);
                        let length = (0..).take_while(|&j| *arg.add(j) != 0).count();
                        String::from_utf16(std::slice::from_raw_parts(arg, length)).expect("the arguments are text")
                    })
                    .collect();
                let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(array.cast())));
                read
            }
        }

        /// Where a path starts: a drive in either case, a share, a verbatim drive, nothing, the current folder.
        const ROOTS: [&str; 6] = ["C:\\", "c:\\", "\\\\server\\share\\", "\\\\?\\C:\\", "", ".\\"];
        /// Pieces of the names Windows takes, with every mark a command line might take for its own.
        const NAME_PIECES: [&str; 20] = [
            "app", "My App", "R&D", "50%", "a^b", "wow!", "it's", "(x86)", "a;b", "a=b", "a,b", "v1.2", "zażółć", "日本", "😀",
            " ", "  ", "-", "#", "~1",
        ];
        const SEPARATORS: [&str; 2] = ["\\", "\\\\"];
        const EXTENSIONS: [&str; 5] = [".exe", ".EXE", ".com", "", ".exe.exe"];
        /// Pieces of an argument: each separator a parser might split on, the quote, runs of one to three
        /// backslashes (before a quote, before the end, before a letter and before a space, by what follows
        /// them), and the marks of a shell.
        const ARG_PIECES: [&str; 28] = [
            "a", "word", " ", "\t", "\n", "\u{b}", "\r", "\"", "\\", "\\\\", "\\\\\\", "\\\"", "\u{3000}", "\u{a0}", "^", "&", "|",
            "<", ">", "%", "!", "=", "zażółć", "日本", "😀", "'", "\u{1}", "\u{7f}",
        ];

        fn joined(pieces: &'static [&'static str], count: std::ops::Range<usize>) -> impl Strategy<Value = String> {
            proptest::collection::vec(select(pieces), count).prop_map(|pieces| pieces.concat())
        }

        /// A program path: a root, up to three folders and a file name, never ending in a backslash and
        /// never holding a quote - no file of a program is named either way.
        fn program() -> impl Strategy<Value = String> {
            let folder = (joined(&NAME_PIECES, 1..4), select(SEPARATORS.to_vec())).prop_map(|(name, sep)| format!("{name}{sep}"));
            (select(ROOTS.to_vec()), proptest::collection::vec(folder, 0..4), joined(&NAME_PIECES, 1..4), select(EXTENSIONS.to_vec()))
                .prop_map(|(root, folders, file, extension)| format!("{root}{}{file}{extension}", folders.concat()))
        }

        /// Where a zero character goes, when one does.
        #[derive(Debug, Clone, Copy)]
        enum Zero {
            Path,
            Folder,
            Argument(usize),
        }

        fn with_zero(text: &str, at: usize) -> String {
            let at = text.char_indices().map(|(i, _)| i).chain([text.len()]).nth(at % (text.chars().count() + 1)).unwrap();
            format!("{}\0{}", &text[..at], &text[at..])
        }

        proptest! {
            /// A program reads back its path and every argument, in order and unchanged - an empty one, one
            /// with spaces, quotes or backslashes before a quote included. A zero character, which would cut
            /// the line short, is refused before anything starts, naming where it stood (R4-N16).
            #[test]
            fn a_program_reads_back_the_arguments_it_was_started_with(
                path in program(),
                args in proptest::collection::vec(joined(&ARG_PIECES, 0..6), 0..5),
                folder in proptest::option::of(Just("C:\\work".to_string())),
                zero in proptest::option::weighted(0.15, prop_oneof![
                    Just(Zero::Path), Just(Zero::Folder), (0usize..5).prop_map(Zero::Argument),
                ]),
                at in any::<usize>(),
            ) {
                let (mut path, mut args, mut folder) = (path, args, folder);
                let place = match zero {
                    Some(Zero::Path) => { path = with_zero(&path, at); Some("the target path") }
                    Some(Zero::Folder) => { folder = Some(with_zero("C:\\work", at)); Some("the working folder") }
                    Some(Zero::Argument(i)) if !args.is_empty() => {
                        let i = i % args.len();
                        args[i] = with_zero(&args[i], at);
                        Some("the argument")
                    }
                    _ => None,
                };
                let target = crate::Target {
                    path: &path, args: &args, cwd: folder.as_deref(), env: &[], stdio: crate::TargetStdio::Shared,
                };
                let line = crate::launch_line(&target);
                if let Some(place) = place {
                    let refusal = line.expect_err("a zero character is refused");
                    prop_assert!(refusal.starts_with(place) && refusal.contains("zero character"), "{}", refusal);
                    return Ok(());
                }
                let (application, line) = line.expect("a line without a zero character");
                prop_assert_eq!(String::from_utf16(&application[..application.len() - 1]).unwrap(), path.clone());
                let read = argv(&line);
                prop_assert_eq!(&read[0], &path, "the program's own name, in {:?}", String::from_utf16_lossy(&line));
                prop_assert_eq!(&read[1..], &args[..], "the arguments, in {:?}", String::from_utf16_lossy(&line));
            }

            /// The moment a session reports - in UTC and in its zone - is the one Windows shows the target
            /// for the same fake FILETIME, through `GetSystemTime` and through `GetLocalTime`, which takes
            /// the zone bias off first. And what the session reports reads back as the same second, as a
            /// jump to it would. For every instant a session clock takes and every zone it accepts, the end
            /// of the range included, where the clamp keeps a zone of headroom for exactly this (R2-X7).
            #[test]
            fn the_session_reports_the_moment_its_target_reads(ticks in instant(), bias in zone_bias()) {
                // A moment the session takes: at or after 1601 as local time in its zone too (R4-S7d).
                let ticks = ticks.max(i64::from(bias.max(0)) * 600_000_000);
                prop_assert!(chrono_core::instant_in_session_range(ticks, bias), "{} in a zone {} minutes off", ticks, bias);
                prop_assert_eq!(windows_reads(ticks), Some(chrono_core::filetime_utc_to_wall(ticks, 0)), "UTC of {}", ticks);
                // The hook's own arithmetic for the local channels, and the one `chrono-ctl` keeps for the
                // FILETIME ones (`shift_filetime`).
                let local = ticks - i64::from(bias) * 600_000_000;
                prop_assert_eq!(chrono_ctl::shift_ticks_by_bias(ticks, bias, false), Some(local));
                let reported = chrono_core::filetime_utc_to_wall(ticks, bias);
                prop_assert_eq!(windows_reads(local), Some(reported.clone()), "{} in a zone {} minutes off", ticks, bias);
                let whole = ticks - ticks % 10_000_000;
                let moment = chrono_core::Moment { local: reported, tz_bias_min: Some(bias) };
                prop_assert_eq!(chrono_core::moment_to_filetime_utc(&moment), Ok(whole));
                prop_assert_eq!(chrono_core::session_instant(&moment), Ok((whole, bias)));
            }
        }

        /// What Windows shows for a FILETIME, written the way a session report writes a moment: the
        /// conversion `GetSystemTime` and `GetLocalTime` hand the target through in the hook.
        fn windows_reads(ticks: i64) -> Option<String> {
            use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
            let file = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
            let mut st = SYSTEMTIME::default();
            // SAFETY: both structures are owned here.
            unsafe { windows::Win32::System::Time::FileTimeToSystemTime(&file, &mut st) }.ok()?;
            Some(format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond))
        }

        /// A FILETIME a session clock holds: an edge of the calendar as Windows counts it, either end of the
        /// range, or anything. The edges are the first instant of 1601, the leap day of a leap year, the
        /// last of February and the first of March in century years that are not leap years (1700, 1800,
        /// 1900, 2100) and in one that is (2000), the end of 32-bit Unix time, the last year of four digits
        /// and the last whole year a SYSTEMTIME holds - each at the first, a middle or the last second of
        /// the day, and the first, one or the last tick of that second.
        fn instant() -> impl Strategy<Value = i64> {
            let dates: Vec<(u16, u16, u16)> = vec![
                (1601, 1, 1), (1604, 2, 29), (1700, 2, 28), (1700, 3, 1), (1800, 2, 28), (1900, 2, 28), (1900, 3, 1),
                (2000, 2, 29), (2038, 1, 19), (2100, 2, 28), (2100, 3, 1), (9999, 12, 31), (30827, 12, 31),
            ];
            let edge = (select(dates), select(vec![(0u16, 0u16, 0u16), (3, 14, 7), (23, 59, 59)]), select(vec![0i64, 1, 9_999_999]))
                .prop_map(|((year, month, day), (hour, minute, second), tick)| {
                    let st = windows::Win32::Foundation::SYSTEMTIME {
                        wYear: year, wMonth: month, wDay: day, wHour: hour, wMinute: minute, wSecond: second, ..Default::default()
                    };
                    let mut file = windows::Win32::Foundation::FILETIME::default();
                    // SAFETY: both structures are owned here.
                    unsafe { windows::Win32::System::Time::SystemTimeToFileTime(&st, &mut file) }.expect("a date of Windows's own range");
                    let ticks = ((u64::from(file.dwHighDateTime) << 32) | u64::from(file.dwLowDateTime)) as i64;
                    ticks + tick
                });
            let top = chrono_ctl::FAKE_WALL_MAX;
            prop_oneof![2 => edge, 1 => select(vec![0, top - 10_000_000, top - 1, top]), 3 => 0..=top]
        }

        /// A session zone: either end of the band, the hour offsets most tested, a minute either side of
        /// UTC, or anything the band holds.
        fn zone_bias() -> impl Strategy<Value = i32> {
            let band = chrono_core::ZONE_BIAS_MAX_MIN;
            prop_oneof![1 => select(vec![-band, -840, -60, -1, 0, 1, 60, 300, band]), 1 => -band..=band]
        }
    }
}
