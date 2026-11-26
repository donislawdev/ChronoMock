//! Chrono Mock logical core - pure time-substitution logic.
//!
//! Rule 16 (untouchable): the core knows nothing of CLI, GUI, I/O, or language.
//! It takes plain data in and returns plain values out - no side effects
//! (zasady/06 section 7). Everything here is unit-testable without a target
//! process or an operating system.
//!
//! This crate is the session model. The date arithmetic it reads moments with - parsing, the civil
//! calendar, a step on an instant - is the `chronomock` date library beneath it, which knows nothing
//! of sessions and runs on any system.

use chronomock::calc::{parse_civil_datetime, step_instant, EvalError, Step};
use chronomock::{zone_bias_in_range, ZONE_BIAS_MAX_MIN};

/// How the session expresses the target moment.
///
/// Semantics (docs/01 section 4, untouchable rule 2): the entered moment is in
/// the SESSION zone, everything is UTC internally, and the zone is always explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    /// Local wall-clock text in the session zone, e.g. "2038-01-19T03:14:07".
    pub local: String,
    /// Session zone bias in minutes (UTC = local + bias), no DST.
    ///
    /// A session never runs without one: [`session_instant`] refuses `None` (untouchable rule 2, the
    /// zone is always explicit). This used to promise "`None` = host zone" while every consumer
    /// computed UTC instead, so a client that left the field out got a clock hours away from the one
    /// the documentation described. The plain conversion [`moment_to_filetime_utc`] still reads `None`
    /// as UTC, for the calculator paths that state their zone some other way.
    pub tz_bias_min: Option<i32>,
}

/// How time flows once the moment is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeMode {
    /// Time flows from the moment at real speed.
    Flow,
    /// Time is frozen at the moment.
    Frozen,
    /// Time is accelerated (or slowed) by an integer multiplier.
    Multiplier(i64),
}

/// The largest accepted time multiplier.
///
/// Chosen from the measured distance between a typical session moment and the end of the FILETIME
/// range, not from taste. Starting at 2038 there are ~9.1e18 ticks of headroom, which the fake clock
/// burns through in:
///
/// | multiplier | real time until the fake clock leaves the FILETIME range |
/// |---|---|
/// | 1440 (a day per minute - the largest documented use) | ~20 years |
/// | 86_400 (a day per second) | ~122 days |
/// | **1_000_000 (this limit)** | **~10.5 days** |
/// | 10_000_000 | ~1.1 days - inside a session left over a weekend |
///
/// So this is where the headroom stops outliving any plausible session while still allowing 694x
/// more than anyone has ever asked for. Past the range the substitution does not merely saturate,
/// it comes apart: measured at x1e15, `GetSystemTimeAsFileTime` returned a wrapped nonsense instant
/// while `GetSystemTime` silently fell back to the REAL clock (its conversion rejects the instant),
/// and successive reads jumped backwards and forwards between centuries - two epochs in one process
/// and a non-monotonic clock, against untouchable rules 2 and 3, with the audit still saying `works`.
pub const MULTIPLIER_MAX: i64 = 1_000_000;

/// The smallest accepted time multiplier. Zero is the wire spelling of "freeze" that the GUI's
/// freeze button sends, so it is a value, not an error. Anything negative would run the wall clock
/// CONTINUOUSLY BACKWARD, which the time model does not have: going back is a `jump`, an event, never
/// a rate (docs/01 section 5.1, untouchable rule 3). Measured before this bound existed: a multiplier
/// of -1_000_000 accepted over the protocol had the target read 31, then 29, then 28, then 26
/// December - and the session still reported `works`.
pub const MULTIPLIER_MIN: i64 = 0;

/// Whether a multiplier is one the session may run at. The single gate both surfaces use, so the CLI
/// and the protocol cannot drift apart on what they accept (they had: the CLI required `>= 1` while
/// the protocol accepted anything at all).
pub fn multiplier_in_range(m: i64) -> bool {
    (MULTIPLIER_MIN..=MULTIPLIER_MAX).contains(&m)
}

