//! Control-memory contract shared by the mechanism (chrono-mech) and the injected
//! hook (chrono-hook). Both processes map the SAME `#[repr(C)]` layouts into named
//! shared sections, so they are defined in exactly one place.
//!
//! ONE section, `Ctl` in `Local\ChronoCtl`, holding three kinds of field:
//!
//! - The ANCHOR fields (`a_fake`, `a_real`, `multiplier`, and the duration anchor `dur_tick_c0` /
//!   `dur_quit_c0` / `dur_q0`) are written by the mechanism under a seqlock and read by every hook
//!   (parent and children share ONE fake clock - ADR-3). The duration anchor rides the same seqlock
//!   as the wall anchor, so a hook reads the multiplier and the duration base as one snapshot. The
//!   stable config fields (`tz_bias`, `scale_dur`, `core_pid`) are written once before the target
//!   exists.
//!
//! - The PID REGISTRY (`pid_count`, `pids`) lets each hooked process claim a slot and publish its
//!   own PID, so the mechanism knows which processes joined the session.
//!
//! - `covs` is PER-PROCESS coverage, one `Cov` per registry slot, each written only by the process
//!   that reserved it and read by the mechanism. One writer per slot, so plain volatile access is
//!   enough. (A `calls` increment is a volatile RMW, so concurrent target threads hitting the SAME
//!   channel may lose a bump - that only ever UNDER-counts live evidence, never fabricates
//!   coverage.) A SLOT per process, rather than one shared counter set, is what stops a child's
//!   calls from being summed into the parent's report (untouchable rule 4).
//!
//! Coverage lives HERE, in the session-wide block, rather than in a per-process section named after
//! the pid, because a process's evidence has to outlive the process. A section kept alive only by
//! the owning process's own handle died with a child shorter-lived than the mechanism's poll, and
//! the audit lost that child's evidence for good (stability audit S-9, measured as a certainty for
//! sub-100 ms helpers, not a rare race). The mechanism holds this block for the whole session, so a
//! slot's evidence survives its writer by construction. It also removes the pid-recycling hazard
//! that a name-addressed section carried: a slot is reserved once and never reused.
//!
//! Reserving a slot uses a REAL atomic, not a volatile RMW: a lost reservation would hand two
//! processes the same slot and drop a whole child from the audit, not merely under-count it.
//!
//! Fake wall time is `a_fake + (quit_now - a_real) * multiplier`, in 100 ns units,
//! anchored on `QueryUnbiasedInterruptTime` (ADR-5). UTC channels return that instant
//! directly; `GetLocalTime` returns it shifted back into the session zone by `tz_bias`.

use std::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use std::sync::atomic::{fence, AtomicU32, Ordering};

/// Named shared section for a session's control memory (per interactive session).
pub const CTL_SECTION_NAME: &str = "Local\\ChronoCtl";

/// The last fake instant the wall channels will report: the final whole second Windows can still turn
/// into a calendar date (`FileTimeToSystemTime` rejects anything past the signed range, which lands in
/// the year 30828).
///
/// Past this point the channels stop agreeing with each other, which is worse than a clock that
/// stops. Measured before this clamp existed, starting at 30828-09-01 and running at the maximum
/// multiplier: `GetSystemTimeAsFileTime` handed the target a wrapped instant while `GetSystemTime`
/// silently returned the REAL date - two epochs inside one process, with the audit reporting `works`.
/// The multiplier bound alone does not prevent it, because the session may START near the end of the
/// range, so the clamp is the second half of the same fix (R2-K2).
///
/// Clamping holds the fake clock at the edge instead: every channel keeps agreeing, the clock never
/// runs backward (untouchable rule 3), and it never quietly becomes the real one (rule 2).
pub const FAKE_WALL_MAX: i64 = i64::MAX - (i64::MAX % 10_000_000);

// Checked when the crate compiles, not when a test runs: the clamp has to be a value Windows can
// still turn into a calendar date, or it would fail the very conversion it exists to keep working.
const _: () = {
    assert!(FAKE_WALL_MAX > 0, "the clamp must stay in the signed range FileTimeToSystemTime accepts");
    assert!(FAKE_WALL_MAX % 10_000_000 == 0, "a whole second, no partial tick");
    assert!(i64::MAX - FAKE_WALL_MAX < 10_000_000, "within one second of the end of the range");
};

/// Maximum number of processes (parent + children) whose coverage a session tracks.
/// Installers can spawn dozens of helpers (docs/07 open item 2); 256 leaves headroom.
/// Beyond it, `reserve_cov_slot` returns None and that process runs uncovered in the
/// audit - an honest partial, never a silent overwrite. It also sets the size of the
/// control block, since every slot carries a `Cov`.
pub const MAX_COV_PIDS: usize = 256;

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
/// Coverage bit: `QueryPerformanceCounter` is hooked (QPC axis, opt-in `scale_qpc`, ADR-2 reversal).
pub const CH_QPC: u64 = 1 << 35;

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
pub const IDX_QPC: usize = 35;

