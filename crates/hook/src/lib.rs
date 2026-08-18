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
//! Under the SAME scale_duration flag the wait axis is scaled too (ADR-7): a wait's
//! timeout is divided by the multiplier (real wait = requested / M), so a thread that
//! blocks on time wakes in lockstep with the scaled clock it reads. `INFINITE` and 0 pass
//! through untouched. `Sleep`, `SleepEx`, and the shared funnel `NtDelayExecution` are covered
//! (ADR-7 class A) - a thread-local guard scales an internal cascade (Sleep or SleepEx bottoming
//! out on NtDelayExecution) exactly once. Object waits (`WaitForSingleObject`, ADR-7 class B) are
//! COUNTED but deliberately NOT scaled - shortening a wait on real I/O would fake a timeout, so they
//! ride their own `observed` bucket with an audit warning. The settable timers are a later slice.
//!
//! ABSOLUTE, not delta: a detour computes the fake instant from the anchor and never
//! calls another channel's original. So there is no cross-channel re-entrancy and no
//! double-shift, and hence no thread-local re-entrancy guard - the spike's E2 guard
//! was an artifact of an earlier delta design (`original + delta`) and does not apply.
//! The one wall exception is `NtQuerySystemInformation(SystemTimeOfDayInformation)`: a syscall
//! stub returns the real time from the kernel, so its detour wraps its OWN original and patches
//! only the CurrentTime field (the other fields stay real). It still calls no other channel's
//! original, so the invariant holds.
//!
//! Child processes inherit the session via `CreateProcessW` / `CreateProcessA` detours
//! (ADR-3). `NtCreateUserProcess` is not covered yet.

#![allow(non_snake_case)]

use std::cell::Cell;
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use chrono_ctl::{
    bump_calls, cov_section_name, cov_size, mark_channel_installed, read_anchor, read_core_pid,
    read_scale_dur, read_tz_bias, register_pid, scale_delay_interval, scale_wait, ChannelModule,
    Cov, Ctl, CHANNELS, IDX_GDTZI, IDX_GLT, IDX_GST, IDX_GSTAFT, IDX_GSTPAFT, IDX_GTC, IDX_GTC64,
    IDX_GTZI, IDX_NTDELAY, IDX_NTQSI, IDX_NTQST, IDX_QUIT, IDX_SLEEP, IDX_SLEEPEX, IDX_STSL,
    IDX_STSLEX, IDX_FTLFT, IDX_LFTFT, IDX_TLTST, IDX_TLTSTEX, IDX_WFSO,
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
    FileTimeToSystemTime, SystemTimeToFileTime, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_INFORMATION,
};
use windows::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime;

type FtFn = unsafe extern "system" fn(*mut FILETIME);
type StFn = unsafe extern "system" fn(*mut SYSTEMTIME);
type NtqstFn = unsafe extern "system" fn(*mut i64) -> i32;
type TziFn = unsafe extern "system" fn(*mut TIME_ZONE_INFORMATION) -> u32;
type DtziFn = unsafe extern "system" fn(*mut DYNAMIC_TIME_ZONE_INFORMATION) -> u32;
type StslFn = unsafe extern "system" fn(*const TIME_ZONE_INFORMATION, *const SYSTEMTIME, *mut SYSTEMTIME) -> i32;
type StslexFn = unsafe extern "system" fn(*const DYNAMIC_TIME_ZONE_INFORMATION, *const SYSTEMTIME, *mut SYSTEMTIME) -> i32;
type FtConvFn = unsafe extern "system" fn(*const FILETIME, *mut FILETIME) -> i32;
type TickFn = unsafe extern "system" fn() -> u64;
type Tick32Fn = unsafe extern "system" fn() -> u32;
type QuitFn = unsafe extern "system" fn(*mut u64) -> i32;
type SleepFn = unsafe extern "system" fn(u32);
type SleepExFn = unsafe extern "system" fn(u32, i32) -> u32;
// NtDelayExecution(BOOLEAN Alertable, PLARGE_INTEGER Interval) -> NTSTATUS. Interval is 100 ns:
// negative = relative delay (scaled), positive = absolute deadline (passed through).
type NtDelayFn = unsafe extern "system" fn(u8, *const i64) -> i32;
// NtQuerySystemInformation(SystemInformationClass, SystemInformation, SystemInformationLength,
// ReturnLength) -> NTSTATUS. A multiplexer; we only touch class SystemTimeOfDayInformation.
type NtQsiFn = unsafe extern "system" fn(i32, *mut c_void, u32, *mut u32) -> i32;
// WaitForSingleObject(HANDLE, DWORD dwMilliseconds) -> DWORD. Object wait (ADR-7 class B):
// counted but never scaled, so the signature is only used to forward the call untouched.
type WfsoFn = unsafe extern "system" fn(HANDLE, u32) -> u32;
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
static O_NTQSI: OnceLock<NtQsiFn> = OnceLock::new();
static O_GTZI: OnceLock<TziFn> = OnceLock::new();
static O_GDTZI: OnceLock<DtziFn> = OnceLock::new();
static O_STSL: OnceLock<StslFn> = OnceLock::new();
static O_STSLEX: OnceLock<StslexFn> = OnceLock::new();
static O_FTLFT: OnceLock<FtConvFn> = OnceLock::new();
static O_LFTFT: OnceLock<FtConvFn> = OnceLock::new();
static O_TLTST: OnceLock<StslFn> = OnceLock::new();
static O_TLTSTEX: OnceLock<StslexFn> = OnceLock::new();
static O_TICK: OnceLock<TickFn> = OnceLock::new();
static O_TICK32: OnceLock<Tick32Fn> = OnceLock::new();
static O_QUIT: OnceLock<QuitFn> = OnceLock::new();
static O_SLEEP: OnceLock<SleepFn> = OnceLock::new();
static O_SLEEPEX: OnceLock<SleepExFn> = OnceLock::new();
static O_NTDELAY: OnceLock<NtDelayFn> = OnceLock::new();
static O_WFSO: OnceLock<WfsoFn> = OnceLock::new();

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

