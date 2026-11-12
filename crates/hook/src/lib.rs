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
//! Child processes inherit the session via `CreateProcessW` / `CreateProcessA` detours on kernel32 and
//! the `CreateProcessInternalW` funnel in kernelbase under them (ADR-3, R4/11). A spawn that passes
//! none of them - a user token (`CreateProcessAsUserW`), or `NtCreateUserProcess` called straight - is
//! OBSERVED, not injected:
//! counted and warned (its child may be uncovered), never self-injected - that would mean
//! manipulating undocumented native structures for near-zero real value. A thread-local guard keeps
//! the CreateProcess* funnel to NtCreateUserProcess from counting as a direct spawn.

#![allow(non_snake_case)]

use std::cell::Cell;
use std::ffi::{c_void, CString};
use std::thread::LocalKey;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;

use chrono_ctl::{
    anchor_write_in_progress, bump_calls, bump_uninjected_children, clock_fence, cov_at_mut, find_pid_slot,
    header_is_ours, indirect_jump_slot, record_uncovered_child, release_axes, release_margin_qpc, ReleasedAxes,
    publish_pid, read_anchor, read_anchor_with, read_core_created, read_core_pid, read_dur, read_dur_with,
    read_ended, read_qpc, read_qpc_with, RELEASE_MARGIN_QUIT,
    read_pid_count, read_scale_dur, read_scale_qpc, set_created, set_subsystem, image_subsystem, MAX_COV_PIDS,
    IMAGE_HEADER_SPAN, IDX_GTZIFY,
    bump_waits_at_floor, delay_hit_floor, read_installed, read_late_installed, read_tz_bias, reserve_cov_slot,
    scale_delay_deadline, scale_delay_interval, scale_timer_due, scale_timer_elapse, scale_timer_period,
    scale_timer_period_ms, scale_wait, set_channels_installed, set_failed_channels, set_late_installed,
    wait_hit_floor, bump_timers_at_floor, bump_calls_without_storage, timer_elapse_hit_floor, timer_period_hit_floor, ChannelModule, Cov,
    Ctl, CHANNELS, IDX_GDTZI, IDX_GLT, IDX_GST, IDX_GSTAFT, IDX_GSTPAFT, IDX_GTC, IDX_GTC64,
    IDX_GTZI, IDX_NTDELAY, IDX_NTQSI, IDX_NTQST, IDX_QUIT, IDX_SLEEP, IDX_SLEEPEX, IDX_STSL,
    IDX_STSLEX, IDX_FTLFT, IDX_LFTFT, IDX_TLTST, IDX_TLTSTEX, IDX_WFSO, IDX_WFSOEX, IDX_WFMO,
    IDX_WFMOEX, IDX_SOAW, IDX_MWFMO, IDX_MWFMOEX, IDX_SWT, IDX_SWTEX, IDX_SETTIMER, IDX_TIMESETEVENT,
    IDX_TPTIMER, IDX_TPTIMEREX, IDX_NTCUP, IDX_CONNECT, IDX_QPC, IDX_TIMEGETTIME, IDX_SCVSRW,
    IDX_SCVCS, IDX_WOA, IDX_WSAWFME, IDX_QIT, IDX_QITP, IDX_QUITP, LATE_CHANNELS, bump_absolute_dues, sleep_bias,
    abandon_load_gate, close_load_gate, mark_exit_began, open_load_gate, pass_load_gate, read_pid, LoadGate,
};
use chrono_ctl::inject::{self as remote_load, budget_left_ms, INJECT_TIMEOUT_MS, STILL_ACTIVE_CODE};
use minhook::{MinHook, MH_STATUS};
use windows::core::{s, PCSTR, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, SetLastError, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, ERROR_INVALID_PARAMETER,
    FILETIME, HANDLE,
    HMODULE, SYSTEMTIME, WAIT_FAILED,
};
use windows::Win32::System::Diagnostics::Debug::{OutputDebugStringA, WriteProcessMemory};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleA, GetModuleHandleExA, GetModuleHandleW, GetProcAddress,
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
};
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualAllocEx, VirtualFreeEx,
    FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
    PAGE_READWRITE,
};
use windows::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};
use windows::Win32::System::SystemInformation::{IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_UNKNOWN};
use windows::Win32::System::Threading::{
    CreateEventW, CreateRemoteThread, CreateThread, GetCurrentProcess, GetCurrentProcessId, GetCurrentThread, GetCurrentThreadId,
    GetExitCodeProcess, GetExitCodeThread, GetProcessId, GetProcessTimes, IsWow64Process2, OpenProcess, ResumeThread,
    SetEvent, SuspendThread, WaitForMultipleObjects, WaitForSingleObject, CREATE_SUSPENDED, DEBUG_ONLY_THIS_PROCESS, DEBUG_PROCESS,
    INFINITE, LPTHREAD_START_ROUTINE, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, THREAD_CREATION_FLAGS,
};
use windows::Win32::System::Time::{
    FileTimeToSystemTime, SystemTimeToFileTime, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_INFORMATION,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::WindowsProgramming::{QueryInterruptTime, QueryUnbiasedInterruptTime};

type FtFn = unsafe extern "system" fn(*mut FILETIME);
type StFn = unsafe extern "system" fn(*mut SYSTEMTIME);
type NtqstFn = unsafe extern "system" fn(*mut i64) -> i32;
type TziFn = unsafe extern "system" fn(*mut TIME_ZONE_INFORMATION) -> u32;
type DtziFn = unsafe extern "system" fn(*mut DYNAMIC_TIME_ZONE_INFORMATION) -> u32;
type TziForYearFn =
    unsafe extern "system" fn(u16, *const DYNAMIC_TIME_ZONE_INFORMATION, *mut TIME_ZONE_INFORMATION) -> i32;
type StslFn = unsafe extern "system" fn(*const TIME_ZONE_INFORMATION, *const SYSTEMTIME, *mut SYSTEMTIME) -> i32;
type StslexFn = unsafe extern "system" fn(*const DYNAMIC_TIME_ZONE_INFORMATION, *const SYSTEMTIME, *mut SYSTEMTIME) -> i32;
type FtConvFn = unsafe extern "system" fn(*const FILETIME, *mut FILETIME) -> i32;
type TickFn = unsafe extern "system" fn() -> u64;
type Tick32Fn = unsafe extern "system" fn() -> u32;
type QuitFn = unsafe extern "system" fn(*mut u64) -> i32;
// QueryInterruptTime, QueryInterruptTimePrecise and QueryUnbiasedInterruptTimePrecise (kernelbase only):
// `void (PULONGLONG)`, no BOOL - the Windows SDK declaration, as the windows crate carries it (R5-F4).
type InterruptFn = unsafe extern "system" fn(*mut u64);
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
// NtDeviceIoControlFile(FileHandle, Event, ApcRoutine, ApcContext, IoStatusBlock, IoControlCode,
// InputBuffer, InputBufferLength, OutputBuffer, OutputBufferLength) -> NTSTATUS (ntdll, documented in
// winternl.h). The connection observer: we read the control code, a plain number, and COUNT a
// connection attempt (a suspected server time source) - every other argument is forwarded untouched and
// never dereferenced, so no undocumented structure of the socket driver is ever parsed here.
type NtDeviceIoControlFileFn = unsafe extern "system" fn(
    HANDLE,
    HANDLE,
    *const c_void,
    *const c_void,
    *mut c_void,
    u32,
    *const c_void,
    u32,
    *mut c_void,
    u32,
) -> i32;
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
// CreateProcessInternalW (kernelbase, undocumented): CreateProcessW with the user token in front and an
// out token at the end. The layout every caller of the export uses, and the one the direct-spawn probe
// has called on both bitnesses since 2026-08: (token, app, cmd, pa, ta, inherit, flags, env, cwd, si, pi,
// new_token).
type CpiwFn = unsafe extern "system" fn(
    HANDLE,
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
    *mut HANDLE,
) -> i32;

static CTL_PTR: OnceLock<usize> = OnceLock::new();
static COV_PTR: OnceLock<usize> = OnceLock::new();
/// The registry slot `COV_PTR` points into, which the exit mark checks still names this process.
static COV_SLOT: OnceLock<usize> = OnceLock::new();
static TZ_BIAS: OnceLock<i32> = OnceLock::new();

static O_GSTAFT: OnceLock<FtFn> = OnceLock::new();
static O_GSTPAFT: OnceLock<FtFn> = OnceLock::new();
static O_GST: OnceLock<StFn> = OnceLock::new();
static O_GLT: OnceLock<StFn> = OnceLock::new();
static O_NTQST: OnceLock<NtqstFn> = OnceLock::new();
static O_NTQSI: OnceLock<NtQsiFn> = OnceLock::new();
static O_GTZI: OnceLock<TziFn> = OnceLock::new();
static O_GDTZI: OnceLock<DtziFn> = OnceLock::new();
static O_GTZIFY: OnceLock<TziForYearFn> = OnceLock::new();
static O_STSL: OnceLock<StslFn> = OnceLock::new();
static O_STSLEX: OnceLock<StslexFn> = OnceLock::new();
static O_FTLFT: OnceLock<FtConvFn> = OnceLock::new();
static O_LFTFT: OnceLock<FtConvFn> = OnceLock::new();
static O_TLTST: OnceLock<StslFn> = OnceLock::new();
static O_TLTSTEX: OnceLock<StslexFn> = OnceLock::new();
static O_TICK: OnceLock<TickFn> = OnceLock::new();
static O_TICK32: OnceLock<Tick32Fn> = OnceLock::new();
// The kernelbase copies of the two tick counts (`ChannelModule::KernelBaseAndKernel32`). Separate
// trampolines, because each detour falls back to the body it replaced.
static O_TICK_KB: OnceLock<TickFn> = OnceLock::new();
static O_TICK32_KB: OnceLock<Tick32Fn> = OnceLock::new();
static O_QUIT: OnceLock<QuitFn> = OnceLock::new();
// The three interrupt-time clocks (R5-F4), hooked under scale_duration.
static O_QIT: OnceLock<InterruptFn> = OnceLock::new();
static O_QITP: OnceLock<InterruptFn> = OnceLock::new();
static O_QUITP: OnceLock<InterruptFn> = OnceLock::new();
/// `QueryUnbiasedInterruptTimePrecise` as kernelbase exports it, resolved in `install` before any detour
/// exists - the precise clock the anchors stand on, for a session that does not hook it (R5-P2). Not an
/// import: once the channel IS hooked, a direct call would land in our own detour, which is why
/// `real_quitp` prefers the trampoline, exactly as `real_quit` does.
static QUITP_EXPORT: OnceLock<InterruptFn> = OnceLock::new();
/// Whether this session scales the duration axis, read once in `install` (R5-D2). The deadline detours
/// stand in every session now and ask it on every call: without it they convert only an absolute deadline.
static SCALE_DUR: AtomicBool = AtomicBool::new(false);
/// The largest sleep bias this process has answered with, so the interrupt-time channels never step back on
/// a bias read as a lower bound (`chrono_ctl::sleep_bias`, R5-F4).
static SLEEP_BIAS_SEEN: AtomicI64 = AtomicI64::new(0);
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
static O_NTDIOCF: OnceLock<NtDeviceIoControlFileFn> = OnceLock::new();

// Child inheritance (ADR-3): our own module handle (to inject the same DLL into a
// child) and the CreateProcessW trampoline.
static SELF_HMOD: OnceLock<usize> = OnceLock::new();
static O_CPW: OnceLock<CpwFn> = OnceLock::new();
static O_CPA: OnceLock<CpaFn> = OnceLock::new();
static O_CPIW: OnceLock<CpiwFn> = OnceLock::new();

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
/// The trampoline alone stopped being enough when the waits moved into kernelbase: there, 64-bit
/// WaitForSingleObject is two instructions ending in a jump to the EXPORTED WaitForSingleObjectEx,
/// which is hooked too. Without the flag `unobserved` raises, the watcher's startup polling (a wait
/// every 20 ms while modules may still arrive) reached the audit as waits the application never made:
/// measured on a probe that waits on nothing, WaitForSingleObjectEx = 30.
///
/// # Safety
/// `h` must be a valid handle to wait on.
unsafe fn wait_raw(h: HANDLE, ms: u32) -> u32 { unsafe {
    unobserved(|| match O_WFSO.get() {
        Some(o) => o(h, ms),
        None => WaitForSingleObject(h, ms).0,
    })
}}

// --- Self-detach: let go of the target when the core vanishes -------------------
// The core writes its PID into the control block - we open a SYNCHRONIZE handle to it.
// On the first time call we spawn a watcher that blocks on that handle. When the core
// dies (clean end, crash, or kill -9) the OS signals it, we flip DETACHED, and the
// target is let go: the wall clock and the zone return to the real ones, and the
// duration axes carry on at rate 1 from where they stood (`RELEASED`), because handing
// back the real value there would rewind them (untouchable rule 3, measured 2026-09-24).

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

/// The first interval after the startup burst is over. It doubles from here up to
/// [`LATE_POLL_CEILING_MS`].
const LATE_POLL_SLOW_MS: u32 = 250;

/// The longest interval, kept for the rest of the session once reached (R5, WY13). A target that never
/// loads one of the late modules - a console program never loads user32 - looks for it as long as the
/// session lasts, and at a flat 250 ms that was four wake-ups a second for hours. The price is a module
/// pulled in late being hooked up to a second after it arrives instead of a quarter, a window the
/// audit already reports, since its counts are floors.
const LATE_POLL_CEILING_MS: u32 = 1_000;

/// How many fast looks before backing off - 100 x 20 ms covers the first two seconds of the target's
/// life. A target that pulls in a module later than that (a plugin loaded on demand) is still picked
/// up, just at the backed-off interval, where a wake-up costs at most four a second instead of fifty.
const LATE_FAST_TRIES: u32 = 100;

/// How long the watcher waits before its next look for a late module, after `tries` looks: the fast
/// interval through the startup burst, then 250, 500 and 1 000 ms, and the ceiling from there on.
fn late_poll_interval(tries: u32) -> u32 {
    let Some(slow) = tries.checked_sub(LATE_FAST_TRIES) else {
        return LATE_POLL_FAST_MS;
    };
    // The shift is capped, so it cannot overflow however long the session runs.
    LATE_POLL_SLOW_MS.saturating_mul(1 << slow.min(4)).min(LATE_POLL_CEILING_MS)
}

/// Watch the core AND look for modules that arrive after `DllMain`.
///
/// These two jobs share one loop rather than one thread each, because they are the same wait: the
/// watcher already blocks on the core's handle, and a bounded wait on that handle is both "has the
/// core died" and "time to look again". A second thread in the target would buy nothing and cost a
/// stack.
///
/// The loop degenerates to the original INFINITE block as soon as `late_scan` reports nothing left to
/// settle, which for a target whose modules were all present at `DllMain` is the FIRST look - so that
/// case pays no timed wait at all. A target that never loads one of them keeps looking for the whole
/// session, backing off to one look a second (`late_poll_interval`).
unsafe extern "system" fn watcher_proc(_p: *mut c_void) -> u32 { unsafe {
    if let Some(&h) = CORE_HANDLE.get() {
        let handle = HANDLE(h as *mut c_void);
        let mut tries: u32 = 0;
        let mut said = false;
        loop {
            // Look FIRST, wait second. The watcher is spawned from the first detour, which for most
            // targets fires before `main` - so by the time the loop is running the modules a target
            // pulls in early may already be there, and sleeping before the first look would hand
            // back a whole interval for nothing.
            late_scan();
            let interval = if late_pending() { late_poll_interval(tries) } else { INFINITE };
            tries = tries.saturating_add(1);
            match read_core_wait(wait_raw(handle, interval), || core_exit_code(handle)) {
                CoreWait::Running => {}
                CoreWait::Gone => break, // detach, and install nothing more
                CoreWait::Failed => {
                    if !said {
                        log("[chrono_hook] a wait on the core failed while the core runs - watching on");
                        said = true;
                    }
                    pause_watcher(WATCH_RETRY_MS);
                }
            }
        }
    }
    release_duration_axes();
    DETACHED.store(true, Ordering::SeqCst);
    0
}}

/// `WAIT_OBJECT_0` as the raw value the trampoline returns: the core's handle was signalled.
const WAIT_OBJECT_0_CODE: u32 = 0;

/// How long the watcher pauses after a wait that failed while the core was still running, before it
/// waits again. Long enough not to spin on a handle that keeps failing, short against a session.
const WATCH_RETRY_MS: u32 = 100;

/// What one wait on the core's handle says about the core (R4-N6).
#[derive(Debug, PartialEq, Eq)]
enum CoreWait {
    /// The interval ran out and the core is still there.
    Running,
    /// The core is gone: its handle was signalled, or the wait failed and nothing shows it running.
    Gone,
    /// The wait failed, yet the core's exit code says it is still running.
    Failed,
}

/// Read one wait on the core. Every result but a timeout used to count as the core's death, a FAILED
/// wait included, so a single failure let the target go back to the real clock under a session that
/// was still running. A failure now asks the core's exit code through the same handle, and only a core
/// that has one - or that cannot be asked - is gone.
fn read_core_wait(result: u32, exit_code: impl FnOnce() -> Option<u32>) -> CoreWait {
    match result {
        WAIT_TIMEOUT_CODE => CoreWait::Running,
        WAIT_OBJECT_0_CODE => CoreWait::Gone,
        _ if exit_code() == Some(STILL_ACTIVE_CODE) => CoreWait::Failed,
        _ => CoreWait::Gone,
    }
}

/// The core's exit code, or `None` when the handle cannot say. `STILL_ACTIVE_CODE` while it runs.
fn core_exit_code(handle: HANDLE) -> Option<u32> {
    let mut code: u32 = 0;
    unsafe { GetExitCodeProcess(handle, &mut code) }.ok().map(|()| code)
}

/// Pause the watcher for `ms` without a `Sleep`, which the hook counts and scales as the application's
/// own under `--scale-duration`. A wait on this thread's own handle cannot be signalled while the thread
/// runs, so it times out after `ms`, and `wait_raw` keeps it out of the audit.
fn pause_watcher(ms: u32) {
    unsafe {
        let _ = wait_raw(GetCurrentThread(), ms);
    }
}

/// Where the duration axes stood when the core went away, set once by the watcher. `None` for as long
/// as the session holds, and for good when the block had already been reclaimed by the time the watcher
/// read it - then the detours hand back the real value, as they all did before 2026-09-24.
static RELEASED: OnceLock<ReleasedAxes> = OnceLock::new();

/// The real QUIT from which this process's duration axes run at the real rate, 0 while the session holds
/// it (R4/10a, F6). Published by the watcher the moment it reads the clock, before anything else.
static RELEASE_AT_QUIT: AtomicI64 = AtomicI64::new(0);

/// The real QPC counterpart of [`RELEASE_AT_QUIT`].
static RELEASE_AT_QPC: AtomicI64 = AtomicI64::new(0);

/// Let the duration axes go, for good, before the flag that sends every detour to its released branch
/// goes up (untouchable rule 3, `chrono_ctl::release_axes`).
///
/// Runs on the watcher, once, after the core is gone - so nothing writes the anchors any more and the
/// read cannot race a rate change. What it can race is a NEW core reclaiming the block, which zeroes it:
/// ownership is checked after the read (R2-S6), and a block that is no longer ours releases nothing, so
/// the detours fall back to the real value. Unreachable today by the measurement kept at `still_ours`,
/// and stated because it is the one road left to the old snap-back.
///
/// What it does race is a detour that found the session holding and is answering from the anchor. Each
/// axis is let go at an instant a margin after the watcher's clock (`RELEASE_MARGIN_QUIT`), published
/// right after that clock is read, and the detour reads the published instant after its own clock: an
/// instant it sees, it obeys, computing the released line from its own snapshot of the anchor (which no
/// one writes any more), and one it does not see yet lies more than the margin after its clock. Until
/// 2026-10-01 the axes were let go at the watcher's clock itself, and a detour that had taken the session
/// branch a moment before answered at the session rate past it - the next read, on the released line,
/// was lower by that moment times the rate less one (R4/10a, F6). At a clean end the core has already
/// put the rate to 1, so the two lines were one there, and only a core that died left the window open.
fn release_duration_axes() {
    let Some(p) = ctl_ptr() else {
        return;
    };
    let p = p as *const Ctl;
    let released_at = real_quit().saturating_add(RELEASE_MARGIN_QUIT);
    RELEASE_AT_QUIT.store(released_at, Ordering::SeqCst);
    let mut frequency: i64 = 0;
    let _ = unsafe { QueryPerformanceFrequency(&mut frequency) };
    let qpc_released_at = real_qpc().saturating_add(release_margin_qpc(frequency));
    RELEASE_AT_QPC.store(qpc_released_at, Ordering::SeqCst);
    let (dur, qpc) = unsafe { (read_dur(p), read_qpc(p)) };
    if unsafe { anchor_write_in_progress(p) } {
        // The core died between the two halves of a rate change, so the reads above gave up waiting and
        // took the fields as they stand. The order of the stores keeps that mix running forward (R4-N5),
        // and this line is how anyone reading the log learns the release came from it.
        log("[chrono_hook] the core stopped in the middle of an anchor write - axes released from a partly written anchor");
    }
    if still_ours(p) {
        let _ = RELEASED.set(release_axes(dur, qpc, released_at, qpc_released_at));
    }
}

/// `GetTickCount64` after the session let go of this process, or `None` while it holds it (and when
/// nothing could be released).
fn released_tick() -> Option<u64> {
    RELEASED.get().map(|r| r.tick_at(real_quit()))
}

/// `QueryUnbiasedInterruptTime` after the session let go of this process, as `released_tick`.
fn released_quit() -> Option<i64> {
    RELEASED.get().map(|r| r.quit_at(real_quit()))
}

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
// the acceleration, not just in the audit), `timeSetEvent`, both message waits and the socket wait
// are observed. All six ride the `scale_duration` opt-in. The connection observer used to be the
// seventh, on ws2_32's `connect`, and left this list when it moved to ntdll, which is never late.
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

/// Channels whose module and export were there and whose detour could not be made or switched on, at
/// startup or in the late scan. Published to `Cov::failed_channels`, by `install` with the installed
/// mask and by the watcher after each scan that added to it.
///
/// Without it, a failed detour in an optional module read in the report exactly like a module the
/// application never loaded - no line at all - while the application called the real function. A
/// missing export is left out on purpose: then the application cannot call the function either.
static HOOK_FAILED: AtomicU64 = AtomicU64::new(0);

/// Has `install` published its coverage mask yet.
///
/// This is R1, and it is a real ordering hazard rather than a theoretical one: `install` enables the
/// detours BEFORE it stores `pending`, a detour calls `detached()`, and `detached()` is what spawns
/// this watcher. So the watcher can exist while the mask is still unwritten, and a late bit ORed in
/// during that gap would be erased by the plain volatile store that follows. The watcher simply does
/// not touch the Cov until this flag says the store has happened.
static INSTALL_DONE: AtomicBool = AtomicBool::new(false);

/// The slot a channel's trampoline goes into, whatever function type it holds - so the late channels
/// stand in one table ([`late_site`]) although each slot has a type of its own (R5-A11).
trait Trampoline: Sync {
    fn is_set(&self) -> bool;
    /// # Safety
    /// `original` must be the trampoline MinHook made for this slot's function.
    unsafe fn set_from(&self, original: *mut c_void);
}

impl<T: Copy + Send + Sync> Trampoline for OnceLock<T> {
    fn is_set(&self) -> bool {
        self.get().is_some()
    }

    unsafe fn set_from(&self, original: *mut c_void) {
        // SAFETY: the caller hands the trampoline for this slot's function type, a function pointer
        // the size of `*mut c_void`.
        let _ = self.set(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&original) });
    }
}