/// The last instant a session clock may name, as a UTC FILETIME (100 ns ticks since 1601).
///
/// A mirror of `chrono_ctl::FAKE_WALL_MAX`, where the reasoning lives: the last whole second that
/// leaves room for the largest zone bias, so the local-time channels can still express it. It is
/// restated here rather than imported because this crate depends on nothing (rule 16), and
/// `chrono-mech`, which sees both crates, asserts at compile time that the two are equal and that
/// [`ZONE_BIAS_MAX_MIN`] fits the headroom - so they cannot drift apart without the build failing.
///
/// That is why a session checks its zone against the date library's band (R4-S9). The protocol took
/// any `i32` and nothing checked it, which broke the assumption this range is built on: it stops
/// fifteen hours short of the end of the FILETIME range precisely so that `fake - bias` still fits
/// for every accepted zone. A bias past that can make the hook's local-time channels overflow and
/// fall back to the REAL clock while the UTC channels stay fake - the failure R2-X7 measured at the
/// top of the range.
pub const FAKE_WALL_MAX: i64 = {
    // Fifteen hours: more than the widest accepted zone (14:59), as `chrono_ctl::MAX_ZONE_BIAS_TICKS`.
    const ZONE_HEADROOM_TICKS: i64 = 15 * 3600 * 10_000_000;
    let raw = i64::MAX - ZONE_HEADROOM_TICKS;
    raw - (raw % 10_000_000)
};

/// Why a moment cannot become the clock of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMomentError {
    /// The zone is missing, or outside [`ZONE_BIAS_MAX_MIN`] either side of UTC.
    BadZone,
    /// The text is not a date and time at all. Carries the parser's reason for the human half.
    NotAMoment(String),
    /// A real date the session clock cannot hold: before 1601-01-01 in UTC or in the session zone, or
    /// after [`FAKE_WALL_MAX`].
    OutOfRange,
}

impl std::fmt::Display for SessionMomentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionMomentError::BadZone => write!(
                f,
                "the session zone is missing or outside -14:59..=+14:59 (bias -{ZONE_BIAS_MAX_MIN}..={ZONE_BIAS_MAX_MIN} minutes)"
            ),
            SessionMomentError::NotAMoment(reason) => f.write_str(reason),
            SessionMomentError::OutOfRange => {
                f.write_str("the moment is outside the range a session clock can hold (years 1601 to 30828)")
            }
        }
    }
}

/// Whether a UTC instant can be the session clock in a zone: at or after 1601-01-01 both in UTC and
/// as local time in that zone, and not after [`FAKE_WALL_MAX`].
///
/// Both floors, because the hook serves the same instant twice. The UTC channels read the FILETIME,
/// the local ones read `FILETIME - bias`, and a FILETIME below zero is refused by the conversion every
/// calendar channel goes through, which then answers with the REAL clock (R4-S7d). Checking UTC alone
/// let `1600-12-31T20:00:00` at UTC-05:00 through: fine in UTC, before 1601 locally, and
/// `GetLocalTime` in the target showed today's date. The top needs no local check, because the
/// headroom under [`FAKE_WALL_MAX`] is there for exactly that.
pub fn instant_in_session_range(ft_utc: i64, bias_min: i32) -> bool {
    let local = ft_utc.checked_sub((bias_min as i64) * 60 * 10_000_000);
    (0..=FAKE_WALL_MAX).contains(&ft_utc) && local.is_some_and(|l| l >= 0)
}

/// The UTC FILETIME a session clock starts at, or is jumped to, with the zone it was read in: the one
/// gate every moment passes before it can become a session clock (R4-S7, R4-S9).
///
/// [`moment_to_filetime_utc`] answers a narrower question - does this text name an instant FILETIME
/// can hold - and it was the only check. So a moment past [`FAKE_WALL_MAX`] started a session whose
/// first rate change wrapped the clock to 1601, a zone far outside any real one reached the hook
/// unchecked, and a moment before 1601 in the session zone ran the local-time channels on the real
/// clock. This checks the zone first (a moment cannot be read without one), then the text, then the
/// instant against both ends of the session range.
pub fn session_instant(moment: &Moment) -> Result<(i64, i32), SessionMomentError> {
    let bias = moment
        .tz_bias_min
        .filter(|&b| zone_bias_in_range(b))
        .ok_or(SessionMomentError::BadZone)?;
    let civil = parse_civil_datetime(&moment.local).map_err(SessionMomentError::NotAMoment)?;
    let ft = civil
        .to_filetime_utc(bias)
        .filter(|&ft| instant_in_session_range(ft, bias))
        .ok_or(SessionMomentError::OutOfRange)?;
    Ok((ft, bias))
}

/// Everything needed to define a session. Pure data, no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    pub moment: Moment,
    pub mode: TimeMode,
    /// Whether the monotonic duration axis is scaled too (E4 / ADR-2).
    pub scale_duration: bool,
    /// Whether QueryPerformanceCounter is scaled too (ADR-2 reversal, opt-in). Separate from
    /// scale_duration because scaling QPC also scales a target's QPC-timed rendering (a risk the tick
    /// axis does not carry), so it is a deliberate, separate choice.
    pub scale_qpc: bool,
}