/// Wait via the ORIGINAL WaitForSingleObject (trampoline) when it is hooked, so the hook's own
/// internal waits (the core watcher, child injection) are never counted as the target's
/// object-wait usage (ADR-7 class B - the audit must count the app's waits, not our machinery's,
/// rule 4). With WFSO unhooked (scale_duration off) the direct call is not counted either.
///
/// # Safety
/// `h` must be a valid handle to wait on.
unsafe fn wait_raw(h: HANDLE, ms: u32) {
    match O_WFSO.get() {
        Some(o) => {
            o(h, ms);
        }
        None => {
            WaitForSingleObject(h, ms);
        }
    }
}

// --- Self-detach: revert to real time when the core vanishes --------------------
// The core writes its PID into the control block; we open a SYNCHRONIZE handle to it.
// On the first time call we spawn a watcher that blocks on that handle. When the core
// dies (clean end, crash, or kill -9) the OS signals it, we flip DETACHED, and every
// detour falls through to the original - the target's clock returns to real time.

unsafe extern "system" fn watcher_proc(_p: *mut c_void) -> u32 {
    if let Some(&h) = CORE_HANDLE.get() {
        wait_raw(HANDLE(h as *mut c_void), INFINITE);
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

fn ft_to_i64(ft: FILETIME) -> i64 {
    (((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64) as i64
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

// NtQuerySystemInformation is a syscall stub, so class SystemTimeOfDayInformation returns the REAL
// system time straight from the kernel, bypassing every user-mode wall detour above. Unlike those,
// this detour is WRAP-AND-PATCH: it calls its OWN original (which fills the whole struct), then
// overwrites only the CurrentTime field with the fake instant - the other fields (BootTime,
// TimeZoneBias, ...) stay real. It calls no other channel's original, so the no-cross-channel
// re-entrancy invariant holds, and the original is a MinHook trampoline, so it does not re-enter here.
//
// SystemTimeOfDayInformation = 3: winternl.h (Windows SDK) - source. CurrentTime at byte offset 8
// (LARGE_INTEGER, 100 ns UTC since 1601, the same clock as NtQuerySystemTime): the SDK and MS Learn
// both declare SYSTEM_TIMEOFDAY_INFORMATION opaque (BYTE Reserved1[48]), so the offset is the
// long-stable community/RE layout (BootTime@0, CurrentTime@8, ...), corroborated by the 48-byte size
// and verified empirically by the p1 baseline (CurrentTime == NtQuerySystemTime). Assessment, not
// source (zasady/03 section 4).
const SYSTEM_TIME_OF_DAY_INFORMATION: i32 = 3;
const TOD_CURRENTTIME_OFFSET: usize = 8;

unsafe extern "system" fn h_ntqsi(class: i32, info: *mut c_void, len: u32, retlen: *mut u32) -> i32 {
    let o = match O_NTQSI.get() {
        Some(o) => o,
        None => return 0, // unreachable: O_NTQSI is set before enable_all_hooks
    };
    let status = o(class, info, len, retlen); // always call the original: it fills the whole struct
    if class == SYSTEM_TIME_OF_DAY_INFORMATION {
        // Count only time-of-day queries - NtQuerySystemInformation is a multiplexer, so bumping on
        // every class would inflate the audit's notion of how often the app reads time (rule 4).
        bump(IDX_NTQSI);
        // NT_SUCCESS(status) == status >= 0; the length guard keeps the [8, 16) write in bounds when a
        // caller passes a truncated buffer (honest partial: leave it, never write past its end).
        if status >= 0 && !info.is_null() && len as usize >= TOD_CURRENTTIME_OFFSET + 8 {
            if let Some(fake) = compute_fake() {
                // None when the core detached - then leave the real CurrentTime the original wrote.
                let p = (info as *mut u8).add(TOD_CURRENTTIME_OFFSET) as *mut i64;
                core::ptr::write_unaligned(p, fake);
            }
        }
    }
    status
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

// SystemTimeToTzSpecificLocalTime converts a caller-supplied UTC to local. A NULL zone
// means "the currently active zone" (MS Learn, timezoneapi.h). We substitute the session
// zone (result agrees with GetLocalTime) and pass an explicitly named zone through. For the
// substituted case we compute session-local directly (utc - tz_bias, flat, no DST - the same
// math as GetLocalTime) rather than re-enter the OS. A constructed Ex zone struct made the
// original STtSLTEx fail (measured), so direct math sidesteps the OS zone machinery entirely.

/// Convert a caller-supplied UTC SYSTEMTIME to session-local (utc - tz_bias, flat, no DST)
/// and write it to `local`. Returns false if the UTC could not be converted, so the caller
/// can defer to the original. Shared by the STtSLT and STtSLTEx detours.
///
/// # Safety
/// `utc` must point to a valid SYSTEMTIME and `local` to a valid writable SYSTEMTIME.
unsafe fn write_session_local(utc: *const SYSTEMTIME, local: *mut SYSTEMTIME) -> bool {
    let mut ft = FILETIME::default();
    if SystemTimeToFileTime(utc, &mut ft).is_err() {
        return false;
    }
    let bias_100ns = cur_tz_bias() as i64 * 60 * 10_000_000;
    write_systemtime(local, ft_to_i64(ft) - bias_100ns);
    true
}

/// Convert a caller-supplied local SYSTEMTIME to session-UTC (local + tz_bias, the inverse
/// of write_session_local) and write it to `utc`. Returns false if the local time could not
/// be converted. Shared by the TzSpecificLocalTimeToSystemTime detours.
///
/// # Safety
/// `local` must point to a valid SYSTEMTIME and `utc` to a valid writable SYSTEMTIME.
unsafe fn write_session_utc(local: *const SYSTEMTIME, utc: *mut SYSTEMTIME) -> bool {
    let mut ft = FILETIME::default();
    if SystemTimeToFileTime(local, &mut ft).is_err() {
        return false;
    }
    let bias_100ns = cur_tz_bias() as i64 * 60 * 10_000_000;
    write_systemtime(utc, ft_to_i64(ft) + bias_100ns);
    true
}

/// Shift a FILETIME by the session bias: `add` for local->UTC (utc = local + bias), clear for
/// UTC->local (local = utc - bias). The FILETIME conversions carry no zone argument, so they
/// always mean the active zone, which we replace with the flat session zone.
///
/// # Safety
/// `src` and `dst` must be valid, non-null FILETIME pointers.
unsafe fn shift_filetime(src: *const FILETIME, dst: *mut FILETIME, add: bool) -> i32 {
    let bias_100ns = cur_tz_bias() as i64 * 60 * 10_000_000;
    let ticks = ft_to_i64(*src);
    *dst = i64_to_ft(if add { ticks + bias_100ns } else { ticks - bias_100ns });
    1
}

// FileTimeToLocalFileTime (UTC -> local) and LocalFileTimeToFileTime (local -> UTC) take no
// zone argument, so they always mean the active zone - always substitute the session zone.
unsafe extern "system" fn h_ftlft(utc: *const FILETIME, local: *mut FILETIME) -> i32 {
    bump(IDX_FTLFT);
    if detached() || utc.is_null() || local.is_null() {
        return O_FTLFT.get().map(|o| o(utc, local)).unwrap_or(0);
    }
    shift_filetime(utc, local, false)
}

unsafe extern "system" fn h_lftft(local: *const FILETIME, utc: *mut FILETIME) -> i32 {
    bump(IDX_LFTFT);
    if detached() || local.is_null() || utc.is_null() {
        return O_LFTFT.get().map(|o| o(local, utc)).unwrap_or(0);
    }
    shift_filetime(local, utc, true)
}

// TzSpecificLocalTimeToSystemTime (+Ex): reverse of the STtSLT detours (local -> UTC). A NULL
// zone means the active zone -> session zone (utc = local + tz_bias). A named zone passes through.
unsafe extern "system" fn h_tltst(
    tzi: *const TIME_ZONE_INFORMATION,
    local: *const SYSTEMTIME,
    utc: *mut SYSTEMTIME,
) -> i32 {
    bump(IDX_TLTST);
    let o = match O_TLTST.get() {
        Some(o) => o,
        None => return 0,
    };
    if detached() || !tzi.is_null() || local.is_null() || utc.is_null() {
        return o(tzi, local, utc);
    }
    if write_session_utc(local, utc) {
        1
    } else {
        o(tzi, local, utc)
    }
}

unsafe extern "system" fn h_tltstex(
    tzi: *const DYNAMIC_TIME_ZONE_INFORMATION,
    local: *const SYSTEMTIME,
    utc: *mut SYSTEMTIME,
) -> i32 {
    bump(IDX_TLTSTEX);
    let o = match O_TLTSTEX.get() {
        Some(o) => o,
        None => return 0,
    };
    if detached() || !tzi.is_null() || local.is_null() || utc.is_null() {
        return o(tzi, local, utc);
    }
    if write_session_utc(local, utc) {
        1
    } else {
        o(tzi, local, utc)
    }
}

unsafe extern "system" fn h_stsl(
    tzi: *const TIME_ZONE_INFORMATION,
    utc: *const SYSTEMTIME,
    local: *mut SYSTEMTIME,
) -> i32 {
    bump(IDX_STSL);
    let o = match O_STSL.get() {
        Some(o) => o,
        None => return 0,
    };
    if detached() || !tzi.is_null() || utc.is_null() || local.is_null() {
        return o(tzi, utc, local);
    }
    if write_session_local(utc, local) {
        1
    } else {
        o(tzi, utc, local)
    }
}

unsafe extern "system" fn h_stslex(
    tzi: *const DYNAMIC_TIME_ZONE_INFORMATION,
    utc: *const SYSTEMTIME,
    local: *mut SYSTEMTIME,
) -> i32 {
    bump(IDX_STSLEX);
    let o = match O_STSLEX.get() {
        Some(o) => o,
        None => return 0,
    };
    if detached() || !tzi.is_null() || utc.is_null() || local.is_null() {
        return o(tzi, utc, local);
    }
    if write_session_local(utc, local) {
        1
    } else {
        o(tzi, utc, local)
    }
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

// Wait axis (ADR-7 class A): divide a blocking wait's timeout by the duration multiplier so
// a thread that blocks on time wakes in lockstep with the scaled clock it reads. INFINITE and
// 0 pass through (scale_wait). Unlike the absolute wall detours, a wait detour is RELATIVE - it
// calls the original with a modified argument, and the original may re-enter another hooked wait
// export on the same thread. A thread-local guard makes each app-level wait scale exactly once
// and be counted against the export the app actually called, never an internal cascade. On
// Win11 26200 Sleep does not reach the exported SleepEx (internal path), but both Sleep and
// SleepEx bottom out on NtDelayExecution - so with that funnel hooked the guard is load-bearing:
// Sleep scales at h_sleep, then re-enters h_ntdelay, which the flag makes pass through unscaled.

thread_local! {
    static SCALING_WAIT: Cell<bool> = const { Cell::new(false) };
}

/// Clears the re-entrancy flag when the top-level wait detour returns.
struct WaitGuard;
impl Drop for WaitGuard {
    fn drop(&mut self) {
        SCALING_WAIT.set(false);
    }
}

/// Decide whether this wait call is the top-level app call we should scale. Returns the
/// duration multiplier and a guard (held across the original call, so an inner cascade sees the
/// flag set and passes through) when it is; None when this is an internal cascade (pass the
/// original through, uncounted) or the core has detached (fall through to real time). Bumps
/// coverage only for a top-level app call, so the audit counts what the app called, not what
/// Windows re-entered.
fn try_enter_wait(idx: usize) -> Option<(i64, WaitGuard)> {
    if SCALING_WAIT.get() {
        return None; // internal cascade: pass through, do not bump
    }
    bump(idx);
    if detached() {
        return None; // core gone: real time
    }
    SCALING_WAIT.set(true);
    Some((dur_multiplier(), WaitGuard))
}

unsafe extern "system" fn h_sleep(ms: u32) {
    let o = match O_SLEEP.get() {
        Some(o) => o,
        None => return,
    };
    match try_enter_wait(IDX_SLEEP) {
        Some((m, _guard)) => o(scale_wait(ms, m)),
        None => o(ms),
    }
}

unsafe extern "system" fn h_sleepex(ms: u32, alertable: i32) -> u32 {
    let o = match O_SLEEPEX.get() {
        Some(o) => o,
        None => return 0,
    };
    match try_enter_wait(IDX_SLEEPEX) {
        Some((m, _guard)) => o(scale_wait(ms, m), alertable),
        None => o(ms, alertable),
    }
}

// NtDelayExecution is the shared funnel Sleep and SleepEx bottom out on, so hooking it makes the
// re-entrancy guard load-bearing (a scaled Sleep re-enters here and must pass through). It also
// catches callers that reach ntdll directly. The interval is signed 100 ns: only a negative
// (relative) delay is scaled; a positive (absolute deadline) or null passes through.
unsafe extern "system" fn h_ntdelay(alertable: u8, interval: *const i64) -> i32 {
    let o = match O_NTDELAY.get() {
        Some(o) => o,
        None => return 0,
    };
    match try_enter_wait(IDX_NTDELAY) {
        Some((m, _guard)) => {
            if interval.is_null() {
                o(alertable, interval)
            } else {
                let scaled = scale_delay_interval(*interval, m);
                o(alertable, &scaled as *const i64)
            }
        }
        None => o(alertable, interval),
    }
}

// Wait axis class B (ADR-7, option b): object waits are COUNTED but deliberately NOT scaled.
// Shortening a wait on a real I/O / hardware / IPC handle would fake a timeout, so the timeout
// passes through untouched and the audit warns instead. Just bump and forward - not scaling means
// no re-entrancy guard is needed (we never divide), and one hooked export cannot double-count a
// single app call. Detached state is irrelevant here: we never modify the wait either way.
unsafe extern "system" fn h_wfso(handle: HANDLE, ms: u32) -> u32 {
    bump(IDX_WFSO);
    match O_WFSO.get() {
        Some(o) => o(handle, ms),
        None => 0, // unreachable: O_WFSO is set before enable_all_hooks
    }
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
        wait_raw(hthread, INFINITE);
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
    make_hook(cov, k32, ntdll, IDX_NTQSI, h_ntqsi as *const () as *mut c_void, &O_NTQSI);
    make_hook(cov, k32, ntdll, IDX_GTZI, h_gtzi as *const () as *mut c_void, &O_GTZI);
    make_hook(cov, k32, ntdll, IDX_GDTZI, h_gdtzi as *const () as *mut c_void, &O_GDTZI);
    make_hook(cov, k32, ntdll, IDX_STSL, h_stsl as *const () as *mut c_void, &O_STSL);
    make_hook(cov, k32, ntdll, IDX_STSLEX, h_stslex as *const () as *mut c_void, &O_STSLEX);
    make_hook(cov, k32, ntdll, IDX_FTLFT, h_ftlft as *const () as *mut c_void, &O_FTLFT);
    make_hook(cov, k32, ntdll, IDX_LFTFT, h_lftft as *const () as *mut c_void, &O_LFTFT);
    make_hook(cov, k32, ntdll, IDX_TLTST, h_tltst as *const () as *mut c_void, &O_TLTST);
    make_hook(cov, k32, ntdll, IDX_TLTSTEX, h_tltstex as *const () as *mut c_void, &O_TLTSTEX);

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
        make_hook(cov, k32, ntdll, IDX_SLEEP, h_sleep as *const () as *mut c_void, &O_SLEEP);
        make_hook(cov, k32, ntdll, IDX_SLEEPEX, h_sleepex as *const () as *mut c_void, &O_SLEEPEX);
        make_hook(cov, k32, ntdll, IDX_NTDELAY, h_ntdelay as *const () as *mut c_void, &O_NTDELAY);
        make_hook(cov, k32, ntdll, IDX_WFSO, h_wfso as *const () as *mut c_void, &O_WFSO);
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
