//! Chrono Mock mechanism layer - the only place that touches the OS and the target
//! process. This is the rewrite of the throwaway spike injector/hook plumbing as
//! product code.
//!
//! Stage 4: substitute the full set of wall-clock channels and report the session
//! zone. `prepare` creates the session control memory, computes the fake anchor and
//! the session zone bias from the moment, launches the target SUSPENDED (so the hook
//! is installed before the target's first instruction - no race), injects the hook
//! DLL, reads back which channels were covered, then resumes.
//!
//! The tool injects its OWN probes on the host; injecting into third-party or system
//! processes stays on the VM or requires explicit consent.

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

use chrono_core::{ChannelCoverage, Coverage, SessionSpec, TimeMode};
use chrono_ctl::{
    cov_at, ctl_size, freeze_dur, freeze_qpc, read_anchor, read_calls,
    read_core_pid, read_dur, read_installed, read_pid, read_pid_count, read_qpc,
    read_uninjected_children,
    write_anchor, write_anchor_full,
    write_core_pid, write_scale_dur, write_scale_qpc, write_tz_bias, ChannelCategory, ChannelModule,
    Cov, Ctl, CHANNELS, MAX_COV_PIDS,
};
use windows::core::{s, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE, WAIT_ABANDONED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualAllocEx,
    VirtualFreeEx, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MEM_COMMIT, MEM_RELEASE,
    MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::SystemInformation::{
    GetSystemTimeAsFileTime, GetTickCount64, IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64,
    IMAGE_FILE_MACHINE_ARM64, IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_UNKNOWN,
};
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
use windows::Win32::System::Threading::{
    CreateMutexW, CreateProcessW, CreateRemoteThread, GetCurrentProcess, GetCurrentProcessId,
    GetExitCodeProcess, GetExitCodeThread, IsWow64Process2, OpenProcess, ResumeThread,
    TerminateProcess, WaitForSingleObject, CREATE_SUSPENDED, LPTHREAD_START_ROUTINE,
    PROCESS_INFORMATION, PROCESS_SYNCHRONIZE, STARTUPINFOW,
};
use windows::Win32::System::Performance::QueryPerformanceCounter;
use windows::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime;

/// What to launch.
pub struct Target<'a> {
    pub path: &'a str,
    pub args: &'a [String],
    pub cwd: Option<&'a str>,
}

/// Why preparing a session failed. Each variant carries a message from the point of
/// origin (zasady/06 section 9); the caller maps it to a protocol error key.
#[derive(Debug)]
pub enum PrepareError {
    Moment(String),
    Control(String),
    Launch(String),
    Inject(String),
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
    /// An orphaned control block from a dead core was found at startup and reclaimed (its target
    /// had self-detached to real time). The caller surfaces this so the reclaim is not silent.
    pub orphan_reclaimed: bool,
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
    tz_bias: i32,
    /// The duration axis is opt-in; coverage gathering needs it to know whether the
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
    /// The session lock, held for as long as the session lives. Dropped last, so a second core
    /// cannot start until this one has released the control block it was using.
    _lock: SessionLock,
}

/// Both clocks at one instant, in raw UTC FILETIME ticks. The core formats them.
pub struct SessionState {
    pub fake_ft: i64,
    pub real_ft: i64,
    pub multiplier: i64,
    pub tz_bias: i32,
    pub elapsed_fake_ms: i64,
    pub elapsed_real_ms: i64,
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
    let advanced = now_real.wrapping_sub(anchor_real).saturating_mul(multiplier);
    anchor_fake.saturating_add(advanced).min(chrono_ctl::FAKE_WALL_MAX)
}

impl SessionState {
    /// Whether the fake wall clock is standing on the last instant this build can represent. The
    /// session is then doing less than it promised - the clock has stopped while fake time is still
    /// being counted - so the core says so rather than let the reader infer it from a still picture
    /// (untouchable rule 6, R2-X2).
    pub fn clock_at_range_end(&self) -> bool {
        self.fake_ft >= chrono_ctl::FAKE_WALL_MAX
    }
}

