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
//! (ADR-2) only as far as QPC goes, and QPC scales under its own opt-in. `timeGetTime` LEFT
//! that exception on 2026-09-07: it returns the same milliseconds-since-boot as `GetTickCount`,
//! so it rides the duration axis with it. Once QUIT is hooked, the anchor math reads it through
//! the trampoline so the scaled output never feeds back.
//!
//! Under the SAME scale_duration flag the wait axis is scaled too (ADR-7): a wait's
//! timeout is divided by the multiplier (real wait = requested / M), so a thread that
//! blocks on time wakes in lockstep with the scaled clock it reads. `INFINITE` and 0 pass
//! through untouched. `Sleep`, `SleepEx`, and the shared funnel `NtDelayExecution` are covered
//! (ADR-7 class A) - a thread-local guard scales an internal cascade (Sleep or SleepEx bottoming
//! out on NtDelayExecution) exactly once. The kernel32 object waits (`WaitForSingleObject(Ex)`,
//! `WaitForMultipleObjects(Ex)`, `SignalObjectAndWait`, ADR-7 class B) are COUNTED but deliberately
//! NOT scaled - shortening a wait on real I/O would fake a timeout, so they ride their own `observed`
//! bucket with an audit warning, and a separate thread-local guard counts each app-level wait once.
//! The user32 message waits (`MsgWaitForMultipleObjects(Ex)`) join them on the same guard when the
//! target has user32 loaded (resolved lazily, honest partial if absent). The settable waitable timers
//! (`SetWaitableTimer(Ex)`, ADR-7 class C) ARE scaled - a relative due-time and a periodic lPeriod
//! divide by M, and an absolute due-time is converted to a scaled relative interval - on their own
//! thread-local guard. `SetTimer` (user32, ADR-7 class C) scales its uElapse interval so WM_TIMER
//! keeps step with the fake clock (no guard - it does not cascade onto another hooked export).
//! `timeSetEvent` (winmm, ADR-7 class C) is OBSERVED, not scaled - counted with its own audit warning
//! but left real, because scaling it would shift audio/MIDI timing (the winmm cost ADR-2 avoids). The
//! thread-pool timers `SetThreadpoolTimer` / `SetThreadpoolTimerEx` (kernel32, ADR-7 class C) scale
//! like `SetWaitableTimer` (FILETIME due + msPeriod + msWindowLength by M) - their detour is stateless,
//! which keeps it correct under the thread pool's own worker threads and callback re-arms.
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
//! (ADR-3). A DIRECT `NtCreateUserProcess` (bypassing CreateProcess*) is OBSERVED, not injected:
//! counted and warned (its child may be uncovered), never self-injected - that would mean
//! manipulating undocumented native structures for near-zero real value. A thread-local guard keeps
//! the CreateProcess* funnel to NtCreateUserProcess from counting as a direct spawn.

#![allow(non_snake_case)]

use std::cell::Cell;
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

use chrono_ctl::{
    bump_calls, bump_uninjected_children, cov_at_mut, dur_qpc_at, dur_quit_at, dur_tick_at,
    header_is_ours,
    publish_pid, read_anchor, read_core_pid, read_dur, read_qpc, read_scale_dur, read_scale_qpc,
    read_installed, read_late_installed, read_tz_bias, reserve_cov_slot, scale_delay_interval, scale_timer_due, scale_timer_elapse, scale_timer_period,
    scale_timer_period_ms, scale_wait, set_channels_installed, set_late_installed, ChannelModule, Cov,
    Ctl, CHANNELS, IDX_GDTZI, IDX_GLT, IDX_GST, IDX_GSTAFT, IDX_GSTPAFT, IDX_GTC, IDX_GTC64,
    IDX_GTZI, IDX_NTDELAY, IDX_NTQSI, IDX_NTQST, IDX_QUIT, IDX_SLEEP, IDX_SLEEPEX, IDX_STSL,
    IDX_STSLEX, IDX_FTLFT, IDX_LFTFT, IDX_TLTST, IDX_TLTSTEX, IDX_WFSO, IDX_WFSOEX, IDX_WFMO,
    IDX_WFMOEX, IDX_SOAW, IDX_MWFMO, IDX_MWFMOEX, IDX_SWT, IDX_SWTEX, IDX_SETTIMER, IDX_TIMESETEVENT,
    IDX_TPTIMER, IDX_TPTIMEREX, IDX_NTCUP, IDX_CONNECT, IDX_QPC, IDX_TIMEGETTIME, IDX_SCVSRW,
    IDX_SCVCS, IDX_WOA, IDX_WSAWFME,
};
use minhook::MinHook;
use windows::core::{s, PCSTR, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, SetLastError, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, HMODULE, SYSTEMTIME,
    WAIT_FAILED,
};
use windows::Win32::System::Diagnostics::Debug::{OutputDebugStringA, WriteProcessMemory};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleA, GetModuleHandleExA, GetProcAddress,
    GET_MODULE_HANDLE_EX_FLAG_PIN,
};
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualAllocEx, VirtualFreeEx,
    FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
    PAGE_READWRITE,
};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::System::Threading::{
    CreateRemoteThread, CreateThread, GetCurrentProcessId, GetExitCodeThread, OpenProcess,
    ResumeThread, WaitForSingleObject, CREATE_SUSPENDED, INFINITE, LPTHREAD_START_ROUTINE,
    PROCESS_INFORMATION, PROCESS_SYNCHRONIZE, THREAD_CREATION_FLAGS,
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
// ReturnLength) -> NTSTATUS. A multiplexer - we only touch class SystemTimeOfDayInformation.
type NtQsiFn = unsafe extern "system" fn(i32, *mut c_void, u32, *mut u32) -> i32;
// WaitForSingleObject(HANDLE, DWORD dwMilliseconds) -> DWORD. Object wait (ADR-7 class B):
// counted but never scaled, so the signature is only used to forward the call untouched. The
// rest of the object-wait family (below) is the same story with different argument shapes.
type WfsoFn = unsafe extern "system" fn(HANDLE, u32) -> u32;
type WfsoexFn = unsafe extern "system" fn(HANDLE, u32, i32) -> u32;
type WfmoFn = unsafe extern "system" fn(u32, *const HANDLE, i32, u32) -> u32;
type WfmoexFn = unsafe extern "system" fn(u32, *const HANDLE, i32, u32, i32) -> u32;
type SoawFn = unsafe extern "system" fn(HANDLE, HANDLE, u32, i32) -> u32;
// MsgWaitForMultipleObjects(nCount, pHandles, fWaitAll, dwMilliseconds, dwWakeMask) -> DWORD.
// MsgWaitForMultipleObjectsEx(nCount, pHandles, dwMilliseconds, dwWakeMask, dwFlags) -> DWORD - no
// fWaitAll, args reordered (MS Learn, winuser.h). Both user32, counted but never scaled.
type MwfmoFn = unsafe extern "system" fn(u32, *const HANDLE, i32, u32, u32) -> u32;
type MwfmoexFn = unsafe extern "system" fn(u32, *const HANDLE, u32, u32, u32) -> u32;
// The four waits that were neither scaled nor counted until 2026-09-08 - a target that blocks on any
// of them was simply invisible to the audit, which is the silent non-coverage the product rules out.
// Signatures from MS Learn (synchapi.h, winsock2.h), argument shapes forwarded untouched:
//   SleepConditionVariableSRW(PCONDITION_VARIABLE, PSRWLOCK, DWORD dwMilliseconds, ULONG Flags) -> BOOL
//   SleepConditionVariableCS(PCONDITION_VARIABLE, PCRITICAL_SECTION, DWORD dwMilliseconds) -> BOOL
//   WaitOnAddress(volatile VOID*, PVOID, SIZE_T, DWORD dwMilliseconds) -> BOOL
//   WSAWaitForMultipleEvents(DWORD, const WSAEVENT*, BOOL, DWORD dwTimeout, BOOL fAlertable) -> DWORD
// The three pointer parameters are opaque here and never dereferenced. `SIZE_T` is pointer-wide, so
// it is `usize` and differs between the two targets - which is why both are built.
type ScvsrwFn = unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32) -> i32;
type ScvcsFn = unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> i32;
type WoaFn = unsafe extern "system" fn(*const c_void, *const c_void, usize, u32) -> i32;
type WsawfmeFn = unsafe extern "system" fn(u32, *const HANDLE, i32, u32, i32) -> u32;
// SetWaitableTimer(hTimer, *lpDueTime, lPeriod, pfnCompletionRoutine, lpArg, fResume) -> BOOL. The
// due time is a 100 ns LARGE_INTEGER (positive = absolute FILETIME instant, negative = relative) -
// lPeriod is milliseconds (0 = one-shot). SetWaitableTimerEx drops fResume and adds a REASON_CONTEXT
// and a ULONG TolerableDelay (MS Learn, synchapi.h). We forward the callback/arg/context opaquely
// (never read them), so c_void pointers are enough. ADR-7 class C: due-time + period scaled.
type SwtFn = unsafe extern "system" fn(HANDLE, *const i64, i32, *const c_void, *const c_void, i32) -> i32;
type SwtexFn =
    unsafe extern "system" fn(HANDLE, *const i64, i32, *const c_void, *const c_void, *const c_void, u32) -> i32;
// SetTimer(hWnd, nIDEvent, uElapse, lpTimerFunc) -> UINT_PTR (user32). uElapse is a relative interval
// in ms (no absolute form, no INFINITE) - the HWND, timer id, and TIMERPROC are forwarded opaquely.
// ADR-7 class C: uElapse scaled by M so WM_TIMER arrives in step with the fake clock.
type SetTimerFn = unsafe extern "system" fn(*mut c_void, usize, u32, *const c_void) -> usize;
// timeSetEvent(uDelay, uResolution, lpTimeProc, dwUser, fuEvent) -> MMRESULT (winmm). ADR-7 class C,
// OBSERVED not scaled: uDelay is a relative delay in ms, but scaling it would shift audio/MIDI timing
// (the winmm cost ADR-2 avoids, like timeGetTime), so the detour only counts and forwards untouched.
// All args are opaque to us (lpTimeProc is a callback or an event handle depending on fuEvent).
type TimeSetEventFn = unsafe extern "system" fn(u32, u32, *const c_void, usize, u32) -> u32;
// timeGetTime() -> DWORD (winmm): milliseconds since Windows started, the coarse clock a media stack or
// a game engine reads after raising the timer resolution with timeBeginPeriod. Scaled on the duration
// axis since 2026-09-07 - it is the same quantity as GetTickCount, and leaving it real made an
// application's own duration axis disagree with itself by the multiplier. Takes no argument, so the
// detour is a pure substitution with nothing to translate.
type TimeGetTimeFn = unsafe extern "system" fn() -> u32;
// connect(SOCKET s, const sockaddr *name, int namelen) -> int (ws2_32). SourceObserved: we only COUNT a
// network connection (a suspected server time source) and forward every arg untouched. SOCKET is a
// UINT_PTR (usize), the sockaddr* is opaque (never dereferenced), namelen is int (i32).
type ConnectFn = unsafe extern "system" fn(usize, *const c_void, i32) -> i32;
// SetThreadpoolTimer(pti, pftDueTime, msPeriod, msWindowLength) -> VOID, and SetThreadpoolTimerEx ->
// BOOL (kernel32, threadpoolapiset). pftDueTime is a FILETIME* (same 64 bits as SetWaitableTimer's
// LARGE_INTEGER*): positive/zero = absolute, negative = relative, NULL = cancel. ADR-7 class C: due +
// msPeriod + msWindowLength scaled by M, exactly like SetWaitableTimer. pti is opaque (never touched).
type SetTpTimerFn = unsafe extern "system" fn(*mut c_void, *const FILETIME, u32, u32);
type SetTpTimerExFn = unsafe extern "system" fn(*mut c_void, *const FILETIME, u32, u32) -> i32;
// NtCreateUserProcess (ntdll, ADR-3): the funnel under CreateProcessInternalW. Undocumented - the
// 11-param signature is the stable RE community layout (phnt), an assessment not a source (zasady/03
// section 4). We only OBSERVE it (count a direct call, forward every arg untouched), so a wrong field
// never matters - only the arg count and ABI do. ACCESS_MASK/ULONG are 32-bit on x86 and x64 - the rest
// are opaque pointers we never dereference.
type NtcupFn = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    u32,
    u32,
    *mut c_void,
    *mut c_void,
    u32,
    u32,
    *mut c_void,
    *mut c_void,
    *mut c_void,
) -> i32;
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
static O_WFSOEX: OnceLock<WfsoexFn> = OnceLock::new();
static O_WFMO: OnceLock<WfmoFn> = OnceLock::new();
static O_WFMOEX: OnceLock<WfmoexFn> = OnceLock::new();
static O_SOAW: OnceLock<SoawFn> = OnceLock::new();
static O_MWFMO: OnceLock<MwfmoFn> = OnceLock::new();
static O_MWFMOEX: OnceLock<MwfmoexFn> = OnceLock::new();
static O_SCVSRW: OnceLock<ScvsrwFn> = OnceLock::new();
static O_SCVCS: OnceLock<ScvcsFn> = OnceLock::new();
static O_WOA: OnceLock<WoaFn> = OnceLock::new();
static O_WSAWFME: OnceLock<WsawfmeFn> = OnceLock::new();
static O_SWT: OnceLock<SwtFn> = OnceLock::new();
static O_SWTEX: OnceLock<SwtexFn> = OnceLock::new();
static O_SETTIMER: OnceLock<SetTimerFn> = OnceLock::new();
static O_TIMESETEVENT: OnceLock<TimeSetEventFn> = OnceLock::new();
static O_TIMEGETTIME: OnceLock<TimeGetTimeFn> = OnceLock::new();
static O_TPTIMER: OnceLock<SetTpTimerFn> = OnceLock::new();
static O_TPTIMEREX: OnceLock<SetTpTimerExFn> = OnceLock::new();
static O_NTCUP: OnceLock<NtcupFn> = OnceLock::new();
static O_CONNECT: OnceLock<ConnectFn> = OnceLock::new();

