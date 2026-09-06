//! The clock a Chromium session runs on, and the arithmetic that moves it.
//!
//! Computed entirely Rust-side so the panel matches the app's own `Date.now()` with no browser
//! round-trip, and shared with the shim so the two cannot drift: every context is shimmed from this
//! clock's CURRENT origin, not from the session's initial values.

use chrono_core::calc::{Base, EvalContext, MomentExpr};
use chrono_proto::{Clock, Event, MomentSpec, TimeSpec, PROTOCOL_VERSION};

use crate::events::jump_error_key;
use crate::grammar::parse_shift;
use crate::zone::{epoch_ms_to_wall, moment_epoch_ms};

/// The live clock of a CDP session, computed entirely Rust-side so the panel matches the app's own
/// `Date.now()` with no browser round-trip. The wall origin (`wall_fake0` at `wall_real0`, rate `mult`)
/// is re-anchored on a rate change or a jump and pushed identically to every JS context, so all
/// contexts and the panel share one absolute origin. A separate duration accumulator keeps
/// `elapsed_fake` an honest integral of the rate over time - a jump moves the wall but adds no elapsed
/// duration - mirroring the native split between elapsed time and the fake wall reached.
pub(crate) struct CdpClock {
    wall_fake0: i64,
    wall_real0: i64,
    mult: i64,
    bias: i32,
    session_real0: i64,
    dur_fake_accum: i64,
    dur_real0: i64,
}

impl CdpClock {
    fn new(fake0_ms: i64, real0_ms: i64, mult: i64, bias: i32) -> Self {
        CdpClock {
            wall_fake0: fake0_ms,
            wall_real0: real0_ms,
            mult,
            bias,
            session_real0: real0_ms,
            dur_fake_accum: 0,
            dur_real0: real0_ms,
        }
    }

    /// The fake wall instant (epoch ms) at `now`: the current segment's origin plus scaled real time.
    /// Frozen (mult 0) holds it at the origin; xN accelerates.
    pub(crate) fn fake_wall_ms(&self, now: i64) -> i64 {
        // Saturating on BOTH operations, not just the product. `chrono-cli` keeps the workspace's
        // release `overflow-checks`, so an unsaturated add here panics the core mid-session (R2-W2) -
        // and a panicking CDP core never runs its shutdown, leaving a launched Chromium with an open
        // debug port and a temp profile behind. Defence in depth behind the multiplier bound.
        self.wall_fake0.saturating_add((now - self.wall_real0).saturating_mul(self.mult))
    }

    /// Fake duration elapsed (the integral of the rate): the accumulator plus the current segment. A
    /// jump does not touch this, so a wall discontinuity is never counted as elapsed time.
    pub(crate) fn elapsed_fake_ms(&self, now: i64) -> i64 {
        self.dur_fake_accum.saturating_add((now - self.dur_real0).saturating_mul(self.mult))
    }

    /// A `state` event at `now` (passed in so the mapping is pure and unit-testable).
    pub(crate) fn state_event_at(&self, now: i64) -> Event {
        Event::State {
            v: PROTOCOL_VERSION,
            fake: Clock {
                wall: epoch_ms_to_wall(self.fake_wall_ms(now), self.bias),
                zone_bias_min: self.bias,
            },
            real: Clock { wall: epoch_ms_to_wall(now, self.bias), zone_bias_min: self.bias },
            multiplier: self.mult,
            elapsed_fake_ms: self.elapsed_fake_ms(now),
            elapsed_real_ms: self.elapsed_real_ms(now),
        }
    }

    /// Re-anchor for a new multiplier at `now`: the wall and the duration both continue from where they
    /// are, so neither jumps (rule 3); only the future rate changes. A negative rate would run the wall
    /// backward, which is never valid, so it clamps to 0 (freeze). Returns (fake0, real0, mult) to push
    /// to the shim.
    pub(crate) fn set_multiplier_at(&mut self, m: i64, now: i64) -> (i64, i64, i64) {
        self.dur_fake_accum =
            self.dur_fake_accum.saturating_add((now - self.dur_real0).saturating_mul(self.mult));
        self.dur_real0 = now;
        self.wall_fake0 = self.fake_wall_ms(now);
        self.wall_real0 = now;
        self.mult = m.max(0);
        (self.wall_fake0, self.wall_real0, self.mult)
    }

