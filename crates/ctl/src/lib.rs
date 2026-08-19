//! Control-memory contract shared by the mechanism (chrono-mech) and the injected
//! hook (chrono-hook). Both processes map the SAME `#[repr(C)]` layouts into named
//! shared sections, so they are defined in exactly one place.
//!
//! Two sections, separated by lifetime and writer (per-pid coverage):
//!
//! - `Ctl` in `Local\ChronoCtl` is the SESSION-WIDE control block. The ANCHOR fields
//!   (`a_fake`, `a_real`, `multiplier`) are written by the mechanism under a seqlock
//!   and read by every hook (parent and children share ONE fake clock - ADR-3). The
//!   stable config fields (`tz_bias`, `scale_dur`, `core_pid`) are written once before
//!   the target exists. The PID REGISTRY (`pid_count`, `pids`) lets each hooked process
//!   publish its own PID so the mechanism can find its per-process coverage section.
//!
//! - `Cov` in `Local\ChronoCov.<pid>` is PER-PROCESS coverage, written only by that
//!   process's hook and read by the mechanism. One writer, so plain volatile access is
//!   enough. (A `calls` increment is a volatile RMW, so concurrent target threads
//!   hitting the SAME channel may lose a bump - that only ever UNDER-counts live
//!   evidence, never fabricates coverage.) Splitting coverage out of `Ctl` is what
//!   stops a child's calls from being summed into the parent's report.
//!
//! Registering a PID, by contrast, uses a REAL atomic slot reservation, not a volatile
//! RMW: a lost registry write would drop a whole child from the audit, not just
//! under-count it.
//!
//! Fake wall time is `a_fake + (quit_now - a_real) * multiplier`, in 100 ns units,
//! anchored on `QueryUnbiasedInterruptTime` (ADR-5). UTC channels return that instant
//! directly; `GetLocalTime` returns it shifted back into the session zone by `tz_bias`.

use std::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use std::sync::atomic::{compiler_fence, AtomicU32, Ordering};

/// Named shared section for a session's control memory (per interactive session).
pub const CTL_SECTION_NAME: &str = "Local\\ChronoCtl";

/// Prefix of a process's coverage section; the full name is `Local\ChronoCov.<pid>`.
pub const COV_SECTION_PREFIX: &str = "Local\\ChronoCov.";

/// Maximum number of processes (parent + children) whose coverage a session tracks.
/// Installers can spawn dozens of helpers (docs/07 open item 2); 256 leaves headroom.
/// Beyond it, `register_pid` returns false and that process runs uncovered in the
/// audit - an honest partial, never a silent overwrite.
pub const MAX_COV_PIDS: usize = 256;

/// Full coverage-section name for a process id. Single source, used by both the hook
/// (its own pid) and the mechanism (each registered pid).
pub fn cov_section_name(pid: u32) -> String {
    format!("{COV_SECTION_PREFIX}{pid}")
}

// --- Wall-clock channels -------------------------------------------------------
//
// The coverage bit, the `calls`-array index (IDX_*), and the `CHANNELS` table below
// are three views of the same list and MUST stay in sync. A unit test guards it.

