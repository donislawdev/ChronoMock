//! The clock a Chromium session runs on, and the arithmetic that moves it.
//!
//! Computed entirely Rust-side so the panel matches the app's own `Date.now()` with no browser
//! round-trip, and shared with the shim so the two cannot drift: every context is shimmed from this
//! clock's CURRENT origin, not from the session's initial values.

use std::cell::Cell;

use chrono_core::TimeMode;
use chrono_proto::{Clock, Event, TimeSpec, PROTOCOL_VERSION};

use crate::cdp::{self, ScheduledRate};
use crate::cdp_attach::ShimOrigin;
use crate::events::{jump_error_key, moment_error_key, start_time_mode};
use crate::session_commands::JumpTo;
use crate::zone::{epoch_ms_to_wall, moment_epoch_ms, now_epoch_ms, FT_UNIX_EPOCH, WALL_MAX_MS, WALL_MIN_MS};

/// How long after it is asked for a rate change takes effect - on the panel and in every page at one
/// instant (R4-S17, ADR-9 R4/14b). Taking effect at the moment of the command, the panel changed at
/// once and each page only when the change reached it, and the page's wall stepped at that moment by
/// the delay times the change in rate - backwards when slowing down: about seven seconds from x1440
/// to x1 at a delivery of five milliseconds. This covers renewing the new-document scripts and
/// telling a dozen or so pages, about three CDP calls each, so a page gets the change before its
/// instant and takes it over from its own segment, at the panel's moment to the millisecond.
pub(crate) const RATE_CHANGE_DELAY_MS: i64 = 250;

/// One stretch of the clock: the wall from `wall_fake0` at `wall_real0`, the duration from
/// `dur_fake_accum` at `dur_real0`, both at `mult`. A jump starts a new wall anchor and leaves the
/// duration one alone, so the two anchors are kept apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Segment {
    wall_fake0: i64,
    wall_real0: i64,
    mult: i64,
    dur_fake_accum: i64,
    dur_real0: i64,
}

impl Segment {
    /// The wall at `now`: the origin plus scaled real time, frozen at mult 0.
    fn wall_at(&self, now: i64) -> i64 {
        // Saturating on BOTH operations, not just the product. `chrono-cli` keeps the workspace's
        // release `overflow-checks`, so an unsaturated add here panics the core mid-session (R2-W2) -
        // and a panicking CDP core never runs its shutdown, leaving a launched Chromium with an open
        // debug port and a temp profile behind. Defence in depth behind the multiplier bound.
        //
        // Then clamped where the native clock clamps (R4-S8). Unclamped, this clock ran on past year
        // 30828 into instants no `state` event could name, while the hook, holding a host at the same
        // moment, stopped - and the report said nothing, where the native session says
        // `time.fake_clock_clamped`.
        self.wall_fake0
            .saturating_add(now.saturating_sub(self.wall_real0).saturating_mul(self.mult))
            .clamp(WALL_MIN_MS, WALL_MAX_MS)
    }

    /// The fake duration at `now`: the accumulator plus this segment's integral of the rate.
    fn elapsed_at(&self, now: i64) -> i64 {
        self.dur_fake_accum.saturating_add(now.saturating_sub(self.dur_real0).saturating_mul(self.mult))
    }

    /// The segment a rate change at `at` starts: the wall and the duration go on from where this one
    /// puts them then, so neither steps at the change (rule 3).
    fn continued_at(&self, at: i64, mult: i64) -> Segment {
        Segment {
            wall_fake0: self.wall_at(at),
            wall_real0: at,
            mult,
            dur_fake_accum: self.elapsed_at(at),
            dur_real0: at,
        }
    }
}

/// The live clock of a CDP session, computed entirely Rust-side so the panel matches the app's own
/// `Date.now()` with no browser round-trip. The current segment is pushed identically to every JS
/// context, so all contexts and the panel share one absolute origin. A rate change does not move the
/// segment: it is scheduled for an instant shortly ahead, and every read on either side takes it
/// at that instant (R4-S17). A separate duration accumulator keeps `elapsed_fake` an honest integral
/// of the rate over time - a jump moves the wall but adds no elapsed duration - mirroring the native
/// split between elapsed time and the fake wall reached.
pub(crate) struct CdpClock {
    segment: Segment,
    /// The rate change waiting for its instant, at most one. Every read takes it at that instant,
    /// and the next change or jump folds it into `segment`.
    scheduled: Option<ScheduledRate>,
    bias: i32,
    session_real0: i64,
    /// Whether the wall has been seen standing at [`WALL_MAX_MS`]. Set where the wall is read, so a
    /// heartbeat, a query or a rate change that found it there is remembered after a jump back.
    reached_end: Cell<bool>,
    /// How far the system clock the pages read stands ahead of the axis this clock runs on, in ms
    /// (R5-F6, R5-D42): zero at the start, and it grows by every sleep of the machine and every
    /// correction of its clock. The origin the pages get is given on their clock through it.
    wall_offset: i64,
}

impl CdpClock {
    fn new(fake0_ms: i64, real0_ms: i64, mult: i64, bias: i32) -> Self {
        CdpClock {
            segment: Segment {
                wall_fake0: fake0_ms,
                wall_real0: real0_ms,
                mult,
                dur_fake_accum: 0,
                dur_real0: real0_ms,
            },
            scheduled: None,
            bias,
            session_real0: real0_ms,
            reached_end: Cell::new(false),
            wall_offset: 0,
        }
    }

    /// The system clock stands `offset` ms ahead of this clock's axis now (see `wall_offset`).
    pub(crate) fn set_wall_offset(&mut self, offset: i64) {
        self.wall_offset = offset;
    }

    /// How far the system clock stands ahead of this clock's axis, as last set.
    pub(crate) fn wall_offset(&self) -> i64 {
        self.wall_offset
    }

    /// How far, in fake ms, the pages stand from this clock at `now` when they were last given it at
    /// `pushed_offset`: the system clock they read moved by the difference since, and their wall with
    /// it, times the rate they run at now - a change scheduled for an instant already past included
    /// (CodeRabbit on #110). Positive means the pages run ahead - after a sleep, say.
    pub(crate) fn pages_drift_ms(&self, now: i64, pushed_offset: i64) -> i64 {
        self.wall_offset.saturating_sub(pushed_offset).saturating_mul(self.segment_at(now).mult)
    }

