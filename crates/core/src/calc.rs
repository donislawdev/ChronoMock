//! Date-arithmetic engine - the shared step model behind both the calculator
//! (Stage 4) and, later, the substitution moment grammar (`--at`/`jump`) and
//! preset `moment` (docs/04 section 4.3). ONE canonical evaluable representation,
//! many surfaces (zasady/15 section 3).
//!
//! Pure and unit-testable: `eval` takes an expression plus a context carrying
//! "real now" as DATA and returns plain values - no clock, no I/O, no platform
//! (rule 16, docs/07 section 4). The consumer (CLI driver, later GUI) resolves
//! `now` and renders the result.
//!
//! Why a step list and not a tick delta: months, quarters, and years are NOT a
//! fixed number of ticks (`+1 month` from Jan 31 is Feb 28, not +31 days), so the
//! canonical model folds typed steps onto a running civil date, not a scalar.
//! Stage-4 slice 1 builds `shift` (fixed and calendar units) and `set_time`;
//! `snap`, `nearest`, `zone`, and the `business_days` unit are present in the
//! model so the surface is complete, and return the product's honest "not built
//! yet" vocabulary rather than a silent skip or a faked result (zasady/01 section 2).

use super::{civil_from_days, days_from_civil, is_leap, last_day_of_month, parse_civil};

/// A civil date and time - wall-clock fields in the session zone, no UTC, no DST.
/// This is the running value the evaluator folds steps onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CivilDateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl CivilDateTime {
    /// Format as "YYYY-MM-DDTHH:MM:SS" - the ISO shape the rest of the tool speaks.
    pub fn to_iso(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// The same date at 00:00:00 - used to resolve the `today` base.
    fn at_midnight(&self) -> Self {
        Self { hour: 0, minute: 0, second: 0, ..*self }
    }
}

/// Sign of a `shift` step. Kept separate from the amount so the amount stays a
/// plain non-negative magnitude (the CLI carries the sign as its own token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    Plus,
    Minus,
}

/// A shift unit. Fixed-length units are a constant number of seconds; calendar
/// units depend on the anchor date and are folded through civil arithmetic.
/// `BusinessDays` is in the model but needs a calendar, so it is not built yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Seconds,
    Minutes,
    Hours,
    Days,
    Weeks,
    Months,
    Quarters,
    Years,
    BusinessDays,
}

impl Unit {
    /// Stable name for reports and translation keys (docs/04 section 4.3).
    pub fn name(&self) -> &'static str {
        match self {
            Unit::Seconds => "seconds",
            Unit::Minutes => "minutes",
            Unit::Hours => "hours",
            Unit::Days => "days",
            Unit::Weeks => "weeks",
            Unit::Months => "months",
            Unit::Quarters => "quarters",
            Unit::Years => "years",
            Unit::BusinessDays => "business_days",
        }
    }
}

/// A snap step's target: the calendar period whose boundary to jump to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapTarget {
    StartOfMonth,
    EndOfMonth,
    StartOfQuarter,
    EndOfQuarter,
    StartOfYear,
    EndOfYear,
}

impl SnapTarget {
    /// Human label for reports.
    pub fn label(&self) -> &'static str {
        match self {
            SnapTarget::StartOfMonth => "start of month",
            SnapTarget::EndOfMonth => "end of month",
            SnapTarget::StartOfQuarter => "start of quarter",
            SnapTarget::EndOfQuarter => "end of quarter",
            SnapTarget::StartOfYear => "start of year",
            SnapTarget::EndOfYear => "end of year",
        }
    }
}

/// A nearest step's target: the closest date of a kind, in one direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NearestTarget {
    /// The nearest business day on or after the current date (itself if it is one).
    NextBusinessDay,
    /// The nearest business day on or before the current date.
    PrevBusinessDay,
    /// The next 29 February on or after the current date (itself if the date is a leap day) - the
    /// nearest future leap day. Pure calendar arithmetic, so it needs no holiday calendar.
    NextLeapDay,
}

impl NearestTarget {
    /// Human label for reports.
    pub fn label(&self) -> &'static str {
        match self {
            NearestTarget::NextBusinessDay => "next business day",
            NearestTarget::PrevBusinessDay => "previous business day",
            NearestTarget::NextLeapDay => "next 29 February",
        }
    }
}

/// One step of a moment expression. Step kinds mirror docs/04 section 4.3 exactly
/// so a preset `moment` and the calculator share one model. `Zone` carries the target
/// session-zone bias in minutes ("UTC = local + bias", the same convention as
/// `Moment::tz_bias_min`): it re-expresses the running moment in another fixed-offset
/// zone (same instant, different wall-clock), so `eval` intercepts it to track the zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Shift { sign: Sign, amount: i64, unit: Unit },
    SetTime { hour: u32, minute: u32, second: u32 },
    Snap(SnapTarget),
    Nearest(NearestTarget),
    Zone(i32),
}

impl Step {
    /// Stable kind name for the "not supported yet" key and reports.
    fn kind(&self) -> &'static str {
        match self {
            Step::Shift { .. } => "shift",
            Step::SetTime { .. } => "set_time",
            Step::Snap(_) => "snap",
            Step::Nearest(_) => "nearest",
            Step::Zone(_) => "zone",
        }
    }
}

/// Where an expression starts before its steps apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// Today at 00:00:00 in the session zone (resolved from the context's `now`).
    Today,
    /// The current instant in the session zone (resolved from the context's `now`).
    Now,
    /// An explicit civil date and time.
    Absolute(CivilDateTime),
}

/// A moment expression: a base plus an ordered list of steps. The canonical
/// representation both the calculator and (later) a preset `moment` deserialize to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MomentExpr {
    pub base: Base,
    pub steps: Vec<Step>,
}

/// What the evaluator needs from the outside: "real now" as data, so the core stays pure and a
/// test can pin it (docs/07 section 4), the session zone the base is expressed in (needed so a
/// `zone` step can convert away from it), plus an optional calendar for business-day arithmetic.
/// `calendar` is `None` on the substitution `--at`/`jump` paths, where `business_days` is then
/// honestly unsupported; `zone_bias_min` is the base zone there too (those paths build no `zone`
/// step, so it only sets the result zone, which they ignore).
#[derive(Debug, Clone, Copy)]
pub struct EvalContext<'a> {
    pub now: CivilDateTime,
    /// Session-zone bias in minutes ("UTC = local + bias"): the zone the base lives in.
    pub zone_bias_min: i32,
    pub calendar: Option<&'a crate::calendar::Calendar>,
}

/// The result of evaluating an expression: the base and the intermediate value
/// after each step, so the surface can show progress step by step (7.3 - the user
/// sees where they went wrong, not just the final number). `result_bias` is the zone
/// the FINAL moment is expressed in - it equals the session zone unless a `zone` step
/// re-expressed the moment elsewhere. Intermediate zones are not tracked in this slice
/// (the CLI needs only the final zone for its formats block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalOutcome {
    pub base: CivilDateTime,
    pub after_each: Vec<CivilDateTime>,
    pub result_bias: i32,
}

impl EvalOutcome {
    /// The final moment - the last step's result, or the base when there are no steps.
    pub fn result(&self) -> CivilDateTime {
        *self.after_each.last().unwrap_or(&self.base)
    }
}

/// Why an evaluation could not produce a result. Failure modes are enumerated, not
/// leaned on as a shared property (zasady/02 part 2): a step kind or unit not built
/// yet, arithmetic overflow (reported, never wrapped - zasady/15 section 7), or an
/// out-of-range `set_time`. Each carries the step index so the surface can point at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// A step kind that exists in the model but is not built in this release (only `zone`).
    StepUnsupported { kind: &'static str, index: usize },
    /// A built step or unit that needs a calendar the caller did not provide (the `business_days`
    /// unit, the `nearest` business-day step). The substitution `--at`/`jump` paths have none.
    NeedsCalendar { index: usize },
    /// A `nearest` target had no match within the search range - only a degenerate calendar.
    NotFound { index: usize },
    /// The shift overflowed the representable range - rejected, not wrapped.
    Overflow { index: usize },
    /// `set_time` fields out of range (hour > 23, minute/second > 59).
    BadSetTime { index: usize },
}

/// Parse a civil date-time in "YYYY-MM-DDTHH:MM:SS" form (a space may replace the `T`). The core's
/// `parse_civil` already rejects a day that cannot exist in its month (e.g. 2025-02-31), so this
/// maps the validated fields to `CivilDateTime` - one validation path shared with the substitution
/// moment, no silent normalization to March.
pub fn parse_civil_datetime(s: &str) -> Result<CivilDateTime, String> {
    let (y, mo, d, h, mi, sec) = parse_civil(s)?;
    Ok(CivilDateTime {
        year: y,
        month: mo as u32,
        day: d as u32,
        hour: h as u32,
        minute: mi as u32,
        second: sec as u32,
    })
}

/// Evaluate an expression against a context. Folds each step onto the running civil
/// value left to right, recording the value after each step.
pub fn eval(expr: &MomentExpr, ctx: &EvalContext) -> Result<EvalOutcome, EvalError> {
    let base = match &expr.base {
        Base::Today => ctx.now.at_midnight(),
        Base::Now => ctx.now,
        Base::Absolute(c) => *c,
    };
    let mut cur = base;
    let mut cur_bias = ctx.zone_bias_min;
    let mut after_each = Vec::with_capacity(expr.steps.len());
    for (i, step) in expr.steps.iter().enumerate() {
        // A `zone` step re-expresses the running moment in another fixed-offset zone (same
        // instant, new wall-clock and bias). It is the only step that changes the zone, so it is
        // handled here where the bias lives; every other step folds the civil date and keeps the
        // current zone. The substitution jump path never builds a `zone` step, so `apply_step`
        // still reports it as unsupported there.
        if let Step::Zone(target_bias) = step {
            cur = convert_zone(cur, cur_bias, *target_bias, i)?;
            cur_bias = *target_bias;
        } else {
            cur = apply_step(cur, step, i, ctx.calendar)?;
        }
        after_each.push(cur);
    }
    Ok(EvalOutcome { base, after_each, result_bias: cur_bias })
}