/// Coverage bit: `GetSystemTimeAsFileTime` is hooked.
pub const CH_GSTAFT: u64 = 1 << 0;
/// Coverage bit: `GetSystemTimePreciseAsFileTime` is hooked.
pub const CH_GSTPAFT: u64 = 1 << 1;
/// Coverage bit: `GetSystemTime` is hooked.
pub const CH_GST: u64 = 1 << 2;
/// Coverage bit: `GetLocalTime` is hooked.
pub const CH_GLT: u64 = 1 << 3;
/// Coverage bit: `NtQuerySystemTime` is hooked.
pub const CH_NTQST: u64 = 1 << 4;
/// Coverage bit: `GetTimeZoneInformation` is hooked (session zone).
pub const CH_GTZI: u64 = 1 << 5;
/// Coverage bit: `GetDynamicTimeZoneInformation` is hooked (session zone).
pub const CH_GDTZI: u64 = 1 << 6;
/// Coverage bit: `GetTickCount64` is hooked (duration axis, opt-in).
pub const CH_GTC64: u64 = 1 << 7;
/// Coverage bit: `QueryUnbiasedInterruptTime` is hooked (duration axis, opt-in).
pub const CH_QUIT: u64 = 1 << 8;
/// Coverage bit: `GetTickCount` (32-bit) is hooked (duration axis, opt-in).
pub const CH_GTC: u64 = 1 << 9;
/// Coverage bit: `SystemTimeToTzSpecificLocalTime` is hooked (session zone).
pub const CH_STSL: u64 = 1 << 10;
/// Coverage bit: `SystemTimeToTzSpecificLocalTimeEx` is hooked (session zone).
pub const CH_STSLEX: u64 = 1 << 11;
/// Coverage bit: `FileTimeToLocalFileTime` is hooked (session zone, UTC->local FILETIME).
pub const CH_FTLFT: u64 = 1 << 12;
/// Coverage bit: `LocalFileTimeToFileTime` is hooked (session zone, local->UTC FILETIME).
pub const CH_LFTFT: u64 = 1 << 13;
/// Coverage bit: `TzSpecificLocalTimeToSystemTime` is hooked (session zone, local->UTC).
pub const CH_TLTST: u64 = 1 << 14;
/// Coverage bit: `TzSpecificLocalTimeToSystemTimeEx` is hooked (session zone, local->UTC).
pub const CH_TLTSTEX: u64 = 1 << 15;
/// Coverage bit: `Sleep` is hooked (duration axis / wait-length scaling, opt-in, ADR-7).
pub const CH_SLEEP: u64 = 1 << 16;
/// Coverage bit: `SleepEx` is hooked (duration axis / wait-length scaling, opt-in, ADR-7).
pub const CH_SLEEPEX: u64 = 1 << 17;
/// Coverage bit: `NtDelayExecution` is hooked (duration axis / wait-length scaling, opt-in, ADR-7).
pub const CH_NTDELAY: u64 = 1 << 18;
/// Coverage bit: `NtQuerySystemInformation(SystemTimeOfDayInformation)` is hooked (wall clock, ntdll).
pub const CH_NTQSI: u64 = 1 << 19;
/// Coverage bit: `WaitForSingleObject` is hooked (object wait, observed not scaled, ADR-7 class B).
pub const CH_WFSO: u64 = 1 << 20;
/// Coverage bit: `WaitForSingleObjectEx` is hooked (object wait, observed not scaled, ADR-7 class B).
pub const CH_WFSOEX: u64 = 1 << 21;
/// Coverage bit: `WaitForMultipleObjects` is hooked (object wait, observed not scaled, ADR-7 class B).
pub const CH_WFMO: u64 = 1 << 22;
/// Coverage bit: `WaitForMultipleObjectsEx` is hooked (object wait, observed not scaled, ADR-7 class B).
pub const CH_WFMOEX: u64 = 1 << 23;
/// Coverage bit: `SignalObjectAndWait` is hooked (object wait, observed not scaled, ADR-7 class B).
pub const CH_SOAW: u64 = 1 << 24;
/// Coverage bit: `MsgWaitForMultipleObjects` is hooked (message wait, observed not scaled, ADR-7 class B).
pub const CH_MWFMO: u64 = 1 << 25;
/// Coverage bit: `MsgWaitForMultipleObjectsEx` is hooked (message wait, observed not scaled, ADR-7 class B).
pub const CH_MWFMOEX: u64 = 1 << 26;
/// Coverage bit: `SetWaitableTimer` is hooked (settable timer, due-time + period scaled, ADR-7 class C).
pub const CH_SWT: u64 = 1 << 27;
/// Coverage bit: `SetWaitableTimerEx` is hooked (settable timer, due-time + period scaled, ADR-7 class C).
pub const CH_SWTEX: u64 = 1 << 28;
/// Coverage bit: `SetTimer` is hooked (user32 message timer, uElapse scaled, ADR-7 class C).
pub const CH_SETTIMER: u64 = 1 << 29;
/// Coverage bit: `timeSetEvent` is hooked (winmm multimedia timer, observed not scaled, ADR-7 class C).
pub const CH_TIMESETEVENT: u64 = 1 << 30;
/// Coverage bit: `SetThreadpoolTimer` is hooked (thread-pool timer, due-time + period scaled, ADR-7 class C).
pub const CH_TPTIMER: u64 = 1 << 31;
/// Coverage bit: `SetThreadpoolTimerEx` is hooked (thread-pool timer, due-time + period scaled, ADR-7 class C).
pub const CH_TPTIMEREX: u64 = 1 << 32;
/// Coverage bit: `NtCreateUserProcess` is hooked (direct process creation, observed not injected, ADR-3).
pub const CH_NTCUP: u64 = 1 << 33;
/// Coverage bit: `connect` is hooked (ws2_32 network connection, observed - a suspected server time source).
pub const CH_CONNECT: u64 = 1 << 34;

/// Index of each channel into the `calls` array (== its position in `CHANNELS`).
pub const IDX_GSTAFT: usize = 0;
pub const IDX_GSTPAFT: usize = 1;
pub const IDX_GST: usize = 2;
pub const IDX_GLT: usize = 3;
pub const IDX_NTQST: usize = 4;
pub const IDX_GTZI: usize = 5;
pub const IDX_GDTZI: usize = 6;
pub const IDX_GTC64: usize = 7;
pub const IDX_QUIT: usize = 8;
pub const IDX_GTC: usize = 9;
pub const IDX_STSL: usize = 10;
pub const IDX_STSLEX: usize = 11;
pub const IDX_FTLFT: usize = 12;
pub const IDX_LFTFT: usize = 13;
pub const IDX_TLTST: usize = 14;
pub const IDX_TLTSTEX: usize = 15;
pub const IDX_SLEEP: usize = 16;
pub const IDX_SLEEPEX: usize = 17;
pub const IDX_NTDELAY: usize = 18;
pub const IDX_NTQSI: usize = 19;
pub const IDX_WFSO: usize = 20;
pub const IDX_WFSOEX: usize = 21;
pub const IDX_WFMO: usize = 22;
pub const IDX_WFMOEX: usize = 23;
pub const IDX_SOAW: usize = 24;
pub const IDX_MWFMO: usize = 25;
pub const IDX_MWFMOEX: usize = 26;
pub const IDX_SWT: usize = 27;
pub const IDX_SWTEX: usize = 28;
pub const IDX_SETTIMER: usize = 29;
pub const IDX_TIMESETEVENT: usize = 30;
pub const IDX_TPTIMER: usize = 31;
pub const IDX_TPTIMEREX: usize = 32;
pub const IDX_NTCUP: usize = 33;
pub const IDX_CONNECT: usize = 34;

/// Number of channels tracked (wall-clock, session zone, duration axis, object/message waits,
/// settable timers, multimedia timer, thread-pool timers, direct process creation, network connect).
pub const CHANNEL_COUNT: usize = 35;

/// Which system module exports a channel (the hook resolves it there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelModule {
    Kernel32,
    Ntdll,
    /// user32.dll - the message waits. May be absent in a console/service target that never
    /// loads it; the hook then leaves the channel uninstalled (honest partial), never forces it.
    User32,
    /// winmm.dll - the multimedia timer timeSetEvent. Often absent (a console/service target rarely
    /// loads winmm); resolved lazily like User32, honest partial if absent, never force-loaded.
    Winmm,
    /// ws2_32.dll - the sockets `connect`. Often absent (a target that never touches the network never
    /// loads it); resolved lazily like Winmm, honest partial if absent, never force-loaded.
    Ws2_32,
}