/// Each channel whose module may arrive after `DllMain`, with its detour and the slot for its
/// trampoline: the one table the late scan installs from (R5-A11). `None` for every other channel. A
/// test holds it to [`LATE_CHANNELS`] both ways, so a channel added to user32, winmm or ws2_32 without a
/// site here fails there instead of being owed for ever.
fn late_site(idx: usize) -> Option<(*mut c_void, &'static dyn Trampoline)> {
    let site: (*mut c_void, &'static dyn Trampoline) = match idx {
        IDX_MWFMO => (h_mwfmo as *const () as *mut c_void, &O_MWFMO),
        IDX_MWFMOEX => (h_mwfmoex as *const () as *mut c_void, &O_MWFMOEX),
        IDX_SETTIMER => (h_settimer as *const () as *mut c_void, &O_SETTIMER),
        IDX_TIMESETEVENT => (h_timesetevent as *const () as *mut c_void, &O_TIMESETEVENT),
        IDX_TIMEGETTIME => (h_timegettime as *const () as *mut c_void, &O_TIMEGETTIME),
        IDX_WSAWFME => (h_wsawfme as *const () as *mut c_void, &O_WSAWFME),
        _ => return None,
    };
    Some(site)
}

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
/// does. Reversing that would let a detour fire with no original to call (R4). A created detour goes
/// into `created` with its channel bit and address, for `enable_late`, and one that cannot be created
/// goes into `HOOK_FAILED`.
///
/// # Safety
/// `detour` must be correct for `slot`, and `module` must be a live, pinned module handle.
unsafe fn late_one(
    created: &mut Vec<(u64, usize)>,
    module: HMODULE,
    idx: usize,
    detour: *mut c_void,
    slot: &dyn Trampoline,
) { unsafe {
    let ch = &CHANNELS[idx];
    LATE_TODO.fetch_and(!ch.bit, Ordering::Relaxed);
    if slot.is_set() {
        return; // already installed at DllMain time - nothing owed here
    }
    let Ok(cname) = CString::new(ch.export()) else {
        log(&format!("[chrono_hook] late: bad channel name: {}", ch.name));
        return;
    };
    let Some(target) = GetProcAddress(module, PCSTR(cname.as_ptr() as *const u8)) else {
        log(&format!("[chrono_hook] late: no export: {}", ch.export()));
        return;
    };
    match MinHook::create_hook(target as *const () as *mut c_void, detour) {
        Ok(original) => {
            slot.set_from(original);
            created.push((ch.bit, target as *const () as usize));
        }
        Err(e) => {
            HOOK_FAILED.fetch_or(ch.bit, Ordering::Relaxed);
            log(&format!("[chrono_hook] late: create_hook {} failed: {e:?}", ch.name));
        }
    }
}}

/// One look for modules that arrived after `DllMain`, and an install for whatever is now reachable.
///
/// Ordering is the whole safety argument here (R2). Every module handle and every export address is
/// resolved, and every trampoline built, BEFORE a single hook is enabled - because `MH_EnableHook`
/// freezes all other threads, and calling into the loader while threads are frozen is how a hooking
/// library deadlocks a process. Enabling is the last step, one freeze for the whole batch and one per
/// detour only when the batch fails (`enable_late`), so a failure leaves the ones that went live
/// counted, the ones that did not recorded, and the ones installed at startup untouched (R4-W7).
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

    let failed_before = HOOK_FAILED.load(Ordering::Relaxed);
    let mut created: Vec<(u64, usize)> = Vec::new();
    // Each channel still owed, its module pinned by name and its detour from the one table. A module the
    // application has not loaded yet leaves its channels owed for the next look.
    for (idx, ch) in CHANNELS.iter().enumerate() {
        if todo & ch.bit == 0 {
            continue;
        }
        let Some(dll) = ch.module.late_dll() else { continue };
        let Some(m) = pin_module(PCSTR(dll.as_ptr())) else { continue };
        match late_site(idx) {
            Some((detour, slot)) => late_one(&mut created, m, idx, detour, slot),
            // A late channel with no site: a test holds this to never, and should it happen the channel
            // is reported as not covered rather than looked for at every poll for ever.
            None => {
                LATE_TODO.fetch_and(!ch.bit, Ordering::Relaxed);
                HOOK_FAILED.fetch_or(ch.bit, Ordering::Relaxed);
                log(&format!("[chrono_hook] late: no site for {}", ch.name));
            }
        }
    }

    // Only what went live may be claimed, and what did not is recorded: a failed detour in a module the
    // application loaded is a channel it calls on the real clock, and the audit lists it as not covered
    // instead of reading it as a module that never arrived.
    let (newly, failed) =
        enable_late(&created, |batch| queue_and_apply(batch), |bit, target| enable_one_late(bit, target));
    let failed_now = HOOK_FAILED.fetch_or(failed, Ordering::Relaxed) | failed;
    if let Some(c) = live_cov() {
        if newly != 0 {
            // The late mask FIRST, the coverage mask second. The mechanism reads the two as an
            // intersection, so this order cannot produce a warning about a channel the report does not
            // list - and the other order could not either. Stated rather than left to luck.
            set_late_installed(c, read_late_installed(c) | newly);
            // OR, never a plain store: `install` owns the startup bits and this thread owns the late
            // ones. Read-modify-write is safe here because this is the only writer after INSTALL_DONE.
            set_channels_installed(c, read_installed(c) | newly);
        }
        // A whole-mask write, safe for the same reason. The mechanism asks it only about channels the
        // installed mask does not hold, so the order of the two writes cannot show a live channel as
        // failed once both have landed.
        if failed_now != failed_before {
            set_failed_channels(c, failed_now);
        }
    }
    if newly != 0 {
        log(&format!("[chrono_hook] late: installed 0x{newly:x}"));
    }
}}

/// Spawn the watcher once, lazily - NOT from DllMain, to stay clear of the loader lock.
fn ensure_watcher() {
    // Relaxed load first, and it is the hot path that pays for it. `detached()` calls this on nearly
    // every detour - every clock read, every scaled wait, every timer arm, though not the waits that are
    // only counted (`enter_observed_wait`) - and `compare_exchange` emits a LOCKED
    // read-modify-write whether or not it succeeds. So the one-time setup below was charging a bus
    // lock to the hottest path this product has, forever, to re-learn a fact settled once.
    //
    // A relaxed read is enough because nothing is published alongside the flag (the watcher's own
    // result travels through DETACHED), and it only goes back down after a failed start, below. Both
    // ways of racing are harmless - a stale `false` falls through to the CAS, which then fails exactly
    // as it does today, and a `true` means some thread already won, or is trying and will put the flag
    // down for a later call if it fails.
    if WATCHER_STARTED.load(Ordering::Relaxed) {
        return;
    }
    start_watcher();
}

/// The rest of `ensure_watcher`, out of line: it runs once per process (or a few times after a failed
/// start), and keeping it apart keeps the check above small enough to stay inside every detour. The
/// retry grew this part, and with it in the same function a hooked clock read measured about 1.5 ns
/// dearer than on main - after the split, 0.25 ns (median of six pairs of 200 million reads,
/// tools/probes/r4-6/hotpath.ps1, alternating builds).
#[cold]
#[inline(never)]
fn start_watcher() {
    if let Some(&core) = CORE_HANDLE.get()
        && WATCHER_STARTED
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        if over_at_first_look(HANDLE(core as *mut c_void)) {
            // What the watcher would do the moment it woke, done before this call returns, because the
            // caller reads `DETACHED` next. No thread: there is no core left to wait on, and a session
            // that is over installs nothing more (`watcher_proc`).
            release_duration_axes();
            DETACHED.store(true, Ordering::SeqCst);
            return;
        }
        unsafe {
            match CreateThread(None, 0, Some(watcher_proc), None, THREAD_CREATION_FLAGS(0), None) {
                Ok(h) => {
                    let _ = CloseHandle(h);
                }
                Err(_) => {
                    // The flag used to stay up after a failed start, so a process whose watcher never
                    // ran was never let go - it stayed on the session's clock after the session, and
                    // counted into the next one's slot (R4-N6). A later detour tries again, a bounded
                    // number of times, so a process that cannot start threads does not pay for the
                    // attempt on every clock read. A literal message: this runs on a detour's path.
                    log("[chrono_hook] could not start the watcher on the core - a later call tries again");
                    let failures = WATCHER_FAILURES.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                    if watcher_start_again(failures) {
                        WATCHER_STARTED.store(false, Ordering::SeqCst);
                    }
                }
            }
        }
    }
}

/// How many times a watcher that failed to start is tried again. Each try is one `CreateThread` on some
/// detour's path, and a process that cannot create one thread rarely can a few calls later - but a
/// transient failure deserves more than the single chance it used to get.
const WATCHER_START_TRIES: u32 = 16;

/// Failed starts of the watcher so far.
static WATCHER_FAILURES: AtomicU32 = AtomicU32::new(0);

/// Whether a watcher that has failed to start `failures` times gets another try.
fn watcher_start_again(failures: u32) -> bool {
    failures < WATCHER_START_TRIES
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
///
/// The pid alone is enough HERE, though joining asks for the core's creation time as well (R4-W2).
/// A pid is recycled only once no handle to its process is left, and this process holds one to its core
/// for as long as it lives (`CORE_HANDLE`, opened when it joined, never closed) - so no new core can ever
/// appear under our core's number while we watch. The creation time matters where no handle is held
/// yet, in `install`. Comparing it here as well was measured to cost about 0.8 ns on every clock read
/// (tools/probes/r4-6/hotpath.ps1, pairs against main) for a case that cannot happen.
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

/// When the process behind `handle` was created, as one FILETIME number, or `None` when the handle does
/// not allow the question (it needs `PROCESS_QUERY_LIMITED_INFORMATION`). The kernel's record of the
/// system time at creation, which no hook touches and a later change of the system clock does not move
/// (MS Learn, `GetProcessTimes`) - the same number the mechanism reads for the same process.
///
/// # Safety
/// `handle` must be a process handle, or the pseudo-handle of this process.
unsafe fn process_created(handle: HANDLE) -> Option<u64> { unsafe {
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user).ok()?;
    Some(ft_to_i64(created) as u64)
}}

/// The subsystem this process's executable was built for, from its own image headers, or 0 when they
/// cannot be read. The loader maps the executable, headers first, before any library of the process runs,
/// and the first page of a mapped image always holds them.
///
/// # Safety
/// Called from `install`, inside `DllMain`, where the executable's module is mapped.
unsafe fn own_subsystem() -> u32 { unsafe {
    match GetModuleHandleW(PCWSTR::null()) {
        Ok(module) if !module.is_invalid() => {
            image_subsystem(core::slice::from_raw_parts(module.0 as *const u8, IMAGE_HEADER_SPAN))
        }
        _ => 0,
    }
}}

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

/// The real PRECISE unbiased interrupt time - the clock the session's anchors are read from (R5-P2,
/// R5-D31). The precise channels project from it, so a fake `GetSystemTimePreciseAsFileTime` moves with the
/// timer hardware instead of standing still for a whole timer interrupt times the speed.
///
/// The trampoline when the channel is hooked (R5-F4), the export resolved in `install` otherwise - never an
/// import, which would lead into our own detour. Read under `own_clock`, because on 32-bit Windows the
/// system computes it through the EXPORTED `RtlQueryPerformanceCounter`, where the QPC detour stands under
/// `--scale-qpc` (R5-F5). An export that cannot be found - Windows before 10, which the package does not
/// support - falls back to the plain clock: the resolution of before, never a wrong reading.
fn real_quitp() -> i64 {
    let read = || -> Option<i64> {
        let f = O_QUITP.get().or(QUITP_EXPORT.get())?;
        let mut t: u64 = 0;
        unsafe { f(&mut t) };
        Some(t as i64)
    };
    match own_clock(read) {
        Some(t) => {
            // The clock may come from the time-stamp counter, which nothing orders: it has to be read
            // before a seqlock reader's second look at `seq` (`chrono_ctl::clock_fence`).
            clock_fence();
            t
        }
        None => real_quit(),
    }
}

/// The real interrupt time, which counts sleep (`QueryInterruptTime`), through its trampoline when hooked.
/// Read only beside the plain unbiased time, to learn how long the machine has slept (R5-F4).
fn real_qit() -> i64 {
    match O_QIT.get() {
        Some(o) => {
            let mut t: u64 = 0;
            unsafe { o(&mut t) };
            t as i64
        }
        None => (unsafe { QueryInterruptTime() }) as i64,
    }
}

/// How long the machine has slept since it started, as this process last saw it or more - never less, so
/// an interrupt-time channel never steps back on a bias read as a lower bound (`chrono_ctl::sleep_bias`).
/// A shared word every core only reads, written once per wake-up.
fn sleep_bias_now() -> i64 {
    let measured = sleep_bias(real_quit, real_qit);
    let seen = SLEEP_BIAS_SEEN.load(Ordering::Relaxed);
    if measured > seen {
        SLEEP_BIAS_SEEN.fetch_max(measured, Ordering::Relaxed).max(measured)
    } else {
        seen
    }
}

thread_local! {
    /// Raised while this thread runs a call of the hook's OWN into a system clock (R5-F5). On 32-bit
    /// Windows, `RtlGetSystemTimePrecise` and `RtlGetInterruptTimePrecise` call the EXPORTED
    /// `RtlQueryPerformanceCounter` (disassembled 2026-10-14), which is where the QPC detour stands under
    /// `--scale-qpc`. Without the flag the precise clock the anchors stand on came back scaled, and the
    /// application's own precise wall clock, handed to the system once the session let it go, stayed ahead
    /// of the real one by everything the QPC axis had gained - measured 200 s after a 3 s session at x60
    /// (tools/probes/r5/fala3/f5.ps1). The QPC detour hands such a call to the system untouched and uncounted.
    static OWN_CLOCK: Cell<bool> = const { Cell::new(false) };
}

/// Run `f`, a call of the hook's own into a system clock, with `OWN_CLOCK` raised - only when the QPC
/// detour stands, because otherwise there is nothing to step past and the thread-local write is skipped.
fn own_clock<R>(f: impl FnOnce() -> R) -> R {
    if O_QPC.get().is_none() {
        return f();
    }
    let _back = set_flag(&OWN_CLOCK, true);
    f()
}

/// The real `QueryPerformanceCounter`, through the trampoline. Only the release reads it, and only a
/// hooked counter has anything to release - an unhooked one is never answered from `RELEASED` - so
/// without the trampoline the answer is 0 rather than a second road to the export.
fn real_qpc() -> i64 {
    match O_QPC.get() {
        Some(o) => {
            let mut t: i64 = 0;
            unsafe { o(&mut t) };
            t
        }
        None => 0,
    }
}

fn compute_fake() -> Option<i64> {
    fake_now_and_dur_m().map(|(fake, _)| fake)
}

/// The fake wall clock from the PRECISE real clock, for the precise wall channel (R5-P2). The other wall
/// channels stay on the plain clock, as Windows' own do: the precise reading is never behind the plain one
/// at the same moment, and ahead of it by at most one delayed timer interrupt times the speed.
fn compute_fake_precise() -> Option<i64> {
    fake_now_by(real_quitp).map(|(fake, _)| fake)
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
    fake_now_by(real_quit)
}

