//! Business-day and holiday calendar engine - pure, data-driven (docs/02, docs/04 section 5).
//!
//! The rules (fixed date, n-th weekday of a month, Easter offset) and the weekend-observance
//! modifier drive the computation; the code never hard-codes a country's holidays. Calendar
//! DATA lives in `calendars/*.json` (loaded by the consumer, which owns the I/O and serde);
//! this module is the pure engine over already-parsed rules, unit-testable without files.
//!
//! Stage-4 slice 6 builds `Fixed` and `NthWeekday` (enough for both US calendars, which use
//! no Easter) plus the four observance modifiers. `EasterOffset` is in the model so the schema
//! is complete, and resolves to an honest "not built yet" until the Easter slice.

use crate::calc::CivilDateTime;

/// How a holiday's calendar date is computed for a given year.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolidayRule {
    /// A fixed month and day, e.g. July 4.
    Fixed { month: u32, day: u32 },
    /// The n-th weekday of a month: `weekday` 0 = Sunday .. 6 = Saturday, `order` 1..=5, or
    /// -1 for the last such weekday of the month (docs/02 section 4).
    NthWeekday { month: u32, weekday: u32, order: i32 },
    /// An offset in days from Gregorian Easter Sunday (Easter Monday = +1, Pentecost = +49,
    /// Corpus Christi = +60 - the Thursday after Trinity Sunday).
    EasterOffset { offset: i32 },
}

/// What happens when a holiday falls on a weekend (docs/02 section 4). The names describe the
/// BEHAVIOUR, never a country (a second country with the same behaviour reuses the value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    /// Nothing shifts; the holiday stays on its date (Poland).
    None,
    /// Saturday -> the preceding Friday, Sunday -> the following Monday (US federal).
    SatToFriSunToMon,
    /// Only Sunday -> the following Monday; Saturday does not shift (US banking).
    SunToMon,
    /// Saturday and Sunday both -> the following Monday (UK-style, unconfirmed).
    WeekendToMon,
}

/// One holiday: an identity, bilingual name, a rule, a validity range in years, and its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holiday {
    pub id: String,
    pub name_en: String,
    pub name_local: String,
    pub rule: HolidayRule,
    /// First year the holiday is in force (inclusive), or None = always.
    pub valid_from: Option<i64>,
    /// Last year in force (inclusive), or None = still in force.
    pub valid_to: Option<i64>,
    pub source: String,
}

/// A whole calendar: weekend days, the observance modifier, and the holiday list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    pub id: String,
    pub country: String,
    /// Weekend weekdays (0 = Sunday .. 6 = Saturday).
    pub weekend: Vec<u32>,
    pub observed: Observed,
    pub holidays: Vec<Holiday>,
}

/// Day of week for a civil date: 0 = Sunday .. 6 = Saturday.
fn weekday(days_since_epoch: i64) -> u32 {
    (days_since_epoch + 4).rem_euclid(7) as u32
}

/// Day count since 1970-01-01 for a civil (year, month, day). Reuses the crate-private civil
/// primitive (a sibling module may see it without widening its visibility).
fn days(year: i64, month: u32, day: u32) -> i64 {
    crate::days_from_civil(year, month as i64, day as i64)
}

/// The n-th (`order`) `weekday` of `month` in `year`, as a day count. `order` -1 = the last.
///
/// `None` when the month has no such occurrence - "the 5th Monday of February" is a date that simply
/// does not exist most years, and a holiday defined that way does not happen then. It used to return
/// a day count regardless, landing silently in the NEXT month (R2-N4): the loader allows 1..=5 and
/// its comment claimed the engine resolved a missing 5th, which it did not. No shipped calendar uses
/// 5, but calendars are the one part of this tool outsiders are invited to write, and a rule that
/// quietly marks a day in March as a February holiday is the silent-wrong-date class this project
/// calls inadmissible.
fn nth_weekday_days(year: i64, month: u32, weekday_target: u32, order: i32) -> Option<i64> {
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let last_of_month = days(next_year, next_month, 1) - 1;
    if order == -1 {
        // Walk back from the first day of the next month to the last matching weekday.
        return Some(last_of_month - ((weekday(last_of_month) + 7 - weekday_target) % 7) as i64);
    }
    let first = days(year, month, 1);
    let shift = (weekday_target + 7 - weekday(first)) % 7;
    let d = first + shift as i64 + (order as i64 - 1) * 7;
    (d <= last_of_month).then_some(d)
}