/// What kind of time a channel carries. The duration axis and the object-wait observation
/// are opt-in (scale_duration), so the mechanism only expects `Duration` and `WaitObserved`
/// channels when the session asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelCategory {
    Wall,
    Zone,
    Duration,
    /// Hooked and counted, but deliberately never modified (ADR-7 class B, option b): the
    /// object waits. Shortening their timeout could fake a timeout on a real I/O / hardware /
    /// IPC handle, so the wait is left real, reported in its own `observed` bucket, and the
    /// audit warns. Not `Duration` (that gets scaled), not a gap (we do hook and count it).
    WaitObserved,
    /// Hooked and counted, but deliberately never scaled (ADR-7 class C, observed): the multimedia
    /// timer timeSetEvent (winmm). It shares the `observed` bucket with `WaitObserved`, but its own
    /// warning key - scaling it would shift audio/MIDI timing, the winmm cost ADR-2 avoids (like
    /// timeGetTime), so it is left real. A separate category only so the audit can name the right
    /// reason (multimedia timer, not an object wait).
    TimerObserved,
    /// Hooked and counted, but deliberately never injected into (ADR-3, observed): a DIRECT
    /// NtCreateUserProcess (a child spawned bypassing CreateProcessW/A). Self-injecting there means
    /// manipulating undocumented native structures, a crash risk for near-zero real value (real QA
    /// targets spawn through CreateProcess*). So we count the direct call and warn that the child may
    /// be uncovered, an honest audit (rule 4) without the risk. NOT opt-in (unlike the time observers):
    /// process creation is watched regardless of scale_duration.
    SpawnObserved,
    /// Hooked and counted, but never modified: a network `connect` (ws2_32). A target that opens a
    /// network connection may read the time from a SERVER, which no local hook can cover - so we observe
    /// it and warn (source.network_at_start), an honest audit (rule 4) of a time source we cannot
    /// substitute. Like SpawnObserved, NOT opt-in: the network is watched regardless of scale_duration,
    /// and it never sways the local-coverage verdict.
    SourceObserved,
}

/// One time channel: its coverage bit, the exported symbol the hook detours, the
/// module that exports it, and its category. Single source of truth so the mechanism
/// reports exactly the channels the hook installs.
#[derive(Debug, Clone, Copy)]
pub struct ChannelDef {
    pub bit: u64,
    pub name: &'static str,
    pub module: ChannelModule,
    pub category: ChannelCategory,
}

// --- Coverage audit against chrono-mock.md 9.1 ---------------------------------
// COVERED (the CHANNELS table below): the Win32 wall clock (GetSystemTime,
// GetSystemTimeAsFileTime, GetSystemTimePreciseAsFileTime, GetLocalTime), the session
// zone (GetTimeZoneInformation, GetDynamicTimeZoneInformation) plus the explicit zone
// conversions (SystemTimeToTzSpecificLocalTime/Ex, FileTimeToLocalFileTime,
// LocalFileTimeToFileTime, TzSpecificLocalTimeToSystemTime/Ex), NtQuerySystemTime,
// NtQuerySystemInformation(SystemTimeOfDayInformation) (wrap-and-patch: its CurrentTime field
// overwritten over the real call - the SDK struct is opaque BYTE[48], so the offset is an
// assessment verified empirically, see the hook), and the opt-in duration axis (GetTickCount,
// GetTickCount64, QueryUnbiasedInterruptTime), wait-length scaling (Sleep, SleepEx,
// NtDelayExecution - ADR-7 class A), and the settable waitable timers (SetWaitableTimer,
// SetWaitableTimerEx - ADR-7 class C): a relative due-time and a periodic lPeriod are scaled by M
// like a wait, and an ABSOLUTE (positive) due-time - a fake wall-clock instant the app computed
// from the substituted clock - is converted to a scaled relative interval (scale_timer_due), so the
// kernel (which reads the real clock for absolute timers) fires it when the FAKE clock reaches it.
// SetTimer (user32, ADR-7 class C) joins them: its uElapse interval is scaled by M (scale_timer_elapse)
// so WM_TIMER arrives in step with the fake clock - Windows clamps a scaled interval below
// USER_TIMER_MINIMUM (10 ms) up to it, a documented floor under heavy acceleration. The thread-pool
// timers SetThreadpoolTimer / SetThreadpoolTimerEx (kernel32) scale the same way as SetWaitableTimer:
// their FILETIME due-time (absolute converted to a scaled relative interval, relative scaled) and
// their msPeriod / msWindowLength divide by M. CRT time / _time64 ride the hooked Win32 exports, so
// they follow for free.
//
// DELIBERATELY EXCLUDED, for two different reasons:
//   - ADR-2 (scaling them destabilizes the target): the performance counter
//     (QueryPerformanceCounter, NtQueryPerformanceCounter) and timeGetTime.
//   - not a "now" clock at all: GetFileTime returns a file's stored creation / last-access /
//     last-write timestamps (MS Learn, fileapi.h), not the current time. Shifting them would
//     falsify filesystem metadata, never advance a clock - the target must read real file times
//     (a file written in 2026 was written in 2026, even under a fake 2038 wall clock).
//
// UNHOOKABLE BY NATURE (chrono-mock.md 9.2 - the verifier warns, it does not hide):
// direct KUSER_SHARED_DATA reads, direct syscalls, and out-of-process or network time.
//
// OBSERVED, counted and warned but deliberately NOT scaled (ADR-7 class B, option b): the object
// waits WaitForSingleObject, WaitForSingleObjectEx, WaitForMultipleObjects(Ex), SignalObjectAndWait,
// and the message waits MsgWaitForMultipleObjects(Ex) (user32, hooked only when the target has
// user32 loaded). Shortening their timeout would fake a timeout on a real I/O / hardware / IPC
// handle, so we leave the wait real, count it in its own `observed` bucket (never the verdict), and
// the audit raises a warning. A thread-local guard counts each app-level wait once, attributed to
// the export the app called, so an internal cascade (e.g. WaitForSingleObject -> ...Ex) is not
// double-counted. This is the wait-axis analog of ADR-2's QPC exclusion, except we still hook it to
// count and warn honestly. The multimedia timer timeSetEvent (winmm, ADR-7 class C) joins the same
// observed bucket under its own warning (timer.multimedia_not_scaled): scaling its uDelay would shift
// audio/MIDI timing - the winmm cost ADR-2 avoids, like timeGetTime - so it is hooked, counted, and
// left real, never scaled. A DIRECT NtCreateUserProcess (ntdll, ADR-3) - a child spawned bypassing
// CreateProcessW/A - joins the observed bucket under inheritance.ntcreateuserprocess_child_maybe_uncovered:
// self-injecting there means manipulating undocumented native structures, a crash risk for near-zero
// value (real targets spawn through CreateProcess*, which we do inject), so we count the direct call
// and warn that its child may be uncovered, an honest audit (rule 4) without the risk. A guard makes
// the CreateProcess* funnel to NtCreateUserProcess NOT count (the child is already inherited).
//
// KNOWN GAPS, not yet covered (the verifier should report these honestly): none of the major time or
// spawn surfaces remain; residual exotica (SetThreadpoolWait timeouts, RtlCreateUserProcess legacy
// path) are out of scope and would be reported honestly if a target hit them.