const STILL_ACTIVE_CODE: u32 = 259;

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
        let now_real = quit_now();
        let fake_ft = project_fake_ft(a_fake, a_real, now_real, m);
        SessionState {
            fake_ft,
            real_ft: real_system_filetime(),
            multiplier: m,
            tz_bias: self.tz_bias,
            elapsed_fake_ms: self.fake_elapsed_ticks(now_real, m) / 10_000,
            elapsed_real_ms: now_real.wrapping_sub(self.start_real) / 10_000,
        }
    }

    /// Fake ticks elapsed since the session started: what was banked in earlier rate segments, plus the
    /// current segment at the rate in force. Frozen (m = 0) contributes nothing, which is correct - a
    /// frozen clock spends no fake time.
    fn fake_elapsed_ticks(&self, now_real: i64, m: i64) -> i64 {
        let segment = now_real.wrapping_sub(self.rate_segment_real0.get()).wrapping_mul(m);
        self.fake_elapsed_before.get().wrapping_add(segment)
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

    /// The target's exit code, once it has exited.
    pub fn exit_code(&self) -> Option<i32> {
        let mut code: u32 = 0;
        unsafe {
            if GetExitCodeProcess(self.hprocess, &mut code).is_ok() && code != STILL_ACTIVE_CODE {
                Some(code as i32)
            } else {
                None
            }
        }
    }

    fn ctl_mut(&self) -> *mut Ctl {
        self.ctl_addr as *mut Ctl
    }

    /// Change the multiplier in flight, re-anchoring from the current clock so the
    /// fake time is continuous across the change (ADR-5): the fake instant now becomes
    /// the new fake anchor and the real clock now the new real anchor.
    ///
    /// The duration axis is re-anchored the SAME way, in the same seqlock transaction. Without it, a
    /// smaller multiplier would retroactively rescale the whole `(real_now - dur_q0)` history and rewind
    /// GetTickCount64/QUIT (H-1) - a violation of untouchable rule 3. `freeze_dur` captures the axis at its
    /// current value under the OLD multiplier, then it re-bases at `now`, so it continues from there at the
    /// new speed without ever going backward.
    pub fn set_multiplier(&self, m: i64) {
        let now = quit_now();
        let now_qpc = qpc_now();
        let (a_fake, a_real, cur_m) = unsafe { read_anchor(self.ctl()) };
        // Bank the fake time spent at the OLD rate before the new one starts, so elapsed stays an
        // integral over the whole session instead of being rescaled by whatever the latest rate is.
        self.close_rate_segment(now, cur_m);
        let fake_now = a_fake.wrapping_add(now.wrapping_sub(a_real).wrapping_mul(cur_m));
        let (dur_tick_c0, dur_quit_c0, dur_q0, _) = unsafe { read_dur(self.ctl()) };
        let (frozen_tick, frozen_quit) = freeze_dur(dur_tick_c0, dur_quit_c0, dur_q0, cur_m, now);
        // Freeze the QPC axis at the OLD multiplier too, then re-anchor at the current real QPC, so a
        // speed change never rewinds it (H-1 applied to QPC, untouchable rule 3).
        let (qpc_c0, qpc_q0, _) = unsafe { read_qpc(self.ctl()) };
        let frozen_qpc = freeze_qpc(qpc_c0, qpc_q0, cur_m, now_qpc);
        unsafe {
            write_anchor_full(
                self.ctl_mut(),
                fake_now,
                now,
                m,
                frozen_tick,
                frozen_quit,
                now,
                frozen_qpc,
                now_qpc,
            )
        };
    }

    /// Jump the wall clock to `to_ft` (UTC FILETIME), keeping the current multiplier.
    /// The duration axis anchors separately in the hook, so it is not affected - a
    /// backward jump never rewinds it (untouchable rule 3).
    pub fn jump(&self, to_ft: i64) {
        let now = quit_now();
        let (_, _, cur_m) = unsafe { read_anchor(self.ctl()) };
        unsafe { write_anchor(self.ctl_mut(), to_ft, now, cur_m) };
    }

    /// Jump the wall clock by ONE shift step from its CURRENT fake value, keeping the
    /// multiplier. Fixed-length units add a tick delta (sub-second precision preserved);
    /// calendar units (months/quarters/years) fold through the civil date in the session
    /// zone. Computed under ONE anchor read so no real time leaks between reading the
    /// current fake and re-anchoring - a calendar jump is as race-free as a fixed one
    /// (ADR-5). Returns Err if the step is unsupported (business days) or overflows.
    pub fn jump_step(
        &self,
        step: &chrono_core::calc::Step,
    ) -> Result<(), chrono_core::calc::EvalError> {
        let now = quit_now();
        let (a_fake, a_real, cur_m) = unsafe { read_anchor(self.ctl()) };
        let fake_now = a_fake.wrapping_add(now.wrapping_sub(a_real).wrapping_mul(cur_m));
        let target = chrono_core::calc::step_target(fake_now, self.tz_bias, step)?;
        unsafe { write_anchor(self.ctl_mut(), target, now, cur_m) };
        Ok(())
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
                // 0 = empty or reserved-but-not-yet-published; skip and retry later.
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
    /// full. Zero for every ordinary session; `MAX_COV_PIDS` is 256 and an installer spawning dozens of
    /// helpers is the realistic way past it (docs/07 open item 2).
    ///
    /// These processes are invisible to `poll_new_coverage` - they published no pid, because they had
    /// nowhere to publish one - so the family count and the coverage list are both short by this many,
    /// and nothing else in the audit can say so. Read it BEFORE `end`, while the block is still mapped.
    pub fn uncovered_process_count(&self) -> u32 {
        uncovered_from_attempts(unsafe { read_pid_count(self.ctl()) })
    }

    /// Stop the target, for the one case where letting it run would be worse than not running it: the
    /// opening verdict says the substitution did not take effect, so every minute the tester spends in
    /// that application produces evidence about the REAL clock while looking like a time-shifted run.
    /// Coverage already gathered stays valid and is still reported - this ends the process, not the
    /// audit. Best-effort: a target that exited on its own needs no stopping.
    pub fn terminate_target(&self) {
        unsafe {
            let _ = TerminateProcess(self.hprocess, 1);
        }
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
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.ctl_addr as *mut c_void,
            });
            let _ = CloseHandle(self.hmap);
            let _ = CloseHandle(self.hprocess);
        }
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn quit_now() -> i64 {
    let mut t: u64 = 0;
    unsafe {
        let _ = QueryUnbiasedInterruptTime(&mut t);
    }
    t as i64
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

/// Whether a process with this pid is still running. "Cannot open" and "already signalled" both mean
/// gone. PID reuse is a hazard in the usual direction: a recycled pid reads as alive, so a caller
/// deciding whether to clean something up errs toward leaving it - never toward destroying something
/// that belongs to a live process. The mechanism layer owns this because it is the only layer that
/// touches the OS (docs/07), so the CDP driver in `cli` borrows it rather than growing its own
/// Win32 dependency.
pub fn process_is_alive(pid: u32) -> bool {
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(h) => {
                let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
                let _ = CloseHandle(h);
                alive
            }
            Err(_) => false,
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
/// Maps and unmaps the control section; safe to call with no session running.
unsafe fn read_active_core_pid() -> u32 {
    let Ok(hmap) = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, windows::core::w!("Local\\ChronoCtl"))
    else {
        return 0;
    };
    let view = MapViewOfFile(hmap, FILE_MAP_ALL_ACCESS, 0, 0, ctl_size());
    let pid = if view.Value.is_null() { 0 } else { read_core_pid(view.Value as *const Ctl) };
    if !view.Value.is_null() {
        let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
    }
    let _ = CloseHandle(hmap);
    pid
}

/// Take the session lock, or report who holds it. A named mutex rather than inference from the
/// control block's contents: the previous design read `core_pid`, which `prepare` writes LAST, so
/// a core still initializing published 0 and a second core read that as "orphan" and zeroed a LIVE
/// session's anchor - the target then saw 1601-01-01, frozen, with no error at all (untouchable
/// rules 2 and 3). The kernel releases a mutex when its owner dies, kill -9 included, so an
/// abandoned lock is exactly the orphan signal, with no pid guessing and no pid-reuse hazard.
///
/// # Safety
/// Calls the Win32 synchronization APIs.
unsafe fn take_session_lock() -> Result<SessionLock, PrepareError> {
    let h = CreateMutexW(None, false, windows::core::w!("Local\\ChronoCtl.lock"))
        .map_err(|e| PrepareError::Control(format!("CreateMutexW: {e:?}")))?;
    let lock = SessionLock(h);
    if lock_is_ours(WaitForSingleObject(h, 0)) {
        Ok(lock)
    } else {
        Err(PrepareError::SessionActive(read_active_core_pid()))
    }
}

/// Whether a zero-timeout wait on the session mutex left US holding it. `WAIT_OBJECT_0` is the
/// plain case. `WAIT_ABANDONED` also means ours: the previous owner died without releasing, which
/// is precisely the orphan case we want to reclaim rather than refuse - the whole reason the lock
/// beats reading a pid out of shared memory. Anything else (`WAIT_TIMEOUT`, a failed wait) means a
/// live core holds it, and refusing is the safe direction.
fn lock_is_ours(state: windows::Win32::Foundation::WAIT_EVENT) -> bool {
    state == WAIT_OBJECT_0 || state == WAIT_ABANDONED
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
) -> Coverage {
    let mut out = Coverage::default();
    // Track which KIND of observed channel actually ran, so the audit names the right reason: an
    // object wait left real (class B), a multimedia timer left real (class C, winmm/ADR-2), or a
    // direct NtCreateUserProcess left un-injected (ADR-3).
    let mut any_wait_observed = false;
    let mut any_timer_observed = false;
    let mut any_spawn_observed = false;
    let mut any_source_observed = false;
    for (idx, ch) in CHANNELS.iter().enumerate() {
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
        if matches!(
            ch.category,
            ChannelCategory::WaitObserved | ChannelCategory::TimerObserved | ChannelCategory::SpawnObserved | ChannelCategory::SourceObserved
        ) {
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
                out.observed.push(ChannelCoverage { channel: ch.name.to_string(), calls });
            }
            continue;
        }
        if installed & ch.bit != 0 {
            out.covered.push(ChannelCoverage {
                channel: ch.name.to_string(),
                calls: read_calls(cov, idx),
            });
        } else {
            // Failed install. A channel in an ALWAYS-present module (kernel32/ntdll) is a real
            // coverage gap -> uncovered. A channel in an OPTIONAL module (user32/winmm) most likely
            // just is not loaded in this process (a console app or service never loaded user32), so
            // the app cannot call it at all - not a gap, so it goes nowhere rather than faking a
            // partial verdict (rule 4: never claim a gap the target could not hit). Static imports,
            // the common case, are already loaded in our DllMain; only a target that loads the module
            // dynamically after startup and then uses the channel would slip past here (documented).
            match ch.module {
                ChannelModule::Kernel32 | ChannelModule::Ntdll => {
                    out.uncovered.push(ch.name.to_string())
                }
                ChannelModule::User32 | ChannelModule::Winmm | ChannelModule::Ws2_32 => {}
            }
        }
    }
    // Separate warnings by observed kind, so the tester knows WHICH time source ran real: an object
    // wait (a real timeout left intact, class B) or a multimedia timer (winmm/audio left unscaled per
    // ADR-2, class C). Each fires only when that kind actually ran under acceleration (calls > 0).
    if any_wait_observed {
        out.warning_keys.push("wait.object_waits_not_scaled".to_string());
    }
    if any_timer_observed {
        out.warning_keys.push("timer.multimedia_not_scaled".to_string());
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
    out
}

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
unsafe fn process_machine(hproc: HANDLE) -> Option<u16> {
    let mut process = IMAGE_FILE_MACHINE(0);
    let mut native = IMAGE_FILE_MACHINE(0);
    IsWow64Process2(hproc, &mut process, Some(&mut native)).ok()?;
    Some(if process == IMAGE_FILE_MACHINE_UNKNOWN { native.0 } else { process.0 })
}

/// Whether this core can reach `hproc` at all. `None` means "go ahead": either the bitness matches, or
/// the query failed and we do not know - and a guess is not grounds for refusing a session (rule 6 cuts
/// both ways, we only declare what we actually established).
///
/// # Safety
/// `hproc` must be a valid process handle with QUERY_LIMITED_INFORMATION rights.
unsafe fn bitness_mismatch(hproc: HANDLE) -> Option<(&'static str, &'static str)> {
    let target = process_machine(hproc)?;
    let core = process_machine(GetCurrentProcess())?;
    (target != core).then(|| (machine_label(target), machine_label(core)))
}

/// How many slot claims went unserved, given the number of claims made. Pure, so the arithmetic is
/// testable without a live session: the registry counter only ever increases and counts ATTEMPTS, so
/// everything past its capacity is a process that ran with nowhere to report coverage into (R2-S9).
fn uncovered_from_attempts(attempts: u32) -> u32 {
    attempts.saturating_sub(MAX_COV_PIDS as u32)
}

/// Find the registry slot a process published its pid into, so its coverage can be read out of the
/// control block. Returns None if the process never registered (its hook failed, or the registry was
/// full) - the honest answer then is no coverage, never a guess.
///
/// # Safety
/// `ctl` must point to a live, correctly aligned `Ctl`.
unsafe fn find_pid_slot(ctl: *const Ctl, pid: u32) -> Option<usize> {
    (0..MAX_COV_PIDS).find(|&i| read_pid(ctl, i) == pid)
}

/// Prepare and start a session on `target` using `spec`, injecting `hook_dll`.
pub fn prepare(spec: &SessionSpec, target: &Target, hook_dll: &Path) -> Result<Prepared, PrepareError> {
    let a_fake = chrono_core::moment_to_filetime_utc(&spec.moment).map_err(PrepareError::Moment)?;
    let tz_bias = spec.moment.tz_bias_min.unwrap_or(0);
    let multiplier = match spec.mode {
        TimeMode::Flow => 1,
        TimeMode::Frozen => 0, // M = 0 holds the wall clock at a_fake
        TimeMode::Multiplier(m) => m,
    };
    let dll_wide = to_wide(&hook_dll.to_string_lossy());

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
            windows::core::w!("Local\\ChronoCtl"),
        )
        .map_err(|e| PrepareError::Control(format!("CreateFileMappingW: {e:?}")))?;
        // The section name is fixed, so it may survive a prior session. GetLastError right after the
        // create says whether it pre-existed - read it before anything else can reset it.
        let already_existed = GetLastError() == ERROR_ALREADY_EXISTS;
        let view = MapViewOfFile(hmap, FILE_MAP_ALL_ACCESS, 0, 0, ctl_size());
        if view.Value.is_null() {
            let _ = CloseHandle(hmap);
            return Err(PrepareError::Control("MapViewOfFile returned null".into()));
        }
        let ctl = view.Value as *mut Ctl;

        // A surviving section can only be an orphan here: we hold the session lock, so no live core
        // owns it - it was left by a killed core whose target self-detached to real time, or by one
        // that exited while the target still held the mapping alive. Reclaim it by zeroing the whole
        // block so no stale PID registry or anchor leaks into the new session. The refusal for a real
        // second session happened at the lock, not here - reading `core_pid` to decide would race
        // with a core that has not written it yet.
        let mut orphan_reclaimed = false;
        if already_existed {
            std::ptr::write_bytes(ctl as *mut u8, 0, ctl_size());
            orphan_reclaimed = true;
        }

        let start_real = quit_now();
        // Initialize the duration anchor from the REAL clock (the core is not hooked, so GetTickCount64 and
        // QUIT are genuine). GetTickCount64 gives the millisecond base, so a target's GetTickCount64 starts
        // near the real uptime; the fake-QUIT base and the real base both start at `start_real`. The axis is
        // re-anchored on every set_multiplier so it never rewinds (H-1). Written in the wall anchor's seqlock.
        // The QPC axis (ADR-2 reversal, opt-in) starts fake == real at the current QPC, so elapsed begins at 0.
        let dur_tick0 = GetTickCount64();
        let start_qpc = qpc_now();
        write_anchor_full(
            ctl, a_fake, start_real, multiplier, dur_tick0, start_real, start_real, start_qpc, start_qpc,
        );
        write_tz_bias(ctl, tz_bias);
        write_scale_dur(ctl, spec.scale_duration);
        write_scale_qpc(ctl, spec.scale_qpc);
        write_core_pid(ctl, GetCurrentProcessId());

        // 2. Launch SUSPENDED so the hook lands before the first instruction.
        let mut app = to_wide(target.path);
        let mut cmdline = build_command_line(target.path, target.args);
        let cwd_wide = target.cwd.map(to_wide);
        let cwd_ptr = cwd_wide
            .as_ref()
            .map(|w| PCWSTR(w.as_ptr()))
            .unwrap_or(PCWSTR::null());
        let si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        let launched = CreateProcessW(
            PCWSTR(app.as_mut_ptr()),
            Some(PWSTR(cmdline.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_SUSPENDED,
            None,
            cwd_ptr,
            &si,
            &mut pi,
        );
        if let Err(e) = launched {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
            let _ = CloseHandle(hmap);
            return Err(PrepareError::Launch(format!("CreateProcessW: {e:?}")));
        }

        // 2a. Refuse a target this core cannot reach, before trying (R2-S1). The process exists but is
        // still suspended - it has not run an instruction - so terminating it here costs the tester
        // nothing and leaves no half-started application behind.
        if let Some((target_bits, core_bits)) = bitness_mismatch(pi.hProcess) {
            let _ = TerminateProcess(pi.hProcess, 1);
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
            let _ = CloseHandle(hmap);
            return Err(PrepareError::BitnessMismatch(target_bits, core_bits));
        }

        // 3. Inject the hook into the suspended target.
        if let Err(e) = inject(pi.hProcess, &dll_wide) {
            // Injection failed, so the target is UNHOOKED and still SUSPENDED (it never ran an instruction).
            // Terminate it (H-2): resuming - the old "let it die naturally" - would run an unhooked process
            // to completion reading REAL time, a silent orphan the caller never asked to run, and hand back
            // a "session" that substituted nothing. Clean up the mapped control view too (M-2).
            let _ = TerminateProcess(pi.hProcess, 1);
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
            let _ = CloseHandle(hmap);
            return Err(e);
        }

        // 4. Find the parent's OWN coverage slot (it published its pid in DllMain, before resume, so
        // this is deterministic) and read the install bitmask. If the hook could not claim a slot
        // (best-effort failure in the target), report no coverage rather than guessing - honest.
        let parent_pid = pi.dwProcessId;
        let parent_slot = find_pid_slot(ctl, parent_pid);
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
        // anything. Terminate and fail loudly instead (the hook's mapped section dies with the target).
        if ResumeThread(pi.hThread) == u32::MAX {
            let _ = TerminateProcess(pi.hProcess, 1);
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
            let _ = CloseHandle(hmap);
            return Err(PrepareError::Inject("ResumeThread failed - target left suspended".into()));
        }
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
        // open - only the thread handle is released here. Session::end releases the rest; the
        // target's own mapped view keeps the control section alive regardless. The parent's slot is
        // marked reported, since its coverage is handed back right here in `Prepared` - without that
        // the first child poll would emit the parent a second time.
        let _ = CloseHandle(pi.hThread);
        let mut reported_slots = vec![false; MAX_COV_PIDS];
        if let Some(slot) = parent_slot {
            reported_slots[slot] = true;
        }
        let session = Session {
            pid: parent_pid,
            ctl_addr: view.Value as usize,
            hmap,
            hprocess: pi.hProcess,
            start_real,
            fake_elapsed_before: std::cell::Cell::new(0),
            rate_segment_real0: std::cell::Cell::new(start_real),
            tz_bias,
            scale_duration: spec.scale_duration,
            scale_qpc: spec.scale_qpc,
            reported_slots,
            _lock: lock,
        };
        Ok(Prepared { coverage, session, vanished_lived_ms, orphan_reclaimed })
    }
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

/// How long to wait for the remote `LoadLibraryW` thread. The hook's `DllMain` does no heavy work (it
/// defers the watcher off the loader lock, ADR-3), so injection is quick; a thread still stuck past this
/// means the target's loader deadlocked (loader lock), and we treat it as an injection failure rather than
/// hang `prepare` forever (M-1).
const INJECT_TIMEOUT_MS: u32 = 10_000;

/// Manual LoadLibrary injection: write the DLL path into the target and run `LoadLibraryW` there on a
/// remote thread. Returns `Err` (and frees the remote page) on any failure, INCLUDING a `LoadLibraryW`
/// that returned NULL in the target - the caller then terminates the still-suspended target rather than
/// resume an unhooked process reading real time (H-2).
unsafe fn inject(hproc: HANDLE, dll_wide: &[u16]) -> Result<(), PrepareError> {
    let bytes = dll_wide.len() * 2;
    let remote = VirtualAllocEx(hproc, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if remote.is_null() {
        return Err(PrepareError::Inject("VirtualAllocEx returned null".into()));
    }
    // Free the remote page on EVERY failure below, not just the write error (L-6). The target is still
    // alive here (the caller terminates it on `Err`), so an un-freed page would leak in its address space.
    let fail = |msg: String| -> PrepareError {
        let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
        PrepareError::Inject(msg)
    };
    if let Err(e) = WriteProcessMemory(hproc, remote, dll_wide.as_ptr() as *const c_void, bytes, None) {
        return Err(fail(format!("WriteProcessMemory: {e:?}")));
    }
    let k32 = match GetModuleHandleA(s!("kernel32.dll")) {
        Ok(h) => h,
        Err(e) => return Err(fail(format!("GetModuleHandleA: {e:?}"))),
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
        Err(e) => return Err(fail(format!("CreateRemoteThread: {e:?}"))),
    };

    // Bounded wait (M-1): a hung DllMain (loader lock) must not hang prepare forever.
    let waited = WaitForSingleObject(hthread, INJECT_TIMEOUT_MS);
    // The remote thread's exit code is the low 32 bits of the HMODULE LoadLibraryW returned; 0 means the
    // DLL did not load (bad architecture, missing runtime dependency, AV block, target tearing down). We
    // only read it when the thread actually finished. (On x64 a module base whose low 32 bits are exactly
    // 0 - a 4 GB-aligned load - would read as 0 too; that false negative is astronomically rare, and
    // refusing is the safe direction: a retry lands a different ASLR base, never an unhooked target.)
    let mut exit_code: u32 = 0;
    let got_code = waited != WAIT_TIMEOUT && GetExitCodeThread(hthread, &mut exit_code).is_ok();
    let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
    let _ = CloseHandle(hthread);

    if waited == WAIT_TIMEOUT {
        return Err(PrepareError::Inject(format!(
            "LoadLibraryW did not return within {INJECT_TIMEOUT_MS} ms (suspected loader lock)"
        )));
    }
    if !got_code || exit_code == 0 {
        return Err(PrepareError::Inject(
            "LoadLibraryW returned NULL in the target (the hook DLL failed to load)".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the QPC-channel test needs this bit, so it is imported here rather than in the lib.
    use chrono_ctl::CH_QPC;

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
}
