//! Chrono Mock logical core - pure time-substitution logic.
//!
//! Rule 16 (untouchable): the core knows nothing of CLI, GUI, I/O, or language.
//! It takes plain data in and returns plain values out - no side effects
//! (zasady/06 section 7). Everything here is unit-testable without a target
//! process or an operating system.

/// Date-arithmetic engine: the canonical step model shared by the calculator and
/// (later) the substitution moment grammar. A child module, so it may reuse the
/// crate-private civil-date primitives below without widening their visibility.
pub mod calc;

/// Business-day and holiday calendar engine (pure, data-driven). Like `calc`, a child
/// module reusing the crate-private civil primitives.
pub mod calendar;

/// How the session expresses the target moment.
///
/// Semantics (docs/01 section 4, untouchable rule 2): the entered moment is in
/// the SESSION zone, everything is UTC internally, and the zone is always explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    /// Local wall-clock text in the session zone, e.g. "2038-01-19T03:14:07".
    pub local: String,
    /// Session zone bias in minutes (UTC = local + bias), no DST. `None` = host zone.
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
    /// exactly the pair `(has_covered, has_uncovered)`; the family ORs those bits across
    /// processes, so the family `works` only when something is covered and nothing is left
    /// uncovered anywhere (untouchable rule 4 at the session level). This aggregates
    /// JUDGMENTS, never call counts - per-process reports stay separate (plasterek 11). OR
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

// --- Anchor math: a session moment -> UTC FILETIME (100 ns ticks since 1601) ---
//
// The entered moment is wall-clock time in the SESSION zone; internally everything
// is UTC (untouchable rule 2). `tz_bias_min` follows the Win32 convention
// UTC = local + bias. A `None` bias treats the moment as UTC (host-zone handling
// and full DST/leap validation are a later slice; docs/08 open item).

/// Days from 1970-01-01 for a civil date (Howard Hinnant's branchless algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Proleptic Gregorian leap-year test. A shared civil primitive next to `days_from_civil`;
/// the `calc` submodule reuses it, so the leap rule lives in exactly one place (rule 6).
fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Last valid day of a month (1..=12): 28/29/30/31. Returns 0 for a month outside 1..=12,
/// which every caller rejects upstream before using the result.
fn last_day_of_month(year: i64, month: i64) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Parse "YYYY-MM-DDTHH:MM:SS" (a space may replace the `T`). Strict on shape and
/// on field ranges; deeper calendar validation comes later.
fn parse_civil(local: &str) -> Result<(i64, i64, i64, i64, i64, i64), String> {
    let (date, time) = local
        .split_once(['T', ' '])
        .ok_or_else(|| format!("moment must be YYYY-MM-DDTHH:MM:SS, got '{local}'"))?;
    let d: Vec<&str> = date.split('-').collect();
    let t: Vec<&str> = time.split(':').collect();
    if d.len() != 3 || t.len() != 3 {
        return Err(format!("moment must be YYYY-MM-DDTHH:MM:SS, got '{local}'"));
    }
    let p = |s: &str, what: &str| -> Result<i64, String> {
        s.parse::<i64>().map_err(|_| format!("bad {what} in moment '{local}'"))
    };
    let (year, month, day) = (p(d[0], "year")?, p(d[1], "month")?, p(d[2], "day")?);
    let (hour, min, sec) = (p(t[0], "hour")?, p(t[1], "minute")?, p(t[2], "second")?);
    if !(1..=12).contains(&month) {
        return Err(format!("month out of range in '{local}'"));
    }
    // Reject a day that cannot exist in its month (e.g. 2026-02-31): BOTH the substitution moment
    // and the calculator refuse an impossible date here rather than let `days_from_civil` silently
    // roll it into the next month (untouchable rule 4/6 - never a silent wrong moment).
    let last = last_day_of_month(year, month) as i64;
    if !(1..=last).contains(&day) {
        return Err(format!("day {day} out of range for month {month} in '{local}'"));
    }
    if !(0..=23).contains(&hour) || !(0..=59).contains(&min) || !(0..=60).contains(&sec) {
        return Err(format!("time out of range in '{local}'"));
    }
    Ok((year, month, day, hour, min, sec))
}

