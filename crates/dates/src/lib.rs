//! Chrono Mock date library - civil date arithmetic, the date calculator and the holiday calendars.
//!
//! Pure: plain data in, plain values out. No clock, no zone of the host, no file and no I/O - the
//! caller hands over "now" and the zone as data. That is what lets one engine serve the Chrono Mock
//! program on Windows and run as a library on any system. It depends on nothing, and the dependency
//! guard in `crates/cli/tests/hygiene.rs` keeps it that way.

/// Date-arithmetic engine: the canonical step model shared by the calculator and the substitution
/// moment grammar. A child module, so it may reuse the crate-private civil-date primitives below
/// without widening their visibility.
pub mod calc;

/// Business-day and holiday calendar engine (pure, data-driven). Like `calc`, a child module reusing
/// the crate-private civil primitives.
pub mod calendar;

/// Which characters do not show as themselves - judged once for the readers of calendars and presets
/// and for quoting what a target says (R5-B1).
pub mod text;

/// What the random properties share - edges, a drawn calendar, a deadline, the weekly canary.
#[cfg(test)]
mod search;

/// The widest zone offset accepted, in minutes either side of UTC: 14:59.
///
/// Every real zone lies inside it (they run from UTC-12:00 to UTC+14:00). It is the band the command
/// line's `--zone` parser has always had (hours 0..=14, minutes 0..=59), stated once for every surface:
/// a zone step, the offset of a pasted date, and the session of the Chrono Mock program, whose clock
/// range is built on it.
pub const ZONE_BIAS_MAX_MIN: i32 = 14 * 60 + 59;

/// Whether a zone bias (minutes, UTC = local + bias) is inside [`ZONE_BIAS_MAX_MIN`] either side of
/// UTC. The single gate for every surface that takes a zone - the calculator's, and the session's
/// start command, absolute jump and both mechanisms.
pub fn zone_bias_in_range(bias_min: i32) -> bool {
    (-ZONE_BIAS_MAX_MIN..=ZONE_BIAS_MAX_MIN).contains(&bias_min)
}

// --- Civil date math: a wall-clock moment in a fixed-offset zone <-> a UTC instant ---
//
// A moment is wall-clock time in a zone, and every instant is UTC - in seconds since the Unix epoch,
// or as a FILETIME (100 ns ticks since 1601). `tz_bias_min` follows the Win32 convention
// UTC = local + bias, and a zone here is a fixed offset: no DST.

/// The proleptic Gregorian year band this build works in, stated once, for everyone.
///
/// [`days_from_civil`] below does unchecked i64 math internally, so a year far enough out overflows
/// it. Measured: at year 25 252 734 927 766 555 `chrono calc` died with `attempt to add with
/// overflow` and exit 101 instead of answering, on `--base` alone - metadata is computed for every
/// result, so no extra step was needed to reach it.
///
/// The guard used to live inside the session's moment conversion and nowhere else, which made the
/// substitution path safe and the calculator not: `run --at 300000-01-01T00:00:00` was refused
/// while `calc --base 300000-01-01T00:00:00` was accepted, so a date the calculator produced could
/// not be carried across its own "use in substitution" bridge. One band, checked wherever a year
/// enters or is computed, removes both the panic and that split.
///
/// FILETIME saturates near year 30828, far inside this band, so out-of-instant moments are still
/// dropped honestly long before this limit matters: the band keeps the CIVIL arithmetic sound, it
/// is not a statement about what can be represented as an instant.
pub const CIVIL_YEAR_MIN: i64 = -262_143;
/// Upper end of the civil year band - see [`CIVIL_YEAR_MIN`].
pub const CIVIL_YEAR_MAX: i64 = 262_143;

/// Whether a year is inside the band this build can compute on.
///
/// Checked at both kinds of entry, because they are genuinely different doors: parsing (`--base`,
/// `--at`, `--analyze`, presets) and each calculator step that COMPUTES a year. A step builds its
/// year arithmetically and never returns through the parser, so a guard at the parser alone leaves
/// the second door open - measured: `--base 2026-01-01T00:00:00 --shift +25252734927766000y`
/// panicked from a base that was perfectly legal.
pub fn civil_year_in_band(year: i64) -> bool {
    (CIVIL_YEAR_MIN..=CIVIL_YEAR_MAX).contains(&year)
}

/// Days from 1970-01-01 for a civil date (Howard Hinnant's branchless algorithm).
///
/// Precondition: `y` is inside [`CIVIL_YEAR_MIN`]..=[`CIVIL_YEAR_MAX`] - see [`civil_year_in_band`].
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Proleptic Gregorian leap-year test. A shared civil primitive next to `days_from_civil` -
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