fn apply_step(
    cur: CivilDateTime,
    step: &Step,
    index: usize,
    calendar: Option<&crate::calendar::Calendar>,
) -> Result<CivilDateTime, EvalError> {
    match step {
        Step::Shift { sign, amount, unit } => apply_shift(cur, *sign, *amount, *unit, index, calendar),
        Step::SetTime { hour, minute, second } => {
            if *hour > 23 || *minute > 59 || *second > 59 {
                return Err(EvalError::BadSetTime { index });
            }
            Ok(CivilDateTime { hour: *hour, minute: *minute, second: *second, ..cur })
        }
        Step::Snap(target) => Ok(apply_snap(cur, *target)),
        Step::Nearest(target) => match target {
            // The leap day is pure arithmetic (the next Feb 29), so it needs no holiday calendar -
            // unlike the business-day targets, which do.
            NearestTarget::NextLeapDay => Ok(apply_nearest_leap_day(cur)),
            NearestTarget::NextBusinessDay | NearestTarget::PrevBusinessDay => match calendar {
                Some(cal) => apply_nearest(cur, *target, cal, index),
                None => Err(EvalError::NeedsCalendar { index }),
            },
        },
        // `eval` intercepts `Zone` (it needs the running zone bias, which this per-civil helper
        // does not carry). This arm is reached only from the substitution jump path, which cannot
        // change zone by a time delta - an honest "unsupported" there, never a silent skip.
        Step::Zone(_) => Err(EvalError::StepUnsupported { kind: step.kind(), index }),
    }
}

/// Re-express a civil moment from one fixed-offset zone in another, preserving the instant: read
/// `civil` in `bias_from` as a UTC instant, then read that instant back in `bias_to`. Reuses the
/// canonical instant conversions (the same the mechanism and the output formats use), so a `zone`
/// step introduces no new instant math. The instant is unchanged - only the wall-clock fields and
/// the offset move. An unrepresentable extreme date is reported as overflow, never wrapped.
fn convert_zone(
    civil: CivilDateTime,
    bias_from: i32,
    bias_to: i32,
    index: usize,
) -> Result<CivilDateTime, EvalError> {
    let ft = super::moment_to_filetime_utc(&super::Moment {
        local: civil.to_iso(),
        tz_bias_min: Some(bias_from),
    })
    .map_err(|_| EvalError::Overflow { index })?;
    Ok(filetime_to_civil(ft, bias_to))
}

/// Jump to the nearest business day in the target's direction (calendar::nearest_business_day).
fn apply_nearest(
    cur: CivilDateTime,
    target: NearestTarget,
    cal: &crate::calendar::Calendar,
    index: usize,
) -> Result<CivilDateTime, EvalError> {
    let forward = matches!(target, NearestTarget::NextBusinessDay);
    crate::calendar::nearest_business_day(&cur, forward, cal).ok_or(EvalError::NotFound { index })
}

/// Jump to the next 29 February on or after `cur` (itself if `cur` is already a leap day). The year
/// advances one at a time, not by a blind +4, so the century rule (2100 is a common year, 2000 is
/// not) is decided by `is_leap`. The time of day is kept. Pure arithmetic - no holiday calendar.
fn apply_nearest_leap_day(cur: CivilDateTime) -> CivilDateTime {
    // Is this year's Feb 29 still ahead of (or equal to) cur? Only when cur is on or before Feb 29 -
    // i.e. in January, or in February (whose last day is at most the 29th).
    let on_or_before_feb29 = cur.month < 2 || (cur.month == 2 && cur.day <= 29);
    let mut year = cur.year;
    if !(is_leap(year) && on_or_before_feb29) {
        year += 1;
        while !is_leap(year) {
            year += 1;
        }
    }
    CivilDateTime { year, month: 2, day: 29, ..cur }
}

/// Jump to the boundary of the calendar period containing `cur`. A "start" is the first day at
/// 00:00:00, an "end" the last day at 23:59:59 - the earliest / latest instant of the period, so a
/// period-end test needs no extra set-time step.
fn apply_snap(cur: CivilDateTime, target: SnapTarget) -> CivilDateTime {
    let quarter_start = (cur.month - 1) / 3 * 3 + 1; // 1, 4, 7, 10
    let start = |month| CivilDateTime { year: cur.year, month, day: 1, hour: 0, minute: 0, second: 0 };
    let end = |month| CivilDateTime {
        year: cur.year,
        month,
        day: last_day_of_month(cur.year, month as i64),
        hour: 23,
        minute: 59,
        second: 59,
    };
    match target {
        SnapTarget::StartOfMonth => start(cur.month),
        SnapTarget::EndOfMonth => end(cur.month),
        SnapTarget::StartOfQuarter => start(quarter_start),
        SnapTarget::EndOfQuarter => end(quarter_start + 2),
        SnapTarget::StartOfYear => start(1),
        SnapTarget::EndOfYear => end(12),
    }
}

fn apply_shift(
    cur: CivilDateTime,
    sign: Sign,
    amount: i64,
    unit: Unit,
    index: usize,
    calendar: Option<&crate::calendar::Calendar>,
) -> Result<CivilDateTime, EvalError> {
    // The amount is a non-negative magnitude; apply the sign here.
    let signed = match sign {
        Sign::Plus => amount,
        Sign::Minus => amount.checked_neg().ok_or(EvalError::Overflow { index })?,
    };
    match unit {
        Unit::Seconds | Unit::Minutes | Unit::Hours | Unit::Days | Unit::Weeks => {
            shift_fixed(cur, signed, unit, index)
        }
        Unit::Months => shift_months(cur, signed, index),
        // A quarter is three months, a year is twelve - one calendar-fold path with
        // the same last-day clamp, so leap-day and month-end behaviour stay identical.
        Unit::Quarters => {
            shift_months(cur, signed.checked_mul(3).ok_or(EvalError::Overflow { index })?, index)
        }
        Unit::Years => {
            shift_months(cur, signed.checked_mul(12).ok_or(EvalError::Overflow { index })?, index)
        }
        // Business days need a calendar (weekends plus holidays). With one, walk the calendar;
        // without one (the substitution paths), stay honestly unsupported.
        Unit::BusinessDays => match calendar {
            Some(cal) => {
                crate::calendar::add_business_days(&cur, signed, cal).ok_or(EvalError::Overflow { index })
            }
            None => Err(EvalError::NeedsCalendar { index }),
        },
    }
}

/// Shift by a fixed-length unit: convert the instant to total seconds, add the
/// signed delta, and convert back. Every multiply/add is checked so an extreme
/// amount is rejected, never wrapped into a plausible-but-false date (zasady/15 s7).
fn shift_fixed(
    cur: CivilDateTime,
    signed_amount: i64,
    unit: Unit,
    index: usize,
) -> Result<CivilDateTime, EvalError> {
    let unit_secs: i64 = match unit {
        Unit::Seconds => 1,
        Unit::Minutes => 60,
        Unit::Hours => 3_600,
        Unit::Days => 86_400,
        Unit::Weeks => 604_800,
        _ => unreachable!("shift_fixed only handles fixed-length units"),
    };
    let ovf = || EvalError::Overflow { index };
    let delta = signed_amount.checked_mul(unit_secs).ok_or_else(ovf)?;
    let day = days_from_civil(cur.year, cur.month as i64, cur.day as i64);
    let tod = cur.hour as i64 * 3_600 + cur.minute as i64 * 60 + cur.second as i64;
    let total = day
        .checked_mul(86_400)
        .and_then(|d| d.checked_add(tod))
        .and_then(|t| t.checked_add(delta))
        .ok_or_else(ovf)?;
    let new_days = total.div_euclid(86_400);
    let secs = total.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(new_days);
    Ok(CivilDateTime {
        year: y,
        month: m as u32,
        day: d as u32,
        hour: (secs / 3_600) as u32,
        minute: (secs % 3_600 / 60) as u32,
        second: (secs % 60) as u32,
    })
}

/// Shift by a whole number of months, clamping the day to the last valid day of the
/// resulting month. This is the anchor-dependent case the tick model cannot express:
/// Jan 31 + 1 month = Feb 28/29, and Feb 29 + 12 months = Feb 28. Time of day is kept.
fn shift_months(cur: CivilDateTime, months: i64, index: usize) -> Result<CivilDateTime, EvalError> {
    let ovf = || EvalError::Overflow { index };
    // Absolute month index since year 0, month 0 (January).
    let total = cur
        .year
        .checked_mul(12)
        .and_then(|y| y.checked_add(cur.month as i64 - 1))
        .and_then(|t| t.checked_add(months))
        .ok_or_else(ovf)?;
    let new_year = total.div_euclid(12);
    let new_month = total.rem_euclid(12) + 1; // 1..=12
    let last = last_day_of_month(new_year, new_month);
    Ok(CivilDateTime {
        year: new_year,
        month: new_month as u32,
        day: cur.day.min(last),
        hour: cur.hour,
        minute: cur.minute,
        second: cur.second,
    })
}

// --- Bridge to the substitution tick world (the relative `jump` path) ---------------
//
// The substitution fake clock is a UTC FILETIME; a relative jump means "advance the fake
// clock by one step". Fixed-length units are a tick delta (kept exact, sub-second and all);
// calendar units are defined on the civil date in the SESSION zone, so they fold through it.

/// Session-local civil fields from a UTC FILETIME. Pure arithmetic mirroring the civil half
/// of `filetime_utc_to_wall` (no string, no parse, so it cannot fail).
fn filetime_to_civil(ft_utc: i64, tz_bias_min: i32) -> CivilDateTime {
    // Session-local = UTC - bias (UTC = local + bias).
    let local_ticks = ft_utc - (tz_bias_min as i64) * 60 * 10_000_000;
    const DAYS_1601_TO_1970: i64 = 134_774;
    let secs_1601 = local_ticks.div_euclid(10_000_000);
    let secs_1970 = secs_1601 - DAYS_1601_TO_1970 * 86_400;
    let days = secs_1970.div_euclid(86_400);
    let tod = secs_1970.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    CivilDateTime {
        year: y,
        month: mo as u32,
        day: d as u32,
        hour: (tod / 3_600) as u32,
        minute: (tod % 3_600 / 60) as u32,
        second: (tod % 60) as u32,
    }
}

