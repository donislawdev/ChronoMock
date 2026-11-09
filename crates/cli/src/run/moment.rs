//! What time the session will run with, decided before anything is spawned.
//!
//! Two sources feed one shape. A named preset carries a moment and a time mode together (docs/04
//! 4.3), the flags carry them separately, and both arms end in the single `TimeSpec` the core is
//! sent. Resolving a preset here rather than in the core is deliberate: the core receives an
//! absolute moment and a plain mode, and never learns that a preset existed.

use chrono_core::calc::{Base, CivilDateTime, EvalContext, EvalError, MomentExpr, Step, Unit};
use chrono_core::Moment;
use chrono_proto::{MomentKind, MomentSpec, TimeSpec};

use super::args::RunArgs;
use crate::calc::{calc_error_exit_code, describe_calc_error, resolve_now_civil};
use crate::cli::print_usage;
use crate::grammar::parse_shift;
use crate::output::{diag, Stream};
use crate::preset::{
    load_for, load_market_calendar, parameter_provenance, read_target_creation_date, PresetRefusal, Surface,
};

/// Where the session's moment and mode came from. The wire carries only the resolved absolute
/// moment, which is right for the core and useless to a reader asking "why that date" - a preset
/// resolves against the clock and, for a trial, against the target's own file date.
///
/// Produced here rather than worked out again by whoever renders it: a second resolution could
/// disagree with the one that was actually sent, which is the drift untouchable rule 4 is about.
pub(super) enum TimeOrigin {
    /// `--at`, as the user wrote it - absolute, or relative and resolved before anything is spawned.
    At(String),
    /// Neither `--at` nor `--preset`: the session clock starts at the real current time.
    Now,
    /// `--preset <id>`, with what each declared parameter resolved to and where that value came from.
    Preset { id: String, parameters: Vec<(String, String, String)> },
}

/// Everything decided before the spawn: the wire shape, and where it came from.
pub(super) struct ResolvedTime {
    pub(super) spec: TimeSpec,
    pub(super) origin: TimeOrigin,
}

/// What the two arms below agree on, before it becomes a `TimeSpec`. A named shape rather than a
/// six-value tuple, for the same reason the return type is one: a positional list of loose values is
/// where a mode and a moment quietly swap places.
struct Decided {
    at: Option<String>,
    mode: chrono_proto::TimeModeKind,
    multiplier: Option<i64>,
    scale_duration: bool,
    session_bias: Option<i32>,
    origin: TimeOrigin,
}

