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

use chrono_core::{ChannelCoverage, Coverage, SessionSpec, TimeMode};
use chrono_ctl::{
    ctl_size, read_calls, read_installed, write_anchor, write_scale_dur, write_tz_bias,
    ChannelCategory, Ctl, CHANNELS,
};
use windows::core::{s, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, VirtualAllocEx, VirtualFreeEx, FILE_MAP_ALL_ACCESS,
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    CreateProcessW, CreateRemoteThread, ResumeThread, WaitForSingleObject, CREATE_SUSPENDED,
    INFINITE, LPTHREAD_START_ROUTINE, PROCESS_INFORMATION, STARTUPINFOW,
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

/// A prepared (running) session.
pub struct Prepared {
    pub pid: u32,
    /// Which channels were covered, each with a live call count sampled shortly after
    /// resume - evidence the substitution is actually being served, on top of the
    /// install flag.
    pub coverage: Coverage,
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

/// Build coverage from the install bitmask and the live per-channel call counters.
/// Iterates the single-source `CHANNELS` table so the report names exactly what the
/// hook installs.
///
/// # Safety
/// `ctl` must point to a live, correctly aligned `Ctl`.
unsafe fn gather_coverage(ctl: *const Ctl, installed: u32, scale_duration: bool) -> Coverage {
    let mut cov = Coverage::default();
    for (idx, ch) in CHANNELS.iter().enumerate() {
        // The duration axis is opt-in: with scale_duration off, its channels are not
        // expected, so they count as neither covered nor uncovered.
        if ch.category == ChannelCategory::Duration && !scale_duration {
            continue;
        }
        if installed & ch.bit != 0 {
            cov.covered.push(ChannelCoverage {
                channel: ch.name.to_string(),
                calls: read_calls(ctl, idx),
            });
        } else {
            cov.uncovered.push(ch.name.to_string());
        }
    }
    cov
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
        write_anchor(ctl, a_fake, quit_now(), multiplier);
        write_tz_bias(ctl, tz_bias);
        write_scale_dur(ctl, spec.scale_duration);

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

        // 4. Read the install bitmask set in DllMain (deterministic before resume).
        let installed = read_installed(ctl);

        // 5. Resume, then sample per-channel call counters as live evidence.
        let _ = ResumeThread(pi.hThread);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let coverage = gather_coverage(ctl as *const Ctl, installed, spec.scale_duration);

        let pid = pi.dwProcessId;
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
        // Our handle to the section is closed here; the target's mapped view keeps
        // the section alive, so the hook keeps reading a valid anchor after we exit.
        let _ = CloseHandle(hmap);

        Ok(Prepared { pid, coverage })
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
