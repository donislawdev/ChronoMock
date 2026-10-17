//! Business-day and holiday calendar engine - pure, data-driven (docs/02, docs/04 section 5).
//!
//! The rules (fixed date, n-th weekday of a month, Easter offset) and the weekend-observance
//! modifier drive the computation - the code never hard-codes a country's holidays. Calendar
//! DATA lives in `calendars/*.json` (loaded by the consumer, which owns the I/O and serde) -
//! this module is the pure engine over already-parsed rules, unit-testable without files.
//!
//! All three rule types are built - `Fixed`, `NthWeekday` and `EasterOffset` (Meeus) - together
//! with the four observance modifiers. The doc here used to promise an honest "not built yet" for
//! Easter long after `holiday_days` had implemented it, which is the kind of drift that makes a
//! reader trust the comment over the code (R2-N2).

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
///
/// Each variant names where an observance is aimed. Where it lands is one step further when that
/// day is already off - another holiday, or a weekend day - and so on in the same direction until a
/// free day (R4-N40, placed by `OffDays::rebuild`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    /// Nothing shifts - the holiday stays on its date (Poland).
    None,
    /// Saturday -> the preceding Friday, Sunday -> the following Monday (US federal).
    SatToFriSunToMon,
    /// Only Sunday -> the following Monday - Saturday does not shift (US banking).
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

/// A whole calendar: weekend days, the observance modifier, the holiday list, and the years it answers
/// for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    pub id: String,
    pub country: String,
    /// Weekend weekdays (0 = Sunday .. 6 = Saturday).
    pub weekend: Vec<u32>,
    pub observed: Observed,
    /// The first year this calendar answers for (docs/04 section 5): from that year on every rule in it -
    /// the holidays, the weekend and the observance - has a source. `None` for a calendar built in code,
    /// which answers for every year. A calendar read from a file always has one.
    ///
    /// The holidays alone could not say this. Their own `valid_from` limits one holiday, and a calendar
    /// whose rules are right only from some year on - Poland before the five-day week, where Saturday was
    /// a working day - used to answer for every year the calculator computes, the year 1000 included,
    /// with the same confidence as for this one (R4-S20).
    pub valid_from: Option<i64>,
    /// The day the holiday list was last checked against the law, or `None` for a calendar built in
    /// code. A date after it is the law as it stood that day, carried forward - see [`outdated`].
    pub law_as_of: Option<CivilDateTime>,
    pub holidays: Vec<Holiday>,
}

impl Calendar {
    /// Whether this calendar judges dates of `year` at all - false before its first year.
    pub fn judges_year(&self, year: i64) -> bool {
        self.valid_from.is_none_or(|first| year >= first)
    }

    /// The first day it judges, as a day count since 1970-01-01, or `None` when it judges every day.
    fn first_day(&self) -> Option<i64> {
        self.valid_from.map(|year| days(year, 1, 1))
    }
}

