//! Control-memory contract shared by the mechanism (chrono-mech) and the injected
//! hook (chrono-hook). Both processes map the SAME `#[repr(C)]` layouts into named
//! shared sections, so they are defined in exactly one place.
//!
//! ONE section, `Ctl` in `Local\ChronoCtl`, holding three kinds of field:
//!
//! - The ANCHOR fields (`a_fake`, `a_real`, `multiplier`, and the duration anchor `dur_tick_offset` /
//!   `dur_quit_c0` / `dur_q0`) are written by the mechanism under a seqlock and read by every hook
//!   (parent and children share ONE fake clock - ADR-3). The duration anchor rides the same seqlock
//!   as the wall anchor, so a hook reads the multiplier and the duration base as one snapshot, with
//!   the real clock it projects from read inside the same window (`read_dur_with`, R4-S3). The
//!   stable config fields (`tz_bias`, `zone_name`, `scale_dur`, `core_created`, `core_pid`) are written
//!   once before the target exists. `ended` is the one written later, once, when the core ends the session.
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
//! directly - `GetLocalTime` returns it shifted back into the session zone by `tz_bias`.

use std::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

pub mod inject;
#[cfg(test)]
mod search;
pub mod zone_name;
use std::sync::atomic::{fence, AtomicI64, AtomicU32, AtomicUsize, Ordering};

/// Named shared section for a session's control memory (per interactive session).
///
/// 🔴 This is the ONE source of the name, and until 2026-09-06 it was not: the constant sat here
/// reading like a contract while the real name was a `w!` literal repeated in FOUR places across
/// `chrono-hook` and `chrono-mech`, so changing it changed nothing. The dead-code guard in
/// `crates/cli/tests/hygiene.rs` found it by noticing that nothing read it.
pub const CTL_SECTION_NAME: &str = "Local\\ChronoCtl";

/// The same name as a NUL-terminated UTF-16 string, which is what the Win32 object APIs take.
///
/// `w!` cannot serve here - it is a macro over a string LITERAL and cannot be handed a constant -
/// and `chrono-ctl` deliberately has no dependencies at all (the layer guard enforces that), so
/// the `windows` crate's `PCWSTR` cannot appear in this crate either. A `static` rather than a
/// `const` on purpose: a `const` is inlined at every use, so taking a pointer to one hands the
/// API a pointer into a temporary.
pub static CTL_SECTION_NAME_W: [u16; 16] = ascii_utf16_z(CTL_SECTION_NAME);

/// Cross-process lock guarding the creation of the section above.
pub const CTL_LOCK_NAME: &str = concat!("Local", "\\", "ChronoCtl.lock");

/// The lock name as NUL-terminated UTF-16.
pub static CTL_LOCK_NAME_W: [u16; 21] = ascii_utf16_z(CTL_LOCK_NAME);

/// ASCII string to a NUL-terminated UTF-16 array, at compile time.
///
/// ASCII only, and it refuses anything else: these are fixed object names this crate owns, not
/// user input. `N` must be the length plus one for the terminator, and a mismatch is a COMPILE
/// error rather than a truncated object name - which would silently open a different section and
/// hand the target a clock nobody is driving.
const fn ascii_utf16_z<const N: usize>(s: &str) -> [u16; N] {
    let bytes = s.as_bytes();
    assert!(bytes.len() + 1 == N, "N must be the string length plus the NUL terminator");
    let mut out = [0u16; N];
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i] < 128, "ascii_utf16_z takes ASCII only");
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}

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
///
/// 🔴 The clamp stops one zone bias short of the end of the range, and that headroom is load-bearing
/// (R2-X7). The local-time channels express the same instant as `fake - bias`, so a clamp sitting on
/// the very last tick overflowed for every zone east of UTC: `GetLocalTime` failed its conversion and
/// fell through to the REAL clock. Measured at 30828-09-01 under the maximum multiplier, on a host at
/// UTC+02:00 - `GetSystemTime` held at 30828-09-14T02:48:05 while `GetLocalTime` answered
/// 2026-09-03T20:54:05. One process, two epochs: exactly the failure this clamp exists to end,
/// surviving on the channel the clamp forgot.
pub const FAKE_WALL_MAX: i64 = {
    let raw = i64::MAX - MAX_ZONE_BIAS_TICKS;
    raw - (raw % 10_000_000)
};

/// The largest session-zone bias the tool accepts, in 100 ns ticks. `parse_zone_to_bias` takes hours
/// 0..=14 and minutes 0..=59, so fifteen hours covers every accepted offset with room to spare.
pub const MAX_ZONE_BIAS_TICKS: i64 = 15 * 3600 * 10_000_000;

// Checked when the crate compiles, not when a test runs: the clamp has to be a value Windows can
// still turn into a calendar date, or it would fail the very conversion it exists to keep working.
const _: () = {
    assert!(FAKE_WALL_MAX > 0, "the clamp must stay in the signed range FileTimeToSystemTime accepts");
    assert!(FAKE_WALL_MAX % 10_000_000 == 0, "a whole second, no partial tick");
    assert!(
        i64::MAX - FAKE_WALL_MAX >= MAX_ZONE_BIAS_TICKS,
        "the local-time channels subtract the zone bias from the clamped instant, so the clamp must \
         leave room for the largest bias the tool accepts - without it they overflow and fall back to \
         the REAL clock (R2-X7)"
    );
    assert!(
        i64::MAX - FAKE_WALL_MAX < MAX_ZONE_BIAS_TICKS + 10_000_000,
        "and no more headroom than that, so the clamp stays the last instant it can be"
    );
};

/// Maximum number of processes (parent + children) whose coverage a session tracks.
/// Beyond it, `reserve_cov_slot` returns None and that process still runs on the session clock, but
/// outside the audit - an honest partial, never a silent overwrite. It also sets the size of the
/// control block, since every slot carries a `Cov`.
///
/// Slots are never reused (S-9, R2-S9), so what fills the registry is a session's whole history, not
/// how many processes run at once: an installer's dozens of helpers, or one helper an application
/// starts every ten seconds, which filled the old 256 in about 43 minutes (R5-P26). 4096 takes that to
/// about eleven hours. At 552 bytes a slot the block is about 2.3 MB, mapped by every process of the
/// session and committed only where it is written, which is what was weighed against reusing slots.
pub const MAX_COV_PIDS: usize = 4096;

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
/// Coverage bit: network connections are observed (every connection attempt, whichever API made it,
/// counted at `NtDeviceIoControlFile` - a suspected server time source, see `ChannelDef::export`).
pub const CH_CONNECT: u64 = 1 << 34;
/// Coverage bit: `QueryPerformanceCounter` is hooked (QPC axis, opt-in `scale_qpc`, ADR-2 reversal).
pub const CH_QPC: u64 = 1 << 35;
/// Coverage bit: `timeGetTime` is hooked and SCALED on the duration axis (winmm millisecond clock,
/// opt-in `scale_duration`, partial ADR-2 reversal of 2026-09-07).
pub const CH_TIMEGETTIME: u64 = 1 << 36;
/// Coverage bit: `SleepConditionVariableSRW` is hooked (condition-variable wait, observed not scaled).
pub const CH_SCVSRW: u64 = 1 << 37;
/// Coverage bit: `SleepConditionVariableCS` is hooked (condition-variable wait, observed not scaled).
pub const CH_SCVCS: u64 = 1 << 38;
/// Coverage bit: `WaitOnAddress` is hooked (address wait, observed not scaled).
pub const CH_WOA: u64 = 1 << 39;
/// Coverage bit: `WSAWaitForMultipleEvents` is hooked (socket event wait, observed not scaled).
pub const CH_WSAWFME: u64 = 1 << 40;
/// Coverage bit: `QueryInterruptTime` is hooked (duration axis, opt-in, R5-F4).
pub const CH_QIT: u64 = 1 << 41;
/// Coverage bit: `QueryInterruptTimePrecise` is hooked (duration axis, opt-in, R5-F4).
pub const CH_QITP: u64 = 1 << 42;
/// Coverage bit: `QueryUnbiasedInterruptTimePrecise` is hooked (duration axis, opt-in, R5-F4).
pub const CH_QUITP: u64 = 1 << 43;
/// Coverage bit: `GetTimeZoneInformationForYear` is hooked (session zone, the way the Rust `chrono`
/// crate asks for the current zone).
pub const CH_GTZIFY: u64 = 1 << 44;

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
pub const IDX_TIMEGETTIME: usize = 36;
pub const IDX_SCVSRW: usize = 37;
pub const IDX_SCVCS: usize = 38;
pub const IDX_WOA: usize = 39;
pub const IDX_WSAWFME: usize = 40;
pub const IDX_QIT: usize = 41;
pub const IDX_QITP: usize = 42;
pub const IDX_QUITP: usize = 43;
pub const IDX_GTZIFY: usize = 44;

/// Number of channels tracked (wall-clock, session zone, duration axis, object/message/condition/
/// address/socket waits, settable timers, multimedia timer, thread-pool timers, direct process
/// creation, network connect, the QPC axis, the winmm millisecond clock, the interrupt-time clocks,
/// the zone of a given year).
pub const CHANNEL_COUNT: usize = 45;

/// The channels that carry an ABSOLUTE deadline - the settable and thread-pool timers and
/// `NtDelayExecution` - and so are installed in EVERY session, not only under `scale_duration` (R5-D2).
///
/// An absolute deadline is a wall-clock instant the application computed from the substituted clock,
/// and the kernel compares it with the REAL clock. That is true of a plain date shift as much as of an
/// accelerated one: a timer set for "tomorrow 03:00" in a session at 2038 waited twelve real years
/// without `--scale-duration`, and one set in the past fired at once. So these detours convert an
/// absolute deadline in every session, and scale a relative one only when the session asked for the
/// duration axis.
pub const DEADLINE_CHANNELS: u64 = CH_SWT | CH_SWTEX | CH_TPTIMER | CH_TPTIMEREX | CH_NTDELAY;

/// Which system module exports a channel (the hook resolves it there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelModule {
    /// kernel32's entry, taken as it is. Since 2026-09-23 no channel in the table uses it: every time
    /// export kernel32 has turned out to be a stub, a forwarder, or one of two copies (the two variants
    /// at the end). It stays for a channel whose code really lives in kernel32 alone.
    Kernel32,
    Ntdll,
    /// user32.dll - the message waits. May be absent in a console/service target that never
    /// loads it - the hook then leaves the channel uninstalled (honest partial), never forces it.
    User32,
    /// winmm.dll - the multimedia timer timeSetEvent. Often absent (a console/service target rarely
    /// loads winmm) - resolved lazily like User32, honest partial if absent, never force-loaded.
    Winmm,
    /// ws2_32.dll - the socket wait `WSAWaitForMultipleEvents`. Often absent (a target that never
    /// touches the network never loads it) - resolved lazily like Winmm, honest partial if absent,
    /// never force-loaded. The connection observer used to live here too, on `connect`, and moved to
    /// ntdll because two of the three ways to connect never call that export (`ChannelDef::export`).
    Ws2_32,
    /// kernelbase.dll - the synchronisation waits that kernel32 does not export itself, and the three
    /// interrupt-time clocks, which kernel32 does not export at all (`QueryInterruptTime`,
    /// `QueryInterruptTimePrecise`, `QueryUnbiasedInterruptTimePrecise` - measured 2026-10-14 on both
    /// bitnesses, `GetProcAddress` on kernel32 answers null for all three).
    ///
    /// Not an optional module in the sense the three above are: every Win32 process has it, because
    /// kernel32 is built on it. It is a separate entry because of where the EXPORTS are, measured with
    /// `dumpbin` on this machine rather than taken from the documentation, which lists both DLLs for
    /// all three: kernel32 forwards `SleepConditionVariableSRW` and `SleepConditionVariableCS` to the
    /// api-set and does not export `WaitOnAddress` at all, while kernelbase exports all three for real.
    /// Hooking where the code actually lives also catches a caller that resolved the api-set directly.
    KernelBase,
    /// Exported by kernel32, with the code in kernelbase: the hook follows kernel32's entry to the
    /// function it lands on and detours THAT, so a caller that went to the api-set directly is caught too.
    ///
    /// Measured with `dumpbin` and a probe on both bitnesses: for the wall-clock and zone channels,
    /// kernel32 exports a stub of its own rather than a forwarder, and the stub is one indirect jump
    /// into kernelbase (on x86 behind the hot-patch prologue). A detour on that stub misses every
    /// caller that imports the api-set, and the dynamic C runtime is one of them: its `time()` and
    /// `localtime()` read the REAL date and zone under a session that reported `works`, while the
    /// audit showed the channel installed with zero calls.
    ///
    /// The decision is taken per process, at install, not in this table (`indirect_jump_slot`). When
    /// kernel32's entry does NOT lead to kernelbase's export (another Windows build, or a stub someone
    /// else already patched), the hook stays on kernel32 exactly as before and logs it, and so it does
    /// when the detour cannot be created in kernelbase. A change of shape can then cost the api-set
    /// callers, never the callers this build already covered.
    ///
    /// "Kernelbase's export" is whatever kernelbase resolves the name to, and for two duration channels
    /// that is ntdll: kernelbase forwards `QueryUnbiasedInterruptTime` and `QueryPerformanceCounter` to
    /// `RtlQueryUnbiasedInterruptTime` and `RtlQueryPerformanceCounter`, and kernel32's stub jumps
    /// straight there. The detour then lands in ntdll, which is where both paths meet.
    KernelBaseBehindKernel32,
    /// Code of its own in BOTH kernel32 and kernelbase, so one detour cannot see both paths: the hook
    /// detours kernel32's entry for its importers and kernelbase's copy for the api-set ones, and both
    /// count against the same channel.
    ///
    /// Measured on both bitnesses (2026-09-23): `GetTickCount64` has two separate bodies, and so has
    /// `GetTickCount` on 64-bit. The runtimes import these from kernel32, while `msvcrt`, `user32`,
    /// `combase`, `rpcrt4` and the network stacks import them through the api-set and read the real
    /// tick count under a session that scales the duration axis.
    ///
    /// No thread guard is needed against counting twice, because the detours are absolute: they
    /// compute the answer from the session's anchors and call the original only once the session is
    /// gone. 32-bit `GetTickCount` shows why that matters, since kernel32's copy is a `call` into
    /// kernelbase's, which `indirect_jump_slot` rightly refuses to treat as a jump. When kernel32's entry
    /// IS kernelbase's (a forwarder) or one jump away from it, the hook makes one detour there instead,
    /// because two detours on one address cannot exist.
    KernelBaseAndKernel32,
}

impl ChannelModule {
    /// Whether every Win32 process has this module loaded before any library of ours. For such a module
    /// a channel that is wanted and not installed can only mean the hook failed. For the other three it
    /// can also mean the application never loaded the module, and only the hook's own record
    /// (`Cov::failed_channels`) tells the two apart.
    pub const fn in_every_process(self) -> bool {
        match self {
            Self::Kernel32 | Self::Ntdll | Self::KernelBase | Self::KernelBaseBehindKernel32 | Self::KernelBaseAndKernel32 => {
                true
            }
            Self::User32 | Self::Winmm | Self::Ws2_32 => false,
        }
    }

    /// The DLL of a module that may arrive after `DllMain`, NUL-terminated for the loader, or `None` for
    /// one in every process (R5-A11). The hook's install and its late scan both look the module up by
    /// this name, so the three names stand in one place.
    pub const fn late_dll(self) -> Option<&'static [u8]> {
        match self {
            Self::User32 => Some(b"user32.dll\0"),
            Self::Winmm => Some(b"winmm.dll\0"),
            Self::Ws2_32 => Some(b"ws2_32.dll\0"),
            Self::Kernel32 | Self::Ntdll | Self::KernelBase | Self::KernelBaseBehindKernel32 | Self::KernelBaseAndKernel32 => {
                None
            }
        }
    }
}

/// Every channel whose module may arrive after `DllMain`: the set the hook's watcher looks for, read off
/// [`CHANNELS`] (R5-A11). It was written out by hand, module by module, beside the table and beside the
/// scan, so a channel added to one of those modules and not to the mask would never have been looked for.
pub const LATE_CHANNELS: u64 = {
    let mut mask = 0;
    let mut i = 0;
    while i < CHANNEL_COUNT {
        if !CHANNELS[i].module.in_every_process() {
            mask |= CHANNELS[i].bit;
        }
        i += 1;
    }
    mask
};

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
    /// Hooked and counted, but deliberately never injected into (ADR-3, observed): an
    /// NtCreateUserProcess that no CreateProcess detour started (a user token, or the call made
    /// straight - since R4/11 kernelbase's own CreateProcess path is inherited). Self-injecting there means
    /// manipulating undocumented native structures, a crash risk for near-zero real value (real QA
    /// targets spawn through CreateProcess*). So we count the direct call and warn that the child may
    /// be uncovered, an honest audit (rule 4) without the risk. NOT opt-in (unlike the time observers):
    /// process creation is watched regardless of scale_duration.
    SpawnObserved,
    /// Hooked and counted, but never modified: a network connection attempt. A target that opens a
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

impl ChannelCategory {
    /// Hooked and counted but never modified: the `observed` bucket of the audit, apart from the
    /// channels a session puts on its own clock. Exhaustive on purpose, so a new category has to say
    /// which side it is on - the mechanism sorts the report by it and the website counts by it.
    pub const fn observed(self) -> bool {
        match self {
            Self::WaitObserved | Self::TimerObserved | Self::SpawnObserved | Self::SourceObserved => true,
            Self::Wall | Self::Zone | Self::Duration | Self::Qpc => false,
        }
    }
}

/// One time channel: its coverage bit, its name, the module that exports it, and its category.
/// Single source of truth so the mechanism reports exactly the channels the hook installs.
#[derive(Debug, Clone, Copy)]
pub struct ChannelDef {
    pub bit: u64,
    /// The channel's name on the wire and in the report - a public contract key (untouchable
    /// rule 17). It is also the exported symbol the hook detours, with the one exception
    /// `export` names.
    pub name: &'static str,
    pub module: ChannelModule,
    pub category: ChannelCategory,
}

impl ChannelDef {
    /// The exported symbol the hook detours for this channel. The channel's own name for every
    /// channel but one.
    ///
    /// The exception is `connect`, which counts every network connection attempt at
    /// `ntdll!NtDeviceIoControlFile` - the one place all of them pass through. Measured on both
    /// bitnesses (2026-09-23): `connect` and `WSAConnect` reach the socket driver with control code
    /// 0x12007, while `ConnectEx` - and with it `WSAConnectByName`, `WSAConnectByList`, WinHTTP and
    /// WinINet - uses 0x120C7 and never calls the `connect` export. A detour on that export saw two
    /// of those eleven paths, and the .NET, Node.js and Go runtimes connect through the others. The
    /// name stays `connect` because it is a contract key, and because it still says what is counted.
    pub const fn export(&self) -> &'static str {
        if self.bit == CH_CONNECT { "NtDeviceIoControlFile" } else { self.name }
    }
}

/// The address of the pointer slot an indirect jump at a function's entry loads its target from, or
/// `None` when the entry is not one of the stub shapes kernel32 uses. The hook reads that slot to
/// learn whether kernel32's export leads into kernelbase (`ChannelModule::KernelBaseBehindKernel32`).
///
/// Only the shapes measured on kernel32 (both bitnesses, 2026-09-23) are recognised, and any other
/// entry answers `None`, which keeps the detour on kernel32 as it always was:
/// - 64-bit `FF 25 disp32` and `48 FF 25 disp32`: `jmp [rip + disp32]`, relative to the end of the
///   instruction.
/// - 32-bit `FF 25 abs32`: `jmp [abs32]`.
/// - 32-bit `8B FF 55 8B EC 5D FF 25 abs32`: the hot-patch prologue (`mov edi, edi`, `push ebp`,
///   `mov ebp, esp`, `pop ebp`), which undoes itself before the same jump.
///
/// `rip_relative` picks the encoding, because the same `FF 25` addresses relative to the next
/// instruction in 64-bit code and carries an absolute address in 32-bit code. Pure over the bytes it
/// is given, so the caller owns the one unsafe read and both encodings are testable on either host.
pub fn indirect_jump_slot(code: &[u8; 12], entry: usize, rip_relative: bool) -> Option<usize> {
    let disp = |at: usize| i32::from_le_bytes([code[at], code[at + 1], code[at + 2], code[at + 3]]);
    if rip_relative {
        let (len, at) = match code {
            [0x48, 0xFF, 0x25, ..] => (7, 3),
            [0xFF, 0x25, ..] => (6, 2),
            _ => return None,
        };
        return entry.checked_add(len)?.checked_add_signed(disp(at) as isize);
    }
    let at = match code {
        [0xFF, 0x25, ..] => 2,
        [0x8B, 0xFF, 0x55, 0x8B, 0xEC, 0x5D, 0xFF, 0x25, ..] => 8,
        _ => return None,
    };
    Some(disp(at) as u32 as usize)
}

// --- Coverage audit against chrono-mock.md 9.1 ---------------------------------
// COVERED (the CHANNELS table below): the Win32 wall clock (GetSystemTime,
// GetSystemTimeAsFileTime, GetSystemTimePreciseAsFileTime, GetLocalTime), the session
// zone (GetTimeZoneInformation, GetDynamicTimeZoneInformation, and GetTimeZoneInformationForYear
// asked about the current zone, which is how the Rust `chrono` crate reads it) plus the explicit zone
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
// The absolute conversion happens in EVERY session, the scaling of relative times only under
// scale_duration (`DEADLINE_CHANNELS`, R5-D2). SetTimer (user32, ADR-7 class C) joins them: its uElapse interval is scaled by M (scale_timer_elapse)
// so WM_TIMER arrives in step with the fake clock - Windows clamps a scaled interval below
// USER_TIMER_MINIMUM (10 ms) up to it, a documented floor under heavy acceleration. The thread-pool
// timers SetThreadpoolTimer / SetThreadpoolTimerEx (kernel32) scale the same way as SetWaitableTimer:
// their FILETIME due-time (absolute converted to a scaled relative interval, relative scaled) and
// their msPeriod / msWindowLength divide by M. CRT time / _time64 / localtime ride the hooked Win32
// exports - but only because the wall-clock and zone channels are detoured in KERNELBASE, where the
// code lives (`ChannelModule::KernelBaseBehindKernel32`). The dynamic C runtime imports them through
// the api-set, which skips kernel32 entirely. Before 2026-09-23 they were detoured on kernel32's stub,
// so "for free" held for the static runtime only, and a /MD program read the real date.
//
// OPT-IN UNDER ITS OWN FLAG (`scale_qpc`, the ADR-2 reversal): QueryPerformanceCounter. Left real by
// default - scaling it also scales a target's QPC-timed rendering - but a Python 3.13+ / .NET / Java
// elapsed clock stands on QPC and nothing else reaches it, so the session may ask. Kept in this table
// (R2-S3) because a channel outside it cannot be reported: the option was installed by hand and its
// failure lived in a debug string, so the one person who deliberately turned it on could not learn
// whether it took (rule 4). NtQueryPerformanceCounter stays out - ADR-2 unchanged for it.
//
// DELIBERATELY EXCLUDED, for two different reasons:
//   - ADR-2 (scaling them destabilizes the target): the native NtQueryPerformanceCounter.
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
// audio/MIDI timing - the winmm cost ADR-2 avoids - so it is hooked, counted, and left real, never
// scaled. The winmm CLOCK timeGetTime does NOT stay with it: since 2026-09-07 it is SCALED on the
// duration axis under `scale_duration`, a partial reversal of ADR-2 decided on measurement. It returns
// milliseconds since boot, which is exactly what GetTickCount returns and what that flag already scales,
// so leaving it real made one application see its own duration axis disagree with itself by the
// multiplier - measured at x60: GetTickCount64 +12017 ms and timeGetTime +200 ms across one 200 ms
// window. Seven runtimes were surveyed first (see CHANGELOG-DEV): it is never the dominant clock, but
// two game engines read it about 150 times a second, which is per-frame, and a clock a target reads
// every frame is one this tool covers. The winmm cost ADR-2 avoided is real but belongs mostly to
// timeSetEvent, the audio SCHEDULER, which stays observed and untouched - reading a clock schedules
// nothing. The residual risk is named by clock.timegettime_scaled_audio_may_shift. An NtCreateUserProcess (ntdll, ADR-3) that no CreateProcess
// detour started - a user token, or the call made straight - joins the observed bucket under inheritance.ntcreateuserprocess_child_maybe_uncovered:
// self-injecting there means manipulating undocumented native structures, a crash risk for near-zero
// value (real targets spawn through CreateProcess*, which we do inject), so we count the direct call
// and warn that its child may be uncovered, an honest audit (rule 4) without the risk. A guard makes
// the CreateProcess* funnel to NtCreateUserProcess NOT count (the child is already inherited).
//
// WHERE THE DURATION AXIS AND THE WAITS ARE DETOURED (2026-09-23, the second half of the move above):
// where their code lives, like the wall clock. Until then they stood on kernel32's stubs, and a caller
// that went through the api-set (msvcrt, ucrtbase, user32, combase, rpcrt4, the network stacks) read a
// real tick count, a real interrupt time and a real QPC, armed a real SetWaitableTimer, and waited on
// objects the audit never saw. Sleep and SleepEx were the exception that made the others look covered:
// they reached the hooked NtDelayExecution anyway, scaled but counted under that name. The thread-pool
// timers and SetWaitableTimerEx never had the gap, because kernel32 forwards them to ntdll and
// kernelbase and GetProcAddress already resolved the forwarder. The consequence to know about: the
// session's duration axis now reaches the system components inside the target too, so a component that
// computes a deadline from the tick count and then waits on an object with a real timeout can give up
// sooner in real time. The thread-pool timers inside those components were already scaled before this.
//
// THE INTERRUPT-TIME CLOCKS (R5-F4, 2026-10-14): QueryInterruptTime, QueryInterruptTimePrecise and
// QueryUnbiasedInterruptTimePrecise ride the duration axis under `scale_duration`, like QUIT. No language
// runtime on the measured machine calls them, but system components do, and so does the Chromium engine of
// WebView2 (QueryUnbiasedInterruptTimePrecise), which used to read one duration axis scaled and this one
// real inside the same process. The interrupt time counts sleep and the unbiased one does not, so the fake
// pair keeps exactly that difference: the fake QueryInterruptTime is the fake QUIT plus the real time the
// machine has slept since boot (`consistent_bias`), never the slept time multiplied (ADR-5).
//
// ON 32-BIT WINDOWS UNDER `scale_qpc` (R5-F5, measured 2026-10-14): ntdll's RtlGetSystemTimePrecise and
// RtlGetInterruptTimePrecise call the EXPORTED RtlQueryPerformanceCounter, which is where the QPC detour
// stands, so the precise system clocks ride the scaled counter. The hook raises its own-clock flag around
// every call of its own into them, so the QPC detour hands those back real and uncounted. What stays: an
// application on 32-bit that calls QueryUnbiasedInterruptTimePrecise or QueryInterruptTimePrecise itself in
// a session with `scale_qpc` and WITHOUT `scale_duration`, where these two are not hooked, reads a value
// that follows the scaled counter. No such caller was found besides one system component (R5 scan, 4.5).
//
// KNOWN GAPS, not yet covered (the verifier should report these honestly): LocalFileTimeToLocalSystemTime
// and LocalSystemTimeToLocalFileTime are not hooked at all. GetTimeZoneInformationForYear left this list
// in layout 13: the Rust `chrono` crate reads the current zone through it alone, so every program built on
// it showed this machine's zone under any session (measured 2026-10-27).
// Residual exotica (SetThreadpoolWait timeouts, RtlCreateUserProcess legacy path) are out of scope
// and would be reported honestly if a target hit them.