    /// The rate the clock runs at `now`, a scheduled change already past included.
    pub(crate) fn mult_at(&self, now: i64) -> i64 {
        self.segment_at(now).mult
    }

    /// A rate change as the pages take it: its instant on the system clock they read.
    pub(crate) fn for_pages(&self, next: ScheduledRate) -> ScheduledRate {
        ScheduledRate { at_ms: next.at_ms.saturating_add(self.wall_offset), ..next }
    }

    /// The segment the clock runs on at `now`: the scheduled change taken once its instant has come.
    fn segment_at(&self, now: i64) -> Segment {
        match self.scheduled {
            Some(next) if now >= next.at_ms => self.segment.continued_at(next.at_ms, next.mult),
            _ => self.segment,
        }
    }

    /// Fold a scheduled change whose instant has come into the segment - what the shim's `settle` does.
    fn settle(&mut self, now: i64) {
        if self.scheduled.is_some_and(|next| now >= next.at_ms) {
            self.segment = self.segment_at(now);
            self.scheduled = None;
        }
    }

    /// The fake wall instant (epoch ms) at `now`, segment by segment. Frozen (mult 0) holds it at the
    /// origin - xN accelerates.
    pub(crate) fn fake_wall_ms(&self, now: i64) -> i64 {
        let wall = self.segment_at(now).wall_at(now);
        if wall == WALL_MAX_MS {
            self.reached_end.set(true);
        }
        wall
    }

    /// Whether the wall stood at the end of its range at any point this clock was read, or stands
    /// there at `now` - the Chromium side of `time.fake_clock_clamped`.
    pub(crate) fn reached_range_end(&self, now: i64) -> bool {
        self.fake_wall_ms(now) == WALL_MAX_MS || self.reached_end.get()
    }

    /// Fake duration elapsed (the integral of the rate), segment by segment. A jump does not touch
    /// this, so a wall discontinuity is never counted as elapsed time.
    pub(crate) fn elapsed_fake_ms(&self, now: i64) -> i64 {
        self.segment_at(now).elapsed_at(now)
    }

    /// The rate the clock is set to: a scheduled one as soon as it is asked for. The window sets its
    /// list of rates from every event, and the rate still running for the next quarter of a second
    /// would set it back for a moment, right after the change was confirmed.
    fn target_mult(&self) -> i64 {
        self.scheduled.map_or(self.segment.mult, |next| next.mult)
    }

    /// A `state` event at `now` (passed in so the mapping is pure and unit-testable).
    pub(crate) fn state_event_at(&self, now: i64) -> Event {
        Event::State {
            v: PROTOCOL_VERSION,
            fake: Clock {
                wall: epoch_ms_to_wall(self.fake_wall_ms(now), self.bias),
                zone_bias_min: self.bias,
            },
            // The real wall is the system clock's, whatever this clock's axis did across a sleep.
            real: Clock { wall: epoch_ms_to_wall(now.saturating_add(self.wall_offset), self.bias), zone_bias_min: self.bias },
            multiplier: self.target_mult(),
            elapsed_fake_ms: self.elapsed_fake_ms(now),
            elapsed_real_ms: self.elapsed_real_ms(now),
        }
    }

    /// Schedule a new multiplier asked for at `now`, for [`RATE_CHANGE_DELAY_MS`] later: until then the
    /// clock runs on at its rate, from then on at the new one, with the wall and the duration going
    /// on from where they stand at that instant, so neither steps (rule 3). A negative rate would run
    /// the wall backward, which is never valid, so it clamps to 0 (freeze). Returns the change to
    /// push to the shim.
    ///
    /// A change still waiting is replaced and never takes effect, and the new one takes its instant
    /// rather than a later one. A page that has not reached that instant replaces it the same way,
    /// and one that has is late for the new change and says so - at a later instant it would have run
    /// the replaced rate meanwhile, away from the panel with no word.
    pub(crate) fn set_multiplier_at(&mut self, m: i64, now: i64) -> ScheduledRate {
        let at = match self.scheduled {
            Some(waiting) if now < waiting.at_ms => waiting.at_ms,
            _ => {
                self.settle(now);
                now.saturating_add(RATE_CHANGE_DELAY_MS)
            }
        };
        let mult = m.max(0);
        let next = ScheduledRate { at_ms: at, mult, dur: mult };
        self.scheduled = Some(next);
        next
    }

    /// Re-anchor for a jump to a new fake wall at `now`: the wall moves and continues at the rate it
    /// runs at - the duration axis is untouched, so a backward jump never rewinds elapsed time (rule
    /// 3). A rate change still waiting stays scheduled and takes effect at its instant from the new
    /// wall. Push [`Self::shim_origin`] to the shim with [`cdp_set_expr`].
    pub(crate) fn jump_to_at(&mut self, new_fake_ms: i64, now: i64) {
        self.settle(now);
        self.segment.wall_fake0 = new_fake_ms;
        self.segment.wall_real0 = now;
    }