/// The moment and the time mode the session will run with, as the one `TimeSpec` that goes on the
/// wire, plus where they came from. The error side carries the exit code rather than a message,
/// because every failure on this path is a usage or catalogue error and not a substitution verdict -
/// the text is printed next to the branch that knows what went wrong.
///
/// Returning the wire shape instead of five loose values is the point. The terminal report, the
/// evidence file and a dry run describe the session by reading this same value, so none of them can
/// describe a mode or a moment other than the one that was sent (untouchable rule 4).
pub(super) fn resolve_time_spec(ra: &RunArgs, now_bias: i32) -> Result<ResolvedTime, i32> {
    if let Err(e) = check_schedule(ra, now_bias) {
        diag!("chrono: {e}");
        return Err(1);
    }
    // The moment AND the time mode come either from a named preset (docs/04 4.3) or from the flags.
    // A preset is resolved driver-side here - the same way a relative --at is - so the core still
    // receives an absolute moment and a plain mode, and never learns that a preset existed.
    let decided = if let Some(pid) = &ra.preset {
        let p = match load_for(pid, Surface::Substitution) {
            Ok(p) => p,
            Err(PresetRefusal::Unreadable(e)) => {
                diag!("chrono: {}", e.message());
                return Err(e.exit_code());
            }
            // A calculator-only preset is not a substitution question. Refused rather than run a moment
            // nobody asked to run.
            Err(PresetRefusal::NotForSurface { id, applies_to }) => {
                diag!("chrono: preset '{id}' targets {applies_to}, not substitution (preset.not_for_substitution)");
                return Err(1);
            }
        };
        // The target file's date, read in the SESSION's zone: a creation time near midnight resolves to a
        // different calendar day in UTC than on the host, and that day is what a trial preset counts from.
        let target_date = read_target_creation_date(&ra.target, Some(now_bias));
        let (values, mut moment) = match p.fill(&ra.params, target_date) {
            Ok(filled) => filled,
            Err((e, _)) => {
                diag!("chrono: {}", e.message());
                return Err(e.exit_code());
            }
        };
        // What the preset was filled with, read while the declarations and the values are both in hand.
        // A dry run prints it, because "which date is this trial counting from" is exactly the question a
        // resolved absolute moment does not answer.
        let provenance = parameter_provenance(&p.parameters, &ra.params, &values);
        back_in_session_zone(&mut moment, now_bias);
        let now = match resolve_now_civil(Some(now_bias)) {
            Ok(n) => n,
            Err(e) => {
                diag!("chrono: cannot resolve current time: {e}");
                return Err(3);
            }
        };
        Decided {
            at: Some(eval_preset_moment(&p.id, p.market.as_deref(), &moment, now, now_bias)?),
            mode: p.time_mode.mode,
            multiplier: p.time_mode.multiplier,
            scale_duration: p.time_mode.scale_duration,
            // The zone the moment was computed in travels with it: a preset moment paired with a bias of
            // 0 would land an offset away from the instant it names.
            session_bias: Some(now_bias),
            origin: TimeOrigin::Preset { id: p.id.clone(), parameters: provenance },
        }
    } else {
        // Resolve a relative --at (now + delta) to an absolute moment before we spawn. With no --at at
        // all, the session clock starts at the real current time - the same thing the Chromium path
        // already does with the same command line. Until now the two mechanisms disagreed: on a native
        // target the empty moment travelled all the way into the core and came back as "moment must be
        // YYYY-MM-DDTHH:MM:SS, got ''" - an error about a flag the usage line calls optional, raised as
        // far from the user's mistake as it could be. It also makes `chrono run app.exe --mode x60`
        // mean what it reads as: run this application faster without moving its date.
        let resolved = match &ra.at {
            Some(raw) => match resolve_at(raw, Some(now_bias)) {
                Ok(s) => Some(s),
                Err(e) => {
                    diag!("chrono: {e}");
                    print_usage(Stream::Err);
                    return Err(1);
                }
            },
            None => match resolve_now_civil(Some(now_bias)) {
                Ok(now) => Some(now.to_iso()),
                Err(e) => {
                    diag!("chrono: {e}");
                    return Err(1);
                }
            },
        };
        // The zone the moment above was read in travels with it: the core turns local + bias into the
        // UTC anchor, so a moment paired with the wrong bias lands an offset away from the instant it
        // names. ONE zone for the session, whatever shape the moment came in (R2-X5). An absolute
        // `--at` used to be the exception - a typed wall-clock string was read as UTC - which made the
        // same string mean two different instants depending on which half of the tool read it, and
        // made the exported evidence say "(host default)" about a session that ran on UTC.
        let session_bias = Some(now_bias);
        Decided {
            at: resolved,
            mode: ra.mode,
            multiplier: ra.multiplier,
            scale_duration: ra.scale_duration,
            session_bias,
            // What the user asked for, not what it became: a dry run that showed only the resolved
            // moment would hide the one thing worth checking about `--at +18y`.
            origin: match &ra.at {
                Some(raw) => TimeOrigin::At(raw.clone()),
                None => TimeOrigin::Now,
            },
        }
    };

    let Decided { at, mode, multiplier, scale_duration, session_bias, origin } = decided;
    // The gate the core puts every start through (R4-S7), here as well, so a dry run refuses what the
    // run would refuse and a run refuses before it starts anything (R4-S12). Measured before this:
    // `--at 1500-01-01T00:00:00` was planned with exit 0 and refused by the core with exit 1, and a
    // relative `--at` or a preset landing past 30828 went the same way. Exit 1 is the core's code.
    if let Some(local) = &at
        && let Err(e) = chrono_core::session_instant(&Moment { local: local.clone(), tz_bias_min: session_bias })
    {
        diag!("chrono: {local} cannot start a session - {e}");
        return Err(1);
    }
    let spec = TimeSpec {
        moment: MomentSpec {
            kind: MomentKind::Absolute,
            local: at,
            tz_bias_min: session_bias,
            delta: None,
        },
        mode,
        multiplier,
        scale_duration,
        scale_qpc: ra.scale_qpc,
    };
    Ok(ResolvedTime { spec, origin })
}

