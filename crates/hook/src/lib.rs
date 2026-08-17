//! Chrono Mock injected hook (Stage 6): substitute the full set of wall-clock
//! channels, report the session zone, and optionally scale the duration axis.
//!
//! On `DLL_PROCESS_ATTACH` this opens the session control memory (`Local\ChronoCtl`),
//! installs a MinHook detour on every time export listed in `chrono_ctl::CHANNELS`,
//! and records each covered channel in the control block. The wall detours return
//! `a_fake + (quit_now - a_real) * multiplier` (multiplier from the anchor), anchored
//! on `QueryUnbiasedInterruptTime` (ADR-5). The UTC channels return that instant
//! directly, `GetLocalTime` shifts it back into the session zone by `tz_bias`, and
//! the zone detours report the session zone (`Bias = tz_bias`, no DST) so a target
//! that asks its offset agrees with `GetLocalTime`.
//!
//! The duration axis (`GetTickCount`, `GetTickCount64`, `QueryUnbiasedInterruptTime`) is
//! scaled by the multiplier only when scale_duration is set, and never below real speed - so the
//! monotonic clock keeps advancing even when the wall clock is frozen (untouchable
//! rule 3). `QueryPerformanceCounter` and `timeGetTime` are deliberately left real
//! (ADR-2). Once QUIT is hooked, the anchor math reads it through the trampoline so the
//! scaled output never feeds back.
//!
//! ABSOLUTE, not delta: a detour computes the fake instant from the anchor and never
//! calls another channel's original. So there is no cross-channel re-entrancy and no
//! double-shift, and hence no thread-local re-entrancy guard - the spike's E2 guard
//! was an artifact of an earlier delta design (`original + delta`) and does not apply.
//!
//! Child processes inherit the session via `CreateProcessW` / `CreateProcessA` detours
//! (ADR-3). `NtCreateUserProcess` is not covered yet.

#![allow(non_snake_case)]

use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use chrono_ctl::{
    bump_calls, cov_section_name, cov_size, mark_channel_installed, read_anchor, read_core_pid,
    read_scale_dur, read_tz_bias, register_pid, ChannelModule, Cov, Ctl, CHANNELS, IDX_GDTZI,
    IDX_GLT, IDX_GST, IDX_GSTAFT, IDX_GSTPAFT, IDX_GTC, IDX_GTC64, IDX_GTZI, IDX_NTQST, IDX_QUIT,
};
use minhook::MinHook;
use windows::core::{s, w, PCSTR, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, FILETIME, HANDLE, HMODULE, INVALID_HANDLE_VALUE, SYSTEMTIME,
};
use windows::Win32::System::Diagnostics::Debug::{OutputDebugStringA, WriteProcessMemory};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleA, GetProcAddress,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, VirtualAllocEx, VirtualFreeEx,
    FILE_MAP_ALL_ACCESS, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::System::Threading::{
    CreateRemoteThread, CreateThread, GetCurrentProcessId, OpenProcess, ResumeThread,
    WaitForSingleObject, CREATE_SUSPENDED, INFINITE, LPTHREAD_START_ROUTINE, PROCESS_INFORMATION,
    PROCESS_SYNCHRONIZE, THREAD_CREATION_FLAGS,
};
use windows::Win32::System::Time::{
    FileTimeToSystemTime, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_INFORMATION,
};
use windows::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime;

type FtFn = unsafe extern "system" fn(*mut FILETIME);
type StFn = unsafe extern "system" fn(*mut SYSTEMTIME);
type NtqstFn = unsafe extern "system" fn(*mut i64) -> i32;
type TziFn = unsafe extern "system" fn(*mut TIME_ZONE_INFORMATION) -> u32;
type DtziFn = unsafe extern "system" fn(*mut DYNAMIC_TIME_ZONE_INFORMATION) -> u32;
type TickFn = unsafe extern "system" fn() -> u64;
type Tick32Fn = unsafe extern "system" fn() -> u32;
type QuitFn = unsafe extern "system" fn(*mut u64) -> i32;
type CpwFn = unsafe extern "system" fn(
    *const u16,
    *mut u16,
    *const c_void,
    *const c_void,
    i32,
    u32,
    *const c_void,
    *const u16,
    *const c_void,
    *mut PROCESS_INFORMATION,
) -> i32;
// CreateProcessA: same ABI shape as CreateProcessW, only the string params are ANSI. We
// forward them opaquely (never read them), so u8 pointers are enough.
type CpaFn = unsafe extern "system" fn(
    *const u8,
    *mut u8,
    *const c_void,
    *const c_void,
    i32,
    u32,
    *const c_void,
    *const u8,
    *const c_void,
    *mut PROCESS_INFORMATION,
) -> i32;

