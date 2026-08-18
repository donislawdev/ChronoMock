//! Chrono Mock logical core - pure time-substitution logic.
//!
//! Rule 16 (untouchable): the core knows nothing of CLI, GUI, I/O, or language.
//! It takes plain data in and returns plain values out - no side effects
//! (zasady/06 section 7). Everything here is unit-testable without a target
//! process or an operating system.

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

/// Everything needed to define a session. Pure data, no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    pub moment: Moment,
    pub mode: TimeMode,
    /// Whether the monotonic duration axis is scaled too (E4 / ADR-2).
    pub scale_duration: bool,
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
    let has_covered = !cov.covered.is_empty();
    let has_uncovered = !cov.uncovered.is_empty();
    match (has_covered, has_uncovered) {
        (false, false) => Verdict::Undetermined,
        (true, false) => Verdict::Works,
        (false, true) => Verdict::Fails,
        (true, true) => Verdict::Partial,
    }
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
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(format!("month/day out of range in '{local}'"));
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
    let local_secs = days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + s;
    let bias = moment.tz_bias_min.unwrap_or(0) as i64;
    let utc_secs = local_secs + bias * 60; // UTC = local + bias
    Ok((utc_secs + DAYS_1601_TO_1970 * 86_400) * 10_000_000)
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

/// Parse a relative delta like "+2h" / "-30m" into signed FILETIME ticks (100 ns). Units are
/// fixed-length s/m/h/d/w. Shared by a relative `--at` (now + delta, resolved in the driver) and a
/// relative `jump` (current fake + delta, resolved in the core) so the two never drift. Overflow is
/// reported, not wrapped.
pub fn parse_relative_delta(raw: &str) -> Result<i64, String> {
    let split = raw.len().saturating_sub(1);
    let (num, unit) = raw.split_at(split);
    let unit_secs: i64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return Err(format!("relative delta must end in s/m/h/d/w, got '{raw}'")),
    };
    let n: i64 = num
        .parse()
        .map_err(|_| format!("bad number in relative delta '{raw}'"))?;
    n.checked_mul(unit_secs)
        .and_then(|s| s.checked_mul(10_000_000))
        .ok_or_else(|| format!("relative delta too large: '{raw}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(name: &str) -> ChannelCoverage {
        ChannelCoverage { channel: name.to_string(), calls: 1 }
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
    fn relative_delta_parses_signed_units() {
        assert_eq!(parse_relative_delta("+2h").unwrap(), 2 * 3600 * 10_000_000);
        assert_eq!(parse_relative_delta("-30m").unwrap(), -30 * 60 * 10_000_000);
        assert_eq!(parse_relative_delta("+1d").unwrap(), 86_400 * 10_000_000);
        assert_eq!(parse_relative_delta("+1w").unwrap(), 604_800 * 10_000_000);
        assert!(parse_relative_delta("+1x").is_err()); // bad unit
        assert!(parse_relative_delta("-y").is_err()); // no number
        assert!(parse_relative_delta("+abcd").is_err()); // bad number
        assert!(parse_relative_delta("+99999999999999w").is_err()); // overflow, reported not wrapped
    }

    #[test]
    fn exit_codes_match_contract() {
        assert_eq!(Verdict::Works.exit_code(), 0);
        assert_eq!(Verdict::Undetermined.exit_code(), 4);
        assert_eq!(Verdict::Partial.exit_code(), 10);
        assert_eq!(Verdict::Fails.exit_code(), 11);
    }
}