/// A preset's moment against real "now" in the session zone, the way a relative `--at` is resolved,
/// as the wall moment that goes on the wire. A step that counts business days gets the calendar of
/// the preset's market - the one the panel's scenario list counts in - loaded only when a step asks
/// for it, so a market preset that counts none does not start depending on the calendar files
/// (R4-S12). Without a market calendar the refusal names what is missing without pointing at a
/// `--calendar` that `chrono run` does not have. Errors are printed here, and the exit code returned.
fn eval_preset_moment(
    id: &str,
    market: Option<&str>,
    moment: &MomentExpr,
    now: CivilDateTime,
    now_bias: i32,
) -> Result<String, i32> {
    let outcome = match chrono_core::calc::eval(moment, &EvalContext { now, zone_bias_min: now_bias, calendar: None }) {
        Err(needs @ EvalError::NeedsCalendar { .. }) => match load_market_calendar(id, market) {
            Some(Ok(calendar)) => {
                let counted = chrono_core::calc::eval(
                    moment,
                    &EvalContext { now, zone_bias_min: now_bias, calendar: Some(&calendar) },
                );
                // The calculator marks this `calendar_outdated`. A session has no marks to carry it, so
                // the line goes where the caller reads the rest of the start (R4/20).
                if let Ok(out) = &counted
                    && out.calendar_reach.is_some_and(|day| chrono_core::calendar::outdated(&calendar, &now, &day))
                {
                    diag!(
                        "chrono: preset '{id}' counted business days in the {} calendar, whose holidays were checked against the law more than a year ago - a holiday added since may be missing (calendar_outdated)",
                        calendar.id
                    );
                }
                counted
            }
            Some(Err(e)) => {
                diag!("chrono: {e}");
                return Err(1);
            }
            None => Err(needs),
        },
        first => first,
    };
    match outcome {
        Ok(outcome) => Ok(outcome.result().to_iso()),
        Err(e @ EvalError::NeedsCalendar { .. }) => {
            diag!(
                "chrono: preset '{id}' counts business days, and its market names no calendar to count them in - chrono run takes the calendar from the preset's market (calc.needs_calendar)"
            );
            Err(calc_error_exit_code(&e))
        }
        // The calculator's sentence ends in "pick another calendar", which `chrono run` cannot do - it
        // takes the market's. What a session can do instead is start at a moment given directly.
        Err(e @ EvalError::BeforeCalendar { first_year, .. }) => {
            diag!(
                "chrono: preset '{id}' counts business days back past {first_year}, the first year its market's calendar covers - start the session with --at and a date instead (calc.before_calendar)"
            );
            Err(calc_error_exit_code(&e))
        }
        Err(e) => {
            diag!("chrono: preset '{id}' moment: {}", describe_calc_error(&e));
            Err(calc_error_exit_code(&e))
        }
    }
}

/// A preset's `zone` step expresses its moment in another zone, and the result used to go on the wire
/// as that zone's wall clock paired with the SESSION's bias - an instant the zones' difference away
/// from the one the preset names (R4-S12). The session runs in one zone, `--zone` or the host's (rule
/// 2), so the same instant is brought back into it by one more `zone` step. A preset without a zone
/// step is left exactly as it was. The panel's scenario list does the same (`ScenarioMoment`).
fn back_in_session_zone(moment: &mut MomentExpr, session_bias: i32) {
    if moment.steps.iter().any(|s| matches!(s, Step::Zone(_))) {
        moment.steps.push(Step::Zone(session_bias));
    }
}