    /// Re-anchor for a jump to a new fake wall at `now`: the wall moves and continues at the same rate;
    /// the duration axis is untouched, so a backward jump never rewinds elapsed time (rule 3). Returns
    /// (fake0, real0) to push to the shim.
    pub(crate) fn jump_to_at(&mut self, new_fake_ms: i64, now: i64) -> (i64, i64) {
        self.wall_fake0 = new_fake_ms;
        self.wall_real0 = now;
        (self.wall_fake0, self.wall_real0)
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
    pub(crate) fn from_time_spec(time: &TimeSpec, real_now_ms: i64) -> Result<Self, &'static str> {
        let bias = time.moment.tz_bias_min.unwrap_or(0);
        let fake_start_ms = match time.moment.local.as_deref() {
            Some(local) => moment_epoch_ms(local, time.moment.tz_bias_min).ok_or("moment.invalid")?,
            None => real_now_ms, // no --at: the fake clock starts at real now (pure offset/acceleration)
        };
        // flow = x1 (a plain wall offset), xN accelerates, frozen = x0 (the wall is held; the shim keeps
        // timers real via TS = M || 1).
        let mult = match time.mode.as_str() {
            "multiplier" => time.multiplier.unwrap_or(1).max(1),
            "frozen" => 0,
            _ => 1,
        };
        Ok(CdpClock::new(fake_start_ms, real_now_ms, mult, bias))
    }

    /// Real milliseconds since the session started. One source for the subtraction, which the
    /// heartbeat and the closing `ended` both need and used to spell out separately.
    pub(crate) fn elapsed_real_ms(&self, now: i64) -> i64 {
        now - self.session_real0
    }

    /// The origin a newly attached context must be shimmed from: the CURRENT wall origin and rate,
    /// not the session's initial values, so a context attaching after an in-flight change starts on
    /// the same clock as every other one (rule 3).
    pub(crate) fn shim_origin(&self) -> (i64, i64, i64) {
        (self.wall_fake0, self.wall_real0, self.mult)
    }
}