/// Number of channels tracked (wall-clock, session zone, duration axis, object/message waits,
/// settable timers, multimedia timer, thread-pool timers, direct process creation, network connect,
/// the QPC axis).
pub const CHANNEL_COUNT: usize = 36;

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
    /// Scaled, but only when the session asked for it (`scale_qpc`, the ADR-2 reversal):
    /// `QueryPerformanceCounter`. Its own category rather than `Duration`, because the two opt-ins are
    /// deliberately separate - scaling QPC also scales a target's QPC-timed rendering, a risk
    /// `scale_duration` does not carry, so the mechanism must expect this channel under a different
    /// flag. It was outside the table entirely until R2-S3: installed by hand, and a failure to install
    /// visible only in an OutputDebugStringA line, so whoever deliberately turned the option on had no
    /// way to learn whether the QPC axis actually scaled - the one question this product exists to
    /// answer (untouchable rule 4).
    Qpc,
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
// OPT-IN UNDER ITS OWN FLAG (`scale_qpc`, the ADR-2 reversal): QueryPerformanceCounter. Left real by
// default - scaling it also scales a target's QPC-timed rendering - but a Python 3.13+ / .NET / Java
// elapsed clock stands on QPC and nothing else reaches it, so the session may ask. Kept in this table
// (R2-S3) because a channel outside it cannot be reported: the option was installed by hand and its
// failure lived in a debug string, so the one person who deliberately turned it on could not learn
// whether it took (rule 4). NtQueryPerformanceCounter stays out - ADR-2 unchanged for it.
//
// DELIBERATELY EXCLUDED, for two different reasons:
//   - ADR-2 (scaling them destabilizes the target): timeGetTime and the native
//     NtQueryPerformanceCounter.
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
    ChannelDef { bit: CH_QPC, name: "QueryPerformanceCounter", module: ChannelModule::Kernel32, category: ChannelCategory::Qpc },
];

/// Session-wide control block in `Local\ChronoCtl`. `#[repr(C)]` so both processes
/// agree on the layout. Coverage lives here too, one `Cov` per registry slot, so a
/// process's evidence outlives the process (S-9) while staying attributed to it alone.
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
    /// Duration-axis anchor (opt-in `scale_duration`), IN THE SAME SEQLOCK as the wall anchor so a
    /// hook reads the multiplier and the duration base as one consistent snapshot - a torn read that
    /// mixed the new multiplier with the old base would dip the axis (untouchable rule 3). The mechanism
    /// REBASES it on every `set_multiplier` (freezes the axis at the switch, then re-anchors), so a
    /// speed change never rewinds it, and leaves it untouched on `jump` (a wall jump must not move the
    /// duration axis). Fake `GetTickCount64` base, in milliseconds (also feeds `GetTickCount` 32-bit).
    pub dur_tick_c0: u64,
    /// Fake `QueryUnbiasedInterruptTime` base, in 100 ns units (full resolution, kept separate from the
    /// millisecond tick base so QUIT does not lose precision across rebases).
    pub dur_quit_c0: i64,
    /// Real QUIT base (100 ns) the duration elapsed is measured from. Distinct from `a_real`: a `jump`
    /// re-anchors `a_real` but must leave `dur_q0` (and so the whole duration axis) alone.
    pub dur_q0: i64,
    /// QPC axis anchor (opt-in `scale_qpc`, ADR-2 reversal). QPC is a SEPARATE system counter from QUIT
    /// (different ticks and epoch), so it needs its own base rather than riding `dur_q0`. Fake QPC base,
    /// in raw QPC ticks. Rebased on every `set_multiplier` like the tick axis (freeze then re-anchor), so
    /// a speed change never rewinds it (untouchable rule 3); left untouched on `jump`.
    pub dur_qpc_c0: i64,
    /// Real QPC base (raw ticks) the QPC elapsed is measured from - QPC is a system-wide counter, so the
    /// core reads the same value the target's hook does.
    pub dur_qpc_q0: i64,
    /// 1 = also scale the duration axis by the multiplier (the scale_duration opt-in).
    /// Stable per session: written once by the mechanism, read once by the hook.
    pub scale_dur: u32,
    /// 1 = also scale QueryPerformanceCounter by the multiplier (the scale_qpc opt-in, ADR-2 reversal).
    /// SEPARATE from scale_dur because scaling QPC also scales a target's QPC-timed rendering (a risk
    /// scale_dur does not carry). Stable per session: written once by the mechanism, read once by the hook.
    pub scale_qpc: u32,
    /// PID of the core process, so the hook can watch it and revert the target to
    /// real time when the core vanishes (clean end, crash, or kill -9). Stable.
    pub core_pid: u32,
    /// PID registry slot counter, reserved atomically by `register_pid`. Only ever
    /// increases; the mechanism does not read it - it scans `pids` for nonzero entries.
    pub pid_count: u32,
    pub _pad: u32,
    /// Registered PIDs (parent + children), indexed by reserved slot. A hook publishes its own PID
    /// here LAST, once its coverage slot carries the truth, so the mechanism never reads a pid whose
    /// slot is not yet filled in. A zero means "empty, or reserved but not yet published".
    pub pids: [u32; MAX_COV_PIDS],
    /// Per-process coverage, parallel to `pids`: slot `i` belongs to the process that published
    /// `pids[i]`. Never summed across slots - that is the whole point of the split (rule 4).
    pub covs: [Cov; MAX_COV_PIDS],
}