/// Signed length of a FIXED-length shift step in FILETIME ticks (100 ns), or `None` if the
/// step is not a fixed-length shift (calendar units and business days need the civil fold,
/// non-shift steps have no tick length). `Err` only on overflow of a fixed shift.
pub fn fixed_shift_ticks(step: &Step) -> Result<Option<i64>, EvalError> {
    let Step::Shift { sign, amount, unit } = step else {
        return Ok(None);
    };
    let unit_secs: i64 = match unit {
        Unit::Seconds => 1,
        Unit::Minutes => 60,
        Unit::Hours => 3_600,
        Unit::Days => 86_400,
        Unit::Weeks => 604_800,
        _ => return Ok(None), // calendar unit or business days: civil fold
    };
    let signed = match sign {
        Sign::Plus => *amount,
        Sign::Minus => amount.checked_neg().ok_or(EvalError::Overflow { index: 0 })?,
    };
    let ticks = signed
        .checked_mul(unit_secs)
        .and_then(|s| s.checked_mul(10_000_000))
        .ok_or(EvalError::Overflow { index: 0 })?;
    Ok(Some(ticks))
}

/// Apply one calendar shift step to a UTC FILETIME read as wall-clock in `tz_bias_min`.
/// Round-trips ft -> session-local civil -> shifted civil -> ft, reusing the canonical
/// `moment_to_filetime_utc` (the same civil->ft the mechanism uses). Overflow of the
/// resulting instant is reported.
fn shift_filetime(ft_utc: i64, tz_bias_min: i32, step: &Step) -> Result<i64, EvalError> {
    let civil = filetime_to_civil(ft_utc, tz_bias_min);
    // No calendar on the jump path, so a business-day step stays unsupported here.
    let shifted = apply_step(civil, step, 0, None)?;
    super::moment_to_filetime_utc(&super::Moment {
        local: shifted.to_iso(),
        tz_bias_min: Some(tz_bias_min),
    })
    .map_err(|_| EvalError::Overflow { index: 0 })
}

/// The target UTC FILETIME after applying ONE shift step to the current fake instant - the
/// single source of truth for "current fake + one step" on the substitution jump path. A
/// fixed-length unit adds a tick delta (sub-second precision preserved); a calendar unit
/// folds through the civil date in the session zone. Business days are not built yet.
pub fn step_target(fake_now_ft: i64, tz_bias_min: i32, step: &Step) -> Result<i64, EvalError> {
    if let Some(ticks) = fixed_shift_ticks(step)? {
        return fake_now_ft.checked_add(ticks).ok_or(EvalError::Overflow { index: 0 });
    }
    shift_filetime(fake_now_ft, tz_bias_min, step)
}

// --- Output formats (the calculator's right column, docs/02 section 8) ---------------

const MONTH_ABBR: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTH_FULL: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
    "November", "December",
];
const DOW_ABBR: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]; // 0 = Sunday

/// Day of week for a civil date: 0 = Sunday .. 6 = Saturday. 1970-01-01 (day 0) was a Thursday.
fn day_of_week(civil: &CivilDateTime) -> usize {
    (days_from_civil(civil.year, civil.month as i64, civil.day as i64) + 4).rem_euclid(7) as usize
}

/// A "+HH:MM" / "-HH:MM" offset for a session bias (UTC = local + bias, so the offset is -bias).
fn offset_label(tz_bias_min: i32) -> String {
    let off = -tz_bias_min;
    let sign = if off < 0 { '-' } else { '+' };
    format!("{sign}{:02}:{:02}", off.abs() / 60, off.abs() % 60)
}

/// A moment rendered in every fixed output format at once (docs/02 section 8, in that order).
/// The civil formats are always present; the instant-based ones (epoch, FILETIME, RFC 1123)
/// need the UTC instant and are `None` only when the civil date is outside the FILETIME range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formats {
    pub iso_date: String,
    pub iso_datetime: String,
    pub us: String,
    pub pl: String,
    pub rfc1123: Option<String>,
    pub epoch_seconds: Option<i64>,
    pub epoch_millis: Option<i64>,
    pub filetime: Option<i64>,
}

/// FILETIME ticks (100 ns since 1601) at the Unix epoch (1970-01-01T00:00:00Z).
const FT_1970: i64 = 116_444_736_000_000_000;

/// The UTC FILETIME of a civil moment read in the session zone `tz_bias_min`, or `None` if the
/// civil date cannot be represented as an instant (an extreme year whose ISO form does not
/// round-trip). The single instant conversion behind both the instant-based output formats and
/// the instant-based significance markers.
fn instant_filetime(civil: &CivilDateTime, tz_bias_min: i32) -> Option<i64> {
    super::moment_to_filetime_utc(&super::Moment { local: civil.to_iso(), tz_bias_min: Some(tz_bias_min) }).ok()
}

/// Render `civil` (wall-clock in the session zone `tz_bias_min`) in every fixed format.
pub fn formats(civil: &CivilDateTime, tz_bias_min: i32) -> Formats {
    let iso_date = format!("{:04}-{:02}-{:02}", civil.year, civil.month, civil.day);
    let iso_datetime = format!("{}{}", civil.to_iso(), offset_label(tz_bias_min));
    let us = format!("{:02}/{:02}/{:04}", civil.month, civil.day, civil.year);
    let pl = format!("{:02}.{:02}.{:04}", civil.day, civil.month, civil.year);

    // Instant-based formats: the civil moment interpreted in the session zone as a UTC instant.
    let (rfc1123, epoch_seconds, epoch_millis, filetime) = match instant_filetime(civil, tz_bias_min) {
        Some(ft) => {
            let utc = filetime_to_civil(ft, 0);
            let rfc = format!(
                "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
                DOW_ABBR[day_of_week(&utc)],
                utc.day,
                MONTH_ABBR[(utc.month - 1) as usize],
                utc.year,
                utc.hour,
                utc.minute,
                utc.second
            );
            (Some(rfc), Some((ft - FT_1970) / 10_000_000), Some((ft - FT_1970) / 10_000), Some(ft))
        }
        None => (None, None, None, None),
    };

    Formats { iso_date, iso_datetime, us, pl, rfc1123, epoch_seconds, epoch_millis, filetime }
}

// --- Custom format mask (7.3, docs/02 section 8 point 9 - hit the target app's exact format) ---

/// Render `civil` in a user-supplied .NET/Java-style mask so the output can match the exact format
/// the target app uses. Case-sensitive: `M` is month, `m` is minute. A token is a maximal run of
/// one letter (`yyyy`, `MM`, `dd`, `HH`, `mm`, `ss`, plus `MMM`/`MMMM` month names and `ddd`/`dddd`
/// weekday names); anything else, and any run whose length is not a known token (e.g. `yyy`), is
/// emitted literally - never guessed. Escaping a literal that happens to be a token letter, a zone
/// token, and single `y` are not built yet.
pub fn format_with_mask(civil: &CivilDateTime, mask: &str) -> String {
    let chars: Vec<char> = mask.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Consume the maximal run of the same character.
        let mut n = 1;
        while i + n < chars.len() && chars[i + n] == c {
            n += 1;
        }
        match mask_token(civil, c, n) {
            Some(field) => out.push_str(&field),
            None => (0..n).for_each(|_| out.push(c)),
        }
        i += n;
    }
    out
}

/// The field for a mask token: character `c` repeated `n` times, or None when `(c, n)` is not a
/// recognised token (then the caller emits the run literally). The .NET / Java token vocabulary.
fn mask_token(civil: &CivilDateTime, c: char, n: usize) -> Option<String> {
    let month = (civil.month - 1) as usize;
    let dow = day_of_week(civil);
    Some(match (c, n) {
        ('y', 4) => format!("{:04}", civil.year),
        ('y', 2) => format!("{:02}", civil.year.rem_euclid(100)),
        ('M', 4) => MONTH_FULL[month].to_string(),
        ('M', 3) => MONTH_ABBR[month].to_string(),
        ('M', 2) => format!("{:02}", civil.month),
        ('M', 1) => civil.month.to_string(),
        ('d', 4) => DOW_FULL[dow].to_string(),
        ('d', 3) => DOW_ABBR[dow].to_string(),
        ('d', 2) => format!("{:02}", civil.day),
        ('d', 1) => civil.day.to_string(),
        ('H', 2) => format!("{:02}", civil.hour),
        ('H', 1) => civil.hour.to_string(),
        ('m', 2) => format!("{:02}", civil.minute),
        ('m', 1) => civil.minute.to_string(),
        ('s', 2) => format!("{:02}", civil.second),
        ('s', 1) => civil.second.to_string(),
        _ => return None,
    })
}

// --- Metadata (the calculator's info block, 7.3 - calendar-independent fields) --------

const DOW_FULL: [&str; 7] =
    ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]; // 0 = Sunday

/// Day of the year, 1..=366.
fn day_of_year(civil: &CivilDateTime) -> i64 {
    days_from_civil(civil.year, civil.month as i64, civil.day as i64)
        - days_from_civil(civil.year, 1, 1)
        + 1
}

/// ISO 8601 weekday: 1 = Monday .. 7 = Sunday.
fn iso_weekday(civil: &CivilDateTime) -> i64 {
    match day_of_week(civil) {
        0 => 7, // Sunday
        d => d as i64,
    }
}

/// Whether an ISO week-year has 53 weeks: its Jan 1 is a Thursday, or it is a leap year
/// whose Jan 1 is a Wednesday.
fn is_long_iso_year(year: i64) -> bool {
    let jan1 = day_of_week(&CivilDateTime { year, month: 1, day: 1, hour: 0, minute: 0, second: 0 });
    jan1 == 4 || (is_leap(year) && jan1 == 3)
}

/// ISO 8601 week: (week-numbering year, week 1..=53). The week-year can differ from the
/// calendar year at the boundaries (2005-01-01 is 2004-W53; 2019-12-30 is 2020-W01).
fn iso_week(civil: &CivilDateTime) -> (i64, u32) {
    let week = (day_of_year(civil) - iso_weekday(civil) + 10) / 7;
    if week < 1 {
        return (civil.year - 1, if is_long_iso_year(civil.year - 1) { 53 } else { 52 });
    }
    if week > 52 && !is_long_iso_year(civil.year) {
        return (civil.year + 1, 1);
    }
    (civil.year, week as u32)
}

/// US week number: weeks start Sunday and week 1 contains January 1 (docs/02 section 7).
/// A different number from the ISO week, on purpose - both are shown side by side (7.3).
fn us_week(civil: &CivilDateTime) -> u32 {
    let jan1 = days_from_civil(civil.year, 1, 1);
    let jan1_dow = (jan1 + 4).rem_euclid(7); // 0 = Sunday, same basis as day_of_week
    let week1_sunday = jan1 - jan1_dow; // the Sunday that starts week 1
    let days = days_from_civil(civil.year, civil.month as i64, civil.day as i64);
    ((days - week1_sunday) / 7 + 1) as u32
}

