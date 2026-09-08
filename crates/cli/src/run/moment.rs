//! What time the session will run with, decided before anything is spawned.
//!
//! Two sources feed one shape. A named preset carries a moment and a time mode together (docs/04
//! 4.3), the flags carry them separately, and both arms end in the single `TimeSpec` the core is
//! sent. Resolving a preset here rather than in the core is deliberate: the core receives an
//! absolute moment and a plain mode, and never learns that a preset existed.

use chrono_core::calc::{Base, EvalContext, EvalError, MomentExpr};
use chrono_proto::{MomentSpec, TimeSpec};

use super::args::RunArgs;
use crate::calc::{calc_error_exit_code, describe_calc_error, resolve_now_civil};
use crate::cli::print_usage;
use crate::grammar::parse_shift;
use crate::preset::{
    load_preset, parameter_provenance, preset_targets_substitution, read_target_creation_date,
    resolve_moment, resolve_parameters,
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
    mode: String,
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
    // The moment AND the time mode come either from a named preset (docs/04 4.3) or from the flags.
    // A preset is resolved driver-side here - the same way a relative --at is - so the core still
    // receives an absolute moment and a plain mode, and never learns that a preset existed.
    let decided = if let Some(pid) = &ra.preset {
        match load_preset(pid) {
            Ok(p) => {
                // The substitution surface honours applies_to: a calculator-only preset is not a
                // substitution question. Refuse it rather than run a moment nobody asked to run.
                if !preset_targets_substitution(&p.applies_to) {
                    eprintln!(
                        "chrono: preset '{}' targets {}, not substitution (preset.not_for_substitution)",
                        p.id, p.applies_to
                    );
                    return Err(1);
                }
                // Resolve parameters (--param, then the target's file date for a
                // target_file_creation hint), then substitute them into the moment. A non-parametric
                // preset resolves to an empty map and an unchanged moment.
                // The target file's date, read in the SESSION's zone: a creation time near midnight
                // resolves to a different calendar day in UTC than on the host, and that day is what a
                // trial preset counts from.
                let target_date = read_target_creation_date(&ra.target, Some(now_bias));
                let values = match resolve_parameters(&p.parameters, &ra.params, target_date) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("chrono: {}", e.message());
                        return Err(e.exit_code());
                    }
                };
                // What the preset was filled with, read while the declarations and the values are
                // both in hand. A dry run prints it, because "which date is this trial counting
                // from" is exactly the question a resolved absolute moment does not answer.
                let provenance = parameter_provenance(&p.parameters, &ra.params, &values);
                let moment = match resolve_moment(p.moment, &values) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("chrono: {}", e.message());
                        return Err(e.exit_code());
                    }
                };
                // Evaluate the preset moment against real "now" in the session zone, exactly like a
                // relative --at, to an absolute wall moment. No calendar here - a preset that needs
                // one is an honest error (the run surface has no --calendar yet).
                let now = match resolve_now_civil(Some(now_bias)) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("chrono: cannot resolve current time: {e}");
                        return Err(3);
                    }
                };
                match chrono_core::calc::eval(
                    &moment,
                    &EvalContext { now, zone_bias_min: now_bias, calendar: None },
                ) {
                    Ok(outcome) => Decided {
                        at: Some(outcome.result().to_iso()),
                        mode: p.time_mode.mode.clone(),
                        multiplier: p.time_mode.multiplier,
                        scale_duration: p.time_mode.scale_duration,
                        // The zone the moment was computed in travels with it: a preset moment paired
                        // with a bias of 0 would land an offset away from the instant it names.
                        session_bias: Some(now_bias),
                        origin: TimeOrigin::Preset { id: p.id.clone(), parameters: provenance },
                    },
                    Err(e) => {
                        eprintln!("chrono: preset '{}' moment: {}", p.id, describe_calc_error(&e));
                        return Err(calc_error_exit_code(&e));
                    }
                }
            }
            Err(e) => {
                eprintln!("chrono: {}", e.message());
                return Err(e.exit_code());
            }
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
                    eprintln!("chrono: {e}");
                    print_usage();
                    return Err(1);
                }
            },
            None => match resolve_now_civil(Some(now_bias)) {
                Ok(now) => Some(now.to_iso()),
                Err(e) => {
                    eprintln!("chrono: {e}");
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
            mode: ra.mode.clone(),
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
    let spec = TimeSpec {
        moment: MomentSpec {
            kind: "absolute".into(),
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

/// Resolve the `--at` value to an absolute wall string (the core only ever sees an
/// absolute moment). A leading `+`/`-` marks a relative moment - now plus one shift
/// step - resolved through the SHARED calc evaluator, so `--at` accepts exactly the
/// units the calculator does, including months, quarters, and years, which fold onto
/// the civil date (a fixed-tick delta cannot express them). Anything else passes
/// through as an absolute moment.
///
/// One grammar, not two: `--at`, `jump`, and the calculator all resolve through the same
/// step evaluator. The old `parse_relative_delta` (fixed-tick only) is gone entirely.
pub(crate) fn resolve_at(raw: &str, tz_bias_min: Option<i32>) -> Result<String, String> {
    if raw.starts_with(['+', '-']) {
        let now = resolve_now_civil(tz_bias_min)?;
        resolve_relative_at(raw, now)
    } else {
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
        assert_eq!(spec.moment.kind, "absolute");
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
        assert_eq!(spec.mode, "multiplier");
        assert_eq!(spec.multiplier, Some(60));
        assert!(spec.scale_duration);
        assert!(spec.scale_qpc);

        let plain = parse_run_args(&["app.exe".into(), "--at".into(), "2026-01-01T00:00:00".into()]).unwrap();
        let spec = resolve_time_spec(&plain, 0).unwrap().spec;
        assert_eq!(spec.mode, "flow");
        assert_eq!(spec.multiplier, None);
        assert!(!spec.scale_duration);
        assert!(!spec.scale_qpc);
    }
}