/// All time channels, ordered by their `calls` index (IDX_*): the wall-clock set, the
/// session-zone functions, then the opt-in duration axis.
pub const CHANNELS: [ChannelDef; CHANNEL_COUNT] = [
    ChannelDef { bit: CH_GSTAFT, name: "GetSystemTimeAsFileTime", module: ChannelModule::Kernel32, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_GSTPAFT, name: "GetSystemTimePreciseAsFileTime", module: ChannelModule::Kernel32, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_GST, name: "GetSystemTime", module: ChannelModule::Kernel32, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_GLT, name: "GetLocalTime", module: ChannelModule::Kernel32, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_NTQST, name: "NtQuerySystemTime", module: ChannelModule::Ntdll, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_GTZI, name: "GetTimeZoneInformation", module: ChannelModule::Kernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_GDTZI, name: "GetDynamicTimeZoneInformation", module: ChannelModule::Kernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_GTC64, name: "GetTickCount64", module: ChannelModule::Kernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_QUIT, name: "QueryUnbiasedInterruptTime", module: ChannelModule::Kernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_GTC, name: "GetTickCount", module: ChannelModule::Kernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_STSL, name: "SystemTimeToTzSpecificLocalTime", module: ChannelModule::Kernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_STSLEX, name: "SystemTimeToTzSpecificLocalTimeEx", module: ChannelModule::Kernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_FTLFT, name: "FileTimeToLocalFileTime", module: ChannelModule::Kernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_LFTFT, name: "LocalFileTimeToFileTime", module: ChannelModule::Kernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_TLTST, name: "TzSpecificLocalTimeToSystemTime", module: ChannelModule::Kernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_TLTSTEX, name: "TzSpecificLocalTimeToSystemTimeEx", module: ChannelModule::Kernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_SLEEP, name: "Sleep", module: ChannelModule::Kernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_SLEEPEX, name: "SleepEx", module: ChannelModule::Kernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_NTDELAY, name: "NtDelayExecution", module: ChannelModule::Ntdll, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_NTQSI, name: "NtQuerySystemInformation", module: ChannelModule::Ntdll, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_WFSO, name: "WaitForSingleObject", module: ChannelModule::Kernel32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_WFSOEX, name: "WaitForSingleObjectEx", module: ChannelModule::Kernel32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_WFMO, name: "WaitForMultipleObjects", module: ChannelModule::Kernel32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_WFMOEX, name: "WaitForMultipleObjectsEx", module: ChannelModule::Kernel32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_SOAW, name: "SignalObjectAndWait", module: ChannelModule::Kernel32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_MWFMO, name: "MsgWaitForMultipleObjects", module: ChannelModule::User32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_MWFMOEX, name: "MsgWaitForMultipleObjectsEx", module: ChannelModule::User32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_SWT, name: "SetWaitableTimer", module: ChannelModule::Kernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_SWTEX, name: "SetWaitableTimerEx", module: ChannelModule::Kernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_SETTIMER, name: "SetTimer", module: ChannelModule::User32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_TIMESETEVENT, name: "timeSetEvent", module: ChannelModule::Winmm, category: ChannelCategory::TimerObserved },
    ChannelDef { bit: CH_TPTIMER, name: "SetThreadpoolTimer", module: ChannelModule::Kernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_TPTIMEREX, name: "SetThreadpoolTimerEx", module: ChannelModule::Kernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_NTCUP, name: "NtCreateUserProcess", module: ChannelModule::Ntdll, category: ChannelCategory::SpawnObserved },
    ChannelDef { bit: CH_CONNECT, name: "connect", module: ChannelModule::Ws2_32, category: ChannelCategory::SourceObserved },
];

/// Session-wide control block in `Local\ChronoCtl`. `#[repr(C)]` so both processes
/// agree on the layout. Coverage is NOT here - it lives per-process in `Cov`.
#[repr(C)]
pub struct Ctl {
    /// Seqlock counter for the anchor fields (odd = write in progress).
    pub seq: u32,
    /// Session zone bias in minutes (UTC = local + bias), no DST. Stable per session.
    pub tz_bias: i32,
    /// Fake anchor in 100 ns FILETIME units.
    pub a_fake: i64,
    /// Real anchor in QueryUnbiasedInterruptTime (QUIT) 100 ns units.
    pub a_real: i64,
    /// Time multiplier. Stage 3 uses 1 (offset only).
    pub multiplier: i64,
    /// 1 = also scale the duration axis by the multiplier (the scale_duration opt-in).
    /// Stable per session: written once by the mechanism, read once by the hook.
    pub scale_dur: u32,
    /// PID of the core process, so the hook can watch it and revert the target to
    /// real time when the core vanishes (clean end, crash, or kill -9). Stable.
    pub core_pid: u32,
    /// PID registry slot counter, reserved atomically by `register_pid`. Only ever
    /// increases; the mechanism does not read it - it scans `pids` for nonzero entries.
    pub pid_count: u32,
    pub _pad: u32,
    /// Registered PIDs (parent + children). A hook writes its own PID here after its
    /// coverage section exists; the mechanism opens `Local\ChronoCov.<pid>` for each.
    pub pids: [u32; MAX_COV_PIDS],
}