/// The calendar-independent metadata for a result (7.3). Calendar-dependent fields
/// (business day, holiday) arrive with the calendar catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub weekday: &'static str,
    pub iso_week_year: i64,
    pub iso_week: u32,
    pub us_week: u32,
    pub day_of_year: u32,
    pub quarter: u32,
    pub is_leap_year: bool,
    /// Whole days from `today` to this date (date only, ignoring time of day). Signed.
    pub days_from_today: i64,
}

/// Compute the metadata for `civil`, with `today` supplied as data (the core stays pure).
pub fn metadata(civil: &CivilDateTime, today: &CivilDateTime) -> Metadata {
    let (iso_week_year, iso_week) = iso_week(civil);
    Metadata {
        weekday: DOW_FULL[day_of_week(civil)],
        iso_week_year,
        iso_week,
        us_week: us_week(civil),
        day_of_year: day_of_year(civil) as u32,
        quarter: (civil.month - 1) / 3 + 1,
        is_leap_year: is_leap(civil.year),
        days_from_today: days_from_civil(civil.year, civil.month as i64, civil.day as i64)
            - days_from_civil(today.year, today.month as i64, today.day as i64),
    }
}

// --- Significance ("what this date tests", 7.3 - the calculator's differentiator) ------
//
// The calculator names the edge case a result date lands on, instead of giving a number and
// staying silent like an online date calculator (6.2). Calendar-independent landmarks are always
// checked; the weekend / holiday / observed-holiday landmarks need a calendar and are added only
// when one is supplied. Daylight-saving transitions (which need the zone's DST rules the tool does
// not carry) are honestly NOT built yet, not silently omitted (docs/08 section 9a, rule 4).

/// A test-relevant landmark a date lands on. Emitted as stable variants, never prose: the text is
/// a translated string (docs/02 section 8, rule 15), so the core names the landmark and the
/// consumer renders it (the CLI in English via `label`, the calculator GUI later in the interface
/// language). `label` returns English human text, matching the other calc enums (`SnapTarget`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Significance {
    /// The first day of the month.
    StartOfMonth,
    /// The last day of the month.
    EndOfMonth,
    /// The first day of a calendar quarter (Jan / Apr / Jul / Oct 1).
    StartOfQuarter,
    /// The last day of a calendar quarter (Mar 31 / Jun 30 / Sep 30 / Dec 31).
    EndOfQuarter,
    /// The first day of the year (Jan 1).
    StartOfYear,
    /// The last day of the year (Dec 31).
    EndOfYear,
    /// February 29 - the leap day.
    LeapDay,
    /// February 28 of a common (non-leap) year - the last day of February with no 29th.
    LastDayOfFebruaryCommonYear,
    /// The Unix epoch instant (epoch second 0, 1970-01-01T00:00:00Z).
    UnixEpoch,
    /// At or past the signed 32-bit `time_t` limit (2038-01-19T03:14:07Z, `i32::MAX` seconds).
    Year2038Boundary,
    /// A weekend day (per the calendar's weekend), so not a business day.
    Weekend,
    /// A public holiday on its own calendar date (the calendar names which one in the metadata).
    PublicHoliday,
    /// A weekday that is a day off because a holiday is OBSERVED on it - its actual date fell on a
    /// weekend and shifted here. Explains a "not a business day, yet not a holiday" date.
    ObservedHoliday,
}

impl Significance {
    /// Stable machine token for the JSON calc output (chronomock.calc/1) and, later, translation keys.
    /// It never changes with wording, unlike `label` (rule 17: contract keys are public names).
    pub fn key(&self) -> &'static str {
        match self {
            Significance::StartOfMonth => "start_of_month",
            Significance::EndOfMonth => "end_of_month",
            Significance::StartOfQuarter => "start_of_quarter",
            Significance::EndOfQuarter => "end_of_quarter",
            Significance::StartOfYear => "start_of_year",
            Significance::EndOfYear => "end_of_year",
            Significance::LeapDay => "leap_day",
            Significance::LastDayOfFebruaryCommonYear => "last_day_of_february_common_year",
            Significance::UnixEpoch => "unix_epoch",
            Significance::Year2038Boundary => "year_2038_boundary",
            Significance::Weekend => "weekend",
            Significance::PublicHoliday => "public_holiday",
            Significance::ObservedHoliday => "observed_holiday",
        }
    }

    /// English human label for the CLI (docs/08 section 9a). Becomes a translation key when the
    /// calculator GUI arrives, like the rest of calc.
    pub fn label(&self) -> &'static str {
        match self {
            Significance::StartOfMonth => "first day of the month",
            Significance::EndOfMonth => "last day of the month (month-end)",
            Significance::StartOfQuarter => "first day of the quarter",
            Significance::EndOfQuarter => "last day of the quarter (quarter-end)",
            Significance::StartOfYear => "first day of the year",
            Significance::EndOfYear => "last day of the year (year-end rollover)",
            Significance::LeapDay => "February 29 - leap day",
            Significance::LastDayOfFebruaryCommonYear => {
                "February 28 - last day of February (common year, no Feb 29)"
            }
            Significance::UnixEpoch => "Unix epoch (1970-01-01T00:00:00Z, epoch 0)",
            Significance::Year2038Boundary => {
                "at or past the 2038-01-19T03:14:07Z 32-bit time_t limit (Y2038)"
            }
            Significance::Weekend => "weekend - not a business day",
            Significance::PublicHoliday => "public holiday",
            Significance::ObservedHoliday => {
                "observed public holiday - a day off shifted here from a weekend holiday"
            }
        }
    }
}

/// The test-relevant landmarks `civil` (read in the session zone `tz_bias_min`) lands on, in
/// priority order (7.3). This is a POSITIVE signal: it names what it detects and stays silent
/// otherwise, never claiming a landmark it did not verify (rule 4). The list is empty when the
/// date hits nothing notable.
///
/// Period boundaries collapse to the single strongest - year subsumes quarter subsumes month - so
/// Dec 31 reads "last day of the year", not three lines. In February the leap-day and last-day-of-
/// a-common-year landmarks stand in for the generic month-end. Instant markers (epoch, 2038) need
/// the UTC instant of the result: a pre-epoch date has a negative instant, so they stay silent,
/// and a year that cannot be represented as an instant at all is skipped rather than guessed.
///
/// The weekend / holiday / observed-holiday landmarks are added only when `calendar` is supplied
/// (the same opt-in the metadata block uses); without one they are simply absent, never guessed.
pub fn significance(
    civil: &CivilDateTime,
    tz_bias_min: i32,
    calendar: Option<&crate::calendar::Calendar>,
) -> Vec<Significance> {
    let mut out = Vec::new();

    let last = last_day_of_month(civil.year, civil.month as i64);
    let quarter_start_month = (civil.month - 1) / 3 * 3 + 1; // 1, 4, 7, 10

    if civil.month == 2 && civil.day == 29 {
        out.push(Significance::LeapDay);
    } else if civil.month == 2 && civil.day == 28 && !is_leap(civil.year) {
        out.push(Significance::LastDayOfFebruaryCommonYear);
    } else if civil.day == last {
        // Strongest end-of-period: year, then quarter, then month.
        out.push(if civil.month == 12 {
            Significance::EndOfYear
        } else if civil.month == quarter_start_month + 2 {
            Significance::EndOfQuarter
        } else {
            Significance::EndOfMonth
        });
    } else if civil.day == 1 {
        // Strongest start-of-period: year, then quarter, then month.
        out.push(if civil.month == 1 {
            Significance::StartOfYear
        } else if civil.month == quarter_start_month {
            Significance::StartOfQuarter
        } else {
            Significance::StartOfMonth
        });
    }

    // Instant markers: the result interpreted in the session zone as a UTC instant.
    if let Some(ft) = instant_filetime(civil, tz_bias_min) {
        let epoch = (ft - FT_1970) / 10_000_000;
        if epoch == 0 {
            out.push(Significance::UnixEpoch);
        }
        if epoch >= i32::MAX as i64 {
            out.push(Significance::Year2038Boundary);
        }
    }

    // Calendar-dependent markers: only when a calendar is supplied. Derived from the existing engine
    // predicates, so a weekend holiday reports both, while a non-business weekday that is not the
    // holiday's own date can only be an observed (shifted) holiday.
    if let Some(cal) = calendar {
        let is_weekend = cal.weekend.contains(&(day_of_week(civil) as u32));
        let is_holiday = crate::calendar::holiday_on(civil, cal).is_some();
        if is_weekend {
            out.push(Significance::Weekend);
        }
        if is_holiday {
            out.push(Significance::PublicHoliday);
        }
        if !is_weekend && !is_holiday && !crate::calendar::is_business_day(civil, cal) {
            out.push(Significance::ObservedHoliday);
        }
    }

    out
}

// --- Reverse analysis (7.3 - paste a date, name what it is; show both readings when ambiguous) ---

/// One reading of an analyzed date string. A numeric date with no clear order has two (US month-
/// first, PL day-first); an ISO date has one. Stable variants, English `label` like the other enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateReading {
    /// An unambiguous ISO 8601 date (dash-separated).
    Iso,
    /// The US reading of a numeric date: month first (MM/DD/YYYY).
    UsMonthDay,
    /// The Polish / European reading of a numeric date: day first (DD/MM/YYYY).
    PlDayMonth,
}

impl DateReading {
    /// Stable machine token for the JSON calc output (chronomock.calc/1), unlike the human `label`.
    pub fn key(&self) -> &'static str {
        match self {
            DateReading::Iso => "iso",
            DateReading::UsMonthDay => "us_month_day",
            DateReading::PlDayMonth => "pl_day_month",
        }
    }

    /// English label for the CLI (a translation key when the calculator GUI arrives).
    pub fn label(&self) -> &'static str {
        match self {
            DateReading::Iso => "ISO 8601",
            DateReading::UsMonthDay => "US MM/DD/YYYY",
            DateReading::PlDayMonth => "PL DD/MM/YYYY",
        }
    }
}

/// The result of analyzing a pasted date string: the reading(s) it resolves to. Two readings mean
/// the numeric order is genuinely ambiguous - 04/08 is April 8 in the US and August 4 in Poland, a
/// real source of bad bug reports (7.3), so the tool shows BOTH rather than choosing one silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateAnalysis {
    pub readings: Vec<(DateReading, CivilDateTime)>,
}