/// All time channels, ordered by their `calls` index (IDX_*): the wall-clock set, the
/// session-zone functions, then the opt-in duration axis.
pub const CHANNELS: [ChannelDef; CHANNEL_COUNT] = [
    ChannelDef { bit: CH_GSTAFT, name: "GetSystemTimeAsFileTime", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_GSTPAFT, name: "GetSystemTimePreciseAsFileTime", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_GST, name: "GetSystemTime", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_GLT, name: "GetLocalTime", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_NTQST, name: "NtQuerySystemTime", module: ChannelModule::Ntdll, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_GTZI, name: "GetTimeZoneInformation", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_GDTZI, name: "GetDynamicTimeZoneInformation", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_GTC64, name: "GetTickCount64", module: ChannelModule::KernelBaseAndKernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_QUIT, name: "QueryUnbiasedInterruptTime", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_GTC, name: "GetTickCount", module: ChannelModule::KernelBaseAndKernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_STSL, name: "SystemTimeToTzSpecificLocalTime", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_STSLEX, name: "SystemTimeToTzSpecificLocalTimeEx", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_FTLFT, name: "FileTimeToLocalFileTime", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_LFTFT, name: "LocalFileTimeToFileTime", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_TLTST, name: "TzSpecificLocalTimeToSystemTime", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_TLTSTEX, name: "TzSpecificLocalTimeToSystemTimeEx", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Zone },
    ChannelDef { bit: CH_SLEEP, name: "Sleep", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_SLEEPEX, name: "SleepEx", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_NTDELAY, name: "NtDelayExecution", module: ChannelModule::Ntdll, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_NTQSI, name: "NtQuerySystemInformation", module: ChannelModule::Ntdll, category: ChannelCategory::Wall },
    ChannelDef { bit: CH_WFSO, name: "WaitForSingleObject", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_WFSOEX, name: "WaitForSingleObjectEx", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_WFMO, name: "WaitForMultipleObjects", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_WFMOEX, name: "WaitForMultipleObjectsEx", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_SOAW, name: "SignalObjectAndWait", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_MWFMO, name: "MsgWaitForMultipleObjects", module: ChannelModule::User32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_MWFMOEX, name: "MsgWaitForMultipleObjectsEx", module: ChannelModule::User32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_SWT, name: "SetWaitableTimer", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_SWTEX, name: "SetWaitableTimerEx", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_SETTIMER, name: "SetTimer", module: ChannelModule::User32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_TIMESETEVENT, name: "timeSetEvent", module: ChannelModule::Winmm, category: ChannelCategory::TimerObserved },
    ChannelDef { bit: CH_TPTIMER, name: "SetThreadpoolTimer", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_TPTIMEREX, name: "SetThreadpoolTimerEx", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_NTCUP, name: "NtCreateUserProcess", module: ChannelModule::Ntdll, category: ChannelCategory::SpawnObserved },
    ChannelDef { bit: CH_CONNECT, name: "connect", module: ChannelModule::Ntdll, category: ChannelCategory::SourceObserved },
    ChannelDef { bit: CH_QPC, name: "QueryPerformanceCounter", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Qpc },
    ChannelDef { bit: CH_TIMEGETTIME, name: "timeGetTime", module: ChannelModule::Winmm, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_SCVSRW, name: "SleepConditionVariableSRW", module: ChannelModule::KernelBase, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_SCVCS, name: "SleepConditionVariableCS", module: ChannelModule::KernelBase, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_WOA, name: "WaitOnAddress", module: ChannelModule::KernelBase, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_WSAWFME, name: "WSAWaitForMultipleEvents", module: ChannelModule::Ws2_32, category: ChannelCategory::WaitObserved },
    ChannelDef { bit: CH_QIT, name: "QueryInterruptTime", module: ChannelModule::KernelBase, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_QITP, name: "QueryInterruptTimePrecise", module: ChannelModule::KernelBase, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_QUITP, name: "QueryUnbiasedInterruptTimePrecise", module: ChannelModule::KernelBase, category: ChannelCategory::Duration },
    ChannelDef { bit: CH_GTZIFY, name: "GetTimeZoneInformationForYear", module: ChannelModule::KernelBaseBehindKernel32, category: ChannelCategory::Zone },
];

/// Marks the control block as one WE built. The section name is fixed and lives in a namespace any
/// process in this session can create in, so `CreateFileMapping` returning `ERROR_ALREADY_EXISTS`
/// used to mean "our orphan" by assumption - the block was zeroed and used whatever it really was.
/// A fresh section is zero-filled by the OS (documented), so anything that merely squatted the name
/// fails this check, and so does a block from a build whose layout differs.
pub const CTL_MAGIC: u64 = 0x4348_524F_4E4F_4354; // "CHRONOCT"

/// Layout version of [`Ctl`]. Bumped whenever fields move, so a hook from one build refuses a block
/// written by another instead of reading the wrong offsets as a clock (untouchable rules 2 and 4).
/// This is an internal seam - mechanism and hook ship in the same package - not a wire contract.
///
/// 2: `CHANNEL_COUNT` grew for the `timeGetTime` channel, which widens `Cov` and so moves every
/// `covs` slot after the first. The dangerous direction is a NEW hook against an OLD core: the wider
/// stride would put a write past the end of the smaller mapping the core created. Bumping is what
/// turns that into a refusal.
///
/// 3: `Cov` gained `late_installed`, which widens it by 8 bytes and moves every slot after the first
/// for the same reason.
///
/// 4: `CHANNEL_COUNT` grew from 37 to 41 for the four waits that were neither scaled nor counted
/// (the condition-variable pair, `WaitOnAddress`, `WSAWaitForMultipleEvents`), which widens `calls`
/// and so moves the slots again.
///
/// 5: `Cov` gained `waits_at_floor`, which widens it by 8 bytes and moves every slot after the first,
/// for the same reason version 3 did.
///
/// 6: `Cov` gained the `uncovered_children` ring and its counter (`UNCOVERED_CHILDREN_MAX` pids plus
/// two u32), appended AFTER `calls` so no existing offset inside a slot moves - but the slot stride
/// does, which is the same hazard as version 3.
///
/// 7: the block says WHICH session it belongs to and WHETHER it is over (R4-W2). `Ctl` gained
/// `core_created` (inserted before `scale_dur`, so every field after it moves) and `ended` (in the
/// old padding), and `Cov` gained `created`, which widens every slot. A pid alone could not tell a
/// live session from one whose core had died, nor a recycled pid from the core that wrote it.
///
/// 8: `Cov` gained `failed_channels`, which widens every slot for the same reason version 3 did. The
/// hook now says which channels it could not switch on, so a failed detour in an optional module is
/// reported instead of reading as a module the application never loaded.
///
/// 9: the millisecond tick base became an offset from the fake QUIT (`dur_tick_offset`, R4/10a). Same
/// offset and size, a different meaning: a hook of the old build would read the offset as a base and
/// hand the application a tick count near zero.
///
/// 10: `Cov` gained `timers_at_floor` (R5, D8), appended after every other field like `failed_channels`,
/// which widens every slot for the same reason version 3 did.
///
/// 11: `CHANNEL_COUNT` grew from 41 to 44 for the three interrupt-time clocks (R5-F4), which widens
/// `calls`, and `Cov` gained `absolute_dues` (R5-D2) after every other field. Both widen every slot.
///
/// 12: `MAX_COV_PIDS` grew from 256 to 4096 (R5-P26). No offset inside the header or a slot moves, but
/// `covs` starts later and the block is sixteen times the size, so an old hook would read a new block's
/// slots at the wrong place and map too little of it.
///
/// 13: `CHANNEL_COUNT` grew from 44 to 45 for `GetTimeZoneInformationForYear`, which widens `calls`, and
/// the slot's padding became `subsystem`. The first moves every slot after the first, and the second gives
/// four bytes an old hook leaves at zero a meaning a new core reads.
///
/// 14: `Cov` gained `calls_without_storage` (2026-10-28) after every other field, which widens every slot.
///
/// 15: `Cov` gained `exit_began_qpc` (ADR-4, 2026-11-12) after every other field, which widens every
/// slot, and `Ctl` gained `pending_loads` after `covs` (ADR-25), which makes the block longer. An old hook
/// would leave the mark unwritten and never pass the gate a new parent opens for it.
///
/// 16: `Ctl` gained `zone_name` (ADR-26, 2026-11-12) in the four bytes after `tz_bias` that alignment had
/// left unnamed. No offset moves, but an old hook would hand the zone out under its own name where a new
/// core chose a borrowed one, and say nothing of it.
///
/// 17: `Cov` gained `zone_not_read_again` (ADR-27, 2026-11-12) after every other field, which widens every
/// slot. An old hook would leave it at zero where it could not have the C runtimes read the zone again.
pub const CTL_LAYOUT_VERSION: u32 = 17;

/// How many uncovered children one process's `Cov` can name. Past this the counter still grows, so
/// the audit says "and N more" rather than losing the number. Sized for what a Chromium browser
/// process spawns in a session (a handful of renderer, GPU and utility processes, more under site
/// isolation) at 4 bytes a pid - 128 bytes per slot, 512 KiB across the registry.
pub const UNCOVERED_CHILDREN_MAX: usize = 32;

/// Session-wide control block in `Local\ChronoCtl`. `#[repr(C)]` so both processes
/// agree on the layout. Coverage lives here too, one `Cov` per registry slot, so a
/// process's evidence outlives the process (S-9) while staying attributed to it alone.
#[repr(C)]
pub struct Ctl {
    /// [`CTL_MAGIC`] once the mechanism has claimed this section. FIRST field on purpose: it is the
    /// one thing a reader may look at before it trusts any offset below it.
    pub magic: u64,
    /// [`CTL_LAYOUT_VERSION`] of the build that wrote this block.
    pub layout: u32,
    /// Seqlock counter for the anchor fields (odd = write in progress).
    pub seq: u32,
    /// Session zone bias in minutes (UTC = local + bias), no DST. Stable per session.
    pub tz_bias: i32,
    /// Which key name the session zone is handed out under: [`zone_name::OWN_ZONE_NAME`], or a name
    /// borrowed from a real zone at the same offset ([`zone_name::BORROWED_NAMES`], ADR-26). Stable per
    /// session. In the four bytes alignment leaves before `a_fake`, so naming them moved no offset.
    pub zone_name: u32,
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
    /// duration axis).
    ///
    /// The fake `GetTickCount64` is the fake QUIT in milliseconds plus this offset (`tick_from_quit`),
    /// set once when the session starts and never rebased (R4/10a, F4). The tick count used to have a
    /// base of its own, rebased with every rate change, and each rebase rounded the elapsed time down to
    /// a whole millisecond: the count fell behind QUIT a little at every change, and a reader that came
    /// in late saw it step back. QUIT rebases exactly (whole 100 ns units), so the tick count that
    /// follows it neither drifts nor rounds. Also feeds `GetTickCount` and `timeGetTime`.
    pub dur_tick_offset: i64,
    /// Fake `QueryUnbiasedInterruptTime` base, in 100 ns units. The one duration axis that is rebased,
    /// and the tick count is derived from it.
    pub dur_quit_c0: i64,
    /// Real QUIT base (100 ns) the duration elapsed is measured from. Distinct from `a_real`: a `jump`
    /// re-anchors `a_real` but must leave `dur_q0` (and so the whole duration axis) alone.
    pub dur_q0: i64,
    /// QPC axis anchor (opt-in `scale_qpc`, ADR-2 reversal). QPC is a SEPARATE system counter from QUIT
    /// (different ticks and epoch), so it needs its own base rather than riding `dur_q0`. Fake QPC base,
    /// in raw QPC ticks. Rebased on every `set_multiplier` like the tick axis (freeze then re-anchor), so
    /// a speed change never rewinds it (untouchable rule 3) - left untouched on `jump`.
    pub dur_qpc_c0: i64,
    /// Real QPC base (raw ticks) the QPC elapsed is measured from - QPC is a system-wide counter, so the
    /// core reads the same value the target's hook does.
    pub dur_qpc_q0: i64,
    /// When the core process was created, as the FILETIME `GetProcessTimes` reports for it. Together
    /// with `core_pid` it names the session: a pid alone is recycled once its process is gone, so a
    /// hook that found a live process under the recorded pid could be looking at a stranger - or at a
    /// NEW core that reclaimed this block under the same number (R4-W2). The system time at creation
    /// cannot be shared by two processes that held one pid, and changing the system clock later does
    /// not change it. Written once, BEFORE `core_pid`, so a reader that sees a pid also sees its time.
    pub core_created: u64,
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
    /// increases. The mechanism does not read it - it scans `pids` for nonzero entries.
    pub pid_count: u32,
    /// 1 once the core has ended this session in order (R4-W2). The block outlives the core for as
    /// long as any process of the session keeps it mapped, and before this flag nothing in it said the
    /// session was over: a child started later by such a process joined a clock nobody drove, and a new
    /// core reclaiming the block could not tell a session that ended from one whose core had died.
    ///
    /// A flag of its own rather than a zeroed `core_pid`: the watcher lets a process go only while the
    /// block still names its core (`still_ours` in the hook), so clearing the pid would send every
    /// process of the session back to the REAL value of its duration axes - a step back (ADR-14,
    /// untouchable rule 3). A core that is killed never writes it, which the pid and `core_created`
    /// cover: a process that can no longer be opened, or one created at another time, is not the core.
    pub ended: u32,
    /// Keeps `pids` ending on an 8-byte boundary, where `covs` starts. Explicit for the reason the one in
    /// `Cov` is: `#[repr(C)]` would add it silently and the layout doc would lie.
    pub _pad: u32,
    /// Registered PIDs (parent + children), indexed by reserved slot. A hook publishes its own PID
    /// here LAST, once its coverage slot carries the truth, so the mechanism never reads a pid whose
    /// slot is not yet filled in. A zero means "empty, or reserved but not yet published".
    pub pids: [u32; MAX_COV_PIDS],
    /// Per-process coverage, parallel to `pids`: slot `i` belongs to the process that published
    /// `pids[i]`. Never summed across slots - that is the whole point of the split (rule 4).
    pub covs: [Cov; MAX_COV_PIDS],
    /// The loads of the hook a parent has started in its children and not yet seen end (ADR-25), each
    /// a gate the child's hook has to pass before it installs anything. A parent that stops waiting for
    /// a load lets the child's program run, and counts the child as running on the real clock - so a load
    /// that finishes later must not hook the program halfway through its run. The parent and the child's
    /// hook each try to move the entry on from pending, atomically, and only one of them can: to
    /// "joined", and the parent waits for the install to finish, or to "abandoned", and the hook refuses
    /// ([`open_load_gate`], [`pass_load_gate`], [`abandon_load_gate`]).
    pub pending_loads: [PendingLoad; PENDING_LOADS_MAX],
}

/// How many loads of the hook into children may be pending at once in one session. A load is pending
/// for the tens of milliseconds it takes, and an abandoned one until its child's hook comes to refuse, so
/// this is far past what an application starts at once. With every entry taken, a parent loads without a
/// gate and says so in its log: a load it stops waiting for can then still join late, as one did before
/// the gate existed.
pub const PENDING_LOADS_MAX: usize = 64;

/// The states of a pending load. Free and claimed belong to the parent alone, the rest are what the
/// child's hook reads.
const LOAD_FREE: u32 = 0;
const LOAD_CLAIMED: u32 = 1;
const LOAD_PENDING: u32 = 2;
const LOAD_JOINED: u32 = 3;
const LOAD_ABANDONED: u32 = 4;

/// One load of the hook into a child, named by the child's pid and creation time: the pid alone comes
/// back with another process once the child is gone. `#[repr(C)]`, 16 bytes, so the table keeps the
/// 8-byte alignment of the block.
#[repr(C)]
pub struct PendingLoad {
    /// `LOAD_*`, moved on only with an atomic compare-and-swap.
    pub state: u32,
    pub pid: u32,
    /// The FILETIME `GetProcessTimes` reports for the child.
    pub created: u64,
}

/// One process's coverage, living in the `Ctl` slot that process reserved. `#[repr(C)]` - written
/// only by the owning process's hook, read by the mechanism. One slot per process, so each
/// process's evidence is attributed to it and never summed with the rest of the tree.
#[repr(C)]
pub struct Cov {
    /// Bitmask of channels this process's hook installed. u64 (63 usable bits) so the channel set can
    /// grow past 32 without a layout break - the low bits still hold channels 0..N as before. Being u64
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
    /// Channels this process hooked only AFTER `DllMain`, once their module finally showed up.
    ///
    /// Always a subset of `installed_channels` - a late channel IS covered, and the report lists it
    /// like any other. This says something the coverage mask cannot: the channel was not live for the
    /// whole session. Two consequences follow and the audit has to own both (untouchable rule 4).
    /// Calls the target made before the hook landed were never counted, so the counter is a floor
    /// rather than a total. And a SCALED channel rejoins the shared duration base at that moment,
    /// which is a one-off jump - measured at +32 575 ms under x60, because the base had been drifting
    /// away from the real clock the whole time the channel ran real.
    ///
    /// Reported as one warning rather than two, because it is one event with two effects.
    pub late_installed: u64,
    /// Waits this process made that were held at the scaling floor ([`MIN_SCALED_WAIT_MS`]) instead of
    /// being divided by the full multiplier.
    ///
    /// A count rather than a flag, because the two readings differ: one such wait in a session is a
    /// rounding detail, while a hot polling loop produces thousands and means a real part of this
    /// application is not accelerating. The report says the number.
    ///
    /// It is a volatile RMW like `calls`, so concurrent threads can lose a bump - which under-counts
    /// and never invents one, the same direction of error the call counters already accept.
    pub waits_at_floor: u64,
    /// Per-channel call counters for this process, indexed by IDX_*.
    pub calls: [u64; CHANNEL_COUNT],
    /// The pids of children THIS process spawned that the hook did not follow into - named, not just
    /// counted. Two ways in, both certain: a direct `NtCreateUserProcess` (the CreateProcess* detours
    /// never saw it, so nothing injected - the path a Chromium sandbox takes for every renderer), and
    /// a CreateProcess* child whose `inject_self` failed (`uninjected_children` counts those too).
    ///
    /// Why a list and not the counter that already existed: a count says "one child ran on the real
    /// clock" and nothing about WHICH - and for an app that embeds a web engine, which one is the whole
    /// finding (the renderer is the process the page's `Date.now()` lives in). The mechanism resolves
    /// each pid to an image name while the child is still alive, and folds the fact into the family
    /// verdict, where a process nobody reached is exactly what keeps a family from `works`
    /// (untouchable rule 4 at the session level).
    ///
    /// Written in the SPAWNING parent's slot for the same reason as `uninjected_children`: the child
    /// has no slot, and this is the process the fact belongs to. Entries are claimed with an atomic
    /// index (`uncovered_children_count`), because two threads spawning at once with a plain RMW
    /// would overwrite each other's pid - a lost COUNT under-reports, a lost PID drops a process.
    pub uncovered_children: [u32; UNCOVERED_CHILDREN_MAX],
    /// How many uncovered children this process spawned in total - the ring above holds the first
    /// `UNCOVERED_CHILDREN_MAX` of them. Also the atomic index that claims a ring entry.
    pub uncovered_children_count: u32,
    /// The Windows subsystem the process that took this slot was built for, as its own image header says
    /// it (`IMAGE_SUBSYSTEM_*`: [`SUBSYSTEM_WINDOWS`] for a windowed program, [`SUBSYSTEM_CONSOLE`] for a
    /// console one), written by its hook before it publishes its pid. 0 = not recorded.
    ///
    /// Why the core has to know (ADR-4, owner decision 2026-10-27): a family that ended inside the guard
    /// window used to read as a single-instance hand-off every time, so a console tool that simply did its
    /// work in a few milliseconds was reported as a session that never took effect. A hand-off is what a
    /// windowed application's second instance does, so the suspicion now needs one windowed process in
    /// the family, or one whose kind is unknown. It sits where the slot's padding was, which keeps the
    /// slot 8-byte aligned as before.
    pub subsystem: u32,
    /// When the process that took this slot was created (the FILETIME `GetProcessTimes` reports), written
    /// by its hook before it publishes its pid. The mechanism watches the family by pid, and a pid read
    /// out of this block names a process only for as long as that process lives: comparing this with
    /// the creation time of the process it opened is what tells the member from a stranger that got the
    /// number (R4-N12). 0 = not recorded, and the mechanism then falls back to asking the process
    /// snapshot who started it. Appended after every other field, so no offset inside a slot moves.
    pub created: u64,
    /// Channels whose module was loaded and whose export was found, but whose detour could not be made
    /// or switched on - at startup or in the late scan. The mechanism asks it only about a channel that
    /// is not in `installed_channels`, because a detour that failed at startup gets one more try when
    /// the watcher looks for late modules, and one that succeeded there is covered.
    ///
    /// Why the hook has to say it: for a channel in a module that is in every process, "wanted and not
    /// installed" can only mean the hook failed, and the report says so. For the three optional modules
    /// (`user32`, `winmm`, `ws2_32`) the same silence also means "the application never loaded it", which
    /// is no gap at all - so a failed detour there used to leave its channel out of the report, and an
    /// application could read the real `timeGetTime` beside a scaled tick count while the report looked
    /// complete (untouchable rule 4). Appended after every other field, so no offset inside a slot moves.
    pub failed_channels: u64,
    /// Timers this process set whose scaled interval could not be divided by the full multiplier and
    /// ran at a floor instead (R5, D8): a `SetTimer` interval Windows lifted to `USER_TIMER_MINIMUM`
    /// ([`timer_elapse_hit_floor`]), or a waitable or thread-pool timer period held at 1 ms
    /// ([`timer_period_hit_floor`]). The timer twin of `waits_at_floor`, kept apart because it names
    /// a different part of the application and a different floor, and counted the same way: a
    /// volatile RMW that can lose a bump and never invents one. Appended after every other field, so
    /// no offset inside a slot moves.
    pub timers_at_floor: u64,
    /// Absolute deadlines this process set - a timer or a `NtDelayExecution` aimed at a wall-clock
    /// instant - that the hook turned into a real delay (R5-D2). The conversion uses the speed of the
    /// moment it is made, so a deadline still pending when the session jumps or changes speed fires where
    /// the old clock would have reached it, while Windows moves its own absolute timers with the clock.
    /// The core asks this count at every jump and speed change and warns when one came before it
    /// (`timer.absolute_due_converted`). Counted like `timers_at_floor`, and appended after every other
    /// field, so no offset inside a slot moves.
    pub absolute_dues: u64,
    /// Hooked waits, object waits and timers this process made on a thread without thread-local storage
    /// (2026-10-28): one Windows is ending, or one started without it. The hook keeps its cascade flags
    /// there, so on such a thread it cannot tell an application's call from a cascade and passes every
    /// one through unscaled - every level of a cascade is counted, so this is a count of hooked calls,
    /// not of application calls. The core warns when it is not zero (`wait.thread_without_storage`).
    /// Counted like `timers_at_floor`, and appended after every other field, so no offset inside a slot
    /// moves.
    pub calls_without_storage: u64,
    /// The real `QueryPerformanceCounter` value at which this process began to exit, written by its hook
    /// when the loader tells it the process is ending, 0 while it runs (ADR-4, 2026-11-12). The core
    /// reads it once, for the process it launched, when that process was not gone by the end of the guard
    /// window: a display driver's process-exit callback can hold an exiting process in the kernel for
    /// seconds (measured up to 2.6 s), and a program that ended its own work inside the window then looked
    /// like one that ran on. A mark inside the window, followed by the real end, is an exit inside it
    /// ([`exit_began_in_window`]). The hook writes it with one atomic store and nothing else, because it
    /// runs under the loader lock. Appended after every other field, so no offset inside a slot moves.
    pub exit_began_qpc: i64,
    /// 1 when the hook could not have this process's shared C runtimes read the zone again at its start
    /// (ADR-27): the main thread could not be found or opened, or the call could not be queued on it. A
    /// library that asked for the time while the process started then leaves the program with this
    /// machine's zone. Written once by `install`, before the pid is published, and the core warns
    /// (`session.c_runtime_zone_kept`). Appended after every other field, so no offset inside a slot moves.
    pub zone_not_read_again: u64,
}

impl Cov {
    /// An empty slot, the state a fresh section starts every slot in. A `const` rather than `Default`,
    /// and no `Copy`: a slot is a type that must never be copied around by accident.
    pub const ZEROED: Cov = Cov {
        installed_channels: 0,
        uninjected_children: 0,
        late_installed: 0,
        waits_at_floor: 0,
        calls: [0; CHANNEL_COUNT],
        uncovered_children: [0; UNCOVERED_CHILDREN_MAX],
        uncovered_children_count: 0,
        subsystem: 0,
        created: 0,
        failed_channels: 0,
        timers_at_floor: 0,
        absolute_dues: 0,
        calls_without_storage: 0,
        exit_began_qpc: 0,
        zone_not_read_again: 0,
    };
}

/// Stamp the block as ours, at the layout this build speaks.
///
/// Written IMMEDIATELY after the section is created or reclaimed, before the anchor. That ordering
/// is what makes the check below safe against a false alarm: a section only outlives its core
/// because the TARGET still holds it mapped, and the target is created long after this point - so a
/// core that died before stamping left nothing behind to be found.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_header(p: *mut Ctl) { unsafe {
    write_volatile(addr_of_mut!((*p).magic), CTL_MAGIC);
    write_volatile(addr_of_mut!((*p).layout), CTL_LAYOUT_VERSION);
}}

/// Whether this block was written by a build that speaks our layout.
///
/// Read before ANY other field, by both sides: the mechanism before it reclaims a surviving section
/// as its own orphan, and the hook before it takes a single number out of it as a clock. Reading an
/// anchor out of a block somebody else shaped is how a target ends up on a time nobody in this tool
/// chose, with an audit that calls it covered (untouchable rules 2 and 4).
///
/// # Safety
/// `p` must point to a mapped view of at least `ctl_size()` bytes, correctly aligned.
pub unsafe fn header_is_ours(p: *const Ctl) -> bool { unsafe {
    read_volatile(addr_of!((*p).magic)) == CTL_MAGIC
        && read_volatile(addr_of!((*p).layout)) == CTL_LAYOUT_VERSION
}}

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
pub unsafe fn write_anchor(p: *mut Ctl, a_fake: i64, a_real: i64, multiplier: i64) { unsafe {
    let sp = addr_of_mut!((*p).seq);
    let s = read_volatile(sp).wrapping_add(1);
    write_volatile(sp, s); // odd - write in progress
    // Release fence: the odd-seq store is ordered before the data writes, and (below) the data
    // writes complete before the even-seq store. Paired with the reader's Acquire fences this is a
    // true cross-thread/-process seqlock, not merely a compiler barrier. On x86/x64 (TSO) a Release
    // or Acquire fence emits no instruction, so the hot anchor-read path keeps its cost - on a weakly
    // ordered ISA (ARM64) it emits the barrier that stops the anchor read from tearing.
    fence(Ordering::Release);
    write_volatile(addr_of_mut!((*p).a_fake), a_fake);
    write_volatile(addr_of_mut!((*p).a_real), a_real);
    write_volatile(addr_of_mut!((*p).multiplier), multiplier);
    fence(Ordering::Release);
    write_volatile(sp, s.wrapping_add(1)); // even - write done
}}

/// One field of the full anchor, as `write_anchor_full` stores it.
#[derive(Clone, Copy, Debug)]
enum AnchorStore {
    AFake(i64),
    AReal(i64),
    DurTickOffset(i64),
    DurQuitC0(i64),
    DurQpcC0(i64),
    DurQ0(i64),
    DurQpcQ0(i64),
    Multiplier(i64),
}