    /// The session clock a `start` asks for, resolved ONCE in Unix-epoch ms.
    ///
    /// The moment is absolute by the time it reaches here (the driver resolves a relative `--at`
    /// before spawning), and an out-of-range one comes back as a translation key rather than a
    /// silent fall-back to real time (untouchable rule 4). The key travels instead of the event so
    /// this stays a pure function: what to emit, and in what order, is the caller's business.
    ///
    /// The zone the moment is read in is the SESSION's, carried on the moment itself, so the same
    /// wall text under two biases is two different instants (untouchable rule 2).
    ///
    /// Checked by the same gate as the native start, in the same order - mode, zone, moment (R4-S9).
    /// This path used to read the mode on its own and take the zone as it came, so a mistyped mode ran
    /// as flow, a rate out of range was taken or turned into x1, and a missing zone read as UTC.
    pub(crate) fn from_time_spec(time: &TimeSpec, real_now_ms: i64) -> Result<Self, &'static str> {
        // flow = x1 (a plain wall offset), xN accelerates, frozen = x0 (the wall is held - the shim keeps
        // timers real via TS = M || 1). A multiplier of 0 is freeze, as it is natively and in flight.
        let mult = match start_time_mode(time)? {
            TimeMode::Flow => 1,
            TimeMode::Frozen => 0,
            TimeMode::Multiplier(m) => m,
        };
        let bias = time
            .moment
            .tz_bias_min
            .filter(|&b| chronomock::zone_bias_in_range(b))
            .ok_or("time.bad_zone")?;
        let fake_start_ms = match time.moment.local.as_deref() {
            Some(local) => moment_epoch_ms(local, bias).map_err(|e| moment_error_key(&e))?,
            None => real_now_ms, // no --at: the fake clock starts at real now (pure offset/acceleration)
        };
        Ok(CdpClock::new(fake_start_ms, real_now_ms, mult, bias))
    }

    /// Real milliseconds since the session started. One source for the subtraction, which the
    /// heartbeat and the closing `ended` both need and used to spell out separately.
    pub(crate) fn elapsed_real_ms(&self, now: i64) -> i64 {
        now - self.session_real0
    }

    /// The origin a newly attached context must be shimmed from: the CURRENT segment and the change
    /// still waiting, not the session's initial values, so a context attaching after an in-flight
    /// change starts on the same clock as every other one (rule 3). A Chromium session runs the
    /// duration axis at the wall rate - the acceleration is the point of driving it.
    ///
    /// Given on the system clock the pages read (R5-F6): this clock runs on interrupt time, which a
    /// sleep or a clock correction does not move, and the pages on `Date.now`, which they do - the
    /// real instants are shifted by `wall_offset`, so a page reads what the panel reads.
    pub(crate) fn shim_origin(&self) -> ShimOrigin {
        ShimOrigin {
            fake0: self.segment.wall_fake0,
            real0: self.segment.wall_real0.saturating_add(self.wall_offset),
            mult: self.segment.mult,
            dur: self.segment.mult,
            scheduled: self.scheduled.map(|next| self.for_pages(next)),
        }
    }
}

/// The axis a Chromium session's clock runs on (R5-F6, R5-D42): Unix-epoch ms that equal the system
/// clock at the start and then go on with unbiased interrupt time - the clock the native anchors stand on
/// (ADR-5). A sleep of the machine and a correction of its clock move the system clock and not this one,
/// so the session clock stands still across a sleep, as a native session's does, where it used to run on
/// through it at the session speed. With the sleep the machine has had since the start, for `host_slept`.
pub(crate) struct HostAxis {
    wall0: i64,
    quit0: i64,
    bias0: i64,
    /// The most sleep read so far, in ms - the reading is a lower bound when an interrupt falls between
    /// its clocks (`chrono_ctl::sleep_bias`), and the count must never go back.
    slept_ms: i64,
}

impl HostAxis {
    pub(crate) fn start() -> HostAxis {
        let quit0 = chrono_mech::unbiased_interrupt_time();
        HostAxis { wall0: now_epoch_ms(), quit0, bias0: chrono_mech::slept_since_boot(), slept_ms: 0 }
    }

    /// Now, on the axis.
    pub(crate) fn now(&self) -> i64 {
        axis_ms(self.wall0, self.quit0, chrono_mech::unbiased_interrupt_time())
    }

    /// How long the machine has slept since the start, in ms, never less than read before.
    pub(crate) fn slept_ms(&mut self) -> i64 {
        let read = chrono_mech::slept_since_boot().saturating_sub(self.bias0).max(0) / 10_000;
        self.slept_ms = self.slept_ms.max(read);
        self.slept_ms
    }
}

/// The axis at interrupt time `quit_now` (100 ns): the system clock at the start plus the interrupt time
/// gone by since, in whole ms. Pure, so the projection is tested without a clock.
fn axis_ms(wall0: i64, quit0: i64, quit_now: i64) -> i64 {
    wall0.saturating_add(quit_now.saturating_sub(quit0) / 10_000)
}

/// A UTC FILETIME as Unix-epoch milliseconds, saturating at either end of the range rather than
/// wrapping - a wall clock standing at the end of its range must not come out as a date centuries
/// before the epoch.
fn filetime_to_epoch_ms(ft: i64) -> i64 {
    ft.saturating_sub(FT_UNIX_EPOCH) / 10_000
}

/// The origin a page inside a natively hooked application is shimmed from: the native session's
/// fake wall at its real wall, both from one `SessionState` snapshot, at the native rate. The
/// duration rate follows the application - it scales only under `scale_duration`, so the pages'
/// timers run at the same speed as the host's own (docs/09 section 12.6).
pub(crate) fn shim_origin_from_state(state: &chrono_mech::SessionState, scale_duration: bool) -> ShimOrigin {
    ShimOrigin {
        fake0: filetime_to_epoch_ms(state.fake_ft),
        real0: filetime_to_epoch_ms(state.real_ft),
        mult: state.multiplier,
        dur: if scale_duration { state.multiplier.max(1) } else { 1 },
        scheduled: None,
    }
}

/// Past this much fake drift between what the pages hold and what the session clock reads, the pages are
/// given the clock again (docs/09 section 12.5) - in a session of the native mechanism and, since R5-F6,
/// in a Chromium session. At x60 that is 17 ms of real drift - below what the two clock bases disagree
/// by in ordinary running, so a re-anchor happens across a sleep or a clock correction and not otherwise.
pub(crate) const DRIFT_MS: i64 = 1_000;

/// The least real time the pages stand from the session clock before they are given it again, however
/// fast the clock runs (CodeRabbit on #110). The two clocks are read in whole milliseconds, so they
/// disagree by one now and then in ordinary running, and at x10 000 one millisecond is ten seconds of
/// session time - past [`DRIFT_MS`] on every turn, a clock move to every page each time.
pub(crate) const RESYNC_FLOOR_MS: i64 = 50;

/// Whether pages that stand `drift_fake_ms` of session time from the clock running at `mult` are to
/// be given it again: past [`DRIFT_MS`] of session time and [`RESYNC_FLOOR_MS`] of real time both. A
/// frozen clock never drifts.
pub(crate) fn resync_due(drift_fake_ms: i64, mult: i64) -> bool {
    let drift = drift_fake_ms.unsigned_abs();
    let real = drift.checked_div(mult.unsigned_abs()).unwrap_or(0);
    drift > DRIFT_MS.unsigned_abs() && real >= RESYNC_FLOOR_MS.unsigned_abs()
}