/// The longest a month (1..=12) is in ANY year: 31/30/29, and 0 for a month outside 1..=12.
/// February answers 29, because a rule naming the leap day is legitimate - it simply does not
/// happen every year. Derived from `last_day_of_month` in a leap year rather than restating the
/// month lengths, so the calendar knowledge stays in one place (rule 6). Public because the
/// calendar loader needs it to tell a day that is impossible in SOME years (Feb 29) from one
/// impossible in EVERY year (April 31), and that loader lives in another crate.
pub fn max_day_in_month(month: u32) -> u32 {
    // 2024 is a leap year, so February answers 29 - the check is "possible in any year at all".
    last_day_of_month(2024, month as i64)
}

/// Whether `s` is one or more ASCII digits and nothing else - no sign, no space, no other script's
/// digits.
///
/// The gate every numeric FIELD of this tool's shapes goes through before it is parsed (a moment, a
/// time of day, a zone offset). Rust's integer parsers accept a leading `+`, so parsing alone let
/// `+05:+30` through as a zone and `2026-+1-05` as a date (R4-N34). One function, because the three
/// parsers had each been written to reject a sign and each had missed the same one.
pub fn is_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Parse "YYYY-MM-DDTHH:MM:SS" (a space may replace the `T`), strict on shape and on field ranges,
/// with every refusal quoting `shown` instead of `local`. The date analyser parses a text it built from
/// the user's (a date alone gets `T00:00:00`, a fraction and an offset come off), and its refusals
/// quoted that text, a moment nobody had typed (R5-D5, item 5). Everyone else reaches it through
/// [`calc::parse_civil_datetime`], which quotes the text it was given.
pub(crate) fn parse_civil_as(local: &str, shown: &str) -> Result<(i64, i64, i64, i64, i64, i64), String> {
    let (date, time) = local
        .split_once(['T', ' '])
        .ok_or_else(|| format!("moment must be YYYY-MM-DDTHH:MM:SS, got '{shown}'"))?;
    // A year before 1 CE carries a leading minus, and splitting on '-' would turn that into a
    // fourth field. The calculator can PRODUCE such a date - any shift back past year 1 does it -
    // so refusing to read one back meant the tool would not accept its own output: `--shift -10y`
    // from 0001-01-01 printed a date that `--base` then rejected (R3-6).
    let (negative_year, date) = match date.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, date),
    };
    let d: Vec<&str> = date.split('-').collect();
    let t: Vec<&str> = time.split(':').collect();
    if d.len() != 3 || t.len() != 3 {
        return Err(format!("moment must be YYYY-MM-DDTHH:MM:SS, got '{shown}'"));
    }
    // Digits only, checked before the parse: `i64::from_str` also takes a leading `+`, so
    // `2026-+1-05T+9:00:00` read as a real moment (R4-N34). A sign has one place in this shape - in
    // front of the year, handled above - and no field of a date the tool prints ever carries one.
    let p = |s: &str, what: &str| -> Result<i64, String> {
        if !is_ascii_digits(s) {
            return Err(format!("bad {what} in moment '{shown}' (digits only)"));
        }
        s.parse::<i64>().map_err(|_| format!("bad {what} in moment '{shown}'"))
    };
    let (year, month, day) = (p(d[0], "year")?, p(d[1], "month")?, p(d[2], "day")?);
    // Rejected here rather than after the field checks: a year outside the band makes the leap-year
    // test below meaningless, and `days_from_civil` further down would overflow on it.
    let year = if negative_year { -year } else { year };
    if !civil_year_in_band(year) {
        return Err(format!(
            "year {year} in moment '{shown}' is outside the range this build computes on \
             ({CIVIL_YEAR_MIN}..={CIVIL_YEAR_MAX})"
        ));
    }
    let (hour, min, sec) = (p(t[0], "hour")?, p(t[1], "minute")?, p(t[2], "second")?);
    if !(1..=12).contains(&month) {
        return Err(format!("month {month} out of range (1..=12) in '{shown}'"));
    }
    // Reject a day that cannot exist in its month (e.g. 2026-02-31): BOTH the substitution moment
    // and the calculator refuse an impossible date here rather than let `days_from_civil` silently
    // roll it into the next month (untouchable rule 4/6 - never a silent wrong moment).
    let last = last_day_of_month(year, month) as i64;
    if !(1..=last).contains(&day) {
        return Err(format!("day {day} out of range for month {month} in '{shown}'"));
    }
    // Field by field, left to right, and each refusal NAMES its field.
    //
    // The order is what was wrong, not the checks. The leap-second refusal came first, so
    // `2026-01-01T25:00:60` was answered "second 60 is a leap second" while the hour was 25 - the
    // reader fixes the second, runs again, and only then learns about the hour. And the catch-all
    // underneath said "time out of range" without saying WHICH of the three, on input where any of
    // them could be the one. A result that is right and cannot be explained is a defect here.
    if !(0..=23).contains(&hour) {
        return Err(format!("hour {hour} out of range (0..=23) in '{shown}'"));
    }
    if !(0..=59).contains(&min) {
        return Err(format!("minute {min} out of range (0..=59) in '{shown}'"));
    }
    // A leap second gets its own refusal, because it used to get silently accepted and then move the
    // DATE. `:60` passed this check as a nod to leap seconds, and the conversion to FILETIME then summed
    // a time-of-day of 86400, which is midnight of the NEXT day - so `2026-06-15T23:59:60` became the
    // 16th with nothing said, and `filetime_utc_to_wall` could not give the input back. In the
    // calculator it was worse than a shift: one result carried two dates, the civil fields saying
    // Monday the 15th while the instant, FILETIME and RFC 1123 said Tuesday the 16th (R2-S11).
    //
    // Neither this model nor FILETIME has leap seconds, so accepting one is a promise we cannot keep.
    // Refuse it the way every other impossible moment above is refused, and name the reason - a tester
    // who pasted a timestamp from a leap-second-aware source needs to know which second to use. It sits
    // BELOW the hour and minute checks so it only ever answers for input whose only fault it is.
    if sec == 60 {
        return Err(format!(
            "second 60 in '{shown}' is a leap second - this model has none (use :59)"
        ));
    }
    if !(0..=59).contains(&sec) {
        return Err(format!("second {sec} out of range (0..=59) in '{shown}'"));
    }
    Ok((year, month, day, hour, min, sec))
}