// Child inheritance (ADR-3): our own module handle (to inject the same DLL into a
// child) and the CreateProcessW trampoline.
static SELF_HMOD: OnceLock<usize> = OnceLock::new();
static O_CPW: OnceLock<CpwFn> = OnceLock::new();
static O_CPA: OnceLock<CpaFn> = OnceLock::new();

// Self-detach: a SYNCHRONIZE handle to the core process, and the flag a watcher flips
// when the core vanishes so every detour reverts to real time.
static CORE_HANDLE: OnceLock<usize> = OnceLock::new();
/// The pid of the core that owned the control block when this process joined its session. Kept so
/// every anchor read can confirm the block is still that session's (R2-S6, `still_ours`).
static CORE_PID: OnceLock<u32> = OnceLock::new();
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
/// Wait via the ORIGINAL WaitForSingleObject (trampoline) when it is hooked, so the hook's own
/// internal waits (the core watcher, child injection) are never counted as the target's
/// object-wait usage (ADR-7 class B - the audit must count the app's waits, not our machinery's,
/// rule 4). With WFSO unhooked (scale_duration off) the direct call is not counted either.
///
/// # Safety
/// `h` must be a valid handle to wait on.
unsafe fn wait_raw(h: HANDLE, ms: u32) -> u32 { unsafe {
    match O_WFSO.get() {
        Some(o) => o(h, ms),
        None => WaitForSingleObject(h, ms).0,
    }
}}

/// `STILL_ACTIVE` (259): the exit code `GetExitCodeThread` reports for a thread that has not finished.
/// Read as "we do not know yet", never as a loaded module - the wait above is bounded, so this really
/// can come back on a child wedged in its loader.
const STILL_ACTIVE_CODE: u32 = 259;

/// How long to wait for a freshly injected child's `LoadLibraryW` thread before giving up (RELEASE-009).
/// A finite bound, mirroring `mech::INJECT_TIMEOUT_MS`, so a child that deadlocks in its loader (loader
/// lock) cannot hang the PARENT's `CreateProcess*` detour forever - the parent returns and the child, if
/// it is wedged, is already broken on its own account.
const CHILD_INJECT_TIMEOUT_MS: u32 = 10_000;

// --- Self-detach: revert to real time when the core vanishes --------------------
// The core writes its PID into the control block - we open a SYNCHRONIZE handle to it.
// On the first time call we spawn a watcher that blocks on that handle. When the core
// dies (clean end, crash, or kill -9) the OS signals it, we flip DETACHED, and every
// detour falls through to the original - the target's clock returns to real time.

/// `WAIT_TIMEOUT` as the raw value the wait returns. Spelled out because the wait goes through the
/// `WaitForSingleObject` TRAMPOLINE (`O_WFSO`), which hands back a bare `u32`, not the typed
/// `WAIT_EVENT` the windows crate would.
const WAIT_TIMEOUT_CODE: u32 = 258;

/// How long the watcher sleeps between two looks, while a target is still starting up.
///
/// The interval is a compromise with two sides that pull opposite ways, and both were measured rather
/// than guessed. Short is better for COVERAGE: everything the target calls before the hook lands runs
/// on the real clock, and on `timeGetTime` that window ends in a JUMP, because the channel rejoins the
/// shared duration base - which under x60 had drifted 138 694 ms from the real one after ~2 s. Long is
/// better for COST: this is a thread inside somebody else's application, and every wake-up is charged
/// to them for the whole session.
///
/// Hence two speeds instead of one number - though the measurement that settled them also said the
/// interval is NOT what dominates, and that is worth writing down rather than implying otherwise.
/// A flat 100 ms lost 7 calls out of 101, identically across three runs. Dropping to 20 ms - five
/// times as often - moved it to 4-10, which is not a win. What actually moved it was giving the
/// WATCHER a head start: the thread is spawned lazily from the first detour, so a target that loads
/// its modules before touching any covered channel pays for the thread's creation as well, and
/// letting it exist first took the loss to a steady 4-5.
///
/// So the floor is roughly 60-100 ms and it is scheduling, not sleeping. The fast interval is still
/// worth its cost for a DIFFERENT case than the one the probe exercises: a module pulled in minutes
/// into a session, long after the watcher exists, where the interval is the only thing between the
/// module and the hook.
const LATE_POLL_FAST_MS: u32 = 20;

/// The interval after the startup burst is over.
const LATE_POLL_SLOW_MS: u32 = 250;

/// How many fast looks before backing off - 100 x 20 ms covers the first two seconds of the target's
/// life. A target that pulls in a module later than that (a plugin loaded on demand) is still picked
/// up, just at the slow interval, where a wake-up costs four a second instead of fifty.
const LATE_FAST_TRIES: u32 = 100;

/// Watch the core AND look for modules that arrive after `DllMain`.
///
/// These two jobs share one loop rather than one thread each, because they are the same wait: the
/// watcher already blocks on the core's handle, and a bounded wait on that handle is both "has the
/// core died" and "time to look again". A second thread in the target would buy nothing and cost a
/// stack.
///
/// The loop degenerates to the original INFINITE block as soon as `late_scan` reports nothing left to
/// settle, which for a target whose modules were all present at `DllMain` is the FIRST call - so the
/// common case pays one extra wait of 100 ms and nothing after that.
unsafe extern "system" fn watcher_proc(_p: *mut c_void) -> u32 { unsafe {
    if let Some(&h) = CORE_HANDLE.get() {
        let handle = HANDLE(h as *mut c_void);
        let mut tries: u32 = 0;
        loop {
            // Look FIRST, wait second. The watcher is spawned from the first detour, which for most
            // targets fires before `main` - so by the time the loop is running the modules a target
            // pulls in early may already be there, and sleeping before the first look would hand
            // back a whole interval for nothing.
            late_scan();
            if !late_pending() {
                wait_raw(handle, INFINITE);
                break;
            }
            let interval =
                if tries < LATE_FAST_TRIES { LATE_POLL_FAST_MS } else { LATE_POLL_SLOW_MS };
            tries = tries.saturating_add(1);
            if wait_raw(handle, interval) != WAIT_TIMEOUT_CODE {
                break; // the core is gone (or the wait failed) - detach, do not install anything
            }
        }
    }
    DETACHED.store(true, Ordering::SeqCst);
    0
}}

// --- Late module arrival --------------------------------------------------------
//
// `make_hook` resolves user32 / winmm / ws2_32 at `DllMain` time and treats a missing module as
// "the target cannot call this". For a runtime that arrives LATER that assumption is false, and it
// was measured false: a video player and a flash runtime both ran with winmm mapped while their
// `timeGetTime` and `timeSetEvent` channels were never installed - and the report did not say so,
// because a channel from an optional module that fails to install is dropped from the report
// entirely rather than listed as uncovered. Under x60 that left `timeGetTime` 138 694 ms adrift
// from `GetTickCount`, two clocks that both mean "milliseconds since boot".
//
// Six channels can land here: `timeGetTime` and `SetTimer` are SCALED (their absence is a hole in
// the acceleration, not just in the audit), `timeSetEvent`, both message waits and `connect` are
// observed.
//
// WHY THE INSTALL RUNS ON THE WATCHER THREAD AND NOT WHERE THE MODULE ARRIVES
// ---------------------------------------------------------------------------
// Both plausible triggers - a detour on the loader, or `LdrRegisterDllNotification` - deliver their
// signal UNDER THE LOADER LOCK, and neither can install from there. Microsoft's own note on the
// notification callback is blunt: "It is unsafe for the notification callback to call functions in
// ANY other module other than itself", which rules out even `GetProcAddress`. Our hygiene guard says
// the same thing from the other side: a detour may not allocate, and resolving a channel formats
// diagnostics. And `MH_EnableHook` freezes every thread in the process, which is the one thing that
// must not happen while another thread sits in the loader.
//
// So the install is asynchronous no matter which trigger is chosen - and once that is settled, a
// signal buys nothing that a bounded wait does not, while `LdrRegisterDllNotification` would add an
// undocumented entry point that ships with "may be changed or removed from Windows without further
// notice". The watcher already waits on the core's handle for the whole session - it now waits with a
// timeout instead of forever, and looks around each time it expires.
//
// The consequence is a race that is NOT an oversight: between the module arriving and the hook
// landing, the target's calls run real. That window is the price of not freezing threads inside the
// loader, and the audit is what makes it honest.

/// Channels whose module was absent at `DllMain` and which the session still wants. Filled ONCE by
/// `install` from what it could not resolve, then cleared bit by bit as each one is settled. Zero
/// means the watcher can go back to blocking forever, which is the steady state of a target whose
/// modules were all present at startup.
static LATE_TODO: AtomicU64 = AtomicU64::new(0);

/// Has `install` published its coverage mask yet.
///
/// This is R1, and it is a real ordering hazard rather than a theoretical one: `install` enables the
/// detours BEFORE it stores `pending`, a detour calls `detached()`, and `detached()` is what spawns
/// this watcher. So the watcher can exist while the mask is still unwritten, and a late bit ORed in
/// during that gap would be erased by the plain volatile store that follows. The watcher simply does
/// not touch the Cov until this flag says the store has happened.
static INSTALL_DONE: AtomicBool = AtomicBool::new(false);

const USER32_LATE: u64 =
    CHANNELS[IDX_MWFMO].bit | CHANNELS[IDX_MWFMOEX].bit | CHANNELS[IDX_SETTIMER].bit;
const WINMM_LATE: u64 = CHANNELS[IDX_TIMESETEVENT].bit | CHANNELS[IDX_TIMEGETTIME].bit;
/// ws2_32's two late channels sit behind DIFFERENT gates, which is why they are named apart. `connect`
/// is watched in every session (the network is a suspected time source regardless of the duration
/// axis), while the socket wait rides the wait family's `scale_duration` opt-in like every other wait.
/// Folding them into one constant would make a session that never asked for the duration axis install
/// a wait channel anyway, the exact thing the `wanted_late` gate exists to prevent.
const WS2_32_CONNECT_LATE: u64 = CHANNELS[IDX_CONNECT].bit;
const WS2_32_WAIT_LATE: u64 = CHANNELS[IDX_WSAWFME].bit;
const WS2_32_LATE: u64 = WS2_32_CONNECT_LATE | WS2_32_WAIT_LATE;

/// Every channel that lives in a module which may show up after `DllMain`.
const LATE_CHANNELS: u64 = USER32_LATE | WINMM_LATE | WS2_32_LATE;

/// Is there still a channel worth looking for.
fn late_pending() -> bool {
    LATE_TODO.load(Ordering::Relaxed) != 0
}

/// Get a handle to an already-loaded module and PIN it for the life of the process.
///
/// Pinning, not a plain `GetModuleHandleA`, and the reason is correctness rather than convenience.
/// A hook is bytes written into the module's own code: if the target later calls `FreeLibrary` and
/// the module unmaps, those bytes go with it while our coverage mask still claims the channel is
/// substituted - the audit would be lying (untouchable rule 4), and a call through a stale trampoline
/// is worse than a lie. `GET_MODULE_HANDLE_EX_FLAG_PIN` is documented as keeping the module loaded
/// "until the process is terminated, no matter how many times FreeLibrary is called".
///
/// This is NOT the force-load that `make_hook` deliberately avoids. We never bring in a module the
/// target did not want - `GetModuleHandleExA` fails for a module that was never loaded, and we simply
/// look again later. We only refuse to let go of one the target already chose to load.
unsafe fn pin_module(name: PCSTR) -> Option<HMODULE> { unsafe {
    let mut h = HMODULE::default();
    match GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_PIN, name, &mut h) {
        Ok(()) if !h.is_invalid() => Some(h),
        _ => None,
    }
}}

