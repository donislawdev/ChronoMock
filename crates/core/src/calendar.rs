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
    /// An offset in days from Gregorian Easter Sunday. Not built yet (the Easter slice).
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

/// A rule the engine does not evaluate yet (only `EasterOffset` in this slice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedRule {
    pub holiday_id: String,
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

/// The calendar date (day count) of a holiday in `year`, or an error if the rule is not built.
fn holiday_days(rule: &HolidayRule, year: i64, holiday_id: &str) -> Result<i64, UnsupportedRule> {
    match rule {
        HolidayRule::Fixed { month, day } => Ok(days(year, *month, *day)),
        HolidayRule::NthWeekday { month, weekday, order } => {
            Ok(nth_weekday_days(year, *month, *weekday, *order))
        }
        HolidayRule::EasterOffset { .. } => Err(UnsupportedRule { holiday_id: holiday_id.to_string() }),
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
/// Returns the first unsupported rule encountered so the caller can surface it honestly.
pub fn holiday_on<'a>(
    date: &CivilDateTime,
    cal: &'a Calendar,
) -> Result<Option<&'a Holiday>, UnsupportedRule> {
    let target = days(date.year, date.month, date.day);
    for h in &cal.holidays {
        if !in_force(h, date.year) {
            continue;
        }
        if holiday_days(&h.rule, date.year, &h.id)? == target {
            return Ok(Some(h));
        }
    }
    Ok(None)
}

/// Whether `date` is a business day: not a weekend day, and not the OBSERVED date of any holiday
/// in force. Observance is what makes a weekday a day off, so a Saturday July 4 shifted to Friday
/// makes that Friday a non-business day (US federal) while leaving it a business day (US banking).
pub fn is_business_day(date: &CivilDateTime, cal: &Calendar) -> Result<bool, UnsupportedRule> {
    let target = days(date.year, date.month, date.day);
    if cal.weekend.contains(&weekday(target)) {
        return Ok(false);
    }
    // A holiday can be observed in an adjacent year (a Jan 1 that falls on Saturday observes on
    // Dec 31 of the previous year under some rules; a Dec 31 on Sunday observes on Jan 1 of the
    // next). Check this year and its neighbours so a shifted observance near a boundary counts.
    for year in [date.year - 1, date.year, date.year + 1] {
        for h in &cal.holidays {
            if !in_force(h, year) {
                continue;
            }
            if observed_days(holiday_days(&h.rule, year, &h.id)?, cal.observed) == target {
                return Ok(false);
            }
        }
    }
    Ok(true)
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
        assert_eq!(holiday_on(&dt(2026, 7, 4), &cal).unwrap().unwrap().id, "independence");
        assert_eq!(holiday_on(&dt(2026, 11, 26), &cal).unwrap().unwrap().id, "thanksgiving");
        assert!(holiday_on(&dt(2026, 7, 5), &cal).unwrap().is_none());
        // Juneteenth is not in force before 2021.
        assert!(holiday_on(&dt(2019, 6, 19), &cal).unwrap().is_none());
        assert!(holiday_on(&dt(2021, 6, 19), &cal).unwrap().is_some());
    }

    #[test]
    fn business_day_basic_weekend_and_holiday() {
        let cal = us(Observed::SunToMon);
        assert!(is_business_day(&dt(2026, 7, 6), &cal).unwrap()); // Monday, ordinary
        assert!(!is_business_day(&dt(2026, 7, 5), &cal).unwrap()); // Sunday
        assert!(!is_business_day(&dt(2026, 1, 1), &cal).unwrap()); // New Year (Thursday)
    }

    #[test]
    fn observance_variants_differ_on_a_saturday_holiday() {
        // 2026-07-04 is a Saturday. Federal shifts the day off to Friday July 3; banking does not
        // (banks are open that Friday) - the docs/02 section 5.2 difference, the point of two US
        // calendars.
        let federal = us(Observed::SatToFriSunToMon);
        let banking = us(Observed::SunToMon);
        assert_eq!(weekday(days(2026, 7, 4)), 6, "guard: July 4 2026 is a Saturday");
        assert!(!is_business_day(&dt(2026, 7, 3), &federal).unwrap()); // observed holiday
        assert!(is_business_day(&dt(2026, 7, 3), &banking).unwrap()); // still a business day
    }

    #[test]
    fn sunday_holiday_shifts_to_monday_in_both_us_rules() {
        // 2027-07-04 is a Sunday; both US rules observe it on Monday July 5.
        assert_eq!(weekday(days(2027, 7, 4)), 0, "guard: July 4 2027 is a Sunday");
        for observed in [Observed::SatToFriSunToMon, Observed::SunToMon] {
            let cal = us(observed);
            assert!(!is_business_day(&dt(2027, 7, 5), &cal).unwrap()); // observed Monday
        }
    }

    #[test]
    fn easter_offset_is_not_built_yet() {
        let cal = Calendar {
            id: "pl-test".into(),
            country: "PL".into(),
            weekend: vec![0, 6],
            observed: Observed::None,
            holidays: vec![h("easter_monday", HolidayRule::EasterOffset { offset: 1 }, None)],
        };
        assert_eq!(
            holiday_on(&dt(2026, 4, 6), &cal),
            Err(UnsupportedRule { holiday_id: "easter_monday".into() })
        );
    }
}