/// The verdict about whether time substitution took effect (chrono-mock.md 7.1).
///
/// `Undetermined` is genuine product vocabulary, not a placeholder: it is the
/// honest state when coverage cannot be established (mechanism not applied, target
/// vanished, audit incomplete). The core NEVER reports `Works` when it does not
/// know - that is the whole promise (untouchable rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Works,
    Partial,
    Fails,
    Undetermined,
}

impl Verdict {
    /// Stable wire token for this verdict (docs/08).
    pub fn wire(&self) -> &'static str {
        match self {
            Verdict::Works => "works",
            Verdict::Partial => "partial",
            Verdict::Fails => "fails",
            Verdict::Undetermined => "undetermined",
        }
    }

    /// Process exit code carried by this verdict (docs/08 section 8).
    pub fn exit_code(&self) -> i32 {
        match self {
            Verdict::Works => 0,
            Verdict::Partial => 10,
            Verdict::Fails => 11,
            Verdict::Undetermined => 4,
        }
    }

    /// Combine two verdicts into the family (session-wide) verdict. Each verdict encodes
    /// exactly the pair `(has_covered, has_uncovered)` - the family ORs those bits across
    /// processes, so the family `works` only when something is covered and nothing is left
    /// uncovered anywhere (untouchable rule 4 at the session level). This aggregates
    /// JUDGMENTS, never call counts - per-process reports stay separate (slice 11). OR
    /// is commutative and associative, so accumulation order does not matter, and coverage
    /// only grows, so the fold is monotonic.
    pub fn combine(self, other: Verdict) -> Verdict {
        let (c1, u1) = self.coverage_bits();
        let (c2, u2) = other.coverage_bits();
        Verdict::from_coverage_bits(c1 || c2, u1 || u2)
    }

    /// The `(has_covered, has_uncovered)` pair this verdict encodes.
    fn coverage_bits(self) -> (bool, bool) {
        match self {
            Verdict::Undetermined => (false, false),
            Verdict::Works => (true, false),
            Verdict::Fails => (false, true),
            Verdict::Partial => (true, true),
        }
    }

    /// The verdict for a coverage-flag pair - the single source of the coverage->verdict
    /// table, shared by `verdict_from_coverage` and the family fold (`combine`).
    fn from_coverage_bits(has_covered: bool, has_uncovered: bool) -> Verdict {
        match (has_covered, has_uncovered) {
            (false, false) => Verdict::Undetermined,
            (true, false) => Verdict::Works,
            (false, true) => Verdict::Fails,
            (true, true) => Verdict::Partial,
        }
    }
}

/// One time channel's coverage under a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelCoverage {
    pub channel: String,
    pub calls: u64,
    /// The count stopped at the hook's ceiling (`chrono_ctl::CALLS_CAP`, R5-WY11), so `calls` is a floor:
    /// the application called the channel at least that many times. Said beside the number rather than
    /// left for a reader to recognise the ceiling, which is an internal value that may change.
    pub capped: bool,
}

/// Audit result: channels `covered` (time substituted), `observed` (hooked and counted but
/// deliberately left real - ADR-7 class B object waits), `uncovered` (queried but not covered),
/// and any warning keys. Gathering (mechanism layer) is separated from evaluation here
/// (zasady/06 section 14).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub covered: Vec<ChannelCoverage>,
    pub observed: Vec<ChannelCoverage>,
    pub uncovered: Vec<String>,
    /// Channels this session meant to WATCH and could not hook. Its own bucket rather than
    /// `uncovered`, because the two answer different questions and only one of them is a verdict:
    /// an uncovered channel means the target asked for time we did not substitute, while one of
    /// these means we are not watching something we said we would. Folding them together would
    /// turn a failed observer into a `Partial` session, which would be a wrong answer to
    /// "did the substitution take effect".
    ///
    /// It exists because the alternative was silence. An observed channel that failed to install
    /// used to produce NO line at all - not covered, not uncovered, absent - so the report looked
    /// complete while a watch it promised was simply not running (untouchable rules 4 and 6).
    pub unobserved: Vec<String>,
    /// Channels that came under the fake clock only once their module loaded (ADR-10), by name.
    ///
    /// Every one of them is also in `covered` or `observed` with its count - this does not add a
    /// channel, it says which counts are floors and which scaled clocks jumped once when they joined.
    /// It exists because the warning alone said "a channel" and left the reader to guess which.
    pub installed_late: Vec<String>,
    pub warning_keys: Vec<String>,
}