/// The full anchor's stores, in the order they reach the block (R4-N5).
///
/// The seqlock hides a write in progress from every reader but one: a core killed in the middle of the
/// write leaves `seq` odd for good, the readers fall back to the fields as they stand (`read_dur`), and
/// the watcher freezes the axes from them for the rest of the application's life (`release_axes`). So
/// every prefix of this order has to be a clock that does not run backwards. The bases go first and the
/// rate last: a new base with the old rate is a step forward, a new base and origin with the old rate is
/// continuous (the base was frozen at the old rate), and only the last store changes the rate. Each
/// base before its origin, because an origin moved to now under an old base is behind where the axis
/// stood. The rate went first until R4/7, which projected the OLD bases at the NEW rate - after a
/// slowdown, behind where the axis stood, and released there for good. The tick offset never changes
/// after the session starts (`rebase` writes back what stands there), so its place is free.
fn full_anchor_stores(a: &FullAnchor) -> [AnchorStore; 8] {
    [
        AnchorStore::AFake(a.a_fake),
        AnchorStore::AReal(a.a_real),
        AnchorStore::DurTickOffset(a.dur_tick_offset),
        AnchorStore::DurQuitC0(a.dur_quit_c0),
        AnchorStore::DurQpcC0(a.dur_qpc_c0),
        AnchorStore::DurQ0(a.dur_q0),
        AnchorStore::DurQpcQ0(a.dur_qpc_q0),
        AnchorStore::Multiplier(a.multiplier),
    ]
}

/// The eight fields `write_anchor_full` stores, named, so the order above is a list of names and not
/// a list of positions. The mechanism builds one for the anchor a session starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FullAnchor {
    /// Fake wall anchor, 100 ns FILETIME units.
    pub a_fake: i64,
    /// Real QUIT the wall anchor is measured from.
    pub a_real: i64,
    /// The rate.
    pub multiplier: i64,
    /// Fake `GetTickCount64` minus the fake QUIT in milliseconds (see `Ctl::dur_tick_offset`).
    pub dur_tick_offset: i64,
    /// Fake QUIT base.
    pub dur_quit_c0: i64,
    /// Real QUIT the duration axes are measured from.
    pub dur_q0: i64,
    /// Fake QPC base.
    pub dur_qpc_c0: i64,
    /// Real QPC the QPC axis is measured from.
    pub dur_qpc_q0: i64,
}

/// Store one field of the full anchor.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
unsafe fn store_anchor_field(p: *mut Ctl, store: AnchorStore) { unsafe {
    match store {
        AnchorStore::AFake(v) => write_volatile(addr_of_mut!((*p).a_fake), v),
        AnchorStore::AReal(v) => write_volatile(addr_of_mut!((*p).a_real), v),
        AnchorStore::DurTickOffset(v) => write_volatile(addr_of_mut!((*p).dur_tick_offset), v),
        AnchorStore::DurQuitC0(v) => write_volatile(addr_of_mut!((*p).dur_quit_c0), v),
        AnchorStore::DurQpcC0(v) => write_volatile(addr_of_mut!((*p).dur_qpc_c0), v),
        AnchorStore::DurQ0(v) => write_volatile(addr_of_mut!((*p).dur_q0), v),
        AnchorStore::DurQpcQ0(v) => write_volatile(addr_of_mut!((*p).dur_qpc_q0), v),
        AnchorStore::Multiplier(v) => write_volatile(addr_of_mut!((*p).multiplier), v),
    }
}}

/// Write the FULL anchor (wall triple plus the duration anchor) under the seqlock, in one transaction
/// so a reader never sees a new multiplier against an old duration base. This is the anchor a session
/// starts from, written by `prepare` before the target exists - a rate change goes through `rebase`,
/// and `jump` uses `write_anchor` to leave the duration axis alone. The order of the stores is
/// `full_anchor_stores`, and it matters only to a writer killed mid-write.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_anchor_full(p: *mut Ctl, anchor: &FullAnchor) { unsafe {
    let sp = addr_of_mut!((*p).seq);
    let s = read_volatile(sp).wrapping_add(1);
    write_volatile(sp, s); // odd - write in progress
    fence(Ordering::Release);
    for store in full_anchor_stores(anchor) {
        store_anchor_field(p, store);
    }
    fence(Ordering::Release);
    write_volatile(sp, s.wrapping_add(1)); // even - write done
}}

/// What a rate change did: the real QUIT it was made at, and the rate it replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rebased {
    /// The real QUIT of the change, read while the write was open.
    pub now: i64,
    /// The rate before the change.
    pub old_m: i64,
}

/// Change the rate, keeping every axis continuous (H-1, untouchable rule 3): freeze each one where it
/// stands under the OLD rate and carry on from there at the new one, in one seqlock transaction.
///
/// The instant of the change is read INSIDE the write, after the odd `seq` is visible (R4-S3, F3). Read
/// before it, as `set_multiplier` did until R4/10a, it left a window: a reader that took the old anchor
/// and then its own clock later than that instant answered from the old rate for a moment the new
/// anchor also covers - after a slowdown, above where the axis carries on, so the next read stepped
/// back (measured: `tools/probes/r4-7/f3.ps1`, 1 406 ms on the tick count). Read inside, the instant
/// comes after every clock a reader of the old anchor took (that reader saw `seq` unchanged after its
/// clock) and before every clock a reader of the new anchor takes (that reader saw the even `seq` this
/// write ends with). So every value from the old anchor is at most the new base, and every value from
/// the new one at least that - across threads and processes alike.
///
/// The full fence is what puts the odd `seq` before the clocks: a store may wait in the store buffer
/// while a later load runs, and a clock read IS a load (QUIT) or a time-stamp read (QPC, `clock_fence`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and the caller must be its only writer.
pub unsafe fn rebase(p: *mut Ctl, multiplier: i64, quit: impl Fn() -> i64, qpc: impl Fn() -> i64) -> Rebased { unsafe {
    let sp = addr_of_mut!((*p).seq);
    let s = read_volatile(sp).wrapping_add(1);
    write_volatile(sp, s); // odd - write in progress
    fence(Ordering::SeqCst);
    clock_fence();
    let now = quit();
    let now_qpc = qpc();
    // The writer is the only one, so the fields stand still while it reads them.
    let old = full_anchor_fields(p);
    let anchor = FullAnchor {
        a_fake: fake_wall_at(old.a_fake, old.a_real, now, old.multiplier),
        a_real: now,
        multiplier,
        dur_tick_offset: old.dur_tick_offset,
        dur_quit_c0: dur_quit_at(old.dur_quit_c0, old.dur_q0, old.multiplier, now),
        dur_q0: now,
        dur_qpc_c0: dur_qpc_at(old.dur_qpc_c0, old.dur_qpc_q0, old.multiplier, now_qpc),
        dur_qpc_q0: now_qpc,
    };
    for store in full_anchor_stores(&anchor) {
        store_anchor_field(p, store);
    }
    fence(Ordering::Release);
    write_volatile(sp, s.wrapping_add(1)); // even - write done
    Rebased { now, old_m: old.multiplier }
}}

/// The eight anchor fields as they stand, read without the seqlock.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and nothing may be writing it.
unsafe fn full_anchor_fields(p: *const Ctl) -> FullAnchor { unsafe {
    FullAnchor {
        a_fake: read_volatile(addr_of!((*p).a_fake)),
        a_real: read_volatile(addr_of!((*p).a_real)),
        multiplier: read_volatile(addr_of!((*p).multiplier)),
        dur_tick_offset: read_volatile(addr_of!((*p).dur_tick_offset)),
        dur_quit_c0: read_volatile(addr_of!((*p).dur_quit_c0)),
        dur_q0: read_volatile(addr_of!((*p).dur_q0)),
        dur_qpc_c0: read_volatile(addr_of!((*p).dur_qpc_c0)),
        dur_qpc_q0: read_volatile(addr_of!((*p).dur_qpc_q0)),
    }
}}

/// Hold every later instruction until everything before it has finished (`LFENCE`). A clock that reads
/// the processor's time-stamp counter, which `QueryPerformanceCounter` may, is not ordered with the loads
/// around it: Intel's manual says neither that it waits for earlier instructions nor that later ones
/// wait for it. Called before such a clock where its reading has to come after something, and after it
/// where its reading has to come before something - a seqlock reader's second look at `seq`.
#[inline(always)]
pub fn clock_fence() {
    // SAFETY: LFENCE needs SSE2, which every x86-64 processor has.
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_mm_lfence()
    };
    // SAFETY: SSE2 is in the baseline of the 32-bit Windows target this library builds for.
    #[cfg(target_arch = "x86")]
    unsafe {
        core::arch::x86::_mm_lfence()
    };
}

/// Whether an anchor write is in progress - `seq` is odd. Read once, after a seqlock reader has given
/// up, it means the writer stopped in the middle for good and the fields are a mix of old and new.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn anchor_write_in_progress(p: *const Ctl) -> bool { unsafe {
    read_volatile(addr_of!((*p).seq)) & 1 == 1
}}

/// How many times a seqlock reader retries before it gives up and returns a fallback (RELEASE-009). A write
/// is a handful of instructions held for nanoseconds and happens only on `set_multiplier`/`jump` (rare,
/// user-driven), so a live writer settles in a few spins - this bound is astronomically above that. It
/// exists only to cap the pathological case where the core is force-killed between the odd and even `seq`
/// writes, which would otherwise leave a reader spinning at 100% CPU forever.
///
/// 🔴 Not shorter (R5-WY12, R5-D36). The bound also has to outlast a LIVE writer that the scheduler took
/// off the processor in the middle of its write, which is a window of nanoseconds but a pause of a whole
/// scheduler quantum, tens of milliseconds. A reader that gave up sooner would answer from the half-written
/// fields - a new base at the old rate (`full_anchor_stores`) - and once the write finished, after a
/// slowdown, the next reading of a duration axis would be lower by the pause times the drop in rate: from
/// x1440 to x1 after a 30 ms pause, 43 s on the tick count (untouchable rule 3). A million PAUSE spins is
/// 14 to 70 ms depending on the processor. What the bound used to cost on EVERY call while a write stood
/// still for good is what [`STUCK_CTL`] and [`STUCK_SEQ`] remove.
const SEQLOCK_READ_TRIES: usize = 1_000_000;

/// The control block and the odd `seq` of a write a reader of THIS process already waited the whole bound
/// on (R5-WY12, R5-D36). A reader that finds the same write still open reads at once instead of waiting the
/// bound again - before this, every clock call paid 14 to 70 ms for as long as the write stood open, a core
/// stopped in a debugger or killed in the middle of a rate change. A reader already spinning on that write
/// leaves too, once another one gave up.
///
/// Nothing clears it, and nothing has to: the next write moves `seq`, and coming back to the same value takes
/// 2^31 writes. 0 is never an odd `seq`, so it means "none". The block's address rides along because a process
/// may map more than one block (the tests here do), and a pair written by two readers giving up on two blocks
/// at once can only fail to match, which costs one more wait, never a reading taken without one.
static STUCK_CTL: AtomicUsize = AtomicUsize::new(0);

/// See [`STUCK_CTL`].
static STUCK_SEQ: AtomicU32 = AtomicU32::new(0);

/// Whether a reader of this process has already waited the whole bound on this block's write `seq`.
fn write_known_stuck(p: *const Ctl, seq: u32) -> bool {
    STUCK_SEQ.load(Ordering::Relaxed) == seq && STUCK_CTL.load(Ordering::Relaxed) == p as usize
}

/// Run `read` between two looks at `seq` that agree and are even, so what it returns belongs to one
/// complete write, retrying while a write is in progress. Every reader of the anchor goes through here.
///
/// Acquire fences put `read` after the first look and before the second, so a torn read racing a
/// writer is caught by the comparison. See `write_anchor` for why Release/Acquire costs nothing on x86
/// and x64 yet is correct on a weakly ordered processor.
///
/// The seqlock never settles within the bound only when the writer (the core) stopped in the middle of
/// a write - force-killed, which leaves `seq` odd for good (RELEASE-009), or held in a debugger. Spinning
/// at 100% CPU for ever is not the answer to that, so `read` runs once more and its answer stands - a
/// stopped writer's fields hold still, and the watcher lets the process go right after a dead core. The
/// write is remembered, so the readers after this one do not wait the bound again (R5-WY12). The check
/// for it is on the odd branch only, so a reader that finds no write open pays nothing for it.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
#[inline]
unsafe fn read_consistent<T>(p: *const Ctl, read: impl Fn() -> T) -> T { unsafe {
    for _ in 0..SEQLOCK_READ_TRIES {
        let s1 = read_volatile(addr_of!((*p).seq));
        if s1 & 1 == 0 {
            fence(Ordering::Acquire);
            let value = read();
            fence(Ordering::Acquire);
            if s1 == read_volatile(addr_of!((*p).seq)) {
                return value;
            }
        } else if write_known_stuck(p, s1) {
            fence(Ordering::Acquire);
            return read();
        }
        std::hint::spin_loop();
    }
    let s = read_volatile(addr_of!((*p).seq));
    if s & 1 == 1 {
        STUCK_CTL.store(p as usize, Ordering::Relaxed);
        STUCK_SEQ.store(s, Ordering::Relaxed);
    }
    fence(Ordering::Acquire);
    read()
}}

/// Read the anchor triple `(a_fake, a_real, multiplier)` under the seqlock, retrying on a concurrent write.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_anchor(p: *const Ctl) -> (i64, i64, i64) { unsafe {
    read_consistent(p, || wall_fields(p))
}}

/// The wall anchor triple and the real clock, read in ONE seqlock window (R4-S3): the clock a projection
/// runs from belongs to the anchor it projects. See `rebase` for why it has to be inside. `clock` returns
/// the real QUIT.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
#[inline]
pub unsafe fn read_anchor_with(p: *const Ctl, clock: impl Fn() -> i64) -> ((i64, i64, i64), i64) { unsafe {
    read_consistent(p, || (wall_fields(p), clock()))
}}

/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
#[inline]
unsafe fn wall_fields(p: *const Ctl) -> (i64, i64, i64) { unsafe {
    (
        read_volatile(addr_of!((*p).a_fake)),
        read_volatile(addr_of!((*p).a_real)),
        read_volatile(addr_of!((*p).multiplier)),
    )
}}

/// The duration anchor with its rate, as one snapshot: what the tick count, `timeGetTime` and QUIT are
/// projected from. The multiplier rides along so the rate and the base it scales can never tear apart
/// across a rate change (untouchable rule 3, a mismatched pair would dip the axis).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurAnchor {
    /// See `Ctl::dur_tick_offset`.
    pub tick_offset: i64,
    /// Fake QUIT base.
    pub quit_c0: i64,
    /// Real QUIT the base is measured from.
    pub q0: i64,
    /// The rate, clamped to at least 1 where it is used.
    pub m: i64,
}

impl DurAnchor {
    /// The fake QUIT at real QUIT `real`. `released_at` is where the session let this process go, 0 while
    /// it holds it: the session's rate up to that instant and the real rate after it (`released_line`).
    pub fn quit_at(&self, real: i64, released_at: i64) -> i64 {
        released_line(real, released_at, |r| dur_quit_at(self.quit_c0, self.q0, self.m, r))
    }

    /// The fake `GetTickCount64` at real QUIT `real`: the fake QUIT in milliseconds plus the offset.
    pub fn tick_at(&self, real: i64, released_at: i64) -> u64 {
        tick_from_quit(self.tick_offset, self.quit_at(real, released_at))
    }
}

/// Read the duration anchor under the seqlock, retrying on a concurrent write.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_dur(p: *const Ctl) -> DurAnchor { unsafe {
    read_consistent(p, || dur_fields(p))
}}

/// The duration anchor and the real QUIT, read in ONE seqlock window (R4-S3, see `rebase`). Every
/// duration detour reads through this, so the clock it projects from belongs to the anchor it projects.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
#[inline]
pub unsafe fn read_dur_with(p: *const Ctl, clock: impl Fn() -> i64) -> (DurAnchor, i64) { unsafe {
    read_consistent(p, || (dur_fields(p), clock()))
}}

/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
#[inline]
unsafe fn dur_fields(p: *const Ctl) -> DurAnchor { unsafe {
    DurAnchor {
        tick_offset: read_volatile(addr_of!((*p).dur_tick_offset)),
        quit_c0: read_volatile(addr_of!((*p).dur_quit_c0)),
        q0: read_volatile(addr_of!((*p).dur_q0)),
        m: read_volatile(addr_of!((*p).multiplier)),
    }
}}

/// The fake wall instant at `now_real`, from the anchor and the rate.
///
/// One implementation, called from both sides, because the two sides answer different questions
/// about the same clock: the hook computes what the TARGET sees, the mechanism computes what the
/// session REPORTS about itself. A difference between them is the tool lying about its own clock,
/// and until this moved here the invariant was two copies of the formula plus a comment asking
/// that they be kept in step - which is exactly the shape untouchable rule 12 calls worse than no
/// guard at all. `chrono-ctl` already hosts this class of pure projection (`dur_tick_at`,
/// `dur_quit_at`, `dur_qpc_at`), so this is finishing an existing split, not inventing one.
///
/// Saturating, then clamped to the last representable instant. Wrapping handed different channels
/// different answers once the fake clock ran past the end of the range: the raw FILETIME channels
/// reported the wrapped number, while `GetSystemTime` and `GetLocalTime` go through
/// `FileTimeToSystemTime`, which rejects it - so they fell back to the REAL clock. One process, two
/// epochs, and the audit still saying `works` (R2-K2). Measured at 30828-09-01 under the maximum
/// multiplier, the target held at the clamp while `state` announced the year -27627 (R2-X2).
///
/// Both crates that call this run with `overflow-checks` off (a panic unwinding across the FFI
/// detour boundary is undefined behaviour), so every bound here is written out rather than left to
/// a debug assertion.
///
/// Never before the anchor (R5-P2, R5-D31), the floor the duration axes have had since R4-N4. The
/// anchors are read from the PRECISE clock (`QueryUnbiasedInterruptTimePrecise`), while most wall
/// channels project from the plain one, which stands on the last timer interrupt and so is never ahead
/// of the precise one (0 of 2 million pairs, measured 2026-10-14) but may be behind it by up to one
/// delayed interrupt (1.77 ms measured). A reader of the plain clock right after an anchor therefore sees
/// a negative elapsed time until the next interrupt, and without this floor its answer would fall below
/// the anchor - after a rate change, below what the previous anchor already answered.
pub fn fake_wall_at(anchor_fake: i64, anchor_real: i64, now_real: i64, multiplier: i64) -> i64 {
    let advanced = now_real.saturating_sub(anchor_real).max(0).saturating_mul(multiplier);
    // Both ends, not just the top. A FILETIME counts from 1601 and Windows types it unsigned, so a
    // negative value is not a small date - read back as unsigned it is a year around 59000, and
    // `FileTimeToSystemTime` refuses it, which drops `GetSystemTime` and `GetLocalTime` back to the
    // REAL clock while the raw channels keep the nonsense. That is the same two-epochs-in-one-process
    // failure the top clamp exists for (R2-K2, R2-X2), and it was measured at the bottom with
    // `--at 1000-01-01T00:00:00`. The moment is refused before a session starts now, so this floor
    // should never be reached - which is exactly why it is cheap to keep as the net.
    //
    // `clamp` panics if its bounds are crossed, which matters in a crate whose panics would unwind
    // across an FFI detour boundary (undefined behaviour). Here both bounds are constants and
    // `FAKE_WALL_MAX > 0` is a compile-time assertion where it is defined, so the panic is
    // unreachable by construction rather than by hope.
    anchor_fake.saturating_add(advanced).clamp(0, FAKE_WALL_MAX)
}

/// Copy `s` into a fixed-length UTF-16 buffer, always NUL-terminated, never overrunning.
///
/// Lives here rather than in the hook for one reason: the hook is a `cdylib` and has no tests at
/// all (0 across 1966 lines), while this is the classic place for an off-by-one - a fixed buffer,
/// a terminator, and a caller that cannot check. The behaviour is unchanged - what is new is that
/// it is now checkable, including the `len == 0` case where there is no room even for the
/// terminator.
pub fn set_wide(dst: &mut [u16], s: &str) {
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

/// Move a FILETIME tick count by a zone bias, or `None` if the result is not a FILETIME.
///
/// The pure half of the hook's `shift_filetime`. FILETIME 0 is the very common "no time recorded",
/// and a positive bias pushes it below zero - which, read back as the unsigned value it is, becomes
/// a date tens of thousands of years out. The old code wrapped there and still reported success, so
/// the caller had no way to notice - `None` here is what lets the detour fail honestly and leave the
/// caller's original value alone.
pub fn shift_ticks_by_bias(ticks: i64, bias_min: i32, add: bool) -> Option<i64> {
    let bias_100ns = bias_min as i64 * 60 * 10_000_000;
    let shifted = if add { ticks.checked_add(bias_100ns) } else { ticks.checked_sub(bias_100ns) };
    match shifted {
        Some(v) if v >= 0 => Some(v),
        _ => None,
    }
}

/// The fake `GetTickCount64` (milliseconds) for a fake QUIT (100 ns): the QUIT in whole milliseconds plus
/// the session's offset (R4/10a, F4). Monotonic in the QUIT, so the tick count runs back exactly when
/// QUIT does, which is never. Below zero only for an offset no session writes, held at 0 there.
pub fn tick_from_quit(dur_tick_offset: i64, fake_quit: i64) -> u64 {
    dur_tick_offset.saturating_add(fake_quit.div_euclid(10_000)).max(0) as u64
}

/// The fake `GetTickCount64` at real QUIT `real_now`, from the duration anchor - `dur_quit_at` in
/// milliseconds plus the offset. Kept as one call because the tick count, `GetTickCount` and
/// `timeGetTime` all answer it.
pub fn dur_tick_at(dur_tick_offset: i64, dur_quit_c0: i64, dur_q0: i64, m: i64, real_now: i64) -> u64 {
    tick_from_quit(dur_tick_offset, dur_quit_at(dur_quit_c0, dur_q0, m, real_now))
}

/// Project the fake `QueryUnbiasedInterruptTime` (100 ns) at real time `real_now` from the anchor:
/// `dur_quit_c0 + (real_now - dur_q0) * M`. Monotonic in `real_now` for a fixed anchor, and `rebase`
/// keeps it continuous across a rate change. `m` is clamped to >= 1, so a frozen wall clock (M = 0)
/// still advances the duration axis at real speed (untouchable rule 3). Pure so the monotonicity is
/// unit-tested without injection.
pub fn dur_quit_at(dur_quit_c0: i64, dur_q0: i64, m: i64, real_now: i64) -> i64 {
    let dm = m.max(1);
    // Never before the base (R4-N4). A reader whose clock came from a time-stamp counter can, by a few
    // nanoseconds, hold a clock earlier than the anchor it read (`clock_fence`), and a negative elapsed
    // time would put the answer under the base - for the tick count, cast to unsigned, at the top of
    // its range.
    let dq = real_now.saturating_sub(dur_q0).max(0);
    // Saturating, not wrapping, and the difference is untouchable rule 3. `dq * M` overflows i64
    // roughly 10.6 days into a session at the maximum multiplier, and WRAPPING there sent the axis
    // BACKWARDS by centuries in one step - a monotonic clock that is not monotonic. Saturating holds
    // it instead, which the rule still does not love (it says "never stops" too), but a stopped clock
    // is a class less harmful than one that rewinds, and unlike a wrap it can be DETECTED and
    // reported - see `dur_axis_at_range_end`.
    //
    // The wall axis has had a clamp and a warning since R2-X2. The three duration axes, which are the
    // ones rule 3 is actually about, had neither.
    dur_quit_c0.saturating_add(dq.saturating_mul(dm))
}

/// An axis after its process was let go at `released_at` (0 = not let go): the session's line up to
/// that instant, then the real rate from where it stood (ADR-14, R4/10a F6). Continuous at the release
/// and monotonic on both sides, so a reader that computes it from its own snapshot of the anchor and the
/// watcher that computes it once give the same answer for the same clock.
pub fn released_line(real: i64, released_at: i64, session: impl Fn(i64) -> i64) -> i64 {
    if released_at == 0 || real < released_at {
        return session(real);
    }
    session(released_at).saturating_add(real.saturating_sub(released_at))
}

/// Whether a duration axis has reached the end of what an i64 can hold and is therefore STANDING
/// rather than advancing.
///
/// Within one rate segment the elapsed term is what saturates first, so this asks about the same
/// product the three projections compute, and then about the sum of it and the base (see below). It
/// asks the arithmetic rather than comparing the projected VALUE with the top, because a saturated term
/// over a base below zero would still produce a value that looks ordinary.
///
/// At the maximum multiplier this arrives about 10.6 days into a session, and at the largest speed
/// anyone has documented using (x1440, a day a minute) about 20 years - so the ordinary session never
/// meets it. It is reported rather than assumed away because the alternative is a clock that quietly
/// stops while the session goes on counting, which is the shape of R2-X2 on the wall axis.
///
/// The base counts too (R5-P17). A rate change banks the elapsed term into the base, so a session whose
/// rate changes now and then never fills the term of any one segment, while the base, the sum of them
/// all, reaches the top and stands there - and asking about the term alone said nothing. The fake
/// elapsed time a session reports is an integral of a rate never above the one this axis runs at, from a
/// base that starts at zero or above, so it cannot reach the top of its range before this answers yes.
pub fn dur_axis_at_range_end(dur_quit_c0: i64, dur_q0: i64, m: i64, real_now: i64) -> bool {
    let dm = m.max(1);
    let dq = real_now.saturating_sub(dur_q0).max(0);
    dq.checked_mul(dm).and_then(|elapsed| dur_quit_c0.checked_add(elapsed)).is_none()
}

/// The QPC anchor with its rate, as one snapshot (a torn read mixing a new rate with an old base would
/// dip the axis, rule 3). The companion of `DurAnchor` for the QPC detour (opt-in `scale_qpc`, ADR-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QpcAnchor {
    /// Fake QPC base, raw ticks.
    pub c0: i64,
    /// Real QPC the base is measured from.
    pub q0: i64,
    /// The rate, clamped to at least 1 where it is used.
    pub m: i64,
}

impl QpcAnchor {
    /// The fake QPC at real QPC `real`, released at `released_at` (0 = not released, `released_line`).
    pub fn at(&self, real: i64, released_at: i64) -> i64 {
        released_line(real, released_at, |r| dur_qpc_at(self.c0, self.q0, self.m, r))
    }
}

/// Read the QPC anchor under the seqlock, retrying on a concurrent write.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_qpc(p: *const Ctl) -> QpcAnchor { unsafe {
    read_consistent(p, || qpc_fields(p))
}}

/// The QPC anchor and the real counter, read in ONE seqlock window (R4-S3, see `rebase`). `clock` is
/// the real `QueryPerformanceCounter` followed by `clock_fence`, so the counter is read before the
/// second look at `seq` and not after it.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
#[inline]
pub unsafe fn read_qpc_with(p: *const Ctl, clock: impl Fn() -> i64) -> (QpcAnchor, i64) { unsafe {
    read_consistent(p, || (qpc_fields(p), clock()))
}}

/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
#[inline]
unsafe fn qpc_fields(p: *const Ctl) -> QpcAnchor { unsafe {
    QpcAnchor {
        c0: read_volatile(addr_of!((*p).dur_qpc_c0)),
        q0: read_volatile(addr_of!((*p).dur_qpc_q0)),
        m: read_volatile(addr_of!((*p).multiplier)),
    }
}}

