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
//! Stage-4 slice 1 builds `shift` (fixed and calendar units) and `set_time` -
//! `snap`, `nearest`, `zone`, and the `business_days` unit are present in the
//! model so the surface is complete, and return the product's honest "not built
//! yet" vocabulary rather than a silent skip or a faked result (zasady/01 section 2).

use super::{
    civil_from_days, days_from_civil, is_leap, last_day_of_month, CIVIL_YEAR_MAX,
    CIVIL_YEAR_MIN,
};

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

/// A year as text: at least four digits, and for a year before 1 CE the sign goes in FRONT of that
/// field rather than being folded into its width.
///
/// One function because the year used to be formatted at six different call sites with `{:04}`, and
/// `{:04}` on -9 gives "-009" - a string the parser cannot read back. That was half of the tool
/// printing a date it would then refuse as input (R3-6) - the parser learning the leading minus is
/// the other half. Fixing `to_iso` alone left `formats()` still printing the short form, which is
/// exactly the kind of split a shared helper prevents.
fn format_year(year: i64) -> String {
    if year < 0 { format!("-{:04}", -year) } else { format!("{year:04}") }
}

impl CivilDateTime {
    /// Format as "YYYY-MM-DDTHH:MM:SS" - the ISO shape the rest of the tool speaks.
    pub fn to_iso(&self) -> String {
        format!(
            "{}-{:02}-{:02}T{:02}:{:02}:{:02}",
            format_year(self.year),
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second
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

/// A shift unit. Fixed-length units are a constant number of seconds - calendar
/// units depend on the anchor date and are folded through civil arithmetic.
/// `BusinessDays` needs a calendar: the calculator counts them with one, and a session, which has
/// none, refuses them.
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

    /// Whether this step asks the calendar about days: a business-day shift, or `nearest` to a
    /// business day. The leap-day target is arithmetic and asks it nothing.
    fn judges_business_days(&self) -> bool {
        match self {
            Step::Shift { unit, .. } => *unit == Unit::BusinessDays,
            Step::Nearest(target) => {
                matches!(target, NearestTarget::NextBusinessDay | NearestTarget::PrevBusinessDay)
            }
            Step::SetTime { .. } | Step::Snap(_) | Step::Zone(_) => false,
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
    /// An explicit civil date and time, read in the SESSION zone (rule 2).
    Absolute(CivilDateTime),
    /// An explicit instant, written as a civil date and time in UTC and re-expressed in the session
    /// zone before any step runs. For a moment whose whole meaning is an instant - the Unix epoch,
    /// the signed 32-bit `time_t` limit - and which therefore must not move with the tester's zone.
    /// `Absolute` is the right base for everything else: "9 AM on the 3rd" means 9 AM where the
    /// tester is, and expressing THAT as UTC would be the same bug in the other direction.
    AbsoluteUtc(CivilDateTime),
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
/// honestly unsupported - `zone_bias_min` is the base zone there too (those paths build no `zone`
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
    /// Steps whose day was CLAMPED to the end of a shorter month, as (step index, the day asked
    /// for). `31 January + 1 month` is 28 or 29 February, and `29 February + 1 year` is 28 February.
    /// Every one of those is correct and documented, and every one is invisible in the result, which
    /// is the problem: a reader who sees only "29 became 28" cannot tell a rule from a defect, on a
    /// date they are about to act on. Reported by the engine rather than inferred by each caller from
    /// the day changing, so the two halves of the product cannot disagree about one shift.
    pub clamped: Vec<(usize, u32)>,
    /// The latest day a business-day step or a business-day `nearest` judged against the calendar, or
    /// `None` when no step used it. With the result, which the metadata judges, it is everything the
    /// calendar decided in this evaluation - what [`crate::calendar::outdated`] is asked about. For a
    /// walk backwards it is the day the walk started from, one day later than the first day it judged,
    /// so the answer errs towards saying the data may be old.
    pub calendar_reach: Option<CivilDateTime>,
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
    /// A calendar step found no business day: the file marks (almost) every day as one off. Its own
    /// variant because it used to travel two different ways for one cause - `nearest` reported
    /// `NotFound`, which the CLI renders as exit 5 "not built in this release" (the operation IS
    /// built, the FILE is wrong), and `+Nbd` reported `Overflow`, a message about number sizes
    /// (R2-S10). Calendars are the one part of this tool outsiders write, so the diagnosis has to
    /// point at the file.
    DegenerateCalendar { index: usize },
    /// The shift overflowed the representable range - rejected, not wrapped.
    Overflow { index: usize },
    /// `set_time` fields out of range (hour > 23, minute/second > 59).
    BadSetTime { index: usize },
    /// The base moment's year is outside the band this build computes on
    /// ([`crate::CIVIL_YEAR_MIN`]..=[`crate::CIVIL_YEAR_MAX`]). Everything that reaches `eval`
    /// through the CLI has been through `parse_civil` and cannot hit this - `eval` is public API, so
    /// it checks rather than trusting its caller with a value that would panic the civil math.
    BaseYearOutOfRange,
    /// A UTC base could not be re-expressed in the session zone: the instant it names is outside
    /// the representable range. Its own variant rather than `Overflow { index: 0 }`, which would
    /// name step 1 - a step the expression may not even have, sending the reader to look at the
    /// wrong thing. Only `AbsoluteUtc` can produce it, because it is the only base that converts.
    BaseOverflow,
    /// The base is not a civil date at all: a month outside 1..=12, or a day that month cannot have.
    ///
    /// `CivilDateTime` has public fields and `eval` is public API, so a caller can hand over fields no
    /// parser ever produced. The year was checked and the rest was not, which left the month running
    /// through arithmetic written for 1..=12: `(month - 1)` underflows on 0 in `apply_snap`, `metadata`
    /// and `mask_token`, and `MONTH_ABBR[month - 1]` indexes past its end above 12. Unreachable through
    /// every surface this build ships - all of them go through `parse_civil` - so this is about the API
    /// keeping the promise its own doc comment makes, on a horizon where a second consumer is likely
    /// (rule 28).
    BaseNotACivilDate,
    /// A step COMPUTED a year outside that band. Its own variant, and not folded into `Overflow`,
    /// because nothing overflowed: `+300000y` from 2026 is an exact, representable number that this
    /// build simply will not compute a calendar on, and saying "overflow" would point at the wrong
    /// thing. A step is the second door into the year band - it builds its year arithmetically and
    /// never goes back through the parser (R3-1).
    YearOutOfRange { index: usize },
    /// The business-day steps of one evaluation, up to this one, add up to more than
    /// [`crate::calendar::MAX_BUSINESS_DAYS`] (one step alone can, too). A limit this build sets, not an
    /// arithmetic one: it used to travel as `Overflow`, so `+2000000bd` was answered with a sentence
    /// about the size of numbers while the number was fine (R4-N35). Counted over the whole evaluation
    /// since R5-B2, see `BusinessDayBudget`.
    TooManyBusinessDays { index: usize },
    /// A business-day step reached a day before the calendar's first year
    /// ([`crate::calendar::Calendar::valid_from`], carried here as `first_year`), where the calendar
    /// has no rules. Counted on with today's holidays and weekend, it gave answers like a Polish
    /// Constitution Day off in 1985, five years before that day off was restored (R4-S20).
    BeforeCalendar { index: usize, first_year: i64 },
}

/// Parse a civil date-time in "YYYY-MM-DDTHH:MM:SS" form (a space may replace the `T`). The core's
/// `parse_civil` already rejects a day that cannot exist in its month (e.g. 2025-02-31), so this
/// maps the validated fields to `CivilDateTime` - one validation path shared with the substitution
/// moment, no silent normalization to March.
pub fn parse_civil_datetime(s: &str) -> Result<CivilDateTime, String> {
    parse_civil_datetime_as(s, s)
}

/// [`parse_civil_datetime`], quoting `shown` in its refusals (see `crate::parse_civil_as`).
fn parse_civil_datetime_as(s: &str, shown: &str) -> Result<CivilDateTime, String> {
    let (y, mo, d, h, mi, sec) = super::parse_civil_as(s, shown)?;
    Ok(CivilDateTime {
        year: y,
        month: mo as u32,
        day: d as u32,
        hour: h as u32,
        minute: mi as u32,
        second: sec as u32,
    })
}

/// Whether these civil fields name a moment that exists: a month in 1..=12, a day within that month's
/// length in that year, and a time of day within 00:00:00..=23:59:59.
///
/// The time fields are checked here too, because a BASE is the one place they arrive from outside:
/// `eval` is public API over a struct with public fields, and an hour of 99 went straight through
/// into every rendered format (R4-N42). The steps build their time fields from arithmetic that cannot
/// leave the range, and `set_time` has its own refusal.
fn is_a_civil_moment(c: &CivilDateTime) -> bool {
    (1..=12).contains(&c.month)
        && c.day >= 1
        && c.day <= last_day_of_month(c.year, c.month as i64)
        && c.hour <= 23
        && c.minute <= 59
        && c.second <= 59
}

/// The business days one evaluation has asked a calendar to count so far, against
/// [`crate::calendar::MAX_BUSINESS_DAYS`] for the WHOLE evaluation (R5-B2).
///
/// The limit used to stand on each step alone, so a preset of two hundred steps of `+1000000bd` stayed
/// under it two hundred times and held the CPU for 5.7 s on a shipped calendar, measured, and a calendar
/// of 2 000 holidays multiplied that. A step count limit on presets does not close it either: the
/// command line repeats `--shift` as often as it fits. One budget for the evaluation bounds the walk
/// whatever the surface, at the step that crosses it, before that step walks.
#[derive(Default)]
struct BusinessDayBudget {
    spent: u64,
}

impl BusinessDayBudget {
    /// Count `step` against the budget, refusing it when it takes the evaluation past the limit. Only
    /// with a calendar: without one a business-day step is refused as needing it, which says more.
    fn spend(&mut self, step: &Step, index: usize, has_calendar: bool) -> Result<(), EvalError> {
        if let Step::Shift { amount, unit: Unit::BusinessDays, .. } = step
            && has_calendar
        {
            self.spent = self.spent.saturating_add(amount.unsigned_abs());
            if self.spent > crate::calendar::MAX_BUSINESS_DAYS.unsigned_abs() {
                return Err(EvalError::TooManyBusinessDays { index });
            }
        }
        Ok(())
    }
}

/// Evaluate an expression against a context. Folds each step onto the running civil
/// value left to right, recording the value after each step.
pub fn eval(expr: &MomentExpr, ctx: &EvalContext) -> Result<EvalOutcome, EvalError> {
    let base = match &expr.base {
        Base::Today => ctx.now.at_midnight(),
        Base::Now => ctx.now,
        Base::Absolute(c) => *c,
        // Read in UTC, handed on in the session zone, so every step and every rendered format
        // downstream works on the session zone exactly as it does for every other base - the whole
        // expression stays in one zone, and only the ENTRY point differs. The same instant
        // arithmetic a `zone` step uses, so a UTC base introduces no second way to cross zones.
        Base::AbsoluteUtc(c) => {
            if !crate::civil_year_in_band(c.year) {
                return Err(EvalError::BaseYearOutOfRange);
            }
            // Before the conversion, not only after it: the conversion is arithmetic on the fields,
            // so month 13 came out of it as January of the next year and the check below saw a real
            // date (R4-N42).
            if !is_a_civil_moment(c) {
                return Err(EvalError::BaseNotACivilDate);
            }
            convert_zone(*c, 0, ctx.zone_bias_min, 0).map_err(|_| EvalError::BaseOverflow)?
        }
    };
    if !crate::civil_year_in_band(base.year) {
        return Err(EvalError::BaseYearOutOfRange);
    }
    // The other two civil fields, for the same reason the year is checked: this is public API over a
    // struct with public fields, so "the caller already validated it" is an assumption rather than a
    // fact. Below here the month indexes month-name tables and feeds `(month - 1)` arithmetic that has
    // no answer for 0.
    if !is_a_civil_moment(&base) {
        return Err(EvalError::BaseNotACivilDate);
    }
    let mut cur = base;
    let mut cur_bias = ctx.zone_bias_min;
    let mut after_each = Vec::with_capacity(expr.steps.len());
    let mut clamped: Vec<(usize, u32)> = Vec::new();
    let mut calendar_reach: Option<CivilDateTime> = None;
    let mut business_days = BusinessDayBudget::default();
    for (i, step) in expr.steps.iter().enumerate() {
        business_days.spend(step, i, ctx.calendar.is_some())?;
        // A `zone` step re-expresses the running moment in another fixed-offset zone (same
        // instant, new wall-clock and bias). It is the only step that changes the zone, so it is
        // handled here where the bias lives - every other step folds the civil date and keeps the
        // current zone. The substitution jump path never builds a `zone` step, so `apply_step`
        // still reports it as unsupported there.
        if let Step::Zone(target_bias) = step {
            cur = convert_zone(cur, cur_bias, *target_bias, i)?;
            cur_bias = *target_bias;
        } else {
            let (next, clamped_day) = apply_step(cur, step, i, ctx.calendar)?;
            if let Some(day) = clamped_day {
                clamped.push((i, day));
            }
            if step.judges_business_days() {
                // The days a calendar step judged lie between where it started and where it landed.
                let latest = later(cur, next);
                calendar_reach = Some(calendar_reach.map_or(latest, |reach| later(reach, latest)));
            }
            cur = next;
        }
        // Checked after EVERY step, not only after the ones that obviously move years. A snap keeps
        // the year, a business-day walk can cross one, a zone step can cross one at the boundary,
        // and `nearest next-leap-day` walks forward until it finds a leap year - one check here
        // covers all of them, and covers the next step kind for free (rule 6: one place, not seven).
        if !crate::civil_year_in_band(cur.year) {
            return Err(EvalError::YearOutOfRange { index: i });
        }
        after_each.push(cur);
    }
    Ok(EvalOutcome { base, after_each, result_bias: cur_bias, clamped, calendar_reach })
}

/// The later of two civil moments of the same zone (field order is date order).
fn later(a: CivilDateTime, b: CivilDateTime) -> CivilDateTime {
    let key = |c: &CivilDateTime| (c.year, c.month, c.day, c.hour, c.minute, c.second);
    if key(&b) > key(&a) { b } else { a }
}

/// Whether a `set_time` step names a time of day that exists: 00:00:00 to 23:59:59.
///
/// Public because a step is refused for it whatever moment it is applied to, so a reader that checks a
/// written step before any moment exists - the preset catalogue - has to ask the same question, and
/// asking it here keeps the range in one place.
pub fn is_time_of_day(hour: u32, minute: u32, second: u32) -> bool {
    hour <= 23 && minute <= 59 && second <= 59
}

/// Returns the stepped moment and, for a month-folding shift that hit a shorter month, the day that
/// was asked for. Only `Shift` can report one - every other step either keeps the day or picks it.
fn apply_step(
    cur: CivilDateTime,
    step: &Step,
    index: usize,
    calendar: Option<&crate::calendar::Calendar>,
) -> Result<(CivilDateTime, Option<u32>), EvalError> {
    match step {
        Step::Shift { sign, amount, unit } => apply_shift(cur, *sign, *amount, *unit, index, calendar),
        Step::SetTime { hour, minute, second } => {
            if !is_time_of_day(*hour, *minute, *second) {
                return Err(EvalError::BadSetTime { index });
            }
            Ok((CivilDateTime { hour: *hour, minute: *minute, second: *second, ..cur }, None))
        }
        Step::Snap(target) => Ok((apply_snap(cur, *target), None)),
        Step::Nearest(target) => match target {
            // The leap day is pure arithmetic (the next Feb 29), so it needs no holiday calendar -
            // unlike the business-day targets, which do.
            NearestTarget::NextLeapDay => Ok((apply_nearest_leap_day(cur), None)),
            NearestTarget::NextBusinessDay | NearestTarget::PrevBusinessDay => match calendar {
                Some(cal) => apply_nearest(cur, *target, cal, index).map(|c| (c, None)),
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
    // Straight through the shared instant arithmetic, with no string in the middle. The round trip
    // through `to_iso` + `parse_civil` that used to sit here made this the only calculator step
    // that could fail on FORMATTING, and it reported every such failure as `Overflow` - a message
    // about the size of numbers handed to someone whose date simply could not be written in the
    // form the parser read (R3-6). Now the only way this fails is the real one: an instant outside
    // FILETIME.
    // Through the INSTANT, not the FILETIME. A zone step re-expresses the same moment in another
    // offset and needs no epoch of its own, so tying it to the FILETIME floor would have made
    // `--to-zone` fail on every date before 1601 - dates the calculator can otherwise handle.
    let secs = super::civil_fields_to_epoch_secs(
        civil.year,
        civil.month as i64,
        civil.day as i64,
        civil.hour as i64,
        civil.minute as i64,
        civil.second as i64,
        bias_from as i64,
    )
    .ok_or(EvalError::Overflow { index })?;
    Ok(epoch_secs_to_civil(secs, bias_to))
}

/// Jump to the nearest business day in the target's direction (calendar::nearest_business_day).
fn apply_nearest(
    cur: CivilDateTime,
    target: NearestTarget,
    cal: &crate::calendar::Calendar,
    index: usize,
) -> Result<CivilDateTime, EvalError> {
    let forward = matches!(target, NearestTarget::NextBusinessDay);
    crate::calendar::nearest_business_day(&cur, forward, cal).map_err(|limit| business_day_error(limit, cal, index))
}

/// The evaluation error for a business-day walk that produced no answer, shared by `bd` and `nearest`
/// so the two calendar steps cannot describe one cause two ways.
fn business_day_error(
    limit: crate::calendar::BusinessDayLimit,
    cal: &crate::calendar::Calendar,
    index: usize,
) -> EvalError {
    match limit {
        // The request is past the limit - the calendar is fine, and nothing overflowed.
        crate::calendar::BusinessDayLimit::TooManyDays => EvalError::TooManyBusinessDays { index },
        // The calendar is the problem, and the message has to say so.
        crate::calendar::BusinessDayLimit::DegenerateCalendar => EvalError::DegenerateCalendar { index },
        // `valid_from` is always set when the walk can stop here - it is the only thing that stops it.
        crate::calendar::BusinessDayLimit::BeforeCalendar => {
            EvalError::BeforeCalendar { index, first_year: cal.valid_from.unwrap_or_default() }
        }
    }
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
        // Bounded by the calendar, not by a counter: in the proleptic Gregorian rule a leap year
        // occurs at least every 8 years (the widest gap is across a non-leap century, e.g. 1896 to
        // 1904), so this walks at most eight steps. `cur.year` is inside the year band by then, so
        // the additions cannot overflow either - a year that lands just past the band is caught by
        // the check `eval` runs after every step.
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

/// Returns the shifted moment and, when a month-length clamp moved the day, the day that was asked
/// for. Only the calendar-folding units can clamp - a fixed-length shift never does.
fn apply_shift(
    cur: CivilDateTime,
    sign: Sign,
    amount: i64,
    unit: Unit,
    index: usize,
    calendar: Option<&crate::calendar::Calendar>,
) -> Result<(CivilDateTime, Option<u32>), EvalError> {
    // The amount is a non-negative magnitude - apply the sign here.
    let signed = match sign {
        Sign::Plus => amount,
        Sign::Minus => amount.checked_neg().ok_or(EvalError::Overflow { index })?,
    };
    match unit {
        Unit::Seconds | Unit::Minutes | Unit::Hours | Unit::Days | Unit::Weeks => {
            shift_fixed(cur, signed, unit, index).map(|c| (c, None))
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
        // Business days need a calendar (weekends plus holidays). With one, walk the calendar -
        // without one (the substitution paths), stay honestly unsupported.
        Unit::BusinessDays => match calendar {
            Some(cal) => crate::calendar::add_business_days(&cur, signed, cal)
                .map(|c| (c, None))
                .map_err(|limit| business_day_error(limit, cal, index)),
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
    let Some(unit_secs) = fixed_unit_seconds(unit) else {
        unreachable!("shift_fixed only handles fixed-length units")
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
fn shift_months(cur: CivilDateTime, months: i64, index: usize) -> Result<(CivilDateTime, Option<u32>), EvalError> {
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
    // The one place a day is clamped, so it is also the one place that can report it honestly.
    let clamped = (cur.day > last).then_some(cur.day);
    Ok((
        CivilDateTime {
            year: new_year,
            month: new_month as u32,
            day: cur.day.min(last),
            hour: cur.hour,
            minute: cur.minute,
            second: cur.second,
        },
        clamped,
    ))
}

// --- Bridge to the substitution tick world (the relative `jump` path) ---------------
//
// The substitution fake clock is a UTC FILETIME - a relative jump means "advance the fake
// clock by one step". Fixed-length units are a tick delta (kept exact, sub-second and all) -
// calendar units are defined on the civil date in the SESSION zone, so they fold through it.

/// Session-local civil fields from a UTC FILETIME. Pure arithmetic, so it cannot fail - and the one
/// conversion `filetime_utc_to_wall` formats, too.
pub(crate) fn filetime_to_civil(ft_utc: i64, tz_bias_min: i32) -> CivilDateTime {
    let secs_1601 = ft_utc.div_euclid(10_000_000);
    epoch_secs_to_civil(secs_1601 - crate::DAYS_1601_TO_1970 * 86_400, tz_bias_min)
}

/// Session-local civil fields from a UTC instant in seconds since the Unix epoch.
///
/// The civil half of the conversion, with the FILETIME epoch factored out - so it works for a
/// moment before 1601, which has an instant but no FILETIME. Pure arithmetic, so it cannot fail.
fn epoch_secs_to_civil(secs_utc: i64, tz_bias_min: i32) -> CivilDateTime {
    // Session-local = UTC - bias (UTC = local + bias). Saturating, because `--analyze` hands this any
    // number a tester pastes: `9223372036854775807` read in a zone east of UTC ran past i64 and the
    // calculator panicked with exit 101 (R4-W8, measured on a UTC+2 host). A saturated instant lies
    // hundreds of billions of years out, far past the year band, so `epoch_readings` drops it exactly
    // as it drops any other reading outside the band - the same refusal the same number already got
    // in UTC. Every other caller hands over an instant inside the band, where nothing saturates.
    let local = secs_utc.saturating_sub((tz_bias_min as i64) * 60);
    let days = local.div_euclid(86_400);
    let tod = local.rem_euclid(86_400);
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

/// The seconds in one fixed-length unit, or `None` for a unit folded through the civil date. One table
/// for the shift and for the tick delta of a jump, which had a copy each (R5-A11). Every unit is named,
/// so a unit added to `Unit` has to be placed here.
fn fixed_unit_seconds(unit: Unit) -> Option<i64> {
    match unit {
        Unit::Seconds => Some(1),
        Unit::Minutes => Some(60),
        Unit::Hours => Some(3_600),
        Unit::Days => Some(86_400),
        Unit::Weeks => Some(604_800),
        Unit::Months | Unit::Quarters | Unit::Years | Unit::BusinessDays => None,
    }
}

/// Signed length of a FIXED-length shift step in FILETIME ticks (100 ns), or `None` if the
/// step is not a fixed-length shift (calendar units and business days need the civil fold,
/// non-shift steps have no tick length). `Err` only on overflow of a fixed shift.
pub fn fixed_shift_ticks(step: &Step) -> Result<Option<i64>, EvalError> {
    let Step::Shift { sign, amount, unit } = step else {
        return Ok(None);
    };
    let Some(unit_secs) = fixed_unit_seconds(*unit) else {
        return Ok(None); // calendar unit or business days: civil fold
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
    // No calendar on the jump path, so a business-day step stays unsupported here. The clamp report
    // is dropped deliberately: a jump's result travels as an instant with no place to carry it, and
    // the jump path builds only `Shift` by a fixed unit today, which never clamps.
    let (shifted, _clamped) = apply_step(civil, step, 0, None)?;
    // The same two things `eval` does for the calculator, for the same two reasons: the step
    // computed this year arithmetically (so the parser never saw it), and going back out through a
    // string would make formatting a failure mode of a jump. The instant check below then rejects
    // anything FILETIME cannot hold, which on this path is nearly everything above year 30828.
    if !crate::civil_year_in_band(shifted.year) {
        return Err(EvalError::YearOutOfRange { index: 0 });
    }
    super::civil_fields_to_filetime_utc(
        shifted.year,
        shifted.month as i64,
        shifted.day as i64,
        shifted.hour as i64,
        shifted.minute as i64,
        shifted.second as i64,
        tz_bias_min as i64,
    )
    .ok_or(EvalError::Overflow { index: 0 })
}

/// The target UTC FILETIME after applying ONE shift step to the current fake instant - the
/// single source of truth for "current fake + one step" on the substitution jump path. A
/// fixed-length unit adds a tick delta (sub-second precision preserved) - a calendar unit
/// folds through the civil date in the session zone. Business days need a calendar, which a session
/// does not have, so they are refused as `NeedsCalendar`.
///
/// A target outside the session range is `Overflow`: rejected, never clamped (R4-S7c). Only an i64
/// overflow was caught before, so `jump -300000d` was accepted and parked the clock on 1601-01-01 with
/// nothing said, and in a zone west of UTC the local-time channels went back to the real clock. The
/// range is the one [`crate::session_instant`] holds a start to, so a jump cannot reach an instant
/// the session could not have started at.
pub fn step_target(fake_now_ft: i64, tz_bias_min: i32, step: &Step) -> Result<i64, EvalError> {
    let target = match fixed_shift_ticks(step)? {
        Some(ticks) => fake_now_ft.checked_add(ticks).ok_or(EvalError::Overflow { index: 0 })?,
        None => shift_filetime(fake_now_ft, tz_bias_min, step)?,
    };
    if !crate::instant_in_session_range(target, tz_bias_min) {
        return Err(EvalError::Overflow { index: 0 });
    }
    Ok(target)
}

// --- Output formats (the calculator's right column, docs/02 section 8) ---------------

const MONTH_ABBR: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTH_FULL: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
    "November", "December",
];
const DOW_ABBR: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]; // 0 = Sunday

/// Day of week for a civil date: 0 = Sunday .. 6 = Saturday.
fn day_of_week(civil: &CivilDateTime) -> usize {
    crate::weekday(days_from_civil(civil.year, civil.month as i64, civil.day as i64)) as usize
}

/// A moment rendered in every fixed output format at once (docs/02 section 8, in that order).
/// The civil formats are always present - the instant-based ones (epoch, FILETIME, RFC 1123)
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

/// The UTC FILETIME of a civil moment read in the session zone `tz_bias_min`, or `None` outside
/// the FILETIME range (before 1601, after 30828). Only the `filetime` output format reads it - the
/// other instant formats and the instant markers go through [`instant_epoch_secs`], which has no
/// such end.
fn instant_filetime(civil: &CivilDateTime, tz_bias_min: i32) -> Option<i64> {
    super::civil_fields_to_filetime_utc(
        civil.year,
        civil.month as i64,
        civil.day as i64,
        civil.hour as i64,
        civil.minute as i64,
        civil.second as i64,
        tz_bias_min as i64,
    )
}

/// The same moment as a UTC instant in seconds since the Unix epoch - defined for dates the
/// FILETIME cannot express, which is the whole reason it is a separate function.
fn instant_epoch_secs(civil: &CivilDateTime, tz_bias_min: i32) -> Option<i64> {
    super::civil_fields_to_epoch_secs(
        civil.year,
        civil.month as i64,
        civil.day as i64,
        civil.hour as i64,
        civil.minute as i64,
        civil.second as i64,
        tz_bias_min as i64,
    )
}

/// Render `civil` (wall-clock in the session zone `tz_bias_min`) in every fixed format.
pub fn formats(civil: &CivilDateTime, tz_bias_min: i32) -> Formats {
    let iso_date = format!("{}-{:02}-{:02}", format_year(civil.year), civil.month, civil.day);
    let iso_datetime = format!("{}{}", civil.to_iso(), crate::bias_label(tz_bias_min));
    let us = format!("{:02}/{:02}/{}", civil.month, civil.day, format_year(civil.year));
    let pl = format!("{:02}.{:02}.{}", civil.day, civil.month, format_year(civil.year));

    // Instant-based formats: the civil moment interpreted in the session zone as a UTC instant.
    //
    // The epoch fields and RFC 1123 come from the INSTANT, not from the FILETIME, because those
    // three exist for moments the FILETIME cannot hold. Year 1000 has an instant of
    // -30 610 231 200 and reads as "Tue, 31 Dec 0999 22:00:00 GMT" - what it does not have is a
    // FILETIME, which counts from 1601 and is unsigned. Deriving all four from the FILETIME made
    // one wrong answer (a negative FILETIME) and, once that was refused, would have taken two
    // correct ones down with it.
    let (rfc1123, epoch_seconds, epoch_millis) = match instant_epoch_secs(civil, tz_bias_min) {
        Some(secs) => {
            let utc = epoch_secs_to_civil(secs, 0);
            // RFC 1123 spells the year in FOUR digits, so a year outside 0..=9999 has no rendering
            // in it - printing six digits there would be a string in no format at all. The epoch
            // fields have no such limit and stay.
            let rfc = (0..=9999).contains(&utc.year).then(|| {
                format!(
                    "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
                    DOW_ABBR[day_of_week(&utc)],
                    utc.day,
                    MONTH_ABBR[(utc.month - 1) as usize],
                    utc.year,
                    utc.hour,
                    utc.minute,
                    utc.second
                )
            });
            (rfc, Some(secs), secs.checked_mul(1_000))
        }
        None => (None, None, None),
    };
    let filetime = instant_filetime(civil, tz_bias_min);

    Formats { iso_date, iso_datetime, us, pl, rfc1123, epoch_seconds, epoch_millis, filetime }
}

// --- Custom format mask (7.3, docs/02 section 8 point 9 - hit the target app's exact format) ---

/// What a mask rendered to, and what the vocabulary did not recognise in it.
///
/// The second half exists because the first one alone can mislead. A run of letters that is not a
/// token used to be emitted verbatim, so `h:mm tt` produced `h:05 tt` - a string that LOOKS like a
/// formatted time and carries raw mask letters. Silent, and the reader's first guess is their own
/// typo. The caller shows `unknown` so the answer says what it could not do (rule 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskRender {
    pub text: String,
    /// Letter runs the vocabulary does not know, first appearance first, without repeats. Empty when
    /// every letter in the mask was either a token or explicitly quoted.
    pub unknown: Vec<String>,
}

/// Render `civil` in a user-supplied .NET/Java-style mask so the output can match the exact format
/// the target app uses. Case-sensitive: `M` is month, `m` is minute, `H` is 24-hour and `h` is
/// 12-hour. A token is a maximal run of one letter (`yyyy`, `MM`, `dd`, `HH`, `hh`, `mm`, `ss`, `tt`,
/// plus `MMM`/`MMMM` month names and `ddd`/`dddd` weekday names).
///
/// Literal text goes in single quotes, as in Java's `SimpleDateFormat`: `'at' HH:mm` renders `at`
/// verbatim, and `''` inside a quoted run is one apostrophe. Without quotes a letter run that is not
/// a token still passes through, but it is REPORTED in [`MaskRender::unknown`] - passing through was
/// never a promise, only what happened to work, and `date=` would have lost its `d` to the day token.
/// Anything that is not a letter (`/`, `.`, `:`, spaces) is literal and needs no quoting.
///
/// A zone token and single `y` are not built.
pub fn format_with_mask(civil: &CivilDateTime, mask: &str) -> MaskRender {
    let chars: Vec<char> = mask.chars().collect();
    let mut text = String::new();
    let mut unknown: Vec<String> = Vec::new();
    // The runs already reported, so each is listed once in the order it first appears. A set, because
    // looking each run up in the list made a mask of 32 000 different letters take 0.95 s (R5-WY10).
    let mut reported: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            i = push_quoted(&chars, i, &mut text, &mut unknown);
            continue;
        }
        // Consume the maximal run of the same character.
        let mut n = 1;
        while i + n < chars.len() && chars[i + n] == c {
            n += 1;
        }
        match mask_token(civil, c, n) {
            Some(field) => text.push_str(&field),
            None => {
                (0..n).for_each(|_| text.push(c));
                // Only LETTERS are reported. A run of `/` or `:` is punctuation the mask is made of,
                // and calling it unrecognised would bury the real finding in noise.
                if c.is_alphabetic() {
                    let run: String = std::iter::repeat_n(c, n).collect();
                    if reported.insert(run.clone()) {
                        unknown.push(run);
                    }
                }
            }
        }
        i += n;
    }
    MaskRender { text, unknown }
}

/// Copy a quoted literal run into `text`, returning the index just past its closing quote. `''` is
/// one apostrophe. An unterminated quote takes the rest of the mask and is reported, because the
/// alternative - ending it silently at the end of input - is a mask that renders and is not what was
/// written.
fn push_quoted(chars: &[char], start: usize, text: &mut String, unknown: &mut Vec<String>) -> usize {
    // `''` is an apostrophe wherever it appears, including outside a quoted run - Java's rule, and
    // the one this vocabulary follows. Reading it as an empty quoted run instead would make a lone
    // apostrophe unwritable, which is the character an English mask most often needs ("o'clock").
    if chars.get(start + 1) == Some(&'\'') {
        text.push('\'');
        return start + 2;
    }
    let mut i = start + 1;
    while i < chars.len() {
        if chars[i] == '\'' {
            if chars.get(i + 1) == Some(&'\'') {
                text.push('\'');
                i += 2;
                continue;
            }
            return i + 1;
        }
        text.push(chars[i]);
        i += 1;
    }
    let unterminated = "'".to_string();
    if !unknown.contains(&unterminated) {
        unknown.push(unterminated);
    }
    i
}

/// The hour on a 12-hour clock: 0 and 12 both read as 12, the rest unchanged.
fn hour_12(hour: u32) -> u32 {
    match hour % 12 {
        0 => 12,
        h => h,
    }
}

/// The field for a mask token: character `c` repeated `n` times, or None when `(c, n)` is not a
/// recognised token (then the caller emits the run literally and reports it). The .NET / Java token
/// vocabulary.
fn mask_token(civil: &CivilDateTime, c: char, n: usize) -> Option<String> {
    let month = (civil.month - 1) as usize;
    let dow = day_of_week(civil);
    Some(match (c, n) {
        ('y', 4) => format_year(civil.year),
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
        // The 12-hour clock, which is how the US market writes time (docs/02 section 7) and one of
        // the two MVP markets. Midnight and noon are 12, not 0 - the same rule .NET and Java state.
        ('h', 2) => format!("{:02}", hour_12(civil.hour)),
        ('h', 1) => hour_12(civil.hour).to_string(),
        // `tt` is the full designator, `t` its first letter - both as .NET defines them. Invariant
        // English, like the month and weekday names above: this mask matches the TARGET APP's
        // output, and the data locale is not the interface language (rule 15).
        ('t', 2) => (if civil.hour < 12 { "AM" } else { "PM" }).to_string(),
        ('t', 1) => (if civil.hour < 12 { "A" } else { "P" }).to_string(),
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
/// calendar year at the boundaries (2005-01-01 is 2004-W53 - 2019-12-30 is 2020-W01).
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
    let jan1_dow = i64::from(crate::weekday(jan1)); // 0 = Sunday, same basis as day_of_week
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

// --- Significance ("what this date lands on", 7.3 - the calculator's differentiator) ------
//
// The calculator names the edge case a result date lands on, instead of giving a number and
// staying silent like an online date calculator (6.2). Calendar-independent landmarks are always
// checked - the weekend / holiday / observed-holiday landmarks need a calendar and are added only
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
    /// The date is before the calendar's first year, so the calendar judges nothing about it - no
    /// weekend, holiday or business-day mark, rather than today's rules applied to a year they did not
    /// hold in (R4-S20). Stands where those three would.
    BeforeCalendar,
    /// The calendar judged a date after the day its holidays were checked against the law, and that
    /// check is more than a year old (`calendar::outdated`): a holiday added since may be missing. Not
    /// produced by [`significance`], which has no "today" - the caller that has one adds it (R4/20).
    CalendarOutdated,
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
            Significance::BeforeCalendar => "before_calendar",
            Significance::CalendarOutdated => "calendar_outdated",
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
            Significance::BeforeCalendar => {
                "before the years the calendar covers - no weekend, holiday or business-day judgement"
            }
            Significance::CalendarOutdated => {
                "the calendar was checked against the law more than a year ago - a holiday added since may be missing"
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
/// (the same opt-in the metadata block uses) - without one they are simply absent, never guessed.
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

    // Instant markers: the result interpreted in the session zone as a UTC instant. Counted in epoch
    // seconds, not through FILETIME: FILETIME ends in the year 30828, so from there on the 2038 marker
    // disappeared from dates that are long past it (R5-P22). `formats()` moved to epoch seconds for the
    // same reason earlier.
    if let Some(epoch) = instant_epoch_secs(civil, tz_bias_min) {
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
    // A date before the calendar's first year gets one mark saying it was not judged - the weekend is
    // one of the rules the calendar has no source for there, so even "weekend" would be a guess.
    match calendar {
        Some(cal) if !cal.judges_year(civil.year) => out.push(Significance::BeforeCalendar),
        Some(cal) => {
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
        None => {}
    }

    out
}

// --- Reverse analysis (7.3 - paste a date, name what it is, show both readings when ambiguous) ---

/// One reading of an analyzed date string. A numeric date with no clear order has two (US month-
/// first, PL day-first) - an ISO date has one. Stable variants, English `label` like the other enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateReading {
    /// An unambiguous ISO 8601 date (dash-separated).
    Iso,
    /// An ISO 8601 date and time with its own UTC offset (`Z` or `+02:00`), the RFC 3339 form this tool
    /// prints itself. It names an instant, so it is shown in the session zone like an epoch (R5-D5,
    /// item 5) - the analyser used to refuse it as a malformed second.
    IsoOffset,
    /// The US reading of a numeric date: month first (MM/DD/YYYY).
    UsMonthDay,
    /// The Polish / European reading of a numeric date: day first (DD/MM/YYYY).
    PlDayMonth,
    /// A bare number read as Unix epoch SECONDS.
    EpochSeconds,
    /// A bare number read as Unix epoch MILLISECONDS. A bare number is genuinely ambiguous between
    /// the two - `1740607200` is a plausible instant in seconds and another in milliseconds - so both
    /// are offered, the same way an ambiguous month/day order is (docs/02 8.1).
    EpochMillis,
}

impl DateReading {
    /// Stable machine token for the JSON calc output (chronomock.calc/1), unlike the human `label`.
    pub fn key(&self) -> &'static str {
        match self {
            DateReading::Iso => "iso",
            DateReading::IsoOffset => "iso_offset",
            DateReading::UsMonthDay => "us_month_day",
            DateReading::PlDayMonth => "pl_day_month",
            DateReading::EpochSeconds => "epoch_seconds",
            DateReading::EpochMillis => "epoch_millis",
        }
    }

    /// Whether this reading names an instant (a number of seconds or milliseconds since 1970, or a date
    /// and time with its own UTC offset) rather than civil fields someone wrote down. An instant has a
    /// wall clock only in some zone, so its reading is the one that depends on the session zone, and the
    /// one that has a time of day.
    pub fn is_instant(&self) -> bool {
        matches!(self, DateReading::EpochSeconds | DateReading::EpochMillis | DateReading::IsoOffset)
    }

    /// English label for the CLI (a translation key when the calculator GUI arrives).
    pub fn label(&self) -> &'static str {
        match self {
            DateReading::Iso => "ISO 8601",
            DateReading::IsoOffset => "ISO 8601 with UTC offset",
            DateReading::UsMonthDay => "US MM/DD/YYYY",
            DateReading::PlDayMonth => "PL DD/MM/YYYY",
            DateReading::EpochSeconds => "Unix epoch seconds",
            DateReading::EpochMillis => "Unix epoch milliseconds",
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

/// Read a bare number as an epoch, in seconds and in milliseconds, returning every reading whose
/// year this build computes on. `None` when the input is not a bare number at all, which is what
/// lets the caller fall through to the dated formats.
///
/// Both units are offered rather than one guessed from magnitude. A magnitude rule reads well until
/// it does not: `1000000000` is September 2001 in seconds and January 1970 in milliseconds, and both
/// are instants a tester might paste from a log. Choosing by size would silently pick one, which is
/// the thing this analyser refuses to do for month/day order too. A reading whose year falls outside
/// the computable band is dropped rather than shown wrong - if that leaves nothing, the input is not
/// a usable epoch and the caller reports it unrecognised.
/// Whether the input is nothing but digits, with an optional leading minus - the shape of an epoch.
///
/// Separate from reading it, because the two answer different questions and the caller needs both.
/// "Is this meant to be a number" decides WHICH refusal an unusable input gets, and "does it resolve
/// to a date" decides whether there is a refusal at all. Folded together, a 20-digit timestamp came
/// back as a format complaint about input whose format was never in doubt.
fn is_bare_number(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

fn epoch_readings(s: &str, zone_bias_min: i32) -> Option<Vec<(DateReading, CivilDateTime)>> {
    if !is_bare_number(s) {
        return None;
    }
    // Past i64 there is no instant to reach at all, so this is one of the two ways a number can fail
    // to be a date - the caller says so in the same sentence as the other one.
    let n: i64 = s.parse().ok()?;
    let mut readings = Vec::with_capacity(2);
    for (reading, secs) in [
        (DateReading::EpochSeconds, Some(n)),
        (DateReading::EpochMillis, Some(n.div_euclid(1_000))),
    ] {
        let Some(secs) = secs else { continue };
        let civil = epoch_secs_to_civil(secs, zone_bias_min);
        // The two units meet where N seconds and N milliseconds fall in the same second - 0 and -1.
        // One instant is listed once, the way a month/day order that reads the same both ways is:
        // two identical lines under "ambiguous" asked the reader to choose between nothing (R4-N36).
        let repeat = readings.last().is_some_and(|(_, earlier)| *earlier == civil);
        if crate::civil_year_in_band(civil.year) && !repeat {
            readings.push((reading, civil));
        }
    }
    (!readings.is_empty()).then_some(readings)
}

/// The reading of an ISO 8601 date (`s`, trimmed `input`): civil fields as written, or - when a time
/// carries a UTC offset - the instant they name, shown in the session zone (R5-D5, item 5).
///
/// RFC 3339 is the form this tool prints (`iso_datetime`, `2026-01-01T00:00:00+02:00`), and the
/// analyser refused its own output: the offset and a fraction of a second came back as "bad second",
/// and `Z` the same. A fraction (1 to 9 digits) is read and cut to the second, as every reading here is
/// to the second. An offset is `Z`, `z` or `±HH:MM` after the time, inside the range a zone may have. A
/// date alone stays a date at midnight. Every refusal quotes what was pasted, not the text built from
/// it - `T00:00:00` appended to a date nobody wrote with a time.
fn iso_reading(s: &str, input: &str, zone_bias_min: i32) -> Result<(DateReading, CivilDateTime), String> {
    let Some((date, time)) = s.split_once(['T', ' ']) else {
        return Ok((DateReading::Iso, parse_civil_datetime_as(&format!("{s}T00:00:00"), input)?));
    };
    let (time, offset) = split_utc_offset(time, input)?;
    let whole = whole_seconds(time, input)?;
    let civil = parse_civil_datetime_as(&format!("{date}T{whole}"), input)?;
    let Some(bias) = offset else {
        return Ok((DateReading::Iso, civil));
    };
    // The written year is inside the band, but the same instant shown in the session zone can fall a
    // day past either end of it: `262143-12-31T23:59:59-14:59` in a zone of +14:59 is in the year
    // 262144 (review round on PR #104).
    convert_zone(civil, bias, zone_bias_min, 0)
        .ok()
        .filter(|at| crate::civil_year_in_band(at.year))
        .map(|at| (DateReading::IsoOffset, at))
        .ok_or_else(|| format!("'{input}' names an instant outside the range this build computes on"))
}

/// The time part without its UTC offset, and the offset as a bias (minutes, UTC = local + bias), or
/// `None` when the time names no offset.
fn split_utc_offset<'a>(time: &'a str, input: &str) -> Result<(&'a str, Option<i32>), String> {
    if let Some(rest) = time.strip_suffix(['Z', 'z']) {
        return Ok((rest, Some(0)));
    }
    let Some(at) = time.rfind(['+', '-']) else {
        return Ok((time, None));
    };
    let (rest, offset) = time.split_at(at);
    let refused = || format!("the UTC offset in '{input}' must be Z or +HH:MM / -HH:MM, from -14:59 to +14:59");
    let two_digits = |t: &str| (t.len() == 2 && crate::is_ascii_digits(t)).then(|| t.parse::<i32>().ok()).flatten();
    let (hours, minutes) = offset[1..]
        .split_once(':')
        .and_then(|(h, m)| Some((two_digits(h)?, two_digits(m)?)))
        .filter(|&(_, m)| m <= 59)
        .ok_or_else(refused)?;
    // East of UTC is ahead, so its bias is negative: +02:00 is a bias of -120.
    let span = hours * 60 + minutes;
    let bias = if offset.starts_with('+') { -span } else { span };
    if !crate::zone_bias_in_range(bias) {
        return Err(refused());
    }
    Ok((rest.trim_end(), Some(bias)))
}

/// The time without a fraction of a second, which this model does not hold. A fraction is a dot and 1
/// to 9 digits after the seconds - more digits than a nanosecond, or none, is refused by name.
fn whole_seconds<'a>(time: &'a str, input: &str) -> Result<&'a str, String> {
    let Some((whole, fraction)) = time.split_once('.') else {
        return Ok(time);
    };
    if fraction.len() > 9 || !crate::is_ascii_digits(fraction) {
        return Err(format!("the fraction of a second in '{input}' must be 1 to 9 digits"));
    }
    Ok(whole)
}

/// A civil date at midnight, or None if the month or day cannot exist - the validity gate that
/// turns a candidate month/day order into a real date (month 13 and Feb 30 are rejected).
fn civil_date(year: i64, month: i64, day: i64) -> Option<CivilDateTime> {
    if !(1..=12).contains(&month) || day < 1 || day > last_day_of_month(year, month) as i64 {
        return None;
    }
    Some(CivilDateTime { year, month: month as u32, day: day as u32, hour: 0, minute: 0, second: 0 })
}

/// Recognise a pasted date and return its reading(s). ISO (dash-separated, a four-digit year first,
/// optional time) is unambiguous - a slash- or dot-separated `N/N/YYYY` yields the US and PL
/// readings, keeping only the ones that form a real date (so 29/02 is PL-only, 02/29 US-only, 02/30
/// an error, 05/05 one).
/// A bare number is a Unix epoch, read in `zone_bias_min`, in seconds and in milliseconds - both,
/// because a number carries no unit and choosing one would be a guess (docs/02 8.1).
/// FILETIME, RFC 1123 and year-first numeric are honestly not recognised yet (an error, never a
/// guess).
///
/// `zone_bias_min` is the session zone: only the epoch readings depend on it, because only they
/// start from an instant rather than from civil fields someone already wrote down (rule 2).
pub fn analyze_date(input: &str, zone_bias_min: i32) -> Result<DateAnalysis, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("empty input".into());
    }
    // A bare number: epoch. Checked BEFORE the separator dispatch, and it cannot collide with the
    // dated forms - those all carry a separator, and this one may carry nothing but digits and a
    // leading minus (pre-1970 instants are real and this tool exists to reach them).
    //
    // The SHAPE decides which refusal this input gets, and that is the point. A run of digits fell
    // through to the bottom of this function and came back "unrecognised date format", which is a
    // sentence about shape, on input whose shape was read perfectly - what failed was the RANGE.
    // Someone pasting a microsecond timestamp was sent to look at their formatting.
    if is_bare_number(s) {
        return epoch_readings(s, zone_bias_min)
            .map(|readings| DateAnalysis { readings })
            .ok_or_else(|| {
                format!(
                    "'{input}' reads as a number, but neither seconds nor milliseconds since 1970 \
                     put it on a date this build computes on ({CIVIL_YEAR_MIN}..={CIVIL_YEAR_MAX})"
                )
            });
    }
    // ISO: dash-separated, with or without a time (midnight if the time is absent).
    if s.contains('-') {
        // An ISO date leads with its year written in full - four digits at least, after an optional
        // minus for a year before 1 CE. `31-12-25` has the dashes and not the year, and read as ISO
        // it came back as the year 31: a confident answer to a date nobody wrote (R4-N36). Refused
        // like any other shape this analyser does not read, rather than guessed at.
        let date = s.split(['T', ' ']).next().unwrap_or(s);
        let year = date.strip_prefix('-').unwrap_or(date).split('-').next().unwrap_or("");
        if year.len() < 4 || !year.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!(
                "expected an ISO date that starts with a four-digit year (YYYY-MM-DD), got '{input}'"
            ));
        }
        return iso_reading(s, input, zone_bias_min).map(|reading| DateAnalysis { readings: vec![reading] });
    }
    // Numeric: exactly one separator kind, three parts, a four-digit year last.
    let sep = match (s.contains('/'), s.contains('.')) {
        (true, false) => '/',
        (false, true) => '.',
        _ => return Err(format!("unrecognised date format '{input}'")),
    };
    let parts: Vec<&str> = s.split(sep).collect();
    // Four DIGITS, not four characters. Length alone let `-999` through - four characters that parse
    // as a year and produce a perfectly real date in 999 BC, from input the error message promised to
    // reject. The check has to mean what it says (R2-C3).
    if parts.len() != 3
        || parts[2].len() != 4
        || !parts[2].chars().all(|c| c.is_ascii_digit())
    {
        return Err(format!("expected N{sep}N{sep}YYYY, got '{input}'"));
    }
    let num = |p: &str| p.parse::<i64>().map_err(|_| format!("bad number in '{input}'"));
    let (a, b, year) = (num(parts[0])?, num(parts[1])?, num(parts[2])?);
    // Two candidate orders - keep the ones that form a real date, and never list one date twice.
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

    /// Just the rendered text, for the assertions that are about formatting. The unknown-token half
    /// has its own tests - reading both out of one assertion would hide which one failed.
    fn mask_text(civil: &CivilDateTime, mask: &str) -> String {
        format_with_mask(civil, mask).text
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

    /// R2-C2. A clamp is correct, documented, and invisible in the result - which is how a reader ends
    /// up unable to tell it from a defect. The engine reports it so both surfaces can say WHICH step
    /// clamped and from what, instead of each inferring it from a day that changed.
    #[test]
    fn a_clamped_day_is_reported_with_its_step_and_the_day_asked_for() {
        let at = |expr: &MomentExpr| eval_at(expr, dt(2000, 1, 1, 0, 0, 0)).unwrap();

        // The fiscal-year shape: +1 year off the leap day clamps, the following -1 day does not.
        let fiscal = MomentExpr {
            base: abs(dt(2024, 2, 29, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::Years), shift(Sign::Minus, 1, Unit::Days)],
        };
        assert_eq!(at(&fiscal).clamped, vec![(0, 29)], "step 0 asked for day 29");

        // A shift that lands in a month long enough reports nothing - the list is not "every month
        // shift", it is the ones that lost a day.
        let no_clamp = MomentExpr {
            base: abs(dt(2026, 3, 15, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::Months)],
        };
        assert!(at(&no_clamp).clamped.is_empty());

        // Fixed-length units never clamp, whatever they cross.
        let days = MomentExpr {
            base: abs(dt(2024, 2, 29, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 400, Unit::Days)],
        };
        assert!(at(&days).clamped.is_empty());

        // Two clamps in one expression are both reported, each against its own step index - a single
        // flag would hide the second, and a reader checking one number would miss the other.
        let twice = MomentExpr {
            base: abs(dt(2026, 1, 31, 0, 0, 0)),
            steps: vec![
                shift(Sign::Plus, 1, Unit::Months),  // 31 Jan -> 28 Feb, clamped from 31
                shift(Sign::Plus, 1, Unit::Days),    // 28 Feb -> 1 Mar, no clamp
                shift(Sign::Plus, 1, Unit::Months),  // 1 Mar -> 1 Apr, no clamp
            ],
        };
        assert_eq!(at(&twice).clamped, vec![(0, 31)]);

        // Quarters and years fold through the same month arithmetic, so they report the same way.
        let quarters = MomentExpr {
            base: abs(dt(2026, 5, 31, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::Quarters)], // 31 May -> 31 Aug, no clamp
        };
        assert!(at(&quarters).clamped.is_empty());
        let quarters_short = MomentExpr {
            base: abs(dt(2026, 3, 31, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::Quarters)], // 31 Mar -> 30 Jun, clamped
        };
        assert_eq!(at(&quarters_short).clamped, vec![(0, 31)]);
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
            valid_from: None, law_as_of: None, holidays: vec![],
        };
        // 2026-07-10 is a Friday - +1 business day is Monday 2026-07-13, time of day kept.
        let expr = MomentExpr {
            base: abs(dt(2026, 7, 10, 9, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::BusinessDays)],
        };
        let out =
            eval(&expr, &EvalContext { now: dt(2000, 1, 1, 0, 0, 0), zone_bias_min: 0, calendar: Some(&cal) })
                .unwrap();
        assert_eq!(out.result(), dt(2026, 7, 13, 9, 0, 0));
    }

    /// R4-S20 and R4/20, at the evaluation: a step that walks before the calendar's first year is
    /// refused with that year, and every calendar step leaves how far it judged - the later end of its
    /// walk - so the caller can ask whether the calendar was old for it.
    #[test]
    fn calendar_steps_report_their_reach_and_refuse_a_walk_before_the_first_year() {
        use crate::calendar::{Calendar, Observed};
        let cal = Calendar {
            id: "t".into(),
            country: "US".into(),
            weekend: vec![0, 6],
            observed: Observed::None,
            valid_from: Some(2026),
            law_as_of: None,
            holidays: vec![],
        };
        let run = |base: CivilDateTime, steps: Vec<Step>| {
            eval(
                &MomentExpr { base: abs(base), steps },
                &EvalContext { now: dt(2000, 1, 1, 0, 0, 0), zone_bias_min: 0, calendar: Some(&cal) },
            )
        };
        // Forward: the walk ends latest. 2026-07-10 is a Friday.
        let out = run(dt(2026, 7, 10, 9, 0, 0), vec![shift(Sign::Plus, 1, Unit::BusinessDays)]).unwrap();
        assert_eq!(out.calendar_reach, Some(dt(2026, 7, 13, 9, 0, 0)));
        // Backward: it started latest. A later step that does not ask the calendar leaves the reach alone.
        let out = run(
            dt(2026, 7, 13, 9, 0, 0),
            vec![shift(Sign::Minus, 1, Unit::BusinessDays), shift(Sign::Plus, 30, Unit::Days)],
        )
        .unwrap();
        assert_eq!(out.calendar_reach, Some(dt(2026, 7, 13, 9, 0, 0)));
        assert_eq!(out.result(), dt(2026, 8, 9, 9, 0, 0));
        // No calendar step, no reach.
        let out = run(dt(2026, 7, 10, 9, 0, 0), vec![shift(Sign::Plus, 1, Unit::Days)]).unwrap();
        assert_eq!(out.calendar_reach, None);
        // Out of the range: refused, with the first year, on the step that walked out.
        assert_eq!(
            run(dt(2026, 1, 2, 9, 0, 0), vec![shift(Sign::Plus, 1, Unit::Days), shift(Sign::Minus, 3, Unit::BusinessDays)]),
            Err(EvalError::BeforeCalendar { index: 1, first_year: 2026 })
        );
        assert_eq!(
            run(dt(2026, 1, 3, 9, 0, 0), vec![Step::Nearest(NearestTarget::PrevBusinessDay)]),
            Ok(EvalOutcome {
                base: dt(2026, 1, 3, 9, 0, 0),
                after_each: vec![dt(2026, 1, 2, 9, 0, 0)],
                result_bias: 0,
                clamped: vec![],
                calendar_reach: Some(dt(2026, 1, 3, 9, 0, 0)),
            })
        );
        assert_eq!(
            run(dt(2026, 1, 1, 9, 0, 0), vec![Step::Nearest(NearestTarget::PrevBusinessDay)]),
            Ok(EvalOutcome {
                base: dt(2026, 1, 1, 9, 0, 0),
                after_each: vec![dt(2026, 1, 1, 9, 0, 0)],
                result_bias: 0,
                clamped: vec![],
                calendar_reach: Some(dt(2026, 1, 1, 9, 0, 0)),
            })
        );
        assert_eq!(
            run(dt(2026, 1, 4, 9, 0, 0), vec![shift(Sign::Minus, 3, Unit::BusinessDays)]),
            Err(EvalError::BeforeCalendar { index: 0, first_year: 2026 })
        );
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
            valid_from: None, law_as_of: None, holidays: vec![],
        };
        // 2026-07-04 is a Saturday - the next business day is Monday 2026-07-06.
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
        // At the very TOP of the year band, not at a comfortable round number. Both this test and
        // the one below used year 40 000 and passed while the real edge sat eleven orders of
        // magnitude further out, where the same call panicked (R3-1): the property was right, the
        // value never reached far enough to test it.
        let f = formats(&dt(crate::CIVIL_YEAR_MAX, 1, 1, 0, 0, 0), 0);
        assert_eq!(f.iso_date, "262143-01-01");
        assert_eq!(f.us, "01/01/262143");
        // No FILETIME (it saturates near 30828) and no RFC 1123 (its year field is four digits),
        // but the INSTANT itself is an ordinary number and is reported. Those are three different
        // limits, and folding them into one used to drop two correct answers with the wrong one.
        assert!(f.filetime.is_none());
        assert!(f.rfc1123.is_none());
        assert_eq!(f.epoch_seconds, Some(8_210_266_876_800));

        // And at the bottom, which nothing covered at all: a year before 1 CE renders with its sign
        // in front of a four-digit field, in every format, which is the form the parser reads back.
        //
        // Its `filetime` is deliberately NOT asserted here. A date before 1601 yields a negative
        // FILETIME, and this build prints it - measured to predate this change (the binary at the
        // previous commit prints the same -189657576000000000 for year 1000), so it is a separate,
        // older question about a field Windows defines as unsigned, not something to settle inside
        // a formatting test.
        let neg = formats(&dt(-9, 1, 1, 0, 0, 0), 0);
        assert_eq!(neg.iso_date, "-0009-01-01");
        assert_eq!(neg.us, "01/01/-0009");
        assert_eq!(neg.pl, "01.01.-0009");
    }

    #[test]
    fn significance_out_of_range_year_does_not_panic() {
        // The instant markers (epoch, 2038) are skipped when the year has no FILETIME instant -
        // the calendar-independent civil landmarks still work, with no panic - checked at the edge
        // of the band rather than well short of it.
        let s = sig(&dt(crate::CIVIL_YEAR_MAX, 1, 1, 0, 0, 0), 0);
        assert!(s.contains(&Significance::StartOfYear));
        let s = sig(&dt(crate::CIVIL_YEAR_MIN, 1, 1, 0, 0, 0), 0);
        assert!(s.contains(&Significance::StartOfYear));
    }

    /// A step BUILDS its year arithmetically and never returns through the parser, so a guard at
    /// the parser alone leaves this door open. Measured before the fix: this exact expression died
    /// with `attempt to add with overflow` and exit 101 from a base that was perfectly legal.
    #[test]
    fn a_step_that_computes_a_year_past_the_band_is_refused_not_panicked() {
        let expr = MomentExpr {
            base: Base::Absolute(dt(2026, 1, 1, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 25_252_734_927_766_000, Unit::Years)],
        };
        assert!(matches!(
            eval_at(&expr, dt(2026, 1, 1, 0, 0, 0)),
            Err(EvalError::YearOutOfRange { index: 0 })
        ));

        // Just past the edge counts too - the band is the rule, not "somewhere near an overflow".
        let expr = MomentExpr {
            base: Base::Absolute(dt(crate::CIVIL_YEAR_MAX, 1, 1, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::Years)],
        };
        assert!(matches!(
            eval_at(&expr, dt(2026, 1, 1, 0, 0, 0)),
            Err(EvalError::YearOutOfRange { index: 0 })
        ));

        // And one step inside it still computes.
        let expr = MomentExpr {
            base: Base::Absolute(dt(crate::CIVIL_YEAR_MAX - 1, 1, 1, 0, 0, 0)),
            steps: vec![shift(Sign::Plus, 1, Unit::Years)],
        };
        assert_eq!(eval_at(&expr, dt(2026, 1, 1, 0, 0, 0)).unwrap().after_each[0].year, crate::CIVIL_YEAR_MAX);
    }

    /// `eval` is public API of the core, so it does not take its caller's word for the base either.
    #[test]
    fn a_base_year_past_the_band_is_refused_by_eval_itself() {
        let expr = MomentExpr {
            base: Base::Absolute(dt(crate::CIVIL_YEAR_MAX + 1, 1, 1, 0, 0, 0)),
            steps: vec![],
        };
        assert!(matches!(eval_at(&expr, dt(2026, 1, 1, 0, 0, 0)), Err(EvalError::BaseYearOutOfRange)));
    }

    /// The calculator can produce a date before 1 CE (any shift back past year 1), and until this
    /// was fixed it printed one it would then refuse as input - a closed loop that did not close,
    /// on the surface whose whole job is "work out a date, then use it" (R3-6).
    #[test]
    fn a_date_before_year_one_can_be_read_back_in() {
        for step in [
            shift(Sign::Minus, 10, Unit::Years),
            shift(Sign::Minus, 400, Unit::Years),
            shift(Sign::Minus, 1000, Unit::Days),
        ] {
            let expr =
                MomentExpr { base: Base::Absolute(dt(1, 1, 1, 0, 0, 0)), steps: vec![step] };
            let out = eval_at(&expr, dt(2026, 1, 1, 0, 0, 0)).expect("shift back past year 1");
            let printed = out.after_each[0].to_iso();
            assert!(printed.starts_with('-'), "expected a negative year, got {printed}");
            let read_back = parse_civil_datetime(&printed)
                .unwrap_or_else(|e| panic!("the tool refused its own output {printed}: {e}"));
            assert_eq!(read_back, out.after_each[0]);
        }
    }

    /// A `zone` step used to go out through `to_iso` and back through the parser, which made it the
    /// one step that could fail on FORMATTING - and every such failure was reported as an
    /// arithmetic overflow. Before the fix this returned `Err(Overflow)` - the instant is unchanged
    /// by a zone step, so the only honest answer is the same moment read in the other zone.
    #[test]
    fn a_zone_step_works_on_a_date_before_year_one() {
        let expr = MomentExpr {
            base: Base::Absolute(dt(1, 1, 1, 0, 0, 0)),
            steps: vec![shift(Sign::Minus, 10, Unit::Years), Step::Zone(-120)],
        };
        let out = eval_at(&expr, dt(2026, 1, 1, 0, 0, 0)).expect("a zone step must not fail on formatting");
        assert_eq!(out.result_bias, -120);
        assert_eq!(out.after_each[1].year, -9);
    }

    #[test]
    fn significance_and_reading_keys_are_stable_tokens() {
        // The JSON calc contract (chronomock.calc/1) rides on these tokens, so they are pinned here
        // - a wording change to `label` must never silently change the machine key (rule 17).
        assert_eq!(Significance::EndOfQuarter.key(), "end_of_quarter");
        assert_eq!(Significance::LeapDay.key(), "leap_day");
        assert_eq!(Significance::UnixEpoch.key(), "unix_epoch");
        assert_eq!(Significance::ObservedHoliday.key(), "observed_holiday");
        assert_eq!(Significance::BeforeCalendar.key(), "before_calendar");
        assert_eq!(Significance::CalendarOutdated.key(), "calendar_outdated");
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
        // A sub-second remainder on an ordinary date. The base used to be 100 s after 1601, where
        // the two-hour step below landed BEFORE 1601 and the test asserted that as a success - the
        // very jump R4-S7c is about.
        let ft = ft_of("2026-01-01T00:00:00", 0) + 123;
        assert_eq!(step_target(ft, 0, &shift(Sign::Plus, 1, Unit::Seconds)).unwrap(), ft + 10_000_000);
        assert_eq!(step_target(ft, 0, &shift(Sign::Minus, 2, Unit::Hours)).unwrap(), ft - 2 * 3600 * 10_000_000);
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
        let top = crate::FAKE_WALL_MAX;
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
        // `eval` builds the `zone` step - `jump` never does, so the jump path reports it unsupported.
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

    // --- significance ("what this date lands on") ----------------------------

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
        // Past the end of FILETIME too (R5-P22): the marker was counted through FILETIME and
        // disappeared from the year 30829 on, while the calculator reaches the year 262143.
        assert!(sig(&dt(30829, 1, 1, 0, 0, 0), 0).contains(&Significance::Year2038Boundary));
        assert!(sig(&dt(40000, 1, 1, 0, 0, 0), 0).contains(&Significance::Year2038Boundary));
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
            valid_from: None, law_as_of: None, holidays: vec![h("new_year", 1, 1), h("independence", 7, 4)],
        }
    }

    /// R4-S20: a date before the calendar's first year gets one honest mark instead of weekend and
    /// holiday marks judged with rules that did not hold then. The civil marks stay - they need no
    /// calendar.
    #[test]
    fn significance_before_the_calendar_says_so_instead_of_judging() {
        let limited = crate::calendar::Calendar {
            valid_from: Some(2027),
            ..cal_us(crate::calendar::Observed::SatToFriSunToMon)
        };
        // 2026-07-04: a Saturday and Independence Day - neither is claimed for a year the calendar does not cover.
        assert_eq!(significance(&dt(2026, 7, 4, 0, 0, 0), 0, Some(&limited)), vec![Significance::BeforeCalendar]);
        // 2026-12-31 keeps its year-end mark, which is arithmetic.
        assert_eq!(
            significance(&dt(2026, 12, 31, 0, 0, 0), 0, Some(&limited)),
            vec![Significance::EndOfYear, Significance::BeforeCalendar]
        );
        // From the first year on the calendar judges as before.
        assert_eq!(
            significance(&dt(2027, 7, 5, 0, 0, 0), 0, Some(&limited)),
            vec![Significance::ObservedHoliday]
        );
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
        // An ordinary Saturday (2026-07-11) is just a weekend - an ordinary weekday hits nothing.
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
        let a = analyze_date("2008-08-04", 0).unwrap();
        assert!(!a.is_ambiguous());
        assert_eq!(a.readings, vec![(DateReading::Iso, dt(2008, 8, 4, 0, 0, 0))]);
        // A time component is kept.
        assert_eq!(analyze_date("1990-08-03T23:59:59", 0).unwrap().readings[0].1, dt(1990, 8, 3, 23, 59, 59));
    }

    /// RFC 3339, the form this tool prints, is read (R5-D5, item 5): with an offset it is an instant
    /// shown in the session zone, a fraction is cut to the second, and a refusal quotes the paste.
    #[test]
    fn analyze_reads_its_own_rfc3339_output() {
        let one = |text: &str, zone: i32| analyze_date(text, zone).unwrap().readings;
        // +02:00 is 08:00 UTC, and 10:00 again in a session zone of +02:00 (bias -120).
        assert_eq!(one("2026-01-01T10:00:00+02:00", 0), vec![(DateReading::IsoOffset, dt(2026, 1, 1, 8, 0, 0))]);
        assert_eq!(one("2026-01-01T10:00:00+02:00", -120), vec![(DateReading::IsoOffset, dt(2026, 1, 1, 10, 0, 0))]);
        assert_eq!(one("2025-12-31T23:30:00-01:00", 0), vec![(DateReading::IsoOffset, dt(2026, 1, 1, 0, 30, 0))]);
        assert_eq!(one("2026-01-01T10:00:00Z", 0), vec![(DateReading::IsoOffset, dt(2026, 1, 1, 10, 0, 0))]);
        assert_eq!(one("2026-01-01T10:00:00.123456789Z", 0)[0].1, dt(2026, 1, 1, 10, 0, 0));
        assert_eq!(one("2026-01-01 10:00:00 +02:00", 0)[0].1, dt(2026, 1, 1, 8, 0, 0), "a space before the offset");
        // A fraction without an offset is still the civil fields as written.
        assert_eq!(one("2026-01-01T10:00:00.5", 60), vec![(DateReading::Iso, dt(2026, 1, 1, 10, 0, 0))]);
        assert!(DateReading::IsoOffset.is_instant() && !DateReading::Iso.is_instant());
        // A year before 1 CE keeps its sign, which is not an offset.
        assert_eq!(one("-0001-06-01T00:00:00", 0)[0].0, DateReading::Iso);

        let err = analyze_date("2026-01-01T10:00:00.1234567890", 0).unwrap_err();
        assert!(err.contains("fraction of a second") && err.contains("'2026-01-01T10:00:00.1234567890'"), "{err}");
        let err = analyze_date("2026-01-01T10:00:00+15:00", 0).unwrap_err();
        assert!(err.contains("UTC offset"), "{err}");
        let err = analyze_date("2026-01-01T10:00:00+0200", 0).unwrap_err();
        assert!(err.contains("UTC offset"), "{err}");
        // The refusal quotes what was pasted, and names the month it refuses.
        let err = analyze_date("2026-13-01", 0).unwrap_err();
        assert!(err.contains("month 13") && err.contains("'2026-13-01'") && !err.contains("T00:00:00"), "{err}");

        // A written year inside the band whose instant, in the session zone, falls past either end of
        // it is refused, not shown as a year this build does not compute on (review round on PR #104).
        assert_eq!(one("262143-12-31T23:59:59Z", 0)[0].1.year, crate::CIVIL_YEAR_MAX);
        for (text, zone) in [("262143-12-31T23:59:59-14:59", -899), ("-262143-01-01T00:00:00+14:59", 899)] {
            let err = analyze_date(text, zone).unwrap_err();
            assert!(err.contains("outside the range") && err.contains(text), "{text}: {err}");
        }
    }

    #[test]
    fn analyze_ambiguous_numeric_shows_both_us_and_pl() {
        // 04/08/2008: April 8 (US) and August 4 (PL) - both valid, both shown, US first.
        let a = analyze_date("04/08/2008", 0).unwrap();
        assert!(a.is_ambiguous());
        assert_eq!(
            a.readings,
            vec![
                (DateReading::UsMonthDay, dt(2008, 4, 8, 0, 0, 0)),
                (DateReading::PlDayMonth, dt(2008, 8, 4, 0, 0, 0)),
            ]
        );
        // Dots read the same way.
        assert!(analyze_date("04.08.2008", 0).unwrap().is_ambiguous());
    }

    /// R2-C3. The year check counted CHARACTERS, not digits, so a four-character run that merely
    /// PARSES as a number was taken for a year: `12/25/+999` produced a date in 999, from input the
    /// error message beside it promises to reject as not `N/N/YYYY`.
    ///
    /// 🔴 The case that proves it is `+999`, not `-999`. A minus makes the string contain a dash, so
    /// it is dispatched to the ISO branch long before this check and refused there for a different
    /// reason - a test written on the minus passes with the bug still in place. Checked by reverting.
    #[test]
    fn a_numeric_date_needs_four_year_digits_not_four_characters() {
        for bad in ["12/25/+999", "12.25.+999", "12/25/+009"] {
            assert!(
                analyze_date(bad, 0).is_err(),
                "'{bad}' has no four-digit year and must be refused"
            );
        }
        // The neighbours still work, so this narrowed nothing it should not have.
        assert!(analyze_date("12/25/0999", 0).is_ok());
        assert!(analyze_date("12/25/2026", 0).is_ok());
    }

    /// R2-C5. docs/02 8.1 lists epoch among the formats the analyser recognises, and it did not -
    /// pasting a number out of a log, the commonest thing a tester has in hand, was an error. A bare
    /// number carries no unit, so both readings are offered rather than one picked by magnitude.
    #[test]
    fn a_bare_number_reads_as_epoch_in_both_units() {
        let a = analyze_date("1740607200", 0).unwrap();
        assert!(a.is_ambiguous(), "seconds and milliseconds are both plausible readings");
        assert_eq!(
            a.readings,
            vec![
                (DateReading::EpochSeconds, dt(2025, 2, 26, 22, 0, 0)),
                (DateReading::EpochMillis, dt(1970, 1, 21, 3, 30, 7)),
            ]
        );

        // Epoch is an INSTANT, so the reading follows the session zone - the same rule every other
        // instant in this product obeys (untouchable rule 2).
        let east = analyze_date("1740607200", -120).unwrap();
        assert_eq!(east.readings[0].1, dt(2025, 2, 27, 0, 0, 0));

        // Zero and negative numbers are real instants this tool exists to reach.
        assert_eq!(analyze_date("0", 0).unwrap().readings[0].1, dt(1970, 1, 1, 0, 0, 0));
        assert_eq!(analyze_date("-7200", 0).unwrap().readings[0].1, dt(1969, 12, 31, 22, 0, 0));

        // A number is only an epoch when it is ONLY a number - a dated form still parses as a date.
        assert_eq!(analyze_date("2008-08-04", 0).unwrap().readings[0].0, DateReading::Iso);
        assert!(analyze_date("12/25/2026", 0).unwrap().readings.iter().all(|(r, _)| *r
            != DateReading::EpochSeconds));

        // Still honest about what it cannot read, rather than reaching for a number inside it.
        assert!(analyze_date("Wed, 26 Feb 2025 22:00:00 GMT", 0).is_err());
        assert!(analyze_date("133850880000000000x", 0).is_err());
    }

    #[test]
    fn analyze_resolves_when_only_one_order_is_valid() {
        // 29/02/2024: "month 29" is impossible, so only the PL reading (Feb 29, a leap day).
        assert_eq!(
            analyze_date("29/02/2024", 0).unwrap().readings,
            vec![(DateReading::PlDayMonth, dt(2024, 2, 29, 0, 0, 0))]
        );
        // 02/29/2024: the mirror - US only.
        assert_eq!(
            analyze_date("02/29/2024", 0).unwrap().readings,
            vec![(DateReading::UsMonthDay, dt(2024, 2, 29, 0, 0, 0))]
        );
        // 25/12/2008: "month 25" impossible - PL only.
        assert_eq!(
            analyze_date("25/12/2008", 0).unwrap().readings,
            vec![(DateReading::PlDayMonth, dt(2008, 12, 25, 0, 0, 0))]
        );
    }

    #[test]
    fn analyze_same_date_both_orders_is_not_ambiguous() {
        // 05/05/2008 reads the same either way - one reading, not two.
        let a = analyze_date("05/05/2008", 0).unwrap();
        assert!(!a.is_ambiguous());
        assert_eq!(a.readings, vec![(DateReading::UsMonthDay, dt(2008, 5, 5, 0, 0, 0))]);
    }

    #[test]
    fn analyze_rejects_the_unrepresentable_and_unrecognised() {
        assert!(analyze_date("02/30/2008", 0).is_err()); // Feb 30 in neither order
        assert!(analyze_date("13/13/2008", 0).is_err()); // month 13 in neither order
        assert!(analyze_date("", 0).is_err());
        assert!(analyze_date("hello", 0).is_err());
        assert!(analyze_date("04/08/08", 0).is_err()); // year not four digits
        assert!(analyze_date("2008/08/04", 0).is_err()); // year-first numeric not recognised yet
    }

    /// R4-N36: the ISO branch took anything with a dash, so `31-12-25` came back as the year 31 and
    /// `-9-05-05` as the year -9 - confident answers to dates nobody wrote in that shape.
    #[test]
    fn an_iso_date_needs_its_year_written_in_full() {
        for shapeless in ["31-12-25", "-9-05-05", "26-01-31T12:00:00", "+2026-01-01", "31-12-2025"] {
            assert!(analyze_date(shapeless, 0).is_err(), "{shapeless} must be refused");
        }
        // Written in full, the same years are read - before 1 CE and in the first century too.
        assert_eq!(analyze_date("-0009-05-05", 0).unwrap().readings[0].1, dt(-9, 5, 5, 0, 0, 0));
        assert_eq!(analyze_date("0031-12-25", 0).unwrap().readings[0].1, dt(31, 12, 25, 0, 0, 0));
        assert_eq!(analyze_date("2026-01-31 13:45:00", 0).unwrap().readings[0].1, dt(2026, 1, 31, 13, 45, 0));
    }

    /// R4-N36: 0 and -1 are the same second in both units, and were listed twice under "ambiguous".
    #[test]
    fn one_instant_in_both_units_is_listed_once() {
        for same in ["0", "-1"] {
            let a = analyze_date(same, 0).unwrap();
            assert_eq!(a.readings.len(), 1, "{same}: {a:?}");
            assert!(!a.is_ambiguous(), "{same}");
        }
        // Where the two units part, both are still offered - including the first number past the seam.
        for apart in ["1", "-2", "-7200", "1740607200"] {
            assert_eq!(analyze_date(apart, 0).unwrap().readings.len(), 2, "{apart}");
        }
    }

    /// `eval` is public API over a struct with public fields, so "the caller validated it" is an
    /// assumption, not a fact. It checked the year and nothing else - and below that check the month
    /// indexes month-name tables and feeds `(month - 1)` arithmetic with no answer for 0.
    ///
    /// Unreachable through every surface this build ships (all of them parse the base), which is why
    /// this is a low-priority fix and not a bug report. It is fixed because the doc comment on `eval`
    /// already promised it, and because a second consumer of this crate is likely on a decade horizon.
    #[test]
    fn eval_refuses_a_base_that_is_not_a_real_date() {
        let with_base = |c: CivilDateTime| {
            eval(
                &MomentExpr { base: Base::Absolute(c), steps: vec![Step::Snap(SnapTarget::StartOfQuarter)] },
                &EvalContext { now: dt(2026, 1, 1, 0, 0, 0), zone_bias_min: 0, calendar: None },
            )
        };

        // Month 0: `(month - 1) / 3` in the quarter arithmetic has no answer for it.
        assert_eq!(with_base(dt(2026, 0, 15, 0, 0, 0)), Err(EvalError::BaseNotACivilDate));
        // Month 13: indexes past the end of the month-name tables.
        assert_eq!(with_base(dt(2026, 13, 15, 0, 0, 0)), Err(EvalError::BaseNotACivilDate));
        // A day that month cannot have, and the leap-year case that makes it a real rule.
        assert_eq!(with_base(dt(2026, 4, 31, 0, 0, 0)), Err(EvalError::BaseNotACivilDate));
        assert_eq!(with_base(dt(2026, 2, 29, 0, 0, 0)), Err(EvalError::BaseNotACivilDate));
        assert_eq!(with_base(dt(2026, 1, 0, 0, 0, 0)), Err(EvalError::BaseNotACivilDate));

        // February 29 in a LEAP year is a real date and still passes - the check is about impossible
        // dates, not about unusual ones.
        assert!(with_base(dt(2024, 2, 29, 0, 0, 0)).is_ok());
        assert!(with_base(dt(2026, 12, 31, 0, 0, 0)).is_ok());
    }

    /// The time of day of a base is checked too, and a UTC base is checked BEFORE its conversion to
    /// the session zone (R4-N42). The conversion is arithmetic on the fields, so month 13 came out of it
    /// as January of the next year, and an hour of 99 went into every rendered format as it was.
    #[test]
    fn eval_refuses_a_base_whose_time_or_utc_fields_are_not_real() {
        let run = |base: Base| {
            eval(
                &MomentExpr { base, steps: vec![] },
                &EvalContext { now: dt(2026, 1, 1, 0, 0, 0), zone_bias_min: -120, calendar: None },
            )
        };
        for bad in [dt(2026, 6, 15, 24, 0, 0), dt(2026, 6, 15, 99, 0, 0), dt(2026, 6, 15, 12, 60, 0), dt(2026, 6, 15, 12, 0, 60)] {
            assert_eq!(run(Base::Absolute(bad)), Err(EvalError::BaseNotACivilDate), "{bad:?}");
            assert_eq!(run(Base::AbsoluteUtc(bad)), Err(EvalError::BaseNotACivilDate), "{bad:?}");
        }
        assert_eq!(run(Base::AbsoluteUtc(dt(2026, 13, 15, 0, 0, 0))), Err(EvalError::BaseNotACivilDate));
        assert_eq!(run(Base::AbsoluteUtc(dt(2026, 2, 30, 0, 0, 0))), Err(EvalError::BaseNotACivilDate));
        // The last second of a day is a real moment on both bases.
        assert!(run(Base::Absolute(dt(2026, 6, 15, 23, 59, 59))).is_ok());
        assert!(run(Base::AbsoluteUtc(dt(2026, 6, 15, 23, 59, 59))).is_ok());
    }

    /// Past the business-day limit the refusal names the limit, not an overflow (R4-N35). One past it
    /// is refused, the limit itself is not refused for being too many.
    #[test]
    fn a_business_day_step_past_the_limit_names_the_limit() {
        use crate::calendar::{Calendar, Observed, MAX_BUSINESS_DAYS};
        let cal =
            Calendar { id: "t".into(), country: "US".into(), weekend: vec![0, 6], observed: Observed::None, valid_from: None, law_as_of: None, holidays: vec![] };
        let step = |amount: i64| {
            let expr = MomentExpr {
                base: abs(dt(2026, 7, 6, 0, 0, 0)),
                steps: vec![Step::Shift { sign: Sign::Plus, amount, unit: Unit::BusinessDays }],
            };
            eval(&expr, &EvalContext { now: dt(2000, 1, 1, 0, 0, 0), zone_bias_min: 0, calendar: Some(&cal) })
        };
        assert_eq!(step(MAX_BUSINESS_DAYS + 1), Err(EvalError::TooManyBusinessDays { index: 0 }));
        assert_ne!(step(MAX_BUSINESS_DAYS), Err(EvalError::TooManyBusinessDays { index: 0 }));
    }

    /// The limit stands on the whole evaluation, not on each step (R5-B2): steps that are each under it
    /// are refused at the one that takes the sum past it, before that one walks. Without a calendar the
    /// first business-day step is refused as needing one, as before - that says more.
    #[test]
    fn business_day_steps_share_one_limit_for_the_evaluation() {
        use crate::calendar::{Calendar, Observed, MAX_BUSINESS_DAYS};
        let cal =
            Calendar { id: "t".into(), country: "US".into(), weekend: vec![0, 6], observed: Observed::None, valid_from: None, law_as_of: None, holidays: vec![] };
        let half = MAX_BUSINESS_DAYS / 2 + 1;
        let walk = |signs: &[Sign], calendar: Option<&Calendar>| {
            let steps = signs.iter().map(|&sign| Step::Shift { sign, amount: half, unit: Unit::BusinessDays }).collect();
            eval(&MomentExpr { base: abs(dt(2026, 7, 6, 0, 0, 0)), steps }, &EvalContext { now: dt(2000, 1, 1, 0, 0, 0), zone_bias_min: 0, calendar })
        };
        // Forth and back counts both ways: the walk is the cost, not where it ends.
        assert_eq!(walk(&[Sign::Plus, Sign::Minus], Some(&cal)), Err(EvalError::TooManyBusinessDays { index: 1 }));
        assert!(walk(&[Sign::Plus], Some(&cal)).is_ok(), "one step under the limit is fine");
        assert_eq!(walk(&[Sign::Plus, Sign::Minus], None), Err(EvalError::NeedsCalendar { index: 0 }));
    }

    /// A number at either end of i64, read as an epoch in any zone, is refused as out of range - the
    /// same answer it got in UTC - and never panics (R4-W8: `9223372036854775807` on a UTC+2 host was
    /// "attempt to subtract with overflow", exit 101).
    #[test]
    fn an_epoch_at_the_ends_of_i64_is_refused_in_every_zone() {
        for n in ["9223372036854775807", "-9223372036854775808"] {
            for bias in [-899, -120, 0, 300, 899] {
                assert!(epoch_readings(n, bias).is_none(), "{n} read at bias {bias}");
            }
        }
        // And an ordinary epoch still reads, in both units, in a zone east of UTC.
        let readings = epoch_readings("1000000000", -120).expect("a real epoch");
        assert_eq!(readings.len(), 2);
    }

    /// A number that cannot be a date gets a refusal about its RANGE, not about its format.
    ///
    /// It used to fall through every branch and arrive at "unrecognised date format", a sentence
    /// about shape - on input whose shape was read perfectly. Someone pasting a microsecond timestamp
    /// (the ordinary way to reach this) was sent to look at their formatting.
    #[test]
    fn a_number_that_is_no_date_is_refused_for_its_range_not_its_format() {
        // Too large for i64 at all: there is no instant behind it to reach.
        let huge = analyze_date("99999999999999999999", 0).expect_err("not an instant");
        assert!(huge.contains("reads as a number"), "got: {huge}");
        assert!(!huge.contains("unrecognised date format"), "the shape was never in doubt: {huge}");

        // Inside i64 and still no date: both readings land outside the computable year band.
        let far = analyze_date("999999999999999999", 0).expect_err("not a date this build computes on");
        assert!(far.contains("reads as a number"), "got: {far}");

        // The shape complaint stays for input that really is shapeless.
        let shapeless = analyze_date("hello", 0).expect_err("not a date");
        assert!(shapeless.contains("unrecognised date format"), "got: {shapeless}");
    }

    // --- custom format mask --------------------------------------------------

    #[test]
    fn format_mask_common_patterns() {
        let d = dt(2008, 8, 4, 23, 59, 9);
        assert_eq!(mask_text(&d, "yyyy-MM-dd HH:mm:ss"), "2008-08-04 23:59:09");
        assert_eq!(mask_text(&d, "MM/dd/yyyy"), "08/04/2008");
        assert_eq!(mask_text(&d, "dd.MM.yyyy"), "04.08.2008");
        assert_eq!(mask_text(&d, "yy"), "08");
    }

    #[test]
    fn format_mask_is_case_sensitive_month_vs_minute() {
        // M is month, m is minute - the classic gotcha.
        let d = dt(2008, 3, 4, 12, 7, 0);
        assert_eq!(mask_text(&d, "M m"), "3 7"); // month 3, minute 7 (no pad)
        assert_eq!(mask_text(&d, "MM mm"), "03 07");
    }

    #[test]
    fn format_mask_names_and_no_pad() {
        let d = dt(2008, 8, 4, 5, 6, 7); // 2008-08-04 was a Monday
        assert_eq!(mask_text(&d, "ddd, dd MMM yyyy"), "Mon, 04 Aug 2008");
        assert_eq!(mask_text(&d, "dddd MMMM"), "Monday August");
        assert_eq!(mask_text(&d, "d/M/yyyy H:m:s"), "4/8/2008 5:6:7"); // single = no pad
    }

    #[test]
    fn format_mask_literals_and_unknown_runs_pass_through() {
        let d = dt(2008, 8, 4, 0, 0, 0);
        // A single 'y' and non-token characters are literal - a 3-run 'yyy' is not a token, so verbatim.
        assert_eq!(mask_text(&d, "year=yyyy"), "year=2008");
        assert_eq!(mask_text(&d, "yyy"), "yyy");
        assert_eq!(mask_text(&d, "yyyy//MM"), "2008//08");
        assert_eq!(mask_text(&d, ""), "");
    }

    #[test]
    fn format_mask_single_token_letters_are_still_tokens() {
        // A lone token letter is a token even inside a would-be word, so a literal 's' (seconds)
        // needs quoting. Deliberate, not a bug - and quoting now exists to say so.
        assert_eq!(mask_text(&dt(2008, 8, 4, 0, 0, 30), "s"), "30");
    }

    /// R2-C1. The 12-hour clock with a designator is how the US market writes time (docs/02 section
    /// 7) - one of the two MVP markets - and the mask exists to match the target app's exact output.
    /// `h:mm tt` used to render `h:05 tt`: raw mask letters in a string shaped like a formatted time.
    #[test]
    fn format_mask_has_a_twelve_hour_clock_and_a_designator() {
        // The measured case from the audit, now rendering as .NET would.
        let morning = dt(2026, 3, 7, 9, 5, 3);
        assert_eq!(mask_text(&morning, "M/d/yyyy h:mm tt"), "3/7/2026 9:05 AM");

        // Midnight and noon are 12, not 0 - the rule most home-made conversions get wrong.
        assert_eq!(mask_text(&dt(2026, 3, 7, 0, 30, 0), "h:mm tt"), "12:30 AM");
        assert_eq!(mask_text(&dt(2026, 3, 7, 12, 30, 0), "h:mm tt"), "12:30 PM");
        assert_eq!(mask_text(&dt(2026, 3, 7, 23, 0, 0), "hh:mm tt"), "11:00 PM");
        assert_eq!(mask_text(&dt(2026, 3, 7, 1, 0, 0), "hh:mm t"), "01:00 A");

        // `H` stays the 24-hour clock beside it - the case difference is the whole point.
        assert_eq!(mask_text(&dt(2026, 3, 7, 23, 0, 0), "H h"), "23 11");
    }

    /// A mask the vocabulary does not fully understand must SAY so. Passing letters through silently
    /// produces a string that looks like a result, and the reader blames their own typo (rule 6).
    #[test]
    fn format_mask_reports_letter_runs_it_does_not_know() {
        let d = dt(2008, 8, 4, 13, 5, 0);

        // Punctuation is never reported - a mask is made of it, and naming it would bury the finding.
        assert!(format_with_mask(&d, "yyyy-MM-dd HH:mm:ss").unknown.is_empty());
        assert!(format_with_mask(&d, "yyyy//MM").unknown.is_empty());

        // Letters that are not tokens are reported once each, in the order they first appear.
        let r = format_with_mask(&d, "year=yyyy");
        assert_eq!(r.text, "year=2008", "the text still renders - this is a warning, not a refusal");
        assert_eq!(r.unknown, vec!["y".to_string(), "e".into(), "a".into(), "r".into()]);

        // A repeat is listed once, not once per occurrence.
        assert_eq!(format_with_mask(&d, "zz-zz").unknown, vec!["zz".to_string()]);

        // A run of a token letter at an unknown LENGTH is unknown too: `yyy` is not a year.
        assert_eq!(format_with_mask(&d, "yyy").unknown, vec!["yyy".to_string()]);
    }

    /// Quoting is the answer the warning above points at, so it has to exist and work.
    #[test]
    fn format_mask_quotes_literal_text() {
        let d = dt(2008, 8, 4, 13, 5, 0);

        // The letters inside quotes are text, and nothing is reported.
        let r = format_with_mask(&d, "'year=' yyyy");
        assert_eq!(r.text, "year= 2008");
        assert!(r.unknown.is_empty(), "quoted text is not an unknown token: {:?}", r.unknown);

        // Without quotes the same word is eaten by tokens - `d` becomes the day and `t` the PM
        // designator, so "date=" renders "4aPe=". This is exactly why quoting had to come with the
        // new `t` token: adding a token silently changes what every unquoted mask means.
        assert_eq!(mask_text(&d, "date=yyyy"), "4aPe=2008");
        assert_eq!(mask_text(&d, "'date='yyyy"), "date=2008");

        // Doubled quotes are one apostrophe.
        assert_eq!(mask_text(&d, "''"), "'");
        assert_eq!(mask_text(&d, "'o''clock'"), "o'clock");

        // An unterminated quote takes the rest and is REPORTED - rendering it silently would produce
        // something other than what was written, with no way to notice.
        let open = format_with_mask(&d, "yyyy 'text");
        assert_eq!(open.text, "2008 text");
        assert_eq!(open.unknown, vec!["'".to_string()]);
    }

    /// Random properties of the evaluation - the engine every `--shift`, `--snap`, `--nearest` and
    /// `--to-zone` a command line can write goes through, from any base the calculator accepts.
    mod properties {
        use super::*;
        use crate::search::{calendar, edge_i64, within, year};
        use proptest::prelude::*;
        use std::time::Duration;

        /// A moment that is one, in any year of the band.
        fn moment() -> impl Strategy<Value = CivilDateTime> {
            (year(), 1u32..=12, 1u32..=28, 0u32..=23, 0u32..=59, 0u32..=59)
                .prop_map(|(y, mo, d, h, mi, s)| dt(y, mo, d, h, mi, s))
        }

        /// Fields that may name no moment at all - month 13, day 32, hour 24.
        fn fields() -> impl Strategy<Value = CivilDateTime> {
            (year(), 0u32..=13, 0u32..=32, 0u32..=24, 0u32..=60, 0u32..=60)
                .prop_map(|(y, mo, d, h, mi, s)| dt(y, mo, d, h, mi, s))
        }

        /// A base as the calculator may be handed one - mostly a moment, sometimes mere fields.
        fn base() -> impl Strategy<Value = Base> {
            prop_oneof![
                1 => Just(Base::Today),
                1 => Just(Base::Now),
                4 => moment().prop_map(Base::Absolute),
                2 => moment().prop_map(Base::AbsoluteUtc),
                2 => fields().prop_map(Base::Absolute),
                1 => fields().prop_map(Base::AbsoluteUtc),
            ]
        }

        /// A zone offset in minutes: mostly one `--zone` accepts, sometimes any 32-bit value.
        fn bias() -> impl Strategy<Value = i32> {
            prop_oneof![
                6 => -(14 * 60 + 59)..=(14 * 60 + 59),
                1 => Just(i32::MAX),
                1 => Just(i32::MIN),
                1 => any::<i32>(),
            ]
        }

        fn step() -> impl Strategy<Value = Step> {
            let sign = prop_oneof![Just(Sign::Plus), Just(Sign::Minus)];
            let unit = prop_oneof![
                Just(Unit::Seconds),
                Just(Unit::Minutes),
                Just(Unit::Hours),
                Just(Unit::Days),
                Just(Unit::Weeks),
                Just(Unit::Months),
                Just(Unit::Quarters),
                Just(Unit::Years),
                Just(Unit::BusinessDays),
            ];
            let snap = prop_oneof![
                Just(SnapTarget::StartOfMonth),
                Just(SnapTarget::EndOfMonth),
                Just(SnapTarget::StartOfQuarter),
                Just(SnapTarget::EndOfQuarter),
                Just(SnapTarget::StartOfYear),
                Just(SnapTarget::EndOfYear),
            ];
            let nearest = prop_oneof![
                Just(NearestTarget::NextBusinessDay),
                Just(NearestTarget::PrevBusinessDay),
                Just(NearestTarget::NextLeapDay),
            ];
            prop_oneof![
                5 => (sign, edge_i64(), unit).prop_map(|(sign, amount, unit)| Step::Shift { sign, amount, unit }),
                1 => (0u32..=25, 0u32..=61, 0u32..=61)
                    .prop_map(|(hour, minute, second)| Step::SetTime { hour, minute, second }),
                1 => snap.prop_map(Step::Snap),
                1 => nearest.prop_map(Step::Nearest),
                1 => bias().prop_map(Step::Zone),
            ]
        }

        proptest! {
            // The shrink is capped in time as well as in steps: a case that hangs costs `within`'s
            // whole limit at every step and leaves its thread spinning (see `within`).
            #![proptest_config(ProptestConfig {
                max_shrink_iters: 256,
                max_shrink_time: 20_000,
                ..ProptestConfig::default()
            })]

            /// R3-1 and P2. Any chain of steps from any base ends in a real moment inside the year
            /// band, one per step, or says why not - and everything the calculator then shows for
            /// that moment is defined for it. `--snap end-of-month` panicked in the metadata, not in
            /// the step, and `--shift +25252734927766000y` in the arithmetic after it.
            #[test]
            fn any_chain_of_steps_ends_in_a_moment_inside_the_band_or_says_why(
                base in base(),
                steps in proptest::collection::vec(step(), 0..6),
                now in moment(),
                zone in bias(),
                cal in proptest::option::of(calendar()),
            ) {
                let count = steps.len();
                let expr = MomentExpr { base, steps };
                let reached = within(Duration::from_secs(5), "eval", move || {
                    let ctx = EvalContext { now, zone_bias_min: zone, calendar: cal.as_ref() };
                    eval(&expr, &ctx).map(|outcome| {
                        let result = outcome.result();
                        // What `chrono calc` prints for a result, each of which must hold for it.
                        let _ = formats(&result, outcome.result_bias);
                        let _ = metadata(&result, &now);
                        let _ = significance(&result, outcome.result_bias, cal.as_ref());
                        let _ = format_with_mask(&result, "yyyy-MM-dd HH:mm:ss dddd");
                        (outcome.base, outcome.after_each)
                    })
                });
                if let Ok((first, after_each)) = reached {
                    prop_assert_eq!(after_each.len(), count);
                    for moment in std::iter::once(&first).chain(after_each.iter()) {
                        prop_assert!(crate::civil_year_in_band(moment.year), "left the band: {:?}", moment);
                        prop_assert!(is_a_civil_moment(moment), "not a moment: {:?}", moment);
                    }
                }
            }
        }
    }
}