/// Compute the verdict from gathered coverage.
///
/// An uncovered-but-queried channel is the CAUSE of a `Partial` verdict - it does
/// not have a separate exit code (docs/08 section 8). No evidence at all yields
/// `Undetermined`, never a fake `Works`. `observed` channels (ADR-7 class B, hooked
/// but deliberately left real) never sway the verdict - they are an honest side-channel
/// carried by their own warning, so an object wait left real neither makes nor breaks `works`.
pub fn verdict_from_coverage(cov: &Coverage) -> Verdict {
    Verdict::from_coverage_bits(!cov.covered.is_empty(), !cov.uncovered.is_empty())
}

/// Convert a session moment to a UTC FILETIME (100 ns ticks since 1601-01-01).
pub fn moment_to_filetime_utc(moment: &Moment) -> Result<i64, String> {
    // The year band is enforced by the parser, so by here the civil math is sound and what can
    // still fail is the instant: FILETIME saturates i64 near year 30828, far inside the band, so
    // the checked conversion below is what rejects an out-of-FILETIME-range moment - as an honest
    // Err, never a panic (debug) or a wrapped number (release).
    let civil = parse_civil_datetime(&moment.local)?;
    civil
        .to_filetime_utc(moment.tz_bias_min.unwrap_or(0))
        .ok_or_else(|| format!("moment '{}' is out of the representable FILETIME range", moment.local))
}

/// The target UTC FILETIME after applying ONE step to the current fake instant - the single source of
/// truth for "current fake + one step" on the substitution jump path. The step itself is the date
/// library's [`chronomock::calc::step_instant`]: a fixed-length unit adds a tick delta (sub-second
/// precision preserved), a calendar unit folds through the civil date in the session zone. Business
/// days need a calendar, which a session does not have, so they are refused as `NeedsCalendar`.
///
/// A target outside the session range is `Overflow`: rejected, never clamped (R4-S7c). Only an i64
/// overflow was caught before, so `jump -300000d` was accepted and parked the clock on 1601-01-01 with
/// nothing said, and in a zone west of UTC the local-time channels went back to the real clock. The
/// range is the one [`session_instant`] holds a start to, so a jump cannot reach an instant the
/// session could not have started at.
pub fn step_target(fake_now_ft: i64, tz_bias_min: i32, step: &Step) -> Result<i64, EvalError> {
    let target = step_instant(fake_now_ft, tz_bias_min, step)?;
    if !instant_in_session_range(target, tz_bias_min) {
        return Err(EvalError::Overflow { index: 0 });
    }
    Ok(target)
}

// The relative-delta grammar (fixed-length ticks) was retired into the shared step model of the
// date library: fixed units live in `chronomock::calc::fixed_shift_ticks`, a step on an instant in
// `chronomock::calc::step_instant`, and parsing in the CLI's `parse_shift`. One grammar for `--at`,
// `jump`, and the calculator.

#[cfg(test)]
mod tests {
    use super::*;
    use chronomock::calc::{Sign, Unit};
    use chronomock::{calc, filetime_utc_to_wall};

    fn ch(name: &str) -> ChannelCoverage {
        ChannelCoverage { channel: name.to_string(), calls: 1, capped: false }
    }

    #[test]
    fn multiplier_range_admits_freeze_and_every_documented_speed() {
        // Freeze is a VALUE on the wire, not an error - the GUI's freeze button sends 0.
        assert!(multiplier_in_range(0));
        // Every speed the product actually offers, and the bound itself.
        for m in [1, 10, 60, 1440, 86_400, MULTIPLIER_MAX] {
            assert!(multiplier_in_range(m), "x{m} must be allowed");
        }
        // Backward is a jump, never a rate (rule 3) - past the bound the clock leaves the
        // representable range mid-session (rule 2). Both measured before this gate existed.
        for m in [-1, -1_000_000, i64::MIN, MULTIPLIER_MAX + 1, i64::MAX] {
            assert!(!multiplier_in_range(m), "x{m} must be refused");
        }
    }

    fn moment(local: &str, bias: Option<i32>) -> Moment {
        Moment { local: local.to_string(), tz_bias_min: bias }
    }

    #[test]
    fn unix_epoch_utc_is_known_filetime() {
        // 1970-01-01T00:00:00 UTC == 116444736000000000 in FILETIME ticks.
        let ft = moment_to_filetime_utc(&moment("1970-01-01T00:00:00", Some(0))).unwrap();
        assert_eq!(ft, 116_444_736_000_000_000);
    }

    #[test]
    fn bias_shifts_local_to_utc() {
        // Local 03:14:07 in a UTC-5 session (bias +300) is 08:14:07 UTC.
        let with_bias = moment_to_filetime_utc(&moment("2038-01-19T03:14:07", Some(300))).unwrap();
        let as_utc = moment_to_filetime_utc(&moment("2038-01-19T08:14:07", Some(0))).unwrap();
        assert_eq!(with_bias, as_utc);
    }