/// Gregorian Easter Sunday (month, day) for a year - the Meeus/Butcher "Anonymous Gregorian"
/// algorithm. Easter always falls between March 22 and April 25. The single-letter bindings are
/// the algorithm's canonical notation, kept verbatim so a reader can check against the published
/// algorithm.
#[allow(clippy::many_single_char_names)]
fn easter_sunday(year: i64) -> (u32, u32) {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    (month as u32, day as u32)
}

/// The calendar date (day count since 1970-01-01) of a holiday in `year`. Every rule type is
/// built; a rule this build did not know would have been rejected by the loader, and adding a
/// `HolidayRule` variant without handling it here is a compile error (the match is exhaustive).
fn holiday_days(rule: &HolidayRule, year: i64) -> Option<i64> {
    match rule {
        HolidayRule::Fixed { month, day } => Some(days(year, *month, *day)),
        // `None` when the month has no such occurrence: the holiday does not fall in this year at all
        // (R2-N4), rather than falling on an invented date in the next month.
        HolidayRule::NthWeekday { month, weekday, order } => {
            nth_weekday_days(year, *month, *weekday, *order)
        }
        HolidayRule::EasterOffset { offset } => {
            let (month, day) = easter_sunday(year);
            Some(days(year, month, day) + *offset as i64)
        }
    }
}

/// Whether a holiday is in force in `year` (inclusive range, open on either side).
fn in_force(h: &Holiday, year: i64) -> bool {
    h.valid_from.is_none_or(|f| year >= f) && h.valid_to.is_none_or(|t| year <= t)
}

/// The weekday a holiday whose calendar date is `d` (day count) is OBSERVED on, per the modifier.
/// A weekday holiday observes on its own day; a weekend one shifts (or not) by the rule.
fn observed_days(d: i64, observed: Observed) -> i64 {
    match (weekday(d), observed) {
        // Saturday.
        (6, Observed::SatToFriSunToMon) => d - 1,
        (6, Observed::WeekendToMon) => d + 2,
        (6, Observed::SunToMon | Observed::None) => d,
        // Sunday.
        (0, Observed::SatToFriSunToMon | Observed::SunToMon | Observed::WeekendToMon) => d + 1,
        (0, Observed::None) => d,
        // Weekday: observed on its own date.
        _ => d,
    }
}

/// The holiday whose CALENDAR date (before observance) is `date`, if any and in force. Answers
/// "is this date Independence Day?" - true for July 4 regardless of the weekday it lands on.
pub fn holiday_on<'a>(date: &CivilDateTime, cal: &'a Calendar) -> Option<&'a Holiday> {
    let target = days(date.year, date.month, date.day);
    cal.holidays
        .iter()
        .find(|h| in_force(h, date.year) && holiday_days(&h.rule, date.year) == Some(target))
}

/// Whether the day-count `target` is a business day: not a weekend day, and not the OBSERVED date
/// of any holiday in force. Observance is what makes a weekday a day off, so a Saturday July 4
/// shifted to Friday makes that Friday a non-business day (US federal) while leaving it a business
/// day (US banking).
fn is_business_day_days(target: i64, cal: &Calendar) -> bool {
    if cal.weekend.contains(&weekday(target)) {
        return false;
    }
    // A holiday can be observed in an adjacent year (a Jan 1 that falls on Saturday observes on
    // Dec 31 of the previous year under some rules; a Dec 31 on Sunday observes on Jan 1 of the
    // next). Check that year and its neighbours so a shifted observance near a boundary counts.
    let (year, _, _) = crate::civil_from_days(target);
    for y in [year - 1, year, year + 1] {
        for h in &cal.holidays {
            let observed = holiday_days(&h.rule, y).map(|d| observed_days(d, cal.observed));
            if in_force(h, y) && observed == Some(target) {
                return false;
            }
        }
    }
    true
}