/// Project the fake QueryPerformanceCounter (raw QPC ticks) at real QPC `real_now` from the anchor:
/// `dur_qpc_c0 + (real_now - dur_qpc_q0) * M`. QueryPerformanceFrequency is NOT scaled, so elapsed
/// (delta / freq) scales by exactly M. `m` clamped to >= 1 (rule 3, like `dur_quit_at`). Pure, so the
/// monotonicity is unit-tested without injection.
pub fn dur_qpc_at(dur_qpc_c0: i64, dur_qpc_q0: i64, m: i64, real_now: i64) -> i64 {
    let dm = m.max(1);
    // Never before the base (R4-N4), as in `dur_quit_at` - and this is the axis whose clock is the
    // time-stamp counter the note there is about.
    let dq = real_now.saturating_sub(dur_qpc_q0).max(0);
    // Saturating, like the other two axes. QPC is the one a target is most likely to read as elapsed
    // time (Stopwatch, nanoTime, perf_counter), so a rewind here is the most visible of the three.
    dur_qpc_c0.saturating_add(dq.saturating_mul(dm))
}

/// How long after the watcher reads the clock the session lets a process go (R4/10a, F6): 100 ms, in
/// QUIT units. Until then the axes keep the session's rate, as they would had the watcher noticed the
/// core's end that much later.
///
/// Why there is a margin at all: a detour that found the session holding reads the anchor and its clock
/// and answers from them, and the watcher may choose its instant meanwhile. The detour looks at the
/// published instant AFTER its clock (`released_line`), so an instant it can see is one it obeys. One
/// published after that look was read by the watcher after the detour's clock, so the detour answered
/// for a moment still on the session's line - unless the watcher stood between reading the clock and
/// publishing the instant for longer than the margin, two instructions apart. That is the bound this
/// keeps, and ADR-14 says it.
pub const RELEASE_MARGIN_QUIT: i64 = 1_000_000;

/// The QPC counterpart of [`RELEASE_MARGIN_QUIT`] for a counter running at `frequency` ticks a second:
/// a tenth of a second, at least one tick.
pub fn release_margin_qpc(frequency: i64) -> i64 {
    (frequency / 10).max(1)
}

/// The three duration axes after the session let a process go: the anchor they ran on and the instants
/// from which they run at the real rate. Built by [`release_axes`], read by the hook once its core is
/// gone - and the same line a detour computes for itself from its own snapshot of the anchor while the
/// watcher is still on its way (`DurAnchor::quit_at`, `QpcAnchor::at`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleasedAxes {
    dur: DurAnchor,
    released_at: i64,
    qpc: QpcAnchor,
    qpc_released_at: i64,
}

/// Let every duration axis go at `released_at` (QUIT) and `qpc_released_at` (QPC), so that it carries on
/// from where it stood at rate 1 instead of snapping back to the real value.
///
/// Snapping back is what the hook did until 2026-09-24, and it was measured rather than supposed: a
/// session at x60 that ended after 5.4 s while its target kept running sent GetTickCount64,
/// GetTickCount, timeGetTime, QueryUnbiasedInterruptTime and QPC back by 316 s in one step, on x64 and
/// x86 alike. The axis untouchable rule 3 says never rewinds was rewound by the tool leaving.
///
/// The line is `released_line`: the session's rate up to the instant and the real rate after it, so the
/// value right after equals the value right before. The wall clock is not part of it - it goes back to
/// the real one, which a wall may do and the report says it did.
pub fn release_axes(dur: DurAnchor, qpc: QpcAnchor, released_at: i64, qpc_released_at: i64) -> ReleasedAxes {
    ReleasedAxes { dur, released_at, qpc, qpc_released_at }
}

impl ReleasedAxes {
    /// `GetTickCount64` after the release, in milliseconds, from the real QUIT.
    pub fn tick_at(&self, real_quit: i64) -> u64 {
        self.dur.tick_at(real_quit, self.released_at)
    }

    /// `QueryUnbiasedInterruptTime` after the release, in 100 ns, from the real QUIT.
    pub fn quit_at(&self, real_quit: i64) -> i64 {
        self.dur.quit_at(real_quit, self.released_at)
    }

    /// `QueryPerformanceCounter` after the release, in raw ticks, from the real QPC.
    pub fn qpc_at(&self, real_qpc: i64) -> i64 {
        self.qpc.at(real_qpc, self.qpc_released_at)
    }
}

/// How many times [`sleep_bias`] reads its three clocks before it settles for a lower bound.
pub const SLEEP_BIAS_TRIES: u32 = 4;

/// How long the machine has slept since it started, in 100 ns units: the interrupt time
/// (`QueryInterruptTime`, which counts sleep) minus the unbiased one (`quit`, which does not). Read on
/// both sides from one implementation - the hook adds it to the fake QUIT to answer the interrupt-time
/// channels (R5-F4), the core watches it grow to tell that the machine slept (R5-P1).
///
/// Read as a triple, QUIT, interrupt time, QUIT, and taken only when the two QUIT readings agree. Both
/// clocks stand on the same timer interrupt - `QueryInterruptTime` reads the interrupt time and QUIT is that
/// same time minus the sleep bias (disassembled on both bitnesses, 2026-10-14) - so with no interrupt between
/// the two QUIT readings the difference is exact. Two separate calls straddle an interrupt now and then, and
/// their difference scattered by about a millisecond (measured, 4.5 of the R5 plan) - a fake clock built on
/// that scatter would step back. A thread interrupted between the reads four times running settles for `interrupt - the
/// later QUIT`, which is never above the real bias, so a caller that must not go back keeps its own maximum.
pub fn sleep_bias(quit: impl Fn() -> i64, interrupt_time: impl Fn() -> i64) -> i64 {
    let mut bound = 0;
    for _ in 0..SLEEP_BIAS_TRIES {
        let before = quit();
        let interrupt = interrupt_time();
        let after = quit();
        if before == after {
            return interrupt.saturating_sub(before).max(0);
        }
        bound = interrupt.saturating_sub(after).max(0);
    }
    bound
}

/// Write the session zone bias (stable field, outside the seqlock). Mechanism side.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_tz_bias(p: *mut Ctl, bias: i32) { unsafe {
    write_volatile(addr_of_mut!((*p).tz_bias), bias);
}}

/// Read the session zone bias (hook side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_tz_bias(p: *const Ctl) -> i32 { unsafe {
    read_volatile(addr_of!((*p).tz_bias))
}}

/// Write which name the session zone is handed out under (stable field, outside the seqlock, ADR-26).
/// Mechanism side.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_zone_name(p: *mut Ctl, index: u32) { unsafe {
    write_volatile(addr_of_mut!((*p).zone_name), index);
}}

/// Read which name the session zone is handed out under (hook side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_zone_name(p: *const Ctl) -> u32 { unsafe {
    read_volatile(addr_of!((*p).zone_name))
}}

/// Write the scale-duration flag (stable field, outside the seqlock). Mechanism side.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_scale_dur(p: *mut Ctl, on: bool) { unsafe {
    write_volatile(addr_of_mut!((*p).scale_dur), on as u32);
}}

/// Read the scale-duration flag (hook side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_scale_dur(p: *const Ctl) -> bool { unsafe {
    read_volatile(addr_of!((*p).scale_dur)) != 0
}}

/// Write the scale-QPC flag (stable field, outside the seqlock). Mechanism side (ADR-2 reversal, opt-in).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_scale_qpc(p: *mut Ctl, on: bool) { unsafe {
    write_volatile(addr_of_mut!((*p).scale_qpc), on as u32);
}}

/// Read the scale-QPC flag (hook side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_scale_qpc(p: *const Ctl) -> bool { unsafe {
    read_volatile(addr_of!((*p).scale_qpc)) != 0
}}

/// Write the core PID (stable field, outside the seqlock). Mechanism side.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_core_pid(p: *mut Ctl, pid: u32) { unsafe {
    write_volatile(addr_of_mut!((*p).core_pid), pid);
}}

/// Read the core PID (hook side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_core_pid(p: *const Ctl) -> u32 { unsafe {
    read_volatile(addr_of!((*p).core_pid))
}}

/// Write when the core process was created (stable field, outside the seqlock). Mechanism side, BEFORE
/// `write_core_pid`: the pid is the last word of the session's identity to land, so a reader that sees
/// it sees this too.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn write_core_created(p: *mut Ctl, created: u64) { unsafe {
    write_volatile(addr_of_mut!((*p).core_created), created);
}}

/// Read when the core process was created (hook side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_core_created(p: *const Ctl) -> u64 { unsafe {
    read_volatile(addr_of!((*p).core_created))
}}

/// Mark the session as ended in order (mechanism side, R4-W2). One-way: nothing clears it but the
/// zeroing a new core does when it reclaims the block for the next session.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn mark_ended(p: *mut Ctl) { unsafe {
    write_volatile(addr_of_mut!((*p).ended), 1);
}}

/// Whether the core ended this session in order - read by a hook deciding whether to join, and by a
/// new core deciding what to say about the block it reclaims.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_ended(p: *const Ctl) -> bool { unsafe {
    read_volatile(addr_of!((*p).ended)) != 0
}}

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
pub unsafe fn reserve_cov_slot(p: *mut Ctl) -> Option<usize> { unsafe {
    // Atomic reservation: a volatile RMW could hand two children the same slot and
    // lose one from the audit entirely (worse than the calls under-count).
    let counter = &*(addr_of!((*p).pid_count) as *const AtomicU32);
    let slot = counter.fetch_add(1, Ordering::SeqCst) as usize;
    if slot >= MAX_COV_PIDS {
        return None;
    }
    Some(slot)
}}

/// Publish this process's PID into the slot it reserved (hook side), announcing to the mechanism
/// that the slot is ready to read. Call LAST, after the coverage slot carries the installed mask.
///
/// The release fence pairs with the acquire in `read_pid`: it keeps the writes that filled the slot
/// from being seen after the pid that advertises them. On x86 this costs nothing at runtime, but it
/// is what stops the compiler from reordering the publication ahead of the evidence.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `slot < MAX_COV_PIDS`.
pub unsafe fn publish_pid(p: *mut Ctl, slot: usize, pid: u32) { unsafe {
    fence(Ordering::Release);
    let slotp = (addr_of_mut!((*p).pids) as *mut u32).add(slot);
    write_volatile(slotp, pid);
}}

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
pub unsafe fn read_pid_count(p: *const Ctl) -> u32 { unsafe {
    let counter = &*(addr_of!((*p).pid_count) as *const AtomicU32);
    counter.load(Ordering::SeqCst)
}}

/// Read one PID registry slot (mechanism side). `i` must be < MAX_COV_PIDS. A zero
/// means "empty or not yet published" - the mechanism scans all slots and skips zeros,
/// so a slot reserved but not yet published is simply picked up on the next refresh.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `i < MAX_COV_PIDS`.
pub unsafe fn read_pid(p: *const Ctl, i: usize) -> u32 { unsafe {
    let slotp = (addr_of!((*p).pids) as *const u32).add(i);
    let pid = read_volatile(slotp);
    if pid != 0 {
        // Pairs with the release in `publish_pid`, so the slot's contents are visible to whoever
        // just saw the pid that advertises them.
        fence(Ordering::Acquire);
    }
    pid
}}

/// Pointer to one slot's coverage (hook side, its own slot). `i` must be < MAX_COV_PIDS.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `i < MAX_COV_PIDS`.
pub unsafe fn cov_at_mut(p: *mut Ctl, i: usize) -> *mut Cov { unsafe {
    (addr_of_mut!((*p).covs) as *mut Cov).add(i)
}}

/// Pointer to one slot's coverage (mechanism side, read-only). `i` must be < MAX_COV_PIDS.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `i < MAX_COV_PIDS`.
pub unsafe fn cov_at(p: *const Ctl, i: usize) -> *const Cov { unsafe {
    (addr_of!((*p).covs) as *const Cov).add(i)
}}

/// Publish the set of installed channels (hook side, per-process `Cov`). A whole-mask WRITE,
/// not an OR-in: the hook side collects bits locally while it creates the detours and publishes
/// them only once every detour is actually ENABLED, so a failure part-way through can publish
/// nothing (mask 0) instead of leaving a half-set claim behind. A bit here means "this channel's
/// detour is live", and the audit reads it as such - so it must never be set for a detour that was
/// merely prepared (rule 4: the audit never claims a channel it did not cover).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn set_channels_installed(p: *mut Cov, mask: u64) { unsafe {
    write_volatile(addr_of_mut!((*p).installed_channels), mask);
}}

/// Record when the process that owns this slot was created (hook side, its own `Cov`). Written before
/// the pid is published, so the release in `publish_pid` carries it to the mechanism with the rest.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn set_created(p: *mut Cov, created: u64) { unsafe {
    write_volatile(addr_of_mut!((*p).created), created);
}}

/// When the process that owns this slot was created, or 0 when its hook did not record it (mechanism
/// side, per-process `Cov`). Read after `read_pid` returned the slot's pid, whose acquire makes it visible.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_created(p: *const Cov) -> u64 { unsafe {
    read_volatile(addr_of!((*p).created))
}}

/// `IMAGE_SUBSYSTEM_WINDOWS_GUI`: a program that starts without a console, the kind that opens windows.
pub const SUBSYSTEM_WINDOWS: u32 = 2;

/// `IMAGE_SUBSYSTEM_WINDOWS_CUI`: a console program.
pub const SUBSYSTEM_CONSOLE: u32 = 3;

/// How far into an image its headers are read for the subsystem. The first page of a mapped image always
/// holds them, and a header placed further in answers "not recorded", which is the safe direction.
pub const IMAGE_HEADER_SPAN: usize = 4096;

/// The subsystem an image's own headers name (`OptionalHeader.Subsystem`), from the bytes at the start of
/// the image, or 0 when they do not read as a program's headers: no `MZ`, a PE header outside `headers`,
/// no `PE\0\0`, or an optional header that is neither PE32 nor PE32+. The field sits at the same offset in
/// both. Never panics and never allocates, because the hook calls it inside `DllMain` on its own image.
pub fn image_subsystem(headers: &[u8]) -> u32 {
    let u16_at = |at: usize| headers.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    if headers.get(..2) != Some(b"MZ".as_slice()) {
        return 0;
    }
    let Some(pe) = headers.get(0x3C..0x40).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) else {
        return 0;
    };
    // The signature, then the 20-byte file header, then the optional header.
    let optional = pe.saturating_add(24);
    if headers.get(pe..pe.saturating_add(4)) != Some(b"PE\0\0".as_slice())
        || !matches!(u16_at(optional), Some(0x10B | 0x20B))
    {
        return 0;
    }
    u16_at(optional.saturating_add(68)).map_or(0, u32::from)
}

/// Record the subsystem of the process that owns this slot (hook side, its own `Cov`). Written before the
/// pid is published, like `created`.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn set_subsystem(p: *mut Cov, subsystem: u32) { unsafe {
    write_volatile(addr_of_mut!((*p).subsystem), subsystem);
}}

/// The subsystem the process that owns this slot recorded, or 0 when it did not (mechanism side). Read
/// after `read_pid` returned the slot's pid, whose acquire makes it visible.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_subsystem(p: *const Cov) -> u32 { unsafe {
    read_volatile(addr_of!((*p).subsystem))
}}

/// Whether a slot whose process recorded `recorded` as its creation time can be the process created at
/// `created`. Zero recorded (the hook could not read it) or no time asked is unknown, and unknown does
/// not rule a slot out - only two known, different times do.
pub fn same_creation(recorded: u64, created: Option<u64>) -> bool {
    match created {
        Some(c) if recorded != 0 => c == recorded,
        _ => true,
    }
}

/// The registry slot the process `pid`, created at `created` when that is known, published its pid
/// into. `None` if it never did - its hook failed, or the registry was full - and the honest answer is
/// then no coverage, never a guess.
///
/// One source for both sides: the mechanism finds the target it launched, and the hook asks whether a
/// child it injected signed in (R4-S2). The creation time is what tells a child apart from an earlier
/// process that published the same pid - slots are never freed, and the system hands pids out again.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn find_pid_slot(p: *const Ctl, pid: u32, created: Option<u64>) -> Option<usize> { unsafe {
    if pid == 0 {
        return None; // the value of every slot not yet published
    }
    (0..MAX_COV_PIDS).find(|&i| read_pid(p, i) == pid && same_creation(read_created(cov_at(p, i)), created))
}}

/// Mark the moment this process began to exit (hook side, its own `Cov`), with the real
/// `QueryPerformanceCounter` value. One atomic store and nothing else: the hook calls it under the loader
/// lock, as the process ends.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`. Its `exit_began_qpc` then sits on the 8-byte boundary
/// an `AtomicI64` view needs, in both bitnesses (the layout test pins the offset).
pub unsafe fn mark_exit_began(p: *mut Cov, qpc: i64) { unsafe {
    let field = &*(addr_of_mut!((*p).exit_began_qpc) as *const AtomicI64);
    field.store(qpc, Ordering::Release);
}}

/// When the process that owns this slot began to exit, as its hook marked it, or 0 when it has not
/// (mechanism side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`, which aligns the field as `mark_exit_began` says.
pub unsafe fn read_exit_began(p: *const Cov) -> i64 { unsafe {
    let field = &*(addr_of!((*p).exit_began_qpc) as *const AtomicI64);
    field.load(Ordering::Acquire)
}}

/// The hook's mark of a process's exit when it falls inside the guard window, from the moment the core
/// released the process (`released_qpc`) to the moment its wait for the window ended (`window_end_qpc`),
/// or `None` (ADR-4). The mark is written by a process of the session into a block every such process can
/// write, so it is read as untrusted: only a value inside the window counts, and a mark that counts still
/// needs the process to really end within the grace the core gives it - a mark alone is never an exit.
pub fn exit_began_in_window(mark: i64, released_qpc: i64, window_end_qpc: i64) -> Option<i64> {
    (mark > 0 && released_qpc <= mark && mark <= window_end_qpc).then_some(mark)
}

/// Whole milliseconds from one `QueryPerformanceCounter` value to a later one, at `frequency` ticks a
/// second. 0 for a span that runs backwards or a frequency that is not positive, never a panic.
pub fn qpc_span_ms(from: i64, to: i64, frequency: i64) -> u64 {
    if frequency <= 0 || to <= from {
        return 0;
    }
    let ms = (i128::from(to) - i128::from(from)) * 1000 / i128::from(frequency);
    u64::try_from(ms).unwrap_or(u64::MAX)
}

/// Pointer to one entry of the pending-loads table. `i` must be < `PENDING_LOADS_MAX`.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `i < PENDING_LOADS_MAX`.
unsafe fn pending_load(p: *const Ctl, i: usize) -> *mut PendingLoad { unsafe {
    (addr_of!((*p).pending_loads) as *mut PendingLoad).add(i)
}}

/// The state word of one pending load, as the atomic it is used as.
///
/// # Safety
/// `entry` must point to a live, correctly aligned `PendingLoad`.
unsafe fn load_state<'a>(entry: *mut PendingLoad) -> &'a AtomicU32 { unsafe {
    &*(addr_of_mut!((*entry).state) as *const AtomicU32)
}}

/// Open the gate for one load of the hook into the child `pid` created at `created` (parent side), before
/// the remote thread that loads it starts. The index of the entry, or `None` when the child cannot be
/// named (a pid or a time of 0) or every entry is taken - the load then goes ahead without a gate.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn open_load_gate(p: *mut Ctl, pid: u32, created: u64) -> Option<usize> { unsafe {
    if pid == 0 || created == 0 {
        return None;
    }
    for i in 0..PENDING_LOADS_MAX {
        let entry = pending_load(p, i);
        let state = load_state(entry);
        if state.compare_exchange(LOAD_FREE, LOAD_CLAIMED, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            write_volatile(addr_of_mut!((*entry).pid), pid);
            write_volatile(addr_of_mut!((*entry).created), created);
            // Pairs with the acquire in `pass_load_gate`: a hook that sees the entry pending sees its name.
            state.store(LOAD_PENDING, Ordering::Release);
            return Some(i);
        }
    }
    None
}}

/// What a child's hook found at its gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadGate {
    /// Its parent was waiting for this load, and still is: install.
    Joined,
    /// Its parent stopped waiting and let the program run on the real clock: refuse, before anything.
    Refused,
    /// No gate names this process - the core's own target, or a parent that found the table full.
    NotPending,
}

/// Pass the gate a parent opened for this process's hook, if one names it (hook side, before anything is
/// installed). A pending entry is moved to joined, which the parent can no longer abandon. An abandoned
/// one is a refusal, and is freed here: the one load it was for has now come and been refused.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn pass_load_gate(p: *mut Ctl, pid: u32, created: u64) -> LoadGate { unsafe {
    if pid == 0 || created == 0 {
        return LoadGate::NotPending;
    }
    for i in 0..PENDING_LOADS_MAX {
        let entry = pending_load(p, i);
        let state = load_state(entry);
        let seen = state.load(Ordering::Acquire);
        if !matches!(seen, LOAD_PENDING | LOAD_ABANDONED)
            || read_volatile(addr_of!((*entry).pid)) != pid
            || read_volatile(addr_of!((*entry).created)) != created
        {
            continue;
        }
        if seen == LOAD_PENDING {
            match state.compare_exchange(LOAD_PENDING, LOAD_JOINED, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return LoadGate::Joined,
                // The parent abandoned it between the two looks.
                Err(LOAD_ABANDONED) => {}
                Err(_) => continue,
            }
        }
        let _ = state.compare_exchange(LOAD_ABANDONED, LOAD_FREE, Ordering::SeqCst, Ordering::SeqCst);
        return LoadGate::Refused;
    }
    LoadGate::NotPending
}}

/// Stop waiting for the load the gate `i` stands for (parent side, the wait for the remote thread ran
/// out). True when the gate is now abandoned and the child's hook will refuse, false when the hook got
/// there first and is installing - the parent then waits for the install before it lets the child go.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `i` an index `open_load_gate` returned.
pub unsafe fn abandon_load_gate(p: *mut Ctl, i: usize) -> bool { unsafe {
    if i >= PENDING_LOADS_MAX {
        return false;
    }
    load_state(pending_load(p, i))
        .compare_exchange(LOAD_PENDING, LOAD_ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}}

/// Give the gate `i` back once the load it stands for has ended, however it ended (parent side). Never
/// called for a gate the parent abandoned: that one waits for the child's hook.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `i` an index `open_load_gate` returned.
pub unsafe fn close_load_gate(p: *mut Ctl, i: usize) { unsafe {
    if i < PENDING_LOADS_MAX {
        load_state(pending_load(p, i)).store(LOAD_FREE, Ordering::Release);
    }
}}

/// Read the installed-channels bitmask (mechanism side, per-process `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_installed(p: *const Cov) -> u64 { unsafe {
    read_volatile(addr_of!((*p).installed_channels))
}}

/// Record which channels were hooked only after `DllMain` (hook side, per-process `Cov`).
///
/// Written BEFORE the matching bits reach `installed_channels`, and that ordering does not have to
/// be the safe one: the mechanism reads these two masks as an intersection, so a reader that catches
/// the pair half-written sees a late channel only once it is also a covered one. Neither order can
/// produce a warning about a channel the report does not list.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn set_late_installed(p: *mut Cov, mask: u64) { unsafe {
    write_volatile(addr_of_mut!((*p).late_installed), mask);
}}

/// Read the late-install bitmask (mechanism side, per-process `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_late_installed(p: *const Cov) -> u64 { unsafe {
    read_volatile(addr_of!((*p).late_installed))
}}

/// Publish the channels whose detour could not be made or switched on (hook side, per-process `Cov`).
/// A whole-mask write of everything that failed so far: `install` writes it with the installed mask,
/// and after that only the watcher does, so there is one writer at a time.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn set_failed_channels(p: *mut Cov, mask: u64) { unsafe {
    write_volatile(addr_of_mut!((*p).failed_channels), mask);
}}

/// Read the failed-detour bitmask (mechanism side, per-process `Cov`). A bit that is also in
/// `installed_channels` is a detour that failed once and went live on a later try.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_failed_channels(p: *const Cov) -> u64 { unsafe {
    read_volatile(addr_of!((*p).failed_channels))
}}

/// Where a channel's call counter stops (R5-WY11, R5-D32): 2^20, a little over a million calls. A
/// counter that reached it says "at least this many", which the report and the window say in words
/// (`ChannelCoverage::capped`), so the audit still never claims more calls than there were.
///
/// Why a ceiling at all: the counter was the only WRITE on the clock's hot path, and every thread of the
/// target that read a clock wrote the same cache line, which then moved between cores on every call.
/// Measured on this machine (tools/probes/r5/fala3/ab.ps1, `GetSystemTimeAsFileTime`, 8 threads): 86.6 ns
/// a call with the counter against 14.1 ns without it, 174 against 21 at 16 threads, while one thread
/// could not tell them apart. A counter that stops writing turns that line into one every core only
/// reads, and the cost is paid once, on the way up to the ceiling. Per-thread counters in thread-local
/// storage were the alternative and were turned down: the same steady state, and a new way to fail
/// inside a library injected into somebody else's application (a count lost or double-taken when a
/// thread ends).
///
/// The counter never passes the ceiling, so it always fits the low word, and the 64-bit read on 32-bit
/// Windows, which is two loads, can no longer tear.
pub const CALLS_CAP: u64 = 1 << 20;

// Checked when the crate compiles: the ceiling fits the low word, which is what keeps a 64-bit read made of
// two 32-bit loads from tearing a count.
const _: () = assert!(CALLS_CAP < 1 << 32, "the call ceiling must fit the low 32 bits");

/// Increment a channel's call counter (hook side, per-process `Cov`), up to [`CALLS_CAP`]. `idx` must be
/// < CHANNEL_COUNT.
///
/// A plain read and write, not an atomic: two threads may lose a bump, which only under-counts, the
/// direction of error the audit already accepts. It never writes past the ceiling, because it writes
/// only a value it read below it, plus one.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`, and `idx < CHANNEL_COUNT`.
pub unsafe fn bump_calls(p: *mut Cov, idx: usize) { unsafe {
    let slot = (addr_of_mut!((*p).calls) as *mut u64).add(idx);
    let cur = read_volatile(slot);
    if cur < CALLS_CAP {
        write_volatile(slot, cur + 1);
    }
}}

/// Whether a call count read out of a `Cov` stopped at the ceiling, so the real number is at least it.
pub fn calls_capped(calls: u64) -> bool {
    calls >= CALLS_CAP
}

/// Read a channel's call counter (mechanism side, per-process `Cov`). `idx` must be
/// < CHANNEL_COUNT.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`, and `idx < CHANNEL_COUNT`.
pub unsafe fn read_calls(p: *const Cov, idx: usize) -> u64 { unsafe {
    let slot = (addr_of!((*p).calls) as *const u64).add(idx);
    read_volatile(slot)
}}

/// Record that a child this process spawned could not be followed into (hook side, own `Cov`).
/// Same volatile RMW as `bump_calls`: two threads spawning at once may lose a bump, which can only
/// ever UNDER-count a failure, never invent one - and one is already enough to raise the warning.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn bump_uninjected_children(p: *mut Cov) { unsafe {
    let slot = addr_of_mut!((*p).uninjected_children);
    let cur = read_volatile(slot);
    write_volatile(slot, cur.wrapping_add(1));
}}

/// How many children this process spawned without coverage (mechanism side, per-process `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_uninjected_children(p: *const Cov) -> u64 { unsafe {
    read_volatile(addr_of!((*p).uninjected_children))
}}

