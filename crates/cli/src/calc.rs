//! `chrono calc`: the date calculator, the product's second half.
//!
//! Two renderings of one result. The human one is prose meant to be read in a terminal; the machine
//! one is `chronomock.calc/1`, which the GUI reads over the same boundary the substitution side uses
//! (ADR-6). Both come from the same evaluation, so they cannot disagree.
//!
//! The step grammar is `crate::grammar` - the same parser the preset reader uses.
//!
//! The calculator surface of the shared step grammar (docs/04 section 4.3). The driver parses typed
//! flags into a canonical `MomentExpr`, resolves "now" for a today/now base (the core stays pure and
//! takes now as data), evaluates, and renders. No natural language (6.2): each flag is one step,
//! order is step order.
//!
//! The calc engine lives in `chrono-core` (serde-free), so the CLI owns serialization: the JSON DTOs
//! here are populated from the core's typed results, the mirror of the calendar and preset loaders.
//! The GUI is a thin client of this contract (ADR-6), consuming the same engine output the human
//! render shows. Contract keys are public names (rule 17); a breaking change needs a schema bump.
//! An error still goes to stderr with a non-zero exit - stdout carries a result only on success.


use std::collections::HashMap;

use chrono_core::calc::{Base, EvalContext, EvalError, MomentExpr, Sign, Step};
use chrono_core::filetime_utc_to_wall;

use crate::calendar::load_calendar;
use crate::cli::print_calc_usage;
use crate::grammar::{parse_base, parse_nearest, parse_set_time, parse_shift, parse_snap};
use crate::preset::{
    load_preset, preset_targets_calculator, resolve_moment, resolve_parameters,
};
use crate::zone::{format_bias, now_filetime_utc, parse_zone_to_bias, session_zone_default};
/// Parsed `chrono calc` arguments: a base, an ordered step list, and the session
/// zone used to resolve a today/now base.
pub(crate) struct CalcArgs {
    base: Base,
    steps: Vec<Step>,
    zone_bias_min: Option<i32>,
    /// Calendar id (e.g. "us-banking") for business-day and holiday metadata. None = omit them.
    calendar: Option<String>,
    /// A pasted date to analyze in reverse (7.3) instead of building a moment. None = build mode.
    analyze: Option<String>,
    /// A custom .NET/Java-style format mask (7.3, docs/02 8.9) for the result. None = fixed formats only.
    format: Option<String>,
    /// A named preset id (docs/04 4.3): the moment comes from `presets/<id>.json`, not from step
    /// flags. None = build the moment from the flags. Cannot be combined with the step flags.
    preset: Option<String>,
    /// Preset parameter values from `--param id=value` (docs/04 4.2). Only meaningful with --preset.
    params: HashMap<String, String>,
    /// Emit the result as machine JSON (chronomock.calc/1) instead of human text, so the GUI can consume
    /// the same engine output as a thin client (ADR-6). Composes with build, --analyze, and --preset.
    json: bool,
}

pub(crate) fn calc_run(argv: &[String]) -> i32 {
    let ca = match parse_calc_args(argv) {
        Ok(ca) => ca,
        Err(e) => {
            eprintln!("chrono: {e}");
            print_calc_usage();
            return 1;
        }
    };

    // The session zone for this whole invocation: the one the caller named, else the HOST's. It used
    // to be UTC here while `chrono run` beside it already followed the host, so the two halves of one
    // tool disagreed about what day it is - and in a zone ahead of UTC, `--base today` between midnight
    // and the offset returned YESTERDAY (R2-S7). A calculator that is a day out around midnight is
    // worse than no calculator. `--zone` still wins, and the zone is printed either way (rule 2).
    //
    // ONE zone per invocation, not one per base kind: the civil fields and the instant formats
    // (epoch, FILETIME, RFC 1123) have to be read in the same zone, or a single result would carry
    // two answers - the very shape of R2-S11 next door.
    let zone_bias = session_zone_default(ca.zone_bias_min);
    let zone_from_host = ca.zone_bias_min.is_none();

    // Resolve the real current time in that zone, as data for the pure core.
    let now = match resolve_now_civil(Some(zone_bias)) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("chrono: cannot resolve current time: {e}");
            return 3;
        }
    };

    // Load the calendar (if requested) before evaluating: a missing or malformed calendar is a
    // usage error surfaced before any result, never a silently dropped metadata field.
    let calendar = match &ca.calendar {
        Some(id) => match load_calendar(id) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("chrono: {e}");
                return 1;
            }
        },
        None => None,
    };

    // Reverse analysis (7.3): with --analyze, interpret a pasted date instead of building a moment.
    // The build flags (--base/--shift/...) do not apply; --calendar and --zone still do.
    if let Some(input) = &ca.analyze {
        return match chrono_core::calc::analyze_date(input) {
            Ok(analysis) => {
                if ca.json {
                    println!("{}", calc_analysis_json(&analysis, input, &now, Some(zone_bias), calendar.as_ref()));
                } else {
                    print!("{}", render_analysis(&analysis, input, &now, Some(zone_bias), calendar.as_ref()));
                }
                0
            }
            Err(e) => {
                eprintln!("chrono calc: {e} (calc.analyze_unrecognized)");
                1
            }
        };
    }

    // A named preset (docs/04 4.3) supplies the moment in place of the step flags; otherwise the
    // moment is built from the flags. The preset also carries a human header (name + "explains").
    let preset_id = ca.preset.clone();
    let (expr, preset_header, preset_meta) = match preset_id {
        Some(pid) => match load_preset(&pid) {
            Ok(p) => {
                // The calculator surface honours `applies_to`: a substitution-only preset
                // (e.g. year-rollover) is not a calculator question (docs/05 3.1). Refuse it
                // instead of computing a moment nobody asked the calculator for.
                if !preset_targets_calculator(&p.applies_to) {
                    eprintln!(
                        "chrono calc: preset '{}' targets {}, not the calculator (calc.preset_not_for_calculator)",
                        p.id, p.applies_to
                    );
                    return 1;
                }
                // Resolve the preset's parameters (--param / default) then substitute them into its
                // moment. A non-parametric preset resolves to an empty map and an unchanged moment.
                // The calculator has no target, so no target_file_creation hint (None).
                let values = match resolve_parameters(&p.parameters, &ca.params, None) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("chrono calc: {}", e.message());
                        return e.exit_code();
                    }
                };
                let moment = match resolve_moment(p.moment, &values) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("chrono calc: {}", e.message());
                        return e.exit_code();
                    }
                };
                let pj = PresetJson { id: p.id, name: p.name_en, explains: p.explains_en };
                let header =
                    format!("  preset:   {} - {}\n  explains: {}\n", pj.id, pj.name, pj.explains);
                (moment, Some(header), Some(pj))
            }
            Err(e) => {
                eprintln!("chrono calc: {}", e.message());
                return e.exit_code();
            }
        },
        None => (MomentExpr { base: ca.base, steps: ca.steps }, None, None),
    };
    match chrono_core::calc::eval(
        &expr,
        &EvalContext { now, zone_bias_min: zone_bias, calendar: calendar.as_ref() },
    ) {
        Ok(outcome) => {
            if ca.json {
                println!(
                    "{}",
                    calc_moment_json(&outcome, &now, calendar.as_ref(), ca.format.as_deref(), preset_meta)
                );
                return 0;
            }
            let mut text =
                render_calc(&expr, &outcome, Some(zone_bias), zone_from_host, &now, calendar.as_ref(), preset_header.as_deref());
            // A custom mask (7.3) adds one more line in the target app's exact format.
            if let Some(mask) = &ca.format {
                text.push_str(&format!(
                    "  custom format:  {}\n",
                    chrono_core::calc::format_with_mask(&outcome.result(), mask)
                ));
            }
            print!("{text}");
            0
        }
        Err(e) => {
            eprintln!("{}", describe_calc_error(&e));
            calc_error_exit_code(&e)
        }
    }
}