/// Day of week for a day counted from the Unix epoch: 0 = Sunday .. 6 = Saturday - the crate's one
/// formula ([`crate::weekday`]), under the name this file reads it by.
use crate::weekday;

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
/// built - a rule this build did not know would have been rejected by the loader, and adding a
/// `HolidayRule` variant without handling it here is a compile error (the match is exhaustive).
///
/// `None` means "this holiday does not fall in this year at all" - the answer for every rule whose
/// date does not exist in the year asked about. Two rules can say that, and both used to answer with
/// a date in the NEXT month instead.
fn holiday_days(rule: &HolidayRule, year: i64) -> Option<i64> {
    match rule {
        // `None` when this year's month is too short: February 29 is a legitimate rule (the loader
        // accepts it, and a holiday observed only in leap years is a real thing), but in a common
        // year it does not happen - it does not become March 1. `days_from_civil` is Hinnant's
        // algorithm and does not validate, so without this check the day rolls into the next month
        // and the report NAMES a holiday on a date that has none - the silent-wrong-date class this
        // project calls inadmissible, and worse than the `nth_weekday` case because it also attaches
        // a name (R2-N7).
        HolidayRule::Fixed { month, day } => {
            (*day <= crate::last_day_of_month(year, *month as i64))
                .then(|| days(year, *month, *day))
        }
        // `None` when the month has no such occurrence: the holiday does not fall in this year at all
        // (R2-N4), rather than falling on an invented date in the next month.
        HolidayRule::NthWeekday { month, weekday, order } => {
            nth_weekday_days(year, *month, *weekday, *order)
        }
        HolidayRule::EasterOffset { offset } => {
            // Meeus' algorithm is stated for the Gregorian calendar, and its `%` on a negative year
            // truncates toward zero, producing a month and day that are not a date at all - then cast
            // to u32 (R2-N3). Unreachable through the loaders, which validate `valid_from`, but this
            // engine is a library: an out-of-era year has no Easter rather than a nonsensical one.
            if year < 1 {
                return None;
            }
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
/// A weekday holiday observes on its own day - a weekend one shifts (or not) by the rule.
///
/// 🔴 Matches Saturday and Sunday by NAME, not `Calendar::weekend`, because that is what these rules
/// are: the variant names say `sun_to_mon`. For a calendar whose weekend is not Saturday-Sunday the
/// answer would be wrong in both directions at once (a Friday holiday never shifts, a working Sunday
/// shifts anyway), so the CLI loader refuses that combination instead of letting it through - the
/// guard is in `crate::calendar::calendar_from_text`, not merely in this comment (rule 12).
/// Generalising wants new variant names and a real market to define them, not a guess here.
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
/// 🔴 The FIRST holiday whose rule lands on this date, in file order - two holidays sharing a day
/// (Christmas and a national day, say) report only the first, and the loader's duplicate check is on
/// `id`, not on date (R2-N5). A documented limit rather than a silent one: nothing in the shipped
/// calendars collides, and returning a list would change the report's shape for a case that has not
/// arisen. If one ever does, the second holiday still makes the day non-business - only its NAME is
/// missing.
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
/// The days off around one year, worked out once and reused while the walk stays in that year.
///
/// Deciding whether a single day is a business day means evaluating every holiday rule for three
/// years - Easter by Meeus among them - and a day-by-day walk did that again for every day it
/// stepped over. The answer only changes when the YEAR changes, which is once per ~365 steps.
///
/// Kept as a type rather than a loop-local so there is still exactly ONE definition of "is this a
/// business day": `is_business_day_days` builds one of these and asks it a single question, and the
/// walk builds one and asks it many. Two copies of that predicate is how a calendar starts giving
/// two answers.
struct OffDays<'a> {
    cal: &'a Calendar,
    /// The year the cache was built for - `None` before the first question.
    year: Option<i64>,
    /// Observed dates of the holidays in force for that year and its neighbours, as day-counts,
    /// sorted and without repeats, so a day is found by binary search.
    ///
    /// It used to be a flat scan, on the reasoning that a calendar holds about fourteen holidays. A
    /// calendar file may hold 2 000 (`MAX_HOLIDAYS` in the reader), which makes this 6 000 entries
    /// read on every day a walk asks about: `+200000bd` on such a file took about 2 s, against 16-21 ms
    /// on a shipped one, measured (R5-WY2). Sorted, the cost no longer grows with the file that much.
    days: Vec<i64>,
    /// Holidays whose observance moves, as (calendar date, date the rule aims at) - only a working
    /// list for `rebuild`, kept here so a walk across many years does not allocate it once per year.
    moved: Vec<(i64, i64)>,
}

impl<'a> OffDays<'a> {
    fn new(cal: &'a Calendar) -> Self {
        Self { cal, year: None, days: Vec::new(), moved: Vec::new() }
    }

    fn is_business_day(&mut self, target: i64) -> bool {
        if self.cal.weekend.contains(&weekday(target)) {
            return false;
        }
        let (year, _, _) = crate::civil_from_days(target);
        if self.year != Some(year) {
            self.rebuild(year);
        }
        self.days.binary_search(&target).is_err()
    }

    fn rebuild(&mut self, year: i64) {
        // A holiday can be observed in an adjacent year (a Jan 1 that falls on Saturday observes on
        // Dec 31 of the previous year under some rules - a Dec 31 on Sunday observes on Jan 1 of the
        // next). Take that year and its neighbours so a shifted observance near a boundary counts.
        //
        // Two passes, because an observance can land on a day that is already off (R4-N40). Christmas
        // on a Saturday and Boxing Day on a Sunday both moved to the same Monday under
        // `weekend_to_mon`, and the two days off became one - a business-day answer a whole day early,
        // with no message. A holiday that keeps its own date holds it first. Then the moved ones are
        // placed in date order, and one that lands on a weekend day or a day already taken goes on to
        // the next free day in the same direction - the substitute day of the United Kingdom, applied
        // to every observance rule. The shipped calendars never collide, so their answers do not move.
        self.days.clear();
        self.moved.clear();
        for y in [year - 1, year, year + 1] {
            for h in &self.cal.holidays {
                if in_force(h, y)
                    && let Some(d) = holiday_days(&h.rule, y)
                {
                    let observed = observed_days(d, self.cal.observed);
                    if observed == d {
                        self.days.push(d);
                    } else {
                        self.moved.push((d, observed));
                    }
                }
            }
        }
        self.year = Some(year);
        // Sorted, so a taken day is a binary search - here and in `is_business_day`, which used to scan
        // the whole list whenever nothing moved (R5-WY2). A long walk rebuilds this once per year it
        // crosses, and a hash set here measured a fifth slower over a million business days.
        self.days.sort_unstable();
        self.days.dedup();
        if self.moved.is_empty() {
            return; // nothing shifts this year - the common case, and every year of a calendar with `none`
        }
        // Stable, so two holidays on one date keep the order the file lists them in.
        self.moved.sort_by_key(|&(d, _)| d);
        // Bounded because this engine is a library, and a caller may hand it a week with no working day,
        // where no free day exists. With at least one working day a week, seven steps per day already
        // taken, plus a week, always reach one - so the bound never cuts a real placement short.
        let limit = 7 * (self.days.len() + self.moved.len() + 7);
        for &(d, observed) in &self.moved {
            let step = (observed - d).signum();
            let mut day = observed;
            for _ in 0..limit {
                if !self.cal.weekend.contains(&weekday(day)) && self.days.binary_search(&day).is_err() {
                    break;
                }
                day += step;
            }
            if let Err(at) = self.days.binary_search(&day) {
                self.days.insert(at, day);
            }
        }
    }
}

fn is_business_day_days(target: i64, cal: &Calendar) -> bool {
    OffDays::new(cal).is_business_day(target)
}

/// Whether `date` is a business day (see `is_business_day_days`).
pub fn is_business_day(date: &CivilDateTime, cal: &Calendar) -> bool {
    is_business_day_days(days(date.year, date.month, date.day), cal)
}

/// The largest business-day shift the day-by-day walk will take before reporting overflow -
/// about 4000 years, far beyond any real use, but a hard bound so a pathological `+Nbd` cannot
/// loop unbounded. Signalled to the caller as `None`.
pub const MAX_BUSINESS_DAYS: i64 = 1_000_000;

/// How many days in a row a walk looks at without meeting a business day before it calls the calendar
/// degenerate, in both walks below.
///
/// Longer than a year, so a calendar the walk gives up on went a whole year - every weekend, every
/// holiday and every observance in it - without one working day. The two walks used to carry two
/// different bounds, and neither was this. `nearest_business_day` looked a month ahead, and
/// `add_business_days` spent one budget of seven days per business day plus 400 over the WHOLE walk,
/// so a legal calendar with fewer than one working day a week ran it dry partway and was reported as
/// having no business days at all: a six-day weekend with holidays on the 1st and the 15th gave
/// `+100bd` and refused `+1000bd` (R4-N41). Counted from the last business day, the bound only
/// catches a calendar that really has stopped having them.
const DEGENERATE_GAP_DAYS: i64 = 400;

/// The nearest business day to `from` in one direction, INCLUDING `from` itself if it is already a
/// business day (roll semantics: "adjust to a business day"). `forward` rolls toward later dates,
/// otherwise earlier. Time of day is kept. Refused for a degenerate calendar, one with no business day
/// in `DEGENERATE_GAP_DAYS` days from `from` (e.g. every weekday marked weekend), and for a roll that
/// has to judge a day before the calendar's first year.
pub fn nearest_business_day(
    from: &CivilDateTime,
    forward: bool,
    cal: &Calendar,
) -> Result<CivilDateTime, BusinessDayLimit> {
    let step = if forward { 1 } else { -1 };
    let mut d = days(from.year, from.month, from.day);
    let first = cal.first_day();
    // One cache for the whole roll, as in `add_business_days`: it was rebuilt for every day looked at.
    let mut off = OffDays::new(cal);
    for _ in 0..DEGENERATE_GAP_DAYS {
        // Before the weekend test, because the weekend is one of the rules this calendar has no source
        // for before its first year.
        if first.is_some_and(|f| d < f) {
            return Err(BusinessDayLimit::BeforeCalendar);
        }
        if off.is_business_day(d) {
            let (year, month, day) = crate::civil_from_days(d);
            return Ok(CivilDateTime {
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
    Err(BusinessDayLimit::DegenerateCalendar)
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
    /// The walk went `DEGENERATE_GAP_DAYS` days in a row without a business day: this calendar has
    /// (almost) none at all.
    DegenerateCalendar,
    /// The walk reached a day before the calendar's first year (`Calendar::valid_from`), which it has no
    /// rules for. Counting on with today's rules there is the wrong answer the calendar was limited to
    /// stop (R4-S20).
    BeforeCalendar,
}

/// How many days `law_as_of` may lie behind today before a date after it is treated as possibly
/// missing a holiday. A year: a holiday is usually enacted months ahead (Poland's Christmas Eve, in
/// December 2024 for December 2025), but not always (12 November 2018 was made a day off five days
/// before), so no lag is safe - and a calendar checked this year should not warn on every date of the
/// next one. A package older than a year warns exactly where a new holiday could be missing.
pub const LAW_AS_OF_FRESH_DAYS: i64 = 365;

/// Whether `judged` - a day this calendar decided something about - may be missing a holiday: it lies
/// after the day the calendar was checked against the law, and that check is more than
/// [`LAW_AS_OF_FRESH_DAYS`] older than `today`. The owner's case: a package last updated in 2030 and
/// run in 2032, where a holiday added in 2031 is simply not in the file (R4/20).
///
/// False for a calendar built in code (no `law_as_of`), and when `today` is not after the check at all
/// (a machine whose clock runs behind) - the data is then not older than a year from where the caller
/// stands.
pub fn outdated(cal: &Calendar, today: &CivilDateTime, judged: &CivilDateTime) -> bool {
    let Some(law) = cal.law_as_of else {
        return false;
    };
    let law_day = days(law.year, law.month, law.day);
    days(judged.year, judged.month, judged.day) > law_day
        && days(today.year, today.month, today.day) - law_day > LAW_AS_OF_FRESH_DAYS
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
    // with nothing to interrupt it. The bound counts the days since the last business day, not the
    // days walked in total, so it catches a calendar that has stopped having business days and never
    // a sparse one still walking towards its next (R4-N41, see `DEGENERATE_GAP_DAYS`). It is reported
    // as the same "no result" the caller already handles, rather than hanging.
    let mut days_off_in_a_row = 0;
    // The start day is not judged (Friday + 1 = Monday), so only the days stepped on are held against
    // the calendar's first year: `+1bd` from the last day before it is a question the calendar can
    // answer, `-1bd` from its first working day is not. One comparison of two integers per step.
    let first = cal.first_day();
    // One cache for the whole walk: it is rebuilt when the walk crosses into another year, so the
    // holiday rules are evaluated about once per 365 steps instead of once per step.
    let mut off = OffDays::new(cal);
    while remaining > 0 {
        d += step;
        if first.is_some_and(|f| d < f) {
            return Err(BusinessDayLimit::BeforeCalendar);
        }
        if off.is_business_day(d) {
            remaining -= 1;
            days_off_in_a_row = 0;
        } else {
            days_off_in_a_row += 1;
            if days_off_in_a_row >= DEGENERATE_GAP_DAYS {
                return Err(BusinessDayLimit::DegenerateCalendar);
            }
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
            valid_from: None, law_as_of: None, holidays: vec![
                h("new_year", HolidayRule::Fixed { month: 1, day: 1 }, None),
                h("independence", HolidayRule::Fixed { month: 7, day: 4 }, None),
                h("juneteenth", HolidayRule::Fixed { month: 6, day: 19 }, Some(2021)),
                h("mlk", HolidayRule::NthWeekday { month: 1, weekday: 1, order: 3 }, None),
                h("thanksgiving", HolidayRule::NthWeekday { month: 11, weekday: 4, order: 4 }, None),
                h("memorial", HolidayRule::NthWeekday { month: 5, weekday: 1, order: -1 }, None),
            ],
        }
    }

    /// The year cache in `OffDays` must answer exactly as a from-scratch evaluation does, at every
    /// date - a cache that forgets to rebuild when the walk crosses into a new year would still pass
    /// every golden-value test, because those all sit inside one year. Reused instance versus a fresh
    /// instance per question, over five years and across four year boundaries, on all four observance
    /// modifiers (they move holidays across 31 December and 1 January, which is exactly where a stale
    /// window shows).
    #[test]
    fn the_reused_year_cache_answers_as_a_fresh_one_does() {
        for observed in [
            Observed::None,
            Observed::SatToFriSunToMon,
            Observed::SunToMon,
            Observed::WeekendToMon,
        ] {
            let cal = us(observed);
            let mut reused = OffDays::new(&cal);
            let start = days(2021, 1, 1);
            for d in start..start + 5 * 365 {
                let fresh = OffDays::new(&cal).is_business_day(d);
                assert_eq!(reused.is_business_day(d), fresh, "day {d}, observed {observed:?}");
            }
            // ...and backwards, which is the direction a negative business-day shift walks.
            let mut back = OffDays::new(&cal);
            for d in (start..start + 5 * 365).rev() {
                assert_eq!(back.is_business_day(d), OffDays::new(&cal).is_business_day(d), "back {d}");
            }
        }
    }

    /// `OffDays::rebuild` deliberately scans the year asked about AND its two neighbours, because an
    /// observance can cross a year boundary in both directions. Nothing pinned that: the cache test
    /// above compares a reused instance with a fresh one, and both would carry the same missing year.
    /// This is the only place where the answer for one year depends on another year's data.
    #[test]
    fn an_observance_that_crosses_a_year_boundary_is_still_found() {
        // Backwards: 1 January 2022 is a Saturday, so under sat->fri it is observed on Friday
        // 31 December 2021 - a day off that belongs to the PREVIOUS year's answer.
        let cal = Calendar {
            id: "t".into(),
            country: "XX".into(),
            weekend: vec![0, 6],
            observed: Observed::SatToFriSunToMon,
            valid_from: None, law_as_of: None, holidays: vec![h("new_year", HolidayRule::Fixed { month: 1, day: 1 }, None)],
        };
        assert!(
            !is_business_day(&dt(2021, 12, 31), &cal),
            "31 December 2021 is the observed New Year's Day of 2022"
        );
        // The day before it is ordinary, so the whole end of December did not simply go dark.
        assert!(is_business_day(&dt(2021, 12, 30), &cal));

        // Forwards: a 31 December holiday falling on Sunday observes on 1 January of the NEXT year.
        // 31 December 2023 is a Sunday, so 1 January 2024 (a Monday) is the day off.
        let nye = Calendar {
            observed: Observed::SunToMon,
            valid_from: None, law_as_of: None, holidays: vec![h("nye", HolidayRule::Fixed { month: 12, day: 31 }, None)],
            ..cal
        };
        assert!(
            !is_business_day(&dt(2024, 1, 1), &nye),
            "1 January 2024 is the observed 31 December 2023"
        );
        assert!(is_business_day(&dt(2024, 1, 2), &nye));

        // Asked in the other order, a reused cache must give the same answers - walking backwards
        // across the boundary is what a negative business-day shift does.
        let mut off = OffDays::new(&nye);
        assert!(!off.is_business_day(days(2024, 1, 1)));
        assert!(off.is_business_day(days(2023, 12, 29)));
        assert!(!off.is_business_day(days(2024, 1, 1)), "the answer must not change on re-ask");
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
    fn an_easter_rule_has_no_answer_before_the_gregorian_era() {
        // R2-N3: Meeus' algorithm is stated for the Gregorian calendar and its `%` truncates toward
        // zero for a negative year, so it produced a (month, day) that is not a date - then cast to
        // u32. Unreachable through the loaders, reachable through this engine as a library.
        let cal = Calendar {
            id: "t".into(),
            country: "PL".into(),
            weekend: vec![0, 6],
            observed: Observed::None,
            valid_from: None, law_as_of: None, holidays: vec![h("easter", HolidayRule::EasterOffset { offset: 0 }, None)],
        };
        for year in [-100, 0] {
            for month in 1..=12u32 {
                for day in [1u32, 15, 28] {
                    let d = CivilDateTime { year, month, day, hour: 0, minute: 0, second: 0 };
                    assert!(holiday_on(&d, &cal).is_none(), "{year}-{month}-{day}");
                }
            }
        }
        // The era it IS stated for still answers: Easter 2026 is 5 April.
        let easter = CivilDateTime { year: 2026, month: 4, day: 5, hour: 0, minute: 0, second: 0 };
        assert!(holiday_on(&easter, &cal).is_some());
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
        // 2026-07-04 is a Saturday. Federal shifts the day off to Friday July 3 - banking does not
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
        // 2027-07-04 is a Sunday - both US rules observe it on Monday July 5.
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

    /// The calendar loader bounds an Easter offset to -80..=250 (R4-N39) on the claim that exactly
    /// this range keeps a holiday inside the year of its own Easter, whatever the year. The claim is
    /// the whole reason for those two numbers, so it is checked here over every Gregorian Easter to
    /// 4100, together with the fact that one day more at either end already breaks it somewhere.
    #[test]
    fn easter_offsets_from_minus_80_to_250_stay_in_the_year_of_their_easter() {
        let year_of = |offset: i32, year: i64| {
            let day = holiday_days(&HolidayRule::EasterOffset { offset }, year).expect("an Easter");
            crate::civil_from_days(day).0
        };
        for year in 1583..=4100 {
            assert_eq!(year_of(-80, year), year, "-80 in {year}");
            assert_eq!(year_of(250, year), year, "+250 in {year}");
        }
        assert!((1583..=4100).any(|year| year_of(-81, year) != year), "-81 leaves some year");
        assert!((1583..=4100).any(|year| year_of(251, year) != year), "+251 leaves some year");
    }

    /// A minimal Poland-shaped calendar for Easter-offset engine tests.
    fn pl() -> Calendar {
        Calendar {
            id: "pl-test".into(),
            country: "PL".into(),
            weekend: vec![0, 6],
            observed: Observed::None,
            valid_from: None, law_as_of: None, holidays: vec![
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
        // Pentecost = +49 = 2026-05-24 - Corpus Christi = +60 = 2026-06-04 (a Thursday).
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
            valid_from: None, law_as_of: None, holidays: vec![h("fifth_mon_feb", HolidayRule::NthWeekday { month: 2, weekday: 1, order: 5 }, None)],
        };
        assert!(holiday_on(&dt(2026, 3, 2), &cal).is_none(), "March 2 is not a February holiday");
        assert!(is_business_day(&dt(2026, 3, 2), &cal), "and it stays a working day");
        // In a month that HAS a fifth, it does occur - this is a capability, not a banned value.
        let cal5 = Calendar {
            valid_from: None, law_as_of: None, holidays: vec![h("fifth_mon_mar", HolidayRule::NthWeekday { month: 3, weekday: 1, order: 5 }, None)],
            ..cal
        };
        assert!(holiday_on(&dt(2026, 3, 30), &cal5).is_some(), "a real fifth Monday still fires");
    }

    /// R2-N7, the `Fixed` half of the same class. February 29 is a rule a calendar may legitimately
    /// name - a holiday observed only in leap years is a real thing, and the loader accepts it. In a
    /// common year it must simply not happen. It used to roll into March 1 through `days_from_civil`,
    /// which does not validate, and the report then NAMED that day as the holiday: worse than the
    /// `nth_weekday` case, because a wrong day off is one error and a wrong day off carrying a
    /// holiday's name is two.
    #[test]
    fn a_fixed_day_the_month_does_not_have_is_absent_not_rolled_into_the_next() {
        let cal = Calendar {
            id: "t".into(),
            country: "XX".into(),
            weekend: vec![0, 6],
            observed: Observed::None,
            valid_from: None, law_as_of: None, holidays: vec![h("leap_day", HolidayRule::Fixed { month: 2, day: 29 }, None)],
        };

        // 2025 is a common year. March 1 is not the leap-day holiday under any name, and stays a
        // day the calendar can be asked about like any other (it is a Saturday, hence the weekend).
        assert!(holiday_on(&dt(2025, 3, 1), &cal).is_none(), "March 1 is not February 29");
        assert!(holiday_on(&dt(2025, 2, 28), &cal).is_none(), "and it did not move to February 28");
        assert_eq!(holiday_days(&HolidayRule::Fixed { month: 2, day: 29 }, 2025), None);

        // In a leap year it fires on its own date - this is a capability, not a banned value.
        assert!(holiday_on(&dt(2024, 2, 29), &cal).is_some(), "the leap day itself still fires");
        assert!(!is_business_day(&dt(2024, 2, 29), &cal), "and it is still a day off");

        // A common year's March 1 on a WEEKDAY is the sharpest form: nothing else can explain a day
        // off there, so a business-day answer proves the holiday is gone rather than merely unnamed.
        // 2027-03-01 is a Monday.
        assert!(is_business_day(&dt(2027, 3, 1), &cal), "a weekday March 1 is a working day");
    }

    #[test]
    fn add_business_days_skips_weekends() {
        let cal = us(Observed::SunToMon);
        // 2026-07-10 is a Friday - +1 business day is the next Monday, 2026-07-13.
        assert_eq!(add_business_days(&dt(2026, 7, 10), 1, &cal).unwrap(), dt(2026, 7, 13));
        // 2026-07-06 is a Monday - +5 business days is the next Monday.
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
            valid_from: None, law_as_of: None, holidays: vec![],
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
            valid_from: None, law_as_of: None, holidays: vec![],
        };
        // Only Saturday works. 2026-07-06 is a Monday, so +2 business days is the second Saturday.
        assert_eq!(add_business_days(&dt(2026, 7, 6), 2, &cal).unwrap(), dt(2026, 7, 18));
    }

    /// R4-N41. A legal calendar with fewer than one working day a week: only Wednesday works, and the
    /// 1st and the 15th of every month are holidays. The walk used to spend one budget of seven days
    /// per business day over the whole walk, so `+100bd` worked and `+1000bd` ran the budget dry and
    /// reported a calendar with no business days - of a calendar that has about forty-six a year.
    #[test]
    fn a_calendar_with_under_one_working_day_a_week_walks_as_far_as_asked() {
        let mut holidays = Vec::new();
        for month in 1..=12 {
            for day in [1, 15] {
                holidays.push(h(&format!("h{month}-{day}"), HolidayRule::Fixed { month, day }, None));
            }
        }
        let cal = Calendar {
            id: "wednesday".into(),
            country: "XX".into(),
            weekend: vec![0, 1, 2, 4, 5, 6],
            observed: Observed::None,
            valid_from: None, law_as_of: None, holidays,
        };
        let start = dt(2026, 1, 7);
        let forward = add_business_days(&start, 1000, &cal).expect("a thousand Wednesdays exist");
        assert_eq!(weekday(days(forward.year, forward.month, forward.day)), 3, "it lands on a Wednesday");
        assert!(![1, 15].contains(&forward.day), "and not on a holiday");
        assert!(is_business_day(&forward, &cal));
        // Walking back the same thousand returns to the start, so no Wednesday was skipped or counted twice.
        assert_eq!(add_business_days(&forward, -1000, &cal).expect("and back"), start);
    }

    /// The bound behind both walks counts the days since the last business day. A calendar that goes
    /// a whole year without one - every Saturday of 2027 a holiday where Saturday is the only working
    /// day - still has the next one, and `nearest` used to give up after a month and call it degenerate.
    /// Two such years in a row are past the bound, and that is the calendar the bound exists for.
    #[test]
    fn a_year_without_a_business_day_is_crossed_and_two_are_not() {
        let saturdays = |to: i64| {
            let mut holidays = Vec::new();
            for month in 1..=12 {
                for order in 1..=5 {
                    let rule = HolidayRule::NthWeekday { month, weekday: 6, order };
                    holidays.push(Holiday {
                        valid_to: Some(to),
                        ..h(&format!("sat{month}-{order}"), rule, Some(2027))
                    });
                }
            }
            Calendar {
                id: "saturday".into(),
                country: "XX".into(),
                weekend: vec![0, 1, 2, 3, 4, 5],
                observed: Observed::None,
                valid_from: None, law_as_of: None, holidays,
            }
        };
        let one_year = saturdays(2027);
        // 2027-01-01 is a Friday - the next working Saturday is 1 January 2028, 365 days on.
        assert_eq!(nearest_business_day(&dt(2027, 1, 1), true, &one_year), Ok(dt(2028, 1, 1)));
        // 26 December 2026 is the last working Saturday before the gap.
        assert_eq!(add_business_days(&dt(2026, 12, 26), 1, &one_year), Ok(dt(2028, 1, 1)));

        let two_years = saturdays(2028);
        assert_eq!(
            nearest_business_day(&dt(2027, 1, 1), true, &two_years),
            Err(BusinessDayLimit::DegenerateCalendar)
        );
        assert_eq!(
            add_business_days(&dt(2026, 12, 26), 1, &two_years),
            Err(BusinessDayLimit::DegenerateCalendar)
        );
    }

    /// R4-N40, the case the report names. Under `weekend_to_mon` Christmas on a Saturday and Boxing Day
    /// on a Sunday both aimed at the Monday, and the two days off became one, so the Tuesday counted
    /// as a business day. The second observance now moves on to the Tuesday. In 2022 Boxing Day is
    /// itself the Monday and keeps it, so Christmas, a Sunday, moves past it to the Tuesday.
    #[test]
    fn an_observance_that_lands_on_a_day_already_off_moves_on() {
        let cal = Calendar {
            id: "uk".into(),
            country: "XX".into(),
            weekend: vec![0, 6],
            observed: Observed::WeekendToMon,
            valid_from: None, law_as_of: None, holidays: vec![
                h("christmas", HolidayRule::Fixed { month: 12, day: 25 }, None),
                h("boxing_day", HolidayRule::Fixed { month: 12, day: 26 }, None),
            ],
        };
        assert_eq!(weekday(days(2021, 12, 25)), 6, "guard: Christmas 2021 is a Saturday");
        assert!(!is_business_day(&dt(2021, 12, 27), &cal));
        assert!(!is_business_day(&dt(2021, 12, 28), &cal), "the second day off moved on to Tuesday");
        assert!(is_business_day(&dt(2021, 12, 29), &cal));
        assert_eq!(add_business_days(&dt(2021, 12, 24), 1, &cal), Ok(dt(2021, 12, 29)));

        assert_eq!(weekday(days(2022, 12, 26)), 1, "guard: Boxing Day 2022 is a Monday");
        assert!(!is_business_day(&dt(2022, 12, 26), &cal));
        assert!(!is_business_day(&dt(2022, 12, 27), &cal), "Christmas moved past the Monday it found taken");
        assert!(is_business_day(&dt(2022, 12, 28), &cal));
    }

    /// A moved observance steps over a weekend like over any other day off. Christmas 2021 is a
    /// Saturday and every working day of the week after it is a holiday of its own, so both
    /// observances run past that Friday, over the next weekend, to the Monday and Tuesday after it.
    #[test]
    fn a_moved_observance_steps_over_a_weekend_on_its_way() {
        let mut holidays = vec![
            h("christmas", HolidayRule::Fixed { month: 12, day: 25 }, None),
            h("boxing_day", HolidayRule::Fixed { month: 12, day: 26 }, None),
        ];
        for day in 27..=31 {
            holidays.push(h(&format!("dec{day}"), HolidayRule::Fixed { month: 12, day }, None));
        }
        let cal = Calendar { id: "t".into(), country: "XX".into(), weekend: vec![0, 6], observed: Observed::WeekendToMon, valid_from: None, law_as_of: None, holidays };
        assert_eq!(weekday(days(2022, 1, 1)), 6, "guard: 1 January 2022 is a Saturday");
        assert!(!is_business_day(&dt(2022, 1, 3), &cal), "Christmas lands on the Monday after the weekend");
        assert!(!is_business_day(&dt(2022, 1, 4), &cal), "and Boxing Day on the Tuesday");
        assert!(is_business_day(&dt(2022, 1, 5), &cal));
    }

    /// R4-N40 for every rule, in both directions. A Saturday observed on Friday moves back to Thursday
    /// when Friday is a holiday of its own, and a Sunday observed on Monday moves on to Tuesday when
    /// Monday is one - the owner's decision was the same substitute rule for every observance, not
    /// only `weekend_to_mon`.
    #[test]
    fn a_taken_day_moves_an_observance_on_in_its_own_direction_under_every_rule() {
        // 3 and 4 July 2026 are a Friday and a Saturday.
        let back = Calendar {
            id: "back".into(),
            country: "XX".into(),
            weekend: vec![0, 6],
            observed: Observed::SatToFriSunToMon,
            valid_from: None, law_as_of: None, holidays: vec![
                h("eve", HolidayRule::Fixed { month: 7, day: 3 }, None),
                h("day", HolidayRule::Fixed { month: 7, day: 4 }, None),
            ],
        };
        assert!(!is_business_day(&dt(2026, 7, 3), &back));
        assert!(!is_business_day(&dt(2026, 7, 2), &back), "the Saturday holiday moved back to Thursday");
        assert!(is_business_day(&dt(2026, 7, 1), &back));
        assert!(is_business_day(&dt(2026, 7, 6), &back), "and nothing moved forward");

        // 4 July 2027 is a Sunday and 5 July a Monday. Both Sunday rules move it on to Tuesday.
        for observed in [Observed::SatToFriSunToMon, Observed::SunToMon] {
            let on = Calendar {
                id: "on".into(),
                country: "XX".into(),
                weekend: vec![0, 6],
                observed,
                valid_from: None, law_as_of: None, holidays: vec![
                    h("day", HolidayRule::Fixed { month: 7, day: 4 }, None),
                    h("first_monday", HolidayRule::NthWeekday { month: 7, weekday: 1, order: 1 }, None),
                ],
            };
            assert!(!is_business_day(&dt(2027, 7, 5), &on), "{observed:?}");
            assert!(!is_business_day(&dt(2027, 7, 6), &on), "{observed:?}: moved on to Tuesday");
            assert!(is_business_day(&dt(2027, 7, 7), &on), "{observed:?}");
        }
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
        // A Saturday rolls forward to Monday - a business day is itself (roll includes it).
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

    /// R4-S20. A calendar limited to the years its rules have a source for refuses a walk that steps on
    /// a day before its first year - the same walk without the limit is answered with today's rules,
    /// which is the wrong answer the limit exists to stop - and answers everything that stays inside.
    #[test]
    fn a_walk_that_steps_before_the_first_year_is_refused() {
        let cal = Calendar { valid_from: Some(2026), ..us(Observed::SunToMon) };
        // 2026-01-02 is a Friday: -1bd judges New Year's Day, then 31 December 2025.
        assert_eq!(add_business_days(&dt(2026, 1, 2), -1, &cal), Err(BusinessDayLimit::BeforeCalendar));
        let unlimited = us(Observed::SunToMon);
        assert_eq!(add_business_days(&dt(2026, 1, 2), -1, &unlimited), Ok(dt(2025, 12, 31)));
        // The start day is not judged, so +1bd from the last day before the range is answerable.
        assert_eq!(add_business_days(&dt(2025, 12, 31), 1, &cal), Ok(dt(2026, 1, 2)));
        // A walk that stays inside is untouched.
        assert_eq!(add_business_days(&dt(2026, 1, 2), 1, &cal), Ok(dt(2026, 1, 5)));
        assert_eq!(add_business_days(&dt(2026, 1, 5), -1, &cal), Ok(dt(2026, 1, 2)));
    }

    /// A roll judges the day it starts on, so a date before the range is refused even on a weekday, and
    /// rolling back from the first day of the range is refused when it has to step out of it.
    #[test]
    fn a_roll_that_judges_a_day_before_the_first_year_is_refused() {
        let cal = Calendar { valid_from: Some(2026), ..us(Observed::SunToMon) };
        assert_eq!(nearest_business_day(&dt(2025, 12, 31), true, &cal), Err(BusinessDayLimit::BeforeCalendar));
        assert_eq!(nearest_business_day(&dt(2026, 1, 1), false, &cal), Err(BusinessDayLimit::BeforeCalendar));
        assert_eq!(nearest_business_day(&dt(2026, 1, 1), true, &cal), Ok(dt(2026, 1, 2)));
        assert!(cal.judges_year(2026) && !cal.judges_year(2025));
        assert!(us(Observed::SunToMon).judges_year(1000), "a calendar built in code judges every year");
    }

    /// R4/20, the owner's case: data last checked in 2030, the program run in 2032. A date after the
    /// check counts as possibly missing a holiday only once the check is more than a year old.
    #[test]
    fn a_date_after_a_check_older_than_a_year_is_outdated() {
        let checked = Calendar { law_as_of: Some(dt(2030, 6, 1)), ..us(Observed::SunToMon) };
        assert!(outdated(&checked, &dt(2032, 3, 1), &dt(2032, 3, 10)));
        // The day after the check, the list is as current as anyone's - no warning on every later date.
        assert!(!outdated(&checked, &dt(2030, 6, 2), &dt(2032, 3, 10)));
        // A date the check already covered stays trusted, however old the check is.
        assert!(!outdated(&checked, &dt(2032, 3, 1), &dt(2030, 6, 1)));
        // 2031-06-01 is 365 days after the check - still fresh. A day later it is not.
        assert!(!outdated(&checked, &dt(2031, 6, 1), &dt(2031, 7, 1)));
        assert!(outdated(&checked, &dt(2031, 6, 2), &dt(2031, 7, 1)));
        // A clock behind the check, and a calendar built in code, never warn.
        assert!(!outdated(&checked, &dt(2029, 1, 1), &dt(2032, 3, 10)));
        assert!(!outdated(&us(Observed::SunToMon), &dt(2032, 3, 1), &dt(2032, 3, 10)));
    }

    /// Random properties of the business-day walk. A calendar is the one file outsiders are invited
    /// to write, so the walk meets every shape of week a file can state.
    mod properties {
        use super::*;
        use crate::search::{calendar, edge_i64, within};
        use proptest::prelude::*;
        use std::time::Duration;

        /// A start day in the years calendars are written for, at any time of day.
        fn start() -> impl Strategy<Value = CivilDateTime> {
            (1850i64..=2250, 1u32..=12, 1u32..=28, 0u32..=23, 0u32..=59, 0u32..=59).prop_map(
                |(year, month, day, hour, minute, second)| CivilDateTime { year, month, day, hour, minute, second },
            )
        }

        /// How many business days to walk: mostly a few, sometimes just past the limit, sometimes any
        /// number at all. A count AT the limit is a walk of a million business days, a second or more
        /// on a sparse week, so it is left out - past it is refused at once and is what is drawn.
        fn count() -> impl Strategy<Value = i64> {
            prop_oneof![
                12 => -60i64..=60,
                1 => (MAX_BUSINESS_DAYS + 1)..=(MAX_BUSINESS_DAYS + 3),
                1 => -(MAX_BUSINESS_DAYS + 3)..=-(MAX_BUSINESS_DAYS + 1),
                2 => edge_i64().prop_filter("a walk, not a refusal, is drawn above", |n| {
                    n.unsigned_abs() > MAX_BUSINESS_DAYS as u64 || n.unsigned_abs() <= 1_000
                }),
            ]
        }

        /// Business days from `from` up to `to`, the start not counted - each day judged on its own,
        /// apart from the walk's cache.
        fn business_days_between(from: &CivilDateTime, to: &CivilDateTime, cal: &Calendar) -> i64 {
            let (a, b) = (days(from.year, from.month, from.day), days(to.year, to.month, to.day));
            let span = if a <= b { (a + 1)..=b } else { b..=(a - 1) };
            span.filter(|day| is_business_day_days(*day, cal)).count() as i64
        }

        proptest! {
            #![proptest_config(ProptestConfig { max_shrink_iters: 64, max_shrink_time: 20_000, ..ProptestConfig::default() })]

            /// S-1 and R2-S10. On every calendar a file can write the walk ends, and with the answer
            /// that fits: a count past the limit is refused as one, a calendar without a working day
            /// is named degenerate (never walked for ever), a calendar is asked nothing before its
            /// first year, and a day reached keeps the start's time on the side the sign points to.
            #[test]
            fn the_walk_ends_with_the_answer_that_fits_on_every_calendar(
                cal in calendar(),
                from in start(),
                n in count(),
            ) {
                let walked = cal.clone();
                let got = within(Duration::from_secs(5), "add_business_days", move || {
                    add_business_days(&from, n, &walked)
                });
                let too_many = n.unsigned_abs() > MAX_BUSINESS_DAYS as u64;
                match got {
                    Err(BusinessDayLimit::TooManyDays) => prop_assert!(too_many),
                    Err(BusinessDayLimit::DegenerateCalendar) => prop_assert!(!too_many && n != 0),
                    Err(BusinessDayLimit::BeforeCalendar) => {
                        prop_assert!(!too_many && n != 0 && cal.valid_from.is_some());
                    }
                    Ok(day) => {
                        prop_assert!(!too_many);
                        prop_assert!(
                            cal.weekend.len() < 7 || n == 0,
                            "a calendar without a working day walked {} business days to {:?}", n, day
                        );
                        prop_assert_eq!((day.hour, day.minute, day.second), (from.hour, from.minute, from.second));
                        let moved = days(day.year, day.month, day.day) - days(from.year, from.month, from.day);
                        prop_assert_eq!(moved.signum(), n.signum());
                        prop_assert!(n == 0 || cal.judges_year(day.year));
                    }
                }
            }

            /// The count itself, on calendars that keep a working day every week: the day reached is
            /// a business day, and exactly |n| business days lie after the start up to it. A sparse
            /// week - six days off and holidays on top - walks as far as a full one, only slower
            /// (R4-N41: such a calendar ran the walk's budget dry and was refused `+1000bd`).
            #[test]
            fn every_business_day_between_is_counted_once(
                cal in calendar().prop_filter("a working day every week, every year judged", |c| {
                    c.weekend.len() < 7 && c.valid_from.is_none()
                }),
                from in start(),
                n in prop_oneof![4 => -40i64..=40, 1 => 1_000i64..=1_200, 1 => -1_200i64..=-1_000],
            ) {
                let walked = cal.clone();
                let got = within(Duration::from_secs(5), "add_business_days", move || {
                    add_business_days(&from, n, &walked)
                });
                let day = match got {
                    Ok(day) => day,
                    Err(why) => {
                        return Err(TestCaseError::fail(format!(
                            "{n} business days refused ({why:?}) on a calendar with a working day every week"
                        )));
                    }
                };
                prop_assert_eq!(business_days_between(&from, &day, &cal), n.abs());
                prop_assert!(n == 0 || is_business_day(&day, &cal), "the walk ended on a day off: {:?}", day);
            }
        }
    }
}