/// One process's coverage, living in the `Ctl` slot that process reserved. `#[repr(C)]`; written
/// only by the owning process's hook, read by the mechanism. One slot per process, so each
/// process's evidence is attributed to it and never summed with the rest of the tree.
#[repr(C)]
pub struct Cov {
    /// Bitmask of channels this process's hook installed. u64 (63 usable bits) so the channel set can
    /// grow past 32 without a layout break; the low bits still hold channels 0..N as before. Being u64
    /// also aligns `calls` to 8 bytes with no explicit padding (the old u32 + u32 pad was the same 8
    /// bytes, so the section size is unchanged).
    pub installed_channels: u64,
    /// Children THIS process spawned that the hook could not follow into (ADR-3). `inject_self` is
    /// best-effort by design - the parent is somebody else's application and we do not get to kill its
    /// child - but "best-effort" used to mean the failure existed only in an OutputDebugStringA line.
    /// The family then reported one process fewer, and the verdict said `works` without mentioning that
    /// a child had been running on the REAL clock (untouchable rule 4). A 32-bit child of a 64-bit
    /// parent is the ordinary way to reach this, and mixed installers are common.
    ///
    /// Counted here, in the SPAWNING parent's own slot, because that is the process the fact belongs to:
    /// the child never reserved a slot of its own, and never will.
    pub uninjected_children: u64,
    /// Per-channel call counters for this process, indexed by IDX_*.
    pub calls: [u64; CHANNEL_COUNT],
}

impl Cov {
    /// An empty slot. A `const` rather than `Default` so `[Cov::ZEROED; MAX_COV_PIDS]` builds the
    /// `Ctl` array without requiring `Copy` on a type that must never be copied around by accident.
    pub const ZEROED: Cov =
        Cov { installed_channels: 0, uninjected_children: 0, calls: [0; CHANNEL_COUNT] };
}

/// Size of the session control block, for CreateFileMapping.
pub const fn ctl_size() -> usize {
    core::mem::size_of::<Ctl>()
}

/// Write the WALL anchor triple under the seqlock, leaving the duration anchor untouched. This is the
/// `jump` writer: a wall jump moves the wall clock but must NOT move the duration axis (untouchable
/// rule 3), so the `dur_*` fields keep their prior values. `set_multiplier` and the initial anchor use
/// `write_anchor_full` instead. Caller guarantees `p` is a valid, aligned pointer into the shared section.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_anchor(p: *mut Ctl, a_fake: i64, a_real: i64, multiplier: i64) {
    let sp = addr_of_mut!((*p).seq);
    let s = read_volatile(sp).wrapping_add(1);
    write_volatile(sp, s); // odd - write in progress
    // Release fence: the odd-seq store is ordered before the data writes, and (below) the data
    // writes complete before the even-seq store. Paired with the reader's Acquire fences this is a
    // true cross-thread/-process seqlock, not merely a compiler barrier. On x86/x64 (TSO) a Release
    // or Acquire fence emits no instruction, so the hot anchor-read path keeps its cost; on a weakly
    // ordered ISA (ARM64) it emits the barrier that stops the anchor read from tearing.
    fence(Ordering::Release);
    write_volatile(addr_of_mut!((*p).a_fake), a_fake);
    write_volatile(addr_of_mut!((*p).a_real), a_real);
    write_volatile(addr_of_mut!((*p).multiplier), multiplier);
    fence(Ordering::Release);
    write_volatile(sp, s.wrapping_add(1)); // even - write done
}

/// Write the FULL anchor (wall triple plus the duration anchor) under the seqlock, in one transaction
/// so a reader never sees a new multiplier against an old duration base. This is the `prepare` (initial)
/// and `set_multiplier` (rebase) writer; `jump` uses `write_anchor` to leave the duration axis alone.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
#[allow(clippy::too_many_arguments)]
pub unsafe fn write_anchor_full(
    p: *mut Ctl,
    a_fake: i64,
    a_real: i64,
    multiplier: i64,
    dur_tick_c0: u64,
    dur_quit_c0: i64,
    dur_q0: i64,
    dur_qpc_c0: i64,
    dur_qpc_q0: i64,
) {
    let sp = addr_of_mut!((*p).seq);
    let s = read_volatile(sp).wrapping_add(1);
    write_volatile(sp, s); // odd - write in progress
    fence(Ordering::Release);
    write_volatile(addr_of_mut!((*p).a_fake), a_fake);
    write_volatile(addr_of_mut!((*p).a_real), a_real);
    write_volatile(addr_of_mut!((*p).multiplier), multiplier);
    write_volatile(addr_of_mut!((*p).dur_tick_c0), dur_tick_c0);
    write_volatile(addr_of_mut!((*p).dur_quit_c0), dur_quit_c0);
    write_volatile(addr_of_mut!((*p).dur_q0), dur_q0);
    write_volatile(addr_of_mut!((*p).dur_qpc_c0), dur_qpc_c0);
    write_volatile(addr_of_mut!((*p).dur_qpc_q0), dur_qpc_q0);
    fence(Ordering::Release);
    write_volatile(sp, s.wrapping_add(1)); // even - write done
}

/// How many times a seqlock reader retries before it gives up and returns a fallback (RELEASE-009). A write
/// is a handful of instructions held for nanoseconds and happens only on `set_multiplier`/`jump` (rare,
/// user-driven), so a live writer settles in a few spins - this bound is astronomically above that. It
/// exists only to cap the pathological case where the core is force-killed between the odd and even `seq`
/// writes, which would otherwise leave a reader spinning at 100% CPU forever.
const SEQLOCK_READ_TRIES: usize = 1_000_000;