/// What `--set-after` and `--jump-after` promise, checked before anything starts, so a plan never
/// describes a change the session would not make (R4-N21, R4-S12). Heartbeats count from 1, and a
/// session cut by `--ticks N` ends after heartbeat N, so a change scheduled at 0 or past the cut never
/// happened - without a word, while `--dry-run` promised it "at heartbeat 0". The jump's moment gets
/// every check the core would give it at that heartbeat that does not depend on where the session
/// clock will stand by then: the grammar, business days (which need a calendar a session does not
/// have), and for an absolute moment the range of the session clock.
fn check_schedule(ra: &RunArgs, session_bias: i32) -> Result<(), String> {
    let changes = [
        ("--set-after", ra.set_after.map(|(tick, _)| tick)),
        ("--jump-after", ra.jump_after.as_ref().map(|(tick, _)| *tick)),
    ];
    for (flag, tick) in changes {
        match tick {
            Some(0) => {
                return Err(format!("{flag} counts state heartbeats from 1, so a change at heartbeat 0 would never happen"));
            }
            Some(t) if ra.ticks > 0 && t > ra.ticks => {
                return Err(format!(
                    "{flag} at heartbeat {t} would never happen - --ticks {} ends the session after heartbeat {}",
                    ra.ticks, ra.ticks
                ));
            }
            _ => {}
        }
    }
    let Some((_, moment)) = &ra.jump_after else {
        return Ok(());
    };
    if moment.starts_with(['+', '-']) {
        return match parse_shift(moment) {
            Ok(Step::Shift { unit: Unit::BusinessDays, .. }) => Err(format!(
                "--jump-after {moment} counts business days, which need a calendar that a session does not have"
            )),
            Ok(_) => Ok(()),
            Err(e) => Err(format!("--jump-after: {e}")),
        };
    }
    chrono_core::session_instant(&Moment { local: moment.clone(), tz_bias_min: Some(session_bias) })
        .map(|_| ())
        .map_err(|e| format!("--jump-after {moment} - {e}"))
}

/// Resolve the `--at` value to an absolute wall string (the core only ever sees an
/// absolute moment). A leading `+`/`-` marks a relative moment - now plus one shift
/// step - resolved through the SHARED calc evaluator, so `--at` accepts exactly the
/// units the calculator does, including months, quarters, and years, which fold onto
/// the civil date (a fixed-tick delta cannot express them). Anything else is an absolute
/// moment, checked here by the parser the core itself uses.
///
/// One grammar, not two: `--at`, `jump`, and the calculator all resolve through the same
/// step evaluator. The old `parse_relative_delta` (fixed-tick only) is gone entirely.
pub(crate) fn resolve_at(raw: &str, tz_bias_min: Option<i32>) -> Result<String, String> {
    // `--at` takes the next word whatever it is, so `--at --dry-run` handed the flag over as the
    // moment, and its leading '-' made that a relative one: the refusal then spoke of a "shift"
    // nobody had written. No moment starts with two dashes, and none is spelled like the help flag
    // (`-h` has no number), so the word is named as what it is.
    if raw.starts_with("--") || crate::cli::is_help_flag(raw) {
        return Err(format!(
            "--at needs a moment after it, but the next word is the flag '{raw}' - write --at YYYY-MM-DDTHH:MM:SS, or a relative +N<unit>"
        ));
    }
    if raw.starts_with(['+', '-']) {
        let now = resolve_now_civil(tz_bias_min)?;
        resolve_relative_at(raw, now)
    } else {
        // The core parses this same string with this same function before it starts anything, and
        // refused 2038-13-45 there - AFTER the driver had already let a dry run approve it with exit
        // 0 and a plan naming that moment. Docs/08 section 8 says a dry run's 0 means "this plan is
        // sound", so a moment the run would refuse is refused here, in the core's own words.
        chrono_core::calc::parse_civil_datetime(raw)?;
        Ok(raw.to_string())
    }
}

/// The pure core of a relative `--at`, taking "now" as data so it is deterministic to
/// test. The caller guarantees `raw` starts with a sign.
pub(crate) fn resolve_relative_at(raw: &str, now: chrono_core::calc::CivilDateTime) -> Result<String, String> {
    let step = parse_shift(raw)?;
    let expr = MomentExpr { base: Base::Now, steps: vec![step] };
    // `--at` builds a single shift step (no `zone` step) and reads back the civil result, so the
    // session-zone bias here only sets the unused result zone - 0 is fine.
    let outcome = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None })
        .map_err(describe_at_error)?;
    Ok(outcome.result().to_iso())
}