static CTL_PTR: OnceLock<usize> = OnceLock::new();
static COV_PTR: OnceLock<usize> = OnceLock::new();
static TZ_BIAS: OnceLock<i32> = OnceLock::new();

static O_GSTAFT: OnceLock<FtFn> = OnceLock::new();
static O_GSTPAFT: OnceLock<FtFn> = OnceLock::new();
static O_GST: OnceLock<StFn> = OnceLock::new();
static O_GLT: OnceLock<StFn> = OnceLock::new();
static O_NTQST: OnceLock<NtqstFn> = OnceLock::new();
static O_GTZI: OnceLock<TziFn> = OnceLock::new();
static O_GDTZI: OnceLock<DtziFn> = OnceLock::new();
static O_TICK: OnceLock<TickFn> = OnceLock::new();
static O_TICK32: OnceLock<Tick32Fn> = OnceLock::new();
static O_QUIT: OnceLock<QuitFn> = OnceLock::new();

// Duration-axis anchors, captured real (before hooking) at install time.
static Q0: OnceLock<i64> = OnceLock::new();
static C0_TICK: OnceLock<u64> = OnceLock::new();

// Child inheritance (ADR-3): our own module handle (to inject the same DLL into a
// child) and the CreateProcessW trampoline.
static SELF_HMOD: OnceLock<usize> = OnceLock::new();
static O_CPW: OnceLock<CpwFn> = OnceLock::new();
static O_CPA: OnceLock<CpaFn> = OnceLock::new();

// Self-detach: a SYNCHRONIZE handle to the core process, and the flag a watcher flips
// when the core vanishes so every detour reverts to real time.
static CORE_HANDLE: OnceLock<usize> = OnceLock::new();
static DETACHED: AtomicBool = AtomicBool::new(false);
static WATCHER_STARTED: AtomicBool = AtomicBool::new(false);

fn ctl_ptr() -> Option<*mut Ctl> {
    CTL_PTR.get().map(|a| *a as *mut Ctl)
}

fn cov_ptr() -> Option<*mut Cov> {
    COV_PTR.get().map(|a| *a as *mut Cov)
}

/// UTF-16, NUL-terminated - for a section name built at runtime (the pid varies, so
/// the compile-time `w!` macro used for the fixed `ChronoCtl` name cannot serve here).
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// --- Self-detach: revert to real time when the core vanishes --------------------
// The core writes its PID into the control block; we open a SYNCHRONIZE handle to it.
// On the first time call we spawn a watcher that blocks on that handle. When the core
// dies (clean end, crash, or kill -9) the OS signals it, we flip DETACHED, and every
// detour falls through to the original - the target's clock returns to real time.

unsafe extern "system" fn watcher_proc(_p: *mut c_void) -> u32 {
    if let Some(&h) = CORE_HANDLE.get() {
        WaitForSingleObject(HANDLE(h as *mut c_void), INFINITE);
    }
    DETACHED.store(true, Ordering::SeqCst);
    0
}

/// Spawn the watcher once, lazily - NOT from DllMain, to stay clear of the loader lock.
fn ensure_watcher() {
    if CORE_HANDLE.get().is_some()
        && WATCHER_STARTED
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        unsafe {
            if let Ok(h) =
                CreateThread(None, 0, Some(watcher_proc), None, THREAD_CREATION_FLAGS(0), None)
            {
                let _ = CloseHandle(h);
            }
        }
    }
}

/// Has the core vanished? Also lazily starts the watcher on the first call.
fn detached() -> bool {
    ensure_watcher();
    DETACHED.load(Ordering::SeqCst)
}