/// Per-process coverage block in `Local\ChronoCov.<pid>`. `#[repr(C)]`; written only by
/// the owning process's hook, read by the mechanism. Separated from `Ctl` so each
/// process's evidence is attributed to it and never summed with the rest of the tree.
#[repr(C)]
pub struct Cov {
    /// Bitmask of channels this process's hook installed. u64 (63 usable bits) so the channel set can
    /// grow past 32 without a layout break; the low bits still hold channels 0..N as before. Being u64
    /// also aligns `calls` to 8 bytes with no explicit padding (the old u32 + u32 pad was the same 8
    /// bytes, so the section size is unchanged).
    pub installed_channels: u64,
    /// Per-channel call counters for this process, indexed by IDX_*.
    pub calls: [u64; CHANNEL_COUNT],
}

/// Size of the session control block, for CreateFileMapping.
pub const fn ctl_size() -> usize {
    core::mem::size_of::<Ctl>()
}

/// Size of a per-process coverage block, for CreateFileMapping.
pub const fn cov_size() -> usize {
    core::mem::size_of::<Cov>()
}

/// Write the anchor triple under the seqlock. Caller guarantees `p` is a valid,
/// aligned pointer into the shared section.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_anchor(p: *mut Ctl, a_fake: i64, a_real: i64, multiplier: i64) {
    let sp = addr_of_mut!((*p).seq);
    let s = read_volatile(sp).wrapping_add(1);
    write_volatile(sp, s); // odd - write in progress
    compiler_fence(Ordering::SeqCst);
    write_volatile(addr_of_mut!((*p).a_fake), a_fake);
    write_volatile(addr_of_mut!((*p).a_real), a_real);
    write_volatile(addr_of_mut!((*p).multiplier), multiplier);
    compiler_fence(Ordering::SeqCst);
    write_volatile(sp, s.wrapping_add(1)); // even - write done
}

/// Read the anchor triple under the seqlock, retrying on a concurrent write.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_anchor(p: *const Ctl) -> (i64, i64, i64) {
    loop {
        let s1 = read_volatile(addr_of!((*p).seq));
        if s1 & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        compiler_fence(Ordering::SeqCst);
        let a_fake = read_volatile(addr_of!((*p).a_fake));
        let a_real = read_volatile(addr_of!((*p).a_real));
        let multiplier = read_volatile(addr_of!((*p).multiplier));
        compiler_fence(Ordering::SeqCst);
        if s1 == read_volatile(addr_of!((*p).seq)) {
            return (a_fake, a_real, multiplier);
        }
    }
}

/// Write the session zone bias (stable field, outside the seqlock). Mechanism side.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_tz_bias(p: *mut Ctl, bias: i32) {
    write_volatile(addr_of_mut!((*p).tz_bias), bias);
}

/// Read the session zone bias (hook side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_tz_bias(p: *const Ctl) -> i32 {
    read_volatile(addr_of!((*p).tz_bias))
}

/// Write the scale-duration flag (stable field, outside the seqlock). Mechanism side.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_scale_dur(p: *mut Ctl, on: bool) {
    write_volatile(addr_of_mut!((*p).scale_dur), on as u32);
}

/// Read the scale-duration flag (hook side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_scale_dur(p: *const Ctl) -> bool {
    read_volatile(addr_of!((*p).scale_dur)) != 0
}

/// Write the core PID (stable field, outside the seqlock). Mechanism side.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_core_pid(p: *mut Ctl, pid: u32) {
    write_volatile(addr_of_mut!((*p).core_pid), pid);
}

/// Read the core PID (hook side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_core_pid(p: *const Ctl) -> u32 {
    read_volatile(addr_of!((*p).core_pid))
}

/// Register this process's PID in the session registry (hook side), so the mechanism
/// can find its `Local\ChronoCov.<pid>` section. Reserves a slot atomically - several
/// children may register concurrently. Returns false if the registry is full (the
/// process then runs uncovered in the audit, an honest partial). Call AFTER the
/// process's coverage section exists, so a reader never sees a pid without its section.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn register_pid(p: *mut Ctl, pid: u32) -> bool {
    // Atomic reservation: a volatile RMW could hand two children the same slot and
    // lose one from the audit entirely (worse than the calls under-count).
    let counter = &*(addr_of!((*p).pid_count) as *const AtomicU32);
    let slot = counter.fetch_add(1, Ordering::SeqCst) as usize;
    if slot >= MAX_COV_PIDS {
        return false;
    }
    let slotp = (addr_of_mut!((*p).pids) as *mut u32).add(slot);
    write_volatile(slotp, pid);
    true
}

/// Read one PID registry slot (mechanism side). `i` must be < MAX_COV_PIDS. A zero
/// means "empty or not yet published" - the mechanism scans all slots and skips zeros,
/// so a slot reserved but not yet written is simply picked up on the next refresh.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `i < MAX_COV_PIDS`.
pub unsafe fn read_pid(p: *const Ctl, i: usize) -> u32 {
    let slotp = (addr_of!((*p).pids) as *const u32).add(i);
    read_volatile(slotp)
}

/// Mark a channel as installed (hook side, per-process `Cov`). OR-in, so several
/// channels accumulate.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn mark_channel_installed(p: *mut Cov, channel: u64) {
    let cur = read_volatile(addr_of!((*p).installed_channels));
    write_volatile(addr_of_mut!((*p).installed_channels), cur | channel);
}

/// Read the installed-channels bitmask (mechanism side, per-process `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_installed(p: *const Cov) -> u64 {
    read_volatile(addr_of!((*p).installed_channels))
}