/// Civil fields (already validated) plus a zone bias to a UTC FILETIME, or `None` if the instant
/// falls outside what FILETIME can hold.
///
/// Split out of the conversion of a parsed moment so a caller that already HAS the fields does not have to
/// format them into a string and parse them straight back. The calculator's `zone` step did exactly
/// that, and it was not merely wasteful: it was the one place where a calculator step could fail on
/// FORMATTING, and every such failure was reported as an arithmetic overflow (R3-6).
///
/// Precondition: `y` is inside the band ([`civil_year_in_band`]), and the time fields are in range
/// - both hold for anything that came through `parse_civil_as` or a checked calculator step.
pub(crate) fn civil_fields_to_filetime_utc(
    y: i64,
    mo: i64,
    d: i64,
    h: i64,
    mi: i64,
    s: i64,
    bias_min: i64,
) -> Option<i64> {
    let ft = civil_fields_to_epoch_secs(y, mo, d, h, mi, s, bias_min)?
        .checked_add(DAYS_1601_TO_1970 * 86_400)?
        .checked_mul(10_000_000)?;
    // A FILETIME counts 100 ns intervals SINCE 1601-01-01 and Windows types it unsigned, so a
    // moment before that epoch has no FILETIME at all - and until this check existed the tool
    // produced one anyway, as a negative i64, then handed it around as if it were real.
    //
    // Measured, because the consequence is not cosmetic. `chrono run --at 1000-01-01T00:00:00`
    // reported the session as WORKS with a fake wall of 1000-01-01, while inside the target the
    // probe read: raw FILETIME 18257086497710184170 (the negative value read back as the unsigned
    // number it is - a year around 59000), and `GetSystemTime`/`GetLocalTime` showing the REAL
    // clock, because `FileTimeToSystemTime` refuses that value and the channel falls through.
    // One process, two epochs, and an audit calling it a pass - the same failure R2-X2 fixed at the
    // TOP of the range, which had no counterpart at the bottom (untouchable rules 2 and 4).
    (ft >= 0).then_some(ft)
}