/// Resolve a CDP jump target to a fake epoch-ms instant: an absolute moment in the session zone, or a
/// relative delta applied to the CURRENT fake instant through the shared calc evaluator (the same
/// grammar as `calc` and the native jump). Returns a translation key on a bad moment (rule 6).
pub(crate) fn cdp_resolve_jump(clock: &CdpClock, to: &MomentSpec, now: i64) -> Result<i64, &'static str> {
    if to.kind == "relative" {
        let delta = to.delta.as_deref().ok_or("moment.invalid")?;
        let cur_wall = epoch_ms_to_wall(clock.fake_wall_ms(now), clock.bias);
        let cur_civil = chrono_core::calc::parse_civil_datetime(&cur_wall).map_err(|_| "moment.invalid")?;
        let step = parse_shift(delta).map_err(|_| "moment.invalid")?;
        let expr = MomentExpr { base: Base::Now, steps: vec![step] };
        let outcome = chrono_core::calc::eval(
            &expr,
            &EvalContext { now: cur_civil, zone_bias_min: 0, calendar: None },
        )
        .map_err(jump_error_key)?;
        moment_epoch_ms(&outcome.result().to_iso(), Some(clock.bias)).ok_or("moment.invalid")
    } else {
        let local = to.local.as_deref().ok_or("moment.invalid")?;
        moment_epoch_ms(local, Some(clock.bias)).ok_or("moment.invalid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdp_clock_saturates_instead_of_panicking() {
        // R2-W2: `chrono-cli` keeps the workspace's release overflow-checks, so an unsaturated add
        // panics the core mid-session - and a panicking CDP core never runs its shutdown, leaving a
        // launched Chromium with an open debug port and a temp profile behind. The multiplier bound
        // makes this unreachable from either surface; this is the layer behind it.
        let c = CdpClock::new(i64::MAX - 1, 0, chrono_core::MULTIPLIER_MAX, 0);
        assert_eq!(c.fake_wall_ms(i64::MAX), i64::MAX);
        assert_eq!(c.elapsed_fake_ms(i64::MAX), i64::MAX);

        let mut m = CdpClock::new(i64::MAX - 1, 0, chrono_core::MULTIPLIER_MAX, 0);
        let (fake0, _, mult) = m.set_multiplier_at(1, i64::MAX);
        assert_eq!(fake0, i64::MAX);
        assert_eq!(mult, 1);
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

    #[test]
    fn cdp_clock_rate_change_is_continuous_and_accumulates() {
        let mut c = CdpClock::new(1_000_000, 500_000, 60, 0);
        // Run 1000 ms at x60, then switch to x120 at now = 501_000.
        let (fake0, real0, m) = c.set_multiplier_at(120, 501_000);
        assert_eq!(m, 120);
        // The wall is continuous across the change: the fake instant at the switch is unchanged.
        assert_eq!(fake0, 1_060_000);
        assert_eq!(real0, 501_000);
        assert_eq!(c.fake_wall_ms(501_000), 1_060_000);
        // 500 ms more at x120: wall += 60000, elapsed_fake = 60000 (seg 1) + 60000 (seg 2).
        assert_eq!(c.fake_wall_ms(501_500), 1_120_000);
        assert_eq!(c.elapsed_fake_ms(501_500), 120_000);
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
        let (_, _, m) = c.set_multiplier_at(-5, 501_000);
        assert_eq!(m, 0); // never runs the wall backward
        assert_eq!(c.fake_wall_ms(502_000), c.fake_wall_ms(509_000)); // frozen after
    }

    fn spec(local: Option<&str>, bias: Option<i32>, mode: &str, multiplier: Option<i64>) -> TimeSpec {
        TimeSpec {
            moment: MomentSpec {
                kind: "absolute".into(),
                local: local.map(str::to_string),
                tz_bias_min: bias,
                delta: None,
            },
            mode: mode.into(),
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
            CdpClock::from_time_spec(&spec(Some("not-a-moment"), Some(0), "flow", None), 1_000).err(),
            Some("moment.invalid")
        );
    }

    /// No `--at` at all is a pure offset or acceleration: the fake clock starts where the real one is,
    /// which is what makes `chrono run app.exe --mode x60` mean "faster, same date".
    #[test]
    fn no_moment_starts_the_fake_clock_at_real_now() {
        let clock = CdpClock::from_time_spec(&spec(None, None, "flow", None), 1_700_000_000_000).unwrap();
        assert_eq!(clock.fake_wall_ms(1_700_000_000_000), 1_700_000_000_000);
    }

    /// The wire's mode becomes the rate the shim runs at, including the floor on an accelerated rate.
    #[test]
    fn the_mode_becomes_the_rate_the_shim_runs_at() {
        let rate = |mode: &str, m: Option<i64>| {
            CdpClock::from_time_spec(&spec(None, None, mode, m), 0).unwrap().shim_origin().2
        };
        assert_eq!(rate("flow", None), 1);
        assert_eq!(rate("frozen", None), 0);
        assert_eq!(rate("multiplier", Some(60)), 60);
        assert_eq!(rate("multiplier", Some(0)), 1, "an accelerated session never runs slower than real");
    }

    /// Untouchable rule 2: the same wall text under two session zones is two different instants, and
    /// the bias that travels with the moment is the one it was read in. The bias is the WINDOWS one,
    /// `UTC = local + bias`, so a local zone of UTC+02:00 is MINUS 120 and its instant falls two hours
    /// earlier than the same text read as UTC. This test was first written with the sign the other way
    /// round and went red, which is how the convention came to be checked here rather than assumed.
    #[test]
    fn the_moment_is_read_in_the_session_zone() {
        let as_utc = CdpClock::from_time_spec(&spec(Some("2038-01-19T03:14:07"), Some(0), "flow", None), 0).unwrap();
        let as_east2 = CdpClock::from_time_spec(&spec(Some("2038-01-19T03:14:07"), Some(-120), "flow", None), 0).unwrap();
        assert_eq!(
            as_utc.fake_wall_ms(0) - as_east2.fake_wall_ms(0),
            120 * 60 * 1_000,
            "the same wall text in UTC+02:00 names an instant two hours earlier than in UTC"
        );
    }
}