/// Increment a channel's call counter (hook side, per-process `Cov`). `idx` must be
/// < CHANNEL_COUNT.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`, and `idx < CHANNEL_COUNT`.
pub unsafe fn bump_calls(p: *mut Cov, idx: usize) {
    let slot = (addr_of_mut!((*p).calls) as *mut u64).add(idx);
    let cur = read_volatile(slot);
    write_volatile(slot, cur.wrapping_add(1));
}

/// Read a channel's call counter (mechanism side, per-process `Cov`). `idx` must be
/// < CHANNEL_COUNT.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`, and `idx < CHANNEL_COUNT`.
pub unsafe fn read_calls(p: *const Cov, idx: usize) -> u64 {
    let slot = (addr_of!((*p).calls) as *const u64).add(idx);
    read_volatile(slot)
}

/// Scale a wait timeout in milliseconds by the duration multiplier: real wait =
/// requested / M (ADR-7). `INFINITE` (0xFFFFFFFF) and 0 pass through untouched - never
/// turn "wait forever" into a finite wait, never lengthen a poll. `m` is clamped to >= 1,
/// so frozen (M=0) and slow motion (0<M<1) leave waits at real length, symmetric to the
/// duration axis (untouchable rule 3). Integer division truncates: a sub-M timeout
/// collapses to a yield, the honest coarse behavior under heavy acceleration.
pub fn scale_wait(ms: u32, m: i64) -> u32 {
    const INFINITE_MS: u32 = 0xFFFF_FFFF;
    if ms == 0 || ms == INFINITE_MS {
        return ms;
    }
    let m = m.max(1) as u64;
    (ms as u64 / m) as u32
}

/// Scale an `NtDelayExecution` interval (100 ns units) by the duration multiplier. Only a
/// NEGATIVE interval is a relative delay - scale its magnitude (interval / M, toward zero).
/// A positive interval is an absolute deadline and a zero is a yield - both pass through
/// untouched. `m` is clamped to >= 1 (rule 3, symmetric to scale_wait).
pub fn scale_delay_interval(interval: i64, m: i64) -> i64 {
    if interval < 0 {
        interval / m.max(1)
    } else {
        interval
    }
}

/// Scale a waitable-timer due time (100 ns, FILETIME convention) into a RELATIVE real interval,
/// always negative so the hook forwards one uniform shape to the kernel (ADR-7 class C).
///
/// A NEGATIVE due is already relative: scale its magnitude toward zero, like `scale_delay_interval`.
/// A POSITIVE due is an ABSOLUTE fake wall-clock instant (the app computed it from the substituted
/// clock): convert it to the real delay until the fake clock reaches it - `(due - fake_now) / M` -
/// returned negative. Unlike `NtDelayExecution`, a positive due here is NOT passed through: the
/// kernel reads the REAL clock for an absolute timer, so an unconverted fake instant would fire
/// years off. An absolute due already at or before `fake_now` fires immediately (`-1`).
///
/// `m` is clamped to >= 1, so frozen (M=0) and slow motion (0<M<1) leave the timer at real length,
/// symmetric to the duration axis (untouchable rule 3). One consequence: an ABSOLUTE timer under
/// frozen fires as if M=1 - a frozen wall clock never reaches a future absolute due on its own, so
/// there is no "correct" wait to shorten. Documented limit, not a silent choice.
pub fn scale_timer_due(due: i64, fake_now: i64, m: i64) -> i64 {
    let m = m.max(1);
    if due < 0 {
        // Already relative: magnitude scaled toward zero, stays negative.
        due / m
    } else {
        // Absolute fake instant -> relative real interval until the fake clock reaches it.
        let delta_fake = due - fake_now;
        if delta_fake <= 0 {
            -1 // fake clock already at/past the due time: fire now
        } else {
            -(delta_fake / m)
        }
    }
}

/// Scale a periodic waitable-timer period (milliseconds) by the duration multiplier: real period =
/// period / M. A POSITIVE period never collapses to 0 (that would silently turn a periodic timer
/// into a one-shot), so it is clamped to >= 1. A 0 period (one-shot) stays 0, and a negative period
/// (the API rejects it) passes through unchanged. `m` clamped to >= 1 (rule 3, like `scale_wait`).
pub fn scale_timer_period(period_ms: i32, m: i64) -> i32 {
    if period_ms <= 0 {
        return period_ms; // 0 = one-shot, negative = API error: leave both untouched
    }
    (period_ms as i64 / m.max(1)).max(1) as i32
}

/// Scale a `SetTimer`/WM_TIMER interval (milliseconds) by the duration multiplier: real interval =
/// uElapse / M. `SetTimer` has no INFINITE and no absolute form - every value is a relative interval -
/// so, unlike `scale_wait`, there is no 0xFFFFFFFF guard. A 0 (or a sub-M value that truncates to 0)
/// is left as is: Windows clamps any interval below USER_TIMER_MINIMUM (10 ms) up to it, so under a
/// large M the timer cannot beat that 10 ms floor (documented limit, the WM_TIMER analog of
/// `scale_wait`'s truncation). `m` clamped to >= 1 (rule 3, frozen/slow leave it real).
pub fn scale_timer_elapse(elapse_ms: u32, m: i64) -> u32 {
    (elapse_ms as u64 / m.max(1) as u64) as u32
}

