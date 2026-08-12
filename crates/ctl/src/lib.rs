//! Control-memory contract shared by the mechanism (chrono-mech) and the injected
//! hook (chrono-hook). Both processes map the SAME `#[repr(C)]` layout into a named
//! shared section, so it is defined in exactly one place.
//!
//! - The ANCHOR fields (`a_fake`, `a_real`, `multiplier`) are written by the
//!   mechanism under a seqlock and read by the hook. A seqlock keeps the three
//!   fields mutually consistent without blocking the hot path.
//! - `tz_bias` is a STABLE config field: the mechanism writes it once before the
//!   target exists, the hook reads it once at install. It never changes during a
//!   session, so it lives outside the seqlock.
//! - The COVERAGE fields (`installed_channels`, `calls`) are written only by the
//!   hook and read by the mechanism. One writer per word, aligned, monotonic - a
//!   plain volatile access is enough. (A `calls` increment is a volatile RMW, so
//!   concurrent target threads hitting the SAME channel may lose a bump - that only
//!   ever UNDER-counts live evidence, never fabricates coverage.)
//!
//! Fake wall time is `a_fake + (quit_now - a_real) * multiplier`, in 100 ns units,
//! anchored on `QueryUnbiasedInterruptTime` (ADR-5). UTC channels return that
//! instant directly; `GetLocalTime` returns it shifted back into the session zone
//! by `tz_bias`.

use std::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use std::sync::atomic::{compiler_fence, Ordering};

/// Named shared section for a session's control memory (per interactive session).
pub const CTL_SECTION_NAME: &str = "Local\\ChronoCtl";

// --- Wall-clock channels -------------------------------------------------------
//
// The coverage bit, the `calls`-array index (IDX_*), and the `CHANNELS` table below
// are three views of the same list and MUST stay in sync. A unit test guards it.

/// Coverage bit: `GetSystemTimeAsFileTime` is hooked.
pub const CH_GSTAFT: u32 = 1 << 0;
/// Coverage bit: `GetSystemTimePreciseAsFileTime` is hooked.
pub const CH_GSTPAFT: u32 = 1 << 1;
/// Coverage bit: `GetSystemTime` is hooked.
pub const CH_GST: u32 = 1 << 2;
/// Coverage bit: `GetLocalTime` is hooked.
pub const CH_GLT: u32 = 1 << 3;
/// Coverage bit: `NtQuerySystemTime` is hooked.
pub const CH_NTQST: u32 = 1 << 4;
/// Coverage bit: `GetTimeZoneInformation` is hooked (session zone).
pub const CH_GTZI: u32 = 1 << 5;
/// Coverage bit: `GetDynamicTimeZoneInformation` is hooked (session zone).
pub const CH_GDTZI: u32 = 1 << 6;

/// Index of each channel into the `calls` array (== its position in `CHANNELS`).
pub const IDX_GSTAFT: usize = 0;
pub const IDX_GSTPAFT: usize = 1;
pub const IDX_GST: usize = 2;
pub const IDX_GLT: usize = 3;
pub const IDX_NTQST: usize = 4;
pub const IDX_GTZI: usize = 5;
pub const IDX_GDTZI: usize = 6;

/// Number of time channels tracked (wall-clock plus session zone).
pub const CHANNEL_COUNT: usize = 7;

/// Which system module exports a channel (the hook resolves it there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelModule {
    Kernel32,
    Ntdll,
}

/// One time channel: its coverage bit, the exported symbol the hook detours, and the
/// module that exports it. Single source of truth so the mechanism reports exactly the
/// channels the hook installs.
#[derive(Debug, Clone, Copy)]
pub struct ChannelDef {
    pub bit: u32,
    pub name: &'static str,
    pub module: ChannelModule,
}

