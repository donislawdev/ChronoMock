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

use super::{civil_from_days, days_from_civil, parse_civil};

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

/// One step of a moment expression. Step kinds mirror docs/04 section 4.3 exactly
/// so a preset `moment` and the calculator share one model. `Snap`, `Nearest`, and
/// `Zone` carry their raw target token for now - they are not built in this slice
/// and evaluate to an honest "not supported yet".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Shift { sign: Sign, amount: i64, unit: Unit },
    SetTime { hour: u32, minute: u32, second: u32 },
    Snap(String),
    Nearest(String),
    Zone(String),
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

/// What the evaluator needs from the outside: "real now" as data, so the core
/// stays pure and a test can pin it (docs/07 section 4).
#[derive(Debug, Clone, Copy)]
pub struct EvalContext {
    pub now: CivilDateTime,
}

/// The result of evaluating an expression: the base and the intermediate value
/// after each step, so the surface can show progress step by step (7.3 - the user
/// sees where they went wrong, not just the final number).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalOutcome {
    pub base: CivilDateTime,
    pub after_each: Vec<CivilDateTime>,
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
    /// A step kind that exists in the model but is not built in this release.
    StepUnsupported { kind: &'static str, index: usize },
    /// A shift unit that is not built in this release (business days need a calendar).
    UnitUnsupported { unit: &'static str, index: usize },
    /// The shift overflowed the representable range - rejected, not wrapped.
    Overflow { index: usize },
    /// `set_time` fields out of range (hour > 23, minute/second > 59).
    BadSetTime { index: usize },
}

/// Parse a civil date-time in "YYYY-MM-DDTHH:MM:SS" form (a space may replace the
/// `T`). Reuses the core's shape/range check and additionally rejects a day that
/// cannot exist in its month (e.g. 2025-02-31), so a bad base is an honest error
/// rather than a silent normalization to March.
pub fn parse_civil_datetime(s: &str) -> Result<CivilDateTime, String> {
    let (y, mo, d, h, mi, sec) = parse_civil(s)?;
    let last = last_day_of_month(y, mo) as i64;
    if d > last {
        return Err(format!("day {d} out of range for month {mo} in '{s}'"));
    }
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
    let mut after_each = Vec::with_capacity(expr.steps.len());
    for (i, step) in expr.steps.iter().enumerate() {
        cur = apply_step(cur, step, i)?;
        after_each.push(cur);
    }
    Ok(EvalOutcome { base, after_each })
}

fn apply_step(cur: CivilDateTime, step: &Step, index: usize) -> Result<CivilDateTime, EvalError> {
    match step {
        Step::Shift { sign, amount, unit } => apply_shift(cur, *sign, *amount, *unit, index),
        Step::SetTime { hour, minute, second } => {
            if *hour > 23 || *minute > 59 || *second > 59 {
                return Err(EvalError::BadSetTime { index });
            }
            Ok(CivilDateTime { hour: *hour, minute: *minute, second: *second, ..cur })
        }
        // Not built yet - honest "not supported" rather than a silent skip.
        Step::Snap(_) | Step::Nearest(_) | Step::Zone(_) => {
            Err(EvalError::StepUnsupported { kind: step.kind(), index })
        }
    }
}

fn apply_shift(
    cur: CivilDateTime,
    sign: Sign,
    amount: i64,
    unit: Unit,
    index: usize,
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
        Unit::BusinessDays => Err(EvalError::UnitUnsupported { unit: unit.name(), index }),
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

/// Proleptic Gregorian leap-year test.
fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Last valid day of a month (1..=12), 28/29/30/31.
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
        _ => 0, // parse_civil already rejects a month outside 1..=12 upstream.
    }
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

    /// Evaluate against a fixed `now` so `today`/`now` bases are deterministic.
    fn eval_at(expr: &MomentExpr, now: CivilDateTime) -> Result<EvalOutcome, EvalError> {
        eval(expr, &EvalContext { now })
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
    fn business_days_unit_is_not_built_yet() {
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 1, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 5, Unit::BusinessDays)],
        };
        assert_eq!(
            eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)),
            Err(EvalError::UnitUnsupported { unit: "business_days", index: 0 })
        );
    }

    #[test]
    fn snap_nearest_zone_steps_are_not_built_yet() {
        for (step, kind) in [
            (Step::Snap("end-of-quarter".into()), "snap"),
            (Step::Nearest("next-business-day".into()), "nearest"),
            (Step::Zone("+05:45".into()), "zone"),
        ] {
            let expr = MomentExpr { base: abs(dt(2026, 1, 1, 0, 0, 0)), steps: vec![step] };
            assert_eq!(
                eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)),
                Err(EvalError::StepUnsupported { kind, index: 0 })
            );
        }
    }

    #[test]
    fn unsupported_step_reports_its_index() {
        // A good step then an unbuilt one: the error points at step 1, not 0.
        let expr = MomentExpr {
            base: abs(dt(2026, 1, 1, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::Days), Step::Snap("eoy".into())],
        };
        assert_eq!(
            eval_at(&expr, dt(2000, 1, 1, 0, 0, 0)),
            Err(EvalError::StepUnsupported { kind: "snap", index: 1 })
        );
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
}