/// Read the anchor triple under the seqlock, retrying on a concurrent write.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_anchor(p: *const Ctl) -> (i64, i64, i64) {
    for _ in 0..SEQLOCK_READ_TRIES {
        let s1 = read_volatile(addr_of!((*p).seq));
        if s1 & 1 == 0 {
            // Acquire fences: the data reads are ordered after the s1 (seq) read and before the s2 read,
            // so a torn read racing a concurrent writer is caught by the s1 == s2 check below. See
            // write_anchor for why Release/Acquire is zero-cost on x86/x64 yet correct on a weak ISA.
            fence(Ordering::Acquire);
            let a_fake = read_volatile(addr_of!((*p).a_fake));
            let a_real = read_volatile(addr_of!((*p).a_real));
            let multiplier = read_volatile(addr_of!((*p).multiplier));
            fence(Ordering::Acquire);
            if s1 == read_volatile(addr_of!((*p).seq)) {
                return (a_fake, a_real, multiplier);
            }
        }
        std::hint::spin_loop();
    }
    // The seqlock never settled within the bound: the writer (the core) was force-killed mid-write, leaving
    // `seq` odd forever (RELEASE-009). Do not spin at 100% CPU - read the fields once and return them. A
    // dead writer's fields are stable (a rare one-time tear is far better than a permanent hang); the
    // self-detach watcher flips DETACHED right after the core dies, so the detour stops reading this block
    // on its next call and the target falls back to real time.
    fence(Ordering::Acquire);
    (
        read_volatile(addr_of!((*p).a_fake)),
        read_volatile(addr_of!((*p).a_real)),
        read_volatile(addr_of!((*p).multiplier)),
    )
}

/// Read the duration anchor plus the multiplier under the seqlock, as ONE consistent snapshot, retrying
/// on a concurrent write. Returns `(dur_tick_c0, dur_quit_c0, dur_q0, multiplier)`. The duration detours
/// (`GetTickCount64` / `GetTickCount` / `QueryUnbiasedInterruptTime`) call this so the multiplier they
/// scale by and the base they scale from can never tear apart across a `set_multiplier` (untouchable
/// rule 3 - a mismatched pair would dip the monotonic axis).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_dur(p: *const Ctl) -> (u64, i64, i64, i64) {
    for _ in 0..SEQLOCK_READ_TRIES {
        let s1 = read_volatile(addr_of!((*p).seq));
        if s1 & 1 == 0 {
            fence(Ordering::Acquire);
            let dur_tick_c0 = read_volatile(addr_of!((*p).dur_tick_c0));
            let dur_quit_c0 = read_volatile(addr_of!((*p).dur_quit_c0));
            let dur_q0 = read_volatile(addr_of!((*p).dur_q0));
            let multiplier = read_volatile(addr_of!((*p).multiplier));
            fence(Ordering::Acquire);
            if s1 == read_volatile(addr_of!((*p).seq)) {
                return (dur_tick_c0, dur_quit_c0, dur_q0, multiplier);
            }
        }
        std::hint::spin_loop();
    }
    // A force-killed writer left `seq` odd forever - fall back rather than hang (see read_anchor, RELEASE-009).
    fence(Ordering::Acquire);
    (
        read_volatile(addr_of!((*p).dur_tick_c0)),
        read_volatile(addr_of!((*p).dur_quit_c0)),
        read_volatile(addr_of!((*p).dur_q0)),
        read_volatile(addr_of!((*p).multiplier)),
    )
}

/// Project the duration tick (milliseconds, `GetTickCount64` scale) at real time `real_now` (QUIT, 100 ns)
/// from the anchor: `dur_tick_c0 + (real_now - dur_q0) * M / 10_000`. Monotonic in `real_now` for a fixed
/// anchor; `freeze_dur` keeps it continuous across a multiplier change. `m` is clamped to >= 1, so a frozen
/// wall clock (M = 0) still advances the duration axis at real speed (untouchable rule 3). Pure so the
/// monotonicity is unit-tested without injection.
pub fn dur_tick_at(dur_tick_c0: u64, dur_q0: i64, m: i64, real_now: i64) -> u64 {
    let dm = m.max(1);
    let dq = real_now.wrapping_sub(dur_q0);
    dur_tick_c0.wrapping_add((dq.wrapping_mul(dm) / 10_000) as u64)
}

/// Project the fake `QueryUnbiasedInterruptTime` (100 ns) at real time `real_now` from the anchor:
/// `dur_quit_c0 + (real_now - dur_q0) * M`. The 100 ns companion of `dur_tick_at` (QUIT keeps full
/// resolution). `m` clamped to >= 1 (rule 3, like `dur_tick_at`).
pub fn dur_quit_at(dur_quit_c0: i64, dur_q0: i64, m: i64, real_now: i64) -> i64 {
    let dm = m.max(1);
    let dq = real_now.wrapping_sub(dur_q0);
    dur_quit_c0.wrapping_add(dq.wrapping_mul(dm))
}

/// Freeze the duration axis at real time `now` under the OLD multiplier, returning the new
/// `(dur_tick_c0, dur_quit_c0)` bases to re-anchor at `now` (the caller sets `dur_q0 = now`). Called by
/// `set_multiplier` so the axis stays CONTINUOUS across a speed change - the value right after the switch
/// equals the value right before (`dur_tick_at`/`dur_quit_at` at `now`), so it never rewinds (untouchable
/// rule 3). `old_m` is clamped to >= 1 (a frozen wall clock froze the axis at real speed, not stopped).
/// Pure and unit-tested.
pub fn freeze_dur(dur_tick_c0: u64, dur_quit_c0: i64, dur_q0: i64, old_m: i64, now: i64) -> (u64, i64) {
    (
        dur_tick_at(dur_tick_c0, dur_q0, old_m, now),
        dur_quit_at(dur_quit_c0, dur_q0, old_m, now),
    )
}