/// Whether `date` is a business day (see `is_business_day_days`).
pub fn is_business_day(date: &CivilDateTime, cal: &Calendar) -> bool {
    is_business_day_days(days(date.year, date.month, date.day), cal)
}

/// The largest business-day shift the day-by-day walk will take before reporting overflow -
/// about 4000 years, far beyond any real use, but a hard bound so a pathological `+Nbd` cannot
/// loop unbounded. Signalled to the caller as `None`.
pub const MAX_BUSINESS_DAYS: i64 = 1_000_000;

/// The nearest business day to `from` in one direction, INCLUDING `from` itself if it is already a
/// business day (roll semantics: "adjust to a business day"). `forward` rolls toward later dates,
/// otherwise earlier. Time of day is kept. `None` only for a degenerate calendar with no business
/// day within a month (e.g. every weekday marked weekend).
pub fn nearest_business_day(from: &CivilDateTime, forward: bool, cal: &Calendar) -> Option<CivilDateTime> {
    let step = if forward { 1 } else { -1 };
    let mut d = days(from.year, from.month, from.day);
    for _ in 0..31 {
        if is_business_day_days(d, cal) {
            let (year, month, day) = crate::civil_from_days(d);
            return Some(CivilDateTime {
                year,
                month: month as u32,
                day: day as u32,
                hour: from.hour,
                minute: from.minute,
                second: from.second,
            });
        }
        d += step;
    }
    None
}

/// Why a business-day walk produced no answer. The two cases used to be one `None`, and the caller
/// turned both into "overflows the representable range" - a message about number sizes handed to
/// someone whose calendar file marks every weekday as a weekend (R2-S10). Calendars are the one part
/// of this tool outsiders are invited to write, so pointing their author at the wrong thing is the
/// expensive kind of wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusinessDayLimit {
    /// `|n|` is beyond `MAX_BUSINESS_DAYS`: the request is out of range, the calendar is fine.
    TooManyDays,
    /// The walk ran out of budget: this calendar has (almost) no business days at all.
    DegenerateCalendar,
}