/// Scale a thread-pool timer period (milliseconds, DWORD) by the duration multiplier: real period =
/// period / M, clamped to >= 1 so a positive period never collapses to 0 (0 = one-shot, which would
/// silently turn a periodic timer into a one-shot). A 0 period (one-shot) stays 0. Unlike
/// `scale_timer_period` this takes a u32, because SetThreadpoolTimer's msPeriod is a DWORD, not the
/// signed LONG lPeriod of SetWaitableTimer. `m` clamped to >= 1 (rule 3, frozen/slow leave it real).
pub fn scale_timer_period_ms(period_ms: u32, m: i64) -> u32 {
    if period_ms == 0 {
        return 0; // one-shot
    }
    (period_ms as u64 / m.max(1) as u64).max(1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zeroed_ctl() -> Ctl {
        Ctl {
            seq: 0,
            tz_bias: 0,
            a_fake: 0,
            a_real: 0,
            multiplier: 0,
            scale_dur: 0,
            core_pid: 0,
            pid_count: 0,
            _pad: 0,
            pids: [0; MAX_COV_PIDS],
        }
    }

    fn zeroed_cov() -> Cov {
        Cov { installed_channels: 0, calls: [0; CHANNEL_COUNT] }
    }

    #[test]
    fn anchor_round_trips() {
        let mut ctl = zeroed_ctl();
        let p = &mut ctl as *mut Ctl;
        unsafe {
            write_anchor(p, 134_000_000_000_000_000, 42, 1);
            let (af, ar, m) = read_anchor(p);
            assert_eq!(af, 134_000_000_000_000_000);
            assert_eq!(ar, 42);
            assert_eq!(m, 1);
            // seq is even after a completed write
            assert_eq!(ctl.seq & 1, 0);
        }
    }

    #[test]
    fn tz_bias_round_trips() {
        let mut ctl = zeroed_ctl();
        let p = &mut ctl as *mut Ctl;
        unsafe {
            write_tz_bias(p, -120);
            assert_eq!(read_tz_bias(p), -120);
        }
    }

    #[test]
    fn scale_dur_round_trips() {
        let mut ctl = zeroed_ctl();
        let p = &mut ctl as *mut Ctl;
        unsafe {
            assert!(!read_scale_dur(p));
            write_scale_dur(p, true);
            assert!(read_scale_dur(p));
        }
    }

    #[test]
    fn pid_registry_assigns_distinct_slots_and_reads_back() {
        let mut ctl = zeroed_ctl();
        let p = &mut ctl as *mut Ctl;
        unsafe {
            assert!(register_pid(p, 1111));
            assert!(register_pid(p, 2222));
            assert!(register_pid(p, 3333));
            // Three distinct slots, in order, readable back; the rest stay zero.
            assert_eq!(read_pid(p, 0), 1111);
            assert_eq!(read_pid(p, 1), 2222);
            assert_eq!(read_pid(p, 2), 3333);
            assert_eq!(read_pid(p, 3), 0);
        }
    }

    #[test]
    fn pid_registry_reports_full_instead_of_overwriting() {
        let mut ctl = zeroed_ctl();
        let p = &mut ctl as *mut Ctl;
        unsafe {
            for i in 0..MAX_COV_PIDS {
                assert!(register_pid(p, (i as u32) + 1), "slot {i} should fit");
            }
            // One past the end: honest false, no overwrite of a live slot.
            assert!(!register_pid(p, 999_999));
            assert_eq!(read_pid(p, 0), 1);
            assert_eq!(read_pid(p, MAX_COV_PIDS - 1), MAX_COV_PIDS as u32);
        }
    }

    #[test]
    fn channels_accumulate_and_count_per_index() {
        let mut cov = zeroed_cov();
        let p = &mut cov as *mut Cov;
        unsafe {
            assert_eq!(read_installed(p), 0);
            mark_channel_installed(p, CH_GSTAFT);
            mark_channel_installed(p, CH_NTQST);
            assert_eq!(read_installed(p) & CH_GSTAFT, CH_GSTAFT);
            assert_eq!(read_installed(p) & CH_NTQST, CH_NTQST);
            assert_eq!(read_installed(p) & CH_GLT, 0);

            bump_calls(p, IDX_GSTAFT);
            bump_calls(p, IDX_GSTAFT);
            bump_calls(p, IDX_NTQST);
            assert_eq!(read_calls(p, IDX_GSTAFT), 2);
            assert_eq!(read_calls(p, IDX_NTQST), 1);
            assert_eq!(read_calls(p, IDX_GST), 0);
        }
    }

    #[test]
    fn channel_table_matches_bits_and_indices() {
        // The three views of the channel list must never drift.
        let expected = [
            (IDX_GSTAFT, CH_GSTAFT),
            (IDX_GSTPAFT, CH_GSTPAFT),
            (IDX_GST, CH_GST),
            (IDX_GLT, CH_GLT),
            (IDX_NTQST, CH_NTQST),
            (IDX_GTZI, CH_GTZI),
            (IDX_GDTZI, CH_GDTZI),
            (IDX_GTC64, CH_GTC64),
            (IDX_QUIT, CH_QUIT),
            (IDX_GTC, CH_GTC),
            (IDX_STSL, CH_STSL),
            (IDX_STSLEX, CH_STSLEX),
            (IDX_FTLFT, CH_FTLFT),
            (IDX_LFTFT, CH_LFTFT),
            (IDX_TLTST, CH_TLTST),
            (IDX_TLTSTEX, CH_TLTSTEX),
            (IDX_SLEEP, CH_SLEEP),
            (IDX_SLEEPEX, CH_SLEEPEX),
            (IDX_NTDELAY, CH_NTDELAY),
            (IDX_NTQSI, CH_NTQSI),
            (IDX_WFSO, CH_WFSO),
            (IDX_WFSOEX, CH_WFSOEX),
            (IDX_WFMO, CH_WFMO),
            (IDX_WFMOEX, CH_WFMOEX),
            (IDX_SOAW, CH_SOAW),
            (IDX_MWFMO, CH_MWFMO),
            (IDX_MWFMOEX, CH_MWFMOEX),
            (IDX_SWT, CH_SWT),
            (IDX_SWTEX, CH_SWTEX),
            (IDX_SETTIMER, CH_SETTIMER),
            (IDX_TIMESETEVENT, CH_TIMESETEVENT),
            (IDX_TPTIMER, CH_TPTIMER),
            (IDX_TPTIMEREX, CH_TPTIMEREX),
            (IDX_NTCUP, CH_NTCUP),
        ];
        assert_eq!(CHANNELS.len(), CHANNEL_COUNT);
        for (idx, bit) in expected {
            assert_eq!(CHANNELS[idx].bit, bit, "bit mismatch at index {idx}");
        }
        // Bits are distinct and non-zero.
        let mut seen = 0u64;
        for ch in CHANNELS {
            assert_ne!(ch.bit, 0);
            assert_eq!(seen & ch.bit, 0, "duplicate bit for {}", ch.name);
            seen |= ch.bit;
        }
    }

    #[test]
    fn cov_section_name_is_pid_suffixed() {
        assert_eq!(cov_section_name(1234), "Local\\ChronoCov.1234");
    }

    #[test]
    fn scale_wait_divides_and_guards_edges() {
        // INFINITE and 0 are untouched - never a finite wait, never a lengthened poll.
        assert_eq!(scale_wait(0xFFFF_FFFF, 60), 0xFFFF_FFFF);
        assert_eq!(scale_wait(0, 60), 0);
        // Real wait = requested / M.
        assert_eq!(scale_wait(6000, 60), 100);
        assert_eq!(scale_wait(6000, 1), 6000);
        // Frozen (0) and slow motion clamp to real length (>= 1) - rule 3.
        assert_eq!(scale_wait(6000, 0), 6000);
        assert_eq!(scale_wait(6000, -5), 6000);
        // Truncation: a sub-M timeout collapses to a yield (honest coarseness).
        assert_eq!(scale_wait(30, 60), 0);
    }

    #[test]
    fn scale_delay_interval_scales_relative_only() {
        // Negative = relative delay: magnitude scaled by M (toward zero). -0.6s at x60 -> -0.01s.
        assert_eq!(scale_delay_interval(-6_000_000, 60), -100_000);
        assert_eq!(scale_delay_interval(-6_000_000, 1), -6_000_000);
        // Positive = absolute deadline, and zero = yield: both untouched.
        assert_eq!(scale_delay_interval(6_000_000, 60), 6_000_000);
        assert_eq!(scale_delay_interval(0, 60), 0);
        // Frozen and slow motion clamp to real length (>= 1) - rule 3.
        assert_eq!(scale_delay_interval(-6_000_000, 0), -6_000_000);
        assert_eq!(scale_delay_interval(-6_000_000, -5), -6_000_000);
    }

    #[test]
    fn scale_timer_due_handles_relative_and_absolute() {
        // Relative (negative) due: magnitude scaled toward zero, stays relative. -0.6s at x60 -> -0.01s.
        assert_eq!(scale_timer_due(-6_000_000, 0, 60), -100_000);
        assert_eq!(scale_timer_due(-6_000_000, 0, 1), -6_000_000);
        // Absolute (positive) fake instant -> relative real interval until the fake clock reaches it.
        // due = fake_now + 6s (60_000_000 ticks) ahead; at x60 the real wait is 0.1s (1_000_000 ticks).
        let fake_now = 1_000_000_000;
        assert_eq!(scale_timer_due(fake_now + 60_000_000, fake_now, 60), -1_000_000);
        // Absolute already at or before fake_now: fire immediately.
        assert_eq!(scale_timer_due(fake_now - 5, fake_now, 60), -1);
        assert_eq!(scale_timer_due(fake_now, fake_now, 60), -1);
        // Frozen and slow motion clamp to real length (>= 1): a relative due stays real...
        assert_eq!(scale_timer_due(-6_000_000, 0, 0), -6_000_000);
        assert_eq!(scale_timer_due(-6_000_000, 0, -5), -6_000_000);
        // ...and an absolute due under frozen behaves as M=1 (documented limit).
        assert_eq!(scale_timer_due(fake_now + 60_000_000, fake_now, 0), -60_000_000);
    }

    #[test]
    fn scale_timer_period_scales_and_guards_edges() {
        // Positive period scaled by M.
        assert_eq!(scale_timer_period(6000, 60), 100);
        assert_eq!(scale_timer_period(6000, 1), 6000);
        // A >0 period never collapses to 0 (that would turn a periodic timer one-shot).
        assert_eq!(scale_timer_period(30, 60), 1);
        // 0 = one-shot stays 0; negative (API error) passes through untouched.
        assert_eq!(scale_timer_period(0, 60), 0);
        assert_eq!(scale_timer_period(-5, 60), -5);
        // Frozen and slow motion clamp to real length (>= 1) - rule 3.
        assert_eq!(scale_timer_period(6000, 0), 6000);
        assert_eq!(scale_timer_period(6000, -5), 6000);
    }

    #[test]
    fn scale_timer_period_ms_scales_and_guards() {
        // Positive period scaled by M (u32 variant for SetThreadpoolTimer's DWORD msPeriod).
        assert_eq!(scale_timer_period_ms(6000, 60), 100);
        assert_eq!(scale_timer_period_ms(6000, 1), 6000);
        // A >0 period never collapses to 0 (would turn a periodic timer one-shot).
        assert_eq!(scale_timer_period_ms(30, 60), 1);
        // 0 = one-shot stays 0.
        assert_eq!(scale_timer_period_ms(0, 60), 0);
        // Frozen and slow motion clamp to real length (>= 1) - rule 3.
        assert_eq!(scale_timer_period_ms(6000, 0), 6000);
        assert_eq!(scale_timer_period_ms(6000, -5), 6000);
    }

    #[test]
    fn scale_timer_elapse_divides_by_multiplier() {
        // Real interval = uElapse / M.
        assert_eq!(scale_timer_elapse(6000, 60), 100);
        assert_eq!(scale_timer_elapse(6000, 1), 6000);
        // Sub-M truncates toward 0; Windows then clamps up to USER_TIMER_MINIMUM (not our job).
        assert_eq!(scale_timer_elapse(30, 60), 0);
        // No INFINITE guard, unlike scale_wait: a huge interval still scales.
        assert_eq!(scale_timer_elapse(0xFFFF_FFFF, 60), 0xFFFF_FFFF / 60);
        // Frozen and slow motion clamp to real length (>= 1) - rule 3.
        assert_eq!(scale_timer_elapse(6000, 0), 6000);
        assert_eq!(scale_timer_elapse(6000, -5), 6000);
    }
}