/// Read the QPC anchor plus the multiplier under the seqlock, as ONE consistent snapshot (a torn read
/// mixing a new multiplier with an old QPC base would dip the axis - rule 3). Returns `(dur_qpc_c0,
/// dur_qpc_q0, multiplier)`. Companion of `read_dur` for the QPC detour (opt-in `scale_qpc`, ADR-2).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_qpc(p: *const Ctl) -> (i64, i64, i64) {
    for _ in 0..SEQLOCK_READ_TRIES {
        let s1 = read_volatile(addr_of!((*p).seq));
        if s1 & 1 == 0 {
            fence(Ordering::Acquire);
            let dur_qpc_c0 = read_volatile(addr_of!((*p).dur_qpc_c0));
            let dur_qpc_q0 = read_volatile(addr_of!((*p).dur_qpc_q0));
            let multiplier = read_volatile(addr_of!((*p).multiplier));
            fence(Ordering::Acquire);
            if s1 == read_volatile(addr_of!((*p).seq)) {
                return (dur_qpc_c0, dur_qpc_q0, multiplier);
            }
        }
        std::hint::spin_loop();
    }
    // A force-killed writer left `seq` odd forever - fall back rather than hang (see read_dur, RELEASE-009).
    fence(Ordering::Acquire);
    (
        read_volatile(addr_of!((*p).dur_qpc_c0)),
        read_volatile(addr_of!((*p).dur_qpc_q0)),
        read_volatile(addr_of!((*p).multiplier)),
    )
}

/// Project the fake QueryPerformanceCounter (raw QPC ticks) at real QPC `real_now` from the anchor:
/// `dur_qpc_c0 + (real_now - dur_qpc_q0) * M`. QueryPerformanceFrequency is NOT scaled, so elapsed
/// (delta / freq) scales by exactly M. `m` clamped to >= 1 (rule 3, like `dur_quit_at`). Pure, so the
/// monotonicity is unit-tested without injection.
pub fn dur_qpc_at(dur_qpc_c0: i64, dur_qpc_q0: i64, m: i64, real_now: i64) -> i64 {
    let dm = m.max(1);
    let dq = real_now.wrapping_sub(dur_qpc_q0);
    dur_qpc_c0.wrapping_add(dq.wrapping_mul(dm))
}

/// Freeze the QPC axis at real QPC `now` under the OLD multiplier, returning the new `dur_qpc_c0` to
/// re-anchor at `now` (the caller sets `dur_qpc_q0 = now`). Called by `set_multiplier` so the QPC axis
/// stays CONTINUOUS across a speed change - the value right after the switch equals the value right
/// before, so it never rewinds (untouchable rule 3). Pure and unit-tested.
pub fn freeze_qpc(dur_qpc_c0: i64, dur_qpc_q0: i64, old_m: i64, now: i64) -> i64 {
    dur_qpc_at(dur_qpc_c0, dur_qpc_q0, old_m, now)
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

/// Write the scale-QPC flag (stable field, outside the seqlock). Mechanism side (ADR-2 reversal, opt-in).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_scale_qpc(p: *mut Ctl, on: bool) {
    write_volatile(addr_of_mut!((*p).scale_qpc), on as u32);
}

/// Read the scale-QPC flag (hook side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_scale_qpc(p: *const Ctl) -> bool {
    read_volatile(addr_of!((*p).scale_qpc)) != 0
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

/// Reserve this process's coverage slot (hook side). Reserves atomically - several children may
/// start concurrently - and returns the slot index, or None if the registry is full (the process
/// then runs uncovered in the audit, an honest partial, never a silent overwrite of a live slot).
///
/// Call this EARLY, before enabling the detours: a detour that fires needs somewhere to count, and
/// the slot pointer has to exist by then. Publishing the PID is a separate, later step
/// (`publish_pid`) so the mechanism only ever sees a pid whose slot already holds the truth.
///
/// A process whose hook fails after this point keeps its slot without ever publishing a pid. The
/// mechanism skips it (the pid stays zero), so the cost of that failure is one wasted slot out of
/// `MAX_COV_PIDS`, not a wrong report.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn reserve_cov_slot(p: *mut Ctl) -> Option<usize> {
    // Atomic reservation: a volatile RMW could hand two children the same slot and
    // lose one from the audit entirely (worse than the calls under-count).
    let counter = &*(addr_of!((*p).pid_count) as *const AtomicU32);
    let slot = counter.fetch_add(1, Ordering::SeqCst) as usize;
    if slot >= MAX_COV_PIDS {
        return None;
    }
    Some(slot)
}

/// Publish this process's PID into the slot it reserved (hook side), announcing to the mechanism
/// that the slot is ready to read. Call LAST, after the coverage slot carries the installed mask.
///
/// The release fence pairs with the acquire in `read_pid`: it keeps the writes that filled the slot
/// from being seen after the pid that advertises them. On x86 this costs nothing at runtime, but it
/// is what stops the compiler from reordering the publication ahead of the evidence.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `slot < MAX_COV_PIDS`.
pub unsafe fn publish_pid(p: *mut Ctl, slot: usize, pid: u32) {
    fence(Ordering::Release);
    let slotp = (addr_of_mut!((*p).pids) as *mut u32).add(slot);
    write_volatile(slotp, pid);
}