/// `start` advanced by `n` business days (negative = backward), keeping the time of day. The start
/// day is not counted: the walk steps day by day and counts only business days, so Friday + 1 = the
/// next Monday.
pub fn add_business_days(
    start: &CivilDateTime,
    n: i64,
    cal: &Calendar,
) -> Result<CivilDateTime, BusinessDayLimit> {
    if n.unsigned_abs() > MAX_BUSINESS_DAYS as u64 {
        return Err(BusinessDayLimit::TooManyDays);
    }
    let mut d = days(start.year, start.month, start.day);
    let step = if n >= 0 { 1 } else { -1 };
    let mut remaining = n.abs();
    // Bound the WALK, not just the requested count. `remaining` falls only on a business day, so a
    // calendar that has none - every weekday marked weekend, which a data file can say, and calendars
    // are the one part of this tool outsiders are invited to write - spins here forever at 100% CPU
    // with nothing to interrupt it. `nearest_business_day` has carried such a bound since it was
    // written; this walk did not. Seven calendar days per business day plus a year of slack clears
    // every real calendar (the worst shipped case is a long weekend wrapped around a holiday), and a
    // file that needs more than that is degenerate: report it as the same "no result" the caller
    // already handles, rather than hanging.
    let mut budget = n.abs().saturating_mul(7).saturating_add(400);
    while remaining > 0 {
        if budget == 0 {
            return Err(BusinessDayLimit::DegenerateCalendar);
        }
        budget -= 1;
        d += step;
        if is_business_day_days(d, cal) {
            remaining -= 1;
        }
    }
    let (year, month, day) = crate::civil_from_days(d);
    Ok(CivilDateTime {
        year,
        month: month as u32,
        day: day as u32,
        hour: start.hour,
        minute: start.minute,
        second: start.second,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i64, m: u32, d: u32) -> CivilDateTime {
        CivilDateTime { year: y, month: m, day: d, hour: 0, minute: 0, second: 0 }
    }

    fn h(id: &str, rule: HolidayRule, valid_from: Option<i64>) -> Holiday {
        Holiday {
            id: id.into(),
            name_en: id.into(),
            name_local: id.into(),
            rule,
            valid_from,
            valid_to: None,
            source: "test".into(),
        }
    }

    /// A minimal US-shaped calendar for engine tests (the real data lives in calendars/*.json).
    fn us(observed: Observed) -> Calendar {
        Calendar {
            id: "us-test".into(),
            country: "US".into(),
            weekend: vec![0, 6],
            observed,
            holidays: vec![
                h("new_year", HolidayRule::Fixed { month: 1, day: 1 }, None),
                h("independence", HolidayRule::Fixed { month: 7, day: 4 }, None),
                h("juneteenth", HolidayRule::Fixed { month: 6, day: 19 }, Some(2021)),
                h("mlk", HolidayRule::NthWeekday { month: 1, weekday: 1, order: 3 }, None),
                h("thanksgiving", HolidayRule::NthWeekday { month: 11, weekday: 4, order: 4 }, None),
                h("memorial", HolidayRule::NthWeekday { month: 5, weekday: 1, order: -1 }, None),
            ],
        }
    }

    #[test]
    fn nth_weekday_computes_known_dates() {
        // Thanksgiving 2026 = 4th Thursday of November = 2026-11-26.
        assert_eq!(nth_weekday_days(2026, 11, 4, 4), Some(days(2026, 11, 26)));
        // MLK Day 2026 = 3rd Monday of January = 2026-01-19.
        assert_eq!(nth_weekday_days(2026, 1, 1, 3), Some(days(2026, 1, 19)));
        // Memorial Day 2026 = last Monday of May = 2026-05-25.
        assert_eq!(nth_weekday_days(2026, 5, 1, -1), Some(days(2026, 5, 25)));
        // Last-weekday when the month ends exactly on that weekday still works (May 2027 ends Mon).
        assert_eq!(nth_weekday_days(2027, 5, 1, -1), Some(days(2027, 5, 31)));
    }

    #[test]
    fn holiday_on_names_the_actual_date_and_respects_valid_from() {
        let cal = us(Observed::SunToMon);
        assert_eq!(holiday_on(&dt(2026, 7, 4), &cal).unwrap().id, "independence");
        assert_eq!(holiday_on(&dt(2026, 11, 26), &cal).unwrap().id, "thanksgiving");
        assert!(holiday_on(&dt(2026, 7, 5), &cal).is_none());
        // Juneteenth is not in force before 2021.
        assert!(holiday_on(&dt(2019, 6, 19), &cal).is_none());
        assert!(holiday_on(&dt(2021, 6, 19), &cal).is_some());
    }

    #[test]
    fn business_day_basic_weekend_and_holiday() {
        let cal = us(Observed::SunToMon);
        assert!(is_business_day(&dt(2026, 7, 6), &cal)); // Monday, ordinary
        assert!(!is_business_day(&dt(2026, 7, 5), &cal)); // Sunday
        assert!(!is_business_day(&dt(2026, 1, 1), &cal)); // New Year (Thursday)
    }

    #[test]
    fn observance_variants_differ_on_a_saturday_holiday() {
        // 2026-07-04 is a Saturday. Federal shifts the day off to Friday July 3; banking does not
        // (banks are open that Friday) - the docs/02 section 5.2 difference, the point of two US
        // calendars.
        let federal = us(Observed::SatToFriSunToMon);
        let banking = us(Observed::SunToMon);
        assert_eq!(weekday(days(2026, 7, 4)), 6, "guard: July 4 2026 is a Saturday");
        assert!(!is_business_day(&dt(2026, 7, 3), &federal)); // observed holiday
        assert!(is_business_day(&dt(2026, 7, 3), &banking)); // still a business day
    }

    #[test]
    fn sunday_holiday_shifts_to_monday_in_both_us_rules() {
        // 2027-07-04 is a Sunday; both US rules observe it on Monday July 5.
        assert_eq!(weekday(days(2027, 7, 4)), 0, "guard: July 4 2027 is a Sunday");
        for observed in [Observed::SatToFriSunToMon, Observed::SunToMon] {
            let cal = us(observed);
            assert!(!is_business_day(&dt(2027, 7, 5), &cal)); // observed Monday
        }
    }

    #[test]
    fn easter_sunday_matches_known_dates() {
        // Verified Gregorian Easter Sundays (web, 2026-08): the algorithm must reproduce them.
        assert_eq!(easter_sunday(2024), (3, 31));
        assert_eq!(easter_sunday(2025), (4, 20));
        assert_eq!(easter_sunday(2026), (4, 5));
        assert_eq!(easter_sunday(2027), (3, 28));
        // Easter always falls between March 22 and April 25.
        for year in 1900..2100 {
            let (m, d) = easter_sunday(year);
            let ok = (m == 3 && (22..=31).contains(&d)) || (m == 4 && (1..=25).contains(&d));
            assert!(ok, "Easter {year} out of range: {m}-{d}");
        }
    }

    /// A minimal Poland-shaped calendar for Easter-offset engine tests.
    fn pl() -> Calendar {
        Calendar {
            id: "pl-test".into(),
            country: "PL".into(),
            weekend: vec![0, 6],
            observed: Observed::None,
            holidays: vec![
                h("easter_sunday", HolidayRule::EasterOffset { offset: 0 }, None),
                h("easter_monday", HolidayRule::EasterOffset { offset: 1 }, None),
                h("pentecost", HolidayRule::EasterOffset { offset: 49 }, None),
                h("corpus_christi", HolidayRule::EasterOffset { offset: 60 }, None),
                h("christmas_eve", HolidayRule::Fixed { month: 12, day: 24 }, Some(2025)),
            ],
        }
    }

    #[test]
    fn easter_offset_holidays_are_computed() {
        let cal = pl();
        // Easter 2026 = April 5. Easter Monday = April 6.
        assert_eq!(holiday_on(&dt(2026, 4, 5), &cal).unwrap().id, "easter_sunday");
        assert_eq!(holiday_on(&dt(2026, 4, 6), &cal).unwrap().id, "easter_monday");
        // Pentecost = +49 = 2026-05-24; Corpus Christi = +60 = 2026-06-04 (a Thursday).
        assert_eq!(holiday_on(&dt(2026, 5, 24), &cal).unwrap().id, "pentecost");
        assert_eq!(holiday_on(&dt(2026, 6, 4), &cal).unwrap().id, "corpus_christi");
        assert_eq!(weekday(days(2026, 6, 4)), 4, "Corpus Christi is a Thursday");
    }

    #[test]
    fn poland_observes_none_and_christmas_eve_from_2025() {
        let cal = pl();
        // Christmas Eve is a business day in 2024, a holiday from 2025 (valid_from). Dec 24 2024
        // is a Tuesday, Dec 24 2025 a Wednesday - both weekdays, so only the holiday rule differs.
        assert!(is_business_day(&dt(2024, 12, 24), &cal));
        assert!(!is_business_day(&dt(2025, 12, 24), &cal));
        assert!(holiday_on(&dt(2024, 12, 24), &cal).is_none());
        assert_eq!(holiday_on(&dt(2025, 12, 24), &cal).unwrap().id, "christmas_eve");
    }

    /// R2-N4. `order: 5` is legal in the schema and legitimate for a month that HAS a fifth such
    /// weekday. For one that does not, the walk used to run past the month end and hand back a day in
    /// the NEXT month - so a rule saying "the 5th Monday of February" quietly marked a Monday in March
    /// as a February holiday, in a file written by someone outside this project.
    #[test]
    fn a_fifth_weekday_the_month_does_not_have_is_absent_not_borrowed_from_the_next() {
        // February 2026 starts on a Sunday, so its Mondays are the 2nd, 9th, 16th and 23rd - four.
        assert_eq!(nth_weekday_days(2026, 2, 1, 4), Some(days(2026, 2, 23)));
        assert_eq!(nth_weekday_days(2026, 2, 1, 5), None, "there is no fifth Monday to name");

        // A month that DOES have five keeps working - this is a real capability, not a banned value.
        // March 2026 starts on a Sunday too, and 31 days give Mondays on the 2nd, 9th, 16th, 23rd, 30th.
        assert_eq!(nth_weekday_days(2026, 3, 1, 5), Some(days(2026, 3, 30)));
        // And -1 still means the last one, whichever ordinal that is.
        assert_eq!(nth_weekday_days(2026, 2, 1, -1), Some(days(2026, 2, 23)));
        assert_eq!(nth_weekday_days(2026, 3, 1, -1), Some(days(2026, 3, 30)));

        // End to end: a holiday defined that way does not occur in a year without a fifth Monday, and
        // in particular does not turn a March day into a February holiday.
        let cal = Calendar {
            id: "t".into(),
            country: "XX".into(),
            weekend: vec![0, 6],
            observed: Observed::None,
            holidays: vec![h("fifth_mon_feb", HolidayRule::NthWeekday { month: 2, weekday: 1, order: 5 }, None)],
        };
        assert!(holiday_on(&dt(2026, 3, 2), &cal).is_none(), "March 2 is not a February holiday");
        assert!(is_business_day(&dt(2026, 3, 2), &cal), "and it stays a working day");
        // In a month that HAS a fifth, it does occur - this is a capability, not a banned value.
        let cal5 = Calendar {
            holidays: vec![h("fifth_mon_mar", HolidayRule::NthWeekday { month: 3, weekday: 1, order: 5 }, None)],
            ..cal
        };
        assert!(holiday_on(&dt(2026, 3, 30), &cal5).is_some(), "a real fifth Monday still fires");
    }

    #[test]
    fn add_business_days_skips_weekends() {
        let cal = us(Observed::SunToMon);
        // 2026-07-10 is a Friday; +1 business day is the next Monday, 2026-07-13.
        assert_eq!(add_business_days(&dt(2026, 7, 10), 1, &cal).unwrap(), dt(2026, 7, 13));
        // 2026-07-06 is a Monday; +5 business days is the next Monday.
        assert_eq!(add_business_days(&dt(2026, 7, 6), 5, &cal).unwrap(), dt(2026, 7, 13));
    }

    /// S-1 regression. A calendar with every weekday marked weekend has no business day to land
    /// on, so the walk could never finish - it spun at 100% CPU with no way to interrupt it, and
    /// through the GUI it took a `chrono calc` process down with it. Now the walk is bounded and
    /// says "no result" like any other unsatisfiable shift. The loader rejects such a file too,
    /// so this is the second line, not the first.
    #[test]
    fn add_business_days_terminates_on_a_calendar_with_no_business_day() {
        let cal = Calendar {
            id: "degenerate".into(),
            country: "XX".into(),
            weekend: vec![0, 1, 2, 3, 4, 5, 6],
            observed: Observed::None,
            holidays: vec![],
        };
        // R2-S10: and it says WHICH limit it hit. A calendar with no business day is a bad FILE,
        // not a number too large - the caller renders each with its own message.
        for n in [1, -1, 5000] {
            assert_eq!(
                add_business_days(&dt(2026, 7, 6), n, &cal),
                Err(BusinessDayLimit::DegenerateCalendar),
                "a calendar with no business day names itself, whatever the count"
            );
        }
        // Zero steps never enters the walk, so it still resolves to the start day.
        assert_eq!(add_business_days(&dt(2026, 7, 6), 0, &cal).unwrap(), dt(2026, 7, 6));
    }

    /// The bound must not clip a legal calendar: with six weekend days a business day costs seven
    /// calendar days, which is exactly the budget's per-day allowance.
    #[test]
    fn add_business_days_still_walks_a_six_day_weekend() {
        let cal = Calendar {
            id: "one-day-week".into(),
            country: "XX".into(),
            weekend: vec![0, 1, 2, 3, 4, 5],
            observed: Observed::None,
            holidays: vec![],
        };
        // Only Saturday works. 2026-07-06 is a Monday, so +2 business days is the second Saturday.
        assert_eq!(add_business_days(&dt(2026, 7, 6), 2, &cal).unwrap(), dt(2026, 7, 18));
    }

    #[test]
    fn add_business_days_skips_holidays() {
        let cal = us(Observed::SunToMon);
        // New Year (Jan 1 2026, a Thursday holiday): 2025-12-31 (Wed) + 1 business day skips it to Jan 2.
        assert_eq!(add_business_days(&dt(2025, 12, 31), 1, &cal).unwrap(), dt(2026, 1, 2));
    }

    #[test]
    fn add_business_days_backward_and_from_a_day_off() {
        let cal = us(Observed::SunToMon);
        assert_eq!(add_business_days(&dt(2026, 7, 13), -1, &cal).unwrap(), dt(2026, 7, 10)); // Mon -1 = Fri
        // Starting on a Saturday, +1 business day is the following Monday (the start is not counted).
        assert_eq!(add_business_days(&dt(2026, 7, 4), 1, &cal).unwrap(), dt(2026, 7, 6));
    }

    #[test]
    fn add_business_days_zero_is_identity_keeping_time() {
        let cal = us(Observed::SunToMon);
        let start = CivilDateTime { year: 2026, month: 7, day: 6, hour: 9, minute: 30, second: 0 };
        assert_eq!(add_business_days(&start, 0, &cal).unwrap(), start);
    }

    #[test]
    fn add_business_days_caps_absurd_shifts() {
        let cal = us(Observed::SunToMon);
        assert_eq!(
            add_business_days(&dt(2026, 7, 6), MAX_BUSINESS_DAYS + 1, &cal),
            Err(BusinessDayLimit::TooManyDays),
            "an absurd count is the REQUEST being out of range - this calendar is perfectly good"
        );
    }

    #[test]
    fn nearest_business_day_rolls_and_includes_a_business_day() {
        let cal = us(Observed::SunToMon);
        // A Saturday rolls forward to Monday; a business day is itself (roll includes it).
        assert_eq!(nearest_business_day(&dt(2026, 7, 4), true, &cal).unwrap(), dt(2026, 7, 6));
        assert_eq!(nearest_business_day(&dt(2026, 7, 6), true, &cal).unwrap(), dt(2026, 7, 6));
        // A Sunday rolls backward to the Friday (a business day under banking).
        assert_eq!(nearest_business_day(&dt(2026, 7, 5), false, &cal).unwrap(), dt(2026, 7, 3));
        // A holiday (New Year, a Thursday) rolls forward past it to the Friday.
        assert_eq!(nearest_business_day(&dt(2026, 1, 1), true, &cal).unwrap(), dt(2026, 1, 2));
    }

    #[test]
    fn nearest_business_day_keeps_time_of_day() {
        let cal = us(Observed::SunToMon);
        let sat = CivilDateTime { year: 2026, month: 7, day: 4, hour: 15, minute: 45, second: 30 };
        let mon = CivilDateTime { year: 2026, month: 7, day: 6, hour: 15, minute: 45, second: 30 };
        assert_eq!(nearest_business_day(&sat, true, &cal).unwrap(), mon);
    }
}