/// Name a child this process spawned that the hook did not follow into (hook side, own `Cov`).
///
/// The counter is the atomic index: `fetch_add` claims one entry per call, so two threads spawning
/// at once each get their own (the plain RMW `bump_uninjected_children` uses can lose a bump, which
/// for a COUNT only under-reports - here it would drop a pid). An entry past the ring is not written,
/// but the counter still says how many there were. The pid is written AFTER the claim, so a reader
/// can see a claimed entry still holding 0 - it treats that as "not yet" and comes back.
///
/// Every access to the ring and its counter is an atomic on the shared word, not a volatile one:
/// the writer is a thread in the target and the reader a thread in the core, and a volatile store
/// racing a volatile load is a data race in the language's model even where the hardware makes it
/// look fine. Release on the store, acquire on the loads - the same pairing `publish_pid` does with
/// fences - so a reader that sees a pid sees it whole.
///
/// A pid of 0 is never a child (that is the idle process), so 0 is safe as the "empty" mark.
///
/// Called from a detour: no allocation, no panic - two atomics.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn record_uncovered_child(p: *mut Cov, pid: u32) { unsafe {
    if pid == 0 {
        return;
    }
    let counter = &*(addr_of!((*p).uncovered_children_count) as *const AtomicU32);
    let index = counter.fetch_add(1, Ordering::SeqCst) as usize;
    if index < UNCOVERED_CHILDREN_MAX {
        let entry = &*((addr_of!((*p).uncovered_children) as *const u32).add(index) as *const AtomicU32);
        entry.store(pid, Ordering::Release);
    }
}}

/// How many uncovered children this process has spawned in total (mechanism side). The ring holds
/// at most `UNCOVERED_CHILDREN_MAX` of them by pid - see [`read_uncovered_child`].
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_uncovered_children_count(p: *const Cov) -> u32 { unsafe {
    let counter = &*(addr_of!((*p).uncovered_children_count) as *const AtomicU32);
    counter.load(Ordering::Acquire)
}}

/// The pid in ring entry `index`, or 0 while the entry is claimed but not yet written (or never
/// claimed). `index` past the ring reads as 0 as well, so a caller iterating up to the count never
/// indexes out of bounds.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_uncovered_child(p: *const Cov, index: usize) -> u32 { unsafe {
    if index >= UNCOVERED_CHILDREN_MAX {
        return 0;
    }
    let entry = &*((addr_of!((*p).uncovered_children) as *const u32).add(index) as *const AtomicU32);
    entry.load(Ordering::Acquire)
}}

/// Count one wait held at the scaling floor (hook side, this process's own `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn bump_waits_at_floor(p: *mut Cov) { unsafe {
    let slot = addr_of_mut!((*p).waits_at_floor);
    let cur = read_volatile(slot);
    write_volatile(slot, cur.wrapping_add(1));
}}

/// How many of this process's waits were held at the floor (mechanism side, per-process `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_waits_at_floor(p: *const Cov) -> u64 { unsafe {
    read_volatile(addr_of!((*p).waits_at_floor))
}}

/// Count one timer held at a floor (hook side, this process's own `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn bump_timers_at_floor(p: *mut Cov) { unsafe {
    let slot = addr_of_mut!((*p).timers_at_floor);
    let cur = read_volatile(slot);
    write_volatile(slot, cur.wrapping_add(1));
}}

/// How many of this process's timers were held at a floor (mechanism side, per-process `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_timers_at_floor(p: *const Cov) -> u64 { unsafe {
    read_volatile(addr_of!((*p).timers_at_floor))
}}

/// Count one absolute deadline turned into a real delay (hook side, this process's own `Cov`, R5-D2).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn bump_absolute_dues(p: *mut Cov) { unsafe {
    let slot = addr_of_mut!((*p).absolute_dues);
    let cur = read_volatile(slot);
    write_volatile(slot, cur.wrapping_add(1));
}}

/// How many absolute deadlines this process had converted so far (mechanism side, per-process `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_absolute_dues(p: *const Cov) -> u64 { unsafe {
    read_volatile(addr_of!((*p).absolute_dues))
}}

/// Count one hooked call passed through on a thread without thread-local storage (hook side, this
/// process's own `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn bump_calls_without_storage(p: *mut Cov) { unsafe {
    let slot = addr_of_mut!((*p).calls_without_storage);
    let cur = read_volatile(slot);
    write_volatile(slot, cur.wrapping_add(1));
}}

/// How many hooked calls this process passed through on a thread without thread-local storage
/// (mechanism side, per-process `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_calls_without_storage(p: *const Cov) -> u64 { unsafe {
    read_volatile(addr_of!((*p).calls_without_storage))
}}

/// Mark that the hook could not have this process's shared C runtimes read the zone again (hook side, this
/// process's own `Cov`, ADR-27).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn mark_zone_not_read_again(p: *mut Cov) { unsafe {
    write_volatile(addr_of_mut!((*p).zone_not_read_again), 1);
}}

/// Whether the hook could not have this process's shared C runtimes read the zone again (mechanism side,
/// per-process `Cov`).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Cov`.
pub unsafe fn read_zone_not_read_again(p: *const Cov) -> bool { unsafe {
    read_volatile(addr_of!((*p).zone_not_read_again)) != 0
}}

/// The shortest real wait a scaled timeout is allowed to become.
///
/// 🔴 One millisecond, and the difference between it and zero is the whole reason it exists. Integer
/// division truncates, so under a large multiplier every timeout below M ms came out as 0 - and the
/// comment here used to call that "collapsing to a yield". `Sleep(0)` is not a short sleep: it gives
/// up the rest of the time slice and returns AT ONCE. An application polling with
/// `while (!done) Sleep(100)` therefore stopped sleeping under x1000 and started spinning a core
/// flat out, for the length of the session, with nothing said about it anywhere.
///
/// That is a cost the tester pays and cannot see, and it lands on the very thing this tool exists to
/// measure: they would read the load as a property of their application. The timer family already had
/// this floor for the same reason (`scale_timer_period_ms` clamps so a periodic timer never becomes a
/// one-shot) - the wait family simply never got one.
///
/// The floor is not free either, and the honesty is the other half of the fix: a wait that lands here
/// is NOT scaled by the full multiplier, so the hook counts it and the audit warns
/// (`wait.timeout_collapsed`). Coverage first, and say what is left over (rule 27).
pub const MIN_SCALED_WAIT_MS: u32 = 1;

/// Scale a wait timeout in milliseconds by the duration multiplier: real wait =
/// requested / M (ADR-7). `INFINITE` (0xFFFFFFFF) and 0 pass through untouched - never
/// turn "wait forever" into a finite wait, never lengthen a poll, and never turn a caller's own
/// deliberate yield into a sleep. `m` is clamped to >= 1, so frozen (M=0) leaves waits at real length
/// (untouchable rule 3). The multiplier is an integer, so a fractional "slow motion" (0<M<1) is not
/// representable and never arises - symmetric to the duration axis. A positive timeout never reaches
/// zero: see [`MIN_SCALED_WAIT_MS`].
pub fn scale_wait(ms: u32, m: i64) -> u32 {
    const INFINITE_MS: u32 = 0xFFFF_FFFF;
    if ms == 0 || ms == INFINITE_MS {
        return ms;
    }
    let m = m.max(1) as u64;
    ((ms as u64 / m) as u32).max(MIN_SCALED_WAIT_MS)
}

/// Whether [`scale_wait`] had to hold this timeout at the floor - that is, whether the acceleration
/// on THIS wait is less than the session's multiplier.
///
/// Separate from the scaling itself so the hot path keeps one return value and the audit still gets
/// the fact. Both are a couple of instructions and fold together under LTO.
pub fn wait_hit_floor(ms: u32, m: i64) -> bool {
    const INFINITE_MS: u32 = 0xFFFF_FFFF;
    if ms == 0 || ms == INFINITE_MS {
        return false; // passed through by contract, not clamped
    }
    (ms as u64 / m.max(1) as u64) < MIN_SCALED_WAIT_MS as u64
}

/// The shortest real delay a scaled `NtDelayExecution` interval becomes, in 100 ns units - the same
/// millisecond floor as [`MIN_SCALED_WAIT_MS`], in this API's own unit. `Sleep` and `SleepEx` bottom
/// out here, so a floor on one and not the other would be a floor on neither.
pub const MIN_SCALED_DELAY_TICKS: i64 = MIN_SCALED_WAIT_MS as i64 * 10_000;

/// Scale an `NtDelayExecution` interval (100 ns units) by the duration multiplier. Only a
/// NEGATIVE interval is a relative delay - scale its magnitude (interval / M, toward zero).
/// A positive interval is an absolute deadline, which the hook converts with [`scale_delay_deadline`]
/// instead (R5-D2), and a zero is a yield - both pass through here untouched. `m` is clamped to >= 1 (rule 3, symmetric to scale_wait). A relative delay never
/// reaches zero, for the reason [`MIN_SCALED_WAIT_MS`] gives: a zero delay is a yield, and turning a
/// sleep into a yield turns a polling loop into a spin.
///
/// The floor never makes a delay LONGER than the one asked for (R5-F7). This API takes 100 ns units, so
/// unlike `scale_wait` it can be asked for less than the floor, and `NtDelayExecution(-1)`, a hundred
/// nanoseconds, is a common way to give up the processor briefly. The floor used to lift it to a whole
/// millisecond at any speed, x1 included - ten thousand times the delay the application asked for - and
/// count it as a wait the speed could not divide. A delay already under the floor stays as it was.
pub fn scale_delay_interval(interval: i64, m: i64) -> i64 {
    if interval < 0 {
        let floor = interval.max(-MIN_SCALED_DELAY_TICKS);
        (interval / m.max(1)).min(floor)
    } else {
        interval
    }
}

/// Whether [`scale_delay_interval`] could not divide this delay by the full multiplier and held it at
/// the floor, or at the length asked for when that is shorter. Companion of [`wait_hit_floor`], for the
/// relative-delay shape. At x1 nothing is divided, so nothing is held: a short delay there is exact.
pub fn delay_hit_floor(interval: i64, m: i64) -> bool {
    interval < 0 && m > 1 && (interval / m) > -MIN_SCALED_DELAY_TICKS
}

/// Scale a waitable-timer due time (100 ns, FILETIME convention) into a RELATIVE real interval,
/// never positive, so the hook forwards one shape to the kernel (ADR-7 class C). A length under one
/// real tick divides to 0, which the kernel takes as a due already reached and fires at once
/// (measured 2026-10-20, found by the random properties of step 3 - this said "always negative").
///
/// A NEGATIVE due is already relative: scale its magnitude toward zero, like `scale_delay_interval`.
/// A POSITIVE due is an ABSOLUTE fake wall-clock instant (the app computed it from the substituted
/// clock): convert it to the real delay until the fake clock reaches it - `(due - fake_now) / M` -
/// returned negative. Unlike `NtDelayExecution`, a positive due here is NOT passed through: the
/// kernel reads the REAL clock for an absolute timer, so an unconverted fake instant would fire
/// years off. An absolute due already at or before `fake_now` gets `0`, which both timer APIs take as
/// an absolute instant long past and fire at once, exactly as they fire a real due already past
/// (measured on both bitnesses, 2026-10-20, and documented for the thread pool: "If the due time
/// specified by pftDueTime is zero, then the timer expires immediately"). It used to get `-1`, the
/// shortest RELATIVE due, which the kernel fires only at its next timer tick - up to 15.6 ms at the
/// default resolution, 15.6 s of session time at x1000 (R5-F12, R5-D56). `NtDelayExecution` reads a
/// zero differently, which is why it has [`scale_delay_deadline`] of its own.
///
/// `m` is clamped to >= 1, so frozen (M=0) leaves the timer at real length - the multiplier is an
/// integer, so a fractional "slow motion" (0<M<1) is not representable and never arises (symmetric
/// to the duration axis, untouchable rule 3). One consequence: an ABSOLUTE timer under
/// frozen fires as if M=1 - a frozen wall clock never reaches a future absolute due on its own, so
/// there is no "correct" wait to shorten. Documented limit, not a silent choice.
///
/// Two rates, because the two shapes follow two clocks (R5-D2). An absolute due is a moment on the WALL
/// clock, which runs at the session's speed in every session, so `wall_m` divides it. A relative due is
/// a length of time, which only a session that scales the duration axis shortens, so `dur_m` divides it,
/// and a session without `--scale-duration` passes 1 and leaves it as the application asked. Under
/// `--scale-duration` the two are the same number.
pub fn scale_timer_due(due: i64, fake_now: i64, wall_m: i64, dur_m: i64) -> i64 {
    if due < 0 {
        // Already relative: magnitude scaled toward zero - negative, or 0 under one real tick.
        due / dur_m.max(1)
    } else {
        let m = wall_m.max(1);
        // Absolute fake instant -> relative real interval until the fake clock reaches it.
        //
        // Checked, because `due` is the only value in this file that comes straight from the
        // APPLICATION under test (R2-S5). The crate's overflow-checks are off on the grounds that its
        // inputs are moments already validated upstream - true of everything else here, and not true
        // of an argument the target passed to SetWaitableTimer. It cannot overflow while `fake_now`
        // is non-negative, which the clamp guarantees, but a subtraction whose safety depends on a
        // constant two crates away is exactly the kind that stops being safe quietly.
        let Some(delta_fake) = due.checked_sub(fake_now) else {
            return due; // not representable: hand the app's own value back, unscaled
        };
        if delta_fake <= 0 {
            0 // fake clock already at/past the due time: an instant the real clock passed long ago, fired at once
        } else {
            -(delta_fake / m)
        }
    }
}

/// The absolute instant handed to `NtDelayExecution` for a deadline the session's clock has already
/// reached: one hundred nanoseconds into 1601, which the real clock passed long ago.
const PAST_DEADLINE: i64 = 1;