/// Read one PID registry slot (mechanism side). `i` must be < MAX_COV_PIDS. A zero
/// means "empty or not yet published" - the mechanism scans all slots and skips zeros,
/// so a slot reserved but not yet published is simply picked up on the next refresh.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `i < MAX_COV_PIDS`.
/// How many processes have tried to claim a coverage slot this session (mechanism side). Counts
/// ATTEMPTS, not occupied slots: `reserve_cov_slot` increments unconditionally, so anything above
/// `MAX_COV_PIDS` is the number of processes that ran with no slot to report into.
///
/// Slots are deliberately never freed or reused - that is what makes a slot outlive the process that
/// wrote it (stability audit S-9), and it is what removed the pid-recycling hazard - so recycling them
/// is NOT the fix for a full registry, however the finding phrased it. Saying so is (R2-S9): the
/// overflow used to exist only in an OutputDebugStringA line, and the audit reported a smaller family
/// without a word about the processes it could not see.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_pid_count(p: *const Ctl) -> u32 {
    let counter = &*(addr_of!((*p).pid_count) as *const AtomicU32);
    counter.load(Ordering::SeqCst)
}

/// Read one PID registry slot (mechanism side). `i` must be < MAX_COV_PIDS. A zero
/// means "empty or not yet published" - the mechanism scans all slots and skips zeros,
/// so a slot reserved but not yet published is simply picked up on the next refresh.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `i < MAX_COV_PIDS`.
pub unsafe fn read_pid(p: *const Ctl, i: usize) -> u32 {
    let slotp = (addr_of!((*p).pids) as *const u32).add(i);
    let pid = read_volatile(slotp);
    if pid != 0 {
        // Pairs with the release in `publish_pid`, so the slot's contents are visible to whoever
        // just saw the pid that advertises them.
        fence(Ordering::Acquire);
    }
    pid
}

/// Pointer to one slot's coverage (hook side, its own slot). `i` must be < MAX_COV_PIDS.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `i < MAX_COV_PIDS`.
pub unsafe fn cov_at_mut(p: *mut Ctl, i: usize) -> *mut Cov {
    (addr_of_mut!((*p).covs) as *mut Cov).add(i)
}

/// Pointer to one slot's coverage (mechanism side, read-only). `i` must be < MAX_COV_PIDS.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `i < MAX_COV_PIDS`.
pub unsafe fn cov_at(p: *const Ctl, i: usize) -> *const Cov {
    (addr_of!((*p).covs) as *const Cov).add(i)
}

/// Publish the set of installed channels (hook side, per-process `Cov`). A whole-mask WRITE,
/// not an OR-in: the hook side collects bits locally while it creates the detours and publishes
/// them only once every detour is actually ENABLED, so a failure part-way through can publish
/// nothing (mask 0) instead of leaving a half-set claim behind. A bit here means "this channel's
/// detour is live", and the audit reads it as such - so it must never be set for a detour that was
/// merely prepared (rule 4: the audit never claims a channel it did not cover).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn set_channels_installed(p: *mut Cov, mask: u64) {
    write_volatile(addr_of_mut!((*p).installed_channels), mask);
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

/// Record that a child this process spawned could not be followed into (hook side, own `Cov`).
/// Same volatile RMW as `bump_calls`: two threads spawning at once may lose a bump, which can only
/// ever UNDER-count a failure, never invent one - and one is already enough to raise the warning.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn bump_uninjected_children(p: *mut Cov) {
    let slot = addr_of_mut!((*p).uninjected_children);
    let cur = read_volatile(slot);
    write_volatile(slot, cur.wrapping_add(1));
}

/// How many children this process spawned without coverage (mechanism side, per-process `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_uninjected_children(p: *const Cov) -> u64 {
    read_volatile(addr_of!((*p).uninjected_children))
}