/// Resolve, create and record ONE late channel's detour. Mirrors `make_hook`, with two differences
/// that both come from running after startup instead of during it.
///
/// The bit leaves `LATE_TODO` whether or not this succeeds. The module is present by the time we get
/// here, so a missing export is a permanent answer, not a temporary one - retrying it every 100 ms
/// for the rest of the session would burn the target's CPU to re-learn the same no (R6).
///
/// The trampoline is stored in `slot` BEFORE anything enables the detour, exactly as `make_hook`
/// does. Reversing that would let a detour fire with no original to call (R4).
///
/// # Safety
/// `detour` must be correct for `slot`, and `module` must be a live, pinned module handle.
unsafe fn late_one<T: Copy>(
    newly: &mut u64,
    module: HMODULE,
    idx: usize,
    detour: *mut c_void,
    slot: &OnceLock<T>,
) { unsafe {
    let ch = &CHANNELS[idx];
    LATE_TODO.fetch_and(!ch.bit, Ordering::Relaxed);
    if slot.get().is_some() {
        return; // already installed at DllMain time - nothing owed here
    }
    let Ok(cname) = CString::new(ch.name) else {
        log(&format!("[chrono_hook] late: bad channel name: {}", ch.name));
        return;
    };
    let Some(target) = GetProcAddress(module, PCSTR(cname.as_ptr() as *const u8)) else {
        log(&format!("[chrono_hook] late: no export: {}", ch.name));
        return;
    };
    match MinHook::create_hook(target as *const () as *mut c_void, detour) {
        Ok(original) => {
            let _ = slot.set(std::mem::transmute_copy::<*mut c_void, T>(&original));
            *newly |= ch.bit;
        }
        Err(e) => log(&format!("[chrono_hook] late: create_hook {} failed: {e:?}", ch.name)),
    }
}}

/// One look for modules that arrived after `DllMain`, and an install for whatever is now reachable.
///
/// Ordering is the whole safety argument here (R2). Every module handle and every export address is
/// resolved, and every trampoline built, BEFORE a single hook is enabled - because `MH_EnableHook`
/// freezes all other threads, and calling into the loader while threads are frozen is how a hooking
/// library deadlocks a process. `MinHook::enable_all_hooks` is the last step and it takes one
/// freeze for the whole batch, and it skips hooks that are already live, so the ones installed at
/// startup are not touched.
unsafe fn late_scan() { unsafe {
    if !INSTALL_DONE.load(Ordering::Acquire) {
        return; // install has not published its mask yet (R1)
    }
    if DETACHED.load(Ordering::SeqCst) {
        LATE_TODO.store(0, Ordering::Relaxed);
        return; // the session is over - never install into a target that went back to real time
    }
    let todo = LATE_TODO.load(Ordering::Relaxed);
    if todo == 0 {
        return;
    }

    let mut newly: u64 = 0;
    if todo & USER32_LATE != 0
        && let Some(m) = pin_module(s!("user32.dll"))
    {
        late_one(&mut newly, m, IDX_MWFMO, h_mwfmo as *const () as *mut c_void, &O_MWFMO);
        late_one(&mut newly, m, IDX_MWFMOEX, h_mwfmoex as *const () as *mut c_void, &O_MWFMOEX);
        late_one(&mut newly, m, IDX_SETTIMER, h_settimer as *const () as *mut c_void, &O_SETTIMER);
    }
    if todo & WINMM_LATE != 0
        && let Some(m) = pin_module(s!("winmm.dll"))
    {
        late_one(&mut newly, m, IDX_TIMESETEVENT, h_timesetevent as *const () as *mut c_void, &O_TIMESETEVENT);
        late_one(&mut newly, m, IDX_TIMEGETTIME, h_timegettime as *const () as *mut c_void, &O_TIMEGETTIME);
    }
    if todo & WS2_32_LATE != 0
        && let Some(m) = pin_module(s!("ws2_32.dll"))
    {
        // Per bit here, unlike the two blocks above, because these two channels answer to different
        // opt-ins - so "the module arrived" is not on its own a reason to install both.
        if todo & WS2_32_CONNECT_LATE != 0 {
            late_one(&mut newly, m, IDX_CONNECT, h_connect as *const () as *mut c_void, &O_CONNECT);
        }
        if todo & WS2_32_WAIT_LATE != 0 {
            late_one(&mut newly, m, IDX_WSAWFME, h_wsawfme as *const () as *mut c_void, &O_WSAWFME);
        }
    }
    if newly == 0 {
        return;
    }

    if let Err(e) = MinHook::enable_all_hooks() {
        // Nothing may be claimed: the trampolines exist but no new detour is live. The bits stay out
        // of the Cov, so the audit reports these channels as it did before - not as covered.
        log(&format!("[chrono_hook] late: enable_all_hooks: {e:?}"));
        return;
    }
    if let Some(c) = cov_ptr() {
        // The late mask FIRST, the coverage mask second. The mechanism reads the two as an
        // intersection, so this order cannot produce a warning about a channel the report does not
        // list - and the other order could not either. Stated rather than left to luck.
        set_late_installed(c, read_late_installed(c) | newly);
        // OR, never a plain store: `install` owns the startup bits and this thread owns the late
        // ones. Read-modify-write is safe here because this is the only writer after INSTALL_DONE.
        set_channels_installed(c, read_installed(c) | newly);
    }
    log(&format!("[chrono_hook] late: installed 0x{newly:x}"));
}}

