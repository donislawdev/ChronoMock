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
    cov_section_name, cov_size, ctl_size, read_anchor, read_calls, read_installed, read_pid,
    write_anchor, write_core_pid, write_scale_dur, write_tz_bias, ChannelCategory, Cov, Ctl,
    CHANNELS, MAX_COV_PIDS,
};
use windows::core::{s, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualAllocEx,
    VirtualFreeEx, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MEM_COMMIT, MEM_RELEASE,
    MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{
    CreateProcessW, CreateRemoteThread, GetCurrentProcessId, GetExitCodeProcess, ResumeThread,
    WaitForSingleObject, CREATE_SUSPENDED, INFINITE, LPTHREAD_START_ROUTINE, PROCESS_INFORMATION,
    STARTUPINFOW,
};
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
    pub fn set_multiplier(&self, m: i64) {
        let now = quit_now();
        let (a_fake, a_real, cur_m) = unsafe { read_anchor(self.ctl()) };
        let fake_now = a_fake.wrapping_add(now.wrapping_sub(a_real).wrapping_mul(cur_m));
        unsafe { write_anchor(self.ctl_mut(), fake_now, now, m) };
    }

    /// Jump the wall clock to `to_ft` (UTC FILETIME), keeping the current multiplier.
    /// The duration axis anchors separately in the hook, so it is not affected - a
    /// backward jump never rewinds it (untouchable rule 3).
    pub fn jump(&self, to_ft: i64) {
        let now = quit_now();
        let (_, _, cur_m) = unsafe { read_anchor(self.ctl()) };
        unsafe { write_anchor(self.ctl_mut(), to_ft, now, cur_m) };
    }

    /// Jump the wall clock by `delta` ticks (100 ns) from its CURRENT fake value, keeping the
    /// multiplier. Relative jump semantics: "advance the session clock by delta", computed
    /// atomically under one anchor read so no real time leaks between reading and re-anchoring.
    pub fn jump_relative(&self, delta: i64) {
        let now = quit_now();
        let (a_fake, a_real, cur_m) = unsafe { read_anchor(self.ctl()) };
        let fake_now = a_fake.wrapping_add(now.wrapping_sub(a_real).wrapping_mul(cur_m));
        unsafe { write_anchor(self.ctl_mut(), fake_now.wrapping_add(delta), now, cur_m) };
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

/// Build one process's coverage from its `Cov` section: the install bitmask and the
/// live per-channel call counters. Iterates the single-source `CHANNELS` table so the
/// report names exactly what the hook installs.
///
/// # Safety
/// `cov` must point to a live, correctly aligned `Cov`.
unsafe fn gather_coverage(cov: *const Cov, installed: u32, scale_duration: bool) -> Coverage {
    let mut out = Coverage::default();
    for (idx, ch) in CHANNELS.iter().enumerate() {
        // The duration axis and the object-wait observation are opt-in: with scale_duration
        // off, their channels are not expected, so they count as nothing.
        if matches!(ch.category, ChannelCategory::Duration | ChannelCategory::WaitObserved)
            && !scale_duration
        {
            continue;
        }
        // Object waits are counted but never scaled (ADR-7 class B, option b): their own bucket,
        // so they never sway the verdict. A failed install just means we are not observing it -
        // not a verdict-affecting gap, so it goes nowhere rather than into `uncovered`.
        if ch.category == ChannelCategory::WaitObserved {
            if installed & ch.bit != 0 {
                out.observed.push(ChannelCoverage {
                    channel: ch.name.to_string(),
                    calls: read_calls(cov, idx),
                });
            }
            continue;
        }
        if installed & ch.bit != 0 {
            out.covered.push(ChannelCoverage {
                channel: ch.name.to_string(),
                calls: read_calls(cov, idx),
            });
        } else {
            out.uncovered.push(ch.name.to_string());
        }
    }
    // An object wait that actually ran under acceleration was left real, not shortened - warn, so
    // the tester knows a time-based object wait did not accelerate (ADR-7 class B, honest gap).
    if out.observed.iter().any(|c| c.calls > 0) {
        out.warning_keys.push("wait.object_waits_not_scaled".to_string());
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
        let view = MapViewOfFile(hmap, FILE_MAP_ALL_ACCESS, 0, 0, ctl_size());
        if view.Value.is_null() {
            let _ = CloseHandle(hmap);
            return Err(PrepareError::Control("MapViewOfFile returned null".into()));
        }
        let ctl = view.Value as *mut Ctl;
        let start_real = quit_now();
        write_anchor(ctl, a_fake, start_real, multiplier);
        write_tz_bias(ctl, tz_bias);
        write_scale_dur(ctl, spec.scale_duration);
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
            let _ = CloseHandle(hmap);
            return Err(PrepareError::Launch(format!("CreateProcessW: {e:?}")));
        }

        // 3. Inject the hook into the suspended target.
        if let Err(e) = inject(pi.hProcess, &dll_wide) {
            let _ = ResumeThread(pi.hThread); // let it die naturally rather than freeze
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
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
        let _ = ResumeThread(pi.hThread);
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
        Ok(Prepared { coverage, session, vanished_lived_ms })
    }
}

/// Build a mutable command line: `"path" arg1 arg2`.
fn build_command_line(path: &str, args: &[String]) -> Vec<u16> {
    let mut s = String::with_capacity(path.len() + 2);
    s.push('"');
    s.push_str(path);
    s.push('"');
    for a in args {
        s.push(' ');
        s.push_str(a);
    }
    to_wide(&s)
}

/// Manual LoadLibrary injection: write the DLL path into the target and run
/// LoadLibraryW there on a remote thread.
unsafe fn inject(hproc: HANDLE, dll_wide: &[u16]) -> Result<(), PrepareError> {
    let bytes = dll_wide.len() * 2;
    let remote = VirtualAllocEx(hproc, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if remote.is_null() {
        return Err(PrepareError::Inject("VirtualAllocEx returned null".into()));
    }
    if let Err(e) = WriteProcessMemory(hproc, remote, dll_wide.as_ptr() as *const c_void, bytes, None) {
        let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
        return Err(PrepareError::Inject(format!("WriteProcessMemory: {e:?}")));
    }
    let k32 = GetModuleHandleA(s!("kernel32.dll"))
        .map_err(|e| PrepareError::Inject(format!("GetModuleHandleA: {e:?}")))?;
    let loadlib = GetProcAddress(k32, s!("LoadLibraryW"))
        .ok_or_else(|| PrepareError::Inject("no LoadLibraryW export".into()))?;
    let start: LPTHREAD_START_ROUTINE = Some(std::mem::transmute::<
        unsafe extern "system" fn() -> isize,
        unsafe extern "system" fn(*mut c_void) -> u32,
    >(loadlib));
    let hthread = CreateRemoteThread(hproc, None, 0, start, Some(remote as *const c_void), 0, None)
        .map_err(|e| PrepareError::Inject(format!("CreateRemoteThread: {e:?}")))?;
    WaitForSingleObject(hthread, INFINITE);
    let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
    let _ = CloseHandle(hthread);
    Ok(())
}