/// Scale a wait timeout in milliseconds by the duration multiplier: real wait =
/// requested / M (ADR-7). `INFINITE` (0xFFFFFFFF) and 0 pass through untouched - never
/// turn "wait forever" into a finite wait, never lengthen a poll. `m` is clamped to >= 1,
/// so frozen (M=0) leaves waits at real length (untouchable rule 3). The multiplier is an integer,
/// so a fractional "slow motion" (0<M<1) is not representable and never arises; symmetric to the
/// duration axis. Integer division truncates: a sub-M timeout
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
/// `m` is clamped to >= 1, so frozen (M=0) leaves the timer at real length; the multiplier is an
/// integer, so a fractional "slow motion" (0<M<1) is not representable and never arises (symmetric
/// to the duration axis, untouchable rule 3). One consequence: an ABSOLUTE timer under
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

    /// Boxed, not returned by value: the block now carries a `Cov` per slot (tens of kilobytes), and
    /// a by-value helper would push that through the test thread's stack on every call.
    fn zeroed_ctl() -> Box<Ctl> {
        Box::new(Ctl {
            seq: 0,
            tz_bias: 0,
            a_fake: 0,
            a_real: 0,
            multiplier: 0,
            dur_tick_c0: 0,
            dur_quit_c0: 0,
            dur_q0: 0,
            dur_qpc_c0: 0,
            dur_qpc_q0: 0,
            scale_dur: 0,
            scale_qpc: 0,
            core_pid: 0,
            pid_count: 0,
            _pad: 0,
            pids: [0; MAX_COV_PIDS],
            covs: [Cov::ZEROED; MAX_COV_PIDS],
        })
    }

    fn zeroed_cov() -> Cov {
        Cov::ZEROED
    }

    #[test]
    fn anchor_round_trips() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
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
    fn full_anchor_round_trips_and_wall_write_preserves_duration() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            // write_anchor_full writes the wall triple AND the duration anchor (tick/quit + QPC).
            write_anchor_full(p, 134_000_000_000_000_000, 42, 60, 5_000, 42, 42, 700, 700);
            let (af, ar, m) = read_anchor(p);
            assert_eq!((af, ar, m), (134_000_000_000_000_000, 42, 60));
            let (tick_c0, quit_c0, q0, dm) = read_dur(p);
            assert_eq!((tick_c0, quit_c0, q0, dm), (5_000, 42, 42, 60));
            let (qpc_c0, qpc_q0, qm) = read_qpc(p);
            assert_eq!((qpc_c0, qpc_q0, qm), (700, 700, 60));

            // write_anchor (the jump writer) moves the wall clock but must NOT touch the duration anchor.
            write_anchor(p, 999, 77, 60);
            let (af2, ar2, _) = read_anchor(p);
            assert_eq!((af2, ar2), (999, 77));
            let (tick_c0b, quit_c0b, q0b, _) = read_dur(p);
            assert_eq!((tick_c0b, quit_c0b, q0b), (5_000, 42, 42), "jump must leave the duration axis alone");
            let (qpc_c0b, qpc_q0b, _) = read_qpc(p);
            assert_eq!((qpc_c0b, qpc_q0b), (700, 700), "jump must leave the QPC axis alone");
        }
    }

    #[test]
    fn duration_axis_never_rewinds_across_multiplier_changes() {
        // H-1 regression guard: a multiplier change must re-anchor the duration axis so it stays
        // continuous, never dips (untouchable rule 3). Replays the mechanism's rebase math (freeze_dur
        // then re-anchor at `now`) against a rising real clock, sampling GetTickCount64 densely.
        let mut tick_c0: u64 = 1_000_000; // ms
        let mut quit_c0: i64 = 10_000_000; // 100 ns
        let mut q0: i64 = 10_000_000; // real QUIT base, 100 ns
        let mut m: i64 = 60;

        // Real QUIT (100 ns) advances by 1 ms each sample. The multiplier drops (x60 -> x10 -> freeze
        // -> x1) then jumps back up (-> x1440); the down-steps are the ones that used to rewind.
        let changes: &[(i64, i64)] = &[(500, 10), (900, 0), (1300, 1), (1700, 1440)]; // (sample index, new m)
        let mut last_tick: u64 = dur_tick_at(tick_c0, q0, m, q0);
        let mut last_quit: i64 = dur_quit_at(quit_c0, q0, m, q0);
        let mut change_i = 0;
        for step in 0..2_500i64 {
            let now = 10_000_000 + step * 10_000; // +1 ms per step, in 100 ns units

            if change_i < changes.len() && step == changes[change_i].0 {
                let new_m = changes[change_i].1;
                let (frozen_tick, frozen_quit) = freeze_dur(tick_c0, quit_c0, q0, m, now);
                // Continuity: the value right after the switch equals the value right before it.
                assert_eq!(frozen_tick, dur_tick_at(tick_c0, q0, m, now), "tick jumped at the switch");
                assert_eq!(frozen_quit, dur_quit_at(quit_c0, q0, m, now), "quit jumped at the switch");
                tick_c0 = frozen_tick;
                quit_c0 = frozen_quit;
                q0 = now;
                m = new_m;
                change_i += 1;
            }

            let tick = dur_tick_at(tick_c0, q0, m, now);
            let quit = dur_quit_at(quit_c0, q0, m, now);
            assert!(tick >= last_tick, "GetTickCount64 rewound: {last_tick} -> {tick} at step {step}");
            assert!(quit >= last_quit, "QUIT rewound: {last_quit} -> {quit} at step {step}");
            last_tick = tick;
            last_quit = quit;
        }

        // Frozen wall (M = 0) still advances the axis at real speed (clamp to >= 1).
        let a = dur_tick_at(1_000, 0, 0, 5_000_000);
        let b = dur_tick_at(1_000, 0, 0, 6_000_000);
        assert!(b > a, "a frozen wall clock must not stop the monotonic duration axis (rule 3)");
    }

    #[test]
    fn qpc_axis_never_rewinds_across_multiplier_changes() {
        // ADR-2 reversal (A1): a multiplier change must re-anchor the QPC axis so it stays continuous,
        // never dips (rule 3) - exactly what the spike's lazy anchor got WRONG. Replays freeze_qpc then
        // re-anchor at `now` against a rising real QPC, with the multiplier dropping then jumping up.
        let mut qpc_c0: i64 = 5_000_000; // raw QPC ticks
        let mut q0: i64 = 5_000_000; // real QPC base
        let mut m: i64 = 60;
        let changes: &[(i64, i64)] = &[(500, 10), (900, 0), (1300, 1), (1700, 1440)];
        let mut last: i64 = dur_qpc_at(qpc_c0, q0, m, q0);
        let mut ci = 0;
        for step in 0..2_500i64 {
            let now = 5_000_000 + step * 100; // real QPC advances 100 ticks per step
            if ci < changes.len() && step == changes[ci].0 {
                let new_m = changes[ci].1;
                let frozen = freeze_qpc(qpc_c0, q0, m, now);
                assert_eq!(frozen, dur_qpc_at(qpc_c0, q0, m, now), "qpc jumped at the switch");
                qpc_c0 = frozen;
                q0 = now;
                m = new_m;
                ci += 1;
            }
            let qpc = dur_qpc_at(qpc_c0, q0, m, now);
            assert!(qpc >= last, "QPC rewound: {last} -> {qpc} at step {step}");
            last = qpc;
        }
        // Frozen wall (M = 0) still advances the QPC axis at real speed (clamp to >= 1).
        let a = dur_qpc_at(1_000, 0, 0, 5_000);
        let b = dur_qpc_at(1_000, 0, 0, 6_000);
        assert!(b > a, "a frozen wall clock must not stop the QPC axis (rule 3)");
    }

    #[test]
    fn scale_qpc_round_trips() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            assert!(!read_scale_qpc(p));
            write_scale_qpc(p, true);
            assert!(read_scale_qpc(p));
        }
    }

    #[test]
    fn tz_bias_round_trips() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            write_tz_bias(p, -120);
            assert_eq!(read_tz_bias(p), -120);
        }
    }

    #[test]
    fn scale_dur_round_trips() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            assert!(!read_scale_dur(p));
            write_scale_dur(p, true);
            assert!(read_scale_dur(p));
        }
    }

    #[test]
    fn pid_registry_assigns_distinct_slots_and_reads_back() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            for (i, pid) in [1111u32, 2222, 3333].into_iter().enumerate() {
                let slot = reserve_cov_slot(p).expect("slot should fit");
                assert_eq!(slot, i, "slots are handed out in order");
                publish_pid(p, slot, pid);
            }
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
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            for i in 0..MAX_COV_PIDS {
                let slot = reserve_cov_slot(p).unwrap_or_else(|| panic!("slot {i} should fit"));
                publish_pid(p, slot, (i as u32) + 1);
            }
            // One past the end: honest None, no overwrite of a live slot.
            assert!(reserve_cov_slot(p).is_none());
            assert_eq!(read_pid(p, 0), 1);
            assert_eq!(read_pid(p, MAX_COV_PIDS - 1), MAX_COV_PIDS as u32);
        }
    }

    #[test]
    fn reserved_slot_stays_invisible_until_the_pid_is_published() {
        // The whole ordering contract: a hook reserves early (its detours need somewhere to count)
        // but publishes last. Until it does, the mechanism must not see the process at all - reading
        // a half-filled slot as evidence would be the audit claiming what it does not know (rule 4).
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            let slot = reserve_cov_slot(p).expect("first slot");
            set_channels_installed(cov_at_mut(p, slot), CH_GSTAFT);
            bump_calls(cov_at_mut(p, slot), IDX_GSTAFT);
            assert_eq!(read_pid(p, slot), 0, "reserved but unpublished reads as empty");

            publish_pid(p, slot, 4242);
            assert_eq!(read_pid(p, slot), 4242);
            assert_eq!(read_installed(cov_at(p, slot)), CH_GSTAFT);
            assert_eq!(read_calls(cov_at(p, slot), IDX_GSTAFT), 1);
        }
    }

    #[test]
    fn coverage_slots_are_independent_per_process() {
        // Untouchable rule 4: a child's calls are never summed into the parent's report. With
        // coverage in the shared block, that guarantee rests on slot separation, so pin it.
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            let parent = reserve_cov_slot(p).expect("parent slot");
            let child = reserve_cov_slot(p).expect("child slot");
            assert_ne!(parent, child);

            set_channels_installed(cov_at_mut(p, parent), CH_GSTAFT);
            set_channels_installed(cov_at_mut(p, child), CH_NTQST);
            bump_calls(cov_at_mut(p, parent), IDX_GSTAFT);
            bump_calls(cov_at_mut(p, child), IDX_NTQST);
            bump_calls(cov_at_mut(p, child), IDX_NTQST);

            assert_eq!(read_installed(cov_at(p, parent)), CH_GSTAFT);
            assert_eq!(read_installed(cov_at(p, child)), CH_NTQST);
            assert_eq!(read_calls(cov_at(p, parent), IDX_GSTAFT), 1);
            assert_eq!(read_calls(cov_at(p, parent), IDX_NTQST), 0, "child calls stay the child's");
            assert_eq!(read_calls(cov_at(p, child), IDX_NTQST), 2);
        }
    }

    /// R2-S2. The uninjected-child count lives beside the channel counters but is NOT one of them:
    /// no channel index reaches it, and bumping channels never moves it.
    #[test]
    fn uninjected_children_count_separately_from_channels() {
        let mut cov = zeroed_cov();
        let p = &mut cov as *mut Cov;
        unsafe {
            assert_eq!(read_uninjected_children(p), 0);
            bump_uninjected_children(p);
            bump_uninjected_children(p);
            assert_eq!(read_uninjected_children(p), 2);

            // Channel counters and this counter do not alias each other in either direction.
            bump_calls(p, IDX_GSTAFT);
            assert_eq!(read_uninjected_children(p), 2);
            assert_eq!(read_calls(p, IDX_GSTAFT), 1);
            bump_uninjected_children(p);
            assert_eq!(read_calls(p, IDX_GSTAFT), 1);
            assert_eq!(read_calls(p, CHANNEL_COUNT - 1), 0);
        }
    }

    #[test]
    fn channels_accumulate_and_count_per_index() {
        let mut cov = zeroed_cov();
        let p = &mut cov as *mut Cov;
        unsafe {
            assert_eq!(read_installed(p), 0);
            set_channels_installed(p, CH_GSTAFT | CH_NTQST);
            assert_eq!(read_installed(p) & CH_GSTAFT, CH_GSTAFT);
            assert_eq!(read_installed(p) & CH_NTQST, CH_NTQST);
            assert_eq!(read_installed(p) & CH_GLT, 0);

            // A whole-mask write, so a later publish REPLACES the claim rather than adding to it.
            // That is what lets the hook publish nothing when enabling the detours failed.
            set_channels_installed(p, 0);
            assert_eq!(read_installed(p), 0);
            set_channels_installed(p, CH_GSTAFT | CH_NTQST);

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
            (IDX_CONNECT, CH_CONNECT),
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