/// Real (unbiased) monotonic anchor base - ADR-5. QUIT may be hooked for the duration
/// axis, so prefer the trampoline (the real value) to keep our scaled output from
/// feeding back into the anchor math. Before QUIT is hooked, call it directly.
fn real_quit() -> i64 {
    if let Some(o) = O_QUIT.get() {
        let mut t: u64 = 0;
        unsafe { o(&mut t) };
        t as i64
    } else {
        let mut t: u64 = 0;
        unsafe {
            let _ = QueryUnbiasedInterruptTime(&mut t);
        }
        t as i64
    }
}

fn compute_fake() -> Option<i64> {
    if detached() {
        return None; // core gone: wall detours fall through to the real value
    }
    let p = ctl_ptr()? as *const Ctl;
    let (a_fake, a_real, m) = unsafe { read_anchor(p) };
    let dq = real_quit().wrapping_sub(a_real);
    Some(a_fake.wrapping_add(dq.wrapping_mul(m)))
}

fn cur_tz_bias() -> i32 {
    *TZ_BIAS.get().unwrap_or(&0)
}

/// Current multiplier from the anchor (the wall-clock speed factor).
fn cur_m() -> i64 {
    match ctl_ptr() {
        Some(p) => unsafe { read_anchor(p as *const Ctl).2 },
        None => 1,
    }
}

/// Duration multiplier: never below 1, so the monotonic clock keeps advancing even
/// when the wall clock is frozen (M = 0) - untouchable rule 3 (duration is monotonic
/// unconditionally).
fn dur_multiplier() -> i64 {
    cur_m().max(1)
}

fn i64_to_ft(t: i64) -> FILETIME {
    FILETIME {
        dwLowDateTime: (t as u64 & 0xFFFF_FFFF) as u32,
        dwHighDateTime: ((t as u64) >> 32) as u32,
    }
}

fn bump(idx: usize) {
    if let Some(p) = cov_ptr() {
        unsafe { bump_calls(p, idx) }
    }
}

/// Convert a fake UTC FILETIME (100 ns ticks) into `*lp` as a SYSTEMTIME.
///
/// # Safety
/// `lp` must be a valid, writable pointer to a `SYSTEMTIME`.
unsafe fn write_systemtime(lp: *mut SYSTEMTIME, ft_ticks: i64) {
    let ft = i64_to_ft(ft_ticks);
    let mut st = SYSTEMTIME::default();
    if FileTimeToSystemTime(&ft, &mut st).is_ok() {
        *lp = st;
    }
}

// --- Detours -------------------------------------------------------------------
// Each fills its out-parameter with the fake instant, or falls back to the original
// if the anchor is unreadable or the pointer is null.

unsafe extern "system" fn h_gstaft(lp: *mut FILETIME) {
    bump(IDX_GSTAFT);
    match compute_fake() {
        Some(t) if !lp.is_null() => *lp = i64_to_ft(t),
        _ => {
            if let Some(o) = O_GSTAFT.get() {
                o(lp)
            }
        }
    }
}

unsafe extern "system" fn h_gstpaft(lp: *mut FILETIME) {
    bump(IDX_GSTPAFT);
    match compute_fake() {
        Some(t) if !lp.is_null() => *lp = i64_to_ft(t),
        _ => {
            if let Some(o) = O_GSTPAFT.get() {
                o(lp)
            }
        }
    }
}

unsafe extern "system" fn h_gst(lp: *mut SYSTEMTIME) {
    bump(IDX_GST);
    match compute_fake() {
        Some(t) if !lp.is_null() => write_systemtime(lp, t),
        _ => {
            if let Some(o) = O_GST.get() {
                o(lp)
            }
        }
    }
}

unsafe extern "system" fn h_glt(lp: *mut SYSTEMTIME) {
    bump(IDX_GLT);
    match compute_fake() {
        // local = UTC_fake - Bias (UTC = local + Bias), session zone without DST.
        Some(t) if !lp.is_null() => {
            let bias_100ns = cur_tz_bias() as i64 * 60 * 10_000_000;
            write_systemtime(lp, t - bias_100ns);
        }
        _ => {
            if let Some(o) = O_GLT.get() {
                o(lp)
            }
        }
    }
}

unsafe extern "system" fn h_ntqst(lp: *mut i64) -> i32 {
    bump(IDX_NTQST);
    match compute_fake() {
        Some(t) if !lp.is_null() => {
            *lp = t;
            0 // STATUS_SUCCESS
        }
        Some(_) => 0,
        None => O_NTQST.get().map(|o| o(lp)).unwrap_or(0),
    }
}