pub(crate) const CALC_SCHEMA: &str = "chronomock.calc/1";

#[derive(serde::Serialize)]
pub(crate) struct CalcJson {
    schema: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    moment: Option<MomentJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    analysis: Option<AnalysisJson>,
}

#[derive(serde::Serialize)]
pub(crate) struct MomentJson {
    /// The result moment as ISO wall-clock in `zone_bias_min`.
    iso: String,
    /// Result-zone bias in minutes (UTC = local + bias); a `zone` step may move it off the session zone.
    zone_bias_min: i32,
    base: String,
    /// The intermediate result after each step, in order.
    steps: Vec<String>,
    formats: FormatsJson,
    metadata: MetadataJson,
    /// Stable significance tokens (Significance::key); empty when the date hits no landmark.
    significance: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<PresetJson>,
}

#[derive(serde::Serialize)]
pub(crate) struct FormatsJson {
    iso_date: String,
    iso_datetime: String,
    us: String,
    pl: String,
    /// Instant-based formats are null outside the representable FILETIME range (never a wrong number).
    epoch_seconds: Option<i64>,
    epoch_millis: Option<i64>,
    filetime: Option<i64>,
    rfc1123: Option<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct MetadataJson {
    weekday: &'static str,
    iso_week_year: i64,
    iso_week: u32,
    us_week: u32,
    day_of_year: u32,
    quarter: u32,
    is_leap_year: bool,
    days_from_today: i64,
    /// null when no calendar was supplied; otherwise whether the date is a business day.
    business_day: Option<bool>,
    /// The holiday's English name, or null (no calendar, or not a holiday - business_day disambiguates).
    holiday: Option<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct PresetJson {
    id: String,
    name: String,
    explains: String,
}

#[derive(serde::Serialize)]
pub(crate) struct AnalysisJson {
    input: String,
    ambiguous: bool,
    readings: Vec<ReadingJson>,
}

#[derive(serde::Serialize)]
pub(crate) struct ReadingJson {
    /// Stable reading token (DateReading::key): iso / us_month_day / pl_day_month.
    reading: &'static str,
    iso: String,
    significance: Vec<&'static str>,
    metadata: MetadataJson,
}

pub(crate) fn formats_json(civil: &chrono_core::calc::CivilDateTime, bias: i32) -> FormatsJson {
    let f = chrono_core::calc::formats(civil, bias);
    FormatsJson {
        iso_date: f.iso_date,
        iso_datetime: f.iso_datetime,
        us: f.us,
        pl: f.pl,
        epoch_seconds: f.epoch_seconds,
        epoch_millis: f.epoch_millis,
        filetime: f.filetime,
        rfc1123: f.rfc1123,
    }
}

pub(crate) fn metadata_json(
    civil: &chrono_core::calc::CivilDateTime,
    now: &chrono_core::calc::CivilDateTime,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> MetadataJson {
    let m = chrono_core::calc::metadata(civil, now);
    let (business_day, holiday) = match calendar {
        Some(cal) => (
            Some(chrono_core::calendar::is_business_day(civil, cal)),
            chrono_core::calendar::holiday_on(civil, cal).map(|h| h.name_en.clone()),
        ),
        None => (None, None),
    };
    MetadataJson {
        weekday: m.weekday,
        iso_week_year: m.iso_week_year,
        iso_week: m.iso_week,
        us_week: m.us_week,
        day_of_year: m.day_of_year,
        quarter: m.quarter,
        is_leap_year: m.is_leap_year,
        days_from_today: m.days_from_today,
        business_day,
        holiday,
    }
}

pub(crate) fn significance_keys(
    civil: &chrono_core::calc::CivilDateTime,
    bias: i32,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> Vec<&'static str> {
    chrono_core::calc::significance(civil, bias, calendar).iter().map(|s| s.key()).collect()
}

pub(crate) fn calc_moment_json(
    outcome: &chrono_core::calc::EvalOutcome,
    now: &chrono_core::calc::CivilDateTime,
    calendar: Option<&chrono_core::calendar::Calendar>,
    format_mask: Option<&str>,
    preset: Option<PresetJson>,
) -> String {
    let result = outcome.result();
    let bias = outcome.result_bias;
    let moment = MomentJson {
        iso: result.to_iso(),
        zone_bias_min: bias,
        base: outcome.base.to_iso(),
        steps: outcome.after_each.iter().map(|c| c.to_iso()).collect(),
        formats: formats_json(&result, bias),
        metadata: metadata_json(&result, now, calendar),
        significance: significance_keys(&result, bias, calendar),
        custom_format: format_mask.map(|m| chrono_core::calc::format_with_mask(&result, m)),
        preset,
    };
    let doc = CalcJson { schema: CALC_SCHEMA, moment: Some(moment), analysis: None };
    serde_json::to_string(&doc).unwrap_or_else(|e| format!(r#"{{"error":"serialize: {e}"}}"#))
}

pub(crate) fn calc_analysis_json(
    analysis: &chrono_core::calc::DateAnalysis,
    input: &str,
    now: &chrono_core::calc::CivilDateTime,
    zone_bias_min: Option<i32>,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    let bias = zone_bias_min.unwrap_or(0);
    let readings = analysis
        .readings
        .iter()
        .map(|(reading, civil)| ReadingJson {
            reading: reading.key(),
            iso: civil.to_iso(),
            significance: significance_keys(civil, bias, calendar),
            metadata: metadata_json(civil, now, calendar),
        })
        .collect();
    let doc = CalcJson {
        schema: CALC_SCHEMA,
        moment: None,
        analysis: Some(AnalysisJson { input: input.to_string(), ambiguous: analysis.is_ambiguous(), readings }),
    };
    serde_json::to_string(&doc).unwrap_or_else(|e| format!(r#"{{"error":"serialize: {e}"}}"#))
}

pub(crate) fn parse_calc_args(argv: &[String]) -> Result<CalcArgs, String> {
    let mut base = Base::Today;
    let mut steps: Vec<Step> = Vec::new();
    let mut zone_bias_min: Option<i32> = None;
    let mut calendar: Option<String> = None;
    let mut analyze: Option<String> = None;
    let mut format: Option<String> = None;
    let mut preset: Option<String> = None;
    let mut params: HashMap<String, String> = HashMap::new();
    let mut json = false;
    // Whether any moment-building flag appeared, so `--preset` (which supplies its own moment)
    // can reject being combined with them instead of silently ignoring one source.
    let mut saw_step_flag = false;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--base" => {
                i += 1;
                base = parse_base(argv.get(i).ok_or("--base needs a value")?)?;
                saw_step_flag = true;
            }
            "--param" => {
                i += 1;
                let raw = argv.get(i).ok_or("--param needs id=value like start_date=2026-01-01")?;
                let (id, value) = raw
                    .split_once('=')
                    .ok_or_else(|| format!("--param must be id=value, got '{raw}'"))?;
                if id.is_empty() {
                    return Err(format!("--param needs a non-empty id, got '{raw}'"));
                }
                params.insert(id.to_string(), value.to_string());
            }
            "--preset" => {
                i += 1;
                preset = Some(argv.get(i).ok_or("--preset needs an id like month-end")?.clone());
            }
            "--calendar" => {
                i += 1;
                calendar = Some(argv.get(i).ok_or("--calendar needs an id like us-banking")?.clone());
            }
            "--analyze" => {
                i += 1;
                analyze = Some(argv.get(i).ok_or("--analyze needs a date like 04/08/2008")?.clone());
            }
            "--format" => {
                i += 1;
                let m = argv.get(i).ok_or("--format needs a mask like yyyy-MM-dd")?;
                if m.is_empty() {
                    return Err("--format mask is empty".into());
                }
                format = Some(m.clone());
            }
            "--zone" => {
                i += 1;
                let raw = argv.get(i).ok_or("--zone needs a value like +02:00")?;
                zone_bias_min = Some(parse_zone_to_bias(raw)?);
            }
            "--to-zone" => {
                i += 1;
                let raw = argv.get(i).ok_or("--to-zone needs a value like +05:45")?;
                steps.push(Step::Zone(parse_zone_to_bias(raw)?));
                saw_step_flag = true;
            }
            "--shift" => {
                i += 1;
                steps.push(parse_shift(argv.get(i).ok_or("--shift needs a value like +18years")?)?);
                saw_step_flag = true;
            }
            "--set-time" => {
                i += 1;
                steps.push(parse_set_time(argv.get(i).ok_or("--set-time needs a value like 23:59:59")?)?);
                saw_step_flag = true;
            }
            "--snap" => {
                i += 1;
                steps.push(Step::Snap(parse_snap(argv.get(i).ok_or("--snap needs a target")?)?));
                saw_step_flag = true;
            }
            "--nearest" => {
                i += 1;
                steps.push(Step::Nearest(parse_nearest(argv.get(i).ok_or("--nearest needs a target")?)?));
                saw_step_flag = true;
            }
            "--json" => json = true,
            other if other.starts_with("--") => return Err(format!("unknown flag '{other}'")),
            other => return Err(format!("unexpected argument '{other}'")),
        }
        i += 1;
    }

    // A preset supplies its own moment (base + steps); combining it with step flags would mean two
    // sources for one moment. Reject it rather than pick one silently. `--analyze` is a different
    // mode entirely (it reads a date, builds nothing), so it cannot ride with `--preset` either.
    if preset.is_some() && saw_step_flag {
        return Err("--preset builds the moment on its own; it cannot be combined with \
                    --base/--shift/--set-time/--snap/--nearest/--to-zone"
            .into());
    }
    if preset.is_some() && analyze.is_some() {
        return Err("--preset and --analyze are different modes; use one at a time".into());
    }
    // --param only makes sense with --preset (it fills a preset's declared parameters).
    if !params.is_empty() && preset.is_none() {
        return Err("--param needs --preset (parameters belong to a preset)".into());
    }

    Ok(CalcArgs { base, steps, zone_bias_min, calendar, analyze, format, preset, params, json })
}

/// Real current time in the session zone, as a civil date-time for the pure core.
/// Reuses the tested UTC-now and wall-clock conversion, then parses back to civil.
pub(crate) fn resolve_now_civil(zone_bias_min: Option<i32>) -> Result<chrono_core::calc::CivilDateTime, String> {
    let wall = filetime_utc_to_wall(now_filetime_utc(), zone_bias_min.unwrap_or(0));
    chrono_core::calc::parse_civil_datetime(&wall)
}

/// Exit code for a calc error (a small table separate from the substitution verdict
/// codes in docs/08 section 8): bad input is a usage error (1), an operation not built
/// in this release is its own honest code (5) so a script can tell the two apart.
pub(crate) fn calc_error_exit_code(e: &EvalError) -> i32 {
    match e {
        EvalError::StepUnsupported { .. } | EvalError::NeedsCalendar { .. } => 5,
        EvalError::Overflow { .. }
        | EvalError::BadSetTime { .. }
        | EvalError::DegenerateCalendar { .. }
        // Out of the year band is bad INPUT, not an unbuilt operation: the step is built, the year
        // is one this build will not compute a calendar on. Exit 1, next to the other usage errors.
        | EvalError::BaseYearOutOfRange
        | EvalError::YearOutOfRange { .. } => 1,
    }
}

/// Human message for a calc error, carrying a stable key (docs/08 section 10) and a
/// 1-based step number. "Not built yet" is the product's honest vocabulary, never a
/// silent skip or a faked result (zasady/01 section 2).
pub(crate) fn describe_calc_error(e: &EvalError) -> String {
    match e {
        EvalError::StepUnsupported { kind, index } => {
            format!("chrono calc: step {} ({kind}) is not built yet (calc.step_unsupported)", index + 1)
        }
        EvalError::NeedsCalendar { index } => {
            format!("chrono calc: step {} needs a calendar - pass --calendar (calc.needs_calendar)", index + 1)
        }
        EvalError::DegenerateCalendar { index } => format!(
            "chrono calc: step {} found no business day - this calendar marks (almost) every day as one off; check its weekend and holidays (calc.degenerate_calendar)",
            index + 1
        ),
        EvalError::Overflow { index } => {
            format!("chrono calc: step {} overflows the representable range (calc.overflow)", index + 1)
        }
        EvalError::BadSetTime { index } => {
            format!("chrono calc: step {} has an out-of-range time (calc.bad_set_time)", index + 1)
        }
        EvalError::BaseYearOutOfRange => format!(
            "chrono calc: the base year is outside the range this build computes on ({}..={}) (calc.year_out_of_range)",
            chrono_core::CIVIL_YEAR_MIN,
            chrono_core::CIVIL_YEAR_MAX
        ),
        // Deliberately not phrased as an overflow: nothing overflowed. The step produced an exact
        // year that this build does not compute calendars on, and naming it that way is the whole
        // reason it is a separate variant.
        EvalError::YearOutOfRange { index } => format!(
            "chrono calc: step {} lands on a year outside the range this build computes on ({}..={}) (calc.year_out_of_range)",
            index + 1,
            chrono_core::CIVIL_YEAR_MIN,
            chrono_core::CIVIL_YEAR_MAX
        ),
    }
}

/// Render a calc result: the base, the intermediate value after each step, and the
/// final moment (7.3 - the user sees where they went wrong, not just the final number).
pub(crate) fn render_calc(
    expr: &MomentExpr,
    outcome: &chrono_core::calc::EvalOutcome,
    zone_bias_min: Option<i32>,
    // True when the caller named no `--zone` and this is the host's offset. Printed, so a reader
    // never has to wonder whether a zone they did not type is one they can rely on (rule 2).
    zone_from_host: bool,
    now: &chrono_core::calc::CivilDateTime,
    calendar: Option<&chrono_core::calendar::Calendar>,
    preset_header: Option<&str>,
) -> String {
    let zone = match (zone_bias_min.map(format_bias), zone_from_host) {
        (Some(z), true) => format!("{z}, from the host"),
        (Some(z), false) => z,
        (None, _) => "UTC".into(),
    };
    let mut out = String::from("Chrono Mock - date calculator\n");
    // When the moment came from a named preset, name it and show its "explains" line (the
    // preset's authored framing, docs/04 4.2 - distinct from the computed significance block).
    if let Some(h) = preset_header {
        out.push_str(h);
    }
    match &expr.base {
        Base::Today => out.push_str(&format!("  base:    {}  (today, session zone {zone})\n", outcome.base.to_iso())),
        Base::Now => out.push_str(&format!("  base:    {}  (now, session zone {zone})\n", outcome.base.to_iso())),
        Base::Absolute(_) => out.push_str(&format!("  base:    {}\n", outcome.base.to_iso())),
    }
    for (i, step) in expr.steps.iter().enumerate() {
        out.push_str(&format!("  step {}:  {}  -> {}\n", i + 1, describe_step(step), outcome.after_each[i].to_iso()));
    }
    out.push_str(&format!("  result:  {}\n", outcome.result().to_iso()));
    // Formats and significance follow the RESULT's zone, which a `zone` step may have moved away
    // from the session zone. Without a `zone` step this equals the session zone, so the output is
    // unchanged from before.
    let result_bias = outcome.result_bias;
    out.push_str(&render_formats(&outcome.result(), result_bias));
    out.push_str(&render_metadata(&outcome.result(), now, calendar));
    out.push_str(&render_significance(&outcome.result(), result_bias, calendar));
    out
}

/// Render the "what this date tests" block (7.3): the test-relevant landmarks the result lands
/// on, one per line, ready to read at a glance. This is the calculator's differentiator over an
/// online date calculator (6.2). With a calendar it also names the weekend / holiday / observed-
/// holiday landmarks. Omitted entirely when the date hits nothing notable - the block is a
/// positive signal, never a "no landmark" line to scan past.
pub(crate) fn render_significance(
    civil: &chrono_core::calc::CivilDateTime,
    tz_bias_min: i32,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    let marks = chrono_core::calc::significance(civil, tz_bias_min, calendar);
    if marks.is_empty() {
        return String::new();
    }
    let mut out = String::from("  what this date tests:\n");
    for m in marks {
        out.push_str(&format!("    {}\n", m.label()));
    }
    out
}

/// Render a reverse-analysis result (7.3): the input, then each reading with its resolved date,
/// weekday, and the "what this date tests" markers. Two readings mean an ambiguous numeric order
/// (04/08 is April 8 in the US, August 4 in Poland) - both are shown rather than one chosen silently.
pub(crate) fn render_analysis(
    analysis: &chrono_core::calc::DateAnalysis,
    input: &str,
    now: &chrono_core::calc::CivilDateTime,
    zone_bias_min: Option<i32>,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    let mut out = String::from("Chrono Mock - date analysis\n");
    if analysis.is_ambiguous() {
        out.push_str(&format!("  input:   {input}  (ambiguous - month/day order differs by locale)\n"));
    } else {
        out.push_str(&format!("  input:   {input}\n"));
    }
    let bias = zone_bias_min.unwrap_or(0);
    for (reading, civil) in &analysis.readings {
        let weekday = chrono_core::calc::metadata(civil, now).weekday;
        out.push_str(&format!(
            "  {}:  {:04}-{:02}-{:02}  ({weekday})\n",
            reading.label(),
            civil.year,
            civil.month,
            civil.day
        ));
        for m in chrono_core::calc::significance(civil, bias, calendar) {
            out.push_str(&format!("      {}\n", m.label()));
        }
    }
    out
}

/// Render the metadata for the result (7.3): weekday, ISO and US week numbers side by side
/// (they are different numbers), day of year, quarter, leap year, and the signed day distance
/// from today. With a calendar, also the business-day and holiday fields (which need one) -
/// omitted entirely without a calendar, never guessed.
pub(crate) fn render_metadata(
    civil: &chrono_core::calc::CivilDateTime,
    now: &chrono_core::calc::CivilDateTime,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    let m = chrono_core::calc::metadata(civil, now);
    let days = match m.days_from_today {
        0 => "today".to_string(),
        n if n > 0 => format!("+{n} days"),
        n => format!("{n} days"),
    };
    let mut out = String::from("  metadata:\n");
    out.push_str(&format!("    weekday       {}\n", m.weekday));
    out.push_str(&format!("    ISO week      {:04}-W{:02}\n", m.iso_week_year, m.iso_week));
    out.push_str(&format!("    US week       {}\n", m.us_week));
    out.push_str(&format!("    day of year   {}\n", m.day_of_year));
    out.push_str(&format!("    quarter       Q{}\n", m.quarter));
    out.push_str(&format!("    leap year     {}\n", if m.is_leap_year { "yes" } else { "no" }));
    out.push_str(&format!("    days from now {days}\n"));

    if let Some(cal) = calendar {
        let business = if chrono_core::calendar::is_business_day(civil, cal) { "yes" } else { "no" };
        out.push_str(&format!("    business day  {business}  ({})\n", cal.id));
        let holiday = match chrono_core::calendar::holiday_on(civil, cal) {
            Some(h) => h.name_en.as_str(),
            None => "no",
        };
        out.push_str(&format!("    holiday       {holiday}\n"));
    }
    out
}

/// Render the result in every output format, each on its own labelled line ready to copy
/// (docs/02 section 8, in that order). An instant-based format outside FILETIME range shows
/// "(out of range)" rather than a wrong number or nothing.
pub(crate) fn render_formats(civil: &chrono_core::calc::CivilDateTime, tz_bias_min: i32) -> String {
    let f = chrono_core::calc::formats(civil, tz_bias_min);
    let num = |n: Option<i64>| n.map(|v| v.to_string()).unwrap_or_else(|| "(out of range)".into());
    let mut out = String::from("  formats:\n");
    out.push_str(&format!("    ISO date      {}\n", f.iso_date));
    out.push_str(&format!("    ISO datetime  {}\n", f.iso_datetime));
    out.push_str(&format!("    US            {}\n", f.us));
    out.push_str(&format!("    PL            {}\n", f.pl));
    out.push_str(&format!("    epoch (s)     {}\n", num(f.epoch_seconds)));
    out.push_str(&format!("    epoch (ms)    {}\n", num(f.epoch_millis)));
    out.push_str(&format!("    FILETIME      {}\n", num(f.filetime)));
    out.push_str(&format!("    RFC 1123      {}\n", f.rfc1123.unwrap_or_else(|| "(out of range)".into())));
    out
}

/// One step described in English for the report.
pub(crate) fn describe_step(step: &Step) -> String {
    match step {
        Step::Shift { sign, amount, unit } => {
            let s = match sign {
                Sign::Plus => "+",
                Sign::Minus => "-",
            };
            format!("shift {s}{amount} {}", unit.name())
        }
        Step::SetTime { hour, minute, second } => format!("set time {hour:02}:{minute:02}:{second:02}"),
        Step::Snap(t) => format!("snap to {}", t.label()),
        Step::Nearest(t) => format!("nearest {}", t.label()),
        Step::Zone(bias) => format!("zone {}", format_bias(*bias)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calc_exit_codes_split_bad_input_from_needs_data() {
        // Not built / needs data -> code 5; bad input -> usage 1.
        assert_eq!(calc_error_exit_code(&EvalError::StepUnsupported { kind: "zone", index: 0 }), 5);
        assert_eq!(calc_error_exit_code(&EvalError::NeedsCalendar { index: 0 }), 5);
        // R2-S10: a degenerate calendar is BAD INPUT (exit 1), not an unbuilt operation (exit 5).
        // It used to travel as `NotFound` -> 5 from `--nearest` and as `Overflow` -> 1 from `+Nbd`:
        // two answers, one cause, and the 5 told a script the feature does not exist yet.
        assert_eq!(calc_error_exit_code(&EvalError::DegenerateCalendar { index: 0 }), 1);
        assert_eq!(calc_error_exit_code(&EvalError::Overflow { index: 0 }), 1);
        assert_eq!(calc_error_exit_code(&EvalError::BadSetTime { index: 0 }), 1);
    }

    #[test]
    fn calc_error_message_carries_key_and_one_based_step() {
        let msg = describe_calc_error(&EvalError::NeedsCalendar { index: 0 });
        assert!(msg.contains("step 1"), "1-based step number, got: {msg}");
        assert!(msg.contains("calc.needs_calendar"), "stable key, got: {msg}");
        let msg = describe_calc_error(&EvalError::StepUnsupported { kind: "zone", index: 2 });
        assert!(msg.contains("step 3") && msg.contains("calc.step_unsupported"), "got: {msg}");
    }

    #[test]
    fn calc_parse_to_eval_end_to_end_absolute_base() {
        // The whole CLI path minus the system clock: parse flags, evaluate, check the
        // month clamp that proves the model beats a fixed-tick delta.
        let ca = parse_calc_args(&[
            "--base".into(),
            "2025-01-31T12:00:00".into(),
            "--shift".into(),
            "+1mo".into(),
        ])
        .unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        assert_eq!(out.result().to_iso(), "2025-02-28T12:00:00");
    }

    #[test]
    fn render_calc_shows_base_steps_and_result() {
        let ca = parse_calc_args(&[
            "--base".into(),
            "2008-08-04T00:00:00".into(),
            "--shift".into(),
            "-18years".into(),
            "--shift".into(),
            "-1d".into(),
            "--set-time".into(),
            "23:59:59".into(),
        ])
        .unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, None, false, &now, None, None);
        assert!(text.contains("base:    2008-08-04T00:00:00"), "got:\n{text}");
        assert!(text.contains("step 1:  shift -18 years  -> 1990-08-04T00:00:00"), "got:\n{text}");
        assert!(text.contains("step 3:  set time 23:59:59  -> 1990-08-03T23:59:59"), "got:\n{text}");
        assert!(text.contains("result:  1990-08-03T23:59:59"), "got:\n{text}");
    }

    #[test]
    fn calc_default_base_is_today_with_no_steps() {
        // The degenerate-but-legal input: `chrono calc` with no args -> today, no steps.
        let ca = parse_calc_args(&[]).unwrap();
        assert_eq!(ca.base, Base::Today);
        assert!(ca.steps.is_empty());
    }

    #[test]
    fn render_calc_includes_the_formats_block() {
        let ca = parse_calc_args(&["--base".into(), "1970-01-01T00:00:00".into()]).unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, Some(0), false, &now, None, None);
        assert!(text.contains("formats:"), "got:\n{text}");
        assert!(text.contains("ISO datetime  1970-01-01T00:00:00+00:00"), "got:\n{text}");
        assert!(text.contains("US            01/01/1970"), "got:\n{text}");
        assert!(text.contains("epoch (s)     0"), "got:\n{text}");
        assert!(text.contains("FILETIME      116444736000000000"), "got:\n{text}");
        assert!(text.contains("RFC 1123      Thu, 01 Jan 1970 00:00:00 GMT"), "got:\n{text}");
    }

    #[test]
    fn render_calc_includes_the_metadata_block() {
        let ca = parse_calc_args(&["--base".into(), "2026-01-01T00:00:00".into()]).unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        // A fixed "today" makes days-from-now deterministic: 2026-01-01 is 9 days before 2026-01-10.
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 10, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, Some(0), false, &now, None, None);
        assert!(text.contains("metadata:"), "got:\n{text}");
        assert!(text.contains("weekday       Thursday"), "got:\n{text}"); // 2026-01-01 is a Thursday
        assert!(text.contains("ISO week      2026-W01"), "got:\n{text}");
        assert!(text.contains("US week       1"), "got:\n{text}");
        assert!(text.contains("quarter       Q1"), "got:\n{text}");
        assert!(text.contains("days from now -9 days"), "got:\n{text}");
    }

    fn test_calendar() -> chrono_core::calendar::Calendar {
        use chrono_core::calendar::{Calendar, Holiday, HolidayRule, Observed};
        Calendar {
            id: "us-test".into(),
            country: "US".into(),
            weekend: vec![0, 6],
            observed: Observed::SunToMon,
            holidays: vec![Holiday {
                id: "independence_day".into(),
                name_en: "Independence Day".into(),
                name_local: "Independence Day".into(),
                rule: HolidayRule::Fixed { month: 7, day: 4 },
                valid_from: None,
                valid_to: None,
                source: "test".into(),
            }],
        }
    }

    #[test]
    fn render_metadata_with_calendar_shows_business_day_and_holiday() {
        let civil = chrono_core::calc::CivilDateTime { year: 2026, month: 7, day: 4, hour: 0, minute: 0, second: 0 };
        let cal = test_calendar();
        let text = render_metadata(&civil, &civil, Some(&cal));
        assert!(text.contains("holiday       Independence Day"), "got:\n{text}");
        // 2026-07-04 is a Saturday, so it is not a business day.
        assert!(text.contains("business day  no  (us-test)"), "got:\n{text}");
    }

    #[test]
    fn calc_snap_end_of_quarter_end_to_end() {
        let ca = parse_calc_args(&[
            "--base".into(),
            "2026-05-15T09:30:00".into(),
            "--snap".into(),
            "end-of-quarter".into(),
        ])
        .unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        assert_eq!(out.result().to_iso(), "2026-06-30T23:59:59");
    }

    #[test]
    fn render_calc_shows_the_what_this_tests_block_when_a_landmark_is_hit() {
        // A snap to the end of the year lands on Dec 31: the block names the year-end landmark.
        let ca = parse_calc_args(&["--base".into(), "2026-05-15T00:00:00".into(), "--snap".into(), "eoy".into()])
            .unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, Some(0), false, &now, None, None);
        assert!(text.contains("what this date tests:"), "got:\n{text}");
        assert!(text.contains("last day of the year (year-end rollover)"), "got:\n{text}");
    }

    #[test]
    fn render_calc_omits_the_block_for_a_plain_date() {
        // A mid-month weekday hits no landmark, so the block is absent (positive signal only).
        let ca = parse_calc_args(&["--base".into(), "2026-08-12T09:00:00".into()]).unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, Some(0), false, &now, None, None);
        assert!(!text.contains("what this date tests:"), "got:\n{text}");
    }

    #[test]
    fn render_calc_names_calendar_landmarks_only_with_a_calendar() {
        // 2026-07-04 is a Saturday and Independence Day: with a calendar the block names both.
        let ca = parse_calc_args(&["--base".into(), "2026-07-04T00:00:00".into()]).unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let cal = test_calendar();
        let text = render_calc(&expr, &out, Some(0), false, &now, Some(&cal), None);
        assert!(text.contains("what this date tests:"), "got:\n{text}");
        assert!(text.contains("weekend - not a business day"), "got:\n{text}");
        assert!(text.contains("public holiday"), "got:\n{text}");
        // Without a calendar the same date names no calendar landmark (and here nothing at all).
        assert!(!render_calc(&expr, &out, Some(0), false, &now, None, None).contains("what this date tests:"));
    }

    #[test]
    fn calc_to_zone_converts_preserving_the_instant_end_to_end() {
        // 12:00 in UTC+2 re-expressed in UTC+5:45 (Kathmandu's offset): 15:45 wall-clock, and the
        // instant (epoch) is unchanged - the whole point of a zone conversion.
        let ca = parse_calc_args(&[
            "--base".into(),
            "2026-01-15T12:00:00".into(),
            "--zone".into(),
            "+02:00".into(),
            "--to-zone".into(),
            "+05:45".into(),
        ])
        .unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let bias = ca.zone_bias_min.unwrap_or(0);
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: bias, calendar: None }).unwrap();
        assert_eq!(out.result().to_iso(), "2026-01-15T15:45:00");
        assert_eq!(out.result_bias, -345);
        let text = render_calc(&expr, &out, ca.zone_bias_min, false, &now, None, None);
        assert!(text.contains("step 1:  zone +05:45"), "got:\n{text}");
        assert!(text.contains("ISO datetime  2026-01-15T15:45:00+05:45"), "got:\n{text}");
        // Same instant as the +02:00 base (12:00+02:00 = 10:00 UTC).
        let base = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 15, hour: 12, minute: 0, second: 0 };
        assert_eq!(
            chrono_core::calc::formats(&out.result(), out.result_bias).epoch_seconds,
            chrono_core::calc::formats(&base, -120).epoch_seconds
        );
    }

    #[test]
    fn calc_to_zone_rejects_a_malformed_offset() {
        assert!(parse_calc_args(&["--to-zone".into(), "midnight".into()]).is_err());
        assert!(parse_calc_args(&["--to-zone".into(), "+5".into()]).is_err());
    }

    #[test]
    fn render_analysis_shows_both_readings_for_an_ambiguous_date() {
        let analysis = chrono_core::calc::analyze_date("04/08/2008").unwrap();
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 25, hour: 0, minute: 0, second: 0 };
        let text = render_analysis(&analysis, "04/08/2008", &now, Some(0), None);
        assert!(text.contains("ambiguous"), "got:\n{text}");
        assert!(text.contains("US MM/DD/YYYY:  2008-04-08  (Tuesday)"), "got:\n{text}");
        assert!(text.contains("PL DD/MM/YYYY:  2008-08-04  (Monday)"), "got:\n{text}");
    }

    #[test]
    fn calc_json_flag_parses() {
        let ca = parse_calc_args(&["--base".into(), "2026-09-30T23:59:59".into(), "--json".into()]).unwrap();
        assert!(ca.json);
        assert!(!parse_calc_args(&["--base".into(), "2026-09-30T00:00:00".into()]).unwrap().json);
    }

    #[test]
    fn calc_moment_json_carries_the_contract_fields() {
        let base = chrono_core::calc::CivilDateTime { year: 2026, month: 9, day: 30, hour: 23, minute: 59, second: 59 };
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 26, hour: 0, minute: 0, second: 0 };
        let expr = MomentExpr { base: Base::Absolute(base), steps: vec![] };
        let outcome =
            chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let json = calc_moment_json(&outcome, &now, None, None, None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema"], "chronomock.calc/1");
        assert_eq!(v["moment"]["iso"], "2026-09-30T23:59:59");
        assert_eq!(v["moment"]["formats"]["iso_date"], "2026-09-30");
        // No calendar supplied, so the calendar-dependent metadata is null (never guessed).
        assert!(v["moment"]["metadata"]["business_day"].is_null());
        let sig = v["moment"]["significance"].as_array().unwrap();
        assert!(sig.iter().any(|s| s == "end_of_quarter"), "got: {sig:?}");
    }

    #[test]
    fn calc_analysis_json_shows_both_readings_with_stable_keys() {
        let analysis = chrono_core::calc::analyze_date("04/08/2008").unwrap();
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 26, hour: 0, minute: 0, second: 0 };
        let json = calc_analysis_json(&analysis, "04/08/2008", &now, Some(0), None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema"], "chronomock.calc/1");
        assert_eq!(v["analysis"]["ambiguous"], true);
        let readings = v["analysis"]["readings"].as_array().unwrap();
        assert_eq!(readings.len(), 2);
        // `iso` is the full ISO datetime (midnight for a date reading), consistent with moment.iso.
        assert_eq!(readings[0]["reading"], "us_month_day");
        assert_eq!(readings[0]["iso"], "2008-04-08T00:00:00");
        assert_eq!(readings[1]["reading"], "pl_day_month");
        assert_eq!(readings[1]["iso"], "2008-08-04T00:00:00");
    }

    #[test]
    fn render_analysis_names_a_holiday_with_a_calendar() {
        // 07/04/2026: the US reading is Independence Day (a Saturday), named only with a calendar.
        let analysis = chrono_core::calc::analyze_date("07/04/2026").unwrap();
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let cal = test_calendar();
        let text = render_analysis(&analysis, "07/04/2026", &now, Some(0), Some(&cal));
        assert!(text.contains("public holiday"), "got:\n{text}");
    }

    #[test]
    fn parse_calc_reads_the_format_mask_and_rejects_empty() {
        let ca = parse_calc_args(&["--format".into(), "dd.MM.yyyy".into()]).unwrap();
        assert_eq!(ca.format.as_deref(), Some("dd.MM.yyyy"));
        assert!(parse_calc_args(&["--format".into(), String::new()]).is_err());
    }

    /// `--preset` supplies its own moment, so combining it with a step flag (or --analyze) is a
    /// usage error; alone it parses.
    #[test]
    fn preset_flag_is_exclusive_of_step_flags() {
        assert!(parse_calc_args(&["--preset".into(), "month-end".into()]).is_ok());
        assert_eq!(
            parse_calc_args(&["--preset".into(), "month-end".into()]).unwrap().preset.as_deref(),
            Some("month-end")
        );
        assert!(parse_calc_args(&["--preset".into(), "month-end".into(), "--shift".into(), "+1d".into()]).is_err());
        assert!(parse_calc_args(&["--shift".into(), "+1d".into(), "--preset".into(), "month-end".into()]).is_err());
        assert!(parse_calc_args(&["--preset".into(), "x".into(), "--analyze".into(), "2020-01-01".into()]).is_err());
    }

    /// --param only makes sense with --preset.
    #[test]
    fn calc_param_needs_preset() {
        assert!(parse_calc_args(&["--param".into(), "start_date=2026-01-01".into()]).is_err());
        let ok = parse_calc_args(&["--preset".into(), "trial-first-day-after".into(), "--param".into(), "start_date=2026-01-01".into()]).unwrap();
        assert_eq!(ok.params.get("start_date").map(String::as_str), Some("2026-01-01"));
    }
}