/// Convert an absolute `NtDelayExecution` deadline (100 ns, positive, a fake wall-clock instant the
/// application computed from the substituted clock) into what the kernel gets: the real relative delay
/// until the fake clock reaches it, as [`scale_timer_due`] works it out on the wall's speed. A non-positive
/// interval is not a deadline - a relative delay or a yield, [`scale_delay_interval`]'s business - and
/// passes through untouched, as does a deadline the arithmetic cannot reach.
///
/// A deadline already reached, or less than one real tick ahead, is where this differs from the timers
/// (R5-D59). [`scale_timer_due`] answers `0` there, and to `NtDelayExecution` a zero is not an instant: it
/// is a request to give up the processor, answered with `STATUS_NO_YIELD_PERFORMED` (0x40000024) or a
/// yield to another ready thread. A real deadline already past returns at once with `STATUS_SUCCESS` and
/// yields nothing (measured on both bitnesses, 2026-10-20), so a reached deadline goes to the kernel as an
/// absolute instant long past, [`PAST_DEADLINE`], and the application sees what it would see without the
/// session. It used to get `-1`, which waits for the next timer tick (R5-F12).
pub fn scale_delay_deadline(deadline: i64, fake_now: i64, wall_m: i64) -> i64 {
    if deadline <= 0 {
        return deadline;
    }
    match scale_timer_due(deadline, fake_now, wall_m, 1) {
        0 => PAST_DEADLINE,
        converted => converted,
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

/// `USER_TIMER_MINIMUM` from the Windows SDK: `SetTimer` lifts any shorter interval to it (the
/// function's documentation, "If uElapse is less than USER_TIMER_MINIMUM (0x0000000A)").
pub const USER_TIMER_MINIMUM_MS: u32 = 10;

/// Whether a `SetTimer` interval scaled by [`scale_timer_elapse`] lands under the minimum Windows
/// lifts it to, so this timer runs slower than the session (R5, D8). Only an interval the application
/// asked for at or above that minimum counts: a shorter one sits at the floor with or without a
/// session, so the floor takes nothing from it that the session gave.
pub fn timer_elapse_hit_floor(elapse_ms: u32, m: i64) -> bool {
    elapse_ms >= USER_TIMER_MINIMUM_MS && scale_timer_elapse(elapse_ms, m) < USER_TIMER_MINIMUM_MS
}

/// Whether a periodic timer's period scaled by [`scale_timer_period`] or [`scale_timer_period_ms`]
/// was held at their 1 ms floor instead of being divided by the full multiplier (R5, D8). Takes the
/// period widened to `i64`, which holds both the signed and the unsigned form. A one-shot (0) and a
/// period the API rejects (negative) are passed through by contract, not clamped.
pub fn timer_period_hit_floor(period_ms: i64, m: i64) -> bool {
    period_ms > 0 && period_ms / m.max(1) < 1
}

#[cfg(test)]
mod tests {

    /// The UTF-16 forms must be exactly the names, terminator included. `ascii_utf16_z` checks the
    /// LENGTH at compile time, which catches an off-by-one in `N`, and this checks the CONTENT -
    /// a wrong name would open a different kernel object and hand the target a clock nobody drives,
    /// which no other test in this workspace would notice.
    #[test]
    fn the_utf16_object_names_match_the_strings_they_come_from() {
        for (text, wide) in [
            (CTL_SECTION_NAME, &CTL_SECTION_NAME_W[..]),
            (CTL_LOCK_NAME, &CTL_LOCK_NAME_W[..]),
        ] {
            let expected: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            assert_eq!(wide, expected.as_slice(), "wide form of {text:?} drifted");
            assert_eq!(*wide.last().expect("non-empty"), 0, "not NUL-terminated");
        }
    }

    use super::*;

    /// Zeroed like a section the OS just handed us, header included: a helper that stamped the magic
    /// for free would hide the very state `header_is_ours` exists to reject.
    ///
    /// Allocated zeroed on the heap, never built on the stack: with a `Cov` for each of 4096 slots the
    /// block is over 2 MB, more than a test thread's whole stack, and `Box::new` of a literal builds the
    /// value on the stack first in a debug build (R5-P26, measured: the tests overflowed their stacks).
    fn zeroed_ctl() -> Box<Ctl> {
        let layout = std::alloc::Layout::new::<Ctl>();
        // SAFETY: every field of `Ctl` is an integer or an array of integers, so all-zero bytes are a
        // valid `Ctl`. The allocation has `Ctl`'s own layout, and the Box frees it with that layout.
        unsafe {
            let raw = std::alloc::alloc_zeroed(layout) as *mut Ctl;
            assert!(!raw.is_null(), "no memory for a test control block");
            Box::from_raw(raw)
        }
    }

    /// Headers built the way a linker lays them out: `MZ`, the PE offset at 0x3C, the signature, a 20-byte
    /// file header, then an optional header whose magic and subsystem are the two fields read.
    fn headers(pe: usize, magic: u16, subsystem: u16) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..2].copy_from_slice(b"MZ");
        h[0x3C..0x40].copy_from_slice(&(pe as u32).to_le_bytes());
        h[pe..pe + 4].copy_from_slice(b"PE\0\0");
        h[pe + 24..pe + 26].copy_from_slice(&magic.to_le_bytes());
        h[pe + 24 + 68..pe + 24 + 70].copy_from_slice(&subsystem.to_le_bytes());
        h
    }

    /// Both optional-header kinds name their subsystem at the same place, and the two values the core
    /// tells apart come back as written.
    #[test]
    fn an_image_names_its_subsystem_in_either_optional_header() {
        assert_eq!(image_subsystem(&headers(0x80, 0x20B, 3)), SUBSYSTEM_CONSOLE);
        assert_eq!(image_subsystem(&headers(0x80, 0x10B, 2)), SUBSYSTEM_WINDOWS);
        assert_eq!(image_subsystem(&headers(0xF8, 0x20B, 2)), SUBSYSTEM_WINDOWS);
    }

    /// Anything that does not read as a program's headers is "not recorded", never a guess and never a
    /// panic: the hook asks this inside `DllMain`, and the core reads 0 as a reason to keep its suspicion.
    #[test]
    fn headers_that_do_not_read_as_a_program_name_no_subsystem() {
        let mut no_mz = headers(0x80, 0x20B, 3);
        no_mz[0] = b'X';
        assert_eq!(image_subsystem(&no_mz), 0);
        let mut no_signature = headers(0x80, 0x20B, 3);
        no_signature[0x80] = b'X';
        assert_eq!(image_subsystem(&no_signature), 0);
        assert_eq!(image_subsystem(&headers(0x80, 0x107, 3)), 0, "a ROM image's magic is not a program");
        let mut far = headers(0x80, 0x20B, 3);
        far[0x3C..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(image_subsystem(&far), 0, "a PE offset past the bytes read");
        let full = headers(0x80, 0x20B, 3);
        assert_eq!(image_subsystem(&full[..0x80 + 24 + 69]), 0, "the subsystem field cut in half");
        assert_eq!(image_subsystem(&[]), 0);
    }

    /// The layout is shared by two binaries through a mapped section, so where each field lands is part
    /// of the contract. The 64-bit fields added in version 7 must sit on 8-byte boundaries (a hook of
    /// either bitness reads them with one load), every slot must keep the next one's counters aligned,
    /// and `covs` must start right where `pids` ends - padding the compiler inserted there would be a
    /// layout nobody wrote down.
    #[test]
    fn the_identity_fields_are_aligned_and_no_padding_hides_before_the_slots() {
        use core::mem::{offset_of, size_of};
        assert_eq!(offset_of!(Ctl, core_created) % 8, 0, "core_created is not 8-byte aligned");
        assert_eq!(offset_of!(Cov, created) % 8, 0, "Cov.created is not 8-byte aligned");
        assert_eq!(offset_of!(Cov, failed_channels) % 8, 0, "Cov.failed_channels is not 8-byte aligned");
        assert_eq!(offset_of!(Cov, timers_at_floor) % 8, 0, "Cov.timers_at_floor is not 8-byte aligned");
        assert_eq!(offset_of!(Cov, calls_without_storage) % 8, 0, "Cov.calls_without_storage is not 8-byte aligned");
        assert_eq!(offset_of!(Cov, exit_began_qpc) % 8, 0, "Cov.exit_began_qpc is not 8-byte aligned");
        assert_eq!(offset_of!(Cov, zone_not_read_again), offset_of!(Cov, exit_began_qpc) + 8, "Cov.zone_not_read_again moved");
        assert_eq!(size_of::<Cov>() % 8, 0, "a slot would misalign the next one");
        assert_eq!(size_of::<PendingLoad>(), 16, "a pending load is two words and a time");
        // The zone's two fields fill the header up to the first anchor, which a hook of either bitness
        // reads with one load (version 16 named the four bytes alignment had left there).
        assert_eq!(offset_of!(Ctl, zone_name), offset_of!(Ctl, tz_bias) + 4, "zone_name moved away from tz_bias");
        assert_eq!(offset_of!(Ctl, a_fake), offset_of!(Ctl, zone_name) + 4, "padding the layout does not name sits before a_fake");
        assert_eq!(offset_of!(Ctl, a_fake) % 8, 0, "a_fake is not 8-byte aligned");
        assert_eq!(
            offset_of!(Ctl, pending_loads),
            offset_of!(Ctl, covs) + size_of::<[Cov; MAX_COV_PIDS]>(),
            "padding the layout does not name sits between covs and pending_loads"
        );
        assert_eq!(
            offset_of!(Ctl, covs),
            offset_of!(Ctl, pids) + size_of::<[u32; MAX_COV_PIDS]>(),
            "padding the layout does not name sits between pids and covs"
        );
        assert_eq!(offset_of!(Ctl, ended) + 4, offset_of!(Ctl, _pad), "ended moved out of the header's tail");
    }

    /// A mark counts only inside the window, ends included, and a slot that was never marked never
    /// counts - whatever the window.
    #[test]
    fn an_exit_mark_counts_only_inside_the_guard_window() {
        assert_eq!(exit_began_in_window(150, 100, 200), Some(150));
        assert_eq!(exit_began_in_window(100, 100, 200), Some(100), "marked as the process was let go");
        assert_eq!(exit_began_in_window(200, 100, 200), Some(200), "marked as the window ended");
        assert_eq!(exit_began_in_window(99, 100, 200), None, "before the process was let go");
        assert_eq!(exit_began_in_window(201, 100, 200), None, "after the window - a process that ran on");
        assert_eq!(exit_began_in_window(0, 0, 200), None, "never marked");
        assert_eq!(exit_began_in_window(-5, -10, 200), None, "no real counter value is negative");
        let mut cov = zeroed_cov();
        unsafe {
            assert_eq!(read_exit_began(&cov), 0, "a fresh slot reads as not exiting");
            mark_exit_began(&mut cov, 0x1234_5678_9ABC);
            assert_eq!(read_exit_began(&cov), 0x1234_5678_9ABC);
        }
    }

    /// Milliseconds between two counter values, and 0 rather than a panic or a wrap for a span that runs
    /// backwards, a frequency of 0, or values at the ends of the type.
    #[test]
    fn a_counter_span_is_whole_milliseconds_and_never_wraps() {
        assert_eq!(qpc_span_ms(1_000, 11_000, 10_000_000), 1, "10 000 ticks at 10 MHz");
        assert_eq!(qpc_span_ms(0, 2_999_999, 10_000_000), 299, "rounded down");
        assert_eq!(qpc_span_ms(5, 5, 10_000_000), 0);
        assert_eq!(qpc_span_ms(10, 5, 10_000_000), 0, "backwards");
        assert_eq!(qpc_span_ms(0, 10, 0), 0, "no frequency");
        assert_eq!(qpc_span_ms(0, 10, -1), 0, "a negative frequency");
        assert_eq!(qpc_span_ms(i64::MIN, i64::MAX, 1), u64::MAX, "past u64 is held at its top");
    }

    /// The gate's protocol, one step at a time: a pending load joins once, an abandoned one is refused
    /// once and freed by the refusal, the parent cannot abandon a load that joined, and a closed gate
    /// names nobody.
    #[test]
    fn a_load_joins_or_is_abandoned_and_never_both() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            assert_eq!(pass_load_gate(p, 7, 70), LoadGate::NotPending, "no gate opened yet");
            let joined = open_load_gate(p, 7, 70).expect("a gate for the first child");
            assert_eq!(pass_load_gate(p, 7, 71), LoadGate::NotPending, "another process under the same pid");
            assert_eq!(pass_load_gate(p, 8, 70), LoadGate::NotPending, "another pid");
            assert_eq!(pass_load_gate(p, 7, 70), LoadGate::Joined);
            assert!(!abandon_load_gate(p, joined), "a load whose hook joined cannot be abandoned");
            close_load_gate(p, joined);
            assert_eq!(pass_load_gate(p, 7, 70), LoadGate::NotPending, "a closed gate names nobody");

            let late = open_load_gate(p, 9, 90).expect("a gate for the second child");
            assert!(abandon_load_gate(p, late), "a pending load can be abandoned");
            assert!(!abandon_load_gate(p, late), "and only once");
            assert_eq!(pass_load_gate(p, 9, 90), LoadGate::Refused, "the late hook refuses");
            assert_eq!(pass_load_gate(p, 9, 90), LoadGate::NotPending, "the refusal freed the entry");

            assert_eq!(open_load_gate(p, 0, 90), None, "a child without a pid is not named");
            assert_eq!(open_load_gate(p, 9, 0), None, "nor one without a creation time");
            assert_eq!(pass_load_gate(p, 0, 0), LoadGate::NotPending);
            assert!(!abandon_load_gate(p, PENDING_LOADS_MAX), "an index past the table is no gate");
        }
    }

    /// A full table opens no gate - the load then goes ahead without one - and an entry closed or
    /// refused is taken again.
    #[test]
    fn a_full_table_opens_no_gate_until_an_entry_comes_back() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            let gates: Vec<usize> = (1..=PENDING_LOADS_MAX as u32)
                .map(|pid| open_load_gate(p, pid, u64::from(pid) * 10).expect("a free entry"))
                .collect();
            assert_eq!(open_load_gate(p, 9_999, 1), None, "every entry is taken");
            close_load_gate(p, gates[3]);
            assert_eq!(open_load_gate(p, 9_999, 1), Some(gates[3]), "a closed entry is taken again");
            assert!(abandon_load_gate(p, gates[5]));
            assert_eq!(open_load_gate(p, 9_998, 1), None, "an abandoned entry waits for its hook");
            assert_eq!(pass_load_gate(p, 6, 60), LoadGate::Refused);
            assert_eq!(open_load_gate(p, 9_998, 1), Some(gates[5]), "and is taken again once refused");
        }
    }

    /// The identity fields round-trip, and the end mark is one-way until a reclaim zeroes the block.
    #[test]
    fn the_session_identity_and_its_end_round_trip() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        let mut cov = zeroed_cov();
        unsafe {
            assert!(!read_ended(p), "a fresh block reads as ended");
            write_core_created(p, 0x01DC_0000_1234_5678);
            write_core_pid(p, 4242);
            mark_ended(p);
            assert_eq!(read_core_created(p), 0x01DC_0000_1234_5678);
            assert_eq!(read_core_pid(p), 4242);
            assert!(read_ended(p));
            assert_eq!(read_created(&cov), 0, "an unrecorded slot must read as 0, the fallback mark");
            set_created(&mut cov, 0x01DC_0000_0000_0001);
            assert_eq!(read_created(&cov), 0x01DC_0000_0000_0001);
        }
    }

    fn zeroed_cov() -> Cov {
        Cov::ZEROED
    }

    /// The name `Local\ChronoCtl` is fixed and anyone in the session can create it, so
    /// `ERROR_ALREADY_EXISTS` alone never meant "our orphan". A section the OS just created is
    /// zero-filled, which is exactly the state a bare name-squat leaves behind - and the state this
    /// rejects, so the mechanism refuses the block instead of zeroing it and running the session on
    /// a mapping somebody else keeps a handle to.
    #[test]
    fn an_unstamped_block_is_not_recognised_as_ours() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            assert!(!header_is_ours(p), "a zero-filled section passed as ours");
            write_header(p);
            assert!(header_is_ours(p), "our own stamp was not recognised");
        }
    }

    /// Two ways a block can be present and still not be one we may read a clock out of: written by
    /// something else entirely, or by a build whose fields sit at different offsets. The second is
    /// the reason the version is checked at all - the mechanism and the hook ship together, but a
    /// half-updated folder does happen, and reading the wrong offsets yields a plausible-looking
    /// number rather than an error (untouchable rules 2 and 4).
    #[test]
    fn a_foreign_magic_or_a_different_layout_is_refused() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            write_header(p);

            write_volatile(addr_of_mut!((*p).magic), CTL_MAGIC ^ 1);
            assert!(!header_is_ours(p), "a foreign magic passed as ours");

            write_volatile(addr_of_mut!((*p).magic), CTL_MAGIC);
            write_volatile(addr_of_mut!((*p).layout), CTL_LAYOUT_VERSION + 1);
            assert!(!header_is_ours(p), "another layout version passed as ours");
        }
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
            write_anchor_full(
                p,
                &FullAnchor {
                    a_fake: 134_000_000_000_000_000,
                    a_real: 42,
                    multiplier: 60,
                    dur_tick_offset: 5_000,
                    dur_quit_c0: 42,
                    dur_q0: 42,
                    dur_qpc_c0: 700,
                    dur_qpc_q0: 700,
                },
            );
            let (af, ar, m) = read_anchor(p);
            assert_eq!((af, ar, m), (134_000_000_000_000_000, 42, 60));
            assert_eq!(read_dur(p), DurAnchor { tick_offset: 5_000, quit_c0: 42, q0: 42, m: 60 });
            assert_eq!(read_qpc(p), QpcAnchor { c0: 700, q0: 700, m: 60 });

            // write_anchor (the jump writer) moves the wall clock but must NOT touch the duration anchor.
            write_anchor(p, 999, 77, 60);
            let (af2, ar2, _) = read_anchor(p);
            assert_eq!((af2, ar2), (999, 77));
            let d = read_dur(p);
            assert_eq!((d.tick_offset, d.quit_c0, d.q0), (5_000, 42, 42), "jump must leave the duration axis alone");
            let q = read_qpc(p);
            assert_eq!((q.c0, q.q0), (700, 700), "jump must leave the QPC axis alone");
        }
    }

    /// A session's starting anchor at rate `m`: both duration axes start where the real clocks stand
    /// (`q0` for QUIT, `qpc_q0` for QPC), the tick count `offset` above QUIT in milliseconds.
    fn started(m: i64, offset: i64, q0: i64, qpc_q0: i64) -> Box<Ctl> {
        let mut ctl = zeroed_ctl();
        let start = FullAnchor {
            a_fake: 0,
            a_real: q0,
            multiplier: m,
            dur_tick_offset: offset,
            dur_quit_c0: q0,
            dur_q0: q0,
            dur_qpc_c0: qpc_q0,
            dur_qpc_q0: qpc_q0,
        };
        unsafe { write_anchor_full(&mut *ctl, &start) };
        ctl
    }

    #[test]
    fn duration_axis_never_rewinds_across_multiplier_changes() {
        // H-1 regression guard: a multiplier change must re-anchor the duration axis so it stays
        // continuous, never dips (untouchable rule 3). Drives `rebase` itself against a rising real
        // clock, sampling GetTickCount64 and QUIT densely.
        let q0 = 10_000_000i64; // real QUIT base, 100 ns
        let mut ctl = started(60, 0, q0, q0);
        let p = &mut *ctl as *mut Ctl;
        let at = |now: i64| {
            let d = unsafe { read_dur(p) };
            (d.tick_at(now, 0), d.quit_at(now, 0))
        };

        // Real QUIT (100 ns) advances by 1 ms each sample, off the millisecond grid. The multiplier
        // drops (x60 -> x10 -> freeze -> x1) then jumps back up (-> x1440) - the down-steps are the ones
        // that used to rewind.
        let changes: &[(i64, i64)] = &[(500, 10), (900, 0), (1300, 1), (1700, 1440)]; // (sample index, new m)
        let mut last = at(q0);
        let mut change_i = 0;
        for step in 0..2_500i64 {
            let now = q0 + step * 10_000 + 3_333;
            if change_i < changes.len() && step == changes[change_i].0 {
                let before = at(now);
                let done = unsafe { rebase(p, changes[change_i].1, || now, || now) };
                assert_eq!(done.now, now);
                // Continuity: the value right after the switch equals the value right before it.
                assert_eq!(at(now), before, "an axis jumped at the switch to x{}", changes[change_i].1);
                change_i += 1;
            }
            let value = at(now);
            assert!(value.0 >= last.0, "GetTickCount64 rewound: {} -> {} at step {step}", last.0, value.0);
            assert!(value.1 >= last.1, "QUIT rewound: {} -> {} at step {step}", last.1, value.1);
            last = value;
        }

        // Frozen wall (M = 0) still advances the axis at real speed (clamp to >= 1).
        let a = dur_tick_at(0, 1_000, 0, 0, 5_000_000);
        let b = dur_tick_at(0, 1_000, 0, 0, 6_000_000);
        assert!(b > a, "a frozen wall clock must not stop the monotonic duration axis (rule 3)");
    }

    /// The tick count follows QUIT, so an anchor rewritten at the rate it already had moves no axis at all
    /// (R4/10a, F4). The tick count used to have a base of its own, rounded down to a whole millisecond
    /// at every rewrite - measured as steps back of 1 ms with no change of rate, and a count that fell
    /// behind QUIT a little each time.
    #[test]
    fn rewriting_the_anchor_at_the_same_rate_moves_no_axis() {
        let q0 = 10_000_000i64;
        let straight = started(1440, 7, q0, q0);
        let mut ctl = started(1440, 7, q0, q0);
        let p = &mut *ctl as *mut Ctl;
        for i in 1..=1_000i64 {
            let now = q0 + i * 12_345; // 1.2345 ms apart, never on the millisecond grid
            unsafe { rebase(p, 1440, || now, || now) };
            let (d, s) = unsafe { (read_dur(p), read_dur(&*straight)) };
            let (q, sq) = unsafe { (read_qpc(p), read_qpc(&*straight)) };
            for later in [now, now + 4_321] {
                assert_eq!(d.tick_at(later, 0), s.tick_at(later, 0), "the tick count drifted after {i} rewrites");
                assert_eq!(d.quit_at(later, 0), s.quit_at(later, 0), "QUIT drifted after {i} rewrites");
                assert_eq!(q.at(later, 0), sq.at(later, 0), "QPC drifted after {i} rewrites");
            }
        }
    }

    /// A rate change reads the instant it is made at while its write is open (R4-S3, F3): every reader
    /// of the old anchor took its clock before that instant, every reader of the new one after it.
    #[test]
    fn a_rate_change_reads_its_clocks_while_its_write_is_open() {
        let mut ctl = started(1440, 0, 1_000, 2_000);
        let p = &mut *ctl as *mut Ctl;
        let open = || unsafe { read_volatile(addr_of!((*p).seq)) } & 1 == 1;
        let seen = std::cell::Cell::new((false, false));
        let done = unsafe {
            rebase(
                p,
                60,
                || {
                    seen.set((open(), seen.get().1));
                    5_000
                },
                || {
                    seen.set((seen.get().0, open()));
                    7_000
                },
            )
        };
        assert_eq!(seen.get(), (true, true), "a clock was read outside the write");
        assert_eq!(done, Rebased { now: 5_000, old_m: 1440 });
        assert_eq!(ctl.seq & 1, 0, "the write was left open");
        let d = unsafe { read_dur(&*ctl) };
        assert_eq!((d.q0, d.m, d.quit_c0), (5_000, 60, 1_000 + 4_000 * 1440));
        let q = unsafe { read_qpc(&*ctl) };
        assert_eq!((q.q0, q.c0), (7_000, 2_000 + 5_000 * 1440));
    }

    /// A reader takes its clock inside the window it validates (R4-S3). The clock here IS a rate change
    /// landing while the reader holds the old anchor: the first read of it runs a whole `rebase` and then
    /// answers an instant after it. Read inside the window, the reader sees `seq` move, reads again, and
    /// returns the NEW anchor with a clock taken after it. Read after the window, it returned the OLD
    /// anchor with that later clock, which is the reader that stepped the axis back.
    #[test]
    fn a_reader_takes_its_clock_inside_the_window_it_validates() {
        for axis in ["dur", "qpc", "wall"] {
            let mut ctl = started(1440, 0, 1_000, 1_000);
            let p = &mut *ctl as *mut Ctl;
            let reads = std::cell::Cell::new(0i64);
            let clock = || {
                let n = reads.get();
                reads.set(n + 1);
                if n == 0 {
                    unsafe { rebase(p, 1, || 2_000, || 2_000) };
                }
                2_000 + n
            };
            let (rate, base, real) = unsafe {
                match axis {
                    "dur" => {
                        let (d, real) = read_dur_with(p, clock);
                        (d.m, d.q0, real)
                    }
                    "qpc" => {
                        let (q, real) = read_qpc_with(p, clock);
                        (q.m, q.q0, real)
                    }
                    _ => {
                        let ((_, a_real, m), real) = read_anchor_with(p, clock);
                        (m, a_real, real)
                    }
                }
            };
            assert_eq!(rate, 1, "{axis}: the reader kept the anchor the write replaced");
            assert_eq!(base, 2_000, "{axis}");
            assert!(real >= base, "{axis}: a clock from before the anchor it was paired with");
        }
    }

    /// A clock read before its anchor answers the anchor's base, never less (R4-N4). A reader whose clock
    /// is the time-stamp counter can hold one a few nanoseconds older than the anchor it read, and a
    /// negative elapsed time used to put the answer under the base - for the tick count, cast to
    /// unsigned, at the very top of its range.
    #[test]
    fn a_clock_read_before_its_anchor_answers_the_base() {
        assert_eq!(dur_quit_at(500, 1_000, 60, 999), 500);
        assert_eq!(dur_qpc_at(500, 1_000, 60, 990), 500);
        assert_eq!(dur_tick_at(3, 50_000, 60_000, 60, 59_999), 8, "5 ms of QUIT plus the offset of 3");
        assert!(!dur_axis_at_range_end(0, 1_000, i64::MAX, 0), "an elapsed time below zero is no overflow");
        assert_eq!(tick_from_quit(-10, 0), 0, "held at zero rather than wrapped to the top");
    }

    #[test]
    fn releasing_a_process_neither_rewinds_nor_stops_its_duration_axes() {
        // The measured case (2026-09-24): x60, the session ends after 5.4 s of real time and the target
        // lives on. Before the release the axes ran at the session rate, after it they must carry on
        // from the same value at the real rate. The real value itself stands 5.4 s x 59 = 318.6 s behind
        // by then (the measured run lasted 5.36 s, hence the 316 s on record).
        // QUIT starts at 1 000 000 ms, so with an offset of 0 the tick count starts there too.
        let quit_c0: i64 = 10_000_000_000; // 100 ns
        let q0: i64 = 10_000_000_000; // real QUIT base, 100 ns
        let qpc_c0: i64 = 50_000_000; // raw ticks, a 10 MHz counter
        let qpc_q0: i64 = 50_000_000;
        let end = q0 + 54_000_000; // 5.4 s later
        let end_qpc = qpc_q0 + 54_000_000;
        let dur = |m: i64| DurAnchor { tick_offset: 0, quit_c0, q0, m };
        let qpc = |m: i64| QpcAnchor { c0: qpc_c0, q0: qpc_q0, m };
        for m in [60i64, 1, 0, 1440] {
            let r = release_axes(dur(m), qpc(m), end, end_qpc);
            assert_eq!(r.tick_at(end), dur_tick_at(0, quit_c0, q0, m, end), "tick jumped at the release (x{m})");
            assert_eq!(r.quit_at(end), dur_quit_at(quit_c0, q0, m, end), "quit jumped at the release (x{m})");
            assert_eq!(
                r.qpc_at(end_qpc),
                dur_qpc_at(qpc_c0, qpc_q0, m, end_qpc),
                "qpc jumped at the release (x{m})"
            );
            // Rate 1 afterwards: one real second is one second on every axis, whatever the session ran at.
            assert_eq!(r.tick_at(end + 10_000_000) - r.tick_at(end), 1_000, "tick rate after the release (x{m})");
            assert_eq!(r.quit_at(end + 10_000_000) - r.quit_at(end), 10_000_000, "quit rate after the release (x{m})");
            assert_eq!(
                r.qpc_at(end_qpc + 10_000_000) - r.qpc_at(end_qpc),
                10_000_000,
                "qpc rate after the release (x{m})"
            );
            // Before the instant it is the session's own line, so a detour that answered from the
            // session just before the release answered what the release answers (R4/10a, F6).
            assert_eq!(r.quit_at(end - 1), dur_quit_at(quit_c0, q0, m, end - 1), "quit before the release (x{m})");
            assert_eq!(r.qpc_at(end_qpc - 1), dur_qpc_at(qpc_c0, qpc_q0, m, end_qpc - 1), "qpc before the release (x{m})");
        }

        // Dense sampling across the release at x60, 1 ms of real time a step: never a step down. The
        // value comes from the line a detour computes for itself out of its own snapshot (the anchor and
        // the published instant), and the watcher's line has to agree with it at every step.
        let r = release_axes(dur(60), qpc(60), end, end_qpc);
        let mut last_tick = 0u64;
        let mut last_quit = i64::MIN;
        let mut last_qpc = i64::MIN;
        for step in 0..10_800i64 {
            let now = q0 + step * 10_000;
            let now_qpc = qpc_q0 + step * 10_000;
            let (tick, quit, qpc_value) = (dur(60).tick_at(now, end), dur(60).quit_at(now, end), qpc(60).at(now_qpc, end_qpc));
            assert_eq!((tick, quit, qpc_value), (r.tick_at(now), r.quit_at(now), r.qpc_at(now_qpc)), "a detour and the watcher disagree (step {step})");
            assert!(tick >= last_tick, "GetTickCount64 rewound at the release: {last_tick} -> {tick} (step {step})");
            assert!(quit >= last_quit, "QUIT rewound at the release: {last_quit} -> {quit} (step {step})");
            assert!(qpc_value >= last_qpc, "QPC rewound at the release: {last_qpc} -> {qpc_value} (step {step})");
            (last_tick, last_quit, last_qpc) = (tick, quit, qpc_value);
        }
        // What the old behaviour handed back instead: the base started at the real tick, so the real
        // value at the end is the base plus 5.4 s, which is 318.6 s under what the target had just read.
        let real_at_end = 1_000_000 + 5_400;
        assert_eq!(r.tick_at(end) - real_at_end, 318_600, "the gap the release closes (5.4 s x 59)");

        // An axis already standing at the end of its range stays where it stood rather than wrapping or
        // dropping (see `dur_axis_at_range_end`, which the session warns on). The tick count follows QUIT
        // since R4/10a, so it stands there with QUIT instead of moving on alone at rate 1 as it used to.
        let far = release_axes(
            DurAnchor { tick_offset: 0, quit_c0, q0: 0, m: i64::MAX },
            QpcAnchor { c0: qpc_c0, q0: 0, m: i64::MAX },
            end,
            end_qpc,
        );
        assert!(far.tick_at(end + 10_000_000) >= far.tick_at(end), "a saturated tick axis must not drop after the release");
        assert!(far.quit_at(end + 10_000_000) >= far.quit_at(end), "a saturated quit axis must not drop after the release");

        // The margins: a tenth of a second on both clocks, and never no margin at all.
        assert_eq!(RELEASE_MARGIN_QUIT, 1_000_000);
        assert_eq!(release_margin_qpc(10_000_000), 1_000_000);
        assert_eq!(release_margin_qpc(0), 1);
    }

    #[test]
    fn qpc_axis_never_rewinds_across_multiplier_changes() {
        // ADR-2 reversal (A1): a multiplier change must re-anchor the QPC axis so it stays continuous,
        // never dips (rule 3) - exactly what the spike's lazy anchor got WRONG. Drives `rebase` against a
        // rising real QPC, with the multiplier dropping then jumping up.
        let q0 = 5_000_000i64; // real QPC base, raw ticks
        let mut ctl = started(60, 0, 1_000, q0);
        let p = &mut *ctl as *mut Ctl;
        let at = |now: i64| unsafe { read_qpc(p) }.at(now, 0);
        let changes: &[(i64, i64)] = &[(500, 10), (900, 0), (1300, 1), (1700, 1440)];
        let mut last = at(q0);
        let mut ci = 0;
        for step in 0..2_500i64 {
            let now = q0 + step * 100; // real QPC advances 100 ticks per step
            if ci < changes.len() && step == changes[ci].0 {
                let before = at(now);
                unsafe { rebase(p, changes[ci].1, || 1_000, || now) };
                assert_eq!(at(now), before, "qpc jumped at the switch");
                ci += 1;
            }
            let qpc = at(now);
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

    /// A zeroed block is the zone's own name, which is what every block reads before the mechanism
    /// writes the field, and the field does not reach into the bias beside it.
    #[test]
    fn zone_name_round_trips_beside_the_bias() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            assert_eq!(read_zone_name(p), zone_name::OWN_ZONE_NAME);
            write_tz_bias(p, -330);
            write_zone_name(p, 4);
            assert_eq!(read_zone_name(p), 4);
            assert_eq!(read_tz_bias(p), -330);
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
            // Three distinct slots, in order, readable back - the rest stay zero.
            assert_eq!(read_pid(p, 0), 1111);
            assert_eq!(read_pid(p, 1), 2222);
            assert_eq!(read_pid(p, 2), 3333);
            assert_eq!(read_pid(p, 3), 0);
        }
    }

    /// A core killed in the middle of a rate change leaves the anchor half written, and the watcher then
    /// releases the axes from whatever stands there (R4-N5). Every prefix of the stores, for a slowdown
    /// and a speed-up alike, must read no earlier on any of the three duration axes than the axis read
    /// when the write began - and the complete write must be continuous with it.
    #[test]
    fn every_prefix_of_a_rate_change_keeps_the_duration_axes_from_running_back() {
        const Q0: i64 = 1_000_000_000;
        const QPC_Q0: i64 = 2_000_000_000;
        const T: i64 = Q0 + 36_000_000_000; // an hour of real 100 ns units after the last anchor
        const QPC_T: i64 = QPC_Q0 + 36_000_000_000;
        for (old_m, new_m) in [(1440, 1), (1, 1440), (60, 2), (2, 60)] {
            let at = |c: &Ctl, t: i64, qt: i64| {
                let p = c as *const Ctl;
                let (d, q) = unsafe { (read_dur(p), read_qpc(p)) };
                (d.tick_at(t, 0), d.quit_at(t, 0), q.at(qt, 0))
            };
            let before = started(old_m, 5_000, Q0, QPC_Q0);
            let last = at(&before, T, QPC_T);
            // The fields the complete change ends with, exactly as `rebase` writes them.
            let mut after = started(old_m, 5_000, Q0, QPC_Q0);
            unsafe { rebase(&mut *after, new_m, || T, || QPC_T) };
            let stores = full_anchor_stores(&unsafe { full_anchor_fields(&*after) });
            for k in 0..=stores.len() {
                let mut ctl = started(old_m, 5_000, Q0, QPC_Q0);
                let p = &mut *ctl as *mut Ctl;
                unsafe {
                    for store in &stores[..k] {
                        store_anchor_field(p, *store);
                    }
                }
                for later in [0, 1, 10_000, 10_000_000] {
                    let now = at(&ctl, T + later, QPC_T + later);
                    assert!(
                        now.0 >= last.0 && now.1 >= last.1 && now.2 >= last.2,
                        "x{old_m} to x{new_m}, killed after {k} of {} stores ({:?}), {later} later: tick, quit, \
                         qpc {now:?} read earlier than {last:?}",
                        stores.len(),
                        stores.get(k.wrapping_sub(1))
                    );
                }
            }
            assert_eq!(at(&after, T, QPC_T), last, "x{old_m} to x{new_m}: the complete write is not continuous");
        }
    }

    /// A slot is found by its pid, and among slots with the same pid by its creation time - an earlier
    /// process that published the pid the system later gave a child is not that child (R4-S2). Unknown
    /// times rule nothing out, and pid 0, the value of every empty slot, is never found.
    #[test]
    fn a_slot_is_found_by_its_pid_and_its_creation_time() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        unsafe {
            for (pid, created) in [(700, 11), (800, 22), (700, 33), (900, 0)] {
                let slot = reserve_cov_slot(p).expect("a slot");
                set_created(cov_at_mut(p, slot), created);
                publish_pid(p, slot, pid);
            }
            assert_eq!(find_pid_slot(p, 800, None), Some(1));
            assert_eq!(find_pid_slot(p, 700, Some(33)), Some(2), "the later process under a reused pid");
            assert_eq!(find_pid_slot(p, 700, Some(11)), Some(0));
            assert_eq!(find_pid_slot(p, 700, Some(44)), None, "neither process under that pid");
            assert_eq!(find_pid_slot(p, 900, Some(55)), Some(3), "a slot that recorded no time is not ruled out");
            assert_eq!(find_pid_slot(p, 600, None), None, "never published");
            assert_eq!(find_pid_slot(p, 0, None), None, "an empty slot is nobody's");
        }
        assert!(same_creation(0, Some(5)) && same_creation(5, None) && same_creation(5, Some(5)));
        assert!(!same_creation(5, Some(6)));
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

    /// R5-WY11. The counter stops at the ceiling and never passes it, and a count at the ceiling reads as
    /// "at least".
    #[test]
    fn a_call_counter_stops_at_its_ceiling() {
        let mut cov = zeroed_cov();
        let p = &mut cov as *mut Cov;
        unsafe {
            let slot = (addr_of_mut!((*p).calls) as *mut u64).add(IDX_GSTAFT);
            write_volatile(slot, CALLS_CAP - 2);
            bump_calls(p, IDX_GSTAFT);
            assert_eq!(read_calls(p, IDX_GSTAFT), CALLS_CAP - 1);
            assert!(!calls_capped(read_calls(p, IDX_GSTAFT)), "one under the ceiling is an exact count");
            for _ in 0..5 {
                bump_calls(p, IDX_GSTAFT);
            }
            assert_eq!(read_calls(p, IDX_GSTAFT), CALLS_CAP, "the counter went past its ceiling");
            assert!(calls_capped(read_calls(p, IDX_GSTAFT)));
            // The neighbours are untouched.
            assert_eq!(read_calls(p, IDX_GSTPAFT), 0);
        }
    }

    /// R5-WY12, R5-D36. A write that stays open is waited for once: the reader that waited the whole bound
    /// remembers it, and the readers after it read at once instead of waiting the bound again. Before, every
    /// clock call paid the whole bound for as long as the core stood still in the middle of a write.
    #[test]
    fn a_write_that_stays_open_is_waited_for_once() {
        let mut ctl = zeroed_ctl();
        let p = &mut *ctl as *mut Ctl;
        // An odd value no other test's block reaches, so the process-wide memory is this test's alone.
        unsafe { write_volatile(addr_of_mut!((*p).seq), 0xFFFF_FFF1) };
        let reads = std::cell::Cell::new(0u32);
        let read = || {
            reads.set(reads.get() + 1);
            7
        };

        let started = std::time::Instant::now();
        assert_eq!(unsafe { read_consistent(p, read) }, 7, "a stopped write still answers from the fields");
        let first = started.elapsed();
        assert_eq!(reads.get(), 1, "the fields were read once, after the wait");

        // The next readers do not wait the bound again. The fastest of five, so one preemption cannot
        // fail the test, against a tenth of the first wait.
        let mut fastest = std::time::Duration::MAX;
        for _ in 0..5 {
            let started = std::time::Instant::now();
            assert_eq!(unsafe { read_consistent(p, read) }, 7);
            fastest = fastest.min(started.elapsed());
        }
        assert!(fastest * 10 < first, "a reader waited the bound again: {fastest:?} after a first wait of {first:?}");
        assert_eq!(reads.get(), 6);

        // The write finishes: readers are back on the seqlock, and the memory does not stand in the way.
        unsafe { write_volatile(addr_of_mut!((*p).seq), 0xFFFF_FFF2) };
        assert_eq!(unsafe { read_consistent(p, read) }, 7);
        assert_eq!(reads.get(), 7);
    }

    /// The sleep bias is taken from a triple whose two QUIT readings agree, so no timer interrupt fell
    /// between them and the difference is exact (R5-F4, R5-P1).
    #[test]
    fn the_sleep_bias_is_read_between_two_agreeing_quit_readings() {
        use std::cell::RefCell;
        use std::collections::VecDeque;
        let run = |quits: &[i64], interrupts: &[i64]| {
            let q = RefCell::new(quits.iter().copied().collect::<VecDeque<i64>>());
            let i = RefCell::new(interrupts.iter().copied().collect::<VecDeque<i64>>());
            sleep_bias(|| q.borrow_mut().pop_front().expect("more QUIT reads than scripted"), || {
                i.borrow_mut().pop_front().expect("more interrupt-time reads than scripted")
            })
        };
        // Agreeing on the first try.
        assert_eq!(run(&[1_000, 1_000], &[51_000]), 50_000);
        // An interrupt fell between the reads, the second try agrees: the scattered first pair is not used.
        assert_eq!(run(&[1_000, 1_100, 1_100, 1_100], &[51_150, 51_100]), 50_000);
        // Never agreeing: the lower bound from the last try, never above the real bias.
        let q: Vec<i64> = (0..8).map(|k| 1_000 + k * 100).collect();
        assert_eq!(run(&q, &[51_050, 51_250, 51_450, 51_650]), 51_650 - 1_700);
        // Never below zero.
        assert_eq!(run(&[1_000, 1_000], &[900]), 0);
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
            (IDX_QPC, CH_QPC),
            (IDX_TIMEGETTIME, CH_TIMEGETTIME),
            (IDX_SCVSRW, CH_SCVSRW),
            (IDX_SCVCS, CH_SCVCS),
            (IDX_WOA, CH_WOA),
            (IDX_WSAWFME, CH_WSAWFME),
            (IDX_QIT, CH_QIT),
            (IDX_QITP, CH_QITP),
            (IDX_QUITP, CH_QUITP),
            (IDX_GTZIFY, CH_GTZIFY),
        ];
        assert_eq!(expected.len(), CHANNEL_COUNT, "every channel is listed here, so a new one cannot skip this check");
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
        // The deadline channels are expected in EVERY session (R5-D2), which the audit can only do for a
        // module every process has: an optional one that is absent would read as a failed hook.
        let deadlines: Vec<_> = CHANNELS.iter().filter(|ch| DEADLINE_CHANNELS & ch.bit != 0).collect();
        assert_eq!(deadlines.len(), 5);
        for ch in deadlines {
            assert_eq!(ch.category, ChannelCategory::Duration, "{}", ch.name);
            assert!(ch.module.in_every_process(), "{} is in a module a process may not load", ch.name);
        }
    }

    /// R5-A11: a module has a DLL name to be looked up by exactly when it may be missing at `DllMain`, and
    /// the late set read off the table is the six channels it was written out as by hand - the message
    /// waits and the window timer in user32, the multimedia timer and clock in winmm, the socket wait.
    #[test]
    fn the_late_channels_are_read_off_the_table_and_their_modules_have_a_name() {
        use ChannelModule::*;
        for module in [Kernel32, Ntdll, User32, Winmm, Ws2_32, KernelBase, KernelBaseBehindKernel32, KernelBaseAndKernel32] {
            assert_eq!(module.late_dll().is_some(), !module.in_every_process(), "{module:?}");
            if let Some(name) = module.late_dll() {
                assert_eq!(name.iter().filter(|b| **b == 0).count(), 1, "{module:?}");
                assert_eq!(name.last(), Some(&0), "{module:?} is not NUL-terminated");
            }
        }
        assert_eq!(LATE_CHANNELS, CH_MWFMO | CH_MWFMOEX | CH_SETTIMER | CH_TIMESETEVENT | CH_TIMEGETTIME | CH_WSAWFME);
    }

    /// The hook resolves every channel by `export`, and the report names it by `name`. The two differ
    /// for the connection observer alone, which lives in ntdll - where no `connect` export exists, so
    /// resolving it by its name would leave the channel quietly not installed.
    #[test]
    fn every_channel_detours_its_own_name_except_the_connection_observer() {
        for ch in CHANNELS {
            if ch.bit == CH_CONNECT {
                assert_eq!(ch.export(), "NtDeviceIoControlFile");
                assert_eq!(ch.name, "connect", "the name is a contract key");
                assert_eq!(ch.module, ChannelModule::Ntdll, "the export lives in ntdll");
            } else {
                assert_eq!(ch.export(), ch.name, "{} detours a different symbol than it names", ch.name);
            }
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
    }

    /// 🔴 Untouchable rule 3 says the duration clock never goes backwards. It did.
    ///
    /// The three axes multiplied elapsed real time by M with WRAPPING arithmetic, so at the maximum
    /// multiplier - about 10.6 days into a session - the product overflowed i64 and the axis jumped
    /// BACKWARDS by centuries in a single step. A monotonic clock that is not monotonic, on the axis
    /// .NET Stopwatch, Java nanoTime and Python perf_counter all read. The wall clock had had a clamp
    /// and a warning for this since R2-X2 - these three, which are the ones the rule is about, had
    /// neither.
    ///
    /// Saturating does not make the rule whole (a standing clock still is not advancing), and that is
    /// why the session WARNS. What it does is turn an invisible rewind into a visible stop.
    #[test]
    fn a_duration_axis_holds_at_the_end_of_its_range_instead_of_rewinding() {
        let m = 1_000_000; // MULTIPLIER_MAX
        // Far past the point where dq * M overflows an i64.
        let huge = i64::MAX / 2;

        let quit = dur_quit_at(0, 0, m, huge);
        assert_eq!(quit, i64::MAX, "held, not wrapped");
        // Non-decreasing, not increasing - which is what "held" means. Two readings taken while the
        // axis stands are EQUAL, and that is the honest shape: it is the wrap that has to be gone,
        // not the standing, which is why the session warns about the standing separately.
        assert!(quit >= dur_quit_at(0, 0, m, huge - 1_000_000), "never below an earlier reading");

        let qpc = dur_qpc_at(0, 0, m, huge);
        assert_eq!(qpc, i64::MAX);

        // The tick count is QUIT in milliseconds, so its saturation shows up as the largest millisecond
        // count a held QUIT expresses - still held, still never backwards.
        let tick = dur_tick_at(0, 0, 0, m, huge);
        assert!(tick >= dur_tick_at(0, 0, 0, m, huge - 1_000_000));

        // Monotonic across the boundary itself: sample either side of the overflow point and the
        // later reading is never the smaller one.
        let boundary = i64::MAX / m;
        let before = dur_quit_at(0, 0, m, boundary - 1);
        let after = dur_quit_at(0, 0, m, boundary + 1);
        assert!(after >= before, "before={before} after={after}");
    }

    /// The standing has to be REPORTABLE, or saturating just replaces a loud wrong answer with a
    /// quiet one. This is the predicate the session warns on.
    #[test]
    fn a_standing_duration_axis_can_be_detected() {
        let m = 1_000_000;
        assert!(dur_axis_at_range_end(0, 0, m, i64::MAX / 2));
        assert!(!dur_axis_at_range_end(0, 0, m, 10_000_000), "one second in, nowhere near it");
        // At the largest speed anyone has documented using, a session would have to run for decades.
        assert!(!dur_axis_at_range_end(0, 0, 1440, 10_000_000 * 86_400 * 365));
        // Frozen clamps the multiplier to 1, so the axis advances at real speed and never saturates
        // within any session length that can exist.
        assert!(!dur_axis_at_range_end(0, 0, 0, i64::MAX / 4));
    }

    /// R5-P17. A rate change banks the elapsed term into the base, so the base can reach the top while
    /// no single segment's term does - and the old question, about the term alone, said nothing then.
    #[test]
    fn a_duration_axis_standing_on_its_banked_base_is_detected_too() {
        let m = 1_000_000;
        // One day into the current segment: the term alone is nowhere near the top...
        let day: i64 = 10_000_000 * 86_400;
        assert!(day.checked_mul(m).is_some());
        // ...but the base banked from earlier segments already sits just under it.
        let banked = i64::MAX - day * m / 2;
        assert!(dur_axis_at_range_end(banked, 0, m, day), "a base at the top went unreported");
        assert!(!dur_axis_at_range_end(banked, 0, m, day / 4), "a quarter day in, there is room left");
        // The projection the target sees stands there, it does not wrap.
        assert_eq!(dur_quit_at(banked, 0, m, day), i64::MAX);
    }

    /// 🔴 A sub-M timeout used to come out as 0, and this test asserted that as "honest coarseness".
    /// It is not coarseness. `Sleep(0)` gives up the time slice and returns AT ONCE, so
    /// `while (!done) Sleep(100)` stopped sleeping under a large multiplier and started spinning a
    /// core flat out - a cost charged to the application under test, invisible, and easily read back
    /// as a property of that application. The timer family has had this floor all along.
    #[test]
    fn a_positive_timeout_never_becomes_a_yield() {
        assert_eq!(scale_wait(30, 60), MIN_SCALED_WAIT_MS);
        assert_eq!(scale_wait(100, 1_000), MIN_SCALED_WAIT_MS);
        assert_eq!(scale_wait(1, 1_000_000), MIN_SCALED_WAIT_MS);

        // A caller's OWN zero is still a yield - the floor is about what the scaling produces, never
        // about overriding what the application asked for.
        assert_eq!(scale_wait(0, 1_000), 0);
        assert_eq!(scale_wait(0xFFFF_FFFF, 1_000), 0xFFFF_FFFF);
    }

    /// The floor is a partial coverage, so it has to be reportable - the audit warns on it.
    #[test]
    fn the_floor_reports_itself_and_only_when_it_applies() {
        assert!(wait_hit_floor(30, 60), "30 ms at x60 is under a millisecond");
        assert!(!wait_hit_floor(6000, 60), "100 ms comes out exactly");
        assert!(!wait_hit_floor(60, 60), "exactly 1 ms is the floor, not below it");
        // Pass-through shapes are not clamped, so they are not reported either.
        assert!(!wait_hit_floor(0, 1_000));
        assert!(!wait_hit_floor(0xFFFF_FFFF, 1_000));
        // Frozen leaves waits real, so nothing is ever at the floor.
        assert!(!wait_hit_floor(30, 0));
    }

    /// Sleep and SleepEx bottom out on NtDelayExecution, so a floor on one and not the other would be
    /// a floor on neither - the scaled Sleep would re-enter here and be truncated back to a yield.
    #[test]
    fn a_relative_delay_never_becomes_a_yield_either() {
        // -3 ms at x60 would be -0.05 ms, which truncates to 0 - a yield.
        assert_eq!(scale_delay_interval(-30_000, 60), -MIN_SCALED_DELAY_TICKS);
        assert!(delay_hit_floor(-30_000, 60));

        // A delay that divides cleanly is untouched, and does not report.
        assert_eq!(scale_delay_interval(-6_000_000, 60), -100_000);
        assert!(!delay_hit_floor(-6_000_000, 60));

        // A positive interval is an absolute deadline: passed through, never floored or reported.
        assert_eq!(scale_delay_interval(1_000, 60), 1_000);
        assert!(!delay_hit_floor(1_000, 60));
        assert!(!delay_hit_floor(0, 60));
    }

    /// R5-F7. The floor exists so that scaling never turns a sleep into a yield, and it used to lengthen a
    /// delay the application asked for below it: `NtDelayExecution(-1)`, a hundred nanoseconds, became a
    /// whole millisecond at every speed, x1 included, and was counted as a wait the speed could not divide.
    #[test]
    fn the_delay_floor_never_lengthens_what_was_asked() {
        // A hundred nanoseconds stays a hundred nanoseconds, at x1 and at x60.
        assert_eq!(scale_delay_interval(-1, 1), -1);
        assert_eq!(scale_delay_interval(-1, 60), -1);
        // Half a millisecond: at x60 it cannot be divided without becoming a near-yield, so it stays as asked.
        assert_eq!(scale_delay_interval(-5_000, 60), -5_000);
        assert_eq!(scale_delay_interval(-5_000, 1), -5_000);
        // At x1 nothing is divided, so nothing is held, however short.
        assert!(!delay_hit_floor(-1, 1));
        assert!(!delay_hit_floor(-5_000, 1));
        assert!(!delay_hit_floor(-30_000, 1));
        // Above x1 a delay that could not be divided by the full speed is reported, short or not.
        assert!(delay_hit_floor(-5_000, 60));
        assert!(delay_hit_floor(-1, 60));
        // Exactly the floor at a speed that cannot divide it: held there, and said.
        assert_eq!(scale_delay_interval(-MIN_SCALED_DELAY_TICKS, 60), -MIN_SCALED_DELAY_TICKS);
        assert!(delay_hit_floor(-MIN_SCALED_DELAY_TICKS, 60));
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
        assert_eq!(scale_timer_due(-6_000_000, 0, 60, 60), -100_000);
        assert_eq!(scale_timer_due(-6_000_000, 0, 1, 1), -6_000_000);
        // Absolute (positive) fake instant -> relative real interval until the fake clock reaches it.
        // due = fake_now + 6s (60_000_000 ticks) ahead - at x60 the real wait is 0.1s (1_000_000 ticks).
        let fake_now = 1_000_000_000;
        assert_eq!(scale_timer_due(fake_now + 60_000_000, fake_now, 60, 60), -1_000_000);
        // Absolute already at or before fake_now: an instant long past, fired at once (R5-F12).
        assert_eq!(scale_timer_due(fake_now - 5, fake_now, 60, 60), 0);
        assert_eq!(scale_timer_due(fake_now, fake_now, 60, 60), 0);
        // Frozen and slow motion clamp to real length (>= 1): a relative due stays real...
        assert_eq!(scale_timer_due(-6_000_000, 0, 0, 0), -6_000_000);
        assert_eq!(scale_timer_due(-6_000_000, 0, -5, -5), -6_000_000);
        // ...and an absolute due under frozen behaves as M=1 (documented limit).
        assert_eq!(scale_timer_due(fake_now + 60_000_000, fake_now, 0, 1), -60_000_000);

        // R2-S5: `due` is the one input here that comes from the APPLICATION under test, and this
        // crate compiles with overflow-checks off, so a subtraction it cannot represent has to answer
        // rather than wrap. It hands the app's own value straight back, unscaled - the same safe
        // default the detours use when they cannot express something.
        assert_eq!(scale_timer_due(i64::MAX, -1, 60, 60), i64::MAX);
        assert_eq!(scale_timer_due(i64::MAX, i64::MIN, 60, 60), i64::MAX);
        // And the ordinary large-but-representable case still scales.
        assert_eq!(scale_timer_due(i64::MAX, 0, 1, 1), -i64::MAX);
    }

    /// R5-D2. A session without `--scale-duration` still runs its wall clock at the session's speed, so a
    /// due on that clock is divided by it, while a LENGTH of time is left as the application asked - the
    /// duration axis is the opt-in. Before, both shapes went to the kernel untouched in such a session, and
    /// a timer set for "tomorrow 03:00" on a clock at 2038 waited twelve real years.
    #[test]
    fn an_absolute_due_follows_the_wall_and_a_relative_one_the_duration_axis() {
        let fake_now = 1_000_000_000;
        // No --scale-duration (duration rate 1), wall at x60: the absolute due is divided...
        assert_eq!(scale_timer_due(fake_now + 60_000_000, fake_now, 60, 1), -1_000_000);
        // ...and the relative one is not.
        assert_eq!(scale_timer_due(-6_000_000, fake_now, 60, 1), -6_000_000);
        // At x1 the absolute due is still converted - a shifted DATE is enough to need it.
        assert_eq!(scale_timer_due(fake_now + 60_000_000, fake_now, 1, 1), -60_000_000);
        // A due in the past on the fake clock fires at once in either kind of session.
        assert_eq!(scale_timer_due(fake_now - 1, fake_now, 1, 1), 0);
    }

    /// R5-F12. A timer whose absolute due the session's clock has already passed fires at once, as the same
    /// timer fires without the session. It used to get `-1`, the shortest relative due, which the kernel
    /// fires only at its next timer tick: up to 15.6 ms of real time, 15.6 s of session time at x1000.
    #[test]
    fn a_due_already_reached_fires_at_once_and_not_at_the_next_tick() {
        let fake_now = 1_000_000_000;
        for (due, wall_m, dur_m) in [(fake_now, 1, 1), (fake_now - 1, 1000, 1), (fake_now - 10_000_000, 1000, 1000), (0, 60, 60)] {
            assert_eq!(scale_timer_due(due, fake_now, wall_m, dur_m), 0, "due {due} at x{wall_m}");
        }
    }

    /// R5-F12 and R5-D59. An `NtDelayExecution` deadline the session's clock has already reached returns at
    /// once, as it does without the session. A zero would not do it: to that call a zero is a yield,
    /// answered with another status, so the kernel gets an instant long past instead. A deadline still
    /// ahead is the delay until the wall reaches it, and what is not a deadline passes through.
    #[test]
    fn a_delay_deadline_already_reached_is_an_instant_long_past_and_never_a_yield() {
        let fake_now = 1_000_000_000;
        assert_eq!(scale_delay_deadline(fake_now, fake_now, 1), PAST_DEADLINE);
        assert_eq!(scale_delay_deadline(fake_now - 10_000_000, fake_now, 1000), PAST_DEADLINE);
        // Less than one real tick ahead at x60: nothing left to wait, and still not a yield.
        assert_eq!(scale_delay_deadline(fake_now + 59, fake_now, 60), PAST_DEADLINE);
        assert_eq!(scale_delay_deadline(fake_now + 60_000_000, fake_now, 60), -1_000_000);
        // A relative delay and a yield are not deadlines.
        assert_eq!(scale_delay_deadline(-10_000, fake_now, 60), -10_000);
        assert_eq!(scale_delay_deadline(0, fake_now, 60), 0);
        // A deadline the arithmetic cannot reach is handed back as it was, like the timers.
        assert_eq!(scale_delay_deadline(i64::MAX, -1, 60), i64::MAX);
    }

    #[test]
    fn scale_timer_period_scales_and_guards_edges() {
        // Positive period scaled by M.
        assert_eq!(scale_timer_period(6000, 60), 100);
        assert_eq!(scale_timer_period(6000, 1), 6000);
        // A >0 period never collapses to 0 (that would turn a periodic timer one-shot).
        assert_eq!(scale_timer_period(30, 60), 1);
        // 0 = one-shot stays 0 - negative (API error) passes through untouched.
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

    /// A timer counts as held at a floor exactly when the scaling could not give it the session's
    /// rate (R5, D8): a `SetTimer` interval asked at or above Windows' minimum that lands under it, and
    /// a positive period divided below 1 ms. Each half pinned against the scaling it describes.
    #[test]
    fn a_timer_counts_as_floored_only_when_the_scaling_could_not_reach_the_session_rate() {
        assert!(timer_elapse_hit_floor(100, 60), "100 ms at x60 is 1 ms, under the 10 ms Windows lifts it to");
        assert!(!timer_elapse_hit_floor(600, 60), "600 ms at x60 is exactly the minimum");
        assert!(timer_elapse_hit_floor(599, 60), "one below that is under it");
        assert!(!timer_elapse_hit_floor(5, 60), "an interval under the minimum sits there without a session too");
        assert!(!timer_elapse_hit_floor(0, 60), "zero asks for the minimum itself");
        assert!(!timer_elapse_hit_floor(100, 1), "real speed divides nothing");
        assert!(!timer_elapse_hit_floor(100, 0), "frozen leaves the interval real");

        assert!(timer_period_hit_floor(30, 60), "30 ms at x60 is held at 1 ms");
        assert_eq!(scale_timer_period(30, 60), 1, "the floor the check describes");
        assert!(!timer_period_hit_floor(60, 60), "exactly 1 ms is the floor, not below it");
        assert!(!timer_period_hit_floor(0, 60), "a one-shot passes through");
        assert!(!timer_period_hit_floor(-5, 60), "a rejected period passes through");
        assert!(timer_period_hit_floor(i64::from(u32::MAX), i64::MAX), "the unsigned form widens without loss");
        assert!(!timer_period_hit_floor(30, 0), "frozen leaves the period real");
    }

    #[test]
    fn scale_timer_elapse_divides_by_multiplier() {
        // Real interval = uElapse / M.
        assert_eq!(scale_timer_elapse(6000, 60), 100);
        assert_eq!(scale_timer_elapse(6000, 1), 6000);
        // Sub-M truncates toward 0 - Windows then clamps up to USER_TIMER_MINIMUM (not our job).
        assert_eq!(scale_timer_elapse(30, 60), 0);
        // No INFINITE guard, unlike scale_wait: a huge interval still scales.
        assert_eq!(scale_timer_elapse(0xFFFF_FFFF, 60), 0xFFFF_FFFF / 60);
        // Frozen and slow motion clamp to real length (>= 1) - rule 3.
        assert_eq!(scale_timer_elapse(6000, 0), 6000);
        assert_eq!(scale_timer_elapse(6000, -5), 6000);
    }

    /// The projection the target sees and the projection the session reports are now the same
    /// function - these pin its shape so a "small simplification" cannot quietly change what one
    /// side answers. The clamp is the load-bearing part: past the end of the range, wrapping gave
    /// the raw FILETIME channels one epoch and the SYSTEMTIME channels another (R2-K2, R2-X2).
    #[test]
    fn the_fake_wall_projection_scales_clamps_and_never_wraps() {
        // Plain acceleration: 1 s of real time at x60 is 60 s of fake time.
        let one_sec = 10_000_000;
        assert_eq!(fake_wall_at(1_000, 0, one_sec, 60), 1_000 + one_sec * 60);
        // Frozen (M = 0) holds the anchor exactly - the wall does not creep.
        assert_eq!(fake_wall_at(1_000, 0, one_sec * 3, 0), 1_000);
        // At the top of the range it holds at the clamp instead of wrapping to a negative year.
        assert_eq!(fake_wall_at(FAKE_WALL_MAX - 5, 0, one_sec, 1_000_000), FAKE_WALL_MAX);
        assert_eq!(fake_wall_at(i64::MAX, 0, one_sec, 1), FAKE_WALL_MAX);
        // A real clock that appears to run backwards (a wrapped QUIT reading) must not push the
        // fake clock past either clamp - and the LOW one matters as much as the high one, because a
        // negative FILETIME is not an early date, it is an unsigned number around year 59000 that
        // `FileTimeToSystemTime` rejects (measured at `--at 1000-01-01`, where the raw channels
        // showed nonsense and the SYSTEMTIME channels fell back to the real clock).
        let back = fake_wall_at(1_000, one_sec, 0, 60);
        assert!((0..=FAKE_WALL_MAX).contains(&back), "the fake clock left the representable range");
        assert_eq!(fake_wall_at(-5, 0, 0, 1), 0, "a negative anchor must not produce a negative clock");
    }

    /// R5-P2, R5-D31. The anchors stand on the precise clock and most channels read the plain one, which
    /// is behind it until the next timer interrupt. A plain reading taken right after an anchor is below
    /// the anchor's real instant, and the projection has to hold at the anchor rather than go under it - on
    /// a slowdown, under what the previous anchor already answered.
    #[test]
    fn a_clock_reading_behind_the_anchor_holds_at_the_anchor() {
        let tick = 15_625; // 1.5625 ms, a delayed interrupt
        assert_eq!(fake_wall_at(5_000_000, 1_000_000, 1_000_000 - tick, 1440), 5_000_000);
        assert_eq!(fake_wall_at(5_000_000, 1_000_000, 1_000_000 - tick, 1), 5_000_000);
        // A reading after the anchor runs on as before.
        assert_eq!(fake_wall_at(5_000_000, 1_000_000, 1_000_000 + tick, 60), 5_000_000 + tick * 60);

        // Across a slowdown, with the anchor's instant on the precise clock and a reader on the plain one,
        // nothing the new anchor answers is below what the old one answered.
        let (old_fake, old_real, old_m) = (0i64, 0i64, 1440i64);
        let switch_precise = 10_000_000; // the instant the rate change read, precise
        let new_fake = fake_wall_at(old_fake, old_real, switch_precise, old_m);
        let last_old = fake_wall_at(old_fake, old_real, switch_precise, old_m); // a precise reader at the switch
        for plain_after in [switch_precise - tick, switch_precise - 1, switch_precise, switch_precise + tick] {
            let first_new = fake_wall_at(new_fake, switch_precise, plain_after, 1);
            assert!(first_new >= last_old, "the wall stepped back after the slowdown: {first_new} < {last_old}");
        }
    }

    /// A fixed buffer, a terminator, and a caller that cannot check - the classic off-by-one. This
    /// code was correct and untested for its whole life because it lived in a crate with no tests.
    #[test]
    fn set_wide_always_terminates_and_never_overruns() {
        let mut buf = [0xFFFFu16; 8];
        set_wide(&mut buf, "abc");
        assert_eq!(&buf[..4], &['a' as u16, 'b' as u16, 'c' as u16, 0]);
        assert_eq!(buf[4], 0xFFFF, "wrote past the string it was given");

        // Longer than the buffer: truncated, still terminated, last slot never a character.
        let mut buf = [0xFFFFu16; 4];
        set_wide(&mut buf, "abcdefgh");
        assert_eq!(buf, ['a' as u16, 'b' as u16, 'c' as u16, 0]);

        // Exactly one slot: no room for a character, so it holds only the terminator.
        let mut buf = [0xFFFFu16; 1];
        set_wide(&mut buf, "abc");
        assert_eq!(buf, [0]);

        // Zero-length: nothing to write and nothing to overrun. The loop's `i + 1 >= dst.len()`
        // test is what makes this safe, and it is the case no caller can observe.
        let mut buf: [u16; 0] = [];
        set_wide(&mut buf, "abc");

        // Non-BMP input is UTF-16, so one character takes two slots - and truncation must still
        // leave room for the terminator.
        let mut buf = [0xFFFFu16; 3];
        set_wide(&mut buf, "\u{1F600}x");
        assert_eq!(buf[2], 0);
    }

    /// FILETIME 0 means "no time recorded" and is extremely common - a positive bias pushes it below
    /// zero, which read back as the unsigned value it is becomes a date tens of thousands of years
    /// out. `None` is what lets the detour report failure and leave the caller's value alone.
    #[test]
    fn a_bias_shift_that_leaves_the_filetime_range_is_refused() {
        let hour = 60 * 60 * 10_000_000i64;
        assert_eq!(shift_ticks_by_bias(hour * 5, 120, true), Some(hour * 5 + hour * 2));
        assert_eq!(shift_ticks_by_bias(hour * 5, 120, false), Some(hour * 3));
        // The common one: zero minus a positive bias is not a FILETIME.
        assert_eq!(shift_ticks_by_bias(0, 120, false), None);
        // Exactly zero is still a FILETIME, so it is allowed.
        assert_eq!(shift_ticks_by_bias(hour * 2, 120, false), Some(0));
        // An addition that would overflow i64 is refused rather than wrapped.
        assert_eq!(shift_ticks_by_bias(i64::MAX, 120, true), None);
    }

    /// Every entry below was read out of kernel32 on this machine (2026-09-23), so the stubs are the
    /// ones the hook will meet, and the three refusals are real kernel32 entries with code of their own.
    #[test]
    fn a_kernel32_stub_is_followed_to_its_slot_and_anything_else_is_left_alone() {
        // Fits either bitness, so the test compiles for the 32-bit target too.
        let entry = 0x7FF0_1000usize;

        // 64-bit, with the REX prefix (GetLocalTime) and without it (SetWaitableTimer). The slot is
        // relative to the END of the instruction, which is seven bytes long in one and six in the other.
        let rex = [0x48, 0xFF, 0x25, 0x10, 0x00, 0x00, 0x00, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC];
        assert_eq!(indirect_jump_slot(&rex, entry, true), Some(entry + 7 + 0x10));
        let plain = [0xFF, 0x25, 0x9A, 0x00, 0x00, 0x00, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC];
        assert_eq!(indirect_jump_slot(&plain, entry, true), Some(entry + 6 + 0x9A));
        // The displacement is signed: an import table BEFORE the stub is a negative one.
        let back = [0xFF, 0x25, 0xF0, 0xFF, 0xFF, 0xFF, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC];
        assert_eq!(indirect_jump_slot(&back, entry, true), Some(entry + 6 - 0x10));

        // 32-bit plain (Sleep) and behind the hot-patch prologue (QueryUnbiasedInterruptTime): the
        // operand is the slot's absolute address, whatever the entry.
        let x86 = [0xFF, 0x25, 0xD0, 0x19, 0xB8, 0x75, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC];
        assert_eq!(indirect_jump_slot(&x86, 0x7650_0000, false), Some(0x75B8_19D0));
        let hot = [0x8B, 0xFF, 0x55, 0x8B, 0xEC, 0x5D, 0xFF, 0x25, 0xC8, 0x19, 0xB8, 0x75];
        assert_eq!(indirect_jump_slot(&hot, 0x7650_0000, false), Some(0x75B8_19C8));
        // The same bytes mean something else in the other bitness, which is why the flag exists: the
        // 32-bit reading of a 64-bit stub is an absolute address, not an offset from the entry.
        assert_eq!(indirect_jump_slot(&plain, entry, false), Some(0x9A));
        assert_eq!(indirect_jump_slot(&hot, 0x7650_0000, true), None);

        // Real kernel32 entries that are NOT stubs. Each must answer None, because None is what keeps
        // the detour where it always was, and a wrong Some would send it somewhere arbitrary.
        let own_x64 = [0xB9, 0x20, 0x03, 0xFE, 0x7F, 0x48, 0x8B, 0x09, 0x8B, 0x04, 0x25, 0x04];
        assert_eq!(indirect_jump_slot(&own_x64, entry, true), None, "GetTickCount reads the shared page itself");
        let prologue_only = [0x8B, 0xFF, 0x55, 0x8B, 0xEC, 0x51, 0x8B, 0x15, 0x24, 0x03, 0xFE, 0x7F];
        assert_eq!(indirect_jump_slot(&prologue_only, 0x7650_0000, false), None, "a hot-patch prologue with a body");
        let call = [0x51, 0xFF, 0x15, 0xB4, 0x1B, 0xB8, 0x75, 0x59, 0xC3, 0xCC, 0xCC, 0xCC];
        assert_eq!(indirect_jump_slot(&call, 0x7650_0000, false), None, "a call returns here, a jump does not");

        // An entry so high that the slot address would wrap is refused rather than wrapped.
        assert_eq!(indirect_jump_slot(&rex, usize::MAX - 3, true), None);
    }

    /// The random half: the clocks of a session as arithmetic, against a model that integrates each rate
    /// over the real time it ran for - in `i128`, so nothing the model does saturates or wraps unless it
    /// says so. Untouchable rule 3 is the claim: no duration axis ever reads less than it read before.
    mod properties {
        use super::*;
        use proptest::prelude::*;
        use proptest::sample::select;
        use std::cell::RefCell;
        use std::collections::VecDeque;

        /// The largest rate a session takes, `chrono_core::MULTIPLIER_MAX` - this crate depends on nothing.
        const RATE_MAX: i64 = 1_000_000;
        /// Rates by their edges: frozen, real, the smallest speed-up, two documented speeds, the largest.
        const RATES: [i64; 6] = [0, 1, 2, 60, 1440, RATE_MAX];
        const TOP: i128 = i64::MAX as i128;

        fn rate() -> impl Strategy<Value = i64> {
            prop_oneof![3 => select(RATES.to_vec()), 1 => 0..=RATE_MAX]
        }

        /// A real clock where a session starts: the boot instant, one tick after it, an hour, a month of
        /// uptime, or anything.
        fn clock() -> impl Strategy<Value = i64> {
            prop_oneof![1 => select(vec![0i64, 1, 36_000_000_000, 25_920_000_000_000]), 2 => 0i64..1_000_000_000_000_000]
        }

        /// A wall moment: either end of the range a session clock holds, one tick inside each, or anything.
        fn moment() -> impl Strategy<Value = i64> {
            prop_oneof![1 => select(vec![0, 1, FAKE_WALL_MAX - 1, FAKE_WALL_MAX]), 2 => 0..=FAKE_WALL_MAX]
        }

        /// How far a real clock moves between two events.
        #[derive(Debug, Clone, Copy)]
        enum Step {
            /// So many ticks: none, one, either side of a millisecond, a second, a day, or anything.
            Ticks(i64),
            /// A share of the whole range at the rate in force, `i64::MAX / share / max(rate, 1)`. One such
            /// step takes an axis halfway to the end of its range or past it, and a few of them with rate
            /// changes between them fill the base a rate change banks (R5-P17).
            Range(i64),
        }

        fn step() -> impl Strategy<Value = Step> {
            prop_oneof![
                4 => select(vec![0i64, 1, 3_333, 9_999, 10_000, 10_001, 10_000_000, 864_000_000_000]).prop_map(Step::Ticks),
                3 => (0i64..1_000_000_000_000).prop_map(Step::Ticks),
                1 => select(vec![2i64, 3, 5]).prop_map(Step::Range),
            ]
        }

        fn ticks(step: Step, rate: i64) -> i64 {
            match step {
                Step::Ticks(n) => n,
                Step::Range(share) => i64::MAX / share / rate.max(1),
            }
        }

        #[derive(Debug, Clone, Copy)]
        enum Event {
            /// The real clocks move on, QUIT and QPC each by its own step.
            Pass(Step, Step),
            /// The session changes its rate (`set_multiplier`, through `rebase`).
            Rate(i64),
            /// The wall jumps to a moment, and the duration axes stay where they are (`jump`).
            Jump(i64),
            /// A reader asks every axis with clocks taken now.
            Read,
            /// A reader whose clocks were taken this many ticks before the anchor it then read (R4-N4).
            ReadEarly(i64),
            /// The session lets the process go, so many ticks after now on each clock (`release_axes`).
            /// From here on nothing changes the rate or the wall any more - the core is gone.
            Release(i64, i64),
        }

        fn event() -> impl Strategy<Value = Event> {
            let margin = || select(vec![0i64, 1, RELEASE_MARGIN_QUIT]);
            prop_oneof![
                6 => (step(), step()).prop_map(|(quit, qpc)| Event::Pass(quit, qpc)),
                3 => rate().prop_map(Event::Rate),
                1 => moment().prop_map(Event::Jump),
                5 => Just(Event::Read),
                1 => select(vec![1i64, 5, 100]).prop_map(Event::ReadEarly),
                1 => (margin(), margin()).prop_map(|(quit, qpc)| Event::Release(quit, qpc)),
            ]
        }

        /// One axis by the model: where it stood at its last anchor, and the real clock it was anchored at.
        #[derive(Debug, Clone, Copy)]
        struct Line {
            base: i128,
            at: i64,
        }

        impl Line {
            /// The value at `clock` at `rate`, exact: nothing cut to fit an `i64`, and a clock before the
            /// anchor answers the anchor.
            fn exact(&self, clock: i64, rate: i64) -> i128 {
                self.base + (i128::from(clock) - i128::from(self.at)).max(0) * i128::from(rate)
            }
        }

        /// A session's clocks by the model.
        #[derive(Debug, Clone)]
        struct Model {
            quit: Line,
            qpc: Line,
            wall: Line,
            rate: i64,
            offset: i64,
            /// Where the process was let go on each clock, once it was.
            released: Option<(i64, i64)>,
        }

        /// What a reader of every axis gets.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct Reading {
            tick: u64,
            quit: i64,
            qpc: i64,
            wall: i64,
            /// Whether the QUIT axis says it stands at the end of its range (`dur_axis_at_range_end`).
            at_range_end: bool,
        }

        impl Model {
            /// A duration axis on the session's line: the rate is never below 1 there, and once the value
            /// reaches the end of an `i64` it stands.
            fn session(line: Line, rate: i64, clock: i64) -> i128 {
                line.exact(clock, rate.max(1)).min(TOP)
            }

            /// A duration axis after the process was let go at `released`: the session's line up to that
            /// instant, the real rate from there on (ADR-14).
            fn axis(line: Line, rate: i64, clock: i64, released: Option<i64>) -> i128 {
                match released {
                    Some(at) if clock >= at => (Self::session(line, rate, at) + i128::from(clock) - i128::from(at)).min(TOP),
                    _ => Self::session(line, rate, clock),
                }
            }

            fn reading(&self, quit_clock: i64, qpc_clock: i64, wall_clock: i64) -> Reading {
                let quit = Self::axis(self.quit, self.rate, quit_clock, self.released.map(|r| r.0));
                let qpc = Self::axis(self.qpc, self.rate, qpc_clock, self.released.map(|r| r.1));
                // The tick count is the QUIT in whole milliseconds plus the session's offset (R4/10a).
                let tick = (i128::from(self.offset) + quit / 10_000).max(0);
                Reading {
                    tick: u64::try_from(tick).unwrap(),
                    quit: i64::try_from(quit).unwrap(),
                    qpc: i64::try_from(qpc).unwrap(),
                    wall: i64::try_from(self.wall.exact(wall_clock, self.rate).clamp(0, i128::from(FAKE_WALL_MAX))).unwrap(),
                    at_range_end: self.quit.exact(quit_clock, self.rate.max(1)) > TOP,
                }
            }

            /// A rate change at the real clocks `now`: every axis carries on from where it stands.
            fn rebase(&mut self, now: (i64, i64), rate: i64) {
                self.quit = Line { base: Self::session(self.quit, self.rate, now.0), at: now.0 };
                self.qpc = Line { base: Self::session(self.qpc, self.rate, now.1), at: now.1 };
                let wall = self.wall.exact(now.0, self.rate).clamp(0, i128::from(FAKE_WALL_MAX));
                self.wall = Line { base: wall, at: now.0 };
                self.rate = rate;
            }
        }

        /// What the code answers, read the way a detour reads it: the anchor and its clock in one window.
        fn read(p: *const Ctl, quit_clock: i64, qpc_clock: i64, wall_clock: i64, released: Option<(i64, i64)>) -> Reading {
            // SAFETY: `p` is a live block this test owns and nothing else writes.
            let ((d, real), (q, real_qpc), ((a_fake, a_real, m), real_wall)) = unsafe {
                (read_dur_with(p, || quit_clock), read_qpc_with(p, || qpc_clock), read_anchor_with(p, || wall_clock))
            };
            let (at, qpc_at) = released.unwrap_or((0, 0));
            Reading {
                tick: d.tick_at(real, at),
                quit: d.quit_at(real, at),
                qpc: q.at(real_qpc, qpc_at),
                wall: fake_wall_at(a_fake, a_real, real_wall, m),
                at_range_end: dur_axis_at_range_end(d.quit_c0, d.q0, d.m, real),
            }
        }

        /// Runs one event on the block and on the model, and checks every reading against the model.
        /// `last` is what the previous reader with clocks taken now saw, which no later one may undercut.
        fn apply(p: *mut Ctl, model: &mut Model, now: &mut (i64, i64), last: &mut Option<Reading>, event: Event) -> Result<(), TestCaseError> {
            let gone = model.released.is_some();
            match event {
                Event::Pass(quit, qpc) => {
                    now.0 = now.0.saturating_add(ticks(quit, model.rate));
                    now.1 = now.1.saturating_add(ticks(qpc, model.rate));
                }
                Event::Rate(m) if !gone => {
                    let clocks = *now;
                    // SAFETY: `p` is a live block this test owns, and this is its only writer.
                    let done = unsafe { rebase(p, m, || clocks.0, || clocks.1) };
                    prop_assert_eq!(done, Rebased { now: clocks.0, old_m: model.rate });
                    model.rebase(clocks, m);
                }
                Event::Jump(to) if !gone => {
                    // SAFETY: as above.
                    unsafe { write_anchor(p, to, now.0, model.rate) };
                    model.wall = Line { base: i128::from(to), at: now.0 };
                }
                Event::Read => {
                    let owed = model.reading(now.0, now.1, now.0);
                    let got = read(p, now.0, now.1, now.0, model.released);
                    prop_assert_eq!(got, owed, "read at {:?}", now);
                    if let Some(released) = model.released {
                        // SAFETY: as above.
                        let (d, q) = unsafe { (read_dur(p), read_qpc(p)) };
                        let watcher = release_axes(d, q, released.0, released.1);
                        prop_assert_eq!((watcher.tick_at(now.0), watcher.quit_at(now.0), watcher.qpc_at(now.1)), (got.tick, got.quit, got.qpc));
                    }
                    if let Some(before) = last {
                        prop_assert!(
                            got.tick >= before.tick && got.quit >= before.quit && got.qpc >= before.qpc,
                            "a duration axis ran back: {:?} after {:?}", got, before
                        );
                    }
                    *last = Some(got);
                }
                Event::ReadEarly(early) => {
                    let (quit, qpc, wall) = (model.quit.at - early, model.qpc.at - early, model.wall.at - early);
                    prop_assert_eq!(read(p, quit, qpc, wall, model.released), model.reading(quit, qpc, wall), "read {} ticks before the anchor", early);
                }
                Event::Release(quit, qpc) => {
                    // 0 means "not let go" to the code, so the real clock's very first tick lets nothing go.
                    let at = (now.0.saturating_add(quit), now.1.saturating_add(qpc));
                    if !gone && at.0 > 0 && at.1 > 0 {
                        model.released = Some(at);
                    }
                }
                Event::Rate(_) | Event::Jump(_) => {}
            }
            Ok(())
        }

        proptest! {
            /// Every axis of a session, after any course of rate changes, wall jumps, real time and a
            /// release: the duration axes are the integral of their rates (at least 1) over the real time,
            /// standing once they reach the end of an `i64` and saying so, the wall is its anchor plus the
            /// real time times the rate within the range a session clock holds, and no duration axis ever
            /// reads less than a reader before it read (untouchable rule 3).
            #[test]
            fn every_axis_is_the_integral_of_its_rates_and_no_duration_axis_runs_back(
                start in (clock(), clock()),
                first_rate in rate(),
                offset in prop_oneof![1 => select(vec![0i64, 1, 10_000_000]), 2 => 0i64..1_000_000_000],
                wall in moment(),
                events in proptest::collection::vec(event(), 1..32),
            ) {
                let mut ctl = zeroed_ctl();
                let p = &mut *ctl as *mut Ctl;
                let anchor = FullAnchor {
                    a_fake: wall,
                    a_real: start.0,
                    multiplier: first_rate,
                    dur_tick_offset: offset,
                    dur_quit_c0: start.0,
                    dur_q0: start.0,
                    dur_qpc_c0: start.1,
                    dur_qpc_q0: start.1,
                };
                // SAFETY: `p` is a live block this test owns.
                unsafe { write_anchor_full(p, &anchor) };
                let mut model = Model {
                    quit: Line { base: i128::from(start.0), at: start.0 },
                    qpc: Line { base: i128::from(start.1), at: start.1 },
                    wall: Line { base: i128::from(wall), at: start.0 },
                    rate: first_rate,
                    offset,
                    released: None,
                };
                let (mut now, mut last) = (start, None);
                for event in events {
                    apply(p, &mut model, &mut now, &mut last, event)?;
                }
            }

            /// A core killed in the middle of any rate change, at any rates and any time into the session,
            /// leaves an anchor no axis reads earlier from than it read when the write began - the wall
            /// included - and the complete write is continuous with it (R4-N5).
            #[test]
            fn every_prefix_of_any_rate_change_keeps_every_axis_from_running_back(
                (old_m, new_m) in (rate(), rate()),
                start in (clock(), clock()),
                offset in 0i64..1_000_000_000,
                wall in moment(),
                (elapsed, elapsed_qpc) in (step(), step()),
            ) {
                let anchor = FullAnchor {
                    a_fake: wall,
                    a_real: start.0,
                    multiplier: old_m,
                    dur_tick_offset: offset,
                    dur_quit_c0: start.0,
                    dur_q0: start.0,
                    dur_qpc_c0: start.1,
                    dur_qpc_q0: start.1,
                };
                let now = (start.0.saturating_add(ticks(elapsed, old_m)), start.1.saturating_add(ticks(elapsed_qpc, old_m)));
                let mut ctl = zeroed_ctl();
                let p = &mut *ctl as *mut Ctl;
                // SAFETY: `p` is a live block this test owns, written by nothing else.
                unsafe { write_anchor_full(p, &anchor) };
                let before = read(p, now.0, now.1, now.0, None);
                let stores = unsafe {
                    rebase(p, new_m, || now.0, || now.1);
                    full_anchor_stores(&full_anchor_fields(p))
                };
                let after = read(p, now.0, now.1, now.0, None);
                prop_assert_eq!(
                    (after.tick, after.quit, after.qpc, after.wall),
                    (before.tick, before.quit, before.qpc, before.wall),
                    "the complete write is not continuous"
                );
                for k in 0..=stores.len() {
                    unsafe {
                        write_anchor_full(p, &anchor);
                        for store in &stores[..k] {
                            store_anchor_field(p, *store);
                        }
                    }
                    for later in [0i64, 1, 10_000, 10_000_000] {
                        let r = read(p, now.0.saturating_add(later), now.1.saturating_add(later), now.0.saturating_add(later), None);
                        prop_assert!(
                            r.tick >= before.tick && r.quit >= before.quit && r.qpc >= before.qpc && r.wall >= before.wall,
                            "x{} to x{}, stopped after {} of the stores, {} later: {:?} under {:?}", old_m, new_m, k, later, r, before
                        );
                    }
                }
            }

            /// Every length a session divides - a wait, a delay, a timer's period, interval and due - is the
            /// length asked for divided by the rate, never longer than asked, never a positive length
            /// turned into none at all (zero is a yield, and a sleep turned into a yield is a spin), and
            /// held at a floor exactly when its companion says so.
            #[test]
            fn every_scaled_length_is_divided_never_lengthened_and_never_made_nothing(
                ms in edges(vec![0u32, 1, 2, 9, 10, 11, 999, 1_000, 1_001, u32::MAX - 1, u32::MAX]),
                interval in edges(vec![i64::MIN, -20_001, -10_001, -10_000, -9_999, -2, -1, 0, 1, i64::MAX]),
                period in edges(vec![i32::MIN, -1, 0, 1, 2, 9, 10, 11, i32::MAX]),
                due in edges(vec![i64::MIN, -10_000, -2, -1, 0, 1, FAKE_WALL_MAX, i64::MAX]),
                fake_now in edges(vec![i64::MIN, -1, 0, 1, FAKE_WALL_MAX, i64::MAX]),
                near in edges(vec![-1i64, 0, 1, 2, 59, 60, 61]),
                (m, wall_m, dur_m) in (rate(), rate(), rate()),
            ) {
                waits_and_delays(ms, interval, m)?;
                periods(period, ms, m)?;
                // A due near the clock by name too: a tick or a rate's worth either side of it.
                timer_due(due, fake_now, wall_m, dur_m)?;
                timer_due(fake_now.saturating_add(near), fake_now, wall_m, dur_m)?;
                // The same moments as an NtDelayExecution deadline, which cannot take a zero (R5-D59).
                delay_deadline(due, fake_now, wall_m)?;
                delay_deadline(fake_now.saturating_add(near), fake_now, wall_m)?;
            }

            /// How long the machine slept, read from three clocks while timer interrupts land between the
            /// readings: never more than it slept, never below zero, and exact once two QUIT readings agree,
            /// whichever of the four tries that is (R5-F4).
            #[test]
            fn the_sleep_bias_is_never_more_than_the_sleep_and_exact_once_two_readings_agree(
                slept in prop_oneof![1 => select(vec![0i64, 1, 10_000, 36_000_000_000]), 1 => 0i64..1_000_000_000_000_000],
                start in 0i64..1_000_000_000_000,
                gaps in proptest::collection::vec((tick(), tick(), tick()), SLEEP_BIAS_TRIES as usize),
            ) {
                // The QUIT the function reads, the interrupt time, the QUIT again - with an interrupt moving
                // QUIT on by so much between each two, and the interrupt time always QUIT plus the sleep.
                let (mut quit, mut quits, mut interrupts) = (start, VecDeque::new(), VecDeque::new());
                for &(before, between, after) in &gaps {
                    quits.push_back(quit);
                    quit += before;
                    interrupts.push_back(quit + slept);
                    quit += between;
                    quits.push_back(quit);
                    quit += after;
                }
                let (quits, interrupts) = (RefCell::new(quits), RefCell::new(interrupts));
                let got = sleep_bias(
                    || quits.borrow_mut().pop_front().expect("more QUIT readings than four tries take"),
                    || interrupts.borrow_mut().pop_front().expect("more interrupt readings than four tries take"),
                );
                prop_assert!((0..=slept).contains(&got), "{} for a sleep of {}", got, slept);
                if gaps.iter().any(|&(before, between, _)| before + between == 0) {
                    prop_assert_eq!(got, slept, "two readings agreed");
                }
            }
        }

        /// A value drawn from its edges as often as from anywhere.
        fn edges<T>(named: Vec<T>) -> BoxedStrategy<T>
        where
            T: Arbitrary + Clone + std::fmt::Debug + 'static,
            T::Strategy: 'static,
        {
            prop_oneof![1 => select(named), 1 => any::<T>()].boxed()
        }

        /// How far an interrupt moves QUIT between two readings: mostly not at all, or by a tick of the
        /// system timer (15.625 ms, or 1 ms when an application raised its resolution).
        fn tick() -> impl Strategy<Value = i64> {
            select(vec![0i64, 0, 0, 1, 10_000, 156_250])
        }

        /// `scale_wait` and `scale_delay_interval`, with what they say about their floors.
        fn waits_and_delays(ms: u32, interval: i64, m: i64) -> Result<(), TestCaseError> {
            let rate = i128::from(m.max(1));
            let wait = scale_wait(ms, m);
            if ms == 0 || ms == u32::MAX {
                prop_assert_eq!(wait, ms, "a poll and INFINITE pass through");
                prop_assert!(!wait_hit_floor(ms, m));
            } else {
                let divided = i128::from(ms) / rate;
                prop_assert_eq!(i128::from(wait), divided.max(1), "{} ms at x{}", ms, m);
                prop_assert!(wait >= 1 && wait <= ms);
                prop_assert_eq!(wait_hit_floor(ms, m), divided == 0, "{} ms at x{}", ms, m);
            }

            let delay = scale_delay_interval(interval, m);
            if interval >= 0 {
                prop_assert_eq!(delay, interval, "a deadline and a yield pass through");
                prop_assert!(!delay_hit_floor(interval, m));
                return Ok(());
            }
            let asked = i128::from(interval);
            let divided = asked / rate;
            let floor = -i128::from(MIN_SCALED_DELAY_TICKS);
            // Divided, unless that is shorter than the floor - then the floor, or what was asked when even
            // that is shorter (R5-F7).
            let owed = if divided <= floor { divided } else { asked.max(floor) };
            prop_assert_eq!(i128::from(delay), owed, "{} at x{}", interval, m);
            prop_assert!(delay < 0 && delay >= interval, "{} at x{} came out as {}", interval, m, delay);
            prop_assert_eq!(delay_hit_floor(interval, m), m > 1 && divided > floor, "{} at x{}", interval, m);
            Ok(())
        }

        /// The three timer lengths: a waitable timer's period (`LONG`), a thread-pool timer's (`DWORD`) and
        /// a window timer's interval, with what each says about its floor.
        fn periods(period: i32, period_ms: u32, m: i64) -> Result<(), TestCaseError> {
            let rate = i128::from(m.max(1));
            let scaled = scale_timer_period(period, m);
            if period <= 0 {
                prop_assert_eq!(scaled, period, "a one-shot and a refused period pass through");
            } else {
                prop_assert_eq!(i128::from(scaled), (i128::from(period) / rate).max(1));
                prop_assert!(scaled >= 1 && scaled <= period);
            }
            prop_assert_eq!(timer_period_hit_floor(i64::from(period), m), period > 0 && i128::from(period) / rate == 0);

            let scaled = scale_timer_period_ms(period_ms, m);
            if period_ms == 0 {
                prop_assert_eq!(scaled, 0, "a one-shot stays one");
            } else {
                prop_assert_eq!(i128::from(scaled), (i128::from(period_ms) / rate).max(1));
                prop_assert!(scaled >= 1 && scaled <= period_ms);
            }
            prop_assert_eq!(timer_period_hit_floor(i64::from(period_ms), m), period_ms > 0 && i128::from(period_ms) / rate == 0);

            // A window timer has no floor of ours: Windows lifts anything under its minimum to it.
            let interval = scale_timer_elapse(period_ms, m);
            prop_assert_eq!(i128::from(interval), i128::from(period_ms) / rate);
            prop_assert_eq!(
                timer_elapse_hit_floor(period_ms, m),
                period_ms >= USER_TIMER_MINIMUM_MS && interval < USER_TIMER_MINIMUM_MS
            );
            Ok(())
        }

        /// A timer's due as the kernel gets it: never positive, because a positive due is an instant on the
        /// REAL clock (R5-D2). A relative one is divided by the duration rate. An absolute one is a moment
        /// of the wall: the timer fires no later than the wall reaches it and no more than one real tick
        /// before, a moment already past gets zero, which the kernel fires at once (R5-F12), and a moment
        /// the arithmetic cannot reach is handed back as it was. Otherwise zero only for less than one real
        /// tick, fired at once too.
        fn timer_due(due: i64, fake_now: i64, wall_m: i64, dur_m: i64) -> Result<(), TestCaseError> {
            let got = scale_timer_due(due, fake_now, wall_m, dur_m);
            let (asked, now) = (i128::from(due), i128::from(fake_now));
            if due < 0 {
                let rate = i128::from(dur_m.max(1));
                prop_assert_eq!(i128::from(got), asked / rate, "relative {} at x{}", due, dur_m);
                prop_assert!(got <= 0 && got >= due);
                prop_assert!(got < 0 || asked > -rate, "relative {} at x{} came out as none", due, dur_m);
                return Ok(());
            }
            let rate = i128::from(wall_m.max(1));
            let ahead = asked - now;
            if i64::try_from(ahead).is_err() {
                prop_assert_eq!(got, due, "a due the arithmetic cannot reach is handed back");
            } else if ahead <= 0 {
                prop_assert_eq!(got, 0, "a moment already past fires at once");
            } else {
                prop_assert!(got <= 0, "absolute {} from {} at x{} came out as {}", due, fake_now, wall_m, got);
                // Where the wall stands when the timer fires.
                let fires = -i128::from(got) * rate;
                prop_assert!(fires <= ahead && fires > ahead - rate, "absolute {} from {} at x{}: fires {} of {} ahead", due, fake_now, wall_m, fires, ahead);
                prop_assert!(got < 0 || ahead < rate, "absolute {} from {} at x{} came out as none", due, fake_now, wall_m);
            }
            Ok(())
        }

        /// An `NtDelayExecution` deadline as the kernel gets it (R5-D59): never zero, because to that call
        /// zero is a yield. What is not a deadline passes through. A moment already past, or less than one
        /// real tick ahead, is an instant long past, a moment the arithmetic cannot reach is handed back,
        /// and any other is a relative delay that ends no later than the wall reaches it and no more than
        /// one real tick before.
        fn delay_deadline(deadline: i64, fake_now: i64, wall_m: i64) -> Result<(), TestCaseError> {
            let got = scale_delay_deadline(deadline, fake_now, wall_m);
            if deadline <= 0 {
                prop_assert_eq!(got, deadline, "a relative delay and a yield pass through");
                return Ok(());
            }
            prop_assert_ne!(got, 0, "deadline {} from {} at x{} became a yield", deadline, fake_now, wall_m);
            let rate = i128::from(wall_m.max(1));
            let ahead = i128::from(deadline) - i128::from(fake_now);
            if i64::try_from(ahead).is_err() {
                prop_assert_eq!(got, deadline, "a deadline the arithmetic cannot reach is handed back");
            } else if ahead < rate {
                prop_assert_eq!(got, PAST_DEADLINE, "deadline {} from {} at x{}: nothing left to wait", deadline, fake_now, wall_m);
            } else {
                prop_assert!(got < 0, "deadline {} from {} at x{} came out as {}", deadline, fake_now, wall_m, got);
                let ends = -i128::from(got) * rate;
                prop_assert!(ends <= ahead && ends > ahead - rate, "deadline {} from {} at x{}: ends {} of {} ahead", deadline, fake_now, wall_m, ends, ahead);
            }
            Ok(())
        }
    }
}