// --- Session zone -------------------------------------------------------------
// Report the session zone (Bias = tz_bias, no DST) so a target's notion of "which
// zone am I in" agrees with the shifted GetLocalTime.

const SESSION_ZONE_NAME: &str = "Chrono Session";
const TIME_ZONE_ID_INVALID: u32 = 0xFFFF_FFFF;

/// Copy `s` into a fixed-length UTF-16 field, NUL-terminated (zone name buffers).
fn set_wide(dst: &mut [u16], s: &str) {
    let mut i = 0;
    for c in s.encode_utf16() {
        if i + 1 >= dst.len() {
            break;
        }
        dst[i] = c;
        i += 1;
    }
    if i < dst.len() {
        dst[i] = 0;
    }
}

unsafe extern "system" fn h_gtzi(lp: *mut TIME_ZONE_INFORMATION) -> u32 {
    bump(IDX_GTZI);
    if detached() {
        return O_GTZI.get().map(|o| o(lp)).unwrap_or(TIME_ZONE_ID_INVALID);
    }
    if !lp.is_null() {
        let mut tzi = TIME_ZONE_INFORMATION { Bias: cur_tz_bias(), ..Default::default() };
        set_wide(&mut tzi.StandardName, SESSION_ZONE_NAME);
        *lp = tzi;
        return 0; // TIME_ZONE_ID_UNKNOWN - the session zone has no DST
    }
    O_GTZI.get().map(|o| o(lp)).unwrap_or(TIME_ZONE_ID_INVALID)
}

unsafe extern "system" fn h_gdtzi(lp: *mut DYNAMIC_TIME_ZONE_INFORMATION) -> u32 {
    bump(IDX_GDTZI);
    if detached() {
        return O_GDTZI.get().map(|o| o(lp)).unwrap_or(TIME_ZONE_ID_INVALID);
    }
    if !lp.is_null() {
        let mut d = DYNAMIC_TIME_ZONE_INFORMATION { Bias: cur_tz_bias(), ..Default::default() };
        set_wide(&mut d.StandardName, SESSION_ZONE_NAME);
        set_wide(&mut d.TimeZoneKeyName, SESSION_ZONE_NAME);
        *lp = d;
        return 0; // TIME_ZONE_ID_UNKNOWN - the session zone has no DST
    }
    O_GDTZI.get().map(|o| o(lp)).unwrap_or(TIME_ZONE_ID_INVALID)
}

// --- Duration axis (opt-in) ----------------------------------------------------
// Only installed when scale_duration is set. Elapsed since the real anchor Q0 is
// scaled by dur_multiplier() (>= 1) off the trampoline QUIT. Monotonic by
// construction. QPC and timeGetTime are left real (ADR-2).

unsafe extern "system" fn h_tick() -> u64 {
    bump(IDX_GTC64);
    if detached() {
        return O_TICK.get().map(|o| o()).unwrap_or(0);
    }
    let q0 = *Q0.get().unwrap_or(&0);
    let dq = real_quit().wrapping_sub(q0); // 100 ns
    let dms = dq.wrapping_mul(dur_multiplier()) / 10_000; // scale before /10000 (100ns -> ms)
    C0_TICK.get().copied().unwrap_or(0).wrapping_add(dms as u64)
}

// GetTickCount (32-bit): the low 32 bits of the SAME scaled millisecond count as
// GetTickCount64 (shares the C0_TICK base), so a target comparing the two sees them
// agree. Wraps at 2^32 ms like the real one - and sooner under acceleration - which is the
// honest behavior of a fast 32-bit counter; callers handle the wrap with unsigned deltas.
unsafe extern "system" fn h_tick32() -> u32 {
    bump(IDX_GTC);
    if detached() {
        return O_TICK32.get().map(|o| o()).unwrap_or(0);
    }
    let q0 = *Q0.get().unwrap_or(&0);
    let dq = real_quit().wrapping_sub(q0); // 100 ns
    let dms = dq.wrapping_mul(dur_multiplier()) / 10_000; // scale before /10000 (100ns -> ms)
    C0_TICK.get().copied().unwrap_or(0).wrapping_add(dms as u64) as u32
}