/// How far, in fake milliseconds, the clock the pages were last given has walked away from the
/// session clock: what a page would read now from the origin it holds, minus what the host reads
/// now (`fresh` is the host's fake and real wall at one instant). The two clocks stand on different
/// bases - the host's anchor on interrupt time, which does not run while the machine sleeps, and the
/// pages' on the system clock, which does - so they drift apart across a sleep and across a clock
/// correction (docs/09 section 12.5). Positive means the pages run ahead.
pub(crate) fn drift_ms(pushed: ShimOrigin, fresh: ShimOrigin) -> i64 {
    let page_reads = pushed
        .fake0
        .saturating_add(fresh.real0.saturating_sub(pushed.real0).saturating_mul(pushed.mult));
    page_reads.saturating_sub(fresh.fake0)
}

/// The JS that puts a context on `origin` at once, the change still waiting included, through the
/// shim's own `set`: a jump, the release, and every move of the pages of an embedded engine, which
/// follow the host's clock as it is (ADR-9 R4/14b). The shim re-anchors its duration axis at the rate
/// it ran at first, so `performance.now` stays continuous (rule 3). The segment is the driver's,
/// identical for every context, so all contexts stay in step. The duration rate is floored at 1 like
/// the shim itself.
pub(crate) fn cdp_set_expr(origin: ShimOrigin) -> String {
    cdp::set_expr(origin.fake0, origin.real0, origin.mult, origin.dur, origin.scheduled)
}

/// The JS that schedules a rate change in a context, through the shim's own `schedule` (R4-S17). Only
/// the instant and the rates travel: the context takes the change over from its own segment, so its
/// wall is continuous whenever the message arrives, and the same as the panel's when it arrives
/// before the instant. One that arrives after answers `late`.
pub(crate) fn cdp_schedule_expr(next: ScheduledRate) -> String {
    format!(
        "(function(){{var S=globalThis.__chronomock;if(!S)return 'no-shim';return S.schedule({},{},{});}})()",
        next.at_ms,
        next.mult,
        next.dur.max(1)
    )
}

/// The JS that lets a page go when the session ends and the application lives on: the wall back on the
/// real clock and the duration axis on from where it stands at rate 1, the same thing the hook does for
/// the host once its core is gone. Nothing stays scheduled, the zone is no longer checked, and the
/// audit starts over ([`cdp::release_expr`]). The origin is `0` on both sides on purpose: with the rate at 1, any
/// instant where fake equals real puts the wall on the real clock.
pub(crate) fn cdp_release_expr() -> String {
    cdp::release_expr()
}