/// The same civil fields as a UTC instant in SECONDS since the Unix epoch, which unlike a FILETIME
/// is perfectly happy to be negative.
///
/// Split from the FILETIME conversion because the two answer different questions and only one of
/// them has a floor. Year 1000 has a real instant (-30 610 231 200) and a real RFC 1123 rendering -
/// what it does not have is a FILETIME. Deriving the epoch fields from the FILETIME, as the
/// calculator used to, meant that fixing the FILETIME would have thrown away two correct answers
/// with the wrong one.
pub(crate) fn civil_fields_to_epoch_secs(
    y: i64,
    mo: i64,
    d: i64,
    h: i64,
    mi: i64,
    s: i64,
    bias_min: i64,
) -> Option<i64> {
    // h/mi/s are range-checked before they get here, so this sum is exact (max 86_400).
    let tod = h * 3_600 + mi * 60 + s;
    days_from_civil(y, mo, d)
        .checked_mul(86_400)
        .and_then(|day_secs| day_secs.checked_add(tod))
        .and_then(|local_secs| local_secs.checked_add(bias_min * 60)) // UTC = local + bias
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
/// string "YYYY-MM-DDTHH:MM:SS". Inverse of [`calc::CivilDateTime::to_filetime_utc`] (no DST).
pub fn filetime_utc_to_wall(ft_utc: i64, tz_bias_min: i32) -> String {
    // The calculator's conversion, not a second copy of it (R5-A11): the two were written apart, and
    // copies of this arithmetic had already drifted once. It works in whole SECONDS rather than ticks - the bias is a
    // whole number of minutes, so dividing first gives exactly the same second for every input, and it
    // cannot overflow, which subtracting the bias in ticks did: an instant at the end of the range in a
    // zone east of UTC panicked a Chromium session core on its `state` event, leaving the browser
    // behind with its debugging port open (R4-S8).
    calc::filetime_to_civil(ft_utc, tz_bias_min).to_iso()
}

/// Days from the FILETIME epoch (1601-01-01) to the Unix epoch (1970-01-01). One constant for every
/// conversion between the two counts, which wrote it out four times (R5-A11).
pub const DAYS_1601_TO_1970: i64 = 134_774;

/// The day of the week of a day counted from the Unix epoch: 0 = Sunday .. 6 = Saturday. 1970-01-01
/// was a Thursday. One formula for the calculator and the calendar, which had three copies (R5-A11).
pub(crate) fn weekday(days_since_epoch: i64) -> u32 {
    (days_since_epoch + 4).rem_euclid(7) as u32
}

/// A session bias (minutes, UTC = local + bias) as the `+HH:MM` label every surface prints - the
/// offset is the bias with its sign turned. The command line and the calculator each had a copy.
pub fn bias_label(tz_bias_min: i32) -> String {
    // Widened before the sign turns: i32::MIN has no positive twin, and the label of any bias the
    // type can hold must not take the report down with it (found by the random properties).
    let offset = -i64::from(tz_bias_min);
    let sign = if offset < 0 { '-' } else { '+' };
    format!("{sign}{:02}:{:02}", offset.abs() / 60, offset.abs() % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sign inside a field is refused (R4-N34). The integer parse accepted a leading `+`, so
    /// `2026-+1-05` read as 5 January. The one sign this shape has - a minus before the year - still
    /// reads, because the calculator writes years before 1 CE that way.
    #[test]
    fn a_sign_inside_a_field_of_a_moment_is_refused() {
        for bad in [
            "+2026-01-05T00:00:00",
            "2026-+1-05T00:00:00",
            "2026-01-+5T00:00:00",
            "2026-01-05T+9:00:00",
            "2026-01-05T09:+0:00",
            "2026-01-05T09:00:+0",
            "2026-01-05T 9:00:00",
        ] {
            assert!(calc::parse_civil_datetime(bad).is_err(), "{bad}");
        }
        assert!(calc::parse_civil_datetime("-0009-05-05T00:00:00").is_ok());
        assert!(calc::parse_civil_datetime("2026-01-05T09:00:00").is_ok());
        assert!(!is_ascii_digits(""));
        assert!(!is_ascii_digits("+1"));
        assert!(is_ascii_digits("0042"));
    }

    /// The property the old test was really after: a date before 1601 still has an instant, and the
    /// calculator still reports it. Refusing the FILETIME must not take the epoch down with it.
    #[test]
    fn a_date_before_the_filetime_epoch_still_has_an_epoch_and_an_rfc_rendering() {
        let civil = calc::parse_civil_datetime("1000-01-01T00:00:00").expect("a valid civil date");
        let f = calc::formats(&civil, -120);
        assert_eq!(f.epoch_seconds, Some(-30_610_231_200));
        assert_eq!(f.epoch_millis, Some(-30_610_231_200_000));
        assert_eq!(f.rfc1123.as_deref(), Some("Tue, 31 Dec 0999 22:00:00 GMT"));
        assert_eq!(f.iso_date, "1000-01-01");
        assert!(f.filetime.is_none(), "a pre-1601 moment has no FILETIME");
    }

    /// R4-S9: the zone band is the one `--zone` has always parsed, 14:59 either side, and nothing
    /// past it - `i32` reached the hook unchecked over the protocol before.
    #[test]
    fn the_zone_band_is_fourteen_fifty_nine_either_side() {
        for ok in [0, 1, -1, 899, -899, 300, -120] {
            assert!(zone_bias_in_range(ok), "bias {ok}");
        }
        for bad in [900, -900, 1440, i32::MIN, i32::MAX] {
            assert!(!zone_bias_in_range(bad), "bias {bad}");
        }
    }
}