unsafe extern "system" fn h_quit(lp: *mut u64) -> i32 {
    bump(IDX_QUIT);
    if detached() {
        return O_QUIT.get().map(|o| o(lp)).unwrap_or(0);
    }
    if !lp.is_null() {
        let q0 = *Q0.get().unwrap_or(&0);
        let dq = real_quit().wrapping_sub(q0);
        *lp = q0.wrapping_add(dq.wrapping_mul(dur_multiplier())) as u64;
    }
    1 // nonzero BOOL = success
}

// --- Child inheritance (ADR-3) --------------------------------------------------
// Detour CreateProcessW so children join the session: create suspended, inject the
// same DLL, then resume (unless the caller wanted it suspended). The child opens the
// shared Ctl and hooks itself, so it sees the same wall clock as the parent.

/// Inject this DLL into `hproc` by writing our own module path and running
/// LoadLibraryW there. Best-effort: on failure the child simply runs uncovered.
///
/// # Safety
/// `hproc` must be a valid process handle with injection rights.
unsafe fn inject_self(hproc: HANDLE) {
    let addr = *SELF_HMOD.get().unwrap_or(&0);
    if addr == 0 {
        return;
    }
    let hmod = HMODULE(addr as *mut c_void);
    let mut buf = [0u16; 260];
    let n = GetModuleFileNameW(Some(hmod), &mut buf);
    if n == 0 {
        return;
    }
    let bytes = (n as usize + 1) * 2; // include the NUL terminator
    let remote = VirtualAllocEx(hproc, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if remote.is_null() {
        return;
    }
    if WriteProcessMemory(hproc, remote, buf.as_ptr() as *const c_void, bytes, None).is_err() {
        let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
        return;
    }
    let k32 = match GetModuleHandleA(s!("kernel32.dll")) {
        Ok(h) => h,
        Err(_) => {
            let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
            return;
        }
    };
    let loadlib = GetProcAddress(k32, s!("LoadLibraryW"));
    let start: LPTHREAD_START_ROUTINE = std::mem::transmute(loadlib);
    if let Ok(hthread) =
        CreateRemoteThread(hproc, None, 0, start, Some(remote as *const c_void), 0, None)
    {
        WaitForSingleObject(hthread, INFINITE);
        let _ = CloseHandle(hthread);
    }
    let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
}

/// After a create call we forced to CREATE_SUSPENDED returns, inject the hook into the
/// new child so it joins the session, then resume it unless the caller originally asked
/// for a suspended child. Shared by the CreateProcessW and CreateProcessA detours.
///
/// # Safety
/// `pi`, when non-null, must point to a PROCESS_INFORMATION filled by a successful create.
unsafe fn inherit_into_child(r: i32, pi: *mut PROCESS_INFORMATION, want_suspended: bool) {
    if r != 0 && !pi.is_null() {
        let info = *pi;
        inject_self(info.hProcess);
        if !want_suspended {
            let _ = ResumeThread(info.hThread);
        }
    }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn h_cpw(
    app: *const u16,
    cmd: *mut u16,
    pa: *const c_void,
    ta: *const c_void,
    inherit: i32,
    flags: u32,
    env: *const c_void,
    cwd: *const u16,
    si: *const c_void,
    pi: *mut PROCESS_INFORMATION,
) -> i32 {
    let o = match O_CPW.get() {
        Some(o) => o,
        None => return 0,
    };
    let want_suspended = (flags & CREATE_SUSPENDED.0) != 0;
    let r = o(app, cmd, pa, ta, inherit, flags | CREATE_SUSPENDED.0, env, cwd, si, pi);
    inherit_into_child(r, pi, want_suspended);
    r
}

// CreateProcessA bypasses the CreateProcessW export (it funnels through the internal
// CreateProcessInternalW), so a parent spawning with the ANSI API would escape the
// session unless we hook A too. Mirror of h_cpw with ANSI string params.
#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn h_cpa(
    app: *const u8,
    cmd: *mut u8,
    pa: *const c_void,
    ta: *const c_void,
    inherit: i32,
    flags: u32,
    env: *const c_void,
    cwd: *const u8,
    si: *const c_void,
    pi: *mut PROCESS_INFORMATION,
) -> i32 {
    let o = match O_CPA.get() {
        Some(o) => o,
        None => return 0,
    };
    let want_suspended = (flags & CREATE_SUSPENDED.0) != 0;
    let r = o(app, cmd, pa, ta, inherit, flags | CREATE_SUSPENDED.0, env, cwd, si, pi);
    inherit_into_child(r, pi, want_suspended);
    r
}

/// Diagnostics only (stderr-equivalent for an injected DLL); never affects coverage.
fn log(msg: &str) {
    if let Ok(c) = CString::new(msg) {
        unsafe { OutputDebugStringA(PCSTR(c.as_ptr() as *const u8)) }
    }
}

/// Resolve, create, and record one channel's detour. Best-effort: a missing export
/// or a failed hook logs and leaves the bit unset (honest partial), never aborts the
/// rest. The export name and module come from `CHANNELS[idx]` - single source. The
/// installed bit is marked in this process's own `Cov`; with no coverage section
/// (`cov` is None) the detour is still installed, it just goes unreported.
///
/// # Safety
/// `cov`, when Some, must point to a live `Cov`; `detour` must be correct for `slot`.
unsafe fn make_hook<T: Copy>(
    cov: Option<*mut Cov>,
    k32: HMODULE,
    ntdll: HMODULE,
    idx: usize,
    detour: *mut c_void,
    slot: &OnceLock<T>,
) {
    let ch = &CHANNELS[idx];
    let module = match ch.module {
        ChannelModule::Kernel32 => k32,
        ChannelModule::Ntdll => ntdll,
    };
    let cname = match CString::new(ch.name) {
        Ok(c) => c,
        Err(_) => {
            log(&format!("[chrono_hook] bad channel name: {}", ch.name));
            return;
        }
    };
    let target = match GetProcAddress(module, PCSTR(cname.as_ptr() as *const u8)) {
        Some(f) => f,
        None => {
            log(&format!("[chrono_hook] no export: {}", ch.name));
            return;
        }
    };
    match MinHook::create_hook(target as *const () as *mut c_void, detour) {
        Ok(original) => {
            let _ = slot.set(std::mem::transmute_copy::<*mut c_void, T>(&original));
            if let Some(c) = cov {
                mark_channel_installed(c, ch.bit);
            }
        }
        Err(e) => log(&format!("[chrono_hook] create_hook {} failed: {e:?}", ch.name)),
    }
}

unsafe fn install() -> Result<(), String> {
    let hmap = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, w!("Local\\ChronoCtl"))
        .map_err(|e| format!("OpenFileMappingW: {e:?}"))?;
    let view = MapViewOfFile(hmap, FILE_MAP_ALL_ACCESS, 0, 0, chrono_ctl::ctl_size());
    if view.Value.is_null() {
        return Err("MapViewOfFile returned null".into());
    }
    let ctl = view.Value as *mut Ctl;
    let _ = CTL_PTR.set(view.Value as usize);
    let _ = TZ_BIAS.set(read_tz_bias(ctl as *const Ctl));

    // Watch the core process so the target reverts to real time if the core vanishes.
    let core_pid = read_core_pid(ctl as *const Ctl);
    if core_pid != 0 {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, core_pid) {
            let _ = CORE_HANDLE.set(h.0 as usize);
        }
    }

    // This process's OWN coverage section (Local\ChronoCov.<pid>), so its calls are
    // attributed to it and never summed into the parent's report. Best-effort: on
    // failure the detours still substitute time (they read the shared anchor via
    // CTL_PTR), but this process reports no coverage and does not register its PID -
    // the mechanism simply never sees it, it never fabricates coverage.
    let pid = GetCurrentProcessId();
    let cov: Option<*mut Cov> = {
        let name = to_wide(&cov_section_name(pid));
        match CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE,
            0,
            cov_size() as u32,
            PCWSTR(name.as_ptr()),
        ) {
            Ok(hmap_cov) => {
                let cview = MapViewOfFile(hmap_cov, FILE_MAP_ALL_ACCESS, 0, 0, cov_size());
                if cview.Value.is_null() {
                    log("[chrono_hook] MapViewOfFile(cov) returned null");
                    None
                } else {
                    let cptr = cview.Value as usize;
                    let _ = COV_PTR.set(cptr);
                    Some(cptr as *mut Cov)
                }
            }
            Err(e) => {
                log(&format!("[chrono_hook] CreateFileMapping(cov) failed: {e:?}"));
                None
            }
        }
    };

    let k32 = GetModuleHandleA(s!("kernel32.dll")).map_err(|e| format!("{e:?}"))?;
    let ntdll = GetModuleHandleA(s!("ntdll.dll")).map_err(|e| format!("{e:?}"))?;

    make_hook(cov, k32, ntdll, IDX_GSTAFT, h_gstaft as *const () as *mut c_void, &O_GSTAFT);
    make_hook(cov, k32, ntdll, IDX_GSTPAFT, h_gstpaft as *const () as *mut c_void, &O_GSTPAFT);
    make_hook(cov, k32, ntdll, IDX_GST, h_gst as *const () as *mut c_void, &O_GST);
    make_hook(cov, k32, ntdll, IDX_GLT, h_glt as *const () as *mut c_void, &O_GLT);
    make_hook(cov, k32, ntdll, IDX_NTQST, h_ntqst as *const () as *mut c_void, &O_NTQST);
    make_hook(cov, k32, ntdll, IDX_GTZI, h_gtzi as *const () as *mut c_void, &O_GTZI);
    make_hook(cov, k32, ntdll, IDX_GDTZI, h_gdtzi as *const () as *mut c_void, &O_GDTZI);

    // Duration axis (opt-in). Capture the real anchors BEFORE creating the hooks:
    // O_QUIT is still unset so real_quit() reads the real value, and GetTickCount64 is
    // not patched until enable_all_hooks(). QPC / timeGetTime stay real (ADR-2).
    if read_scale_dur(ctl as *const Ctl) {
        let _ = Q0.set(real_quit());
        if let Some(tick0) = GetProcAddress(k32, s!("GetTickCount64")) {
            let tick_fn: TickFn = std::mem::transmute(tick0);
            let _ = C0_TICK.set(tick_fn());
        }
        make_hook(cov, k32, ntdll, IDX_GTC64, h_tick as *const () as *mut c_void, &O_TICK);
        make_hook(cov, k32, ntdll, IDX_GTC, h_tick32 as *const () as *mut c_void, &O_TICK32);
        make_hook(cov, k32, ntdll, IDX_QUIT, h_quit as *const () as *mut c_void, &O_QUIT);
    }

    // Child inheritance (ADR-3): hook CreateProcessW and CreateProcessA so the whole
    // process tree joins the session whichever spawn API the parent uses. Not coverage
    // channels - plumbing, not time sources. (CreateProcessA funnels through the internal
    // CreateProcessInternalW, not the W export, so the two detours never re-enter.)
    if let Some(cpw) = GetProcAddress(k32, s!("CreateProcessW")) {
        match MinHook::create_hook(
            cpw as *const () as *mut c_void,
            h_cpw as *const () as *mut c_void,
        ) {
            Ok(original) => {
                let _ = O_CPW.set(std::mem::transmute::<*mut c_void, CpwFn>(original));
            }
            Err(e) => log(&format!("[chrono_hook] create_hook CreateProcessW failed: {e:?}")),
        }
    }
    if let Some(cpa) = GetProcAddress(k32, s!("CreateProcessA")) {
        match MinHook::create_hook(
            cpa as *const () as *mut c_void,
            h_cpa as *const () as *mut c_void,
        ) {
            Ok(original) => {
                let _ = O_CPA.set(std::mem::transmute::<*mut c_void, CpaFn>(original));
            }
            Err(e) => log(&format!("[chrono_hook] create_hook CreateProcessA failed: {e:?}")),
        }
    }

    MinHook::enable_all_hooks().map_err(|e| format!("enable_all_hooks: {e:?}"))?;

    // Publish our PID LAST - after the coverage section exists and the hooks are
    // installed - so the mechanism never reads a pid whose ChronoCov.<pid> is not yet
    // ready. Only if we actually have a section to report (best-effort above).
    if cov.is_some() && !register_pid(ctl, pid) {
        log("[chrono_hook] PID registry full - this process runs uncovered in the audit");
    }
    Ok(())
}

#[no_mangle]
pub extern "system" fn DllMain(hinst: HMODULE, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        // Remember our own module so we can inject the same DLL into children (ADR-3).
        let _ = SELF_HMOD.set(hinst.0 as usize);
        unsafe {
            if let Err(e) = install() {
                log(&format!("[chrono_hook] install failed: {e}"));
            }
        }
    }
    1
}