/// Spawn the watcher once, lazily - NOT from DllMain, to stay clear of the loader lock.
fn ensure_watcher() {
    // Relaxed load first, and it is the hot path that pays for it. `detached()` calls this on EVERY
    // detour - every clock read, every wait, every timer arm - and `compare_exchange` emits a LOCKED
    // read-modify-write whether or not it succeeds. So the one-time setup below was charging a bus
    // lock to the hottest path this product has, forever, to re-learn a fact settled once.
    //
    // A relaxed read is enough because the flag is one-way: set once, never cleared, and nothing is
    // published alongside it (the watcher's own result travels through DETACHED). Both ways of racing
    // are harmless - a stale `false` falls through to the CAS, which then fails exactly as it does
    // today, and a `true` means some thread already won and there is nothing left to do.
    if WATCHER_STARTED.load(Ordering::Relaxed) {
        return;
    }
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

/// Whether the control block still belongs to the session this process joined - checked AFTER a
/// value has been read from it, and the read discarded if not (R2-S6).
///
/// The section has a fixed name, so a NEW core reclaims it: it zeroes the whole block and writes its
/// own anchor. The session mutex proves no other CORE is alive, but it says nothing about the
/// previous session's TARGET, which is still running here and has not necessarily noticed its own
/// core died - the watcher above is woken by the OS, but a thread wakeup is not instantaneous, and
/// this process may be suspended or preempted mid-read. In that window the target read a zeroed
/// block (1601, frozen) and then ANOTHER application's anchor: the one path where a target is handed
/// somebody else's time (rule 2).
///
/// Checking after the read is what makes it sound. The reclaiming core writes the pid LAST, so the
/// block only ever carries our pid while its anchor is still ours: seeing our pid after reading the
/// anchor means no reclaim happened in between, and seeing anything else - zero, or a new core's pid
/// - means the value we just read may not be ours, so we drop it and detach for good.
///
/// 🔴 Two measurements, both worth keeping next to the code. The leak is NOT reproducible on today's
/// design: 0 of 6 runs with this guard removed, killing the core hard and starting a new session at
/// year 3000 while the orphan sampled every 20 ms - because a second core REFUSES rather than waits
/// (`session.already_active`), so it cannot already be inside the window when the first one dies, and
/// starting a process takes far longer than the watcher takes to wake. And the guard is free: an
/// interleaved A/B on two hook builds over the QPC path (5 pairs, 3 M calls) came out at -0.06 ns per
/// call, inside the ±5 ns the probe's timer can even resolve. Unreachable today, free, and the only
/// path on which a target could be handed another session's clock - so it stays.
fn still_ours(p: *const Ctl) -> bool {
    let Some(&mine) = CORE_PID.get() else {
        return true; // no owner was ever recorded (pre-session install): behave as before
    };
    if unsafe { read_core_pid(p) } == mine {
        return true;
    }
    // One-way, like the watcher's flag: a block that stopped being ours never becomes ours again.
    DETACHED.store(true, Ordering::SeqCst);
    false
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
    fake_now_and_dur_m().map(|(fake, _)| fake)
}

/// The fake wall clock AND the duration multiplier, from ONE anchor snapshot.
///
/// `compute_fake` and `dur_multiplier` each open their own seqlock transaction, so a caller needing
/// both read the block twice and paid for it twice. The cost is the smaller half. The larger half is
/// that `set_multiplier` can land BETWEEN the two reads, and then an absolute timer due date is
/// scaled by a rate that does not belong to the `fake_now` it was measured against - the two halves
/// of one answer taken from two different clocks. `read_dur` and `read_qpc` exist precisely so a
/// multiplier and its base cannot drift apart on the duration and QPC axes - the class-C timers were
/// the one place left where they still could.
///
/// The multiplier comes back as the DURATION multiplier (never below 1, untouchable rule 3), which is
/// what every caller of this pair wants: a frozen wall clock must not freeze a timer.
fn fake_now_and_dur_m() -> Option<(i64, i64)> {
    if detached() {
        return None; // core gone: wall detours fall through to the real value
    }
    let p = ctl_ptr()? as *const Ctl;
    let (a_fake, a_real, m) = unsafe { read_anchor(p) };
    if !still_ours(p) {
        return None; // the block was reclaimed by another session mid-read (R2-S6): real time
    }
    // The projection itself lives in `chrono-ctl`, and is the SAME call the mechanism makes for its
    // own `state` reporting. It used to be this formula written out twice, in two crates, with a
    // comment asking that the copies be kept in step - a guard in prose is not a guard (rule 12),
    // and a difference between them is the tool lying about its own clock.
    Some((chrono_ctl::fake_wall_at(a_fake, a_real, real_quit(), m), m.max(1)))
}

fn cur_tz_bias() -> i32 {
    *TZ_BIAS.get().unwrap_or(&0)
}

/// Current multiplier from the anchor (the wall-clock speed factor).
fn cur_m() -> i64 {
    match ctl_ptr() {
        // Ownership checked after the read, like compute_fake: a reclaimed block must not lend this
        // target another session's rate either (R2-S6). Falling back to 1 = real speed.
        Some(p) => {
            let m = unsafe { read_anchor(p as *const Ctl).2 };
            if still_ours(p as *const Ctl) {
                m
            } else {
                1
            }
        }
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

/// Convert a fake UTC FILETIME (100 ns ticks) into `*lp` as a SYSTEMTIME. Returns whether it wrote
/// `*lp`: `false` means `FileTimeToSystemTime` rejected the instant (e.g. a moment near the FILETIME
/// boundary shifted by a large `tz_bias`), and the caller must defer to the real API rather than leave
/// `*lp` holding uninitialized garbage while claiming success (L-3).
///
/// # Safety
/// `lp` must be a valid, writable pointer to a `SYSTEMTIME`.
unsafe fn write_systemtime(lp: *mut SYSTEMTIME, ft_ticks: i64) -> bool { unsafe {
    let ft = i64_to_ft(ft_ticks);
    let mut st = SYSTEMTIME::default();
    if FileTimeToSystemTime(&ft, &mut st).is_ok() {
        *lp = st;
        true
    } else {
        false
    }
}}

// --- Detours -------------------------------------------------------------------
// Each fills its out-parameter with the fake instant, or falls back to the original
// if the anchor is unreadable or the pointer is null.

unsafe extern "system" fn h_gstaft(lp: *mut FILETIME) { unsafe {
    bump(IDX_GSTAFT);
    match compute_fake() {
        Some(t) if !lp.is_null() => *lp = i64_to_ft(t),
        _ => {
            if let Some(o) = O_GSTAFT.get() {
                o(lp)
            }
        }
    }
}}

unsafe extern "system" fn h_gstpaft(lp: *mut FILETIME) { unsafe {
    bump(IDX_GSTPAFT);
    match compute_fake() {
        Some(t) if !lp.is_null() => *lp = i64_to_ft(t),
        _ => {
            if let Some(o) = O_GSTPAFT.get() {
                o(lp)
            }
        }
    }
}}

unsafe extern "system" fn h_gst(lp: *mut SYSTEMTIME) { unsafe {
    bump(IDX_GST);
    // `done` is false when there is no fake instant, the pointer is null, or the SYSTEMTIME conversion
    // failed (L-3) - in every case defer to the real API rather than leave `*lp` as garbage.
    let done = match compute_fake() {
        Some(t) if !lp.is_null() => write_systemtime(lp, t),
        _ => false,
    };
    if !done
        && let Some(o) = O_GST.get() {
            o(lp);
        }
}}

unsafe extern "system" fn h_glt(lp: *mut SYSTEMTIME) { unsafe {
    bump(IDX_GLT);
    let done = match compute_fake() {
        // local = UTC_fake - Bias (UTC = local + Bias), session zone without DST.
        //
        // Checked, not bare: this crate has overflow-checks off, and a plain subtraction wrapped for
        // every zone east of UTC once the fake clock sat on the clamp - the conversion then failed and
        // this channel fell back to the REAL clock while the UTC channels stayed fake (R2-X7). The
        // clamp now keeps a zone bias of headroom, so this cannot trigger for our own clock - it stays
        // explicit because a caller-set bias is data, and silent wrapping is how it hurt the first time.
        Some(t) if !lp.is_null() => match t.checked_sub(cur_tz_bias() as i64 * 60 * 10_000_000) {
            Some(local) => write_systemtime(lp, local),
            None => false,
        },
        _ => false,
    };
    if !done
        && let Some(o) = O_GLT.get() {
            o(lp);
        }
}}

unsafe extern "system" fn h_ntqst(lp: *mut i64) -> i32 { unsafe {
    bump(IDX_NTQST);
    match compute_fake() {
        Some(t) if !lp.is_null() => {
            *lp = t;
            0 // STATUS_SUCCESS
        }
        // A null output pointer: the real NtQuerySystemTime answers STATUS_ACCESS_VIOLATION. Reporting
        // success while writing nothing would let the caller read whatever was in that memory as a time.
        Some(_) => STATUS_ACCESS_VIOLATION,
        None => O_NTQST.get().map(|o| o(lp)).unwrap_or(STATUS_UNSUCCESSFUL),
    }
}}

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

unsafe extern "system" fn h_ntqsi(class: i32, info: *mut c_void, len: u32, retlen: *mut u32) -> i32 { unsafe {
    let o = match O_NTQSI.get() {
        Some(o) => o,
        None => return STATUS_UNSUCCESSFUL, // no trampoline: the buffer would be left unfilled, so do not report success
    };
    let status = o(class, info, len, retlen); // always call the original: it fills the whole struct
    if class == SYSTEM_TIME_OF_DAY_INFORMATION {
        // Count only time-of-day queries - NtQuerySystemInformation is a multiplexer, so bumping on
        // every class would inflate the audit's notion of how often the app reads time (rule 4).
        bump(IDX_NTQSI);
        // NT_SUCCESS(status) == status >= 0 - the length guard keeps the [8, 16) write in bounds when a
        // caller passes a truncated buffer (honest partial: leave it, never write past its end).
        if status >= 0 && !info.is_null() && len as usize >= TOD_CURRENTTIME_OFFSET + 8
            && let Some(fake) = compute_fake() {
                // None when the core detached - then leave the real CurrentTime the original wrote.
                let p = (info as *mut u8).add(TOD_CURRENTTIME_OFFSET) as *mut i64;
                core::ptr::write_unaligned(p, fake);
            }
    }
    status
}}

// --- Session zone -------------------------------------------------------------
// Report the session zone (Bias = tz_bias, no DST) so a target's notion of "which
// zone am I in" agrees with the shifted GetLocalTime.

const SESSION_ZONE_NAME: &str = "Chrono Session";
const TIME_ZONE_ID_INVALID: u32 = 0xFFFF_FFFF;

/// Copy `s` into a fixed-length UTF-16 field, NUL-terminated (zone name buffers).
use chrono_ctl::set_wide;

unsafe extern "system" fn h_gtzi(lp: *mut TIME_ZONE_INFORMATION) -> u32 { unsafe {
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
}}

unsafe extern "system" fn h_gdtzi(lp: *mut DYNAMIC_TIME_ZONE_INFORMATION) -> u32 { unsafe {
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
}}

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
unsafe fn write_session_local(utc: *const SYSTEMTIME, local: *mut SYSTEMTIME) -> bool { unsafe {
    let mut ft = FILETIME::default();
    if SystemTimeToFileTime(utc, &mut ft).is_err() {
        return false;
    }
    // Checked like every other bias shift here (R2-X7): the input is the CALLER's time, so a value
    // near the end of the range is data we are handed, and this crate has overflow-checks off.
    // Propagate the SYSTEMTIME conversion result (L-3): on failure the caller defers to the original,
    // never reporting success with `local` left unwritten.
    match ft_to_i64(ft).checked_sub(cur_tz_bias() as i64 * 60 * 10_000_000) {
        Some(shifted) => write_systemtime(local, shifted),
        None => false,
    }
}}

/// Convert a caller-supplied local SYSTEMTIME to session-UTC (local + tz_bias, the inverse
/// of write_session_local) and write it to `utc`. Returns false if the local time could not
/// be converted. Shared by the TzSpecificLocalTimeToSystemTime detours.
///
/// # Safety
/// `local` must point to a valid SYSTEMTIME and `utc` to a valid writable SYSTEMTIME.
unsafe fn write_session_utc(local: *const SYSTEMTIME, utc: *mut SYSTEMTIME) -> bool { unsafe {
    let mut ft = FILETIME::default();
    if SystemTimeToFileTime(local, &mut ft).is_err() {
        return false;
    }
    // Checked like every other bias shift here (R2-X7) - see write_session_local.
    // Propagate the SYSTEMTIME conversion result (L-3): on failure the caller defers to the original.
    match ft_to_i64(ft).checked_add(cur_tz_bias() as i64 * 60 * 10_000_000) {
        Some(shifted) => write_systemtime(utc, shifted),
        None => false,
    }
}}

/// Shift a FILETIME by the session bias: `add` for local->UTC (utc = local + bias), clear for
/// UTC->local (local = utc - bias). The FILETIME conversions carry no zone argument, so they
/// always mean the active zone, which we replace with the flat session zone.
///
/// # Safety
/// `src` and `dst` must be valid, non-null FILETIME pointers.
unsafe fn shift_filetime(src: *const FILETIME, dst: *mut FILETIME, add: bool) -> i32 { unsafe {
    let ticks = ft_to_i64(*src);
    // Out of range means "we cannot express this", reported as failure so the caller falls back to
    // the original, exactly as `write_systemtime` already does (L-3). The arithmetic and the
    // "still a FILETIME" test live in `chrono-ctl` (`shift_ticks_by_bias`), where they can be
    // tested - this crate is a cdylib and has no tests of its own.
    let shifted = match chrono_ctl::shift_ticks_by_bias(ticks, cur_tz_bias(), add) {
        Some(v) => v,
        None => {
            // FALSE means "call GetLastError" to a Win32 caller, and leaving the thread's last error
            // as whatever the previous operation set it to is how a caller ends up reporting an
            // unrelated failure (R2-N16). Say what actually happened.
            SetLastError(ERROR_INVALID_PARAMETER);
            return 0;
        }
    };
    *dst = i64_to_ft(shifted);
    1
}}

// FileTimeToLocalFileTime (UTC -> local) and LocalFileTimeToFileTime (local -> UTC) take no
// zone argument, so they always mean the active zone - always substitute the session zone.
unsafe extern "system" fn h_ftlft(utc: *const FILETIME, local: *mut FILETIME) -> i32 { unsafe {
    bump(IDX_FTLFT);
    if detached() || utc.is_null() || local.is_null() {
        return O_FTLFT.get().map(|o| o(utc, local)).unwrap_or(0);
    }
    shift_filetime(utc, local, false)
}}

unsafe extern "system" fn h_lftft(local: *const FILETIME, utc: *mut FILETIME) -> i32 { unsafe {
    bump(IDX_LFTFT);
    if detached() || local.is_null() || utc.is_null() {
        return O_LFTFT.get().map(|o| o(local, utc)).unwrap_or(0);
    }
    shift_filetime(local, utc, true)
}}

// TzSpecificLocalTimeToSystemTime (+Ex): reverse of the STtSLT detours (local -> UTC). A NULL
// zone means the active zone -> session zone (utc = local + tz_bias). A named zone passes through.
unsafe extern "system" fn h_tltst(
    tzi: *const TIME_ZONE_INFORMATION,
    local: *const SYSTEMTIME,
    utc: *mut SYSTEMTIME,
) -> i32 { unsafe {
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
}}

unsafe extern "system" fn h_tltstex(
    tzi: *const DYNAMIC_TIME_ZONE_INFORMATION,
    local: *const SYSTEMTIME,
    utc: *mut SYSTEMTIME,
) -> i32 { unsafe {
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
}}

unsafe extern "system" fn h_stsl(
    tzi: *const TIME_ZONE_INFORMATION,
    utc: *const SYSTEMTIME,
    local: *mut SYSTEMTIME,
) -> i32 { unsafe {
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
}}

unsafe extern "system" fn h_stslex(
    tzi: *const DYNAMIC_TIME_ZONE_INFORMATION,
    utc: *const SYSTEMTIME,
    local: *mut SYSTEMTIME,
) -> i32 { unsafe {
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
}}

// --- Duration axis (opt-in) ----------------------------------------------------
// Only installed when scale_duration is set. The anchor (`dur_tick_c0`, `dur_quit_c0`, `dur_q0`) lives
// in the shared `Ctl`, initialized by the core in `prepare` and REBASED on every `set_multiplier` so a
// speed change never rewinds the axis (H-1, untouchable rule 3). Each detour reads the base AND the
// multiplier in one `read_dur` snapshot (they can never tear apart) and projects off the trampoline QUIT.
// `m` is clamped to >= 1 inside `dur_tick_at`/`dur_quit_at`, so the axis keeps advancing even when the
// wall clock is frozen. QPC and timeGetTime are left real (ADR-2).

unsafe extern "system" fn h_tick() -> u64 { unsafe {
    bump(IDX_GTC64);
    if detached() {
        return O_TICK.get().map(|o| o()).unwrap_or(0);
    }
    match ctl_ptr() {
        Some(p) => {
            let (dur_tick_c0, _quit_c0, dur_q0, m) = read_dur(p as *const Ctl);
            let fake = dur_tick_at(dur_tick_c0, dur_q0, m, real_quit());
            // Ownership checked after the read (R2-S6): a reclaimed block would hand this target
            // another session's duration base, which reads as the axis jumping.
            if still_ours(p as *const Ctl) {
                fake
            } else {
                O_TICK.get().map(|o| o()).unwrap_or(0)
            }
        }
        None => O_TICK.get().map(|o| o()).unwrap_or(0),
    }
}}

// GetTickCount (32-bit): the low 32 bits of the SAME scaled millisecond count as
// GetTickCount64 (shares the dur_tick_c0 base), so a target comparing the two sees them
// agree. Wraps at 2^32 ms like the real one - and sooner under acceleration - which is the
// honest behavior of a fast 32-bit counter - callers handle the wrap with unsigned deltas.
unsafe extern "system" fn h_tick32() -> u32 { unsafe {
    bump(IDX_GTC);
    if detached() {
        return O_TICK32.get().map(|o| o()).unwrap_or(0);
    }
    match ctl_ptr() {
        Some(p) => {
            let (dur_tick_c0, _quit_c0, dur_q0, m) = read_dur(p as *const Ctl);
            let fake = dur_tick_at(dur_tick_c0, dur_q0, m, real_quit()) as u32;
            if still_ours(p as *const Ctl) {
                fake
            } else {
                O_TICK32.get().map(|o| o()).unwrap_or(0)
            }
        }
        None => O_TICK32.get().map(|o| o()).unwrap_or(0),
    }
}}

unsafe extern "system" fn h_quit(lp: *mut u64) -> i32 { unsafe {
    bump(IDX_QUIT);
    if detached() {
        return O_QUIT.get().map(|o| o(lp)).unwrap_or(0);
    }
    if !lp.is_null() {
        match ctl_ptr() {
            Some(p) => {
                let (_tick_c0, dur_quit_c0, dur_q0, m) = read_dur(p as *const Ctl);
                let fake = dur_quit_at(dur_quit_c0, dur_q0, m, real_quit()) as u64;
                if !still_ours(p as *const Ctl) {
                    return O_QUIT.get().map(|o| o(lp)).unwrap_or(0);
                }
                *lp = fake;
            }
            // No control block (unreachable: CTL_PTR is set before these hooks install) - defer to the
            // real value rather than fake a zero.
            None => return O_QUIT.get().map(|o| o(lp)).unwrap_or(0),
        }
    }
    1 // nonzero BOOL = success
}}

// QPC axis (ADR-2 reversal, opt-in `scale_qpc`): scale QueryPerformanceCounter, so a target whose elapsed
// clock is monotonic/perf_counter (Python 3.13+), Stopwatch (.NET) or nanoTime (Java) - all QPC-backed -
// also accelerates. The anchor lives in the shared Ctl (dur_qpc_c0 / dur_qpc_q0), initialized by the core
// in prepare and REBASED on every set_multiplier (freeze then re-anchor), so a speed change never rewinds
// the axis (H-1, untouchable rule 3). Each call reads the base AND the multiplier in one read_qpc snapshot
// (they can never tear apart) and projects off the trampoline QPC. QueryPerformanceFrequency is left real,
// so elapsed (delta / freq) scales by exactly M. Spike A (2026-09-02) proved this is stable on Win11 today
// (E4's QPC hang did not recur with the bounded seqlock reader H-2).
type QpcFn = unsafe extern "system" fn(*mut i64) -> i32;
static O_QPC: OnceLock<QpcFn> = OnceLock::new();

unsafe extern "system" fn h_qpc(lp: *mut i64) -> i32 { unsafe {
    // Counted like every other channel (R2-S3). QPC is the hottest clock a process calls, so the cost
    // was measured rather than assumed - and measured as an INTERLEAVED A/B on two hook builds, because
    // sequential batches drifted by ~9 ns between them and would have shown a slowdown that was not
    // there. Five alternating pairs, 5 M calls each, x64, probe `pqpc`: the counted build ran
    // +0.08 ns/call on average (worst pair +0.2), against ~25 ns for a bare QPC and ~37 ns hooked. The
    // bump disappears next to the trampoline call and the seqlock read it rides on.
    bump(IDX_QPC);
    let o = match O_QPC.get() {
        Some(o) => *o,
        None => return 0,
    };
    if lp.is_null() {
        return o(lp);
    }
    match ctl_ptr() {
        Some(p) if !detached() => {
            let mut real: i64 = 0;
            o(&mut real); // real QPC via the trampoline (bypasses this hook, no recursion)
            let (qpc_c0, qpc_q0, m) = read_qpc(p as *const Ctl);
            let fake = dur_qpc_at(qpc_c0, qpc_q0, m, real);
            if !still_ours(p as *const Ctl) {
                return o(lp); // reclaimed mid-read (R2-S6): real QPC
            }
            *lp = fake;
            1
        }
        // Detached (core gone) or no control block -> real QPC, so the target reverts to real time cleanly.
        _ => o(lp),
    }
}}

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
/// flag set and passes through) when it is - None when this is an internal cascade (pass the
/// original through, uncounted) or the core has detached (fall through to real time). Bumps
/// coverage only for a top-level app call, so the audit counts what the app called, not what
/// Windows re-entered.
fn try_enter_wait(idx: usize) -> Option<(i64, WaitGuard)> {
    if SCALING_WAIT.get() {
        return None; // internal cascade: pass through, do not bump
    }
    bump(idx);
    if detached() {
        // The core is gone, so this wait runs real - but it still cascades internally (Sleep funnels
        // into NtDelayExecution), and without the guard the inner call counted as a second top-level
        // call. The guard is held here too, so one application wait is one tally either way (rule 4).
        SCALING_WAIT.set(true);
        return Some((1, WaitGuard)); // multiplier 1 = real time, unchanged
    }
    SCALING_WAIT.set(true);
    Some((dur_multiplier(), WaitGuard))
}

unsafe extern "system" fn h_sleep(ms: u32) { unsafe {
    let o = match O_SLEEP.get() {
        Some(o) => o,
        None => return,
    };
    match try_enter_wait(IDX_SLEEP) {
        Some((m, _guard)) => o(scale_wait(ms, m)),
        None => o(ms),
    }
}}

unsafe extern "system" fn h_sleepex(ms: u32, alertable: i32) -> u32 { unsafe {
    let o = match O_SLEEPEX.get() {
        Some(o) => o,
        None => return 0,
    };
    match try_enter_wait(IDX_SLEEPEX) {
        Some((m, _guard)) => o(scale_wait(ms, m), alertable),
        None => o(ms, alertable),
    }
}}

// NtDelayExecution is the shared funnel Sleep and SleepEx bottom out on, so hooking it makes the
// re-entrancy guard load-bearing (a scaled Sleep re-enters here and must pass through). It also
// catches callers that reach ntdll directly. The interval is signed 100 ns: only a negative
// (relative) delay is scaled - a positive (absolute deadline) or null passes through.
unsafe extern "system" fn h_ntdelay(alertable: u8, interval: *const i64) -> i32 { unsafe {
    let o = match O_NTDELAY.get() {
        Some(o) => o,
        None => return STATUS_UNSUCCESSFUL, // no trampoline: no delay happened, so do not report success
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
}}

// Wait axis class B (ADR-7, option b): object waits are COUNTED but deliberately NOT scaled.
// Shortening a wait on a real I/O / hardware / IPC handle would fake a timeout, so each detour
// forwards the timeout untouched and the audit warns instead. The only subtlety is COUNTING: an
// object-wait export may internally reach another hooked one (WaitForSingleObject -> ...Ex,
// WaitForMultipleObjects -> ...Ex), so a thread-local guard counts each app-level wait once,
// attributed to the export the app actually called - an internal cascade passes through uncounted.
// This guard gates only counting (class B never divides), separate from class A's scaling guard -
// the two wait families never cross-nest (Sleep/NtDelay do not call WaitForX and vice versa).
// Measured on Win11 26200 (guard on vs off, psleep): the cascades take an INTERNAL path and do not
// reach the exported partner (like Sleep -> SleepEx in class A), so the guard is a correct policy
// here, not yet load-bearing - it protects other Windows versions and direct ...Ex callers.
// Detached state is irrelevant: we never modify the wait either way.

thread_local! {
    static OBSERVING_WAIT: Cell<bool> = const { Cell::new(false) };
}

/// Clears the class-B counting-reentrancy flag when the top-level object-wait detour returns.
struct ObservedWaitGuard;
impl Drop for ObservedWaitGuard {
    fn drop(&mut self) {
        OBSERVING_WAIT.set(false);
    }
}

/// Count an app-level object wait once, unless this is an internal cascade from another hooked
/// object-wait export (then the outer call already counted it). When this is the top-level call it
/// returns a guard, held across the forwarded original so the cascade sees the flag set.
fn enter_observed_wait(idx: usize) -> Option<ObservedWaitGuard> {
    if OBSERVING_WAIT.get() {
        return None; // internal cascade: counted at the top level already
    }
    bump(idx);
    OBSERVING_WAIT.set(true);
    Some(ObservedWaitGuard)
}

unsafe extern "system" fn h_wfso(handle: HANDLE, ms: u32) -> u32 { unsafe {
    let o = match O_WFSO.get() {
        Some(o) => o,
        None => return WAIT_FAILED.0, // no trampoline: fail the wait, never claim it was signalled
    };
    let _g = enter_observed_wait(IDX_WFSO);
    o(handle, ms)
}}

unsafe extern "system" fn h_wfsoex(handle: HANDLE, ms: u32, alertable: i32) -> u32 { unsafe {
    let o = match O_WFSOEX.get() {
        Some(o) => o,
        None => return WAIT_FAILED.0, // no trampoline: fail the wait, never claim it was signalled
    };
    let _g = enter_observed_wait(IDX_WFSOEX);
    o(handle, ms, alertable)
}}

unsafe extern "system" fn h_wfmo(count: u32, handles: *const HANDLE, wait_all: i32, ms: u32) -> u32 { unsafe {
    let o = match O_WFMO.get() {
        Some(o) => o,
        None => return WAIT_FAILED.0, // no trampoline: fail the wait, never claim it was signalled
    };
    let _g = enter_observed_wait(IDX_WFMO);
    o(count, handles, wait_all, ms)
}}

unsafe extern "system" fn h_wfmoex(
    count: u32,
    handles: *const HANDLE,
    wait_all: i32,
    ms: u32,
    alertable: i32,
) -> u32 { unsafe {
    let o = match O_WFMOEX.get() {
        Some(o) => o,
        None => return WAIT_FAILED.0, // no trampoline: fail the wait, never claim it was signalled
    };
    let _g = enter_observed_wait(IDX_WFMOEX);
    o(count, handles, wait_all, ms, alertable)
}}

unsafe extern "system" fn h_soaw(signal: HANDLE, wait: HANDLE, ms: u32, alertable: i32) -> u32 { unsafe {
    let o = match O_SOAW.get() {
        Some(o) => o,
        None => return WAIT_FAILED.0, // no trampoline: fail the wait, never claim it was signalled
    };
    let _g = enter_observed_wait(IDX_SOAW);
    o(signal, wait, ms, alertable)
}}

// The message waits live in user32. Same class-B story (count, never scale, forward untouched), and
// the same counting guard - MsgWaitForMultipleObjects may internally reach ...Ex. The Ex form drops
// fWaitAll and reorders its args (see the fn types).
unsafe extern "system" fn h_mwfmo(
    count: u32,
    handles: *const HANDLE,
    wait_all: i32,
    ms: u32,
    wake_mask: u32,
) -> u32 { unsafe {
    let o = match O_MWFMO.get() {
        Some(o) => o,
        None => return WAIT_FAILED.0, // no trampoline: fail the wait, never claim it was signalled
    };
    let _g = enter_observed_wait(IDX_MWFMO);
    o(count, handles, wait_all, ms, wake_mask)
}}

unsafe extern "system" fn h_mwfmoex(
    count: u32,
    handles: *const HANDLE,
    ms: u32,
    wake_mask: u32,
    flags: u32,
) -> u32 { unsafe {
    let o = match O_MWFMOEX.get() {
        Some(o) => o,
        None => return WAIT_FAILED.0, // no trampoline: fail the wait, never claim it was signalled
    };
    let _g = enter_observed_wait(IDX_MWFMOEX);
    o(count, handles, ms, wake_mask, flags)
}}

// The four waits added on 2026-09-08. Same shape as the family above and for the same reason: a
// timeout on a condition variable, an address or a socket event can be a real one, so the value is
// forwarded untouched and only the fact of the call is recorded. What changes is that a target which
// blocks on one of these is no longer invisible to the audit.
//
// `enter_observed_wait` matters more here than anywhere else in this family, because these DO cascade
// into it: the documentation says WSAWaitForMultipleEvents is implemented on WaitForMultipleObjectsEx,
// and the condition-variable waits reach the address wait underneath. Without the thread-local guard
// one blocked thread would be counted two or three times over.

unsafe extern "system" fn h_scvsrw(cv: *mut c_void, lock: *mut c_void, ms: u32, flags: u32) -> i32 { unsafe {
    let o = match O_SCVSRW.get() {
        Some(o) => o,
        None => return 0, // no trampoline: report the failure, never claim the sleep succeeded
    };
    let _g = enter_observed_wait(IDX_SCVSRW);
    o(cv, lock, ms, flags)
}}

unsafe extern "system" fn h_scvcs(cv: *mut c_void, cs: *mut c_void, ms: u32) -> i32 { unsafe {
    let o = match O_SCVCS.get() {
        Some(o) => o,
        None => return 0, // no trampoline: report the failure, never claim the sleep succeeded
    };
    let _g = enter_observed_wait(IDX_SCVCS);
    o(cv, cs, ms)
}}

unsafe extern "system" fn h_woa(
    address: *const c_void,
    compare: *const c_void,
    size: usize,
    ms: u32,
) -> i32 { unsafe {
    let o = match O_WOA.get() {
        Some(o) => o,
        None => return 0, // no trampoline: report the failure, never claim the wait succeeded
    };
    let _g = enter_observed_wait(IDX_WOA);
    o(address, compare, size, ms)
}}

unsafe extern "system" fn h_wsawfme(
    count: u32,
    events: *const HANDLE,
    wait_all: i32,
    ms: u32,
    alertable: i32,
) -> u32 { unsafe {
    let o = match O_WSAWFME.get() {
        Some(o) => o,
        // WSA_WAIT_FAILED is WAIT_FAILED (0xFFFFFFFF) - fail the wait, never claim it was signalled.
        None => return WAIT_FAILED.0,
    };
    let _g = enter_observed_wait(IDX_WSAWFME);
    o(count, events, wait_all, ms, alertable)
}}

// --- Settable timers (ADR-7 class C) -------------------------------------------
// SetWaitableTimer(Ex) ask the kernel to signal a timer after a delay or at an instant. Unlike the
// object waits (class B, left real), a timer is pure time-keeping, so under scale_duration we SCALE
// it like class A: a relative due-time and a periodic lPeriod divide by M (scale_timer_due /
// scale_timer_period). The subtlety is the ABSOLUTE (positive) due-time: the app computed it from
// the FAKE wall clock, but the kernel reads the REAL clock for absolute timers, so we convert it to
// the real interval until the fake clock reaches it and forward it as a scaled RELATIVE due
// (scale_timer_due). One thread-local guard makes each app-level call scale exactly once and be
// counted against the export the app called: SetWaitableTimer may internally reach
// SetWaitableTimerEx, and double-scaling a due (due/M/M) would fire the timer far too early. This
// guard is separate from class A's (Sleep) and class B's (object waits) - the three families never
// cross-nest. Measured on Win11 26200 (psleep, x64+x86): SetWaitableTimer's coverage counts exactly
// the app's calls (2 via SetWaitableTimer, 1 via SetWaitableTimerEx) and the real wait scales once
// (~M, not ~M^2), so the internal path does NOT reach the exported ...Ex partner here (like Sleep ->
// SleepEx in class A) - the guard is correct policy, not yet load-bearing, protecting other Windows
// versions and direct ...Ex callers (zasady/03 section 4: measured, not assumed).

thread_local! {
    static SCALING_TIMER: Cell<bool> = const { Cell::new(false) };
}

/// Clears the class-C re-entrancy flag when the top-level timer detour returns.
struct TimerGuard;
impl Drop for TimerGuard {
    fn drop(&mut self) {
        SCALING_TIMER.set(false);
    }
}

/// Decide whether this settable-timer call is the top-level app call we should scale. Returns the
/// duration multiplier and a guard (held across the original call, so an inner cascade to the Ex
/// partner sees the flag set and passes through) when it is - None on an internal cascade (pass
/// through, uncounted) or when the core has detached (real time). Mirrors try_enter_wait on its own
/// flag. Bumps coverage only for a top-level app call (rule 4).
fn try_enter_timer(idx: usize) -> Option<TimerGuard> {
    if SCALING_TIMER.get() {
        return None; // internal cascade: pass through, do not bump
    }
    bump(idx);
    // The guard is raised whether or not the core is still there. Symmetric to try_enter_wait
    // (R2-S4): with the core gone an internal cascade (SetWaitableTimer -> SetWaitableTimerEx, should
    // a Windows version take that path) must still count as ONE application call rather than two
    // (rule 4). Nothing else is decided here - the caller asks `fake_now_and_dur_m` for the clock and
    // the rate together, and a detached session answers None there, so the arguments are forwarded
    // untouched. This used to return a multiplier of its own, read from a second snapshot of the
    // control block that nothing tied to the one the due date was measured against.
    SCALING_TIMER.set(true);
    Some(TimerGuard)
}

unsafe extern "system" fn h_swt(
    timer: HANDLE,
    due: *const i64,
    period: i32,
    pfn: *const c_void,
    arg: *const c_void,
    resume: i32,
) -> i32 { unsafe {
    let o = match O_SWT.get() {
        Some(o) => o,
        None => return 0,
    };
    match try_enter_timer(IDX_SWT) {
        // _guard held across the whole original call, so an internal cascade to SetWaitableTimerEx
        // passes through uncounted and unscaled. A null due (the API would reject it) or a detach
        // mid-call falls through to the original untouched.
        Some(_guard) => match (due.is_null(), fake_now_and_dur_m()) {
            (false, Some((fake_now, m))) => {
                let scaled_due = scale_timer_due(*due, fake_now, m);
                let scaled_period = scale_timer_period(period, m);
                o(timer, &scaled_due as *const i64, scaled_period, pfn, arg, resume)
            }
            _ => o(timer, due, period, pfn, arg, resume),
        },
        None => o(timer, due, period, pfn, arg, resume),
    }
}}

unsafe extern "system" fn h_swtex(
    timer: HANDLE,
    due: *const i64,
    period: i32,
    pfn: *const c_void,
    arg: *const c_void,
    wake_context: *const c_void,
    tolerable_delay: u32,
) -> i32 { unsafe {
    let o = match O_SWTEX.get() {
        Some(o) => o,
        None => return 0,
    };
    match try_enter_timer(IDX_SWTEX) {
        Some(_guard) => match (due.is_null(), fake_now_and_dur_m()) {
            (false, Some((fake_now, m))) => {
                let scaled_due = scale_timer_due(*due, fake_now, m);
                let scaled_period = scale_timer_period(period, m);
                o(timer, &scaled_due as *const i64, scaled_period, pfn, arg, wake_context, tolerable_delay)
            }
            _ => o(timer, due, period, pfn, arg, wake_context, tolerable_delay),
        },
        None => o(timer, due, period, pfn, arg, wake_context, tolerable_delay),
    }
}}

// SetTimer (user32, ADR-7 class C): scale the uElapse interval so WM_TIMER arrives in step with the
// fake clock. A relative interval only (no absolute form, no INFINITE), and no cross-channel cascade
// (SetTimer bottoms out on the NtUserSetTimer syscall, not another hooked export), so no re-entrancy
// guard - just count and scale. Detached -> pass the real interval through. The HWND, timer id, and
// TIMERPROC are forwarded untouched - the scaled interval below USER_TIMER_MINIMUM is Windows' clamp.
unsafe extern "system" fn h_settimer(
    hwnd: *mut c_void,
    id: usize,
    elapse: u32,
    timer_proc: *const c_void,
) -> usize { unsafe {
    let o = match O_SETTIMER.get() {
        Some(o) => o,
        None => return 0,
    };
    bump(IDX_SETTIMER);
    if detached() {
        return o(hwnd, id, elapse, timer_proc);
    }
    o(hwnd, id, scale_timer_elapse(elapse, dur_multiplier()), timer_proc)
}}

// timeSetEvent (winmm, ADR-7 class C, OBSERVED): count the multimedia timer but never scale its
// uDelay - scaling would shift audio/MIDI timing, the winmm cost ADR-2 avoids (like timeGetTime), so
// the audit warns instead (timer.multimedia_not_scaled). No re-entrancy guard (it does not cascade
// onto another hooked export) and no detached check (we never modify the call, so detached state is
// irrelevant, like the class-B object waits). Every argument is forwarded untouched.
unsafe extern "system" fn h_timesetevent(
    delay: u32,
    resolution: u32,
    time_proc: *const c_void,
    user: usize,
    event: u32,
) -> u32 { unsafe {
    let o = match O_TIMESETEVENT.get() {
        Some(o) => o,
        None => return 0,
    };
    bump(IDX_TIMESETEVENT);
    o(delay, resolution, time_proc, user, event)
}}

// timeGetTime (winmm, duration axis, partial ADR-2 reversal of 2026-09-07): the SAME scaled millisecond
// count as GetTickCount, sharing its dur_tick_c0 base on purpose. Both exports answer "milliseconds since
// the system started", so a target reading one against the other has to see them agree - they differ by
// less than one tick in reality, and inventing that difference would need a second anchor for no gain.
// Wraps at 2^32 ms like the real one, and sooner under acceleration, which is the honest behaviour of a
// fast 32-bit counter.
//
// Detached or without a control block it returns the REAL value through the trampoline, so a target
// reverts cleanly when the core goes away - the same shape as h_tick32. The None arm is unreachable
// (make_hook fills the slot before any hook is enabled) and returns the real value too rather than
// inventing a reading.
unsafe extern "system" fn h_timegettime() -> u32 { unsafe {
    let real = || O_TIMEGETTIME.get().map(|o| o()).unwrap_or(0);
    bump(IDX_TIMEGETTIME);
    if detached() {
        return real();
    }
    match ctl_ptr() {
        Some(p) => {
            let (dur_tick_c0, _quit_c0, dur_q0, m) = read_dur(p as *const Ctl);
            let fake = dur_tick_at(dur_tick_c0, dur_q0, m, real_quit()) as u32;
            // Ownership checked after the read (R2-S6), exactly as the tick detours do.
            if still_ours(p as *const Ctl) { fake } else { real() }
        }
        None => real(),
    }
}}

// connect (ws2_32, SourceObserved): a network connection is a suspected SERVER time source, which no
// local hook can cover. We only COUNT it and forward untouched (never modify the connection) - the audit
// then warns source.network_at_start. Like timeSetEvent: no guard, no detached check (we never change the
// call). The unreachable None path returns SOCKET_ERROR (-1) so an un-hooked call never fakes success.
unsafe extern "system" fn h_connect(s: usize, name: *const c_void, namelen: i32) -> i32 { unsafe {
    let o = match O_CONNECT.get() {
        Some(o) => o,
        None => return -1,
    };
    bump(IDX_CONNECT);
    o(s, name, namelen)
}}

// Thread-pool timers (kernel32, ADR-7 class C): SetThreadpoolTimer / SetThreadpoolTimerEx share the
// time structure of SetWaitableTimer - a FILETIME due (absolute converted to a scaled relative
// interval, relative scaled), an msPeriod, and an msWindowLength - so they scale the same way. The
// detour is STATELESS (no per-timer state), which is what makes it safe under the thread pool's
// concurrency: SetThreadpoolTimer may be called from many threads, and a callback (running on a
// worker thread) may re-arm the timer, but each call just reads the shared anchor (seqlock) and
// scales. The class-C thread-local guard SCALING_TIMER counts each app-level call once and handles a
// Set -> ...Ex cascade - being thread-local, a worker-thread re-arm gets its own fresh guard.

/// Scale a thread-pool timer's FILETIME due, msPeriod, and msWindowLength for a top-level app call.
/// Returns the scaled `(due_ft, period, window)` to forward, or None to forward the originals
/// unchanged (NULL due = cancel, or the core detached mid-call). Shared by both detours.
///
/// # Safety
/// `pft`, when non-null, must point to a valid FILETIME.
unsafe fn scale_tp_timer(pft: *const FILETIME, period: u32, window: u32) -> Option<(FILETIME, u32, u32)> { unsafe {
    if pft.is_null() {
        return None; // NULL = cancel: forward untouched
    }
    // One snapshot for both halves: the due date is absolute, so the rate it is divided by has to be
    // the rate that belongs to this `fake_now` and not to whatever the block said a moment earlier.
    let (fake_now, m) = fake_now_and_dur_m()?; // detached: forward untouched
    let scaled_due = scale_timer_due(ft_to_i64(*pft), fake_now, m);
    Some((i64_to_ft(scaled_due), scale_timer_period_ms(period, m), scale_timer_elapse(window, m)))
}}

unsafe extern "system" fn h_set_tp_timer(pti: *mut c_void, pft: *const FILETIME, period: u32, window: u32) { unsafe {
    let o = match O_TPTIMER.get() {
        Some(o) => o,
        None => return,
    };
    match try_enter_timer(IDX_TPTIMER) {
        // _guard held across the whole original call, so a Set -> ...Ex cascade passes through once.
        Some(_guard) => match scale_tp_timer(pft, period, window) {
            Some((ft, p, w)) => o(pti, &ft as *const FILETIME, p, w),
            None => o(pti, pft, period, window),
        },
        None => o(pti, pft, period, window),
    }
}}

unsafe extern "system" fn h_set_tp_timer_ex(
    pti: *mut c_void,
    pft: *const FILETIME,
    period: u32,
    window: u32,
) -> i32 { unsafe {
    let o = match O_TPTIMEREX.get() {
        Some(o) => o,
        None => return 0,
    };
    match try_enter_timer(IDX_TPTIMEREX) {
        Some(_guard) => match scale_tp_timer(pft, period, window) {
            Some((ft, p, w)) => o(pti, &ft as *const FILETIME, p, w),
            None => o(pti, pft, period, window),
        },
        None => o(pti, pft, period, window),
    }
}}

// --- Direct process creation (ADR-3, observed) ---------------------------------
// NtCreateUserProcess is the funnel under CreateProcessInternalW, so a hooked CreateProcessW/A reaches
// it. We count only a DIRECT NtCreateUserProcess (a child spawned bypassing CreateProcess*), because
// the CreateProcess* detours already inherit the session into their child. SPAWNING is a thread-local
// flag those detours raise around their original call (which funnels here on the same thread) - when it
// is set, this detour just forwards, uncounted. A direct call finds it clear, counts, and warns - we
// deliberately do NOT self-inject (that means manipulating undocumented native structures, a crash
// risk for near-zero value, since real targets spawn through the covered CreateProcess*).

/// NTSTATUS failure returned if the NtCreateUserProcess detour is somehow entered before its
/// trampoline is set (unreachable). Negative NTSTATUS = failure, so the caller does not treat an
/// un-created process as a success.
const STATUS_UNSUCCESSFUL: i32 = 0xC000_0001u32 as i32;
/// What the real NtQuerySystemTime answers for a null output pointer.
const STATUS_ACCESS_VIOLATION: i32 = 0xC000_0005u32 as i32;

thread_local! {
    static SPAWNING: Cell<bool> = const { Cell::new(false) };
}

/// Raised for the duration of a CreateProcess* original call, so the NtCreateUserProcess it funnels
/// into is not counted as a direct spawn. Cleared on drop, even if the original unwinds.
struct SpawningGuard;
impl Drop for SpawningGuard {
    fn drop(&mut self) {
        SPAWNING.set(false);
    }
}
fn enter_spawning() -> SpawningGuard {
    SPAWNING.set(true);
    SpawningGuard
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn h_ntcup(
    process_handle: *mut c_void,
    thread_handle: *mut c_void,
    process_access: u32,
    thread_access: u32,
    process_obj_attr: *mut c_void,
    thread_obj_attr: *mut c_void,
    process_flags: u32,
    thread_flags: u32,
    process_params: *mut c_void,
    create_info: *mut c_void,
    attr_list: *mut c_void,
) -> i32 { unsafe {
    let o = match O_NTCUP.get() {
        Some(o) => o,
        // Unreachable (O_NTCUP is set before enable_all_hooks), but unlike a wait detour, returning
        // STATUS_SUCCESS (0) here would be a FAKE spawn success - the caller would use uninitialized
        // handles and crash. Fail loudly with STATUS_UNSUCCESSFUL instead.
        None => return STATUS_UNSUCCESSFUL,
    };
    // Count only a direct call, not the CreateProcess* funnel (already inherited). Never inject.
    if !SPAWNING.get() {
        bump(IDX_NTCUP);
    }
    o(
        process_handle,
        thread_handle,
        process_access,
        thread_access,
        process_obj_attr,
        thread_obj_attr,
        process_flags,
        thread_flags,
        process_params,
        create_info,
        attr_list,
    )
}}

// --- Child inheritance (ADR-3) --------------------------------------------------
// Detour CreateProcessW so children join the session: create suspended, inject the
// same DLL, then resume (unless the caller wanted it suspended). The child opens the
// shared Ctl and hooks itself, so it sees the same wall clock as the parent.

/// Inject this DLL into `hproc` by writing our own module path and running
/// LoadLibraryW there. Returns whether the DLL actually loaded there.
///
/// Best-effort in the sense that a failure never stops the child: this runs inside somebody else's
/// application, which asked for that process, so killing it (what `mech::prepare` does for the target
/// it launched itself) would change the behaviour under test. What the failure MUST do is get counted,
/// so the audit can say a child ran on the real clock instead of quietly reporting a smaller family
/// (R2-S2, untouchable rule 4).
///
/// # Safety
/// `hproc` must be a valid process handle with injection rights.
unsafe fn inject_self(hproc: HANDLE) -> bool { unsafe {
    let addr = *SELF_HMOD.get().unwrap_or(&0);
    if addr == 0 {
        return false;
    }
    let hmod = HMODULE(addr as *mut c_void);
    // GetModuleFileNameW returns the char count WITHOUT the NUL on success, or the buffer length on
    // truncation (ERROR_INSUFFICIENT_BUFFER) - it never says how much room it needed. A single MAX_PATH
    // buffer therefore silently disabled child inheritance for any install whose path reached 260 chars,
    // which is reachable for a portable tool (a long user name plus an unpacked release folder plus
    // the core folder and the hook DLL gets close on its own, and a network share goes past). Grow up
    // to the Win32 extended-path limit, so the length of a folder name is not what decides whether the
    // audit covers a child.
    let mut buf = vec![0u16; 260];
    let n = loop {
        let n = GetModuleFileNameW(Some(hmod), &mut buf) as usize;
        if n == 0 {
            log("[chrono_hook] GetModuleFileNameW failed, child not injected");
            return false;
        }
        if n < buf.len() {
            break n;
        }
        if buf.len() >= 32_768 {
            // Past the extended-path maximum: give up, but SAY so - the audit will report the child as
            // uncovered, and without this line nobody could tell why (rule 6).
            log("[chrono_hook] own module path exceeds the path limit, child not injected");
            return false;
        }
        buf.resize(buf.len() * 4, 0);
    };
    let bytes = (n + 1) * 2; // include the NUL terminator
    let remote = VirtualAllocEx(hproc, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if remote.is_null() {
        return false;
    }
    if WriteProcessMemory(hproc, remote, buf.as_ptr() as *const c_void, bytes, None).is_err() {
        let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
        return false;
    }
    let k32 = match GetModuleHandleA(s!("kernel32.dll")) {
        Ok(h) => h,
        Err(_) => {
            let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
            return false;
        }
    };
    // Check the export before transmuting (L-5, matching mech::inject): a None from GetProcAddress would
    // otherwise transmute to a null start routine and CreateRemoteThread would run at address 0, faulting
    // the child. LoadLibraryW is always present in kernel32, so this only bails on a genuine anomaly.
    let loadlib = match GetProcAddress(k32, s!("LoadLibraryW")) {
        Some(f) => f,
        None => {
            let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
            return false;
        }
    };
    let start: LPTHREAD_START_ROUTINE = Some(std::mem::transmute::<
        unsafe extern "system" fn() -> isize,
        unsafe extern "system" fn(*mut c_void) -> u32,
    >(loadlib));
    // The remote thread's exit code is the low 32 bits of the HMODULE LoadLibraryW returned - 0 means
    // the DLL did not load - a child of the other bitness being the ordinary reason. Same reading as
    // `mech::inject` (H-2), which is where this check was already made and this one was missing.
    let mut loaded = false;
    // Whether LoadLibraryW is PROVABLY finished with the path buffer below. Nothing else may free it:
    // `remote` holds the very string LoadLibraryW is reading, so releasing it while that call is still
    // in flight is a use-after-free inside somebody else's application - the nondeterministic crash
    // that is the worst outcome this tool can produce. With no thread created, nothing is reading it.
    let mut thread_done = true;
    if let Ok(hthread) =
        CreateRemoteThread(hproc, None, 0, start, Some(remote as *const c_void), 0, None)
    {
        // Finite wait (RELEASE-009): a child wedged in loader lock must not hang the parent's detour.
        wait_raw(hthread, CHILD_INJECT_TIMEOUT_MS);
        // A timeout leaves `loaded` false: we did not establish that the hook is there, and claiming
        // coverage we have not established is the one thing the audit may never do (rule 4).
        let mut code: u32 = 0;
        let read = GetExitCodeThread(hthread, &mut code).is_ok();
        // Anything we could not read counts as "still running": an unknown thread state is not
        // permission to pull the memory out from under it.
        thread_done = read && code != STILL_ACTIVE_CODE;
        loaded = thread_done && code != 0;
        let _ = CloseHandle(hthread);
    }
    if thread_done {
        let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
    } else {
        // Deliberately leaked: about half a kilobyte of committed memory in a child that is already
        // wedged in its own loader, against the chance of faulting it. Said out loud rather than done
        // quietly (rule 6) - and this is exactly why the timeout above is generous. Shortening it does
        // not make anything faster in the ordinary case (measured: child injection costs about 68 ms,
        // and the limit is 10 s) - it only makes THIS branch, and the leak, more likely to be reached.
        log("[chrono_hook] LoadLibraryW still running in the child - leaving its path buffer allocated");
    }
    if !loaded {
        log("[chrono_hook] child not covered - LoadLibraryW did not load the hook there");
    }
    loaded
}}

/// After a create call we forced to CREATE_SUSPENDED returns, inject the hook into the
/// new child so it joins the session, then resume it unless the caller originally asked
/// for a suspended child. Shared by the CreateProcessW and CreateProcessA detours.
///
/// # Safety
/// `pi`, when non-null, must point to a PROCESS_INFORMATION filled by a successful create.
unsafe fn inherit_into_child(r: i32, pi: *mut PROCESS_INFORMATION, want_suspended: bool) { unsafe {
    if r != 0 && !pi.is_null() {
        let info = *pi;
        if !inject_self(info.hProcess) {
            // Record it in OUR slot: the child never reserved one and never will, so without this the
            // process simply would not appear anywhere in the audit (R2-S2). The mechanism turns a
            // non-zero count into `inheritance.child_not_injected`.
            if let Some(c) = cov_ptr() {
                bump_uninjected_children(c);
            }
        }
        // Resume regardless. The parent is the application under test and it asked for this child -
        // holding it suspended or killing it would change the behaviour we were asked to observe.
        if !want_suspended {
            let _ = ResumeThread(info.hThread);
        }
    }
}}

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
) -> i32 { unsafe {
    let o = match O_CPW.get() {
        Some(o) => o,
        None => return 0,
    };
    let want_suspended = (flags & CREATE_SUSPENDED.0) != 0;
    // SPAWNING held across the original: the NtCreateUserProcess it funnels into is not counted as a
    // direct spawn (this child is already being inherited below). Cleared before inherit_into_child.
    let r = {
        let _g = enter_spawning();
        o(app, cmd, pa, ta, inherit, flags | CREATE_SUSPENDED.0, env, cwd, si, pi)
    };
    inherit_into_child(r, pi, want_suspended);
    r
}}

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
) -> i32 { unsafe {
    let o = match O_CPA.get() {
        Some(o) => o,
        None => return 0,
    };
    let want_suspended = (flags & CREATE_SUSPENDED.0) != 0;
    let r = {
        let _g = enter_spawning();
        o(app, cmd, pa, ta, inherit, flags | CREATE_SUSPENDED.0, env, cwd, si, pi)
    };
    inherit_into_child(r, pi, want_suspended);
    r
}}

/// Diagnostics only (stderr-equivalent for an injected DLL) - never affects coverage.
fn log(msg: &str) {
    if let Ok(c) = CString::new(msg) {
        unsafe { OutputDebugStringA(PCSTR(c.as_ptr() as *const u8)) }
    }
}

/// Resolve, create, and record one channel's detour. Best-effort: a missing export
/// or a failed hook logs and leaves the bit unset (honest partial), never aborts the
/// rest. The export name and module come from `CHANNELS[idx]` - single source.
///
/// The bit goes into `pending`, NOT into this process's `Cov`: `MinHook::create_hook` only
/// PREPARES a trampoline, and the detour goes live only at `enable_all_hooks`. `install`
/// publishes `pending` to the `Cov` after that call succeeds, so a target whose hooks were
/// prepared but never enabled (an AV blocking the code-section write, a CFG conflict) reports
/// zero covered channels instead of a full set that never ran - rule 4, the audit never claims
/// a channel it did not cover.
///
/// # Safety
/// `detour` must be correct for `slot`.
unsafe fn make_hook<T: Copy>(
    pending: &mut u64,
    k32: HMODULE,
    ntdll: HMODULE,
    idx: usize,
    detour: *mut c_void,
    slot: &OnceLock<T>,
) { unsafe {
    let ch = &CHANNELS[idx];
    let module = match ch.module {
        ChannelModule::Kernel32 => k32,
        ChannelModule::Ntdll => ntdll,
        // user32 may be absent in a console/service target - resolve it here rather than force-load it
        // (forcing a DLL the target never needed would change its behavior). Absent -> honest partial.
        ChannelModule::User32 => match GetModuleHandleA(s!("user32.dll")) {
            Ok(h) => h,
            Err(_) => {
                log(&format!("[chrono_hook] user32 not loaded, skipping: {}", ch.name));
                return;
            }
        },
        // winmm may be absent in a console/service target - resolve it here rather than force-load it
        // (forcing a DLL the target never needed would change its behavior). Absent -> honest partial.
        ChannelModule::Winmm => match GetModuleHandleA(s!("winmm.dll")) {
            Ok(h) => h,
            Err(_) => {
                log(&format!("[chrono_hook] winmm not loaded, skipping: {}", ch.name));
                return;
            }
        },
        // ws2_32 may be absent in a target that never touches the network - resolve it here rather than
        // force-load it (forcing a DLL the target never needed would change its behavior). Absent -> honest partial.
        ChannelModule::Ws2_32 => match GetModuleHandleA(s!("ws2_32.dll")) {
            Ok(h) => h,
            Err(_) => {
                log(&format!("[chrono_hook] ws2_32 not loaded, skipping: {}", ch.name));
                return;
            }
        },
        // kernelbase is present in every Win32 process (kernel32 is built on it), so this lookup is
        // not the lazy "maybe absent" case the three above are - it is where the exports actually
        // live. A failure here is a real gap, and the audit reports it as one rather than dropping
        // the channel, because the target certainly CAN call these.
        ChannelModule::KernelBase => match GetModuleHandleA(s!("kernelbase.dll")) {
            Ok(h) => h,
            Err(_) => {
                log(&format!("[chrono_hook] kernelbase not loaded, skipping: {}", ch.name));
                return;
            }
        },
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
            *pending |= ch.bit;
        }
        Err(e) => log(&format!("[chrono_hook] create_hook {} failed: {e:?}", ch.name)),
    }
}}

/// Install and enable every channel's detour, wiring this process to the shared anchor.
///
/// INVARIANT (P6, docs/06 ADR-3): injection assumes the target is SUSPENDED - the parent is created
/// `CREATE_SUSPENDED` and injected before its first thread runs, and children are forced
/// `CREATE_SUSPENDED` in `h_cpw`/`h_cpa` before self-injection. This runs from `DLL_PROCESS_ATTACH`
/// under the loader lock, and `MinHook::enable_all_hooks` suspends/resumes threads - safe ONLY while
/// no other application thread exists yet. Do NOT add a path that injects into an already-running,
/// multi-threaded process without moving hook-enabling off the loader lock (the watcher thread is
/// created OUTSIDE DllMain, in `ensure_watcher`, for exactly this reason).
unsafe fn install() -> Result<(), String> { unsafe {
    let hmap = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(chrono_ctl::CTL_SECTION_NAME_W.as_ptr()))
        .map_err(|e| format!("OpenFileMappingW: {e:?}"))?;
    let view = MapViewOfFile(hmap, FILE_MAP_ALL_ACCESS, 0, 0, chrono_ctl::ctl_size());
    if view.Value.is_null() {
        // Close the mapping we opened a line ago. Once the view is mapped the handle is deliberately
        // kept for the process's life (the view outlives it either way), but on this path there is no
        // view - the handle would just sit there for as long as the target runs.
        let _ = CloseHandle(hmap);
        return Err("MapViewOfFile returned null".into());
    }
    let ctl = view.Value as *mut Ctl;
    // Before ANY field is read as a time. The section name is fixed and creatable by any process in
    // this session, so what is mapped here is not guaranteed to be the block our mechanism wrote -
    // and every number below decides either what the target's clock says (untouchable rule 2) or
    // what the audit reports about it (rule 4). Refusing is the honest outcome: the target then runs
    // on the real clock and the driver reports the injection as failed, rather than the target
    // silently running on a stranger's anchor while the report calls the session covered.
    if !header_is_ours(ctl as *const Ctl) {
        let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
        let _ = CloseHandle(hmap);
        return Err("session control block is not this build's".into());
    }
    let _ = CTL_PTR.set(view.Value as usize);
    let _ = TZ_BIAS.set(read_tz_bias(ctl as *const Ctl));

    // Watch the core process so the target reverts to real time if the core vanishes.
    let core_pid = read_core_pid(ctl as *const Ctl);
    let _ = CORE_PID.set(core_pid); // the session we joined, for `still_ours` (R2-S6)
    if core_pid != 0
        && let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, core_pid) {
            let _ = CORE_HANDLE.set(h.0 as usize);
        }

    // This process's OWN coverage slot in the shared block, so its calls are attributed to it and
    // never summed into the parent's report (rule 4). Reserved NOW, before any detour is enabled,
    // because a detour that fires needs somewhere to count - the PID that advertises this slot is
    // published at the very end of install, once the slot holds the truth.
    //
    // The slot lives in `Ctl`, which the mechanism holds for the whole session, so this process's
    // evidence survives the process (S-9). It used to be a section named after our PID, kept alive
    // by our own handle alone, and a child shorter-lived than the mechanism's poll took its evidence
    // to the grave. That also means there is no CreateFileMapping call on this path any more, which
    // is work removed from DllMain and the loader lock.
    //
    // Best-effort: if the registry is full the detours still substitute time (they read the shared
    // anchor via CTL_PTR), but this process reports no coverage and publishes no PID - the mechanism
    // simply never sees it, and never fabricates coverage.
    let pid = GetCurrentProcessId();
    let cov_slot: Option<usize> = reserve_cov_slot(ctl);
    let cov: Option<*mut Cov> = match cov_slot {
        Some(slot) => {
            let cptr = cov_at_mut(ctl, slot);
            let _ = COV_PTR.set(cptr as usize);
            Some(cptr)
        }
        None => {
            log("[chrono_hook] PID registry full - this process runs uncovered in the audit");
            None
        }
    };

    // These two cannot realistically fail (both modules are mapped into every Win32 process before any
    // DLL of ours loads), but the `?` used to walk out past the coverage mapping created just above and
    // leak it. Name the failure and take the same exit as any other install error.
    let (k32, ntdll) = match (
        GetModuleHandleA(s!("kernel32.dll")),
        GetModuleHandleA(s!("ntdll.dll")),
    ) {
        (Ok(k), Ok(n)) => (k, n),
        (k, n) => {
            if let Some(c) = cov {
                set_channels_installed(c, 0);
            }

            return Err(format!("GetModuleHandleA: kernel32 {k:?}, ntdll {n:?}"));
        }
    };

    // Channels whose detour was CREATED. Published to the Cov only after enable_all_hooks succeeds -
    // until then no detour is live, and a bit that says otherwise would be the audit lying (rule 4).
    let mut pending: u64 = 0;

    make_hook(&mut pending, k32, ntdll, IDX_GSTAFT, h_gstaft as *const () as *mut c_void, &O_GSTAFT);
    make_hook(&mut pending, k32, ntdll, IDX_GSTPAFT, h_gstpaft as *const () as *mut c_void, &O_GSTPAFT);
    make_hook(&mut pending, k32, ntdll, IDX_GST, h_gst as *const () as *mut c_void, &O_GST);
    make_hook(&mut pending, k32, ntdll, IDX_GLT, h_glt as *const () as *mut c_void, &O_GLT);
    make_hook(&mut pending, k32, ntdll, IDX_NTQST, h_ntqst as *const () as *mut c_void, &O_NTQST);
    make_hook(&mut pending, k32, ntdll, IDX_NTQSI, h_ntqsi as *const () as *mut c_void, &O_NTQSI);
    make_hook(&mut pending, k32, ntdll, IDX_GTZI, h_gtzi as *const () as *mut c_void, &O_GTZI);
    make_hook(&mut pending, k32, ntdll, IDX_GDTZI, h_gdtzi as *const () as *mut c_void, &O_GDTZI);
    make_hook(&mut pending, k32, ntdll, IDX_STSL, h_stsl as *const () as *mut c_void, &O_STSL);
    make_hook(&mut pending, k32, ntdll, IDX_STSLEX, h_stslex as *const () as *mut c_void, &O_STSLEX);
    make_hook(&mut pending, k32, ntdll, IDX_FTLFT, h_ftlft as *const () as *mut c_void, &O_FTLFT);
    make_hook(&mut pending, k32, ntdll, IDX_LFTFT, h_lftft as *const () as *mut c_void, &O_LFTFT);
    make_hook(&mut pending, k32, ntdll, IDX_TLTST, h_tltst as *const () as *mut c_void, &O_TLTST);
    make_hook(&mut pending, k32, ntdll, IDX_TLTSTEX, h_tltstex as *const () as *mut c_void, &O_TLTSTEX);

    // Duration axis (opt-in). The anchor lives in the shared Ctl now: the core initialized it in
    // prepare (from the real GetTickCount64 / QUIT, before the target ran) and rebases it on every
    // set_multiplier, so a speed change never rewinds the axis (H-1). No per-process capture here - the
    // detours read it under the same seqlock as the wall multiplier. QPC stays real unless its own
    // opt-in asks otherwise (ADR-2) - timeGetTime rides this axis, sharing GetTickCount's base.
    if read_scale_dur(ctl as *const Ctl) {
        make_hook(&mut pending, k32, ntdll, IDX_GTC64, h_tick as *const () as *mut c_void, &O_TICK);
        make_hook(&mut pending, k32, ntdll, IDX_GTC, h_tick32 as *const () as *mut c_void, &O_TICK32);
        make_hook(&mut pending, k32, ntdll, IDX_QUIT, h_quit as *const () as *mut c_void, &O_QUIT);
        make_hook(&mut pending, k32, ntdll, IDX_SLEEP, h_sleep as *const () as *mut c_void, &O_SLEEP);
        make_hook(&mut pending, k32, ntdll, IDX_SLEEPEX, h_sleepex as *const () as *mut c_void, &O_SLEEPEX);
        make_hook(&mut pending, k32, ntdll, IDX_NTDELAY, h_ntdelay as *const () as *mut c_void, &O_NTDELAY);
        make_hook(&mut pending, k32, ntdll, IDX_WFSO, h_wfso as *const () as *mut c_void, &O_WFSO);
        make_hook(&mut pending, k32, ntdll, IDX_WFSOEX, h_wfsoex as *const () as *mut c_void, &O_WFSOEX);
        make_hook(&mut pending, k32, ntdll, IDX_WFMO, h_wfmo as *const () as *mut c_void, &O_WFMO);
        make_hook(&mut pending, k32, ntdll, IDX_WFMOEX, h_wfmoex as *const () as *mut c_void, &O_WFMOEX);
        make_hook(&mut pending, k32, ntdll, IDX_SOAW, h_soaw as *const () as *mut c_void, &O_SOAW);
        make_hook(&mut pending, k32, ntdll, IDX_MWFMO, h_mwfmo as *const () as *mut c_void, &O_MWFMO);
        make_hook(&mut pending, k32, ntdll, IDX_MWFMOEX, h_mwfmoex as *const () as *mut c_void, &O_MWFMOEX);
        // The four waits that used to be neither scaled nor observed. They ride the same opt-in as the
        // rest of the wait family: a session that did not ask for the duration axis is not watching
        // waits at all. Three resolve in kernelbase and the socket one in ws2_32, which is optional and
        // therefore also in the late scan below (ADR-10) - without that entry this would repeat exactly
        // the gap ADR-10 was written to close.
        make_hook(&mut pending, k32, ntdll, IDX_SCVSRW, h_scvsrw as *const () as *mut c_void, &O_SCVSRW);
        make_hook(&mut pending, k32, ntdll, IDX_SCVCS, h_scvcs as *const () as *mut c_void, &O_SCVCS);
        make_hook(&mut pending, k32, ntdll, IDX_WOA, h_woa as *const () as *mut c_void, &O_WOA);
        make_hook(&mut pending, k32, ntdll, IDX_WSAWFME, h_wsawfme as *const () as *mut c_void, &O_WSAWFME);
        make_hook(&mut pending, k32, ntdll, IDX_SWT, h_swt as *const () as *mut c_void, &O_SWT);
        make_hook(&mut pending, k32, ntdll, IDX_SWTEX, h_swtex as *const () as *mut c_void, &O_SWTEX);
        make_hook(&mut pending, k32, ntdll, IDX_SETTIMER, h_settimer as *const () as *mut c_void, &O_SETTIMER);
        make_hook(&mut pending, k32, ntdll, IDX_TIMESETEVENT, h_timesetevent as *const () as *mut c_void, &O_TIMESETEVENT);
        make_hook(&mut pending, k32, ntdll, IDX_TIMEGETTIME, h_timegettime as *const () as *mut c_void, &O_TIMEGETTIME);
        make_hook(&mut pending, k32, ntdll, IDX_TPTIMER, h_set_tp_timer as *const () as *mut c_void, &O_TPTIMER);
        make_hook(&mut pending, k32, ntdll, IDX_TPTIMEREX, h_set_tp_timer_ex as *const () as *mut c_void, &O_TPTIMEREX);
    }

    // QPC axis (opt-in `scale_qpc`, ADR-2 reversal). SEPARATE from scale_duration because scaling QPC also
    // scales a target's QPC-timed rendering. It IS a coverage channel now (R2-S3): installed through
    // make_hook like the rest, so a failure to install shows up as an uncovered channel instead of a
    // debug string nobody reads. The anchor lives in the shared Ctl (core initialized it in prepare,
    // rebases on set_multiplier). QueryPerformanceFrequency is left real.
    if read_scale_qpc(ctl as *const Ctl) {
        make_hook(&mut pending, k32, ntdll, IDX_QPC, h_qpc as *const () as *mut c_void, &O_QPC);
    }

    // Direct process creation (ADR-3, observed): hook NtCreateUserProcess ALWAYS - not gated by
    // scale_duration, since process creation is watched regardless. It only counts a direct call and
    // forwards untouched - the SPAWNING guard keeps the CreateProcess* funnel from counting here.
    make_hook(&mut pending, k32, ntdll, IDX_NTCUP, h_ntcup as *const () as *mut c_void, &O_NTCUP);

    // Suspected time source (Etap 2, observed): hook ws2_32 connect ALWAYS - the network is watched
    // regardless of scale_duration. It only counts a connection (a suspected server time source we cannot
    // cover) and forwards untouched - the audit warns source.network_at_start.
    make_hook(&mut pending, k32, ntdll, IDX_CONNECT, h_connect as *const () as *mut c_void, &O_CONNECT);

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

    // Enable every prepared detour, THEN publish what is actually live. Until this call returns Ok,
    // the trampolines exist but no detour runs, so nothing may be claimed as covered. If it fails
    // (an AV blocking the write to the code section, a CFG conflict, another hooking library in the
    // process) the target keeps running on real time - DllMain cannot undo a load - so the honest
    // report is zero covered channels, which the mechanism turns into a failing verdict rather than
    // a silent "works" over a session that substituted nothing (rule 4).
    if let Err(e) = MinHook::enable_all_hooks() {
        if let Some(c) = cov {
            // Explicit, not merely "we never wrote": a reader that somehow saw this slot must read
            // zero covered channels, not a claim we cannot back. We also return without publishing
            // our PID, so the mechanism never looks at the slot at all.
            set_channels_installed(c, 0);
        }
        return Err(format!("enable_all_hooks: {e:?}"));
    }
    if let Some(c) = cov {
        set_channels_installed(c, pending);
    }

    // Hand the watcher whatever this session WANTED from an optional module and did not get. The set
    // is computed here rather than in the watcher so the opt-in gates are stated once: without
    // scale_duration the duration and observed-time channels are not wanted at all, and looking for
    // them later would install channels the session deliberately did not ask for. `connect` is not
    // gated - the network is watched regardless.
    //
    // `INSTALL_DONE` is released LAST, after the mask store above, and that ordering is the whole
    // point of the flag (R1): until it is set the watcher will not touch the Cov, so a late bit
    // cannot be ORed in and then wiped by our own store.
    let wanted_late =
        if read_scale_dur(ctl as *const Ctl) { LATE_CHANNELS } else { WS2_32_CONNECT_LATE };
    LATE_TODO.store(wanted_late & !pending, Ordering::Relaxed);
    INSTALL_DONE.store(true, Ordering::Release);

    // Publish our PID LAST - after the coverage slot holds the installed mask and the detours are
    // live - so the mechanism never reads a pid whose slot is not yet filled in. Only if we actually
    // reserved a slot to report (best-effort above).
    if let Some(slot) = cov_slot {
        publish_pid(ctl, slot, pid);
    }
    Ok(())
}}

#[unsafe(no_mangle)]
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