    #[test]
    fn wall_round_trips_through_filetime() {
        for (s, bias) in [
            ("2038-01-19T03:14:07", 0),
            ("1970-01-01T00:00:00", 0),
            ("2000-02-29T12:00:00", 300),
            ("2026-08-12T20:30:00", -120),
        ] {
            let ft = moment_to_filetime_utc(&moment(s, Some(bias))).unwrap();
            assert_eq!(filetime_utc_to_wall(ft, bias), s);
        }
    }

    /// The last instant an i64 holds, in the zones at both ends of the accepted band, is written out
    /// rather than panicking - it used to overflow subtracting the bias in ticks for every zone east
    /// of UTC (R4-S8). The value is the known end of the FILETIME range, 30828-09-14T02:48:05 UTC.
    #[test]
    fn the_wall_text_of_the_last_instant_never_overflows() {
        assert_eq!(filetime_utc_to_wall(i64::MAX, 0), "30828-09-14T02:48:05");
        assert_eq!(filetime_utc_to_wall(i64::MAX, -120), "30828-09-14T04:48:05");
        assert_eq!(filetime_utc_to_wall(i64::MAX, -899), "30828-09-14T17:47:05");
        // The other end, in a zone west of UTC, overflowed the same way in the other direction.
        assert!(filetime_utc_to_wall(i64::MIN, 899).starts_with('-'));
        // Dividing to seconds first changes nothing below a second: a tick short of the next second
        // still reads as this one, in a zone on either side of UTC.
        let ft = moment_to_filetime_utc(&moment("2026-08-12T20:30:00", Some(0))).unwrap() + 9_999_999;
        assert_eq!(filetime_utc_to_wall(ft, -120), "2026-08-12T22:30:00");
        assert_eq!(filetime_utc_to_wall(ft, 300), "2026-08-12T15:30:00");
        assert_eq!(filetime_utc_to_wall(-1, 0), "1600-12-31T23:59:59");
    }

