//! Chrono Mock injected hook (Stage 4): substitute the full set of wall-clock
//! channels and the session zone, consistently.
//!
//! On `DLL_PROCESS_ATTACH` this opens the session control memory (`Local\ChronoCtl`),
//! installs a MinHook detour on every time export listed in `chrono_ctl::CHANNELS`,
//! and records each covered channel in the control block. The wall detours return
//! `a_fake + (quit_now - a_real) * multiplier` (multiplier 1 here), anchored on
//! `QueryUnbiasedInterruptTime` (ADR-5). The UTC channels return that instant
//! directly, `GetLocalTime` shifts it back into the session zone by `tz_bias`, and
//! the zone detours report the session zone (`Bias = tz_bias`, no DST) so a target
//! that asks its offset agrees with `GetLocalTime`.
//!
//! ABSOLUTE, not delta: a detour computes the fake instant from the anchor and never
//! calls another channel's original. So there is no cross-channel re-entrancy and no
//! double-shift, and hence no thread-local re-entrancy guard - the spike's E2 guard
//! was an artifact of an earlier delta design (`original + delta`) and does not apply.
//!
//! Out of scope here (later slices): the duration axis - `GetTickCount64` and `QUIT`
//! scaling, with `QueryPerformanceCounter` deliberately excluded per ADR-2 - and
//! child-process inheritance (`CreateProcessW`).

#![allow(non_snake_case)]

use std::ffi::{c_void, CString};
use std::sync::OnceLock;

use chrono_ctl::{
    bump_calls, mark_channel_installed, read_anchor, read_tz_bias, ChannelModule, Ctl, CHANNELS,
    IDX_GDTZI, IDX_GLT, IDX_GST, IDX_GSTAFT, IDX_GSTPAFT, IDX_GTZI, IDX_NTQST,
};
use minhook::MinHook;
use windows::core::{s, w, PCSTR};
use windows::Win32::Foundation::{FILETIME, HMODULE, SYSTEMTIME};
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringA;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{MapViewOfFile, OpenFileMappingW, FILE_MAP_ALL_ACCESS};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::System::Time::{
    FileTimeToSystemTime, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_INFORMATION,
};
use windows::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime;

type FtFn = unsafe extern "system" fn(*mut FILETIME);
type StFn = unsafe extern "system" fn(*mut SYSTEMTIME);
type NtqstFn = unsafe extern "system" fn(*mut i64) -> i32;
type TziFn = unsafe extern "system" fn(*mut TIME_ZONE_INFORMATION) -> u32;
type DtziFn = unsafe extern "system" fn(*mut DYNAMIC_TIME_ZONE_INFORMATION) -> u32;

static CTL_PTR: OnceLock<usize> = OnceLock::new();
static TZ_BIAS: OnceLock<i32> = OnceLock::new();

static O_GSTAFT: OnceLock<FtFn> = OnceLock::new();
static O_GSTPAFT: OnceLock<FtFn> = OnceLock::new();
static O_GST: OnceLock<StFn> = OnceLock::new();
static O_GLT: OnceLock<StFn> = OnceLock::new();
static O_NTQST: OnceLock<NtqstFn> = OnceLock::new();
static O_GTZI: OnceLock<TziFn> = OnceLock::new();
static O_GDTZI: OnceLock<DtziFn> = OnceLock::new();

fn ctl_ptr() -> Option<*mut Ctl> {
    CTL_PTR.get().map(|a| *a as *mut Ctl)
}

/// Real (unbiased) monotonic anchor base - ADR-5. QUIT is not hooked in this slice,
/// so calling it directly yields the real value.
fn real_quit() -> i64 {
    let mut t: u64 = 0;
    unsafe {
        let _ = QueryUnbiasedInterruptTime(&mut t);
    }
    t as i64
}

fn compute_fake() -> Option<i64> {
    let p = ctl_ptr()? as *const Ctl;
    let (a_fake, a_real, m) = unsafe { read_anchor(p) };
    let dq = real_quit().wrapping_sub(a_real);
    Some(a_fake.wrapping_add(dq.wrapping_mul(m)))
}

fn cur_tz_bias() -> i32 {
    *TZ_BIAS.get().unwrap_or(&0)
}

fn i64_to_ft(t: i64) -> FILETIME {
    FILETIME {
        dwLowDateTime: (t as u64 & 0xFFFF_FFFF) as u32,
        dwHighDateTime: ((t as u64) >> 32) as u32,
    }
}

fn bump(idx: usize) {
    if let Some(p) = ctl_ptr() {
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
    if !lp.is_null() {
        let mut d = DYNAMIC_TIME_ZONE_INFORMATION { Bias: cur_tz_bias(), ..Default::default() };
        set_wide(&mut d.StandardName, SESSION_ZONE_NAME);
        set_wide(&mut d.TimeZoneKeyName, SESSION_ZONE_NAME);
        *lp = d;
        return 0; // TIME_ZONE_ID_UNKNOWN - the session zone has no DST
    }
    O_GDTZI.get().map(|o| o(lp)).unwrap_or(TIME_ZONE_ID_INVALID)
}

/// Diagnostics only (stderr-equivalent for an injected DLL); never affects coverage.
fn log(msg: &str) {
    if let Ok(c) = CString::new(msg) {
        unsafe { OutputDebugStringA(PCSTR(c.as_ptr() as *const u8)) }
    }
}

/// Resolve, create, and record one channel's detour. Best-effort: a missing export
/// or a failed hook logs and leaves the bit unset (honest partial), never aborts the
/// rest. The export name and module come from `CHANNELS[idx]` - single source.
///
/// # Safety
/// `ctl` must point to a live `Ctl`; `detour` must be the correct detour for `slot`.
unsafe fn make_hook<T: Copy>(
    ctl: *mut Ctl,
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
            mark_channel_installed(ctl, ch.bit);
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

    let k32 = GetModuleHandleA(s!("kernel32.dll")).map_err(|e| format!("{e:?}"))?;
    let ntdll = GetModuleHandleA(s!("ntdll.dll")).map_err(|e| format!("{e:?}"))?;

    make_hook(ctl, k32, ntdll, IDX_GSTAFT, h_gstaft as *const () as *mut c_void, &O_GSTAFT);
    make_hook(ctl, k32, ntdll, IDX_GSTPAFT, h_gstpaft as *const () as *mut c_void, &O_GSTPAFT);
    make_hook(ctl, k32, ntdll, IDX_GST, h_gst as *const () as *mut c_void, &O_GST);
    make_hook(ctl, k32, ntdll, IDX_GLT, h_glt as *const () as *mut c_void, &O_GLT);
    make_hook(ctl, k32, ntdll, IDX_NTQST, h_ntqst as *const () as *mut c_void, &O_NTQST);
    make_hook(ctl, k32, ntdll, IDX_GTZI, h_gtzi as *const () as *mut c_void, &O_GTZI);
    make_hook(ctl, k32, ntdll, IDX_GDTZI, h_gdtzi as *const () as *mut c_void, &O_GDTZI);

    MinHook::enable_all_hooks().map_err(|e| format!("enable_all_hooks: {e:?}"))?;
    Ok(())
}

#[no_mangle]
pub extern "system" fn DllMain(_hinst: HMODULE, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        unsafe {
            if let Err(e) = install() {
                log(&format!("[chrono_hook] install failed: {e}"));
            }
        }
    }
    1
}