/// `fake_now_and_dur_m` from the real clock `clock`: the plain one for every channel but the precise ones.
fn fake_now_by(clock: fn() -> i64) -> Option<(i64, i64)> {
    if detached() {
        return None; // core gone: wall detours fall through to the real value
    }
    let p = ctl_ptr()? as *const Ctl;
    // The clock inside the anchor's own window (R4-S3), so a rate change cannot land between them and
    // pair the old anchor with an instant the new one covers - the wall would step back by that much.
    let ((a_fake, a_real, m), now) = unsafe { read_anchor_with(p, clock) };
    if !still_ours(p) {
        return None; // the block was reclaimed by another session mid-read (R2-S6): real time
    }
    // The projection itself lives in `chrono-ctl`, and is the SAME call the mechanism makes for its
    // own `state` reporting. It used to be this formula written out twice, in two crates, with a
    // comment asking that the copies be kept in step - a guard in prose is not a guard (rule 12),
    // and a difference between them is the tool lying about its own clock.
    Some((chrono_ctl::fake_wall_at(a_fake, a_real, now, m), m.max(1)))
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

/// This process's coverage slot, while the session it joined still holds it (R4-W2).
///
/// After the core is gone the slot is nobody's evidence any more - and once a new core has reclaimed the
/// block, the same slot belongs to the NEXT session, whose report would count this application's calls
/// as its own target's (read from the code before the fix, `tools/probes/r4-6`). The flag alone left
/// that window open until the watcher woke, and for good in a process whose watcher never started: a
/// detour that only counts - a connection, an observed wait, a direct process creation - never calls
/// `detached()`. So every write asks the block as well, the same pid comparison each clock read already
/// makes after its read.
///
/// No measurement reached that window - a probe that kept waiting or connecting through the next
/// session added nothing to it, with or without this check - and the check's cost could not be told
/// from nothing: `tools/probes/r4-6/hotpath.ps1` puts two copies of the same build 0.40 ns apart (ten
/// pairs of 200 million), and this check 0.45 ns from the same build without it. Unreachable in
/// measurement, and free as far as it can be measured, like the check after each read (R2-S6).
///
/// The end mark is not asked here: the core reads every slot's final counts BEFORE it marks the end, so
/// a count written after the mark lands in a slot nobody reads again, until a reclaim zeroes it - and
/// after a reclaim the pid no longer matches.
fn live_cov() -> Option<*mut Cov> {
    if DETACHED.load(Ordering::Relaxed) {
        return None;
    }
    let c = cov_ptr()?;
    still_ours(ctl_ptr()? as *const Ctl).then_some(c)
}

/// Whether the block says the session this process joined is over: it carries the end mark, or it names
/// another session. The mark comes first, because after an ordered end the block keeps naming this
/// session until a new core takes it over, so `still_ours` alone reads "running" for as long as the
/// watcher has not woken.
fn block_says_over(ended: bool, ours: impl FnOnce() -> bool) -> bool {
    ended || !ours()
}

/// Whether the session this process joined is over, as far as it can tell right now: the watcher saw its
/// core go, or the block says so. Asked before a child is followed, so a process the session left running
/// starts its children as it would without us (R4-W2).
fn session_over() -> bool {
    if detached() {
        return true;
    }
    match ctl_ptr() {
        Some(p) => {
            let p = p as *const Ctl;
            block_says_over(unsafe { read_ended(p) }, || still_ours(p))
        }
        None => true,
    }
}

/// Whether the session is over at the moment this process first reaches for its watcher, which it does
/// from its first detour, not when it joins (`ensure_watcher`). A process that touches no detour while
/// its session runs therefore starts the watcher only after the end - and until that new thread had run,
/// every read came back on the session's clock: measured on a probe that read no clock while its session
/// ran, five reads in a row after the end at the session's date (`tools/probes/r4-6`, case M1).
fn over_at_first_look(core: HANDLE) -> bool {
    let block_over = match ctl_ptr() {
        Some(p) => {
            let p = p as *const Ctl;
            block_says_over(unsafe { read_ended(p) }, || still_ours(p))
        }
        None => true,
    };
    block_over || read_core_wait(unsafe { wait_raw(core, 0) }, || core_exit_code(core)) == CoreWait::Gone
}

fn bump(idx: usize) {
    if let Some(p) = live_cov() {
        unsafe { bump_calls(p, idx) }
    }
}

/// Convert a fake UTC FILETIME (100 ns ticks) into `*lp` as a SYSTEMTIME. Returns whether it wrote
/// `*lp`: `false` means `FileTimeToSystemTime` rejected the instant (e.g. a moment near the FILETIME
/// boundary shifted by a large `tz_bias`), and the caller must defer to the real API rather than leave
/// `*lp` holding uninitialized garbage while claiming success (L-3).
///
/// # Safety
/// `lp` must be a valid, writable pointer to a `SYSTEMTIME`, aligned or not.
unsafe fn write_systemtime(lp: *mut SYSTEMTIME, ft_ticks: i64) -> bool { unsafe {
    let ft = i64_to_ft(ft_ticks);
    let mut st = SYSTEMTIME::default();
    if FileTimeToSystemTime(&ft, &mut st).is_ok() {
        core::ptr::write_unaligned(lp, st);
        true
    } else {
        false
    }
}}

// --- Detours -------------------------------------------------------------------
// Each fills its out-parameter with the fake instant, or falls back to the original
// if the anchor is unreadable or the pointer is null.
//
// Every read and write through a pointer the application passed in is unaligned (R4-N3). The real
// functions accept a buffer at any address - a packed structure puts a FILETIME or a SYSTEMTIME on
// an odd one, and x86 and x64 load and store there without complaint - so the application has done
// nothing wrong. A plain `*lp = x` on such a pointer is undefined behaviour in Rust, and a debug
// build of this library checks it and aborts the application on the spot (measured by the
// misaligned buffers in `crates/cli/tests/hook_integrity.rs`).

unsafe extern "system" fn h_gstaft(lp: *mut FILETIME) { unsafe {
    bump(IDX_GSTAFT);
    match compute_fake() {
        Some(t) if !lp.is_null() => core::ptr::write_unaligned(lp, i64_to_ft(t)),
        _ => {
            if let Some(o) = O_GSTAFT.get() {
                o(lp)
            }
        }
    }
}}

// The precise wall clock projects from the precise real clock (R5-P2). Its original, when the session is
// gone, computes the time through the exported QPC on 32-bit Windows, hence `own_clock` (R5-F5).
unsafe extern "system" fn h_gstpaft(lp: *mut FILETIME) { unsafe {
    bump(IDX_GSTPAFT);
    match compute_fake_precise() {
        Some(t) if !lp.is_null() => core::ptr::write_unaligned(lp, i64_to_ft(t)),
        _ => {
            if let Some(o) = O_GSTPAFT.get() {
                own_clock(|| o(lp))
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
            core::ptr::write_unaligned(lp, t);
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
// Report the session zone (Bias = tz_bias, no DST, and in the dynamic form DST explicitly
// disabled) so a target's notion of "which zone am I in" agrees with the shifted GetLocalTime.

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
        core::ptr::write_unaligned(lp, tzi);
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
        // DynamicDaylightTimeDisabled is TRUE because the session zone has no daylight saving time, and
        // TRUE with both transition dates cleared is how MS Learn says a zone without it is described.
        // FALSE would claim dynamic transition data exists under a registry key that does not. It is
        // also the field a runtime reads to decide whether the zone can be built from Bias alone: the
        // JVM (every line from 8 to the current one) does exactly that when it is TRUE, and otherwise
        // looks the key name up in its own table, misses, and falls back to ActiveTimeBias from the
        // REAL registry, which is how Java kept the host zone under every session. ICU names a
        // whole-hour offset Etc/GMT-N on the same flag. .NET, the C runtime and Go do not read it.
        let mut d = DYNAMIC_TIME_ZONE_INFORMATION {
            Bias: cur_tz_bias(),
            DynamicDaylightTimeDisabled: true,
            ..Default::default()
        };
        set_wide(&mut d.StandardName, SESSION_ZONE_NAME);
        set_wide(&mut d.TimeZoneKeyName, SESSION_ZONE_NAME);
        core::ptr::write_unaligned(lp, d);
        return 0; // TIME_ZONE_ID_UNKNOWN - the session zone has no DST
    }
    O_GDTZI.get().map(|o| o(lp)).unwrap_or(TIME_ZONE_ID_INVALID)
}}

/// Whether a dynamic zone the caller passed is the one `h_gdtzi` handed out, by its key name. Compared in
/// place, without building a string, because a detour does not allocate.
///
/// # Safety
/// `dtzi` must point to a readable `DYNAMIC_TIME_ZONE_INFORMATION`, aligned or not.
unsafe fn names_session_zone(dtzi: *const DYNAMIC_TIME_ZONE_INFORMATION) -> bool { unsafe {
    let key = core::ptr::read_unaligned(dtzi).TimeZoneKeyName;
    let mut name = SESSION_ZONE_NAME.encode_utf16();
    key.iter().all(|&unit| match name.next() {
        Some(expected) => unit == expected,
        None => unit == 0,
    })
}}

// GetTimeZoneInformationForYear gives the rules a zone follows in one year: of the zone the caller names,
// or of the current one when it names none. The Rust `chrono` crate reads the current zone this way and
// no other, so its local time showed this machine's zone under every session (measured 2026-10-27: a
// session at +05:30 printed +01:00). The current zone is the session's, the answer h_gtzi gives, in every
// year, because the session zone has no daylight saving time. A zone named like the session's is the one
// h_gdtzi handed out, which no registry knows and the original would fail on, so it gets the same answer.
// Any other named zone is a question about a real zone, and goes through untouched.
unsafe extern "system" fn h_gtzify(
    year: u16,
    dtzi: *const DYNAMIC_TIME_ZONE_INFORMATION,
    tzi: *mut TIME_ZONE_INFORMATION,
) -> i32 { unsafe {
    bump(IDX_GTZIFY);
    let o = match O_GTZIFY.get() {
        Some(o) => o,
        None => return 0,
    };
    if detached() || tzi.is_null() || !(dtzi.is_null() || names_session_zone(dtzi)) {
        return o(year, dtzi, tzi);
    }
    let mut out = TIME_ZONE_INFORMATION { Bias: cur_tz_bias(), ..Default::default() };
    set_wide(&mut out.StandardName, SESSION_ZONE_NAME);
    core::ptr::write_unaligned(tzi, out);
    1
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
/// `src` and `dst` must be valid, non-null FILETIME pointers, aligned or not.
unsafe fn shift_filetime(src: *const FILETIME, dst: *mut FILETIME, add: bool) -> i32 { unsafe {
    let ticks = ft_to_i64(core::ptr::read_unaligned(src));
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
    core::ptr::write_unaligned(dst, i64_to_ft(shifted));
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
// Only installed when scale_duration is set. The anchor (`dur_tick_offset`, `dur_quit_c0`, `dur_q0`)
// lives in the shared `Ctl`, initialized by the core in `prepare` and REBASED on every `set_multiplier`
// so a speed change never rewinds the axis (H-1, untouchable rule 3). Each detour reads the base, the
// multiplier AND the trampoline QUIT in one `read_dur_with` window (R4-S3), then asks where the watcher
// let the process go (`RELEASE_AT_QUIT`, F6), in that order. The tick count is that QUIT in milliseconds
// plus the session's offset (F4). `m` is clamped to >= 1 inside `dur_quit_at`, so the axis keeps
// advancing even when the wall clock is frozen.

/// The duration axes as the session answers them now: the anchor and the real QUIT read in one window,
/// and the instant this process was let go at, read after that clock (0 while the session holds it).
///
/// # Safety
/// `p` must be the session's mapped control block.
#[inline]
unsafe fn dur_now(p: *const Ctl) -> (chrono_ctl::DurAnchor, i64, i64) { unsafe {
    let (dur, real) = read_dur_with(p, real_quit);
    (dur, real, RELEASE_AT_QUIT.load(Ordering::Acquire))
}}

/// The scaled GetTickCount64, falling back to `original` (the body this detour replaced) once the
/// session is gone. Shared by the kernel32 and the kernelbase detour, which differ only in that body.
///
/// Absolute on purpose: while the session holds, the answer comes from the anchors and `original` is
/// never called, so a kernel32 copy that calls into kernelbase's cannot reach the second detour and
/// count one read twice (`ChannelModule::KernelBaseAndKernel32`).
///
/// # Safety
/// `original` must hold this channel's trampoline, if anything.
unsafe fn tick64_or(original: &OnceLock<TickFn>) -> u64 { unsafe {
    bump(IDX_GTC64);
    // Once the session has let go: on from where the axis stood (rule 3), the real value only when
    // nothing could be released.
    let after = || released_tick().unwrap_or_else(|| original.get().map(|o| o()).unwrap_or(0));
    if detached() {
        return after();
    }
    match ctl_ptr() {
        Some(p) => {
            let (dur, real, released_at) = dur_now(p as *const Ctl);
            let fake = dur.tick_at(real, released_at);
            // Ownership checked after the read (R2-S6): a reclaimed block would hand this target
            // another session's duration base, which reads as the axis jumping.
            if still_ours(p as *const Ctl) {
                fake
            } else {
                after()
            }
        }
        None => original.get().map(|o| o()).unwrap_or(0),
    }
}}

/// GetTickCount (32-bit): the low 32 bits of the SAME scaled millisecond count as
/// GetTickCount64 (the same QUIT and offset), so a target comparing the two sees them
/// agree. Wraps at 2^32 ms like the real one - and sooner under acceleration - which is the
/// honest behavior of a fast 32-bit counter - callers handle the wrap with unsigned deltas.
///
/// # Safety
/// `original` must hold this channel's trampoline, if anything.
unsafe fn tick32_or(original: &OnceLock<Tick32Fn>) -> u32 { unsafe {
    bump(IDX_GTC);
    // The low 32 bits of the released 64-bit axis, exactly as in session.
    let after = || released_tick().map(|t| t as u32).unwrap_or_else(|| original.get().map(|o| o()).unwrap_or(0));
    if detached() {
        return after();
    }
    match ctl_ptr() {
        Some(p) => {
            let (dur, real, released_at) = dur_now(p as *const Ctl);
            let fake = dur.tick_at(real, released_at) as u32;
            if still_ours(p as *const Ctl) {
                fake
            } else {
                after()
            }
        }
        None => original.get().map(|o| o()).unwrap_or(0),
    }
}}

unsafe extern "system" fn h_tick() -> u64 { unsafe { tick64_or(&O_TICK) } }
unsafe extern "system" fn h_tick_kb() -> u64 { unsafe { tick64_or(&O_TICK_KB) } }
unsafe extern "system" fn h_tick32() -> u32 { unsafe { tick32_or(&O_TICK32) } }
unsafe extern "system" fn h_tick32_kb() -> u32 { unsafe { tick32_or(&O_TICK32_KB) } }

/// `QueryUnbiasedInterruptTime` once the session has let go: on from where the axis stood (rule 3), the
/// real call when nothing could be released or there is nowhere to write the answer.
///
/// # Safety
/// `lp` is the caller's out pointer, null or writable, as the API itself requires.
unsafe fn quit_after_session(lp: *mut u64) -> i32 { unsafe {
    match released_quit() {
        Some(v) if !lp.is_null() => {
            core::ptr::write_unaligned(lp, v as u64);
            1
        }
        _ => O_QUIT.get().map(|o| o(lp)).unwrap_or(0),
    }
}}

unsafe extern "system" fn h_quit(lp: *mut u64) -> i32 { unsafe {
    bump(IDX_QUIT);
    if detached() {
        return quit_after_session(lp);
    }
    if !lp.is_null() {
        match ctl_ptr() {
            Some(p) => {
                let (dur, real, released_at) = dur_now(p as *const Ctl);
                let fake = dur.quit_at(real, released_at) as u64;
                if !still_ours(p as *const Ctl) {
                    return quit_after_session(lp);
                }
                core::ptr::write_unaligned(lp, fake);
            }
            // No control block (unreachable: CTL_PTR is set before these hooks install) - defer to the
            // real value rather than fake a zero.
            None => return O_QUIT.get().map(|o| o(lp)).unwrap_or(0),
        }
    }
    1 // nonzero BOOL = success
}}

// The interrupt-time clocks (R5-F4, R5-D30, R5-D33): QueryInterruptTime, QueryInterruptTimePrecise and
// QueryUnbiasedInterruptTimePrecise, kernelbase's own code (kernel32 exports none of them). They ride the
// duration axis like QUIT, so a process that reads one of them beside the tick count sees one speed - the
// Chromium engine of WebView2 imports the third and used to read it real under a scaled tick count.
//
// The unbiased pair is the fake QUIT from the plain or the precise real clock. The interrupt-time pair adds
// the real time the machine has slept since it started: the interrupt time counts sleep and the unbiased
// one does not, and the session never multiplies sleep by its speed (ADR-5), so the fake pair keeps exactly
// the real difference. After the session they carry on from where the axis stood, like QUIT (ADR-14).

/// One interrupt-time channel: the fake QUIT at `clock`, plus the sleep bias when `with_sleep`, or the
/// original's own answer when nothing can be faked (a null pointer, a reclaimed block with nothing
/// released). The original runs under `own_clock`, because its precise variants reach the exported QPC on
/// 32-bit Windows (R5-F5).
///
/// # Safety
/// `lp` is the caller's out pointer, null or writable, as the API itself requires.
unsafe fn interrupt_time_or(lp: *mut u64, clock: fn() -> i64, with_sleep: bool, original: &OnceLock<InterruptFn>) { unsafe {
    let real = || {
        if let Some(o) = original.get() {
            own_clock(|| o(lp));
        }
    };
    // A session with `--scale-qpc` and without the duration axis hooks the two precise ones only to keep
    // them REAL (R5-F5, `install`): the session did not ask for this axis to move.
    if lp.is_null() || !SCALE_DUR.load(Ordering::Relaxed) {
        return real();
    }
    let released = || RELEASED.get().map(|r| r.quit_at(clock()));
    let fake = if detached() {
        released()
    } else {
        match ctl_ptr() {
            Some(p) => {
                let (dur, now) = read_dur_with(p as *const Ctl, clock);
                let value = dur.quit_at(now, RELEASE_AT_QUIT.load(Ordering::Acquire));
                // Ownership checked after the read (R2-S6), as every duration detour does.
                if still_ours(p as *const Ctl) { Some(value) } else { released() }
            }
            None => None,
        }
    };
    match fake {
        Some(v) => {
            let v = if with_sleep { v.saturating_add(sleep_bias_now()) } else { v };
            core::ptr::write_unaligned(lp, v as u64);
        }
        None => real(),
    }
}}

unsafe extern "system" fn h_qit(lp: *mut u64) { unsafe {
    bump(IDX_QIT);
    interrupt_time_or(lp, real_quit, true, &O_QIT)
}}

unsafe extern "system" fn h_qitp(lp: *mut u64) { unsafe {
    bump(IDX_QITP);
    interrupt_time_or(lp, real_quitp, true, &O_QITP)
}}

unsafe extern "system" fn h_quitp(lp: *mut u64) { unsafe {
    bump(IDX_QUITP);
    interrupt_time_or(lp, real_quitp, false, &O_QUITP)
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

/// `QueryPerformanceCounter` once the session has let go: on from where the axis stood (rule 3), the
/// real counter when nothing could be released.
///
/// # Safety
/// `lp` must be non-null and writable - `h_qpc` has already sent a null one to the original.
unsafe fn qpc_after_session(o: QpcFn, lp: *mut i64) -> i32 { unsafe {
    let Some(r) = RELEASED.get() else {
        return o(lp);
    };
    let mut real: i64 = 0;
    o(&mut real);
    core::ptr::write_unaligned(lp, r.qpc_at(real));
    1
}}

unsafe extern "system" fn h_qpc(lp: *mut i64) -> i32 { unsafe {
    // Counted like every other channel (R2-S3). QPC is the hottest clock a process calls, so the cost
    // was measured rather than assumed - and measured as an INTERLEAVED A/B on two hook builds, because
    // sequential batches drifted by ~9 ns between them and would have shown a slowdown that was not
    // there. Five alternating pairs, 5 M calls each, x64, probe `pqpc`: the counted build ran
    // +0.08 ns/call on average (worst pair +0.2), against ~25 ns for a bare QPC and ~37 ns hooked. The
    // bump disappears next to the trampoline call and the seqlock read it rides on - on one thread. Many
    // threads writing one counter is another matter (R5-WY11), hence its ceiling (`chrono_ctl::CALLS_CAP`).
    let o = match O_QPC.get() {
        Some(o) => *o,
        None => return 0,
    };
    // A call of the hook's own into a system clock that reaches this export from inside the system (R5-F5):
    // the real counter, not counted - it is not the application's call, and scaled it corrupts the clock
    // the hook was reading. A thread without thread-local storage cannot say whose call it is, and gets
    // the same untouched answer: a counter left real there is safer than a hook clock scaled under itself.
    // It is not counted as a call without storage either (`enter_detour`), because there the hook's own
    // reads would count too, and the report would name a gap the hook made itself.
    if read_flag(&OWN_CLOCK).unwrap_or(true) {
        return o(lp);
    }
    bump(IDX_QPC);
    if lp.is_null() {
        return o(lp);
    }
    match ctl_ptr() {
        Some(p) if !detached() => {
            // The real counter inside the anchor's window, like every other reader (R4-S3). What a seqlock
            // reader needs is its clock read before the second look at `seq`, and the fence after it keeps
            // the time-stamp read from running past that look. Until R4/10a the counter came before the
            // anchor, which is as good on that count - but a speed-up that landed between the two paired
            // an early counter with the faster anchor, and with no floor under the elapsed time (R4-N4)
            // the answer fell below the anchor's base. Measured: 1 627 steps back in 20 s of rate changes
            // on x64, up to 0.18 s each. The floor and a rate change that reads its instant inside its
            // own write (F3) are what closed it - a mutation back to the old order stays clean.
            let clock = || {
                let mut real: i64 = 0;
                o(&mut real); // real QPC via the trampoline (bypasses this hook, no recursion)
                clock_fence();
                real
            };
            let (qpc, real) = read_qpc_with(p as *const Ctl, clock);
            let fake = qpc.at(real, RELEASE_AT_QPC.load(Ordering::Acquire));
            if !still_ours(p as *const Ctl) {
                return qpc_after_session(o, lp); // reclaimed mid-read (R2-S6)
            }
            core::ptr::write_unaligned(lp, fake);
            1
        }
        // Detached (core gone): on from where the axis stood, never back to the real counter (rule 3).
        Some(_) => qpc_after_session(o, lp),
        // No control block (unreachable: CTL_PTR is set before these hooks install) - the real counter.
        None => o(lp),
    }
}}

// Wait axis (ADR-7 class A): divide a blocking wait's timeout by the duration multiplier so
// a thread that blocks on time wakes in lockstep with the scaled clock it reads. INFINITE and
// 0 pass through (scale_wait). Unlike the absolute wall detours, a wait detour is RELATIVE - it
// calls the original with a modified argument, and the original may re-enter another hooked wait
// export on the same thread. A thread-local guard makes each app-level wait scale exactly once
// and be counted against the export the app actually called, never an internal cascade. Both
// Sleep and SleepEx bottom out on NtDelayExecution, so with that funnel hooked the guard is
// load-bearing: Sleep scales at h_sleep, then re-enters h_ntdelay, which the flag makes pass
// through unscaled. Since the two stand in kernelbase (2026-09-23) a Sleep crosses THREE detours:
// kernelbase's Sleep reaches the exported SleepEx, and SleepEx reaches NtDelayExecution. Measured on
// both bitnesses with every guard off (R4/10b, `tools/probes/pcascade`), Sleep(0) and SleepEx(0)
// included. Before the move, a Sleep through the api-set never met h_sleep: it was scaled at
// h_ntdelay and counted under that name - measured, scaled x57, NtDelayExecution +3.
//
// The flag is up only while the thread runs Windows' own code between two of these detours. An
// alertable wait is where Windows runs the application's APCs, and an APC is the application's code:
// a Sleep in it is an application call, to be scaled and counted. So the flag goes down for the
// kernel wait itself (`wait_runs_application`) and comes back up when the wait returns into Windows'
// code. Until R4/10b it stood through the whole call, and a Sleep(1200) inside an APC ran 1.2 s real
// at x60 and went uncounted (R4-N7, measured on x64 and x86). The same placement settles an exception
// that leaves an APC: it unwinds past every frame between the APC and its handler, and whether or
// not those frames put their flags back, the flag it leaves behind is the lowered one - the state of
// a thread outside any wait. Before, the flag stayed up and every later Sleep of that thread ran real.

thread_local! {
    static SCALING_WAIT: Cell<bool> = const { Cell::new(false) };
}

// A thread can run without the storage every `thread_local!` of the hook lives in: one the loader is
// ending, past the point where it freed that storage (`LdrShutdownThread`), or one started without it.
// The compiled read of a thread-local goes through the TEB's `ThreadLocalStoragePointer`, which is null
// then, so the read faulted inside the detour. Measured 2026-10-28 on two real programs: a helper process
// of one died of the fault, and the other hung at its start for good, because the heap called a hooked
// wait while it freed that very storage, the fault's handler asked the same heap for memory, and the heap
// lock was the thread's own (`TEB+0x58` read 0 on the hung thread, the core gave up after 10 s). Every
// flag read and write below asks `thread_has_tls` first. A thread without the storage cannot tell a
// cascade from an application call, so its call goes to the system as it came: unscaled is wrong by one
// rate, a cascade scaled at every level is wrong by its power, and a fault is worse than both.

/// Whether this thread has the thread-local storage of this module: the TEB's vector and this module's
/// slot in it. The same two reads the compiler makes for a `thread_local!`, made first so they never fault.
#[inline]
fn thread_has_tls() -> bool {
    unsafe extern "C" {
        static _tls_index: u32;
    }
    let vector: *const *const u8;
    // SAFETY: one pointer-sized field of this thread's own TEB, which every thread has -
    // `ThreadLocalStoragePointer`, at 0x58 from gs on x64 and 0x2C from fs on x86, the address the
    // compiled thread-local reads start from.
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("mov {}, qword ptr gs:[0x58]", out(reg) vector, options(nostack, readonly, preserves_flags));
    }
    #[cfg(target_arch = "x86")]
    unsafe {
        core::arch::asm!("mov {}, dword ptr fs:[0x2C]", out(reg) vector, options(nostack, readonly, preserves_flags));
    }
    // SAFETY: a vector that is there holds one slot per module with thread-locals, `_tls_index` this one's.
    !vector.is_null() && unsafe { !(*vector.add(_tls_index as usize)).is_null() }
}

/// The flag on this thread, or None on a thread without thread-local storage to hold it (`thread_has_tls`).
fn read_flag(flag: &'static LocalKey<Cell<bool>>) -> Option<bool> {
    thread_has_tls().then(|| flag.get())
}

/// Puts a thread-local flag of the wait and timer detours back to what it was when it was set.
///
/// Back to what it was, not down: a guard taken inside another's span leaves the outer one standing.
struct FlagRestore {
    flag: &'static LocalKey<Cell<bool>>,
    /// None when the thread had no thread-local storage to set the flag in, so there is nothing to put back.
    was: Option<bool>,
}

impl Drop for FlagRestore {
    fn drop(&mut self) {
        if let Some(was) = self.was
            && thread_has_tls()
        {
            self.flag.set(was);
        }
    }
}

/// Set `flag` to `up` until the returned guard drops. On a thread without thread-local storage, nothing.
fn set_flag(flag: &'static LocalKey<Cell<bool>>, up: bool) -> FlagRestore {
    FlagRestore { flag, was: thread_has_tls().then(|| flag.replace(up)) }
}

/// The call that starts a cascade raises the flag and gets its guard, a call inside one gets None - and
/// so does a call on a thread without thread-local storage, which cannot tell the two apart.
fn enter_once(flag: &'static LocalKey<Cell<bool>>) -> Option<FlagRestore> {
    match read_flag(flag) {
        Some(false) => Some(set_flag(flag, true)),
        _ => None,
    }
}

/// `enter_once` for a wait, object-wait or timer detour, which counts what it lets by: a None there means
/// a cascade, counted at its top already, except on a thread without thread-local storage, where it means
/// a call that ran unscaled and uncounted. That one is counted apart, so the report says a part of the
/// application ran on the real clock (`wait.thread_without_storage`) instead of looking complete (rule 4).
///
/// Not on the thread that loaded the hook: that one is the injector's, ran none of the application's code,
/// and is the very thread measured losing its storage as it ended (2026-10-28) - counting it would name a
/// gap in the application that the hook's own start made.
fn enter_detour(flag: &'static LocalKey<Cell<bool>>) -> Option<FlagRestore> {
    let entered = enter_once(flag);
    // SAFETY: GetCurrentThreadId reads this thread's TEB and takes no argument.
    if entered.is_none() && !thread_has_tls() && unsafe { GetCurrentThreadId() } != LOADER_THREAD.load(Ordering::Relaxed) {
        note_without_storage();
    }
    entered
}

/// The thread `DllMain` ran on - the one the injector started to load the hook (`enter_detour`).
static LOADER_THREAD: AtomicU32 = AtomicU32::new(0);

/// Count one hooked call passed through for want of thread-local storage (`enter_detour`).
fn note_without_storage() {
    if let Some(p) = live_cov() {
        unsafe { bump_calls_without_storage(p) }
    }
}

/// Run the original of a wait with `flag` down when the wait is alertable, so an APC that Windows runs
/// inside it enters the detours as the application call it is (R4-N7).
///
/// Only the innermost hooked wait on a path does this, the one whose original makes the kernel wait:
/// a detour above it lowering the flag would count the cascade below it a second time. Which detours
/// those are is measured, not assumed (`tools/probes/pcascade`, both bitnesses).
fn wait_runs_application<R>(flag: &'static LocalKey<Cell<bool>>, alertable: bool, wait: impl FnOnce() -> R) -> R {
    let _back = alertable.then(|| set_flag(flag, false));
    wait()
}

/// Record that one wait could not be divided by the full multiplier, so the audit can say so.
///
/// Silent partial coverage is what rule 27 calls a breach rather than a compromise, and this is
/// exactly a partial: the wait is still shortened, just not by M. Counting it here, in the process
/// that made the call, keeps it attributable like every other piece of per-process evidence.
fn note_wait_at_floor() {
    if let Some(p) = live_cov() {
        unsafe { bump_waits_at_floor(p) }
    }
}

/// Record that one timer could not be divided by the full multiplier (R5, D8) - the timer twin of
/// `note_wait_at_floor`, for the same reason: a timer held at a floor still runs faster, just not by M,
/// and rule 27 says that part is named rather than left silent.
fn note_timer_at_floor() {
    if let Some(p) = live_cov() {
        unsafe { bump_timers_at_floor(p) }
    }
}

/// A waitable or thread-pool timer period scaled by `scale`, counting the case where the 1 ms floor
/// had to hold it.
fn scaled_timer_period<P: Copy + Into<i64>>(period: P, m: i64, scale: fn(P, i64) -> P) -> P {
    if timer_period_hit_floor(period.into(), m) {
        note_timer_at_floor();
    }
    scale(period, m)
}

/// The scaled timeout, counting the case where the floor had to hold it.
fn scaled_wait_ms(ms: u32, m: i64) -> u32 {
    if wait_hit_floor(ms, m) {
        note_wait_at_floor();
    }
    scale_wait(ms, m)
}

/// Decide whether this wait call is the top-level app call we should scale. Returns the
/// duration multiplier and a guard (held across the original call, so an inner cascade sees the
/// flag set and passes through) when it is - None when this is an internal cascade (pass the
/// original through, uncounted). A detached core gives multiplier 1 (real time). Bumps coverage
/// only for a top-level app call, so the audit counts what the app called, not what Windows
/// re-entered.
fn try_enter_wait(idx: usize) -> Option<(i64, FlagRestore)> {
    // None is an internal cascade: pass through, do not bump.
    let guard = enter_detour(&SCALING_WAIT)?;
    bump(idx);
    if detached() {
        // The core is gone, so this wait runs real - but it still cascades internally (Sleep funnels
        // into NtDelayExecution), and without the guard the inner call counted as a second top-level
        // call. The guard is held here too, so one application wait is one tally either way (rule 4).
        return Some((1, guard)); // multiplier 1 = real time, unchanged
    }
    Some((relative_rate(), guard))
}

/// The rate a LENGTH of time is divided by - a delay, a timer's relative due, its period: the session's
/// under `--scale-duration`, 1 without it (R5-D2). The deadline detours stand in every session, and a
/// session that did not ask for the duration axis must not have its `Sleep` shortened through
/// `NtDelayExecution`, where every `Sleep` ends.
fn relative_rate() -> i64 {
    if SCALE_DUR.load(Ordering::Relaxed) { dur_multiplier() } else { 1 }
}

/// `relative_rate` for a caller that already holds the duration multiplier `m` from its own snapshot of the
/// anchor, so the rate and the clock it is applied with come from one read (`fake_now_and_dur_m`).
fn relative_of(m: i64) -> i64 {
    if SCALE_DUR.load(Ordering::Relaxed) { m } else { 1 }
}

/// A waitable or thread-pool timer's due (100 ns, FILETIME convention) as the kernel gets it: an absolute
/// due converted on the wall's speed, counted (`note_absolute_due`), a relative one divided by `rel_m`.
fn timer_due(due: i64, fake_now: i64, wall_m: i64, rel_m: i64) -> i64 {
    if due >= 0 {
        note_absolute_due();
    }
    scale_timer_due(due, fake_now, wall_m, rel_m)
}

/// Count one absolute deadline this process had converted (R5-D2), so the core can tell whether one was
/// pending when the session jumped or changed speed.
fn note_absolute_due() {
    if let Some(p) = live_cov() {
        unsafe { bump_absolute_dues(p) }
    }
}

/// An absolute `NtDelayExecution` deadline - a wall-clock instant the application computed from the fake
/// clock - as the real relative delay until the fake clock reaches it (R5-D2), or as an instant long past
/// when it has already reached it (R5-D59), or `None` when the session is gone and the wall is real again.
/// Converted in every session: before R5 it went to the kernel as it was, under `--scale-duration` too,
/// and the kernel compares it with the real clock.
fn absolute_delay(deadline: i64) -> Option<i64> {
    let (fake_now, wall_m) = fake_now_and_dur_m()?;
    note_absolute_due();
    Some(scale_delay_deadline(deadline, fake_now, wall_m))
}

unsafe extern "system" fn h_sleep(ms: u32) { unsafe {
    let o = match O_SLEEP.get() {
        Some(o) => o,
        None => return,
    };
    match try_enter_wait(IDX_SLEEP) {
        Some((m, _guard)) => o(scaled_wait_ms(ms, m)),
        None => o(ms),
    }
}}

unsafe extern "system" fn h_sleepex(ms: u32, alertable: i32) -> u32 { unsafe {
    let o = match O_SLEEPEX.get() {
        Some(o) => o,
        None => return 0,
    };
    match try_enter_wait(IDX_SLEEPEX) {
        Some((m, _guard)) => o(scaled_wait_ms(ms, m), alertable),
        None => o(ms, alertable),
    }
}}

// NtDelayExecution is the shared funnel Sleep and SleepEx bottom out on, so hooking it makes the
// re-entrancy guard load-bearing (a scaled Sleep re-enters here and must pass through). It also
// catches callers that reach ntdll directly. The interval is signed 100 ns: a negative (relative) delay is
// scaled under scale_duration, a positive (absolute deadline) one is converted in every session (R5-D2),
// and a null passes through. It stands in every session for that reason, and without scale_duration every
// Sleep of the application reaches it unscaled (`relative_rate`). Its original is the kernel wait of every
// scaled wait, so this is where the flag goes down for an alertable one, whether the call came from the
// application or down a cascade (R4-N7).
unsafe extern "system" fn h_ntdelay(alertable: u8, interval: *const i64) -> i32 { unsafe {
    let o = match O_NTDELAY.get() {
        Some(o) => o,
        None => return STATUS_UNSUCCESSFUL, // no trampoline: no delay happened, so do not report success
    };
    let kernel_wait = |interval: *const i64| wait_runs_application(&SCALING_WAIT, alertable != 0, || o(alertable, interval));
    match try_enter_wait(IDX_NTDELAY) {
        Some((m, _guard)) => {
            if interval.is_null() {
                kernel_wait(interval)
            } else {
                let requested = core::ptr::read_unaligned(interval);
                let forwarded = if requested > 0 {
                    absolute_delay(requested).unwrap_or(requested)
                } else {
                    if delay_hit_floor(requested, m) {
                        note_wait_at_floor();
                    }
                    scale_delay_interval(requested, m)
                };
                kernel_wait(&forwarded as *const i64)
            }
        }
        None => kernel_wait(interval),
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
// Measured on Win11 26200 (guard on vs off, psleep) while these stood on kernel32's stubs: the
// cascades did not reach a hooked partner. Since they stand in kernelbase (2026-09-23) the guard
// is load-bearing: 64-bit WaitForSingleObject there is `xor r8d, r8d` and a jump to the exported
// WaitForSingleObjectEx, and 29 other places in kernelbase call that export directly, among them
// GetOverlappedResult and OutputDebugStringA. The hook's own waits go through `unobserved`.
// Detached state is irrelevant: we never modify the wait either way.
//
// The cascades, measured with every guard off on both bitnesses (R4/10b, `tools/probes/pcascade`):
// WaitForSingleObject -> ...Ex, WaitForMultipleObjects -> ...Ex, WSAWaitForMultipleEvents ->
// WaitForMultipleObjectsEx, and on 32-bit only MsgWaitForMultipleObjects -> ...Ex. Nothing else
// reaches a hooked partner, the condition-variable waits included. So the alertable forms of
// ...Ex, SignalObjectAndWait and MsgWaitForMultipleObjectsEx make the kernel wait themselves, and
// they lower the flag for it the way h_ntdelay does for class A: a wait inside an APC is counted
// as the application's, and an exception leaving the APC leaves the flag down (R4-N7). The outer
// forms never lower it, or the cascade below them would count twice.

thread_local! {
    static OBSERVING_WAIT: Cell<bool> = const { Cell::new(false) };
}

/// Count an app-level object wait once, unless this is an internal cascade from another hooked
/// object-wait export (then the outer call already counted it). When this is the top-level call it
/// returns a guard, held across the forwarded original so the cascade sees the flag set.
fn enter_observed_wait(idx: usize) -> Option<FlagRestore> {
    // None is an internal cascade: counted at the top level already.
    let guard = enter_detour(&OBSERVING_WAIT)?;
    bump(idx);
    Some(guard)
}

/// Run a call of the hook's OWN that may end in an observed wait, with the flag raised and nothing
/// counted, so the wait it reaches passes through as a cascade would. The flag goes back to what it
/// was, so a nested use leaves an outer wait's flag alone.
fn unobserved<R>(f: impl FnOnce() -> R) -> R {
    let _guard = set_flag(&OBSERVING_WAIT, true);
    f()
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
    wait_runs_application(&OBSERVING_WAIT, alertable != 0, || o(handle, ms, alertable))
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
    wait_runs_application(&OBSERVING_WAIT, alertable != 0, || o(count, handles, wait_all, ms, alertable))
}}

unsafe extern "system" fn h_soaw(signal: HANDLE, wait: HANDLE, ms: u32, alertable: i32) -> u32 { unsafe {
    let o = match O_SOAW.get() {
        Some(o) => o,
        None => return WAIT_FAILED.0, // no trampoline: fail the wait, never claim it was signalled
    };
    let _g = enter_observed_wait(IDX_SOAW);
    wait_runs_application(&OBSERVING_WAIT, alertable != 0, || o(signal, wait, ms, alertable))
}}

// The message waits live in user32. Same class-B story (count, never scale, forward untouched), and
// the same counting guard - MsgWaitForMultipleObjects reaches ...Ex on 32-bit (measured, R4/10b), not
// on 64-bit. The Ex form drops fWaitAll and reorders its args (see the fn types), and takes its
// alertable switch as the MWMO_ALERTABLE bit of its flags.

/// MWMO_ALERTABLE in MsgWaitForMultipleObjectsEx's flags (winuser.h).
const MWMO_ALERTABLE_FLAG: u32 = 0x0002;
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
    let alertable = flags & MWMO_ALERTABLE_FLAG != 0;
    wait_runs_application(&OBSERVING_WAIT, alertable, || o(count, handles, ms, wake_mask, flags))
}}

// The four waits added on 2026-09-08. Same shape as the family above and for the same reason: a
// timeout on a condition variable, an address or a socket event can be a real one, so the value is
// forwarded untouched and only the fact of the call is recorded. What changes is that a target which
// blocks on one of these is no longer invisible to the audit.
//
// `enter_observed_wait` matters for WSAWaitForMultipleEvents, which reaches WaitForMultipleObjectsEx
// (the documentation says so and R4/10b measured it on both bitnesses) - without the guard one blocked
// thread would be counted twice. This comment used to say the condition-variable waits reach the
// address wait underneath as well. Measured with every guard off, they reach no hooked wait at all:
// WaitOnAddress is a kernelbase export and they wait below it.

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
// (scale_timer_due). That conversion is needed by any session that moves the date, so these detours
// stand in EVERY session and, without scale_duration, convert the absolute due alone (R5-D2,
// `make_deadline_hooks`). One thread-local guard makes each app-level call scale exactly once and be
// counted against the export the app called: SetWaitableTimer may internally reach
// SetWaitableTimerEx, and double-scaling a due (due/M/M) would fire the timer far too early. This
// guard is separate from class A's (Sleep) and class B's (object waits) - the three families never
// cross-nest. Measured on Win11 26200 (psleep, x64+x86): SetWaitableTimer's coverage counts exactly
// the app's calls (2 via SetWaitableTimer, 1 via SetWaitableTimerEx) and the real wait scales once
// (~M, not ~M^2), so the internal path does NOT reach the exported ...Ex partner here - the guard is
// correct policy, not yet load-bearing, protecting other Windows versions and direct ...Ex callers
// (zasady/03 section 4: measured, not assumed). R4/10b measured it again with every guard off, on both
// bitnesses: still no cascade. (This comment used to add "like Sleep -> SleepEx in class A" - that
// one does cascade since the move to kernelbase.) Setting a timer runs no application code, so this
// flag has nothing to lower: a completion routine runs later, in an alertable wait of its own.

thread_local! {
    static SCALING_TIMER: Cell<bool> = const { Cell::new(false) };
}

/// Decide whether this settable-timer call is the top-level app call we should scale. Returns the
/// duration multiplier and a guard (held across the original call, so an inner cascade to the Ex
/// partner sees the flag set and passes through) when it is - None on an internal cascade (pass
/// through, uncounted) or when the core has detached (real time). Mirrors try_enter_wait on its own
/// flag. Bumps coverage only for a top-level app call (rule 4).
fn try_enter_timer(idx: usize) -> Option<FlagRestore> {
    // None is an internal cascade: pass through, do not bump.
    let guard = enter_detour(&SCALING_TIMER)?;
    bump(idx);
    // The guard is raised whether or not the core is still there. Symmetric to try_enter_wait
    // (R2-S4): with the core gone an internal cascade (SetWaitableTimer -> SetWaitableTimerEx, should
    // a Windows version take that path) must still count as ONE application call rather than two
    // (rule 4). Nothing else is decided here - the caller asks `fake_now_and_dur_m` for the clock and
    // the rate together, and a detached session answers None there, so the arguments are forwarded
    // untouched. This used to return a multiplier of its own, read from a second snapshot of the
    // control block that nothing tied to the one the due date was measured against.
    Some(guard)
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
                let rel_m = relative_of(m);
                let scaled_due = timer_due(core::ptr::read_unaligned(due), fake_now, m, rel_m);
                let scaled_period = scaled_timer_period(period, rel_m, scale_timer_period);
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
                let rel_m = relative_of(m);
                let scaled_due = timer_due(core::ptr::read_unaligned(due), fake_now, m, rel_m);
                let scaled_period = scaled_timer_period(period, rel_m, scale_timer_period);
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
    let m = dur_multiplier();
    // Windows lifts the scaled interval to its minimum, so this timer runs slower than the session.
    // Counted, because nothing else would say so (R5, D8).
    if timer_elapse_hit_floor(elapse, m) {
        note_timer_at_floor();
    }
    o(hwnd, id, scale_timer_elapse(elapse, m), timer_proc)
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
// count as GetTickCount, from the same QUIT and offset on purpose. Both exports answer "milliseconds since
// the system started", so a target reading one against the other has to see them agree - they differ by
// less than one tick in reality, and inventing that difference would need a second anchor for no gain.
// Wraps at 2^32 ms like the real one, and sooner under acceleration, which is the honest behaviour of a
// fast 32-bit counter.
//
// Once the core has gone it carries on from where the shared axis stood, at rate 1 - the same shape as
// h_tick32, so the two still agree after the session. Handing back the real value there rewound it by
// the whole acceleration (rule 3, measured 2026-09-24). The real value only when nothing could be
// released. The None arm is unreachable (make_hook fills the slot before any hook is enabled) and
// returns the real value rather than inventing a reading.
unsafe extern "system" fn h_timegettime() -> u32 { unsafe {
    let real = || O_TIMEGETTIME.get().map(|o| o()).unwrap_or(0);
    let after = || released_tick().map(|t| t as u32).unwrap_or_else(real);
    bump(IDX_TIMEGETTIME);
    if detached() {
        return after();
    }
    match ctl_ptr() {
        Some(p) => {
            let (dur, real_now, released_at) = dur_now(p as *const Ctl);
            let fake = dur.tick_at(real_now, released_at) as u32;
            // Ownership checked after the read (R2-S6), exactly as the tick detours do.
            if still_ours(p as *const Ctl) { fake } else { after() }
        }
        None => real(),
    }
}}

/// The socket driver's control code for a connection made by `connect` or `WSAConnect`.
///
/// Not documented by Microsoft. Measured on both bitnesses (2026-09-23) as the one code a connection
/// attempt through either API sends, once, and corroborated by reverse engineering of the driver,
/// which names its handler AfdConnect. The driver builds its codes as (0x12 << 12) | (op << 2) | method,
/// NOT with the usual CTL_CODE layout, which is why the value looks like device type 1.
const AFD_CONNECT: u32 = 0x12007;

/// The socket driver's control code for a connection made by `ConnectEx`, which `WSAConnectByName`,
/// `WSAConnectByList`, WinHTTP and WinINet all use. The driver names its handler AfdSuperConnect.
/// Measured and corroborated the same way as `AFD_CONNECT`.
const AFD_SUPER_CONNECT: u32 = 0x120C7;

/// Whether a device control code is a network connection attempt. Every path measured sends exactly
/// one of the two codes per attempt, never both, and a datagram sent without a connection sends
/// neither - it is not a connection.
fn is_connection_attempt(code: u32) -> bool {
    code == AFD_CONNECT || code == AFD_SUPER_CONNECT
}

// The connection observer (SourceObserved, channel `connect`): a network connection is a suspected
// SERVER time source, which no local hook can cover. Every Winsock connection attempt reaches the
// socket driver through this one function, whichever API the application called - which is why it is
// detoured here and not on ws2_32's `connect`, which two of the three ways to connect never call
// (`ChannelDef::export`). We read the control code, COUNT a connection, and forward every argument
// untouched. Like timeSetEvent: no guard, no detached check, since we never change the call.
//
// This runs for every device control call in the process, socket reads and writes included, so it is
// two comparisons and a forward: measured against 200 000 socket polls, the difference stayed inside
// the run-to-run noise on both bitnesses. The unreachable None path returns STATUS_UNSUCCESSFUL so an
// un-hooked call never fakes success.
#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn h_ntdiocf(
    file: HANDLE,
    event: HANDLE,
    apc: *const c_void,
    apc_context: *const c_void,
    io_status: *mut c_void,
    code: u32,
    input: *const c_void,
    input_len: u32,
    output: *mut c_void,
    output_len: u32,
) -> i32 { unsafe {
    let o = match O_NTDIOCF.get() {
        Some(o) => o,
        None => return STATUS_UNSUCCESSFUL,
    };
    if is_connection_attempt(code) {
        bump(IDX_CONNECT);
    }
    o(file, event, apc, apc_context, io_status, code, input, input_len, output, output_len)
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
/// `pft`, when non-null, must point to a valid FILETIME, aligned or not.
unsafe fn scale_tp_timer(pft: *const FILETIME, period: u32, window: u32) -> Option<(FILETIME, u32, u32)> { unsafe {
    if pft.is_null() {
        return None; // NULL = cancel: forward untouched
    }
    // One snapshot for both halves: the due date is absolute, so the rate it is divided by has to be
    // the rate that belongs to this `fake_now` and not to whatever the block said a moment earlier.
    let (fake_now, m) = fake_now_and_dur_m()?; // detached: forward untouched
    let rel_m = relative_of(m);
    let scaled_due = timer_due(ft_to_i64(core::ptr::read_unaligned(pft)), fake_now, m, rel_m);
    let scaled_period = scaled_timer_period(period, rel_m, scale_timer_period_ms);
    Some((i64_to_ft(scaled_due), scaled_period, scale_timer_elapse(window, rel_m)))
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
// it. We count only one that no CreateProcess detour started - since R4/11 that is a spawn with a user
// token (h_cpiw leaves those alone) or NtCreateUserProcess called straight - because the CreateProcess
// detours already inherit the session into their child. SPAWNING is a thread-local
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
/// into is not counted as a direct spawn. Cleared on drop, even if the original unwinds. A thread without
/// thread-local storage (`thread_has_tls`) reads it down: its child is still inherited, and its funnel
/// call counts as a direct one, an observed channel counted once too often rather than a child let go.
struct SpawningGuard;
impl Drop for SpawningGuard {
    fn drop(&mut self) {
        if thread_has_tls() {
            SPAWNING.set(false);
        }
    }
}
fn enter_spawning() -> SpawningGuard {
    if thread_has_tls() {
        SPAWNING.set(true);
    }
    SpawningGuard
}

/// Whether this thread is inside a CreateProcess* original call (`SpawningGuard`).
fn spawning() -> bool {
    read_flag(&SPAWNING).unwrap_or(false)
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
    let direct = !spawning();
    if direct {
        bump(IDX_NTCUP);
    }
    let status = o(
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
    );
    // A direct spawn that succeeded is a child nobody will inject into - name it in our own slot so
    // the family verdict can count a process that runs on the real clock (rule 4). This is the ONE
    // place this detour looks through a parameter: the first one is the out `PHANDLE` in every
    // published layout of this call (phnt, ReactOS, and the kernel's own prototype agree on it), and
    // the original has just written it. Read only on NT_SUCCESS (status >= 0) and only when non-null,
    // so a failed create never dereferences anything. A bad handle makes `GetProcessId` return 0,
    // which `record_uncovered_child` ignores - the failure mode is an unnamed child, not a fault.
    if direct && status >= 0 && !process_handle.is_null() {
        let child = HANDLE(core::ptr::read_unaligned(process_handle as *const *mut c_void));
        let pid = GetProcessId(child);
        if let Some(c) = live_cov() {
            record_uncovered_child(c, pid);
        }
    }
    status
}}

// --- Child inheritance (ADR-3) --------------------------------------------------
// Detour CreateProcessW so children join the session: create suspended, let the child's own
// main thread start it and park before its entry point (ADR-25), inject the same DLL, then
// let it go (handed back suspended when the caller wanted it so). The child opens the
// shared Ctl and hooks itself, so it sees the same wall clock as the parent.

/// Inject this DLL into `hproc` by writing our own module path and running LoadLibraryW there, and
/// wait for the load. Says what it came to, read the way the mechanism reads its own target's
/// (`chrono_ctl::inject`, R5-A7).
///
/// Best-effort in the sense that a failure never stops the child: this runs inside somebody else's
/// application, which asked for that process, so killing it (what `mech::prepare` does for the target
/// it launched itself) would change the behaviour under test. What the failure MUST do is get counted,
/// so the audit can say a child ran on the real clock instead of quietly reporting a smaller family
/// (R2-S2, untouchable rule 4).
///
/// The load is waited for at most `limit_ms`, what the child's start left of the injection's budget.
///
/// # Safety
/// `hproc` must be a valid process handle with injection rights.
unsafe fn inject_self(hproc: HANDLE, limit_ms: u32) -> remote_load::Outcome { unsafe {
    let gate = open_gate(hproc);
    match begin_remote_load(hproc) {
        Some(load) => end_remote_load(hproc, load, limit_ms, gate),
        None => {
            close_gate(gate);
            remote_load::Outcome::LibraryNotLoaded
        }
    }
}}

/// Open the gate the child's hook passes before it installs (ADR-25, `chrono_ctl::open_load_gate`), so
/// that a load this side stops waiting for cannot hook the child's program halfway through its run. `None`
/// when the child cannot be named or the table is full, said in the log: the load then goes ahead without
/// one.
///
/// # Safety
/// `hproc` must be a process handle with at least `PROCESS_QUERY_LIMITED_INFORMATION`.
unsafe fn open_gate(hproc: HANDLE) -> Option<usize> { unsafe {
    let ctl = ctl_ptr()?;
    let gate = process_created(hproc).and_then(|created| open_load_gate(ctl, GetProcessId(hproc), created));
    if gate.is_none() {
        log("[chrono_hook] no gate for this child's load - if the wait for it runs out, a load that ends later still hooks its program");
    }
    gate
}}

/// Give a gate back once the load it stands for has ended.
fn close_gate(gate: Option<usize>) {
    if let (Some(ctl), Some(i)) = (ctl_ptr(), gate) {
        // SAFETY: the block this process joined, and an index `open_load_gate` returned.
        unsafe { close_load_gate(ctl, i) };
    }
}

/// Wait for a remote load's thread, through the hook's own unseen wait, and read how the wait ended.
///
/// # Safety
/// `thread` must be a thread handle to wait on.
unsafe fn wait_for_load(thread: HANDLE, ms: u32) -> remote_load::Wait { unsafe {
    match wait_raw(thread, ms) {
        WAIT_OBJECT_0_CODE => remote_load::Wait::ThreadDone,
        WAIT_TIMEOUT_CODE => remote_load::Wait::TimedOut,
        _ => remote_load::Wait::Failed,
    }
}}

/// A load of this DLL started in a child: the remote thread running `LoadLibraryW` there, and the page
/// in the child that holds the path it reads.
struct RemoteLoad {
    thread: HANDLE,
    page: *mut c_void,
}

/// Write this DLL's path into `hproc` and start `LoadLibraryW` there on a remote thread. `None` when
/// nothing was started - the child is of the other bitness, or a step before the thread failed - and
/// then nothing is left behind in the child either.
///
/// # Safety
/// `hproc` must be a valid process handle with injection rights.
unsafe fn begin_remote_load(hproc: HANDLE) -> Option<RemoteLoad> { unsafe {
    let addr = *SELF_HMOD.get().unwrap_or(&0);
    if addr == 0 {
        return None;
    }
    // A child of the other bitness cannot load this library, so nothing is written into it and no thread
    // is started there (R4-N9). The remote load used to be tried anyway and to come back empty, which
    // counted the child right, but only after allocating in it and starting a thread at an address that
    // means nothing in its half of the machine.
    if bitness_differs(process_machine(hproc), process_machine(GetCurrentProcess())) {
        log("[chrono_hook] child of the other bitness - not injected, it runs on the real clock");
        return None;
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
            return None;
        }
        if n < buf.len() {
            break n;
        }
        if buf.len() >= 32_768 {
            // Past the extended-path maximum: give up, but SAY so - the audit will report the child as
            // uncovered, and without this line nobody could tell why (rule 6).
            log("[chrono_hook] own module path exceeds the path limit, child not injected");
            return None;
        }
        buf.resize(buf.len() * 4, 0);
    };
    let bytes = (n + 1) * 2; // include the NUL terminator
    let remote = VirtualAllocEx(hproc, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if remote.is_null() {
        return None;
    }
    // With no thread started, nothing reads the page, so every way out before one frees it.
    let give_up = || {
        let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
        None
    };
    if WriteProcessMemory(hproc, remote, buf.as_ptr() as *const c_void, bytes, None).is_err() {
        return give_up();
    }
    let Ok(k32) = GetModuleHandleA(s!("kernel32.dll")) else {
        return give_up();
    };
    // Check the export before transmuting (L-5, matching mech::inject): a None from GetProcAddress would
    // otherwise transmute to a null start routine and CreateRemoteThread would run at address 0, faulting
    // the child. LoadLibraryW is always present in kernel32, so this only bails on a genuine anomaly.
    let Some(loadlib) = GetProcAddress(k32, s!("LoadLibraryW")) else {
        return give_up();
    };
    let start: LPTHREAD_START_ROUTINE = Some(std::mem::transmute::<
        unsafe extern "system" fn() -> isize,
        unsafe extern "system" fn(*mut c_void) -> u32,
    >(loadlib));
    match CreateRemoteThread(hproc, None, 0, start, Some(remote as *const c_void), 0, None) {
        Ok(thread) => Some(RemoteLoad { thread, page: remote }),
        Err(_) => give_up(),
    }
}}

/// Wait for a load started by `begin_remote_load` and read how it ended, the way the mechanism reads
/// its own target's (`chrono_ctl::inject::outcome`): the child's own exit code before the thread's, so
/// a child its loader ended is not taken for one the hook loaded into, and a loader status is never
/// taken for a module (R5-A7, R5-F8). Closes the thread, and frees the page once nothing reads it.
///
/// # Safety
/// `hproc` must be the process `load` was started in, still open.
unsafe fn end_remote_load(hproc: HANDLE, load: RemoteLoad, limit_ms: u32, gate: Option<usize>) -> remote_load::Outcome { unsafe {
    // Finite wait (RELEASE-009): a child wedged in loader lock must not hang whoever waits here.
    let mut wait = wait_for_load(load.thread, limit_ms);
    let mut gate = gate;
    if wait != remote_load::Wait::ThreadDone
        && let (Some(ctl), Some(i)) = (ctl_ptr(), gate)
    {
        if abandon_load_gate(ctl, i) {
            // The child's hook refuses if this load ever reaches it, and frees the entry then.
            gate = None;
        } else {
            // Its hook passed the gate first and is installing, with the child still parked - letting the
            // child go now would run its program under detours half made. The install takes milliseconds,
            // and the limit only keeps a hook that hangs there from holding the caller for good.
            log("[chrono_hook] the hook joined the child as the wait ran out - waiting for its install");
            wait = wait_for_load(load.thread, INJECT_TIMEOUT_MS);
        }
    }
    // Signalled first, then the code: read the other way round, a child that ends between the two reads
    // would be a signalled process with the code of a live one.
    let signalled = wait_raw(hproc, 0) == WAIT_OBJECT_0_CODE;
    let mut process_code: u32 = 0;
    let process = remote_load::ProcessAfter {
        signalled,
        code: GetExitCodeProcess(hproc, &mut process_code).is_ok().then_some(process_code),
    };
    // Only a finished thread's code is a result - a running one reads 259.
    let mut thread_code: u32 = 0;
    let thread_code = (wait == remote_load::Wait::ThreadDone && GetExitCodeThread(load.thread, &mut thread_code).is_ok())
        .then_some(thread_code);
    let _ = CloseHandle(load.thread);
    // A gate not abandoned has done its work: the load ended, or its hook joined and the second wait ran
    // out too - either way nothing will pass it again.
    close_gate(gate);
    let outcome = remote_load::outcome(wait, process, thread_code);
    if remote_load::page_released_safely(outcome) {
        let _ = VirtualFreeEx(hproc, load.page, 0, MEM_RELEASE);
    } else {
        // Deliberately leaked: the path, a few hundred bytes of committed memory, in a child that is
        // already wedged in its own loader, against the chance of faulting it - `LoadLibraryW` may still
        // be reading it. Said out loud rather than done quietly (rule 6), and this is why the wait above
        // is generous: a child load costs about 68 ms, and a shorter limit would only make this branch
        // more likely.
        log("[chrono_hook] LoadLibraryW still running in the child - leaving its path buffer allocated");
    }
    match outcome {
        remote_load::Outcome::Loaded => {}
        remote_load::Outcome::TargetEnded(_) | remote_load::Outcome::TargetEndedUnread => {
            log("[chrono_hook] child ended while Windows was loading it - it never ran, nothing to cover");
        }
        _ => log("[chrono_hook] child not covered - LoadLibraryW did not load the hook there"),
    }
    outcome
}}

/// `NtQueueApcThread` as ntdll exports it: the thread, the routine, and the three arguments the dispatcher
/// passes to the routine.
type NtQueueApcFn = unsafe extern "system" fn(HANDLE, *const c_void, *const c_void, *const c_void, *const c_void) -> i32;

/// The park of a child's main thread (ADR-25): the child's own main thread starts the process, then
/// waits before its entry point while the hook is loaded. The same queue the mechanism puts on its
/// target, where the measurements and the reasons stand (`chrono-mech`, module `park`): signal `parked`,
/// wait for `go`, close both handles of the child's own. A remote thread that was the child's first
/// thread started the process itself, and a runtime that takes "the process is attaching" for its main
/// thread crashed (a Free Pascal application, measured). These are this process's handles to the two
/// events. Dropped, `go` is set - so a child whose calls are delivered late passes straight through, and
/// a suspension is never what holds it - and both are closed. No allocation: a detour of process
/// creation reaches it.
struct ChildPark {
    parked: HANDLE,
    go: HANDLE,
}

impl ChildPark {
    /// Put the park on `thread`, the child's main thread, created suspended and not resumed. `None` when
    /// it could not be put, and then nothing waits in the child: a refusal after the first call was
    /// queued leaves two handles in it, and the drop sets `go`, so what was queued passes through.
    ///
    /// # Safety
    /// `process` must be the child's handle with handle-duplication access, `thread` its main thread with
    /// the access to queue a call, and the child of this process's bitness - the calls are this process's
    /// addresses.
    unsafe fn put_on(process: HANDLE, thread: HANDLE) -> Option<Self> { unsafe {
        let ntdll = GetModuleHandleA(s!("ntdll.dll")).ok()?;
        let k32 = GetModuleHandleA(s!("kernel32.dll")).ok()?;
        let queue = GetProcAddress(ntdll, s!("NtQueueApcThread"))?;
        let queue = std::mem::transmute::<unsafe extern "system" fn() -> isize, NtQueueApcFn>(queue);
        let wait = GetProcAddress(ntdll, s!("NtWaitForSingleObject"))? as *const c_void;
        let set_event = GetProcAddress(k32, s!("SetEvent"))? as *const c_void;
        let close = GetProcAddress(k32, s!("CloseHandle"))? as *const c_void;
        let parked = CreateEventW(None, true, false, None).ok()?;
        let Ok(go) = CreateEventW(None, true, false, None) else {
            let _ = CloseHandle(parked);
            return None;
        };
        let park = ChildPark { parked, go };
        let parked_there = handle_in(process, parked)?;
        let Some(go_there) = handle_in(process, go) else {
            close_in(process, parked_there);
            return None;
        };
        // SAFETY of the queued calls: `inherit_into_child` puts a park only on a child it did not find to be
        // of the other bitness, so the routines, this process's addresses of ntdll and kernel32, are theirs in
        // the child too - the assumption the remote `LoadLibraryW` has always made. Each handle a call takes
        // is the child's own and stays open there until the last two calls close it, after the wait.
        // Each routine is called with the three arguments the dispatcher passes, the ones it does not take
        // null - the module `park` of `chrono-mech` has why that holds on x86 too.
        let one = |function: *const c_void, argument: HANDLE| {
            queue(thread, function, argument.0, std::ptr::null(), std::ptr::null()) >= 0
        };
        if !one(set_event, parked_there) {
            close_in(process, parked_there);
            close_in(process, go_there);
            return None;
        }
        // From here a queued call may use either handle, so a refusal leaves both in the child rather than
        // risk a call on a handle the child has reused.
        if queue(thread, wait, go_there.0, std::ptr::null(), std::ptr::null()) < 0
            || !one(close, parked_there)
            || !one(close, go_there)
        {
            log("[chrono_hook] the child's start could not be fully held - two handles stay in it");
            return None;
        }
        Some(park)
    }}

    /// Wait until the child's main thread has parked, the child has ended, or `ms` has passed, and read
    /// which, the child first (`chrono_ctl::inject::park_outcome`). The waits are the hook's own, unseen by
    /// the audit.
    ///
    /// # Safety
    /// `process` must be the child the park was put on, still open.
    unsafe fn wait(&self, process: HANDLE, ms: u32) -> remote_load::Park { unsafe {
        let woke = wait_any_raw(&[self.parked, process], ms);
        let wait = match woke {
            WAIT_OBJECT_0_CODE => remote_load::ParkWait::Parked,
            WAIT_TIMEOUT_CODE => remote_load::ParkWait::TimedOut,
            w if w == WAIT_OBJECT_0_CODE + 1 => remote_load::ParkWait::ProcessSignalled,
            _ => remote_load::ParkWait::Failed,
        };
        // Signalled first, then the code, as `end_remote_load` reads them.
        let signalled = wait_raw(process, 0) == WAIT_OBJECT_0_CODE;
        let mut code: u32 = 0;
        let after = remote_load::ProcessAfter {
            signalled,
            code: GetExitCodeProcess(process, &mut code).is_ok().then_some(code),
        };
        remote_load::park_outcome(wait, after)
    }}
}

impl Drop for ChildPark {
    fn drop(&mut self) {
        // SAFETY: both handles are this value's own and closed once, here.
        unsafe {
            let _ = SetEvent(self.go);
            let _ = CloseHandle(self.parked);
            let _ = CloseHandle(self.go);
        }
    }
}

/// A handle in `process` to the object `own` names here.
///
/// # Safety
/// `own` must be an open handle of this process, and `process` a process handle with handle-duplication
/// access.
unsafe fn handle_in(process: HANDLE, own: HANDLE) -> Option<HANDLE> { unsafe {
    let mut there = HANDLE::default();
    DuplicateHandle(GetCurrentProcess(), own, process, &mut there, 0, false, DUPLICATE_SAME_ACCESS)
        .ok()
        .map(|()| there)
}}

/// Close a handle of `process`'s own that nothing there uses. With no target process the source is closed
/// and nothing is duplicated.
///
/// # Safety
/// `process` must be a process handle with handle-duplication access, and `there` a handle of its own
/// that nothing in it uses or will use.
unsafe fn close_in(process: HANDLE, there: HANDLE) { unsafe {
    let _ = DuplicateHandle(process, there, HANDLE::default(), std::ptr::null_mut(), 0, false, DUPLICATE_CLOSE_SOURCE);
}}

/// Wait on two objects through the original `WaitForMultipleObjects`, unseen by the audit - the hook's own
/// wait, as `wait_raw` is for one.
///
/// # Safety
/// Both must be valid handles to wait on.
unsafe fn wait_any_raw(handles: &[HANDLE; 2], ms: u32) -> u32 { unsafe {
    unobserved(|| match O_WFMO.get() {
        Some(o) => o(2, handles.as_ptr(), 0, ms),
        None => WaitForMultipleObjects(handles, false, ms).0,
    })
}}

/// What a child's injection came to: the load's outcome once its main thread parked in time, the start's
/// when it did not, said in the hook's log either way.
///
/// The start and the load share one budget, `INJECT_TIMEOUT_MS`, counted from `began`, the `real_quit`
/// reading taken when the child was resumed for its start (`chrono_ctl::inject::budget_left_ms`), as the
/// mechanism counts its own target's. `None` is a child its caller holds: its start begins whenever the
/// caller resumes it, so the wait for its park has no limit, and that time is the caller's, not the load's.
///
/// # Safety
/// `process` must be the child the park was put on, still open.
unsafe fn load_into_parked(process: HANDLE, park: &ChildPark, began: Option<i64>) -> remote_load::Outcome { unsafe {
    let spent = || began.map_or(0, ms_since);
    let start_limit_ms = match began {
        Some(_) => budget_left_ms(spent()).unwrap_or(0),
        None => INFINITE,
    };
    let started = park.wait(process, start_limit_ms);
    match started.unloaded() {
        None => match budget_left_ms(spent()) {
            Some(limit_ms) => inject_self(process, limit_ms),
            None => {
                log("[chrono_hook] the child's start used all the time allowed for starting it and loading the hook - not injected, it runs uncovered");
                remote_load::Outcome::TimedOut
            }
        },
        Some(outcome) => {
            match outcome {
                remote_load::Outcome::TargetEnded(_) | remote_load::Outcome::TargetEndedUnread => {
                    log("[chrono_hook] child ended while Windows was starting it - it never ran, nothing to cover");
                }
                _ => log("[chrono_hook] the child's start did not finish in time - not injected, it runs uncovered"),
            }
            outcome
        }
    }
}}

/// Milliseconds since `began`, a `real_quit` reading: the real interrupt clock, which the session's own
/// scaling never reaches, so a child's budget is real time whatever rate the application runs at.
fn ms_since(began: i64) -> u64 {
    u64::try_from(real_quit().saturating_sub(began)).unwrap_or(0) / 10_000
}

/// The machine a live process runs as, or `None` when it cannot be asked. The same reading as the
/// mechanism's (`chrono-mech`, `process_machine`), which `chrono-ctl` cannot host without depending on
/// `windows`: `IsWow64Process2` names the emulated machine, or UNKNOWN for a native process, whose
/// machine is then the native one.
///
/// # Safety
/// `process` must be a process handle with at least `PROCESS_QUERY_LIMITED_INFORMATION`, or the
/// pseudo-handle of this process.
unsafe fn process_machine(process: HANDLE) -> Option<u16> { unsafe {
    let (mut own, mut native) = (IMAGE_FILE_MACHINE(0), IMAGE_FILE_MACHINE(0));
    IsWow64Process2(process, &mut own, Some(&mut native)).ok()?;
    Some(if own == IMAGE_FILE_MACHINE_UNKNOWN { native.0 } else { own.0 })
}}

/// Whether a child runs on another machine than this process. Only two known, different answers say
/// so - a machine that could not be asked leaves the injection to be tried, as it always was.
fn bitness_differs(child: Option<u16>, own: Option<u16>) -> bool {
    matches!((child, own), (Some(c), Some(o)) if c != o)
}

/// Whether a child the parent tried to inject ran on the real clock, from what the parent can see once
/// the remote load has returned (R4-S2).
///
/// A load that failed is the old answer. A load that succeeded is not enough on its own: the child's
/// install can fail after the library loaded (MinHook could not enable its detours), and the library
/// then stays, answers the load, and never publishes the child's pid. The child publishes it before
/// the load returns, so a missing pid means it is not covered - unless the registry was full, when it
/// may simply have had no slot to publish into, which `coverage.pid_registry_full` already reports.
fn child_ran_uncovered(loaded: bool, signed_in: bool, slot_claims: u32) -> bool {
    !loaded || (!signed_in && slot_claims <= MAX_COV_PIDS as u32)
}

/// The creation flags that make the caller the debugger of the child it starts.
const DEBUG_FLAGS: u32 = DEBUG_PROCESS.0 | DEBUG_ONLY_THIS_PROCESS.0;

/// After a create call we forced to CREATE_SUSPENDED returns, have the new child join the session: its own
/// main thread starts it and parks before its entry point, the hook is loaded into it, and it is let go -
/// handed back suspended when the caller originally asked for a suspended child (ADR-25). `flags` are the
/// caller's own, before CREATE_SUSPENDED was added. Shared by the three CreateProcess detours.
///
/// # Safety
/// `pi`, when non-null, must point to a PROCESS_INFORMATION filled by a successful create, aligned or
/// not.
unsafe fn inherit_into_child(r: i32, pi: *mut PROCESS_INFORMATION, flags: u32) { unsafe {
    if r == 0 || pi.is_null() {
        return;
    }
    let info = core::ptr::read_unaligned(pi);
    // Hand the child back the way it was asked for. The parent is the application under test and it asked
    // for this child - holding it or killing it would change the behaviour we were asked to observe.
    let resume = (flags & CREATE_SUSPENDED.0) == 0;
    // A child of the other bitness cannot load this library, and the park's calls are this process's
    // addresses, which mean nothing in its half of the machine: nothing is put into it (R4-N9).
    if bitness_differs(process_machine(info.hProcess), process_machine(GetCurrentProcess())) {
        log("[chrono_hook] child of the other bitness - not injected, it runs on the real clock");
        account_child(info.hProcess, info.dwProcessId, remote_load::Outcome::LibraryNotLoaded);
        if resume {
            let _ = ResumeThread(info.hThread);
        }
        return;
    }
    if flags & DEBUG_FLAGS != 0 {
        inherit_into_debugged_child(&info, resume);
        return;
    }
    // The caller stays in this call until it returns, so nothing else touches the child's thread meanwhile:
    // it is resumed for its start, and suspended again before it is let go when the caller asked for that.
    let Some(park) = ChildPark::put_on(info.hProcess, info.hThread) else {
        log("[chrono_hook] could not hold the child's start - not injected, it runs uncovered");
        account_child(info.hProcess, info.dwProcessId, remote_load::Outcome::LibraryNotLoaded);
        if resume {
            let _ = ResumeThread(info.hThread);
        }
        return;
    };
    let began = real_quit();
    if ResumeThread(info.hThread) == u32::MAX {
        log("[chrono_hook] could not resume the child for its start - not injected");
        account_child(info.hProcess, info.dwProcessId, remote_load::Outcome::LibraryNotLoaded);
        return;
    }
    let outcome = load_into_parked(info.hProcess, &park, Some(began));
    account_child(info.hProcess, info.dwProcessId, outcome);
    if !resume && SuspendThread(info.hThread) == u32::MAX {
        log("[chrono_hook] could not hand the child back suspended - it runs");
    }
    drop(park);
}}

/// A child the caller debugs, followed from a thread of its own (R5-P25, R5-D38, ADR-25).
///
/// A child's first thread raises its first debug events and stands until the debugger answers them - and
/// the debugger is the very thread that called CreateProcess, waiting in this detour. Measured on both
/// bitnesses (`tools/probes/r5/fala4/p25.ps1`) when that first thread was the remote thread loading the
/// hook: the call stood the whole ten-second limit, the child was then resumed before the hook had loaded,
/// its program read the real clock, and the report named it uncovered while the hook did load a moment
/// later. The child's own main thread starts it now and raises the same events, so the wait for its park
/// and the load stay on a thread of their own. The detour puts the park on, resumes the child when its
/// caller wanted it running, and returns. The caller goes back to answering the child's events, and the
/// new thread waits for the park, loads the hook, counts the child and lets it go - so its program starts
/// on the session clock. A caller that asked for the child suspended as well resumes it itself, and the park
/// comes once it does - however long the debugger holds it, so the wait for that park has no limit and ends
/// with the park or with the child.
///
/// Without a second handle to the child, or a thread for the wait, the child is let go uncovered and
/// counted so: waiting in the caller would stand the whole limit for a park that cannot come.
///
/// # Safety
/// `info` is the PROCESS_INFORMATION of a child just created suspended, its handles still open, of this
/// process's bitness.
unsafe fn inherit_into_debugged_child(info: &PROCESS_INFORMATION, resume: bool) { unsafe {
    // The caller owns `info`'s handles and may close them as soon as CreateProcess returns, so the
    // waiting thread needs a handle of its own. Taken before anything starts in the child.
    let Some(process) = own_handle(info.hProcess) else {
        log("[chrono_hook] no second handle to a debugged child - not injected, it runs uncovered");
        account_child(info.hProcess, info.dwProcessId, remote_load::Outcome::LibraryNotLoaded);
        if resume {
            let _ = ResumeThread(info.hThread);
        }
        return;
    };
    let park = ChildPark::put_on(process, info.hThread);
    // The child's start is under way from here when it is resumed here, so the budget a start and a load
    // share counts from here. One its caller holds starts whenever the caller resumes it.
    let began = resume.then(real_quit);
    if resume {
        let _ = ResumeThread(info.hThread);
    }
    let Some(park) = park else {
        log("[chrono_hook] could not hold a debugged child's start - not injected, it runs uncovered");
        account_child(process, info.dwProcessId, remote_load::Outcome::LibraryNotLoaded);
        let _ = CloseHandle(process);
        return;
    };
    let pending = Box::into_raw(Box::new(DebuggedChild { process, pid: info.dwProcessId, park, began }));
    match CreateThread(None, 0, Some(debugged_child_proc), Some(pending as *const c_void), THREAD_CREATION_FLAGS(0), None) {
        Ok(waiter) => {
            let _ = CloseHandle(waiter);
        }
        Err(_) => {
            log("[chrono_hook] could not start a thread for a debugged child - not injected, it runs uncovered");
            let child = Box::from_raw(pending);
            account_child(child.process, child.pid, remote_load::Outcome::LibraryNotLoaded);
        }
    }
}}

/// What the thread that follows a debugged child needs: its own handle to the child, the park put on the
/// child's main thread, and when its start began (`load_into_parked`). Dropped, the handle is closed and
/// the park lets the child go.
struct DebuggedChild {
    process: HANDLE,
    pid: u32,
    park: ChildPark,
    began: Option<i64>,
}

impl Drop for DebuggedChild {
    fn drop(&mut self) {
        // SAFETY: the handle is this value's own, taken by `own_handle`, and closed once, here. The park is
        // dropped after this body, as a field, and its events are its own.
        unsafe {
            let _ = CloseHandle(self.process);
        }
    }
}

/// Wait for a debugged child's park, load the hook, count the child, and let it go.
///
/// # Safety
/// `param` must be the `DebuggedChild` that `inherit_into_debugged_child` boxed for this thread alone, which
/// this thread then owns.
unsafe extern "system" fn debugged_child_proc(param: *mut c_void) -> u32 { unsafe {
    let child = Box::from_raw(param as *mut DebuggedChild);
    let outcome = load_into_parked(child.process, &child.park, child.began);
    account_child(child.process, child.pid, outcome);
    0
}}

/// A second handle to `handle`, with the same access, owned by the caller.
unsafe fn own_handle(handle: HANDLE) -> Option<HANDLE> { unsafe {
    let mut own = HANDLE::default();
    DuplicateHandle(GetCurrentProcess(), handle, GetCurrentProcess(), &mut own, 0, false, DUPLICATE_SAME_ACCESS)
        .ok()
        .map(|()| own)
}}

/// Count a child the hook tried to follow into, once its load has ended: nothing when it signed in,
/// or when its loader ended it before it ran (R5-F8), and an uncovered child in this process's slot
/// otherwise.
///
/// # Safety
/// `hproc` must be the child's process handle, still open.
unsafe fn account_child(hproc: HANDLE, pid: u32, outcome: remote_load::Outcome) { unsafe {
    if matches!(outcome, remote_load::Outcome::TargetEnded(_) | remote_load::Outcome::TargetEndedUnread) {
        return;
    }
    let loaded = outcome == remote_load::Outcome::Loaded;
    // Asked of the registry only after the load returned, which is after the child's install ended.
    let (signed_in, slot_claims) = match ctl_ptr() {
        Some(p) if loaded => (
            find_pid_slot(p as *const Ctl, pid, process_created(hproc)).is_some(),
            read_pid_count(p as *const Ctl),
        ),
        _ => (false, 0),
    };
    if child_ran_uncovered(loaded, signed_in, slot_claims) {
        if loaded {
            log("[chrono_hook] child loaded the hook but did not sign in - its install failed, it runs uncovered");
        }
        // Record it in OUR slot: the child never reserved one and never will, so without this the
        // process simply would not appear anywhere in the audit (R2-S2). The mechanism turns a
        // non-zero count into `inheritance.child_not_injected`, and the pid lets it NAME the
        // process that ran on the real clock (the 32-bit child of a 64-bit parent, typically).
        // Asked again rather than trusted from the caller's check: the injection can take seconds,
        // and a session that ended meanwhile must not have a child written into its slot.
        if let Some(c) = live_cov() {
            bump_uninjected_children(c);
            record_uncovered_child(c, pid);
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
    if session_over() {
        // The session let this process go, so its children are the application's business: started
        // with the caller's own flags and nothing injected (R4-W2). Following them used to put a child
        // started after the end on the session's date for good, or on the next session's.
        return o(app, cmd, pa, ta, inherit, flags, env, cwd, si, pi);
    }
    // SPAWNING held across the original: the NtCreateUserProcess it funnels into is not counted as a
    // direct spawn (this child is already being inherited below). Cleared before inherit_into_child.
    let r = {
        let _g = enter_spawning();
        o(app, cmd, pa, ta, inherit, flags | CREATE_SUSPENDED.0, env, cwd, si, pi)
    };
    inherit_into_child(r, pi, flags);
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
    if session_over() {
        return o(app, cmd, pa, ta, inherit, flags, env, cwd, si, pi); // as in h_cpw (R4-W2)
    }
    let r = {
        let _g = enter_spawning();
        o(app, cmd, pa, ta, inherit, flags | CREATE_SUSPENDED.0, env, cwd, si, pi)
    };
    inherit_into_child(r, pi, flags);
    r
}}

// CreateProcessInternalW in kernelbase is where every CreateProcess path meets, and the detours above
// stand on kernel32's exports only. Measured on both bitnesses (R4/11, `tools/probes/pspawnpaths`): a
// child started through kernelbase's own CreateProcessW/A (code that imports through the api-set),
// through WinExec, or through CreateProcessInternalW itself ran on the real clock - the session saw
// it only as a direct NtCreateUserProcess. This detour catches those. A call that already went through
// h_cpw or h_cpa finds SPAWNING raised and passes through, so a child is inherited once.
//
// A call with a user token (CreateProcessAsUserW and the like) is left as it was: observed, counted by
// h_ntcup and named as uncovered. That is how a sandbox broker starts its restricted children, and
// injecting into a process under a restricted token is its own question, not part of this change.
#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn h_cpiw(
    token: HANDLE,
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
    new_token: *mut HANDLE,
) -> i32 { unsafe {
    let o = match O_CPIW.get() {
        Some(o) => o,
        None => return 0,
    };
    if spawning() || !token.0.is_null() || pi.is_null() || session_over() {
        return o(token, app, cmd, pa, ta, inherit, flags, env, cwd, si, pi, new_token);
    }
    let r = {
        let _g = enter_spawning();
        o(token, app, cmd, pa, ta, inherit, flags | CREATE_SUSPENDED.0, env, cwd, si, pi, new_token)
    };
    inherit_into_child(r, pi, flags);
    r
}}

/// Diagnostics only (stderr-equivalent for an injected DLL) - never affects coverage.
///
/// "Never" needs the flag: with a debug monitor listening, OutputDebugStringA waits on its buffer
/// inside kernelbase through the exported WaitForSingleObjectEx, which the hook observes. A line of
/// ours must not reach the audit as a wait of the target's.
/// Write one line to the debugger's log. From a buffer on the stack, not a `CString`: a detour reaches
/// this on its cold paths - a watcher that could not start, axes released from a half-written anchor, a
/// child the target starts - and a detour does not allocate (H4, R5-A11). A message past the buffer is
/// cut there, and one with a NUL in it ends at the NUL.
fn log(msg: &str) {
    let mut line = [0u8; 512];
    let len = msg.bytes().take(line.len() - 1).take_while(|b| *b != 0).count();
    line[..len].copy_from_slice(&msg.as_bytes()[..len]);
    unobserved(|| unsafe { OutputDebugStringA(PCSTR(line.as_ptr())) })
}

/// Where a channel's detour goes, and whether every caller of the channel passes through that address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DetourSite {
    at: usize,
    every_caller: bool,
}

/// Where a channel's detour goes, from where its kernel32 entry leads (`side`, `None` for a channel
/// of any other kind, whose detour stands on its own entry).
///
/// One body under both names (`KernelBaseSide::Same`: a forwarder already resolved to kernelbase, or
/// a stub one indirect jump away) is detoured in kernelbase, where both paths meet. Otherwise the
/// detour stays on kernel32's entry, and what that covers depends on the kind:
/// - `KernelBaseBehindKernel32` with two bodies (`Separate`): the callers that go through kernel32
///   keep the session clock, but the callers that reach kernelbase through the api-set - the dynamic
///   C runtime among them - read the real one. The site does not cover every caller, so the channel
///   is reported uncovered. Until R5 (P4) the bit was set and the report said covered.
/// - `KernelBaseAndKernel32` with two bodies: kernel32's half, and `make_kernelbase_copy_hook` takes
///   kernelbase's and settles the bit.
/// - `Absent`: kernelbase has no export of that name for the api-set to reach.
///
/// Staying on kernel32 is the safety argument. An entry of a shape nobody measured keeps the detour
/// exactly where it was before the move into kernelbase, and `make_hook` does the same when the detour
/// cannot be created at the site. So the worst a Windows build we have never seen can do is the old
/// coverage, never less - and since R5 the audit says so instead of claiming the channel.
fn detour_site(module: ChannelModule, entry: usize, side: Option<KernelBaseSide>) -> DetourSite {
    match (module, side) {
        (_, Some(KernelBaseSide::Same(own))) => DetourSite { at: own, every_caller: true },
        (ChannelModule::KernelBaseBehindKernel32, Some(KernelBaseSide::Separate(_))) => {
            DetourSite { at: entry, every_caller: false }
        }
        _ => DetourSite { at: entry, every_caller: true },
    }
}

/// Whether a detour created at `placed` may set its channel's coverage bit: only when it stands at
/// a site every caller passes. A detour that could not be made in kernelbase and went onto kernel32's
/// entry instead keeps the kernel32 callers on the session clock, while the callers that reach
/// kernelbase through the api-set run past it - so the audit says uncovered (R5, P4).
fn claims_every_caller(site: DetourSite, placed: usize) -> bool {
    site.every_caller && placed == site.at
}

/// What kernelbase holds under a kernel32 export's name, seen from kernel32's entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KernelBaseSide {
    /// Kernel32's entry IS kernelbase's export (a forwarder `GetProcAddress` already resolved) or one
    /// indirect jump away from it, so one detour at this address sees both paths.
    Same(usize),
    /// Kernelbase's export is a different body from the one kernel32's entry runs.
    Separate(usize),
    /// Kernelbase is not loaded or does not export the name. Already logged.
    Absent,
}

/// Where kernelbase's export of `name` stands relative to kernel32's `entry`. The slot read cannot
/// fault: it is the very pointer the CPU loads whenever it executes this entry.
///
/// # Safety
/// `entry` must be the address of a function exported by kernel32 in this process.
unsafe fn kernelbase_side(entry: usize, name: PCSTR, label: &str) -> KernelBaseSide { unsafe {
    let Ok(kernelbase) = GetModuleHandleA(s!("kernelbase.dll")) else {
        log(&format!("[chrono_hook] kernelbase not loaded, {label} stays on kernel32"));
        return KernelBaseSide::Absent;
    };
    let Some(own) = GetProcAddress(kernelbase, name) else {
        log(&format!("[chrono_hook] kernelbase does not export {label}, it stays on kernel32"));
        return KernelBaseSide::Absent;
    };
    let own = own as *const () as usize;
    if entry == own {
        return KernelBaseSide::Same(own);
    }
    let code = core::ptr::read_unaligned(entry as *const [u8; 12]);
    if let Some(slot) = indirect_jump_slot(&code, entry, cfg!(target_pointer_width = "64"))
        && core::ptr::read_unaligned(slot as *const usize) == own
    {
        return KernelBaseSide::Same(own);
    }
    KernelBaseSide::Separate(own)
}}

/// The second detour of a `ChannelModule::KernelBaseAndKernel32` channel: kernelbase's own copy, for
/// the callers that reach it through the api-set. `make_hook` has placed the first one already, on
/// kernel32's entry, or on kernelbase's export when the two are one body, and then there is nothing
/// left to do here.
///
/// The channel counts as covered only when BOTH bodies are detoured, so this function never sets the
/// bit and takes it back when the copy cannot be detoured (`settle_second_body`). The failure of either
/// is logged, and the other one's callers keep their detour, but the audit reports the channel as
/// uncovered, because one of its two sets of callers reads the real tick count.
///
/// # Safety
/// `detour` must be correct for `slot`, and `slot` must not be the first detour's.
unsafe fn make_kernelbase_copy_hook<T: Copy>(
    pending: &mut u64,
    k32: HMODULE,
    idx: usize,
    detour: *mut c_void,
    slot: &OnceLock<T>,
) { unsafe {
    let ch = &CHANNELS[idx];
    let Ok(cname) = CString::new(ch.export()) else {
        return;
    };
    let name = PCSTR(cname.as_ptr() as *const u8);
    let Some(entry) = GetProcAddress(k32, name) else {
        return; // make_hook logged the missing export
    };
    let KernelBaseSide::Separate(own) = kernelbase_side(entry as *const () as usize, name, ch.name) else {
        return;
    };
    let created = match MinHook::create_hook(own as *mut c_void, detour) {
        Ok(original) => {
            let _ = slot.set(std::mem::transmute_copy::<*mut c_void, T>(&original));
            true
        }
        Err(e) => {
            log(&format!(
                "[chrono_hook] create_hook {} in kernelbase failed: {e:?}, a caller that goes through \
                 the api-set is not covered, so the channel is reported uncovered",
                ch.name
            ));
            false
        }
    };
    *pending = settle_second_body(*pending, ch.bit, created);
}}

/// The coverage mask once the second detour of a two-body channel has been attempted.
///
/// The bit is the audit's claim that every caller of the channel reads the session's clock, and with
/// two bodies that holds only when both are detoured. So the second body never sets the bit (a first
/// body that failed leaves it clear) and takes it back when it fails itself - the two cases where one
/// of the two sets of callers keeps reading the real tick count while the report would say covered.
fn settle_second_body(pending: u64, bit: u64, created: bool) -> u64 {
    if created { pending } else { pending & !bit }
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
        // Resolved in kernel32 first either way. The second kind then follows the entry into
        // kernelbase below, once there is an address to follow.
        ChannelModule::Kernel32
        | ChannelModule::KernelBaseBehindKernel32
        | ChannelModule::KernelBaseAndKernel32 => k32,
        ChannelModule::Ntdll => ntdll,
        // user32 may be absent in a console or service target, winmm too, and ws2_32 in a target that
        // never touches the network - each is resolved here by its name (`ChannelModule::late_dll`) rather
        // than force-loaded, because forcing a DLL the target never needed would change its behavior.
        // Absent -> honest partial, and the late scan looks again. Pinned when present, as the late scan
        // does (R5, P27): a module the target frees later would take our detour bytes with it while the
        // mask still claims the channel.
        ChannelModule::User32 | ChannelModule::Winmm | ChannelModule::Ws2_32 => {
            let pinned = match ch.module.late_dll() {
                Some(dll) => pin_module(PCSTR(dll.as_ptr())),
                None => None,
            };
            match pinned {
                Some(h) => h,
                None => {
                    log(&format!("[chrono_hook] {:?} not loaded, skipping: {}", ch.module, ch.name));
                    return;
                }
            }
        }
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
    let cname = match CString::new(ch.export()) {
        Ok(c) => c,
        Err(_) => {
            log(&format!("[chrono_hook] bad channel name: {}", ch.name));
            return;
        }
    };
    let target = match GetProcAddress(module, PCSTR(cname.as_ptr() as *const u8)) {
        Some(f) => f,
        None => {
            log(&format!("[chrono_hook] no export: {}", ch.export()));
            return;
        }
    };
    let entry = target as *const () as usize;
    let name = PCSTR(cname.as_ptr() as *const u8);
    let side = match ch.module {
        ChannelModule::KernelBaseBehindKernel32 | ChannelModule::KernelBaseAndKernel32 => {
            Some(kernelbase_side(entry, name, ch.name))
        }
        _ => None,
    };
    let site = detour_site(ch.module, entry, side);
    if !site.every_caller {
        log(&format!(
            "[chrono_hook] kernel32's {} does not lead to kernelbase, so it stays hooked on kernel32, a \
             caller that goes through the api-set is not covered, and the channel is reported uncovered",
            ch.name
        ));
    }
    // A detour that cannot be created in kernelbase (a prologue MinHook cannot relocate, another
    // hooking library there first) retries on kernel32's entry, which is where it stood before the
    // move. Without this the channel would install nowhere, and the kernel32 callers it covered
    // before would lose it - the one outcome the move promises never to cause. It does not claim the
    // channel, though: the api-set callers go straight to kernelbase (`claims_every_caller`).
    let mut placed = site.at;
    let created = match MinHook::create_hook(site.at as *mut c_void, detour) {
        Err(e) if site.at != entry => {
            log(&format!(
                "[chrono_hook] create_hook {} in kernelbase failed: {e:?}, retrying on kernel32's entry - \
                 a caller that goes through the api-set is not covered, so the channel is reported uncovered",
                ch.name
            ));
            placed = entry;
            MinHook::create_hook(entry as *mut c_void, detour)
        }
        other => other,
    };
    match created {
        Ok(original) => {
            let _ = slot.set(std::mem::transmute_copy::<*mut c_void, T>(&original));
            if claims_every_caller(site, placed) {
                *pending |= ch.bit;
            }
        }
        Err(e) => {
            // The module and the export are there, so the application can call this and will reach the
            // real function. Recorded, because for an optional module nothing else tells this apart from
            // a module it never loaded.
            HOOK_FAILED.fetch_or(ch.bit, Ordering::Relaxed);
            log(&format!("[chrono_hook] create_hook {} failed: {e:?}", ch.name));
        }
    }
}}

/// Why `install` gave up, split by what `DllMain` may still do about it.
enum InstallError {
    /// Nothing in this process has changed yet - no detour exists and nothing of ours is held - so the
    /// library may unload itself: `DllMain` answers FALSE, `LoadLibraryW` returns NULL, and whoever
    /// injected sees the load fail (MS Learn, `DllMain`). A parent that injected a child then counts it
    /// as a child that ran on the real clock, with its pid, which is the truth (R4-D18).
    Refused(String),
    /// Detours may exist. Unloading the library under them would leave jumps into freed code inside
    /// somebody else's application, so the library stays loaded, as it always did on a failure.
    Failed(String),
}

/// Why this process must not join the session the block describes, or `None` when it may (R4-D18).
///
/// A block is joinable only while its session is alive and is still the one that wrote it: the core
/// has not marked it ended, it names a core, that core can be opened and is still running, and the
/// process under that pid was created when the block says the core was. Each test is one way a block
/// outlives its session - an ordered end, a core that died, a pid the system gave to another process -
/// and in each the old hook joined anyway and kept the process on a clock nobody drove.
///
/// "Still running" is its own test because a killed core can still be OPENED: every process of its
/// session holds a handle to it for the watcher, which keeps the process object and its pid alive, and
/// asking that object for its creation time answers truthfully (measured, tools/probes/r4-6 case A2).
///
/// `created_now` is `None` when the core could not be asked (not opened, or the query failed), which
/// cannot confirm the session and so refuses too.
fn join_refusal(core: &CoreLook) -> Option<&'static str> {
    if core.ended {
        Some("the session this control block belongs to has ended")
    } else if core.pid == 0 {
        Some("the control block names no core")
    } else if !core.opened {
        Some("the core that wrote the control block is gone, or cannot be watched from this process")
    } else if !core.running {
        Some("the core that wrote the control block has exited")
    } else if core.created_now != Some(core.recorded_created) {
        Some("the process under the core's pid is not the core that wrote the control block")
    } else {
        None
    }
}

/// What `install` learned about the session's core, for `join_refusal`.
struct CoreLook {
    /// The block's end mark.
    ended: bool,
    /// The core's pid as the block records it.
    pid: u32,
    /// The core's creation time as the block records it.
    recorded_created: u64,
    /// Whether a process under that pid could be opened.
    opened: bool,
    /// Whether that process has not exited - a zero-length wait on it timed out.
    running: bool,
    /// Its creation time, when it could be read.
    created_now: Option<u64>,
}

unsafe extern "system" {
    /// MinHook's initialization, the same symbol the wrapper crate declares privately. Its status comes
    /// back as the plain number the C library returns, so a value outside the wrapper's enum can never
    /// be read into it.
    fn MH_Initialize() -> i32;
}

/// `MH_OK` in MinHook's status list.
const MH_OK: i32 = 0;

/// `MH_ERROR_ALREADY_INITIALIZED` in MinHook's status list.
const MH_ERROR_ALREADY_INITIALIZED: i32 = 1;

/// Whether MinHook stands ready after `MH_Initialize` answered `status`. Anything else - its private
/// heap could not be created - leaves no hook possible, and is a refusal before anything changed.
fn minhook_ready(status: i32) -> bool {
    status == MH_OK || status == MH_ERROR_ALREADY_INITIALIZED
}

/// Keep this library loaded until the process ends, whatever the application does (R4-N2).
///
/// A detour is a jump written into the system's own code, into this library. An application that
/// calls `FreeLibrary` on a module it finds loaded in itself - a plug-in host cleaning up, a tool that
/// unloads what it did not load - used to unmap the library under those jumps, and its next clock read
/// ran into freed memory (measured, `crates/cli/tests/hook_integrity.rs`). Pinned, a module "stays
/// loaded until the process is terminated, no matter how many times FreeLibrary is called" (MS Learn,
/// `GetModuleHandleExW`). The address is one of this library's functions, so no name is looked up.
///
/// A failure is a line in the log and not a refusal: the detours are not made yet, and a library that
/// cannot be pinned is exactly as safe as it was before this existed.
///
/// # Safety
/// Runs under the loader lock, in `install`. MS Learn's DLL best practices rule out any call that may
/// take that lock, and a module lookup likely does - but this thread already holds it, and `install`
/// has made the same kind of lookup (`GetModuleHandleA` on kernel32, ntdll, user32) from the start.
/// That a lookup re-entering a lock its own thread holds cannot deadlock is an assessment, not source.
unsafe fn pin_self() { unsafe {
    let mut module = HMODULE::default();
    let here = PCSTR(pin_self as *const () as *const u8);
    if let Err(e) =
        GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN, here, &mut module)
    {
        log(&format!(
            "[chrono_hook] could not pin the hook library ({e:?}), so an application that frees it loses its clock"
        ));
    }
}}

/// Enable every created detour, or take back the ones that went live when that fails (R4-W7).
///
/// MinHook enables the detours one by one, with every other thread frozen, and stops at the first one
/// it cannot write, leaving the ones before it live (`EnableAllHooksLL`, minhook-0.9.0 `hook.c`). Those
/// would put part of the application on the session's clock while the audit, which publishes nothing
/// after a failure, says none of it is. Disabling writes the original bytes back and keeps every
/// trampoline, so a detour that somehow stayed live still has an original to call (`remove_hook` would
/// free them). The answer names what happened, for the log.
fn enable_or_take_back<E: std::fmt::Debug>(
    enable: impl FnOnce() -> Result<(), E>,
    disable: impl FnOnce() -> Result<(), E>,
) -> Result<(), String> {
    let Err(e) = enable() else {
        return Ok(());
    };
    Err(match disable() {
        Ok(()) => format!("enable_all_hooks: {e:?}, every detour that went live was taken back"),
        Err(d) => format!(
            "enable_all_hooks: {e:?}, and taking the live ones back failed too ({d:?}), so part of the \
             application may read the session's clock while the audit reports no channel"
        ),
    })
}

/// Enable the late detours with one freeze, and one at a time only when that fails (R4-W7). Answers the
/// bits that went live and the bits that did not.
///
/// One freeze for the batch because each costs tens of milliseconds - measured at about 42 ms on the
/// owner's machine, most of it MinHook's snapshot of every thread in the system - and a detour switched
/// on after it leaves its channel on the real clock that much longer. Six detours one by one took about
/// 257 ms. Not `enable_all_hooks`, though: a take-back through `disable_all_hooks` would also switch off
/// every detour `install` enabled at startup. `batch` is MinHook's queue, which applies only what was
/// queued, and `one` is the fallback for a batch that stopped part-way.
fn enable_late(
    created: &[(u64, usize)],
    batch: impl FnOnce(&[(u64, usize)]) -> bool,
    mut one: impl FnMut(u64, usize) -> bool,
) -> (u64, u64) {
    let attempted = created.iter().fold(0, |mask, &(bit, _)| mask | bit);
    if attempted == 0 {
        return (0, 0);
    }
    if batch(created) {
        return (attempted, 0);
    }
    let live = created.iter().filter(|&&(bit, target)| one(bit, target)).fold(0, |mask, &(bit, _)| mask | bit);
    (live, attempted & !live)
}

/// The batch half of `enable_late`: queue every late detour, then apply the queue under one freeze.
///
/// The queue stops at the first detour it cannot write and leaves the ones before it live
/// (`MH_ApplyQueued`, minhook-0.9.0 `hook.c`), which is why a failure goes on to each detour's own enable
/// rather than being read as "none of them". A detour that could not be queued is not applied here, and
/// its own enable reaches it the same way.
///
/// # Safety
/// Every address must be a detour `late_one` created.
unsafe fn queue_and_apply(created: &[(u64, usize)]) -> bool { unsafe {
    created.iter().all(|&(_, target)| MinHook::queue_enable_hook(target as *mut c_void).is_ok())
        && MinHook::apply_queued().is_ok()
}}

/// The fallback half of `enable_late`: one detour's own enable, after a batch that failed part-way.
///
/// "Already on" is a detour the batch switched on before it stopped, and it is live. Counting it as
/// failed would do worse than put a wrong line in the report: the take-out below, applied by the next
/// module's batch, would switch off a detour the coverage mask claims. A detour that really failed is
/// taken out of the queue for the same reason, so no later batch switches it on behind the mask's back.
///
/// # Safety
/// `target` must be a detour `late_one` created.
unsafe fn enable_one_late(bit: u64, target: usize) -> bool { unsafe {
    let answer = MinHook::enable_hook(target as *mut c_void);
    if went_live(&answer) {
        return true;
    }
    let _ = MinHook::queue_disable_hook(target as *mut c_void);
    log(&format!("[chrono_hook] late: enable_hook for channel bit 0x{bit:x} failed: {answer:?}"));
    false
}}

/// Whether one detour's own enable left it live: switched on now, or already on.
fn went_live(answer: &Result<(), MH_STATUS>) -> bool {
    matches!(answer, Ok(()) | Err(MH_STATUS::MH_ERROR_ENABLED))
}

/// What `DllMain` answers for `DLL_PROCESS_ATTACH` after `install`: TRUE unless the install refused
/// before anything changed (see `InstallError`).
fn attach_answer(installed: &Result<(), InstallError>) -> i32 {
    match installed {
        Err(InstallError::Refused(_)) => 0,
        Ok(()) | Err(InstallError::Failed(_)) => 1,
    }
}

/// The duration axis, and the waits and timers that ride it, created only for a session that asked to
/// scale durations. One opt-in group, kept out of `install` so that the install stays readable.
///
/// The anchor lives in the shared Ctl: the core initialized it in
/// prepare (from the real GetTickCount64 / QUIT, before the target ran) and rebases it on every
/// set_multiplier, so a speed change never rewinds the axis (H-1). No per-process capture here - the
/// detours read it under the same seqlock as the wall multiplier. QPC stays real unless its own
/// opt-in asks otherwise (ADR-2) - timeGetTime rides this axis, sharing GetTickCount's base.
///
/// # Safety
/// Called from `install` only, under the same conditions as `make_hook`.
unsafe fn make_duration_hooks(pending: &mut u64, k32: HMODULE, ntdll: HMODULE) { unsafe {
    make_hook(pending, k32, ntdll, IDX_GTC64, h_tick as *const () as *mut c_void, &O_TICK);
    make_kernelbase_copy_hook(pending, k32, IDX_GTC64, h_tick_kb as *const () as *mut c_void, &O_TICK_KB);
    make_hook(pending, k32, ntdll, IDX_GTC, h_tick32 as *const () as *mut c_void, &O_TICK32);
    make_kernelbase_copy_hook(pending, k32, IDX_GTC, h_tick32_kb as *const () as *mut c_void, &O_TICK32_KB);
    make_hook(pending, k32, ntdll, IDX_QUIT, h_quit as *const () as *mut c_void, &O_QUIT);
    // The interrupt-time clocks (R5-F4), in kernelbase alone.
    make_hook(pending, k32, ntdll, IDX_QIT, h_qit as *const () as *mut c_void, &O_QIT);
    make_hook(pending, k32, ntdll, IDX_QITP, h_qitp as *const () as *mut c_void, &O_QITP);
    make_hook(pending, k32, ntdll, IDX_QUITP, h_quitp as *const () as *mut c_void, &O_QUITP);
    make_hook(pending, k32, ntdll, IDX_SLEEP, h_sleep as *const () as *mut c_void, &O_SLEEP);
    make_hook(pending, k32, ntdll, IDX_SLEEPEX, h_sleepex as *const () as *mut c_void, &O_SLEEPEX);
    make_hook(pending, k32, ntdll, IDX_WFSO, h_wfso as *const () as *mut c_void, &O_WFSO);
    make_hook(pending, k32, ntdll, IDX_WFSOEX, h_wfsoex as *const () as *mut c_void, &O_WFSOEX);
    make_hook(pending, k32, ntdll, IDX_WFMO, h_wfmo as *const () as *mut c_void, &O_WFMO);
    make_hook(pending, k32, ntdll, IDX_WFMOEX, h_wfmoex as *const () as *mut c_void, &O_WFMOEX);
    make_hook(pending, k32, ntdll, IDX_SOAW, h_soaw as *const () as *mut c_void, &O_SOAW);
    make_hook(pending, k32, ntdll, IDX_MWFMO, h_mwfmo as *const () as *mut c_void, &O_MWFMO);
    make_hook(pending, k32, ntdll, IDX_MWFMOEX, h_mwfmoex as *const () as *mut c_void, &O_MWFMOEX);
    // The four waits that used to be neither scaled nor observed. They ride the same opt-in as the
    // rest of the wait family: a session that did not ask for the duration axis is not watching
    // waits at all. Three resolve in kernelbase and the socket one in ws2_32, which is optional and
    // therefore also in the late scan below (ADR-10) - without that entry this would repeat exactly
    // the gap ADR-10 was written to close.
    make_hook(pending, k32, ntdll, IDX_SCVSRW, h_scvsrw as *const () as *mut c_void, &O_SCVSRW);
    make_hook(pending, k32, ntdll, IDX_SCVCS, h_scvcs as *const () as *mut c_void, &O_SCVCS);
    make_hook(pending, k32, ntdll, IDX_WOA, h_woa as *const () as *mut c_void, &O_WOA);
    make_hook(pending, k32, ntdll, IDX_WSAWFME, h_wsawfme as *const () as *mut c_void, &O_WSAWFME);
    make_hook(pending, k32, ntdll, IDX_SETTIMER, h_settimer as *const () as *mut c_void, &O_SETTIMER);
    make_hook(pending, k32, ntdll, IDX_TIMESETEVENT, h_timesetevent as *const () as *mut c_void, &O_TIMESETEVENT);
    make_hook(pending, k32, ntdll, IDX_TIMEGETTIME, h_timegettime as *const () as *mut c_void, &O_TIMEGETTIME);
}}

/// The detours that carry an ABSOLUTE deadline (`chrono_ctl::DEADLINE_CHANNELS`), created in EVERY session
/// (R5-D2). A deadline the application computed from the fake clock is compared by the kernel with the real
/// one, which a plain date shift breaks as surely as an accelerated clock: a timer for "tomorrow 03:00" on a
/// clock at 2038 waited twelve real years in a session without `--scale-duration`. Without that flag these
/// detours convert the absolute deadline alone and leave every relative time as the application asked
/// (`relative_rate`).
///
/// # Safety
/// Called from `install` only, under the same conditions as `make_hook`.
unsafe fn make_deadline_hooks(pending: &mut u64, k32: HMODULE, ntdll: HMODULE) { unsafe {
    make_hook(pending, k32, ntdll, IDX_NTDELAY, h_ntdelay as *const () as *mut c_void, &O_NTDELAY);
    make_hook(pending, k32, ntdll, IDX_SWT, h_swt as *const () as *mut c_void, &O_SWT);
    make_hook(pending, k32, ntdll, IDX_SWTEX, h_swtex as *const () as *mut c_void, &O_SWTEX);
    make_hook(pending, k32, ntdll, IDX_TPTIMER, h_set_tp_timer as *const () as *mut c_void, &O_TPTIMER);
    make_hook(pending, k32, ntdll, IDX_TPTIMEREX, h_set_tp_timer_ex as *const () as *mut c_void, &O_TPTIMEREX);
}}

/// The wall clock and the session zone - the channels every session substitutes, whatever it asked for.
/// One group like the duration axis and the deadlines, kept out of `install` so that the install stays
/// under its pinned length.
///
/// # Safety
/// Called from `install` only, under the same conditions as `make_hook`.
unsafe fn make_wall_hooks(pending: &mut u64, k32: HMODULE, ntdll: HMODULE) { unsafe {
    make_hook(pending, k32, ntdll, IDX_GSTAFT, h_gstaft as *const () as *mut c_void, &O_GSTAFT);
    make_hook(pending, k32, ntdll, IDX_GSTPAFT, h_gstpaft as *const () as *mut c_void, &O_GSTPAFT);
    make_hook(pending, k32, ntdll, IDX_GST, h_gst as *const () as *mut c_void, &O_GST);
    make_hook(pending, k32, ntdll, IDX_GLT, h_glt as *const () as *mut c_void, &O_GLT);
    make_hook(pending, k32, ntdll, IDX_NTQST, h_ntqst as *const () as *mut c_void, &O_NTQST);
    make_hook(pending, k32, ntdll, IDX_NTQSI, h_ntqsi as *const () as *mut c_void, &O_NTQSI);
    make_hook(pending, k32, ntdll, IDX_GTZI, h_gtzi as *const () as *mut c_void, &O_GTZI);
    make_hook(pending, k32, ntdll, IDX_GDTZI, h_gdtzi as *const () as *mut c_void, &O_GDTZI);
    make_hook(pending, k32, ntdll, IDX_GTZIFY, h_gtzify as *const () as *mut c_void, &O_GTZIFY);
    make_hook(pending, k32, ntdll, IDX_STSL, h_stsl as *const () as *mut c_void, &O_STSL);
    make_hook(pending, k32, ntdll, IDX_STSLEX, h_stslex as *const () as *mut c_void, &O_STSLEX);
    make_hook(pending, k32, ntdll, IDX_FTLFT, h_ftlft as *const () as *mut c_void, &O_FTLFT);
    make_hook(pending, k32, ntdll, IDX_LFTFT, h_lftft as *const () as *mut c_void, &O_LFTFT);
    make_hook(pending, k32, ntdll, IDX_TLTST, h_tltst as *const () as *mut c_void, &O_TLTST);
    make_hook(pending, k32, ntdll, IDX_TLTSTEX, h_tltstex as *const () as *mut c_void, &O_TLTSTEX);
}}

/// What `install` reads once, before any detour exists: whether the session scales durations, which the
/// deadline detours ask on every call (R5-D2), and the precise clock the anchors stand on, resolved in
/// kernelbase while no detour can be in the way (R5-P2) - kernel32 does not export it.
///
/// # Safety
/// Called from `install` only, with `ctl` the session's mapped block.
unsafe fn read_session_clocks(ctl: *const Ctl) { unsafe {
    SCALE_DUR.store(read_scale_dur(ctl), Ordering::Relaxed);
    if let Ok(kernelbase) = GetModuleHandleA(s!("kernelbase.dll"))
        && let Some(f) = GetProcAddress(kernelbase, s!("QueryUnbiasedInterruptTimePrecise"))
    {
        let _ = QUITP_EXPORT.set(std::mem::transmute::<unsafe extern "system" fn() -> isize, InterruptFn>(f));
    }
}}

/// The QPC axis (opt-in `scale_qpc`, ADR-2 reversal). SEPARATE from scale_duration because scaling QPC also
/// scales a target's QPC-timed rendering. It IS a coverage channel (R2-S3): installed through make_hook like
/// the rest, so a failure to install shows up as an uncovered channel instead of a debug string nobody
/// reads. The anchor lives in the shared Ctl (core initialized it in prepare, rebases on set_multiplier).
/// QueryPerformanceFrequency is left real.
///
/// R5-F5. On 32-bit Windows the system computes the two precise interrupt clocks through the EXPORTED QPC,
/// so with the QPC detour standing and the duration axis left real they followed the scaled counter: 199 s
/// off after a 3 s session at x60, and still off once the session had let the application go (measured,
/// tools/probes/r5/fala3/f5.ps1). They are hooked here to be answered real, through `own_clock`. Under
/// scale_duration `make_duration_hooks` has made them already, to be scaled.
///
/// # Safety
/// Called from `install` only, under the same conditions as `make_hook`.
unsafe fn make_qpc_hooks(pending: &mut u64, k32: HMODULE, ntdll: HMODULE) { unsafe {
    make_hook(pending, k32, ntdll, IDX_QPC, h_qpc as *const () as *mut c_void, &O_QPC);
    if !SCALE_DUR.load(Ordering::Relaxed) {
        make_hook(pending, k32, ntdll, IDX_QITP, h_qitp as *const () as *mut c_void, &O_QITP);
        make_hook(pending, k32, ntdll, IDX_QUITP, h_quitp as *const () as *mut c_void, &O_QUITP);
    }
}}

/// Install and enable every channel's detour, wiring this process to the shared anchor.
///
/// INVARIANT (P6, docs/06 ADR-3, ADR-25): injection assumes the target's program has not run - the
/// parent is created `CREATE_SUSPENDED`, and children are forced `CREATE_SUSPENDED` in `h_cpw`/`h_cpa`.
/// Its main thread starts the process and parks before the entry point, and only then is the hook
/// loaded. This runs from `DLL_PROCESS_ATTACH` under the loader lock, and `MinHook::enable_all_hooks`
/// suspends/resumes threads - safe ONLY while no application thread runs code of the program. Not quite
/// "no code of the target has run" (R4-N20): the start-up code of its static imports and its TLS
/// callbacks ran during the start, on the real clock. The threads here are the main thread, blocked in
/// the park's wait inside an asynchronous call (its address the return after the system call, past any
/// prologue a detour patches, and holding no lock), and a thread one of the start-up routines started.
/// Do NOT add a path that injects into an already-running, multi-threaded process without moving
/// hook-enabling off the loader lock (the watcher thread is created OUTSIDE DllMain, in `ensure_watcher`,
/// for exactly this reason).
unsafe fn install() -> Result<(), InstallError> { unsafe {
    let hmap = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(chrono_ctl::CTL_SECTION_NAME_W.as_ptr()))
        .map_err(|e| InstallError::Refused(format!("OpenFileMappingW: {e:?}")))?;
    let view = MapViewOfFile(hmap, FILE_MAP_ALL_ACCESS, 0, 0, chrono_ctl::ctl_size());
    if view.Value.is_null() {
        // Close the mapping we opened a line ago. Once the view is mapped the handle is deliberately
        // kept for the process's life (the view outlives it either way), but on this path there is no
        // view - the handle would just sit there for as long as the target runs.
        let _ = CloseHandle(hmap);
        return Err(InstallError::Refused("MapViewOfFile returned null".into()));
    }
    let ctl = view.Value as *mut Ctl;
    let give_back = |core: Option<HANDLE>| {
        if let Some(h) = core {
            let _ = CloseHandle(h);
        }
        let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
        let _ = CloseHandle(hmap);
    };
    // Before ANY field is read as a time. The section name is fixed and creatable by any process in
    // this session, so what is mapped here is not guaranteed to be the block our mechanism wrote -
    // and every number below decides either what the target's clock says (untouchable rule 2) or
    // what the audit reports about it (rule 4). Refusing is the honest outcome: the target then runs
    // on the real clock and the driver reports the injection as failed, rather than the target
    // silently running on a stranger's anchor while the report calls the session covered.
    if !header_is_ours(ctl as *const Ctl) {
        give_back(None);
        return Err(InstallError::Refused("session control block is not this build's".into()));
    }

    // Whether the session this block describes is alive, and is the one that wrote it (R4-D18). The
    // block outlives its core for as long as any process of the session keeps it mapped, so a process
    // that session left running can start a child long after the end - and that child used to find the
    // block, join it, and stay on a clock nobody drives, or on the NEXT session's clock once a new core
    // had reclaimed the block (measured before the fix, tools/probes/r4-6). All of it is decided here,
    // before a single detour exists, so a refusal leaves the process exactly as it found it.
    //
    // The handle asks for `QUERY_LIMITED` beside `SYNCHRONIZE`: the creation time needs it, and so does
    // the exit code the watcher reads when a wait fails (R4-N6).
    let core_pid = read_core_pid(ctl as *const Ctl);
    let core_created = read_core_created(ctl as *const Ctl);
    let ended = read_ended(ctl as *const Ctl);
    let core = if core_pid == 0 {
        None
    } else {
        OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, core_pid).ok()
    };
    // No detour exists in this process yet, so this is the real wait, and nothing counts it.
    let look = CoreLook {
        ended,
        pid: core_pid,
        recorded_created: core_created,
        opened: core.is_some(),
        running: core.is_some_and(|h| WaitForSingleObject(h, 0).0 == WAIT_TIMEOUT_CODE),
        created_now: core.and_then(|h| process_created(h)),
    };
    // The gate is passed whatever the session's answer, and before it: a pending one is joined and goes
    // back to the parent when the load returns, refused or not, and one the parent abandoned is freed as
    // this hook refuses - a refusal for its session included, since nothing else would ever free it.
    let pid = GetCurrentProcessId();
    let own_created = process_created(GetCurrentProcess()).unwrap_or(0);
    let late = late_refusal(ctl, pid, own_created);
    if let Some(why) = join_refusal(&look).or(late) {
        give_back(core);
        return Err(InstallError::Refused(why.into()));
    }
    // MinHook's own state, made here where a failure can still be a refusal (R4-N1). The wrapper crate
    // makes this call itself inside its first `create_hook`, and answers every failure but "already
    // initialized" with a panic, which cannot unwind out of `DllMain` and ends the application instead.
    // Asked first, the wrapper then only ever sees "already initialized", which it ignores.
    let status = MH_Initialize();
    if !minhook_ready(status) {
        give_back(core);
        return Err(InstallError::Refused(format!("MH_Initialize answered {status}")));
    }
    // Past the last refusal, so a refused library still unloads (R4-N2).
    pin_self();
    let _ = CTL_PTR.set(view.Value as usize);
    let _ = TZ_BIAS.set(read_tz_bias(ctl as *const Ctl));
    // The session we joined, for `still_ours` (R2-S6), and the core the watcher waits on.
    let _ = CORE_PID.set(core_pid);
    if let Some(h) = core {
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
    let cov_slot: Option<usize> = reserve_cov_slot(ctl);
    let cov: Option<*mut Cov> = match cov_slot {
        Some(slot) => {
            let cptr = cov_at_mut(ctl, slot);
            // Which process this slot is, beyond a pid the system will hand out again once we are gone
            // (R4-N12). Before the pid is published, so the mechanism never reads one without the other.
            set_created(cptr, own_created);
            // Whether this is a windowed or a console program, which lets the core tell a hand-off from a
            // console tool that simply ended (ADR-4, 2026-10-27).
            set_subsystem(cptr, own_subsystem());
            let _ = COV_PTR.set(cptr as usize);
            let _ = COV_SLOT.set(slot);
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
            // `Failed`, not `Refused`, though no detour exists yet: the statics above already point at
            // the block and the slot, and unwinding them for a failure that cannot realistically happen
            // is more code on the loader lock than the case is worth. The library stays loaded, unhooked.
            return Err(InstallError::Failed(format!("GetModuleHandleA: kernel32 {k:?}, ntdll {n:?}")));
        }
    };

    read_session_clocks(ctl as *const Ctl);

    // Channels whose detour was CREATED. Published to the Cov only after enable_all_hooks succeeds -
    // until then no detour is live, and a bit that says otherwise would be the audit lying (rule 4).
    let mut pending: u64 = 0;

    // The wall clock and the session zone, in every session - see `make_wall_hooks`.
    make_wall_hooks(&mut pending, k32, ntdll);

    // The absolute deadlines, in every session (R5-D2) - see `make_deadline_hooks`.
    make_deadline_hooks(&mut pending, k32, ntdll);

    // Duration axis (opt-in), with the waits and timers that ride it - see `make_duration_hooks`.
    if read_scale_dur(ctl as *const Ctl) {
        make_duration_hooks(&mut pending, k32, ntdll);
    }

    // QPC axis (opt-in `scale_qpc`, ADR-2 reversal) - see `make_qpc_hooks`.
    if read_scale_qpc(ctl as *const Ctl) {
        make_qpc_hooks(&mut pending, k32, ntdll);
    }

    // Direct process creation (ADR-3, observed): hook NtCreateUserProcess ALWAYS - not gated by
    // scale_duration, since process creation is watched regardless. It only counts a direct call and
    // forwards untouched - the SPAWNING guard keeps the CreateProcess* funnel from counting here.
    make_hook(&mut pending, k32, ntdll, IDX_NTCUP, h_ntcup as *const () as *mut c_void, &O_NTCUP);

    // Suspected time source (Etap 2, observed): watch network connections ALWAYS - the network is
    // watched regardless of scale_duration. The detour sits in ntdll, where every connection attempt
    // passes whichever API made it, only counts one (a suspected server time source we cannot cover)
    // and forwards untouched - the audit warns source.network_at_start.
    make_hook(&mut pending, k32, ntdll, IDX_CONNECT, h_ntdiocf as *const () as *mut c_void, &O_NTDIOCF);

    // Child inheritance (ADR-3): hook CreateProcessW and CreateProcessA so the whole
    // process tree joins the session whichever spawn API the parent uses. Not coverage
    // channels - plumbing, not time sources. (CreateProcessA funnels through CreateProcessInternalW, not
    // the W export, so the two never re-enter each other. Both reach h_cpiw below with SPAWNING raised.)
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
    // The funnel under both, in kernelbase, for the paths that never meet kernel32's exports (h_cpiw).
    if let Ok(kernelbase) = GetModuleHandleA(s!("kernelbase.dll"))
        && let Some(cpiw) = GetProcAddress(kernelbase, s!("CreateProcessInternalW"))
    {
        match MinHook::create_hook(cpiw as *const () as *mut c_void, h_cpiw as *const () as *mut c_void) {
            Ok(original) => {
                let _ = O_CPIW.set(std::mem::transmute::<*mut c_void, CpiwFn>(original));
            }
            Err(e) => log(&format!("[chrono_hook] create_hook CreateProcessInternalW failed: {e:?}")),
        }
    } else {
        log("[chrono_hook] kernelbase has no CreateProcessInternalW, children started past kernel32 stay uncovered");
    }

    // Enable every prepared detour, THEN publish what is actually live. Until this call returns Ok,
    // the trampolines exist but no detour runs, so nothing may be claimed as covered. If it fails
    // (an AV blocking the write to the code section, a CFG conflict, another hooking library in the
    // process) the target keeps running on real time - DllMain cannot undo a load - so the honest
    // report is zero covered channels, which the mechanism turns into a failing verdict rather than
    // a silent "works" over a session that substituted nothing (rule 4).
    if let Err(why) = enable_or_take_back(|| MinHook::enable_all_hooks(), || MinHook::disable_all_hooks()) {
        if let Some(c) = cov {
            // Explicit, not merely "we never wrote": a reader that somehow saw this slot must read
            // zero covered channels, not a claim we cannot back. We also return without publishing
            // our PID, so the mechanism never looks at the slot at all.
            set_channels_installed(c, 0);
        }
        return Err(InstallError::Failed(why));
    }
    if let Some(c) = cov {
        set_channels_installed(c, pending);
        set_failed_channels(c, HOOK_FAILED.load(Ordering::Relaxed));
    }

    // Hand the watcher whatever this session WANTED from an optional module and did not get. The set
    // is computed here rather than in the watcher so the opt-in gates are stated once: without
    // scale_duration the duration and observed-time channels are not wanted at all, and looking for
    // them later would install channels the session deliberately did not ask for. Every late channel
    // rides that opt-in now - the connection observer lives in ntdll, which is never late - so a
    // session without it leaves the watcher nothing to look for.
    //
    // `INSTALL_DONE` is released LAST, after the mask store above, and that ordering is the whole
    // point of the flag (R1): until it is set the watcher will not touch the Cov, so a late bit
    // cannot be ORed in and then wiped by our own store.
    let wanted_late = if read_scale_dur(ctl as *const Ctl) { LATE_CHANNELS } else { 0 };
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

/// Whether a detach is this process's own exit, which the hook marks for the core (ADR-4, 2026-11-12): the
/// loader passes a `reserved` that is not null when the process is ending and null for a `FreeLibrary`, and
/// only a slot that still names this process is this process's to mark - a block a new session reclaimed
/// may have given the slot to another.
fn marks_exit(reserved_is_null: bool, slot_pid: u32, own_pid: u32) -> bool {
    !reserved_is_null && own_pid != 0 && slot_pid == own_pid
}

/// Why a hook that comes late does not join (ADR-25): the process that started this one stopped waiting
/// for this load and let its program run, and counts it as running on the real clock - so the hook
/// refuses before anything changes rather than hook a program halfway through its run
/// (`chrono_ctl::pass_load_gate`, which frees the gate as it refuses).
///
/// # Safety
/// `ctl` must point to the live block `install` mapped and checked as this build's.
unsafe fn late_refusal(ctl: *mut Ctl, pid: u32, created: u64) -> Option<&'static str> { unsafe {
    (pass_load_gate(ctl, pid, created) == LoadGate::Refused)
        .then_some("the process that started this one stopped waiting for this load and let its program run")
}}

/// The real `QueryPerformanceCounter`: through the trampoline when the counter is hooked, the export when
/// it is not - never the detour, so the value is the system's own whatever the session does to the
/// counter. `real_qpc` answers 0 without the trampoline, for the release, which has nothing to do then.
fn real_counter() -> i64 {
    let mut t: i64 = 0;
    // SAFETY: both write one counter value to a local that lives through the call.
    unsafe {
        match O_QPC.get() {
            Some(o) => {
                o(&mut t);
            }
            None => {
                let _ = QueryPerformanceCounter(&mut t);
            }
        }
    }
    t
}

/// Mark the moment this process began to exit in its slot, for the core's guard window (ADR-4). Runs under
/// the loader lock as the process ends, so it allocates nothing, touches no thread-local storage, logs
/// nothing and takes no lock: two reads of the block and one atomic store. The core reads the mark only
/// for the process it launched and only while it waits on the window, so a mark nobody reads costs one
/// counter read at the end of a process.
///
/// # Safety
/// Called from `DllMain` for `DLL_PROCESS_DETACH`, with whether its `reserved` was null.
unsafe fn mark_own_exit(reserved_is_null: bool) { unsafe {
    let (Some(ctl), Some(&slot)) = (ctl_ptr(), COV_SLOT.get()) else {
        return;
    };
    if slot >= MAX_COV_PIDS || !marks_exit(reserved_is_null, read_pid(ctl, slot), GetCurrentProcessId()) {
        return;
    }
    mark_exit_began(cov_at_mut(ctl, slot), real_counter());
}}

#[unsafe(no_mangle)]
pub extern "system" fn DllMain(hinst: HMODULE, reason: u32, reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_DETACH {
        // SAFETY: this is `DllMain` for `DLL_PROCESS_DETACH`.
        unsafe { mark_own_exit(reserved.is_null()) };
        return 1;
    }
    if reason != DLL_PROCESS_ATTACH {
        return 1; // the answer is ignored for every other reason (MS Learn, `DllMain`)
    }
    // Remember our own module so we can inject the same DLL into children (ADR-3).
    let _ = SELF_HMOD.set(hinst.0 as usize);
    // And the thread that loads it, which is the injector's, before any detour stands.
    // SAFETY: GetCurrentThreadId reads this thread's TEB and takes no argument.
    LOADER_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    let installed = unsafe { install() };
    match &installed {
        Ok(()) => {}
        Err(InstallError::Refused(why)) => log(&format!("[chrono_hook] not joining this session: {why}")),
        Err(InstallError::Failed(why)) => log(&format!("[chrono_hook] install failed: {why}")),
    }
    attach_answer(&installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R5-A11: every channel whose module may arrive late has a site in the late table, and the table holds
    /// nothing else. A channel added to user32, winmm or ws2_32 joins `LATE_CHANNELS` by itself (it is read
    /// off the channel table), so without a site here it would be owed to the watcher for ever.
    #[test]
    fn every_late_channel_has_a_site_and_the_table_holds_only_late_channels() {
        for (idx, ch) in CHANNELS.iter().enumerate() {
            assert_eq!(late_site(idx).is_some(), LATE_CHANNELS & ch.bit != 0, "{}", ch.name);
        }
        assert_eq!(LATE_CHANNELS.count_ones(), 6, "six channels live in a module that may arrive late");
    }

    /// The exit is marked only for the process's own end, never for an unload, and only in a slot that
    /// still names this process - a reclaimed block may have given it to another (ADR-4).
    #[test]
    fn only_the_process_s_own_end_in_its_own_slot_is_marked() {
        assert!(marks_exit(false, 42, 42), "the process ends, its slot names it");
        assert!(!marks_exit(true, 42, 42), "a FreeLibrary is not the end of the process");
        assert!(!marks_exit(false, 43, 42), "the slot names another process now");
        assert!(!marks_exit(false, 0, 42), "the slot was never published");
        assert!(!marks_exit(false, 0, 0), "no pid to compare is no mark");
    }

    /// A block is joined only while its session is alive and is the one that wrote it (R4-D18). Each
    /// refusal is one way the block outlives its session, and the old hook joined in every one of them.
    #[test]
    fn a_block_is_joined_only_while_its_own_session_is_alive() {
        const WHEN: u64 = 0x01DC_3000_0000_0000;
        let live = || CoreLook {
            ended: false,
            pid: 4242,
            recorded_created: WHEN,
            opened: true,
            running: true,
            created_now: Some(WHEN),
        };
        assert_eq!(join_refusal(&live()), None, "a live session was refused");
        let cases: [(&str, CoreLook); 7] = [
            ("an ended session was joined", CoreLook { ended: true, ..live() }),
            ("a block naming no core was joined", CoreLook { pid: 0, opened: false, running: false, created_now: None, ..live() }),
            ("a core that cannot be opened was trusted", CoreLook { opened: false, running: false, created_now: None, ..live() }),
            ("a killed core, still open through its session's handles, was trusted", CoreLook { running: false, ..live() }),
            ("a recycled pid passed for the core that wrote the block", CoreLook { created_now: Some(WHEN + 1), ..live() }),
            ("a core whose creation time could not be read was trusted", CoreLook { created_now: None, ..live() }),
            ("a block with no recorded creation time was trusted", CoreLook { recorded_created: 0, ..live() }),
        ];
        for (failure, look) in cases {
            assert!(join_refusal(&look).is_some(), "{failure}");
        }
    }

    /// A child's park whose first call the thread refuses leaves no handle open, here or in the child: the
    /// two events, and the two handles to them put in the child, are all closed. The child is this process,
    /// so a handle left in it shows in its own count, and the thread a handle to this thread without the
    /// access to queue a call on it.
    #[test]
    fn a_childs_park_whose_first_call_is_refused_leaves_no_handle_open() {
        use windows::Win32::System::Threading::{GetProcessHandleCount, OpenThread, THREAD_QUERY_LIMITED_INFORMATION};
        const TRIES: u32 = 256;
        // SAFETY: the pseudo-handle of this process, and a handle to this thread that `put_on` cannot queue a
        // call on, so nothing is ever queued on it. Every handle that comes back is closed here.
        unsafe {
            let count = || {
                let mut n = 0u32;
                GetProcessHandleCount(GetCurrentProcess(), &mut n).expect("this process can count its handles");
                n
            };
            let thread = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, GetCurrentThreadId())
                .expect("a handle to this thread");
            let before = count();
            for _ in 0..TRIES {
                assert!(
                    ChildPark::put_on(GetCurrentProcess(), thread).is_none(),
                    "a call was queued on a thread handle without the access for it"
                );
            }
            // Other tests open and close handles meanwhile, so the bound is a quarter of what a leak would add.
            let grew = count().saturating_sub(before);
            let _ = CloseHandle(thread);
            assert!(grew < TRIES / 2, "{grew} handles stayed open after {TRIES} refused parks");
        }
    }

    /// A failed wait is not the core's death while the core's exit code says it runs (R4-N6). Only a
    /// signalled handle, or a failure the exit code cannot explain, lets the process go.
    #[test]
    fn only_a_signalled_core_or_one_that_cannot_be_asked_is_gone() {
        let no_question = || -> Option<u32> { panic!("a wait that answered needs no exit code") };
        assert_eq!(read_core_wait(WAIT_TIMEOUT_CODE, no_question), CoreWait::Running);
        assert_eq!(read_core_wait(WAIT_OBJECT_0_CODE, no_question), CoreWait::Gone);
        assert_eq!(read_core_wait(WAIT_FAILED.0, || Some(STILL_ACTIVE_CODE)), CoreWait::Failed);
        assert_eq!(read_core_wait(WAIT_FAILED.0, || Some(0)), CoreWait::Gone);
        assert_eq!(read_core_wait(WAIT_FAILED.0, || None), CoreWait::Gone);
    }

    /// A block is over when it carries the end mark or names another session. The mark answers alone,
    /// because after an ordered end the block still names this session until a new core takes it over.
    #[test]
    fn a_block_with_the_end_mark_or_another_sessions_name_is_over() {
        assert!(!block_says_over(false, || true), "a live block of this session read as over");
        assert!(block_says_over(true, || true), "an ended block still naming this session read as running");
        assert!(block_says_over(false, || false), "a block naming another session read as running");
    }

    /// The late scan looks fast through the startup burst, then backs off to a ceiling and stays
    /// there, so a module that never arrives costs one wake-up a second rather than four (R5, WY13).
    #[test]
    fn the_late_scan_backs_off_to_one_look_a_second() {
        assert_eq!(late_poll_interval(0), LATE_POLL_FAST_MS);
        assert_eq!(late_poll_interval(LATE_FAST_TRIES - 1), LATE_POLL_FAST_MS);
        let backed_off: Vec<u32> = (LATE_FAST_TRIES..LATE_FAST_TRIES + 5).map(late_poll_interval).collect();
        assert_eq!(backed_off, [250, 500, 1_000, 1_000, 1_000]);
        assert_eq!(late_poll_interval(u32::MAX), LATE_POLL_CEILING_MS, "a long session overflowed the back-off");
    }

    /// A watcher that failed to start gets more chances than the one it had, and not endless ones.
    #[test]
    fn a_watcher_that_failed_to_start_is_tried_again_a_bounded_number_of_times() {
        assert!(watcher_start_again(1), "one failure ended the attempts, as before R4-N6");
        assert!(watcher_start_again(WATCHER_START_TRIES - 1));
        assert!(!watcher_start_again(WATCHER_START_TRIES), "the attempts never end");
    }

    /// The watcher's pause is a wait on its own thread's handle, which nothing signals while the thread
    /// runs - so it lasts the full interval rather than returning at once and spinning.
    #[test]
    fn the_watcher_pause_lasts_its_interval() {
        let started = std::time::Instant::now();
        pause_watcher(40);
        let waited = started.elapsed();
        assert!(waited >= std::time::Duration::from_millis(30), "the pause returned after {waited:?}");
    }

    /// Only a refusal before anything changed unloads the library. A failure after the detours were
    /// created keeps it loaded, because unloading would leave them jumping into freed code.
    #[test]
    fn only_a_refusal_before_the_detours_unloads_the_library() {
        assert_eq!(attach_answer(&Ok(())), 1);
        assert_eq!(attach_answer(&Err(InstallError::Refused("ended".into()))), 0);
        assert_eq!(attach_answer(&Err(InstallError::Failed("enable_all_hooks".into()))), 1);
    }

    /// A failed enable takes back whatever went live, a clean one takes nothing back, and a take-back
    /// that fails too says so (R4-W7).
    #[test]
    fn a_failed_enable_takes_back_whatever_went_live() {
        let taken_back = Cell::new(0);
        let take_back = || {
            taken_back.set(taken_back.get() + 1);
            Ok::<(), i32>(())
        };
        assert!(enable_or_take_back(|| Ok::<(), i32>(()), take_back).is_ok());
        assert_eq!(taken_back.get(), 0, "a clean enable takes nothing back");
        assert!(enable_or_take_back(|| Err::<(), i32>(5), take_back).is_err());
        assert_eq!(taken_back.get(), 1, "a failed enable takes back what went live, once");
        let both = enable_or_take_back(|| Err::<(), i32>(5), || Err::<(), i32>(6)).unwrap_err();
        assert!(both.contains("failed too"), "a take-back that fails is named: {both}");
    }

    /// Late detours go on in one batch, one freeze for all of them. Only a batch that fails falls back to
    /// each detour's own enable, where a failure in the middle neither stops the ones after it nor gets
    /// its own channel counted, and comes back as failed so the audit can list it (R4-W7).
    #[test]
    fn late_detours_go_on_in_one_batch_and_one_by_one_only_when_it_fails() {
        let created = [(0b001, 10), (0b010, 20), (0b100, 30)];
        let tried = Cell::new(0);
        let one = |_bit, target| {
            tried.set(tried.get() + 1);
            target != 20
        };

        let batched = Cell::new(0);
        let whole = enable_late(&created, |b| { batched.set(b.len()); true }, one);
        assert_eq!(whole, (0b111, 0), "a batch that went through claims every detour in it");
        assert_eq!((batched.get(), tried.get()), (3, 0), "one batch of three, no detour on its own");

        let split = enable_late(&created, |_| false, one);
        assert_eq!(split, (0b101, 0b010), "only the two that went live are claimed, the third is failed");
        assert_eq!(tried.get(), 3, "a failure does not stop the ones after it");

        let called = Cell::new(false);
        assert_eq!(enable_late(&[], |_| { called.set(true); true }, one), (0, 0));
        assert!(!called.get(), "nothing created, nothing to freeze the threads for");
    }

    /// After a batch that stopped part-way, a detour it already switched on answers "already enabled" to
    /// its own enable, and that is live. Read as failed, its channel would be reported uncovered and taken
    /// out of the queue, and the next batch would switch it off under a mask that claims it.
    #[test]
    fn a_detour_the_batch_already_switched_on_counts_as_live() {
        assert!(went_live(&Ok(())));
        assert!(went_live(&Err(MH_STATUS::MH_ERROR_ENABLED)));
        assert!(!went_live(&Err(MH_STATUS::MH_ERROR_MEMORY_PROTECT)));
        assert!(!went_live(&Err(MH_STATUS::MH_ERROR_NOT_CREATED)));
    }

    /// A child is counted as uncovered when its load failed, and when it loaded but never signed in while
    /// the registry still had room - its install failed after the library loaded (R4-S2). Past the end
    /// of the registry a missing pid proves nothing, and `coverage.pid_registry_full` speaks for it.
    #[test]
    fn a_child_that_loaded_but_never_signed_in_ran_uncovered() {
        let room = MAX_COV_PIDS as u32;
        assert!(child_ran_uncovered(false, false, 3), "the load failed");
        assert!(!child_ran_uncovered(true, true, 3), "loaded and signed in");
        assert!(child_ran_uncovered(true, false, 3), "loaded, no pid, room left");
        assert!(child_ran_uncovered(true, false, room), "the last slot was still a slot");
        assert!(!child_ran_uncovered(true, false, room + 1), "the registry was full");
    }

    /// Only two known, different machines skip the injection (R4-N9). One that could not be asked leaves
    /// it to be tried, as before.
    #[test]
    fn only_a_known_other_machine_skips_the_injection() {
        const AMD64: u16 = 0x8664;
        const I386: u16 = 0x014c;
        assert!(bitness_differs(Some(I386), Some(AMD64)));
        assert!(bitness_differs(Some(AMD64), Some(I386)));
        assert!(!bitness_differs(Some(AMD64), Some(AMD64)));
        assert!(!bitness_differs(None, Some(AMD64)));
        assert!(!bitness_differs(Some(I386), None));
    }

    /// This process can be asked what it runs as, and the answer is the bitness it was built for.
    #[test]
    fn this_process_runs_as_the_machine_it_was_built_for() {
        let own = unsafe { process_machine(GetCurrentProcess()) };
        let built = if cfg!(target_pointer_width = "64") { 0x8664 } else { 0x014c };
        assert_eq!(own, Some(built));
    }

    /// MinHook stands ready when it initialized now or had been already, and every other answer is a
    /// refusal - the wrapper crate would have panicked on it inside `DllMain` (R4-N1). The failures are
    /// MinHook's own codes: unknown, not initialized, and the heap it could not create.
    #[test]
    fn minhook_is_ready_only_when_it_initialized_or_already_had() {
        assert!(minhook_ready(MH_OK));
        assert!(minhook_ready(MH_ERROR_ALREADY_INITIALIZED));
        for failure in [-1, 2, 9, 10] {
            assert!(!minhook_ready(failure), "status {failure} left MinHook unusable and must refuse");
        }
    }

    /// A two-body channel is covered only when both bodies are detoured. Either one failing leaves a
    /// set of callers on the real tick count, so the bit must end up clear - including when the
    /// second body is the one that went live.
    #[test]
    fn a_two_body_channel_is_covered_only_when_both_bodies_are_detoured() {
        let bit = CHANNELS[IDX_GTC64].bit;
        let other = CHANNELS[IDX_GSTAFT].bit;

        assert_eq!(settle_second_body(bit | other, bit, true), bit | other, "both bodies detoured");
        assert_eq!(settle_second_body(bit | other, bit, false), other, "the second body failed");
        assert_eq!(settle_second_body(other, bit, true), other, "the first body failed, the second went live");
        assert_eq!(settle_second_body(other, bit, false), other, "neither body detoured");
    }

    /// A channel is claimed only by a detour that every caller passes (R5, P4). Kernel32's entry is
    /// such a site when kernelbase has nothing else under the name, and not when the api-set callers
    /// reach a body of their own - a separate one, or the kernelbase export a failed detour fell back
    /// from. Those callers read the real clock, and the report used to say covered.
    #[test]
    fn a_channel_is_claimed_only_by_a_detour_every_caller_passes() {
        use ChannelModule::{Kernel32, KernelBaseAndKernel32, KernelBaseBehindKernel32};
        const ENTRY: usize = 0x1000;
        const OWN: usize = 0x2000;
        let claimed = |module, side, fell_back: bool| {
            let site = detour_site(module, ENTRY, side);
            claims_every_caller(site, if fell_back { ENTRY } else { site.at })
        };
        let same_stub = Some(KernelBaseSide::Same(OWN));
        let same_forwarder = Some(KernelBaseSide::Same(ENTRY));
        let separate = Some(KernelBaseSide::Separate(OWN));
        let absent = Some(KernelBaseSide::Absent);

        assert_eq!(detour_site(KernelBaseBehindKernel32, ENTRY, same_stub).at, OWN, "one body is detoured in kernelbase");
        assert!(claimed(KernelBaseBehindKernel32, same_stub, false), "the meeting point covers both paths");
        assert!(claimed(KernelBaseBehindKernel32, same_forwarder, false), "a forwarder is kernelbase's own export");
        assert!(!claimed(KernelBaseBehindKernel32, separate, false), "two bodies, only kernel32's detoured, claimed");
        assert!(!claimed(KernelBaseBehindKernel32, same_stub, true), "a detour that fell back to the stub claimed");
        assert!(!claimed(KernelBaseAndKernel32, same_stub, true), "a two-body kind that fell back to the stub claimed");
        assert!(claimed(KernelBaseBehindKernel32, absent, false), "no kernelbase export, kernel32 is every caller");
        assert!(
            claimed(KernelBaseAndKernel32, separate, false),
            "kernel32's half of a two-body channel - the second detour settles the bit"
        );
        assert!(claimed(Kernel32, None, false), "a plain kernel32 channel");
    }

    /// The codes the connection observer counts, and the neighbours it must not, all measured on this
    /// machine's socket driver (2026-09-23). The last one is a network code in the ordinary CTL_CODE
    /// layout that name resolution sends - the value a filter built on that layout would have matched.
    #[test]
    fn only_the_two_connection_codes_count_as_a_connection_attempt() {
        assert!(is_connection_attempt(0x12007), "connect and WSAConnect");
        assert!(is_connection_attempt(0x120C7), "ConnectEx and everything built on it");
        for other in [0x12003, 0x12023, 0x12024, 0x12047, 0x120BF, 0x120007] {
            assert!(!is_connection_attempt(other), "0x{other:x} is not a connection attempt");
        }
    }

    thread_local! {
        static CASCADE: Cell<bool> = const { Cell::new(false) };
    }

    /// The path a Sleep takes, measured on both bitnesses (R4/10b): Sleep -> SleepEx -> NtDelayExecution,
    /// whose original is the kernel wait. An APC Windows runs inside an alertable one is the
    /// application's code, so a Sleep in it is entered as a call of its own, and its own cascade still
    /// passes through. Until R4/10b the flag stood through the kernel wait and the APC's Sleep was taken
    /// for a cascade: unscaled and uncounted (R4-N7).
    #[test]
    fn a_wait_inside_an_alertable_kernel_wait_is_the_applications_own() {
        let sleep = enter_once(&CASCADE);
        assert!(sleep.is_some(), "the application's Sleep is not a cascade");
        assert!(enter_once(&CASCADE).is_none(), "SleepEx reached from Sleep is a cascade");
        assert!(enter_once(&CASCADE).is_none(), "NtDelayExecution reached from SleepEx is a cascade");
        let mut apc_ran = false;
        wait_runs_application(&CASCADE, true, || {
            let apc_sleep = enter_once(&CASCADE);
            assert!(apc_sleep.is_some(), "a Sleep inside an APC was taken for a cascade");
            assert!(enter_once(&CASCADE).is_none(), "the APC's own cascade counted twice");
            drop(apc_sleep);
            assert!(!CASCADE.get(), "the APC's Sleep left the flag up inside the kernel wait");
            apc_ran = true;
        });
        assert!(apc_ran);
        assert!(CASCADE.get(), "back in Windows' code after the kernel wait, the cascade flag is up again");
        drop(sleep);
        assert!(!CASCADE.get(), "the application's Sleep returned with the flag up");
    }

    /// A wait that is not alertable runs no application code, so its cascade keeps the flag: a hooked
    /// wait reached under it is still the same application call.
    #[test]
    fn a_wait_that_is_not_alertable_keeps_its_cascade() {
        let wait = enter_once(&CASCADE);
        wait_runs_application(&CASCADE, false, || {
            assert!(enter_once(&CASCADE).is_none(), "a non-alertable kernel wait lowered the cascade flag");
        });
        drop(wait);
        assert!(!CASCADE.get());
    }

    /// An exception out of an APC unwinds past every frame between the APC and its handler. Whether
    /// those frames put their flags back (they do under unwinding with cleanups) or not (the injected
    /// detours have none, measured: the APC's exception left the old flag up for good), the thread ends
    /// with the flag down, so its next Sleep is scaled and counted (R4-N7).
    #[test]
    fn an_exception_out_of_an_apc_leaves_the_thread_outside_any_wait() {
        // Cleanups skipped: no frame puts anything back, so the flag stays what the APC saw.
        let sleep = enter_once(&CASCADE).expect("top level");
        assert!(enter_once(&CASCADE).is_none());
        let seen_by_the_apc = wait_runs_application(&CASCADE, true, || CASCADE.get());
        assert!(!seen_by_the_apc, "an exception out of the APC would leave the cascade flag up for good");
        drop(sleep);

        // Cleanups run: a real unwind through the same helpers.
        let unwound = std::panic::catch_unwind(|| {
            let _sleep = enter_once(&CASCADE);
            wait_runs_application(&CASCADE, true, || panic!("an exception out of an APC"));
        });
        assert!(unwound.is_err());
        assert!(!CASCADE.get(), "an unwind with cleanups left the cascade flag up");
    }

    /// Replace this thread's thread-local vector in its TEB, returning the one it had - so a test can stand
    /// on a thread the loader has taken the storage from. Nothing between two calls may touch a thread-local.
    fn swap_tls_vector(new: *const *const u8) -> *const *const u8 {
        let old: *const *const u8;
        // SAFETY: this thread's own TEB field, read and written back by the same test.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("mov {old}, qword ptr gs:[0x58]", "mov qword ptr gs:[0x58], {new}", old = out(reg) old, new = in(reg) new, options(nostack, preserves_flags));
        }
        #[cfg(target_arch = "x86")]
        unsafe {
            core::arch::asm!("mov {old}, dword ptr fs:[0x2C]", "mov dword ptr fs:[0x2C], {new}", old = out(reg) old, new = in(reg) new, options(nostack, preserves_flags));
        }
        old
    }

    /// 2026-10-28: a thread the loader is ending, or one started without thread-local storage, has no
    /// vector in its TEB, or no slot for this module in it. A detour there faulted on its first flag, and
    /// under the heap lock the fault's handler hung the thread for good. Without the storage every flag
    /// helper does nothing and every cascade entry passes the call by, and the thread reads its flags again
    /// once the storage is back. Both shapes are taken from this thread for a moment, so a fault here is
    /// the bug itself, not a failed assertion.
    #[test]
    fn a_thread_without_thread_local_storage_passes_every_flag_by() {
        let gone = swap_tls_vector(core::ptr::null());
        let no_vector = (thread_has_tls(), read_flag(&CASCADE), enter_once(&CASCADE).is_some());
        drop(set_flag(&CASCADE, true));
        swap_tls_vector(gone);
        assert_eq!(no_vector, (false, None, false), "no vector: no storage, no flag, no entry");
        assert!(thread_has_tls(), "the vector is back");
        assert!(!CASCADE.get(), "a flag was written while the thread had nowhere to keep it");

        // A vector without this module's slot - every slot empty, this one's included.
        let index = unsafe {
            unsafe extern "C" {
                static _tls_index: u32;
            }
            _tls_index as usize
        };
        let empty = [core::ptr::null::<u8>(); 64];
        assert!(index < empty.len(), "this test binary's TLS index {index} fits the stand-in vector");
        let gone = swap_tls_vector(empty.as_ptr());
        let no_slot = (thread_has_tls(), read_flag(&CASCADE), enter_once(&CASCADE).is_some());
        drop(set_flag(&CASCADE, true));
        swap_tls_vector(gone);
        assert_eq!(no_slot, (false, None, false), "no slot: no storage, no flag, no entry");
        assert!(!CASCADE.get());

        let entered = enter_once(&CASCADE);
        assert!(entered.is_some() && CASCADE.get(), "with the storage back the thread enters as before");
    }
}
