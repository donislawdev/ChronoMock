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
    cov_section_name, cov_size, ctl_size, freeze_dur, freeze_qpc, read_anchor, read_calls,
    read_core_pid, read_dur, read_installed, read_pid, read_qpc, write_anchor, write_anchor_full,
    write_core_pid, write_scale_dur, write_scale_qpc, write_tz_bias, ChannelCategory, ChannelModule,
    Cov, Ctl, CHANNELS, MAX_COV_PIDS,
};
use windows::core::{s, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualAllocEx,
    VirtualFreeEx, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MEM_COMMIT, MEM_RELEASE,
    MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::SystemInformation::{GetSystemTimeAsFileTime, GetTickCount64};
use windows::Win32::System::Threading::{
    CreateProcessW, CreateRemoteThread, GetCurrentProcessId, GetExitCodeProcess, GetExitCodeThread,
    OpenProcess, ResumeThread, TerminateProcess, WaitForSingleObject, CREATE_SUSPENDED,
    LPTHREAD_START_ROUTINE, PROCESS_INFORMATION, PROCESS_SYNCHRONIZE, STARTUPINFOW,
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

/// One process's mapped coverage section, kept open for the session's lifetime so the
/// section survives even if that process exits (full-family audit).
struct CovMap {
    pid: u32,
    hmap: HANDLE,
    view_addr: usize,
}

/// A live, running session. Keeps the control memory mapped and the target handle
/// open so the core can read state (and later re-anchor) until the session ends.
pub struct Session {
    pub pid: u32,
    ctl_addr: usize,
    hmap: HANDLE,
    hprocess: HANDLE,
    /// Real (QUIT) and fake anchors captured at session start, so elapsed_* stays
    /// measured from the start even after a later re-anchor.
    start_real: i64,
    start_fake: i64,
    tz_bias: i32,
    /// The duration axis is opt-in; coverage gathering needs it to know whether the
    /// Duration channels are expected.
    scale_duration: bool,
    /// Per-process coverage sections (parent + children), kept mapped for the whole
    /// session so a child's evidence survives its exit. Also the record of which PIDs
    /// have been reported, so `poll_new_coverage` emits each process exactly once.
    cov_maps: Vec<CovMap>,
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
        let fake_ft = a_fake.wrapping_add(now_real.wrapping_sub(a_real).wrapping_mul(m));
        SessionState {
            fake_ft,
            real_ft: real_system_filetime(),
            multiplier: m,
            tz_bias: self.tz_bias,
            elapsed_fake_ms: fake_ft.wrapping_sub(self.start_fake) / 10_000,
            elapsed_real_ms: now_real.wrapping_sub(self.start_real) / 10_000,
        }
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
    /// session after `prepare`, ADR-3) and return each one's OWN coverage. Each new
    /// section is mapped and kept for the session, so a child's evidence survives even
    /// if it exits. Idempotent: a pid is returned exactly once across calls.
    pub fn poll_new_coverage(&mut self) -> Vec<(u32, Coverage)> {
        let mut out = Vec::new();
        unsafe {
            for i in 0..MAX_COV_PIDS {
                let pid = read_pid(self.ctl(), i);
                // 0 = empty or reserved-but-not-yet-published; skip and retry later.
                if pid == 0 || self.cov_maps.iter().any(|c| c.pid == pid) {
                    continue;
                }
                if let Some((hmap, addr)) = open_cov(pid) {
                    let cov = addr as *const Cov;
                    let coverage = gather_coverage(cov, read_installed(cov), self.scale_duration);
                    self.cov_maps.push(CovMap { pid, hmap, view_addr: addr });
                    out.push((pid, coverage));
                }
                // Not openable yet (section not published, or process gone before we
                // looked): leave it unseen so a later poll can still pick it up.
            }
        }
        out
    }

    /// Release our own handles, including every mapped coverage section. The target
    /// keeps its own mapped views, so its hooks keep working after we detach (full
    /// residue cleanup is a later slice).
    pub fn end(self) {
        unsafe {
            for cm in &self.cov_maps {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: cm.view_addr as *mut c_void,
                });
                let _ = CloseHandle(cm.hmap);
            }
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

/// Whether a prior session's core (this pid) is still running. Cannot open, or already signaled,
/// means gone. PID reuse is a small hazard: a reused pid reads as alive and we refuse rather than
/// reclaim - the safe direction (refuse), never a wrong reclaim of a live session.
///
/// # Safety
/// Calls the Win32 process APIs; `pid` is otherwise unconstrained.
unsafe fn core_is_alive(pid: u32) -> bool {
    match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
        Ok(h) => {
            let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
            let _ = CloseHandle(h);
            alive
        }
        Err(_) => false,
    }
}

/// Build one process's coverage from its `Cov` section: the install bitmask and the
/// live per-channel call counters. Iterates the single-source `CHANNELS` table so the
/// report names exactly what the hook installs.
///
/// # Safety
/// `cov` must point to a live, correctly aligned `Cov`.
unsafe fn gather_coverage(cov: *const Cov, installed: u64, scale_duration: bool) -> Coverage {
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

/// Open a process's coverage section (`Local\ChronoCov.<pid>`) and map it. The caller
/// keeps the handle+view for the session's lifetime so the section outlives the
/// process. Returns None if the section is not published yet or the process is gone.
///
/// # Safety
/// The returned view must be unmapped and the handle closed exactly once.
unsafe fn open_cov(pid: u32) -> Option<(HANDLE, usize)> {
    let name: Vec<u16> = cov_section_name(pid)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let hmap = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(name.as_ptr())).ok()?;
    let view = MapViewOfFile(hmap, FILE_MAP_ALL_ACCESS, 0, 0, cov_size());
    if view.Value.is_null() {
        let _ = CloseHandle(hmap);
        return None;
    }
    Some((hmap, view.Value as usize))
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

        // Concurrent-session vs orphan. A surviving section whose core is alive is a real second
        // session - refuse (one session at a time, fixed name). A surviving section whose core is
        // dead is an orphan from a killed core (its target self-detached to real time); reclaim it
        // by zeroing the whole block so no stale PID registry or anchor leaks into the new session.
        let mut orphan_reclaimed = false;
        if already_existed {
            let prev_core = read_core_pid(ctl as *const Ctl);
            if prev_core != 0 && core_is_alive(prev_core) {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
                let _ = CloseHandle(hmap);
                return Err(PrepareError::SessionActive(prev_core));
            }
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

        // 4. Open the parent's OWN coverage section (its pid is known) and read the
        // install bitmask set in DllMain (deterministic before resume). If the hook
        // could not publish a section (best-effort failure in the target), report no
        // coverage rather than guessing - honest.
        let parent_pid = pi.dwProcessId;
        let parent_cov = open_cov(parent_pid);
        let installed = match parent_cov {
            Some((_, addr)) => read_installed(addr as *const Cov),
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
            if let Some((hmap_cov, addr)) = parent_cov {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: addr as *mut c_void });
                let _ = CloseHandle(hmap_cov);
            }
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
            let _ = CloseHandle(hmap);
            return Err(PrepareError::Inject("ResumeThread failed - target left suspended".into()));
        }
        let waited = WaitForSingleObject(pi.hProcess, GUARD_MS);
        let coverage = match parent_cov {
            Some((_, addr)) => gather_coverage(addr as *const Cov, installed, spec.scale_duration),
            None => Coverage::default(),
        };
        let vanished_lived_ms = if waited == WAIT_TIMEOUT {
            None
        } else {
            Some(t0.elapsed().as_millis() as u64)
        };

        // 6. Hand back a live session. We keep the control section mapped, the process
        // handle open, and the parent's coverage section mapped - only the thread
        // handle is released here. Session::end releases the rest; the target's own
        // mapped views keep the sections alive regardless.
        let _ = CloseHandle(pi.hThread);
        let mut cov_maps = Vec::new();
        if let Some((hmap_cov, addr)) = parent_cov {
            cov_maps.push(CovMap { pid: parent_pid, hmap: hmap_cov, view_addr: addr });
        }
        let session = Session {
            pid: parent_pid,
            ctl_addr: view.Value as usize,
            hmap,
            hprocess: pi.hProcess,
            start_real,
            start_fake: a_fake,
            tz_bias,
            scale_duration: spec.scale_duration,
            cov_maps,
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

/// The `"path" arg1 arg2` command line as text, each argument quoted with `quote_arg` so a target
/// argument with spaces or quotes survives the round trip through CreateProcess.
fn command_line_string(path: &str, args: &[String]) -> String {
    let mut s = String::with_capacity(path.len() + 2);
    s.push('"');
    s.push_str(path);
    s.push('"');
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
}