/// Message for an eval error while resolving a relative `--at`. A pre-spawn resolution
/// failure is a usage error (the caller exits 1), never a substitution verdict - business
/// days need a calendar, an extreme delta overflows. `--at` only ever builds one shift
/// step, so the step-level variants cannot occur, but the match stays total.
pub(crate) fn describe_at_error(e: EvalError) -> String {
    match e {
        EvalError::NeedsCalendar { .. } => {
            "relative --at uses business days, which need a calendar (not available here)".to_string()
        }
        EvalError::Overflow { .. } | EvalError::BaseOverflow => "relative --at is too large".to_string(),
        EvalError::StepUnsupported { kind, .. } => format!("relative --at step '{kind}' is not supported"),
        EvalError::DegenerateCalendar { .. } => "relative --at found no matching date".to_string(),
        EvalError::BadSetTime { .. } => "relative --at has an invalid time".to_string(),
        EvalError::BaseYearOutOfRange | EvalError::YearOutOfRange { .. } => format!(
            "relative --at lands outside the year range this build computes on ({}..={})",
            chrono_core::CIVIL_YEAR_MIN,
            chrono_core::CIVIL_YEAR_MAX
        ),
        // This path builds its base from the real clock, so it cannot produce an impossible date.
        // Named anyway, because the match stays total - see the note above.
        EvalError::BaseNotACivilDate => "relative --at has an impossible base date".to_string(),
        // Business days already need a calendar here, which this path has none of, so the limit behind
        // them cannot be reached either. Named for the same reason as the arm above.
        EvalError::TooManyBusinessDays { .. } => format!(
            "relative --at asks for more than {} business days",
            chrono_core::calendar::MAX_BUSINESS_DAYS
        ),
        // Only a calendar can refuse a walk for its first year, and this path has none. Named for the
        // same reason as the two arms above.
        EvalError::BeforeCalendar { first_year, .. } => {
            format!("relative --at reaches a day before {first_year}, the first year of the calendar")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::args::parse_run_args;

    #[test]
    fn at_absolute_passes_through() {
        assert_eq!(
            resolve_at("2038-01-19T03:14:07", Some(0)).unwrap(),
            "2038-01-19T03:14:07"
        );
    }

    /// A moment the core would refuse is refused here, before anything starts, and in the core's own
    /// words - the three impossible fields a dry run used to approve with exit 0.
    #[test]
    fn an_absolute_at_the_core_would_refuse_is_refused_before_anything_starts() {
        for (raw, words) in [
            ("2038-13-45T00:00:00", "month 13 out of range"),
            ("2030-02-30T00:00:00", "day 30 out of range for month 2"),
            ("2030-02-28T25:61:00", "hour 25 out of range"),
        ] {
            let e = resolve_at(raw, Some(0)).expect_err(raw);
            assert!(e.contains(words), "{raw}: {e}");
        }
        // The space the core accepts in place of the T stays accepted - one parser, one answer.
        assert_eq!(resolve_at("2038-01-19 03:14:07", Some(0)).unwrap(), "2038-01-19 03:14:07");
    }

    /// `--at --dry-run` took the flag as the moment and answered with a sentence about a "shift", and
    /// so did `--at -h`. A relative moment with a number stays one.
    #[test]
    fn a_flag_where_the_moment_should_be_is_named_as_a_flag() {
        for flag in ["--dry-run", "-h", "--help"] {
            let e = resolve_at(flag, Some(0)).unwrap_err();
            assert!(e.contains("--at needs a moment") && e.contains(&format!("'{flag}'")), "{flag}: {e}");
            assert!(!e.contains("shift"), "{flag}: {e}");
        }
        assert!(resolve_at("-5h", Some(0)).is_ok(), "five hours back is a moment");
    }

    #[test]
    fn at_relative_resolves_to_absolute_wall() {
        // Value is now-dependent, but a valid delta must produce a wall string.
        let s = resolve_at("+1d", Some(0)).unwrap();
        assert!(s.contains('T') && s.len() == 19, "unexpected wall string: {s}");
    }

    #[test]
    fn at_relative_rejects_bad_unit_and_number() {
        assert!(resolve_at("+1x", None).is_err());
        assert!(resolve_at("+abcd", None).is_err());
        assert!(resolve_at("-y", None).is_err());
    }

    #[test]
    fn at_relative_fixed_unit_is_deterministic_with_now() {
        // Fixed-length units resolve exactly as before - now + a plain offset. Deterministic
        // because now is passed as data, not read from the clock.
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 25, hour: 14, minute: 30, second: 45 };
        assert_eq!(resolve_relative_at("+1d", now).unwrap(), "2026-08-26T14:30:45");
        assert_eq!(resolve_relative_at("-2h", now).unwrap(), "2026-08-25T12:30:45");
        assert_eq!(resolve_relative_at("+1w", now).unwrap(), "2026-09-01T14:30:45");
    }

    #[test]
    fn at_relative_now_accepts_calendar_units() {
        // The new capability: `--at` gains months/quarters/years through the shared model,
        // with the same clamp - the substitution side could not express these before.
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 25, hour: 0, minute: 0, second: 0 };
        assert_eq!(resolve_relative_at("+1mo", now).unwrap(), "2026-09-25T00:00:00");
        assert_eq!(resolve_relative_at("-18years", now).unwrap(), "2008-08-25T00:00:00");
        // End-of-month clamp reaches `--at` too: Jan 31 + 1 month = Feb 28 (2027, non-leap).
        let jan31 = chrono_core::calc::CivilDateTime { year: 2027, month: 1, day: 31, hour: 12, minute: 0, second: 0 };
        assert_eq!(resolve_relative_at("+1mo", jan31).unwrap(), "2027-02-28T12:00:00");
    }

    #[test]
    fn at_relative_business_days_need_a_calendar() {
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 25, hour: 0, minute: 0, second: 0 };
        let err = resolve_relative_at("+5bd", now).unwrap_err();
        assert!(err.contains("calendar"), "honest needs-a-calendar message, got: {err}");
    }

    /// Untouchable rule 2 on the driver's own surface: an absolute `--at` names a wall-clock moment
    /// in the SESSION's zone, and the bias it was read in has to travel with it. Sending the string
    /// with no bias makes the same text mean two different instants depending on which half of the
    /// tool reads it (R2-X5). Neither disk nor clock is touched here, so it is deterministic.
    #[test]
    fn an_absolute_at_carries_the_session_zone_onto_the_wire() {
        let ra = parse_run_args(&["app.exe".into(), "--at".into(), "2038-01-19T03:14:07".into()]).unwrap();
        let spec = resolve_time_spec(&ra, 120).expect("an absolute moment needs neither disk nor clock").spec;
        assert_eq!(spec.moment.kind, MomentKind::Absolute);
        assert_eq!(spec.moment.local.as_deref(), Some("2038-01-19T03:14:07"));
        assert_eq!(spec.moment.tz_bias_min, Some(120), "the session zone must travel with the moment");
        assert_eq!(spec.moment.delta, None);
    }

    /// The mode flags reach the wire unchanged, and `--scale-qpc` composes with them rather than
    /// replacing one. The report describes the session by reading this same value, so a drift here
    /// would be a report describing a session that did not run (untouchable rule 4).
    #[test]
    fn the_mode_flags_reach_the_wire_unchanged() {
        let ra = parse_run_args(&[
            "app.exe".into(),
            "--at".into(),
            "2026-01-01T00:00:00".into(),
            "--mode".into(),
            "x60".into(),
            "--scale-duration".into(),
            "--scale-qpc".into(),
        ])
        .unwrap();
        let spec = resolve_time_spec(&ra, 0).unwrap().spec;
        assert_eq!(spec.mode, chrono_proto::TimeModeKind::Multiplier);
        assert_eq!(spec.multiplier, Some(60));
        assert!(spec.scale_duration);
        assert!(spec.scale_qpc);

        let plain = parse_run_args(&["app.exe".into(), "--at".into(), "2026-01-01T00:00:00".into()]).unwrap();
        let spec = resolve_time_spec(&plain, 0).unwrap().spec;
        assert_eq!(spec.mode, chrono_proto::TimeModeKind::Flow);
        assert_eq!(spec.multiplier, None);
        assert!(!spec.scale_duration);
        assert!(!spec.scale_qpc);
    }

    /// A preset's zone step changes where a later step counts, not the instant that goes on the wire
    /// (R4-S12). "The start of the month in +05:45" is 2029-12-31T18:15 in UTC. Without the step back
    /// into the session zone the wire carried 2030-01-01T00:00 - the Kathmandu wall clock - which the
    /// session then read as UTC, 5 h 45 min away from the preset's moment.
    #[test]
    fn a_zone_step_in_a_preset_keeps_its_instant_in_the_session_zone() {
        use chrono_core::calc::{CivilDateTime, SnapTarget};
        let base = CivilDateTime { year: 2030, month: 1, day: 15, hour: 0, minute: 0, second: 0 };
        let kathmandu = crate::zone::parse_zone_to_bias("+05:45").expect("a zone");
        let mut moment = MomentExpr {
            base: Base::Absolute(base),
            steps: vec![Step::Zone(kathmandu), Step::Snap(SnapTarget::StartOfMonth)],
        };
        back_in_session_zone(&mut moment, 0);
        let outcome = chrono_core::calc::eval(&moment, &EvalContext { now: base, zone_bias_min: 0, calendar: None })
            .expect("the moment evaluates");
        assert_eq!(outcome.result().to_iso(), "2029-12-31T18:15:00");

        let mut plain = MomentExpr { base: Base::Absolute(base), steps: vec![Step::Snap(SnapTarget::StartOfMonth)] };
        back_in_session_zone(&mut plain, 0);
        assert_eq!(plain.steps.len(), 1, "a preset without a zone step is left as it was");
    }

    fn run_args(argv: &[&str]) -> RunArgs {
        let owned: Vec<String> = argv.iter().map(|a| (*a).to_string()).collect();
        parse_run_args(&owned).expect("the command line must parse")
    }

    /// Every moment a session starts at goes through the core's own gate before anything starts, so
    /// a dry run and a run refuse the same moments (R4-S12): below 1601, past the last instant the
    /// fake clock holds, and a local moment that is still 1601 while its UTC instant is not.
    #[test]
    fn a_moment_outside_the_session_range_is_refused_before_anything_starts() {
        for (at, zone) in [
            ("1500-01-01T00:00:00", "+00:00"),
            ("30829-01-01T00:00:00", "+00:00"),
            ("1601-01-01T00:30:00", "+01:00"),
        ] {
            let ra = run_args(&["app.exe", "--at", at, "--zone", zone]);
            let bias = ra.zone_bias_min.expect("the zone was given");
            assert_eq!(resolve_time_spec(&ra, bias).err(), Some(1), "{at} {zone}");
        }
        let ra = run_args(&["app.exe", "--at", "1601-01-01T00:30:00", "--zone", "+00:00"]);
        assert!(resolve_time_spec(&ra, 0).is_ok(), "the first half hour of 1601 in UTC is a moment");
    }

    /// Heartbeats count from 1 and a session cut by --ticks never reaches the heartbeat after the cut,
    /// so a change scheduled at 0 or past the cut is refused rather than silently skipped (R4-N21). A
    /// change on the last heartbeat still happens, and without --ticks there is no cut to be past.
    #[test]
    fn a_change_the_session_would_never_make_is_refused() {
        for argv in [
            &["app.exe", "--set-after", "0:60"][..],
            &["app.exe", "--jump-after", "0:+1d"][..],
            &["app.exe", "--ticks", "3", "--set-after", "4:60"][..],
            &["app.exe", "--ticks", "3", "--jump-after", "4:+1d"][..],
        ] {
            let e = check_schedule(&run_args(argv), 0).expect_err(&argv.join(" "));
            assert!(e.contains("would never happen"), "{argv:?}: {e}");
        }
        for argv in [
            &["app.exe", "--ticks", "3", "--set-after", "3:60"][..],
            &["app.exe", "--ticks", "3", "--jump-after", "3:+1d"][..],
            &["app.exe", "--set-after", "1000000:60"][..],
        ] {
            assert!(check_schedule(&run_args(argv), 0).is_ok(), "{argv:?}");
        }
    }

    /// The jump's moment gets the checks the core would give it that do not depend on where the clock
    /// will stand: the grammar, business days, and the range of an absolute moment in the session zone.
    #[test]
    fn a_jump_the_core_would_refuse_is_refused_before_anything_starts() {
        for (moment, words) in [
            ("+5bd", "business days"),
            ("+1x", "--jump-after"),
            ("tomorrow", "--jump-after tomorrow"),
            ("1500-01-01T00:00:00", "outside the range"),
            ("2030-02-30T00:00:00", "--jump-after 2030-02-30T00:00:00"),
        ] {
            let flag = format!("2:{moment}");
            let e = check_schedule(&run_args(&["app.exe", "--jump-after", &flag]), 0).expect_err(moment);
            assert!(e.contains(words), "{moment}: {e}");
        }
        for moment in ["+1d", "-3650d", "2038-01-19T03:14:07"] {
            let flag = format!("2:{moment}");
            assert!(check_schedule(&run_args(&["app.exe", "--jump-after", &flag]), 0).is_ok(), "{moment}");
        }
    }
}