    #[test]
    fn space_separator_is_accepted() {
        let a = moment_to_filetime_utc(&moment("2000-02-29 12:00:00", Some(0))).unwrap();
        let b = moment_to_filetime_utc(&moment("2000-02-29T12:00:00", Some(0))).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn bad_moment_is_rejected() {
        assert!(moment_to_filetime_utc(&moment("not-a-date", None)).is_err());
        assert!(moment_to_filetime_utc(&moment("2038-13-01T00:00:00", None)).is_err());
    }

    #[test]
    fn impossible_day_is_rejected_on_the_substitution_path() {
        // The substitution moment must refuse an impossible day just like the calculator does,
        // instead of silently rolling it into the next month (rule 4/6).
        assert!(moment_to_filetime_utc(&moment("2026-02-31T12:00:00", Some(0))).is_err()); // Feb has 28
        assert!(moment_to_filetime_utc(&moment("2025-02-29T00:00:00", Some(0))).is_err()); // 2025 not leap
        assert!(moment_to_filetime_utc(&moment("2026-04-31T00:00:00", Some(0))).is_err()); // Apr has 30
        assert!(moment_to_filetime_utc(&moment("2026-01-00T00:00:00", Some(0))).is_err()); // day 0
        // The valid leap day still converts.
        assert!(moment_to_filetime_utc(&moment("2024-02-29T00:00:00", Some(0))).is_ok());
    }

    #[test]
    fn a_leap_second_is_refused_not_rolled_into_the_next_day() {
        // R2-S11. `:60` used to pass the range check and then sum to a time-of-day of 86400 - midnight
        // of the NEXT day - so the moment moved a full day with nothing said. In the calculator the same
        // input produced ONE result carrying two dates: the civil fields said Monday the 15th while the
        // instant and RFC 1123 said Tuesday the 16th.
        let err = moment_to_filetime_utc(&moment("2026-06-15T23:59:60", Some(0)))
            .expect_err("a leap second is not a moment this model can represent");
        assert!(err.contains("leap second"), "the message must name the reason, got: {err}");

        // Not only at the end of a day: the second field is refused wherever it appears.
        assert!(moment_to_filetime_utc(&moment("2026-06-15T12:00:60", Some(0))).is_err());
        // The calculator shares this parser, so it refuses the same input.
        assert!(calc::parse_civil_datetime("2026-06-15T23:59:60").is_err());

        // And the second before it still converts, to exactly 23:59:59 of the SAME day.
        let ok = moment_to_filetime_utc(&moment("2026-06-15T23:59:59", Some(0))).unwrap();
        let midnight = moment_to_filetime_utc(&moment("2026-06-16T00:00:00", Some(0))).unwrap();
        assert_eq!(midnight - ok, 10_000_000, "one second apart, not one day");
    }

    /// Each time field is refused BY NAME, and in the order it is written.
    ///
    /// The leap-second check used to run first, so `25:00:60` was answered "second 60 is a leap
    /// second" while the hour was 25 - the reader fixes the second, runs again, and only then hears
    /// about the hour. Underneath it, one catch-all said "time out of range" without naming which of
    /// the three was out of it. Both are the same defect: an answer that is correct and cannot be
    /// acted on.
    #[test]
    fn each_time_field_is_refused_by_name_and_in_reading_order() {
        let refusal = |s: &str| {
            moment_to_filetime_utc(&moment(s, Some(0))).expect_err("this moment cannot be represented")
        };

        let hour = refusal("2026-01-01T25:00:60");
        assert!(hour.contains("hour 25"), "the FIRST fault from the left, named: {hour}");
        assert!(!hour.contains("leap second"), "not the second, which is also wrong here: {hour}");

        let minute = refusal("2026-01-01T12:75:00");
        assert!(minute.contains("minute 75"), "got: {minute}");

        let second = refusal("2026-01-01T12:00:99");
        assert!(second.contains("second 99"), "got: {second}");

        // With hour and minute sound, the leap second still gets its own reason rather than the range.
        let leap = refusal("2026-01-01T23:59:60");
        assert!(leap.contains("leap second"), "got: {leap}");
    }

    #[test]
    fn out_of_filetime_range_year_is_an_error_not_a_wrap() {
        // An extreme but syntactically valid year overflows the FILETIME tick math. It must be a
        // reported Err (so callers show "(out of range)"), never a panic (debug) or a wrapped,
        // plausible-but-false number (release) - the promise "never a bad number".
        assert!(moment_to_filetime_utc(&moment("40000-01-01T00:00:00", Some(0))).is_err());
        // An absurd year would overflow days_from_civil's own internal math - the year guard rejects
        // it before that, so there is no panic even here.
        assert!(moment_to_filetime_utc(&moment("9223372036854775807-01-01T00:00:00", Some(0))).is_err());
    }

    /// A moment before 1601 has no FILETIME, and saying otherwise was not harmless.
    ///
    /// This test used to assert the opposite - that `moment_to_filetime_utc` stays `Ok` for such a
    /// date - so that the calculator could still show its epoch. The intent was right and the
    /// mechanism was wrong: the conversion returned a NEGATIVE i64, and a FILETIME counts from
    /// 1601 unsigned. Measured consequence on the substitution path, which is what makes this a
    /// defect rather than a display quirk: `chrono run --at 1000-01-01T00:00:00` reported WORKS
    /// with a fake wall of 1000-01-01, while inside the target the raw FILETIME channel read
    /// 18257086497710184170 and `GetSystemTime`/`GetLocalTime` showed the REAL clock, because
    /// `FileTimeToSystemTime` rejects that value. One process, two epochs, audit calling it a pass.
    ///
    /// The epoch the old test was protecting is still there - it just no longer comes from the
    /// FILETIME (see `formats`), which is what let both answers be right at once.
    #[test]
    fn a_moment_before_the_filetime_epoch_is_refused_not_returned_negative() {
        assert!(moment_to_filetime_utc(&moment("1000-06-15T00:00:00", Some(0))).is_err());
        // The epoch itself is exactly on the boundary and IS representable.
        assert!(moment_to_filetime_utc(&moment("1601-01-01T00:00:00", Some(0))).is_ok());
        // One second earlier is not.
        assert!(moment_to_filetime_utc(&moment("1600-12-31T23:59:59", Some(0))).is_err());
        // And the zone moves that boundary, which is the case a tester actually hits: midnight on
        // the epoch date in a positive-offset session is BEFORE the epoch in UTC.
        assert!(moment_to_filetime_utc(&moment("1601-01-01T00:00:00", Some(-120))).is_err());
        assert!(moment_to_filetime_utc(&moment("1601-01-01T02:00:00", Some(-120))).is_ok());
    }

    #[test]
    fn no_evidence_is_undetermined_not_works() {
        let cov = Coverage::default();
        assert_eq!(verdict_from_coverage(&cov), Verdict::Undetermined);
    }

    #[test]
    fn all_covered_is_works() {
        let cov = Coverage { covered: vec![ch("GetSystemTimeAsFileTime")], ..Default::default() };
        assert_eq!(verdict_from_coverage(&cov), Verdict::Works);
    }

    #[test]
    fn full_wall_set_is_works() {
        // The whole point of Stage 3: `works` means the full wall-clock set, not one
        // channel. The core stays name-agnostic - it only sees covered vs uncovered.
        let cov = Coverage {
            covered: vec![
                ch("GetSystemTimeAsFileTime"),
                ch("GetSystemTimePreciseAsFileTime"),
                ch("GetSystemTime"),
                ch("GetLocalTime"),
                ch("NtQuerySystemTime"),
            ],
            ..Default::default()
        };
        assert_eq!(verdict_from_coverage(&cov), Verdict::Works);
    }

    #[test]
    fn some_uncovered_is_partial() {
        let cov = Coverage {
            covered: vec![ch("GetSystemTimeAsFileTime")],
            uncovered: vec!["KUSER_SHARED_DATA".to_string()],
            ..Default::default()
        };
        assert_eq!(verdict_from_coverage(&cov), Verdict::Partial);
    }

    #[test]
    fn only_uncovered_is_fails() {
        let cov = Coverage { uncovered: vec!["KUSER_SHARED_DATA".to_string()], ..Default::default() };
        assert_eq!(verdict_from_coverage(&cov), Verdict::Fails);
    }

    #[test]
    fn combine_rolls_up_the_family_verdict() {
        use Verdict::*;
        // Undetermined (a process that touched nothing) is the identity - a launcher whose
        // child does the timekeeping is judged by the child: the family works.
        assert_eq!(Undetermined.combine(Works), Works);
        assert_eq!(Works.combine(Undetermined), Works);
        assert_eq!(Undetermined.combine(Undetermined), Undetermined);
        // Any uncovered anywhere drags a covered family down to partial (rule 4, honest).
        assert_eq!(Works.combine(Fails), Partial);
        assert_eq!(Works.combine(Partial), Partial);
        assert_eq!(Works.combine(Works), Works);
        // Nothing covered anywhere but something queried -> the family fails.
        assert_eq!(Fails.combine(Fails), Fails);
        assert_eq!(Fails.combine(Undetermined), Fails);
        // Commutative and associative, so accumulation order never matters.
        assert_eq!(Fails.combine(Works), Works.combine(Fails));
        assert_eq!(
            Undetermined.combine(Works).combine(Fails),
            Undetermined.combine(Works.combine(Fails))
        );
    }

    #[test]
    fn observed_does_not_sway_verdict() {
        // ADR-7 class B: an object wait left real is counted in `observed`, never covered/uncovered.
        // With the full wall set covered, an observed WaitForSingleObject must not change `works`...
        let works = Coverage {
            covered: vec![ch("GetSystemTimeAsFileTime")],
            observed: vec![ch("WaitForSingleObject")],
            ..Default::default()
        };
        assert_eq!(verdict_from_coverage(&works), Verdict::Works);
        // ...and observed alone (no covered, no uncovered) is not evidence of substitution.
        let observed_only = Coverage { observed: vec![ch("WaitForSingleObject")], ..Default::default() };
        assert_eq!(verdict_from_coverage(&observed_only), Verdict::Undetermined);
    }

    #[test]
    fn exit_codes_match_contract() {
        assert_eq!(Verdict::Works.exit_code(), 0);
        assert_eq!(Verdict::Undetermined.exit_code(), 4);
        assert_eq!(Verdict::Partial.exit_code(), 10);
        assert_eq!(Verdict::Fails.exit_code(), 11);
    }

    /// The top of the session range is `FAKE_WALL_MAX` = 30828-09-13T11:48:05 UTC, a whole second, and
    /// a moment one second later is refused rather than started and clamped (R4-S7b).
    #[test]
    fn a_session_moment_stops_at_the_last_instant_the_clock_can_hold() {
        assert_eq!(session_instant(&moment("30828-09-13T11:48:05", Some(0))), Ok((FAKE_WALL_MAX, 0)));
        assert_eq!(
            session_instant(&moment("30828-09-13T11:48:06", Some(0))),
            Err(SessionMomentError::OutOfRange)
        );
        // The widest zone east of UTC names the same last instant as local time, and fits: the
        // headroom under the clamp is there for exactly this.
        assert_eq!(session_instant(&moment("30828-09-14T02:47:05", Some(-899))), Ok((FAKE_WALL_MAX, -899)));
        // The report's example: parsed fine, representable as a FILETIME, past the clock's range.
        assert_eq!(
            session_instant(&moment("30828-09-14T02:48:05", Some(0))),
            Err(SessionMomentError::OutOfRange)
        );
    }

    /// R4-S7d: the floor holds in UTC AND as local time in the session zone, because the hook serves
    /// the instant both ways and a negative local FILETIME drops `GetLocalTime` to the real clock.
    #[test]
    fn a_session_moment_starts_no_earlier_than_1601_in_utc_or_in_its_zone() {
        assert_eq!(session_instant(&moment("1601-01-01T00:00:00", Some(0))), Ok((0, 0)));
        // UTC-05:00: local midnight on the first day is 05:00 UTC, and both are inside.
        assert_eq!(
            session_instant(&moment("1601-01-01T00:00:00", Some(300))),
            Ok((5 * 3600 * 10_000_000, 300))
        );
        // The report's example: 01:00 UTC is inside, but the local time is before 1601.
        assert_eq!(
            session_instant(&moment("1600-12-31T20:00:00", Some(300))),
            Err(SessionMomentError::OutOfRange)
        );
        // UTC+02:00: 01:00 local is 23:00 UTC the day before 1601 - out on the UTC side.
        assert_eq!(
            session_instant(&moment("1601-01-01T01:00:00", Some(-120))),
            Err(SessionMomentError::OutOfRange)
        );
        assert_eq!(
            session_instant(&moment("1000-01-01T00:00:00", Some(0))),
            Err(SessionMomentError::OutOfRange)
        );
    }

    /// R4-S9 and the decision on a missing zone: a session never guesses one. The zone is checked
    /// before the text, because a moment cannot be read without it.
    #[test]
    fn a_session_moment_needs_a_zone_inside_the_band() {
        assert_eq!(session_instant(&moment("2030-06-15T12:00:00", None)), Err(SessionMomentError::BadZone));
        assert_eq!(session_instant(&moment("2030-06-15T12:00:00", Some(900))), Err(SessionMomentError::BadZone));
        assert_eq!(
            session_instant(&moment("2030-06-15T12:00:00", Some(i32::MIN))),
            Err(SessionMomentError::BadZone)
        );
        assert_eq!(session_instant(&moment("not a moment", None)), Err(SessionMomentError::BadZone));
        assert!(matches!(
            session_instant(&moment("2026-02-30T00:00:00", Some(0))),
            Err(SessionMomentError::NotAMoment(_))
        ));
        // An ordinary moment comes back as the same instant the plain conversion gives.
        let m = moment("2038-01-19T03:14:07", Some(-120));
        assert_eq!(session_instant(&m), Ok((moment_to_filetime_utc(&m).unwrap(), -120)));
    }

    fn shift(sign: Sign, amount: i64, unit: Unit) -> Step {
        Step::Shift { sign, amount, unit }
    }

    fn ft_of(local: &str, bias: i32) -> i64 {
        moment_to_filetime_utc(&moment(local, Some(bias))).unwrap()
    }

    /// R4-S7c: a jump lands inside the range a session may start at, or is refused - never parked
    /// on 1601 or past the top, whichever kind of step took it there.
    #[test]
    fn step_target_stays_inside_the_session_range() {
        let ft = ft_of("2026-01-01T00:00:00", 0);
        // The report's example, and the same by a calendar unit.
        assert_eq!(
            step_target(ft, 0, &shift(Sign::Minus, 300_000, Unit::Days)),
            Err(EvalError::Overflow { index: 0 })
        );
        assert_eq!(
            step_target(ft, 0, &shift(Sign::Minus, 500, Unit::Years)),
            Err(EvalError::Overflow { index: 0 })
        );
        // Past the top by one second, and exactly onto it.
        let top = FAKE_WALL_MAX;
        assert_eq!(
            step_target(top, 0, &shift(Sign::Plus, 1, Unit::Seconds)),
            Err(EvalError::Overflow { index: 0 })
        );
        assert_eq!(step_target(top - 10_000_000, 0, &shift(Sign::Plus, 1, Unit::Seconds)), Ok(top));
        // West of UTC the local time is the one that crosses first: 1601-01-01T01:00 UTC is fine in
        // UTC and 20:00 the day before in UTC-05:00.
        let first_day = ft_of("1601-01-01T05:00:00", 0);
        assert_eq!(
            step_target(first_day, 300, &shift(Sign::Minus, 4, Unit::Hours)),
            Err(EvalError::Overflow { index: 0 })
        );
        assert_eq!(
            step_target(first_day, 300, &shift(Sign::Minus, 5, Unit::Hours)),
            Err(EvalError::Overflow { index: 0 })
        );
        assert_eq!(step_target(first_day, 0, &shift(Sign::Minus, 4, Unit::Hours)), Ok(first_day - 4 * 3600 * 10_000_000));
    }
}