/// Resolve a CDP jump target to a fake epoch-ms instant: an absolute moment in the zone it names (the
/// session zone when it names none), or a relative delta applied to the CURRENT fake instant by the
/// native jump's own step (`chrono_core::step_target`, the grammar `calc` reads). Both go through
/// the session range, so a jump cannot land where a start could not (R4-S7). Returns a translation key
/// on a bad moment (rule 6).
///
/// An absolute target that NAMED a zone used to be read in the session zone regardless, while the
/// native session read it in the zone it named - one command, two instants depending on the
/// mechanism. The built-in clients always name the session zone, so neither of them saw it.
///
/// A relative jump went through the civil text of the fake wall, which has whole seconds, and lost up
/// to 999 ms of it at every jump (R5-P19). It now steps on FILETIME as the native session does. The
/// target arrives read (`session_commands::read_jump`), so a kind of moment neither mechanism knows is
/// refused before this, alike on both paths (R5-P18) - it used to be read here as absolute.
pub(crate) fn cdp_jump_target(clock: &CdpClock, to: &JumpTo<'_>, now: i64) -> Result<i64, &'static str> {
    match to {
        JumpTo::Step(step) => {
            let fake_ft = epoch_ms_to_filetime(clock.fake_wall_ms(now));
            let target = chrono_core::step_target(fake_ft, clock.bias, step).map_err(jump_error_key)?;
            Ok(filetime_to_epoch_ms(target))
        }
        JumpTo::At { local, bias } => {
            moment_epoch_ms(local, bias.unwrap_or(clock.bias)).map_err(|e| moment_error_key(&e))
        }
    }
}

/// Unix-epoch milliseconds as a UTC FILETIME, saturating rather than wrapping.
fn epoch_ms_to_filetime(ms: i64) -> i64 {
    ms.saturating_mul(10_000).saturating_add(FT_UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_commands::read_jump;
    use chrono_proto::{MomentKind, MomentSpec, TimeModeKind};

    fn origin(fake0: i64, real0: i64, mult: i64) -> ShimOrigin {
        ShimOrigin { fake0, real0, mult, dur: mult, scheduled: None }
    }

    /// R5-F6: the session axis is the system clock at the start plus the interrupt time gone by since,
    /// so a sleep, which interrupt time does not count, does not move it.
    #[test]
    fn the_session_axis_goes_on_with_interrupt_time_from_the_start() {
        assert_eq!(axis_ms(1_000_000, 50_000_000, 50_000_000), 1_000_000);
        assert_eq!(axis_ms(1_000_000, 50_000_000, 50_000_000 + 25_000_000), 1_002_500, "2.5 s of interrupt time");
        assert_eq!(axis_ms(i64::MAX - 1, 0, i64::MAX), i64::MAX, "saturating, never wrapping");
    }

    /// R5-F6, R5-D42: across a sleep the system clock moves and the session clock does not. The pages
    /// are given the clock on the system clock, so a page reads what the panel reads. The pages that
    /// still hold the clock of before the sleep stand ahead by the sleep times the speed, and that is
    /// past `DRIFT_MS`. A rate change still waiting is moved by the same offset.
    #[test]
    fn the_pages_are_given_the_clock_on_the_system_clock_they_read() {
        let mut clock = CdpClock::new(2_000_000_000_000, 1_000_000, 60, 0);
        let session_now = 1_010_000; // ten seconds of interrupt time on
        assert_eq!(clock.fake_wall_ms(session_now), 2_000_000_600_000);
        // The machine slept five seconds: the system clock is five seconds ahead of the axis.
        clock.set_wall_offset(5_000);
        let wall_now = session_now + 5_000;
        let origin = clock.shim_origin();
        let page_reads = origin.fake0 + (wall_now - origin.real0) * origin.mult;
        assert_eq!(page_reads, clock.fake_wall_ms(session_now), "a page given the clock now reads the panel's");
        assert_eq!(clock.pages_drift_ms(session_now, 0), 300_000, "the pages of before the sleep run five seconds times 60 ahead");
        assert!(resync_due(clock.pages_drift_ms(session_now, 0), 60));
        assert_eq!(clock.pages_drift_ms(session_now, 5_000), 0, "pages given it after the sleep do not");
        let next = clock.set_multiplier_at(1, session_now);
        assert_eq!(clock.for_pages(next).at_ms, next.at_ms + 5_000, "the change at the same instant on their clock");
        assert_eq!(clock.shim_origin().scheduled.map(|s| s.at_ms), Some(next.at_ms + 5_000));
        // CodeRabbit on #110: once the change's instant is past, the drift is at the rate the pages run
        // at then - x1 here, where the segment still says x60.
        assert_eq!(clock.pages_drift_ms(next.at_ms + 1, 0), 5_000, "five seconds at x1");
        // The panel's real wall is the system clock's.
        match clock.state_event_at(session_now) {
            Event::State { real, .. } => assert_eq!(real.wall, epoch_ms_to_wall(wall_now, 0)),
            other => panic!("expected state, got {other:?}"),
        }
    }

    /// CodeRabbit on #110: the pages are given the clock again past a second of session time AND 50 ms
    /// of real time. The two clocks are read in whole milliseconds and disagree by one now and then, which
    /// at x10 000 is ten seconds of session time - every turn would have moved every page.
    #[test]
    fn a_resync_needs_session_time_and_real_time_both() {
        assert!(!resync_due(10_000, 10_000), "1 ms of real time at x10 000 is not a sleep");
        assert!(!resync_due(-20_000, 10_000), "nor 2 ms the other way");
        assert!(resync_due(500_000, 10_000), "50 ms at x10 000 is");
        assert!(!resync_due(999, 1), "under a second at x1 is not");
        assert!(resync_due(1_001, 1), "over a second at x1 is");
        assert!(!resync_due(2_400, 60), "40 ms at x60 is not");
        assert!(resync_due(3_000, 60), "50 ms at x60 is");
        assert!(!resync_due(0, 0), "a frozen clock never drifts");
    }

    /// A FILETIME to epoch milliseconds, and the end of the range does not come out as a date before
    /// the epoch: the wall standing at its last representable instant saturates on the way.
    #[test]
    fn a_filetime_becomes_epoch_milliseconds_without_wrapping() {
        assert_eq!(filetime_to_epoch_ms(FT_UNIX_EPOCH), 0);
        assert_eq!(filetime_to_epoch_ms(FT_UNIX_EPOCH + 10_000), 1);
        assert!(filetime_to_epoch_ms(i64::MAX) > 0, "the end of the range is far ahead, not behind");
    }

    /// The pages hold the origin they were last given and read `fake0 + (now - real0) * M` off the
    /// system clock. The host reads its own clock off interrupt time. Across a sleep the first keeps
    /// counting and the second does not, and the drift is exactly what the pages would read minus
    /// what the host reads (docs/09 section 12.5).
    #[test]
    fn the_drift_is_what_the_pages_read_minus_what_the_host_reads() {
        let pushed = origin(1_000_000, 500_000, 60);
        // Ten real seconds later, both agree: the pages read 1_000_000 + 10_000 * 60.
        let agreed = origin(1_600_000, 510_000, 60);
        assert_eq!(drift_ms(pushed, agreed), 0);
        // The machine slept for five of those seconds: the host's clock counted five, the pages'
        // system clock counted ten - the pages run 5 s * 60 ahead.
        let slept = origin(1_300_000, 510_000, 60);
        assert_eq!(drift_ms(pushed, slept), 300_000);
        // Frozen: the pages hold fake0 whatever the system clock does, and so does the host.
        let frozen = origin(1_000_000, 500_000, 0);
        assert_eq!(drift_ms(frozen, origin(1_000_000, 900_000, 0)), 0);
    }

    #[test]
    fn cdp_clock_saturates_instead_of_panicking() {
        // R2-W2: `chrono-cli` keeps the workspace's release overflow-checks, so an unsaturated add
        // panics the core mid-session - and a panicking CDP core never runs its shutdown, leaving a
        // launched Chromium with an open debug port and a temp profile behind. The multiplier bound
        // makes this unreachable from either surface - this is the layer behind it. The wall then
        // stands at the end of the session range, as the native one does (R4-S8).
        let c = CdpClock::new(i64::MAX - 1, 0, chrono_core::MULTIPLIER_MAX, 0);
        assert_eq!(c.fake_wall_ms(i64::MAX), WALL_MAX_MS);
        assert_eq!(c.elapsed_fake_ms(i64::MAX), i64::MAX);

        let mut m = CdpClock::new(i64::MAX - 1, 0, chrono_core::MULTIPLIER_MAX, 0);
        let next = m.set_multiplier_at(1, i64::MAX);
        assert_eq!((next.at_ms, next.mult), (i64::MAX, 1), "the instant saturates rather than wrapping");
        assert_eq!(m.fake_wall_ms(i64::MAX), WALL_MAX_MS);
        assert_eq!(m.elapsed_fake_ms(i64::MAX), i64::MAX);
        // A second change at the instant folds the first into the segment, through the same saturation.
        m.set_multiplier_at(2, i64::MAX);
        assert_eq!((m.shim_origin().fake0, m.shim_origin().mult), (WALL_MAX_MS, 1));
    }

    /// R4-S8: the wall stops where the native clock stops and the clock remembers it did, so the end
    /// report can say `time.fake_clock_clamped` - also after a jump took it back into the range.
    #[test]
    fn the_wall_stands_at_the_end_of_the_range_and_says_so() {
        // The last instant is the native clamp, 30828-09-13T11:48:05Z, as epoch ms.
        assert_eq!(epoch_ms_to_wall(WALL_MAX_MS, 0), "30828-09-13T11:48:05");
        let mut c = CdpClock::new(WALL_MAX_MS - 1_000, 0, chrono_core::MULTIPLIER_MAX, 0);
        assert!(!c.reached_range_end(0), "one second short of the end is not the end");
        assert_eq!(c.fake_wall_ms(1), WALL_MAX_MS, "a millisecond at x1e6 is past it");
        assert_eq!(c.fake_wall_ms(10_000), WALL_MAX_MS, "and the wall stays there");
        c.jump_to_at(0, 10_000);
        assert!(c.fake_wall_ms(10_000) < WALL_MAX_MS);
        assert!(c.reached_range_end(10_000), "a clock that stood at the end is reported after a jump back");
        // A clock that never got there says nothing.
        let ordinary = CdpClock::new(1_000_000, 0, 60, 0);
        assert!(!ordinary.reached_range_end(10_000));
    }

    #[test]
    fn cdp_clock_scales_and_holds_frozen() {
        // xN: at +1000 ms real the fake wall advanced 60000 ms, and elapsed_fake is 60000.
        let c = CdpClock::new(1_000_000, 500_000, 60, 0);
        assert_eq!(c.fake_wall_ms(501_000), 1_060_000);
        assert_eq!(c.elapsed_fake_ms(501_000), 60_000);
        // frozen (x0): the wall never moves, however much real time passes.
        let f = CdpClock::new(1_000_000, 500_000, 0, 0);
        assert_eq!(f.fake_wall_ms(505_000), 1_000_000);
        assert_eq!(f.elapsed_fake_ms(505_000), 0);
    }

    /// R4-S17: a rate change takes effect [`RATE_CHANGE_DELAY_MS`] after it is asked for. Until then
    /// the clock runs at the old rate, from then at the new one, the wall and the duration both going
    /// on from where they stand at that instant.
    #[test]
    fn a_rate_change_takes_effect_at_its_instant_and_accumulates() {
        let mut c = CdpClock::new(1_000_000, 500_000, 60, 0);
        // 1000 ms at x60, then x120 asked for at 501_000: it takes effect at 501_250.
        let next = c.set_multiplier_at(120, 501_000);
        assert_eq!(next, ScheduledRate { at_ms: 501_000 + RATE_CHANGE_DELAY_MS, mult: 120, dur: 120 });
        assert_eq!(c.fake_wall_ms(501_249), 1_060_000 + 249 * 60, "still x60 just before the instant");
        assert_eq!(c.fake_wall_ms(501_250), 1_075_000);
        // 500 ms more at x120: wall += 60000, elapsed = 60000 + 15000 (x60) + 60000 (x120).
        assert_eq!(c.fake_wall_ms(501_750), 1_135_000);
        assert_eq!(c.elapsed_fake_ms(501_750), 135_000);
        // A context attached meanwhile gets the segment as it was and the change to come.
        let expected = ShimOrigin { fake0: 1_000_000, real0: 500_000, mult: 60, dur: 60, scheduled: Some(next) };
        assert_eq!(c.shim_origin(), expected);
    }

    /// R4-S17, the case of the report: x1440 to x1. Read every millisecond across the instant,
    /// neither the wall nor the duration ever steps back, and after it both run at x1.
    #[test]
    fn slowing_down_steps_back_neither_the_wall_nor_the_duration() {
        let mut c = CdpClock::new(1_000_000, 0, 1_440, 0);
        let next = c.set_multiplier_at(1, 10_000);
        let (mut wall, mut elapsed) = (c.fake_wall_ms(9_999), c.elapsed_fake_ms(9_999));
        for now in 10_000..=next.at_ms + 1_000 {
            let (w, e) = (c.fake_wall_ms(now), c.elapsed_fake_ms(now));
            let step = if now > next.at_ms { 1 } else { 1_440 };
            assert_eq!((w - wall, e - elapsed), (step, step), "at {now}");
            (wall, elapsed) = (w, e);
        }
    }

    /// A second change inside the window replaces the first, which never takes effect, and keeps its
    /// instant. One asked for after the instant folds the first into the segment and is scheduled
    /// from its own moment.
    #[test]
    fn a_second_change_replaces_one_still_waiting_and_keeps_its_instant() {
        let mut c = CdpClock::new(0, 0, 60, 0);
        let first = c.set_multiplier_at(1_440, 1_000);
        let second = c.set_multiplier_at(1, 1_100);
        assert_eq!(second, ScheduledRate { at_ms: first.at_ms, mult: 1, dur: 1 });
        assert_eq!(c.fake_wall_ms(first.at_ms + 100), first.at_ms * 60 + 100, "x1440 never ran");
        let third = c.set_multiplier_at(60, first.at_ms + 100);
        assert_eq!(third.at_ms, first.at_ms + 100 + RATE_CHANGE_DELAY_MS);
        let folded = ShimOrigin { fake0: first.at_ms * 60, real0: first.at_ms, mult: 1, dur: 1, scheduled: Some(third) };
        assert_eq!(c.shim_origin(), folded);
    }

    /// A jump inside the window moves the wall and keeps the change scheduled, so the change takes
    /// effect at its instant from the new wall - which is also what a page that took the change before
    /// the jump reached it works out, from the whole clock the jump carries. A jump after the instant
    /// folds the change in first and runs at the new rate.
    #[test]
    fn a_jump_inside_the_window_keeps_the_change_and_takes_it_from_the_new_wall() {
        let mut c = CdpClock::new(0, 0, 60, 0);
        let next = c.set_multiplier_at(1, 1_000);
        c.jump_to_at(5_000_000, 1_100);
        let carried = ShimOrigin { fake0: 5_000_000, real0: 1_100, mult: 60, dur: 60, scheduled: Some(next) };
        assert_eq!(c.shim_origin(), carried);
        let at_instant = 5_000_000 + (next.at_ms - 1_100) * 60;
        assert_eq!(c.fake_wall_ms(next.at_ms), at_instant);
        assert_eq!(c.fake_wall_ms(next.at_ms + 10), at_instant + 10, "x1 from the instant");
        assert_eq!(c.elapsed_fake_ms(next.at_ms + 10), next.at_ms * 60 + 10, "the duration did not jump");
        c.jump_to_at(0, next.at_ms + 100);
        let after = ShimOrigin { fake0: 0, real0: next.at_ms + 100, mult: 1, dur: 1, scheduled: None };
        assert_eq!(c.shim_origin(), after);
    }

    /// The rate a `state` event carries is the one asked for, from the moment it is asked for: the
    /// window sets its list of rates from it, and the old rate in the event right after the `ack`
    /// would set the list back for a moment. The wall and the duration it carries run at the old rate
    /// until the instant.
    #[test]
    fn a_state_event_carries_the_rate_asked_for_and_the_clock_segment_by_segment() {
        let mut c = CdpClock::new(0, 0, 60, 0);
        c.set_multiplier_at(1, 1_000);
        let Event::State { multiplier, elapsed_fake_ms, .. } = c.state_event_at(1_100) else {
            panic!("a state event");
        };
        assert_eq!(multiplier, 1);
        assert_eq!(elapsed_fake_ms, 1_100 * 60, "still x60 before the instant");
    }

    /// The expressions only call the shim's own methods, so the clock logic lives in one place
    /// (ADR-9 R4/14b). The release is the shim's own (R5-B8), and on a shim of an older build it puts
    /// the wall on the real clock at rate 1 with nothing scheduled, the shim stops checking the zone,
    /// which is no longer the session's to judge, and its audit starts over for whoever reaches the page
    /// next (CodeRabbit on #89). What each does in a page is run in Node (`cdp::shim_exec`).
    #[test]
    fn the_expressions_call_the_shims_own_methods() {
        let next = ScheduledRate { at_ms: 9, mult: 0, dur: 0 };
        assert!(cdp_schedule_expr(next).contains("if(!S)return 'no-shim';return S.schedule(9,0,1);"));
        let o = ShimOrigin { fake0: 1, real0: 2, mult: 3, dur: 0, scheduled: Some(next) };
        assert!(cdp_set_expr(o).contains("return S.set(1,2,3,1,{ at: 9, M: 0, D: 1 });"), "{}", cdp_set_expr(o));
        assert!(cdp_release_expr().contains(
            "if(!S)return 'no-shim';if(typeof S.release==='function')return S.release();S.Z=null;S.zoneMissed=0;S.hiddenFast=0;for(var k in S.counts){S.counts[k]=0;}return S.set(0,0,1,1,null);"
        ));
    }

    #[test]
    fn cdp_clock_jump_moves_wall_not_duration() {
        let mut c = CdpClock::new(1_000_000, 500_000, 60, 0);
        let now = 501_000;
        let target = c.fake_wall_ms(now) + 86_400_000; // jump forward one day
        c.jump_to_at(target, now);
        assert_eq!(c.fake_wall_ms(now), target); // the wall jumped
        // But elapsed duration did not: still 60000 ms of fake time passed, not a day.
        assert_eq!(c.elapsed_fake_ms(now), 60_000);
        // A backward jump does not rewind elapsed duration either (rule 3).
        c.jump_to_at(1_000_000, now);
        assert_eq!(c.elapsed_fake_ms(now), 60_000);
    }

    #[test]
    fn cdp_clock_negative_rate_clamps_to_freeze() {
        let mut c = CdpClock::new(1_000_000, 500_000, 60, 0);
        let next = c.set_multiplier_at(-5, 501_000);
        assert_eq!(next.mult, 0); // never runs the wall backward
        assert_eq!(c.fake_wall_ms(next.at_ms + 1_000), c.fake_wall_ms(next.at_ms + 9_000)); // frozen after
    }

    fn spec(local: Option<&str>, bias: Option<i32>, mode: TimeModeKind, multiplier: Option<i64>) -> TimeSpec {
        TimeSpec {
            moment: MomentSpec {
                kind: MomentKind::Absolute,
                local: local.map(str::to_string),
                tz_bias_min: bias,
                delta: None,
            },
            mode,
            multiplier,
            scale_duration: false,
            scale_qpc: false,
        }
    }

    /// Untouchable rule 4 on the Chromium path: a moment this build cannot represent is an error the
    /// caller turns into `moment.invalid`, never a session that quietly runs on the real clock and
    /// reports as though it had substituted anything.
    #[test]
    fn a_moment_out_of_range_is_an_error_not_a_fall_back_to_real_time() {
        // `.err()` rather than comparing the whole Result: the type under test has no business
        // deriving Debug and PartialEq so that a test can print it.
        assert_eq!(
            CdpClock::from_time_spec(&spec(Some("not-a-moment"), Some(0), TimeModeKind::Flow, None), 1_000).err(),
            Some("moment.invalid")
        );
    }

    /// No `--at` at all is a pure offset or acceleration: the fake clock starts where the real one is,
    /// which is what makes `chrono run app.exe --mode x60` mean "faster, same date".
    #[test]
    fn no_moment_starts_the_fake_clock_at_real_now() {
        let clock = CdpClock::from_time_spec(&spec(None, Some(0), TimeModeKind::Flow, None), 1_700_000_000_000).unwrap();
        assert_eq!(clock.fake_wall_ms(1_700_000_000_000), 1_700_000_000_000);
    }

    /// The wire's mode becomes the rate the shim runs at. A multiplier of 0 is freeze, the same as the
    /// native session and `set_multiplier` - it used to be floored to x1 here, so one start command
    /// froze a native application and ran a Chromium one at real speed.
    #[test]
    fn the_mode_becomes_the_rate_the_shim_runs_at() {
        let rate = |mode: TimeModeKind, m: Option<i64>| {
            CdpClock::from_time_spec(&spec(None, Some(0), mode, m), 0).unwrap().shim_origin().mult
        };
        assert_eq!(rate(TimeModeKind::Flow, None), 1);
        assert_eq!(rate(TimeModeKind::Frozen, None), 0);
        assert_eq!(rate(TimeModeKind::Multiplier, Some(60)), 60);
        assert_eq!(rate(TimeModeKind::Multiplier, None), 1);
        assert_eq!(rate(TimeModeKind::Multiplier, Some(0)), 0, "zero is freeze on every path");
    }

    /// R4-S9: the Chromium start goes through the gate the native one does, and gets the same keys.
    #[test]
    fn a_chromium_start_is_held_to_the_native_gate() {
        let refusal = |s: TimeSpec| CdpClock::from_time_spec(&s, 0).err();
        assert_eq!(refusal(spec(None, Some(0), TimeModeKind::Unknown, None)), Some("time.bad_mode"));
        assert_eq!(refusal(spec(None, Some(0), TimeModeKind::Multiplier, Some(-1))), Some("time.bad_multiplier"));
        assert_eq!(
            refusal(spec(None, Some(0), TimeModeKind::Multiplier, Some(chrono_core::MULTIPLIER_MAX + 1))),
            Some("time.bad_multiplier")
        );
        assert_eq!(refusal(spec(None, None, TimeModeKind::Flow, None)), Some("time.bad_zone"), "a session never guesses a zone");
        assert_eq!(refusal(spec(None, Some(900), TimeModeKind::Flow, None)), Some("time.bad_zone"));
        assert_eq!(refusal(spec(None, Some(i32::MIN), TimeModeKind::Flow, None)), Some("time.bad_zone"));
        assert_eq!(refusal(spec(Some("30828-09-14T04:00:00"), Some(-120), TimeModeKind::Flow, None)), Some("moment.out_of_range"));
        assert_eq!(refusal(spec(Some("1600-12-31T20:00:00"), Some(300), TimeModeKind::Flow, None)), Some("moment.out_of_range"));
        assert_eq!(refusal(spec(Some("2030-06-15T12:00:00"), Some(899), TimeModeKind::Flow, None)), None);
    }

    fn jump_to(local: Option<&str>, bias: Option<i32>, delta: Option<&str>) -> MomentSpec {
        MomentSpec {
            kind: if delta.is_some() { MomentKind::Relative } else { MomentKind::Absolute },
            local: local.map(str::to_string),
            tz_bias_min: bias,
            delta: delta.map(str::to_string),
        }
    }

    /// A jump as a session takes it: read off the wire, then resolved on this clock.
    fn cdp_resolve_jump(clock: &CdpClock, to: &MomentSpec, now: i64) -> Result<i64, &'static str> {
        cdp_jump_target(clock, &read_jump(to)?, now)
    }

    /// R4-S9 and R4-S7 on the Chromium jump: no zone means the session zone, a named zone is kept,
    /// and a target outside the session range is refused, whichever way it was written.
    #[test]
    fn a_chromium_jump_reads_its_zone_like_the_native_one_and_stays_in_range() {
        let session_east2 = CdpClock::new(0, 0, 1, -120);
        let at = |bias: i32| moment_epoch_ms("2030-06-15T12:00:00", bias).unwrap();
        assert_eq!(cdp_resolve_jump(&session_east2, &jump_to(Some("2030-06-15T12:00:00"), None, None), 0), Ok(at(-120)));
        assert_eq!(cdp_resolve_jump(&session_east2, &jump_to(Some("2030-06-15T12:00:00"), Some(0), None), 0), Ok(at(0)));
        assert_eq!(
            cdp_resolve_jump(&session_east2, &jump_to(Some("2030-06-15T12:00:00"), Some(900), None), 0),
            Err("time.bad_zone")
        );
        assert_eq!(
            cdp_resolve_jump(&session_east2, &jump_to(Some("1000-01-01T00:00:00"), None, None), 0),
            Err("moment.out_of_range")
        );
        // The report's relative example: three hundred thousand days back from 1970.
        assert_eq!(cdp_resolve_jump(&session_east2, &jump_to(None, None, Some("-300000d")), 0), Err("moment.out_of_range"));
    }

    /// R5-P19: a relative jump keeps the milliseconds of the fake clock, as the native one does on ticks -
    /// through the civil text it lost up to 999 ms at every jump. R5-P18: a kind of moment neither
    /// mechanism knows is refused as it is natively, where it was read here as absolute.
    #[test]
    fn a_relative_chromium_jump_keeps_the_milliseconds_and_an_unknown_kind_is_refused() {
        let start = moment_epoch_ms("2030-01-01T00:00:00", 0).unwrap() + 750;
        let clock = CdpClock::new(start, 0, 1, 0);
        assert_eq!(cdp_resolve_jump(&clock, &jump_to(None, None, Some("+1s")), 0), Ok(start + 1_000));
        assert_eq!(cdp_resolve_jump(&clock, &jump_to(None, None, Some("-2h")), 0), Ok(start - 7_200_000));
        assert_eq!(cdp_resolve_jump(&clock, &jump_to(None, None, Some("+5bd")), 0), Err("moment.needs_calendar"));
        let odd = MomentSpec {
            kind: MomentKind::Unknown,
            local: Some("2030-06-15T12:00:00".into()),
            tz_bias_min: None,
            delta: None,
        };
        assert_eq!(cdp_resolve_jump(&clock, &odd, 0), Err("moment.unsupported_kind"));
    }

    /// Untouchable rule 2: the same wall text under two session zones is two different instants, and
    /// the bias that travels with the moment is the one it was read in. The bias is the WINDOWS one,
    /// `UTC = local + bias`, so a local zone of UTC+02:00 is MINUS 120 and its instant falls two hours
    /// earlier than the same text read as UTC. This test was first written with the sign the other way
    /// round and went red, which is how the convention came to be checked here rather than assumed.
    #[test]
    fn the_moment_is_read_in_the_session_zone() {
        let as_utc = CdpClock::from_time_spec(&spec(Some("2038-01-19T03:14:07"), Some(0), TimeModeKind::Flow, None), 0).unwrap();
        let as_east2 = CdpClock::from_time_spec(&spec(Some("2038-01-19T03:14:07"), Some(-120), TimeModeKind::Flow, None), 0).unwrap();
        assert_eq!(
            as_utc.fake_wall_ms(0) - as_east2.fake_wall_ms(0),
            120 * 60 * 1_000,
            "the same wall text in UTC+02:00 names an instant two hours earlier than in UTC"
        );
    }
}