/// Convert a session moment to a UTC FILETIME (100 ns ticks since 1601-01-01).
pub fn moment_to_filetime_utc(moment: &Moment) -> Result<i64, String> {
    let (y, mo, d, h, mi, s) = parse_civil(&moment.local)?;
    // Days between the FILETIME epoch (1601-01-01) and the Unix epoch (1970-01-01).
    const DAYS_1601_TO_1970: i64 = 134_774;
    // `days_from_civil` does unchecked internal i64 math; keep its input within a proleptic
    // Gregorian year band far inside that overflow limit. FILETIME itself saturates i64 near year
    // 30828 - well inside this band - so the checked chain below is what actually rejects an
    // out-of-FILETIME-range moment; this guard only keeps the civil math sound for an absurd year,
    // so such a moment is an honest Err, never a panic (debug) or a wrapped number (release).
    if !(-262_143..=262_143).contains(&y) {
        return Err(format!("year {y} in moment '{}' is out of range", moment.local));
    }
    let bias = moment.tz_bias_min.unwrap_or(0) as i64;
    // h/mi/s are range-checked in parse_civil, so this sum is exact (max 86_400) - no overflow.
    let tod = h * 3_600 + mi * 60 + s;
    let out_of_range = || format!("moment '{}' is out of the representable FILETIME range", moment.local);
    days_from_civil(y, mo, d)
        .checked_mul(86_400)
        .and_then(|day_secs| day_secs.checked_add(tod))
        .and_then(|local_secs| local_secs.checked_add(bias * 60)) // UTC = local + bias
        .and_then(|utc_secs| utc_secs.checked_add(DAYS_1601_TO_1970 * 86_400))
        .and_then(|secs_1601| secs_1601.checked_mul(10_000_000))
        .ok_or_else(out_of_range)
}

/// Civil date `(year, month, day)` from a day count since 1970-01-01 (Howard
/// Hinnant's inverse of `days_from_civil`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Format a UTC FILETIME (100 ns ticks since 1601) as a session-zone wall-clock
/// string "YYYY-MM-DDTHH:MM:SS". Inverse of `moment_to_filetime_utc` (no DST).
pub fn filetime_utc_to_wall(ft_utc: i64, tz_bias_min: i32) -> String {
    // Session-local = UTC - bias (UTC = local + bias).
    let local_ticks = ft_utc - (tz_bias_min as i64) * 60 * 10_000_000;
    const DAYS_1601_TO_1970: i64 = 134_774;
    let secs_1601 = local_ticks.div_euclid(10_000_000);
    let secs_1970 = secs_1601 - DAYS_1601_TO_1970 * 86_400;
    let days = secs_1970.div_euclid(86_400);
    let tod = secs_1970.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    let (hh, mi, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mi:02}:{ss:02}")
}

// The relative-delta grammar (fixed-length ticks) was retired into the shared step model:
// fixed units live in `calc::fixed_shift_ticks`, calendar units in `calc::step_target`, and
// parsing in the CLI's `parse_shift`. One grammar for `--at`, `jump`, and the calculator.

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(name: &str) -> ChannelCoverage {
        ChannelCoverage { channel: name.to_string(), calls: 1 }
    }

    #[test]
    fn multiplier_range_admits_freeze_and_every_documented_speed() {
        // Freeze is a VALUE on the wire, not an error - the GUI's freeze button sends 0.
        assert!(multiplier_in_range(0));
        // Every speed the product actually offers, and the bound itself.
        for m in [1, 10, 60, 1440, 86_400, MULTIPLIER_MAX] {
            assert!(multiplier_in_range(m), "x{m} must be allowed");
        }
        // Backward is a jump, never a rate (rule 3); past the bound the clock leaves the
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
    fn out_of_filetime_range_year_is_an_error_not_a_wrap() {
        // An extreme but syntactically valid year overflows the FILETIME tick math. It must be a
        // reported Err (so callers show "(out of range)"), never a panic (debug) or a wrapped,
        // plausible-but-false number (release) - the promise "never a bad number".
        assert!(moment_to_filetime_utc(&moment("40000-01-01T00:00:00", Some(0))).is_err());
        // An absurd year would overflow days_from_civil's own internal math; the year guard rejects
        // it before that, so there is no panic even here.
        assert!(moment_to_filetime_utc(&moment("9223372036854775807-01-01T00:00:00", Some(0))).is_err());
    }

    #[test]
    fn pre_1601_year_still_converts() {
        // The fix must NOT regress pre-FILETIME-epoch dates: a year before 1601 yields a (negative)
        // instant with no overflow, so it stays Ok and the calculator can still show its epoch.
        assert!(moment_to_filetime_utc(&moment("1000-06-15T00:00:00", Some(0))).is_ok());
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
        // channel. The core stays name-agnostic; it only sees covered vs uncovered.
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
}