/// All time channels, ordered by their `calls` index (IDX_*): the wall-clock set
/// followed by the session-zone functions.
pub const CHANNELS: [ChannelDef; CHANNEL_COUNT] = [
    ChannelDef { bit: CH_GSTAFT, name: "GetSystemTimeAsFileTime", module: ChannelModule::Kernel32 },
    ChannelDef { bit: CH_GSTPAFT, name: "GetSystemTimePreciseAsFileTime", module: ChannelModule::Kernel32 },
    ChannelDef { bit: CH_GST, name: "GetSystemTime", module: ChannelModule::Kernel32 },
    ChannelDef { bit: CH_GLT, name: "GetLocalTime", module: ChannelModule::Kernel32 },
    ChannelDef { bit: CH_NTQST, name: "NtQuerySystemTime", module: ChannelModule::Ntdll },
    ChannelDef { bit: CH_GTZI, name: "GetTimeZoneInformation", module: ChannelModule::Kernel32 },
    ChannelDef { bit: CH_GDTZI, name: "GetDynamicTimeZoneInformation", module: ChannelModule::Kernel32 },
];

/// Shared control block. `#[repr(C)]` so both processes agree on the layout.
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
    /// Bitmask of channels the hook installed (written by the hook).
    pub installed_channels: u32,
    pub _pad1: u32,
    /// Per-channel call counters, indexed by IDX_* (written by the hook).
    pub calls: [u64; CHANNEL_COUNT],
}

/// Size of the control block, for CreateFileMapping.
pub const fn ctl_size() -> usize {
    core::mem::size_of::<Ctl>()
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

/// Mark a channel as installed (hook side). OR-in, so several channels accumulate.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn mark_channel_installed(p: *mut Ctl, channel: u32) {
    let cur = read_volatile(addr_of!((*p).installed_channels));
    write_volatile(addr_of_mut!((*p).installed_channels), cur | channel);
}

/// Read the installed-channels bitmask (mechanism side).
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`.
pub unsafe fn read_installed(p: *const Ctl) -> u32 {
    read_volatile(addr_of!((*p).installed_channels))
}

/// Increment a channel's call counter (hook side). `idx` must be < CHANNEL_COUNT.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `idx < CHANNEL_COUNT`.
pub unsafe fn bump_calls(p: *mut Ctl, idx: usize) {
    let slot = (addr_of_mut!((*p).calls) as *mut u64).add(idx);
    let cur = read_volatile(slot);
    write_volatile(slot, cur.wrapping_add(1));
}

/// Read a channel's call counter (mechanism side). `idx` must be < CHANNEL_COUNT.
///
/// # Safety
/// `p` must point to a live, correctly aligned `Ctl`, and `idx < CHANNEL_COUNT`.
pub unsafe fn read_calls(p: *const Ctl, idx: usize) -> u64 {
    let slot = (addr_of!((*p).calls) as *const u64).add(idx);
    read_volatile(slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zeroed() -> Ctl {
        Ctl {
            seq: 0,
            tz_bias: 0,
            a_fake: 0,
            a_real: 0,
            multiplier: 0,
            installed_channels: 0,
            _pad1: 0,
            calls: [0; CHANNEL_COUNT],
        }
    }

    #[test]
    fn anchor_round_trips() {
        let mut ctl = zeroed();
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
        let mut ctl = zeroed();
        let p = &mut ctl as *mut Ctl;
        unsafe {
            write_tz_bias(p, -120);
            assert_eq!(read_tz_bias(p), -120);
        }
    }

    #[test]
    fn channels_accumulate_and_count_per_index() {
        let mut ctl = zeroed();
        let p = &mut ctl as *mut Ctl;
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
        ];
        assert_eq!(CHANNELS.len(), CHANNEL_COUNT);
        for (idx, bit) in expected {
            assert_eq!(CHANNELS[idx].bit, bit, "bit mismatch at index {idx}");
        }
        // Bits are distinct and non-zero.
        let mut seen = 0u32;
        for ch in CHANNELS {
            assert_ne!(ch.bit, 0);
            assert_eq!(seen & ch.bit, 0, "duplicate bit for {}", ch.name);
            seen |= ch.bit;
        }
    }
}