impl DateAnalysis {
    /// Whether the input has more than one valid reading.
    pub fn is_ambiguous(&self) -> bool {
        self.readings.len() > 1
    }
}

/// A civil date at midnight, or None if the month or day cannot exist - the validity gate that
/// turns a candidate month/day order into a real date (month 13 and Feb 30 are rejected).
fn civil_date(year: i64, month: i64, day: i64) -> Option<CivilDateTime> {
    if !(1..=12).contains(&month) || day < 1 || day > last_day_of_month(year, month) as i64 {
        return None;
    }
    Some(CivilDateTime { year, month: month as u32, day: day as u32, hour: 0, minute: 0, second: 0 })
}

/// Recognise a pasted date and return its reading(s). ISO (dash-separated, optional time) is
/// unambiguous; a slash- or dot-separated `N/N/YYYY` yields the US and PL readings, keeping only
/// the ones that form a real date (so 29/02 is PL-only, 02/29 US-only, 02/30 an error, 05/05 one).
/// Formats beyond these - epoch, FILETIME, RFC 1123, year-first numeric - are honestly not
/// recognised yet (an error, never a guess).
pub fn analyze_date(input: &str) -> Result<DateAnalysis, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("empty input".into());
    }
    // ISO: dash-separated, with or without a time (midnight if the time is absent).
    if s.contains('-') {
        let iso = if s.contains('T') || s.contains(' ') { s.to_string() } else { format!("{s}T00:00:00") };
        let civil = parse_civil_datetime(&iso)?;
        return Ok(DateAnalysis { readings: vec![(DateReading::Iso, civil)] });
    }
    // Numeric: exactly one separator kind, three parts, a four-digit year last.
    let sep = match (s.contains('/'), s.contains('.')) {
        (true, false) => '/',
        (false, true) => '.',
        _ => return Err(format!("unrecognised date format '{input}'")),
    };
    let parts: Vec<&str> = s.split(sep).collect();
    if parts.len() != 3 || parts[2].len() != 4 {
        return Err(format!("expected N{sep}N{sep}YYYY, got '{input}'"));
    }
    let num = |p: &str| p.parse::<i64>().map_err(|_| format!("bad number in '{input}'"));
    let (a, b, year) = (num(parts[0])?, num(parts[1])?, num(parts[2])?);
    // Two candidate orders; keep the ones that form a real date, and never list one date twice.
    let mut readings = Vec::new();
    if let Some(c) = civil_date(year, a, b) {
        readings.push((DateReading::UsMonthDay, c)); // month = a, day = b
    }
    if let Some(c) = civil_date(year, b, a) {
        // day = a, month = b
        if readings.first().map(|(_, uc)| *uc) != Some(c) {
            readings.push((DateReading::PlDayMonth, c));
        }
    }
    if readings.is_empty() {
        return Err(format!("'{input}' is not a valid date in either month/day order"));
    }
    Ok(DateAnalysis { readings })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> CivilDateTime {
        CivilDateTime { year: y, month: mo, day: d, hour: h, minute: mi, second: s }
    }

    fn abs(c: CivilDateTime) -> Base {
        Base::Absolute(c)
    }

    fn shift(sign: Sign, amount: i64, unit: Unit) -> Step {
        Step::Shift { sign, amount, unit }
    }

    /// Evaluate against a fixed `now` (no calendar, UTC session zone) so `today`/`now` bases are
    /// deterministic.
    fn eval_at(expr: &MomentExpr, now: CivilDateTime) -> Result<EvalOutcome, EvalError> {
        eval(expr, &EvalContext { now, zone_bias_min: 0, calendar: None })
    }

    /// Evaluate with an explicit session-zone bias (no calendar) - for the `zone` step, whose
    /// whole point is a base expressed in one zone and re-expressed in another.
    fn eval_at_zone(expr: &MomentExpr, now: CivilDateTime, zone_bias_min: i32) -> Result<EvalOutcome, EvalError> {
        eval(expr, &EvalContext { now, zone_bias_min, calendar: None })
    }

    /// Significance with no calendar - for the calendar-independent landmark tests.
    fn sig(civil: &CivilDateTime, tz_bias_min: i32) -> Vec<Significance> {
        significance(civil, tz_bias_min, None)
    }

    #[test]
    fn no_steps_is_identity() {
        // The key degenerate-but-legal input: an expression with zero steps returns
        // the base unchanged, never an error.
        let base = dt(2025, 1, 1, 0, 0, 0);
        let out = eval_at(&MomentExpr { base: abs(base), steps: vec![] }, dt(2000, 1, 1, 0, 0, 0)).unwrap();
        assert_eq!(out.result(), base);
        assert!(out.after_each.is_empty());
    }

    #[test]
    fn section_7_3_example_minus_18_years_minus_1_day_set_time() {
        // The 7.3 walk-through: -18 years, -1 day, set time to 23:59:59.
        let expr = MomentExpr {
            base: abs(dt(2008, 8, 4, 0, 0, 0)),
            steps: vec![
                shift(Sign::Minus, 18, Unit::Years),
                shift(Sign::Minus, 1, Unit::Days),
                Step::SetTime { hour: 23, minute: 59, second: 59 },
            ],
        };
        let out = eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)).unwrap();
        assert_eq!(out.after_each[0], dt(1990, 8, 4, 0, 0, 0));
        assert_eq!(out.after_each[1], dt(1990, 8, 3, 0, 0, 0));
        assert_eq!(out.result(), dt(1990, 8, 3, 23, 59, 59));
    }

    #[test]
    fn month_shift_clamps_to_end_of_short_month() {
        // Jan 31 + 1 month = Feb 28 (2025, non-leap) - the case a tick delta cannot express.
        let expr = MomentExpr {
            base: abs(dt(2025, 1, 31, 12, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::Months)],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)).unwrap().result(), dt(2025, 2, 28, 12, 0, 0));
    }

    #[test]
    fn month_shift_clamps_into_leap_february() {
        // Jan 31 2024 + 1 month = Feb 29 (2024 is a leap year).
        let expr = MomentExpr {
            base: abs(dt(2024, 1, 31, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::Months)],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)).unwrap().result(), dt(2024, 2, 29, 0, 0, 0));
    }

    #[test]
    fn year_shift_clamps_leap_day() {
        // Feb 29 2024 + 1 year = Feb 28 2025 (no Feb 29 in 2025).
        let expr = MomentExpr {
            base: abs(dt(2024, 2, 29, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::Years)],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)).unwrap().result(), dt(2025, 2, 28, 0, 0, 0));
    }

    #[test]
    fn quarter_is_three_months() {
        // +2 quarters from Jan 15 = +6 months = Jul 15.
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 15, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 2, Unit::Quarters)],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)).unwrap().result(), dt(2026, 7, 15, 0, 0, 0));
    }

    #[test]
    fn month_shift_backwards_across_year_boundary() {
        // Mar 15 - 5 months = Oct 15 of the previous year (rem_euclid handles the wrap).
        let expr = MomentExpr {
            base: abs(dt(2026, 3, 15, 8, 30, 0)),
            steps: vec![shift(Sign::Minus, 5, Unit::Months)],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)).unwrap().result(), dt(2025, 10, 15, 8, 30, 0));
    }

    #[test]
    fn fixed_shift_days_still_works() {
        // +90 days is a plain fixed-length shift, crossing a month boundary.
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 1, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 90, Unit::Days)],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)).unwrap().result(), dt(2026, 4, 1, 0, 0, 0));
    }

    #[test]
    fn fixed_shift_hours_rolls_the_date() {
        // +25 hours from 23:00 crosses midnight and adds a day.
        let expr = MomentExpr {
            base: abs(dt(2026, 6, 1, 23, 0, 0)),
            steps: vec![shift(Sign::Plus, 25, Unit::Hours)],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)).unwrap().result(), dt(2026, 6, 3, 0, 0, 0));
    }

    #[test]
    fn today_base_is_midnight_of_now() {
        let now = dt(2026, 8, 25, 14, 30, 45);
        let out = eval_at(&MomentExpr { base: Base::Today, steps: vec![] }, now).unwrap();
        assert_eq!(out.result(), dt(2026, 8, 25, 0, 0, 0));
    }

    #[test]
    fn now_base_is_the_full_instant() {
        let now = dt(2026, 8, 25, 14, 30, 45);
        let out = eval_at(&MomentExpr { base: Base::Now, steps: vec![] }, now).unwrap();
        assert_eq!(out.result(), now);
    }

    #[test]
    fn business_days_without_a_calendar_needs_one() {
        // No calendar in context (the substitution paths): business_days needs one.
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 1, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 5, Unit::BusinessDays)],
        };
        assert_eq!(
            eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)),
            Err(EvalError::NeedsCalendar { index: 0 })
        );
    }

    #[test]
    fn business_days_with_a_calendar_skips_weekends() {
        use crate::calendar::{Calendar, Observed};
        let cal = Calendar {
            id: "t".into(),
            country: "US".into(),
            weekend: vec![0, 6],
            observed: Observed::None,
            holidays: vec![],
        };
        // 2026-07-10 is a Friday; +1 business day is Monday 2026-07-13, time of day kept.
        let expr = MomentExpr {
            base: abs(dt(2026, 7, 10, 9, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::BusinessDays)],
        };
        let out =
            eval(&expr, &EvalContext { now: dt(2000, 1, 1, 0, 0, 0), zone_bias_min: 0, calendar: Some(&cal) })
                .unwrap();
        assert_eq!(out.result(), dt(2026, 7, 13, 9, 0, 0));
    }

    #[test]
    fn zone_step_converts_preserving_the_instant() {
        // 2026-01-15T12:00:00 in UTC+2, re-expressed in UTC+5:45 (Kathmandu's offset): 15:45
        // wall-clock, new bias, SAME instant.
        let expr = MomentExpr { base: abs(dt(2026, 1, 15, 12, 0, 0)), steps: vec![Step::Zone(-345)] };
        let out = eval_at_zone(&expr, dt(2000, 1, 1, 0, 0, 0), -120).unwrap();
        assert_eq!(out.result(), dt(2026, 1, 15, 15, 45, 0));
        assert_eq!(out.result_bias, -345);
        // Instant preserved: the UTC FILETIME of (base, base zone) equals that of (result, result zone).
        assert_eq!(
            instant_filetime(&dt(2026, 1, 15, 12, 0, 0), -120),
            instant_filetime(&out.result(), out.result_bias)
        );
    }

    #[test]
    fn zone_step_crosses_midnight_forward_and_back() {
        // 23:00 UTC re-expressed in UTC+5:45 rolls into the next day (04:45).
        let fwd = MomentExpr { base: abs(dt(2026, 1, 15, 23, 0, 0)), steps: vec![Step::Zone(-345)] };
        assert_eq!(eval_at_zone(&fwd, dt(2000, 1, 1, 0, 0, 0), 0).unwrap().result(), dt(2026, 1, 16, 4, 45, 0));
        // 02:00 UTC re-expressed in UTC-10 rolls back to the previous day (16:00).
        let back = MomentExpr { base: abs(dt(2026, 1, 15, 2, 0, 0)), steps: vec![Step::Zone(600)] };
        assert_eq!(eval_at_zone(&back, dt(2000, 1, 1, 0, 0, 0), 0).unwrap().result(), dt(2026, 1, 14, 16, 0, 0));
    }

    #[test]
    fn two_zone_steps_compose_from_the_current_zone() {
        // UTC 10:00 -> +02:00 (12:00) -> +05:45 (15:45): each step converts from the then-current zone.
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 15, 10, 0, 0)),
            steps: vec![Step::Zone(-120), Step::Zone(-345)],
        };
        let out = eval_at_zone(&expr, dt(2000, 1, 1, 0, 0, 0), 0).unwrap();
        assert_eq!(out.after_each[0], dt(2026, 1, 15, 12, 0, 0)); // in UTC+2
        assert_eq!(out.result(), dt(2026, 1, 15, 15, 45, 0)); // in UTC+5:45
        assert_eq!(out.result_bias, -345);
    }

    #[test]
    fn zone_then_shift_folds_in_the_new_zone() {
        // Convert to UTC+5:45 (15:45), then +1 day folds on THAT civil date: 16th 15:45, still +05:45.
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 15, 12, 0, 0)),
            steps: vec![Step::Zone(-345), shift(Sign::Plus, 1, Unit::Days)],
        };
        let out = eval_at_zone(&expr, dt(2000, 1, 1, 0, 0, 0), -120).unwrap();
        assert_eq!(out.result(), dt(2026, 1, 16, 15, 45, 0));
        assert_eq!(out.result_bias, -345);
    }

    #[test]
    fn without_a_zone_step_the_result_bias_is_the_session_zone() {
        // Regression guard: with no zone step the result stays in the session zone, so the render
        // is unchanged from before this slice.
        let expr = MomentExpr { base: abs(dt(2026, 1, 15, 12, 0, 0)), steps: vec![shift(Sign::Plus, 1, Unit::Days)] };
        assert_eq!(eval_at_zone(&expr, dt(2000, 1, 1, 0, 0, 0), -120).unwrap().result_bias, -120);
        let empty = MomentExpr { base: abs(dt(2026, 1, 15, 12, 0, 0)), steps: vec![] };
        assert_eq!(eval_at_zone(&empty, dt(2000, 1, 1, 0, 0, 0), 300).unwrap().result_bias, 300);
    }

    #[test]
    fn nearest_without_a_calendar_needs_one() {
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 1, 0, 0, 0)),
            steps: vec![Step::Nearest(NearestTarget::NextBusinessDay)],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)), Err(EvalError::NeedsCalendar { index: 0 }));
    }

    #[test]
    fn nearest_with_a_calendar_rolls_to_a_business_day() {
        use crate::calendar::{Calendar, Observed};
        let cal = Calendar {
            id: "t".into(),
            country: "US".into(),
            weekend: vec![0, 6],
            observed: Observed::None,
            holidays: vec![],
        };
        // 2026-07-04 is a Saturday; the next business day is Monday 2026-07-06.
        let expr = MomentExpr {
            base: abs(dt(2026, 7, 4, 0, 0, 0)),
            steps: vec![Step::Nearest(NearestTarget::NextBusinessDay)],
        };
        let out =
            eval(&expr, &EvalContext { now: dt(2000, 1, 1, 0, 0, 0), zone_bias_min: 0, calendar: Some(&cal) })
                .unwrap();
        assert_eq!(out.result(), dt(2026, 7, 6, 0, 0, 0));
    }

    #[test]
    fn nearest_leap_day_needs_no_calendar_and_finds_next_feb29() {
        let leap = |base| {
            let expr = MomentExpr { base: abs(base), steps: vec![Step::Nearest(NearestTarget::NextLeapDay)] };
            // No calendar: the leap day is pure arithmetic, so eval_at (calendar None) must succeed.
            eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)).unwrap().result()
        };
        // Common year -> the next leap day is 2028.
        assert_eq!(leap(dt(2026, 1, 15, 0, 0, 0)), dt(2028, 2, 29, 0, 0, 0));
        // Leap year, before Feb 29 -> this year's, keeping the time of day.
        assert_eq!(leap(dt(2028, 2, 10, 9, 30, 0)), dt(2028, 2, 29, 9, 30, 0));
        // On Feb 29 itself -> kept (on-or-after includes today).
        assert_eq!(leap(dt(2028, 2, 29, 0, 0, 0)), dt(2028, 2, 29, 0, 0, 0));
        // Leap year, past Feb 29 -> rolls to the next leap year, 2032.
        assert_eq!(leap(dt(2028, 3, 1, 0, 0, 0)), dt(2032, 2, 29, 0, 0, 0));
        // Century rule: 2100 is a common year, so 2097 rolls to 2104, not 2100.
        assert_eq!(leap(dt(2097, 5, 1, 0, 0, 0)), dt(2104, 2, 29, 0, 0, 0));
    }

    #[test]
    fn snap_jumps_to_period_boundaries() {
        let snap = |t| {
            let expr = MomentExpr { base: abs(dt(2026, 5, 15, 9, 30, 0)), steps: vec![Step::Snap(t)] };
            eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)).unwrap().result()
        };
        assert_eq!(snap(SnapTarget::StartOfMonth), dt(2026, 5, 1, 0, 0, 0));
        assert_eq!(snap(SnapTarget::EndOfMonth), dt(2026, 5, 31, 23, 59, 59));
        assert_eq!(snap(SnapTarget::StartOfQuarter), dt(2026, 4, 1, 0, 0, 0)); // Q2 starts in April
        assert_eq!(snap(SnapTarget::EndOfQuarter), dt(2026, 6, 30, 23, 59, 59)); // Q2 ends June 30
        assert_eq!(snap(SnapTarget::StartOfYear), dt(2026, 1, 1, 0, 0, 0));
        assert_eq!(snap(SnapTarget::EndOfYear), dt(2026, 12, 31, 23, 59, 59));
    }

    #[test]
    fn snap_end_of_february_respects_leap_years() {
        let eom = |year| {
            let expr = MomentExpr { base: abs(dt(year, 2, 10, 0, 0, 0)), steps: vec![Step::Snap(SnapTarget::EndOfMonth)] };
            eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)).unwrap().result()
        };
        assert_eq!(eom(2024), dt(2024, 2, 29, 23, 59, 59)); // leap year
        assert_eq!(eom(2025), dt(2025, 2, 28, 23, 59, 59)); // non-leap
    }

    #[test]
    fn error_reports_its_step_index() {
        // A good step then one that errors: the error points at step 1, not 0. Business days need a
        // calendar this context has none of, so step 1 is where it fails.
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 1, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::Days), shift(Sign::Plus, 5, Unit::BusinessDays)],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)), Err(EvalError::NeedsCalendar { index: 1 }));
    }

    #[test]
    fn extreme_shift_overflows_not_wraps() {
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 1, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, i64::MAX, Unit::Weeks)],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)), Err(EvalError::Overflow { index: 0 }));
    }

    #[test]
    fn extreme_year_shift_overflows_not_wraps() {
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 1, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, i64::MAX, Unit::Years)],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)), Err(EvalError::Overflow { index: 0 }));
    }

    #[test]
    fn bad_set_time_is_rejected() {
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 1, 0, 0, 0)),
            steps: vec![Step::SetTime { hour: 25, minute: 0, second: 0 }],
        };
        assert_eq!(eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)), Err(EvalError::BadSetTime { index: 0 }));
    }

    #[test]
    fn parse_civil_datetime_rejects_impossible_day() {
        // 2025 is not a leap year, so Feb 29 must be rejected, not normalized to March.
        assert!(parse_civil_datetime("2025-02-29T00:00:00").is_err());
        assert!(parse_civil_datetime("2025-04-31T00:00:00").is_err());
        // A valid leap day parses.
        assert_eq!(
            parse_civil_datetime("2024-02-29T12:00:00").unwrap(),
            dt(2024, 2, 29, 12, 0, 0)
        );
    }

    #[test]
    fn iso_formats_with_zero_padding() {
        assert_eq!(dt(2008, 8, 3, 23, 59, 59).to_iso(), "2008-08-03T23:59:59");
        assert_eq!(dt(2026, 1, 1, 0, 0, 0).to_iso(), "2026-01-01T00:00:00");
    }

    #[test]
    fn formats_out_of_filetime_range_keep_civil_drop_instant() {
        // An extreme but civilly-valid year: the civil formats still render from the fields, while
        // the instant-based ones honestly become None ("(out of range)") - never a wrapped number.
        let f = formats(&dt(40000, 1, 1, 0, 0, 0), 0);
        assert_eq!(f.iso_date, "40000-01-01");
        assert_eq!(f.us, "01/01/40000");
        assert!(f.filetime.is_none());
        assert!(f.epoch_seconds.is_none());
        assert!(f.epoch_millis.is_none());
        assert!(f.rfc1123.is_none());
    }

    #[test]
    fn significance_out_of_range_year_does_not_panic() {
        // The instant markers (epoch, 2038) are skipped when the year has no FILETIME instant;
        // the calendar-independent civil landmarks still work, with no panic.
        let s = sig(&dt(40000, 1, 1, 0, 0, 0), 0);
        assert!(s.contains(&Significance::StartOfYear));
    }

    #[test]
    fn significance_and_reading_keys_are_stable_tokens() {
        // The JSON calc contract (chronomock.calc/1) rides on these tokens, so they are pinned here
        // - a wording change to `label` must never silently change the machine key (rule 17).
        assert_eq!(Significance::EndOfQuarter.key(), "end_of_quarter");
        assert_eq!(Significance::LeapDay.key(), "leap_day");
        assert_eq!(Significance::UnixEpoch.key(), "unix_epoch");
        assert_eq!(Significance::ObservedHoliday.key(), "observed_holiday");
        assert_eq!(DateReading::Iso.key(), "iso");
        assert_eq!(DateReading::UsMonthDay.key(), "us_month_day");
        assert_eq!(DateReading::PlDayMonth.key(), "pl_day_month");
    }

    // --- step_target: the substitution jump bridge ---------------------------

    fn ft_of(local: &str, bias: i32) -> i64 {
        crate::moment_to_filetime_utc(&crate::Moment { local: local.into(), tz_bias_min: Some(bias) }).unwrap()
    }

    #[test]
    fn step_target_fixed_is_exact_and_keeps_sub_second() {
        // Fixed units add a precise tick delta - sub-second bits survive (no civil truncation),
        // exactly as the old jump_relative did.
        let ft = 1_000_000_123; // arbitrary ticks, with a sub-second remainder
        assert_eq!(step_target(ft, 0, &shift(Sign::Plus, 1, Unit::Seconds)).unwrap(), ft + 10_000_000);
        assert_eq!(step_target(ft, 0, &shift(Sign::Minus, 2, Unit::Hours)).unwrap(), ft - 2 * 3600 * 10_000_000);
    }

    #[test]
    fn step_target_calendar_folds_through_civil_with_clamp() {
        // +1 month on the fake clock clamps like the calculator: Jan 31 -> Feb 28.
        let jan31 = ft_of("2025-01-31T12:00:00", 0);
        let feb28 = ft_of("2025-02-28T12:00:00", 0);
        assert_eq!(step_target(jan31, 0, &shift(Sign::Plus, 1, Unit::Months)).unwrap(), feb28);
    }

    #[test]
    fn step_target_calendar_respects_session_zone() {
        // The fold reads and writes the civil date at the same session bias.
        let jan31 = ft_of("2025-01-31T12:00:00", -120); // UTC+2 session
        let feb28 = ft_of("2025-02-28T12:00:00", -120);
        assert_eq!(step_target(jan31, -120, &shift(Sign::Plus, 1, Unit::Months)).unwrap(), feb28);
    }

    #[test]
    fn step_target_business_days_needs_a_calendar() {
        // The jump path has no calendar, so a business-day step needs one it does not have.
        let ft = ft_of("2026-01-01T00:00:00", 0);
        assert_eq!(
            step_target(ft, 0, &shift(Sign::Plus, 5, Unit::BusinessDays)),
            Err(EvalError::NeedsCalendar { index: 0 })
        );
    }

    #[test]
    fn step_target_zone_is_unsupported_on_the_jump_path() {
        // The substitution jump advances the fake clock by a time delta - it cannot change zone.
        // `eval` builds the `zone` step; `jump` never does, so the jump path reports it unsupported.
        let ft = ft_of("2026-01-01T00:00:00", 0);
        assert_eq!(
            step_target(ft, 0, &Step::Zone(-345)),
            Err(EvalError::StepUnsupported { kind: "zone", index: 0 })
        );
    }

    #[test]
    fn step_target_overflow_is_reported_not_wrapped() {
        let ft = ft_of("2026-01-01T00:00:00", 0);
        assert!(step_target(ft, 0, &shift(Sign::Plus, i64::MAX, Unit::Years)).is_err());
        assert!(step_target(ft, 0, &shift(Sign::Plus, i64::MAX, Unit::Weeks)).is_err());
    }

    #[test]
    fn filetime_to_civil_inverts_moment_to_filetime() {
        for (local, bias) in
            [("2038-01-19T03:14:07", 0), ("2025-02-28T12:00:00", -120), ("1990-08-03T23:59:59", 300)]
        {
            assert_eq!(filetime_to_civil(ft_of(local, bias), bias).to_iso(), local);
        }
    }

    // --- output formats ------------------------------------------------------

    #[test]
    fn formats_unix_epoch_is_all_zeros_and_thursday() {
        let f = formats(&dt(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(f.iso_date, "1970-01-01");
        assert_eq!(f.iso_datetime, "1970-01-01T00:00:00+00:00");
        assert_eq!(f.us, "01/01/1970");
        assert_eq!(f.pl, "01.01.1970");
        assert_eq!(f.epoch_seconds, Some(0));
        assert_eq!(f.epoch_millis, Some(0));
        assert_eq!(f.filetime, Some(116_444_736_000_000_000));
        assert_eq!(f.rfc1123.as_deref(), Some("Thu, 01 Jan 1970 00:00:00 GMT"));
    }

    #[test]
    fn formats_2038_boundary_epoch_is_i32_max() {
        let f = formats(&dt(2038, 1, 19, 3, 14, 7), 0);
        assert_eq!(f.epoch_seconds, Some(2_147_483_647)); // the classic 32-bit time_t boundary
    }

    #[test]
    fn formats_respect_session_zone_offset() {
        // 12:00 local in UTC+2 is 10:00 UTC: the offset shows in ISO, and epoch is the UTC instant.
        let f = formats(&dt(2026, 1, 15, 12, 0, 0), -120);
        assert_eq!(f.iso_datetime, "2026-01-15T12:00:00+02:00");
        assert_eq!(f.us, "01/15/2026");
        assert_eq!(f.pl, "15.01.2026");
        assert_eq!(f.epoch_seconds, formats(&dt(2026, 1, 15, 10, 0, 0), 0).epoch_seconds);
    }

    #[test]
    fn formats_day_of_week_name_is_correct() {
        // 2000-01-01 was a Saturday - the RFC 1123 day name must match.
        assert_eq!(
            formats(&dt(2000, 1, 1, 0, 0, 0), 0).rfc1123.as_deref(),
            Some("Sat, 01 Jan 2000 00:00:00 GMT")
        );
    }

    // --- metadata ------------------------------------------------------------

    #[test]
    fn metadata_iso_week_handles_year_boundaries() {
        let t = dt(2000, 1, 1, 0, 0, 0);
        let iso = |y, m, d| {
            let x = metadata(&dt(y, m, d, 0, 0, 0), &t);
            (x.iso_week_year, x.iso_week)
        };
        assert_eq!(iso(2026, 1, 1), (2026, 1)); // Thursday -> W01
        assert_eq!(iso(2005, 1, 1), (2004, 53)); // belongs to the previous ISO year
        assert_eq!(iso(2019, 12, 30), (2020, 1)); // belongs to the next ISO year
        assert_eq!(iso(2020, 12, 31), (2020, 53)); // a long ISO year
    }

    #[test]
    fn metadata_us_week_starts_sunday_and_week1_has_jan1() {
        let t = dt(2000, 1, 1, 0, 0, 0);
        assert_eq!(metadata(&dt(2026, 1, 1, 0, 0, 0), &t).us_week, 1); // Thu Jan 1 -> week 1
        assert_eq!(metadata(&dt(2026, 1, 3, 0, 0, 0), &t).us_week, 1); // Sat, still week 1
        assert_eq!(metadata(&dt(2026, 1, 4, 0, 0, 0), &t).us_week, 2); // Sunday starts week 2
    }

    #[test]
    fn metadata_quarter_day_of_year_leap_and_weekday() {
        let t = dt(2000, 1, 1, 0, 0, 0);
        let m = metadata(&dt(2024, 2, 29, 0, 0, 0), &t);
        assert_eq!(m.weekday, "Thursday"); // 2024-02-29 was a Thursday
        assert_eq!(m.quarter, 1);
        assert_eq!(m.day_of_year, 60); // 31 (Jan) + 29
        assert!(m.is_leap_year);
        assert_eq!(metadata(&dt(2024, 12, 31, 0, 0, 0), &t).day_of_year, 366);
        assert_eq!(metadata(&dt(2025, 12, 31, 0, 0, 0), &t).day_of_year, 365);
        assert_eq!(metadata(&dt(2025, 7, 1, 0, 0, 0), &t).quarter, 3);
        assert!(!metadata(&dt(1900, 6, 1, 0, 0, 0), &t).is_leap_year); // century, not a leap year
        assert!(metadata(&dt(2000, 6, 1, 0, 0, 0), &t).is_leap_year); // divisible by 400
    }

    #[test]
    fn metadata_days_from_today_is_signed_date_difference() {
        let today = dt(2026, 1, 1, 0, 0, 0);
        assert_eq!(metadata(&dt(2026, 1, 10, 23, 0, 0), &today).days_from_today, 9); // time ignored
        assert_eq!(metadata(&dt(2025, 12, 30, 0, 0, 0), &today).days_from_today, -2);
        assert_eq!(metadata(&today, &today).days_from_today, 0);
    }

    // --- significance ("what this date tests") -------------------------------

    #[test]
    fn significance_february_leap_and_common_year() {
        // Feb 29 is the leap day - and NOT also reported as a generic month-end.
        assert_eq!(sig(&dt(2024, 2, 29, 0, 0, 0), 0), vec![Significance::LeapDay]);
        // Feb 28 of a non-leap year is the last day of February with no 29th - its own landmark.
        assert_eq!(
            sig(&dt(2025, 2, 28, 0, 0, 0), 0),
            vec![Significance::LastDayOfFebruaryCommonYear]
        );
        // Feb 28 of a LEAP year is an ordinary day (the 29th is the month's last).
        assert!(sig(&dt(2024, 2, 28, 0, 0, 0), 0).is_empty());
    }

    #[test]
    fn significance_period_boundary_collapses_to_the_strongest() {
        // Dec 31 is month-, quarter- and year-end at once: only the strongest (year) is reported.
        assert_eq!(sig(&dt(2027, 12, 31, 0, 0, 0), 0), vec![Significance::EndOfYear]);
        // Sep 30 is month- and quarter-end: quarter wins over month.
        assert_eq!(sig(&dt(2027, 9, 30, 0, 0, 0), 0), vec![Significance::EndOfQuarter]);
        // Jan 31 is only a month-end (January is not a quarter-end month).
        assert_eq!(sig(&dt(2027, 1, 31, 0, 0, 0), 0), vec![Significance::EndOfMonth]);
        // Starts mirror the ends: year, quarter, month.
        assert_eq!(sig(&dt(2027, 1, 1, 0, 0, 0), 0), vec![Significance::StartOfYear]);
        assert_eq!(sig(&dt(2027, 4, 1, 0, 0, 0), 0), vec![Significance::StartOfQuarter]);
        assert_eq!(sig(&dt(2027, 2, 1, 0, 0, 0), 0), vec![Significance::StartOfMonth]);
    }

    #[test]
    fn significance_unix_epoch_is_instant_zero_and_zone_aware() {
        // 1970-01-01 in UTC is both the year start and epoch 0 - two independent axes.
        assert_eq!(
            sig(&dt(1970, 1, 1, 0, 0, 0), 0),
            vec![Significance::StartOfYear, Significance::UnixEpoch]
        );
        // In UTC+1 (bias -60), local midnight 1970-01-01 is 1969-12-31T23:00Z - epoch is -3600, so
        // only the (zone-independent) year-start marker fires, not the epoch one.
        assert_eq!(sig(&dt(1970, 1, 1, 0, 0, 0), -60), vec![Significance::StartOfYear]);
    }

    #[test]
    fn significance_2038_boundary_fires_at_or_past_i32_max() {
        // Exactly the signed 32-bit time_t limit: 2038-01-19T03:14:07Z = 2_147_483_647 seconds.
        assert!(sig(&dt(2038, 1, 19, 3, 14, 7), 0).contains(&Significance::Year2038Boundary));
        // One second earlier: not yet at the limit.
        assert!(!sig(&dt(2038, 1, 19, 3, 14, 6), 0).contains(&Significance::Year2038Boundary));
        // Well past it.
        assert!(sig(&dt(2050, 6, 1, 0, 0, 0), 0).contains(&Significance::Year2038Boundary));
    }

    #[test]
    fn significance_plain_and_pre_epoch_dates_are_honest() {
        // A mid-month weekday hits nothing notable - an empty list, not a guessed landmark.
        assert!(sig(&dt(2026, 8, 12, 9, 0, 0), 0).is_empty());
        // A pre-epoch mid-month date: the instant is negative, so no epoch/2038 marker, and it is
        // not a boundary either.
        assert!(sig(&dt(1900, 6, 15, 0, 0, 0), 0).is_empty());
        // A civil boundary in the deep past still fires its civil marker regardless of the instant.
        assert_eq!(sig(&dt(1000, 12, 31, 0, 0, 0), 0), vec![Significance::EndOfYear]);
    }

    /// A small US-shaped calendar (New Year, Independence Day) with a configurable observance, for
    /// the calendar-dependent significance tests.
    fn cal_us(observed: crate::calendar::Observed) -> crate::calendar::Calendar {
        use crate::calendar::{Calendar, Holiday, HolidayRule};
        let h = |id: &str, month, day| Holiday {
            id: id.into(),
            name_en: id.into(),
            name_local: id.into(),
            rule: HolidayRule::Fixed { month, day },
            valid_from: None,
            valid_to: None,
            source: "test".into(),
        };
        Calendar {
            id: "us-test".into(),
            country: "US".into(),
            weekend: vec![0, 6],
            observed,
            holidays: vec![h("new_year", 1, 1), h("independence", 7, 4)],
        }
    }

    #[test]
    fn significance_calendar_markers_weekend_holiday_and_observed() {
        let federal = cal_us(crate::calendar::Observed::SatToFriSunToMon);
        // 2026-07-04 is a Saturday AND Independence Day: both markers, weekend first.
        assert_eq!(
            significance(&dt(2026, 7, 4, 0, 0, 0), 0, Some(&federal)),
            vec![Significance::Weekend, Significance::PublicHoliday]
        );
        // 2026-07-03 is a Friday, a day off only because July 4 (Saturday) is OBSERVED here - the
        // marker that explains a "not a business day, yet not a holiday" date.
        assert_eq!(
            significance(&dt(2026, 7, 3, 0, 0, 0), 0, Some(&federal)),
            vec![Significance::ObservedHoliday]
        );
        // An ordinary Saturday (2026-07-11) is just a weekend; an ordinary weekday hits nothing.
        assert_eq!(significance(&dt(2026, 7, 11, 0, 0, 0), 0, Some(&federal)), vec![Significance::Weekend]);
        assert!(significance(&dt(2026, 7, 7, 0, 0, 0), 0, Some(&federal)).is_empty());
    }

    #[test]
    fn significance_mixes_civil_and_calendar_markers() {
        let federal = cal_us(crate::calendar::Observed::SatToFriSunToMon);
        // 2026-01-01 is the year start (civil) AND New Year, a weekday holiday (calendar).
        assert_eq!(
            significance(&dt(2026, 1, 1, 0, 0, 0), 0, Some(&federal)),
            vec![Significance::StartOfYear, Significance::PublicHoliday]
        );
    }

    #[test]
    fn significance_observed_holiday_needs_a_shifting_calendar() {
        // Under a no-shift calendar (Poland-style), a weekend holiday does not move, so the Friday
        // before is an ordinary business day - no observed-holiday marker.
        let none = cal_us(crate::calendar::Observed::None);
        assert!(significance(&dt(2026, 7, 3, 0, 0, 0), 0, Some(&none)).is_empty());
        // The Saturday itself is still a weekend holiday.
        assert_eq!(
            significance(&dt(2026, 7, 4, 0, 0, 0), 0, Some(&none)),
            vec![Significance::Weekend, Significance::PublicHoliday]
        );
    }

    // --- reverse analysis ----------------------------------------------------

    #[test]
    fn analyze_iso_is_a_single_reading() {
        let a = analyze_date("2008-08-04").unwrap();
        assert!(!a.is_ambiguous());
        assert_eq!(a.readings, vec![(DateReading::Iso, dt(2008, 8, 4, 0, 0, 0))]);
        // A time component is kept.
        assert_eq!(analyze_date("1990-08-03T23:59:59").unwrap().readings[0].1, dt(1990, 8, 3, 23, 59, 59));
    }

    #[test]
    fn analyze_ambiguous_numeric_shows_both_us_and_pl() {
        // 04/08/2008: April 8 (US) and August 4 (PL) - both valid, both shown, US first.
        let a = analyze_date("04/08/2008").unwrap();
        assert!(a.is_ambiguous());
        assert_eq!(
            a.readings,
            vec![
                (DateReading::UsMonthDay, dt(2008, 4, 8, 0, 0, 0)),
                (DateReading::PlDayMonth, dt(2008, 8, 4, 0, 0, 0)),
            ]
        );
        // Dots read the same way.
        assert!(analyze_date("04.08.2008").unwrap().is_ambiguous());
    }

    #[test]
    fn analyze_resolves_when_only_one_order_is_valid() {
        // 29/02/2024: "month 29" is impossible, so only the PL reading (Feb 29, a leap day).
        assert_eq!(
            analyze_date("29/02/2024").unwrap().readings,
            vec![(DateReading::PlDayMonth, dt(2024, 2, 29, 0, 0, 0))]
        );
        // 02/29/2024: the mirror - US only.
        assert_eq!(
            analyze_date("02/29/2024").unwrap().readings,
            vec![(DateReading::UsMonthDay, dt(2024, 2, 29, 0, 0, 0))]
        );
        // 25/12/2008: "month 25" impossible - PL only.
        assert_eq!(
            analyze_date("25/12/2008").unwrap().readings,
            vec![(DateReading::PlDayMonth, dt(2008, 12, 25, 0, 0, 0))]
        );
    }

    #[test]
    fn analyze_same_date_both_orders_is_not_ambiguous() {
        // 05/05/2008 reads the same either way - one reading, not two.
        let a = analyze_date("05/05/2008").unwrap();
        assert!(!a.is_ambiguous());
        assert_eq!(a.readings, vec![(DateReading::UsMonthDay, dt(2008, 5, 5, 0, 0, 0))]);
    }

    #[test]
    fn analyze_rejects_the_unrepresentable_and_unrecognised() {
        assert!(analyze_date("02/30/2008").is_err()); // Feb 30 in neither order
        assert!(analyze_date("13/13/2008").is_err()); // month 13 in neither order
        assert!(analyze_date("").is_err());
        assert!(analyze_date("hello").is_err());
        assert!(analyze_date("04/08/08").is_err()); // year not four digits
        assert!(analyze_date("2008/08/04").is_err()); // year-first numeric not recognised yet
    }

    // --- custom format mask --------------------------------------------------

    #[test]
    fn format_mask_common_patterns() {
        let d = dt(2008, 8, 4, 23, 59, 9);
        assert_eq!(format_with_mask(&d, "yyyy-MM-dd HH:mm:ss"), "2008-08-04 23:59:09");
        assert_eq!(format_with_mask(&d, "MM/dd/yyyy"), "08/04/2008");
        assert_eq!(format_with_mask(&d, "dd.MM.yyyy"), "04.08.2008");
        assert_eq!(format_with_mask(&d, "yy"), "08");
    }

    #[test]
    fn format_mask_is_case_sensitive_month_vs_minute() {
        // M is month, m is minute - the classic gotcha.
        let d = dt(2008, 3, 4, 12, 7, 0);
        assert_eq!(format_with_mask(&d, "M m"), "3 7"); // month 3, minute 7 (no pad)
        assert_eq!(format_with_mask(&d, "MM mm"), "03 07");
    }

    #[test]
    fn format_mask_names_and_no_pad() {
        let d = dt(2008, 8, 4, 5, 6, 7); // 2008-08-04 was a Monday
        assert_eq!(format_with_mask(&d, "ddd, dd MMM yyyy"), "Mon, 04 Aug 2008");
        assert_eq!(format_with_mask(&d, "dddd MMMM"), "Monday August");
        assert_eq!(format_with_mask(&d, "d/M/yyyy H:m:s"), "4/8/2008 5:6:7"); // single = no pad
    }

    #[test]
    fn format_mask_literals_and_unknown_runs_pass_through() {
        let d = dt(2008, 8, 4, 0, 0, 0);
        // A single 'y' and non-token characters are literal; a 3-run 'yyy' is not a token, so verbatim.
        assert_eq!(format_with_mask(&d, "year=yyyy"), "year=2008");
        assert_eq!(format_with_mask(&d, "yyy"), "yyy");
        assert_eq!(format_with_mask(&d, "yyyy//MM"), "2008//08");
        assert_eq!(format_with_mask(&d, ""), "");
    }

    #[test]
    fn format_mask_single_token_letters_are_still_tokens() {
        // A documented limitation: a lone token letter is a token even inside a would-be word, so a
        // literal 's' (seconds) cannot appear without escaping (not built). Deliberate, not a bug.
        assert_eq!(format_with_mask(&dt(2008, 8, 4, 0, 0, 30), "s"), "30");
    }
}
