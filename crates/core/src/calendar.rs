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
fn nth_weekday_days(year: i64, month: u32, weekday_target: u32, order: i32) -> i64 {
    if order == -1 {
        // Walk back from the first day of the next month to the last matching weekday.
        let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
        let last = days(ny, nm, 1) - 1;
        last - ((weekday(last) + 7 - weekday_target) % 7) as i64
    } else {
        let first = days(year, month, 1);
        let shift = (weekday_target + 7 - weekday(first)) % 7;
        first + shift as i64 + (order as i64 - 1) * 7
    }
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
fn holiday_days(rule: &HolidayRule, year: i64) -> i64 {
    match rule {
        HolidayRule::Fixed { month, day } => days(year, *month, *day),
        HolidayRule::NthWeekday { month, weekday, order } => nth_weekday_days(year, *month, *weekday, *order),
        HolidayRule::EasterOffset { offset } => {
            let (month, day) = easter_sunday(year);
            days(year, month, day) + *offset as i64
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
        .find(|h| in_force(h, date.year) && holiday_days(&h.rule, date.year) == target)
}

/// Whether `date` is a business day: not a weekend day, and not the OBSERVED date of any holiday
/// in force. Observance is what makes a weekday a day off, so a Saturday July 4 shifted to Friday
/// makes that Friday a non-business day (US federal) while leaving it a business day (US banking).
pub fn is_business_day(date: &CivilDateTime, cal: &Calendar) -> bool {
    let target = days(date.year, date.month, date.day);
    if cal.weekend.contains(&weekday(target)) {
        return false;
    }
    // A holiday can be observed in an adjacent year (a Jan 1 that falls on Saturday observes on
    // Dec 31 of the previous year under some rules; a Dec 31 on Sunday observes on Jan 1 of the
    // next). Check this year and its neighbours so a shifted observance near a boundary counts.
    for year in [date.year - 1, date.year, date.year + 1] {
        for h in &cal.holidays {
            if in_force(h, year) && observed_days(holiday_days(&h.rule, year), cal.observed) == target {
                return false;
            }
        }
    }
    true
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
        assert_eq!(nth_weekday_days(2026, 11, 4, 4), days(2026, 11, 26));
        // MLK Day 2026 = 3rd Monday of January = 2026-01-19.
        assert_eq!(nth_weekday_days(2026, 1, 1, 3), days(2026, 1, 19));
        // Memorial Day 2026 = last Monday of May = 2026-05-25.
        assert_eq!(nth_weekday_days(2026, 5, 1, -1), days(2026, 5, 25));
        // Last-weekday when the month ends exactly on that weekday still works (May 2027 ends Mon).
        assert_eq!(nth_weekday_days(2027, 5, 1, -1), days(2027, 5, 31));
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
}
