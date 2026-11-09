//! `chrono calc`: the date calculator, the product's second half.
//!
//! Two renderings of one result. The human one is prose meant to be read in a terminal - the machine
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
//! render shows. Contract keys are public names (rule 17) - a breaking change needs a schema bump.
//! An error still goes to stderr with a non-zero exit - stdout carries a result only on success.


use std::collections::HashMap;

use chrono_core::calc::{Base, EvalContext, EvalError, MomentExpr, Sign, Step};
use chrono_core::filetime_utc_to_wall;

use crate::cli::{flag_value, print_calc_usage};
use crate::grammar::{parse_base, parse_nearest, parse_set_time, parse_shift, parse_snap};
use crate::output::{diag, out, outln, Stream};
use crate::preset::{load_for, load_market_calendar, PresetRefusal, PresetStage, Surface};
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
            // One line with a stable key, like every other refusal of this command (R4-S23). Without
            // the key the GUI had nothing to translate, and the usage printed after the sentence
            // became the "detail" under it: a whole command-line synopsis on a calculator panel.
            diag!("chrono calc: {e} (calc.bad_argument)");
            // The usage is for a person at a terminal. A caller asking for machine output (the GUI
            // always does) gets the line alone. Read from the words themselves, because the parser
            // that would have set `json` is the one that just failed.
            if !argv.iter().any(|word| word == "--json") {
                print_calc_usage(Stream::Err);
            }
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

    // Resolve the real current time in that zone, as data for the pure core. The instant is kept, so
    // the same "now" can be read in the result's zone below without a second look at the clock.
    let now_utc = now_filetime_utc();
    let now = match civil_at(now_utc, zone_bias) {
        Ok(n) => n,
        Err(e) => {
            diag!("chrono: cannot resolve current time: {e}");
            return 3;
        }
    };

    // Load the calendar (if requested) before evaluating: a missing or malformed calendar is a
    // usage error surfaced before any result, never a silently dropped metadata field.
    let mut calendar = match &ca.calendar {
        Some(id) => match load_named_calendar(id) {
            Ok(c) => Some(c),
            Err(line) => {
                diag!("{line}");
                return 1;
            }
        },
        None => None,
    };

    // Reverse analysis (7.3): with --analyze, interpret a pasted date instead of building a moment.
    // The build flags (--base/--shift/...) do not apply. --calendar and --zone still do.
    if let Some(input) = &ca.analyze {
        return match chrono_core::calc::analyze_date(input, zone_bias) {
            Ok(analysis) => {
                if ca.json {
                    outln!("{}", calc_analysis_json(&analysis, input, &now, Some(zone_bias), calendar.as_ref()));
                } else {
                    out!(
                        "{}",
                        render_analysis(&analysis, input, &now, Some(zone_bias), zone_from_host, calendar.as_ref())
                    );
                }
                0
            }
            Err(e) => {
                diag!("chrono calc: {e} (calc.analyze_unrecognized)");
                1
            }
        };
    }

    // A named preset (docs/04 4.3) supplies the moment in place of the step flags - otherwise the
    // moment is built from the flags. The preset also carries a human header (name + "explains").
    let preset_id = ca.preset.clone();
    let (expr, preset_header, preset_meta) = match preset_id {
        Some(pid) => {
            let p = match load_for(&pid, Surface::Calculator) {
                Ok(p) => p,
                Err(PresetRefusal::Unreadable(e)) => {
                    diag!("{}", preset_refusal(&e, PresetStage::File));
                    return e.exit_code();
                }
                // A substitution-only preset (e.g. year-rollover) is not a calculator question (docs/05
                // 3.1). Refused instead of computing a moment nobody asked the calculator for.
                Err(PresetRefusal::NotForSurface { id, applies_to }) => {
                    diag!("chrono calc: preset '{id}' targets {applies_to}, not the calculator (calc.preset_not_for_calculator)");
                    return 1;
                }
            };
            // The market's calendar before the parameters, so a calendar that cannot load is the error
            // said first, as it was.
            let implied = match adopt_market_calendar(&p, &mut calendar) {
                Ok(line) => line,
                Err(e) => {
                    diag!("chrono calc: {e} (calc.market_calendar_unavailable)");
                    return 1;
                }
            };
            // The calculator has no target, so no target_file_creation hint (None).
            let moment = match p.fill(&ca.params, None) {
                Ok((_, moment)) => moment,
                Err((e, stage)) => {
                    diag!("{}", preset_refusal(&e, stage));
                    return e.exit_code();
                }
            };
            let pj = PresetJson { id: p.id, name: p.name_en, explains: p.explains_en };
            let header = format!("  preset:   {} - {}\n  explains: {}\n{implied}", pj.id, pj.name, pj.explains);
            (moment, Some(header), Some(pj))
        }
        None => (MomentExpr { base: ca.base, steps: ca.steps }, None, None),
    };
    match chrono_core::calc::eval(
        &expr,
        &EvalContext { now, zone_bias_min: zone_bias, calendar: calendar.as_ref() },
    ) {
        Ok(outcome) => {
            // "Days from now" compares the result's date with today's, and a date is a date in some
            // zone. The result is in the zone after its last `--to-zone`, so today is read there too:
            // from -12:00 re-expressed in +14:00, the very same instant was reported as a day away
            // (R4-N31). The same instant as `now`, because a second look at the clock can land on the
            // far side of midnight.
            let today = match civil_at(now_utc, outcome.result_bias) {
                Ok(t) => t,
                Err(e) => {
                    diag!("chrono calc: cannot resolve current time: {e}");
                    return 3;
                }
            };
            if ca.json {
                outln!(
                    "{}",
                    calc_moment_json(&outcome, &today, calendar.as_ref(), ca.format.as_deref(), preset_meta)
                );
                return 0;
            }
            let mut text =
                render_calc(&expr, &outcome, Some(zone_bias), zone_from_host, &today, calendar.as_ref(), preset_header.as_deref());
            // A custom mask (7.3) adds one more line in the target app's exact format.
            if let Some(mask) = &ca.format {
                let rendered = chrono_core::calc::format_with_mask(&outcome.result(), mask);
                text.push_str(&format!("  custom format:  {}\n", rendered.text));
                // Named right under the line they affect. Without this the output carries raw mask
                // letters and still looks like a formatted date, so the reader's first guess is their
                // own typo rather than a token this build does not know (rule 6).
                if !rendered.unknown.is_empty() {
                    text.push_str(&format!(
                        "                  ^ not format tokens, passed through as text: {} - quote literal text as 'like this'\n",
                        rendered.unknown.join(", ")
                    ));
                }
            }
            out!("{text}");
            0
        }
        Err(e) => {
            diag!("{}", describe_calc_error(&e));
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

/// One step whose day was clamped to a shorter month, with both days so a client can word it itself.
/// `step` is 1-based, matching the human render and every other step reference in this contract.
#[derive(serde::Serialize)]
pub(crate) struct ClampedStepJson {
    step: usize,
    requested_day: u32,
    clamped_to: u32,
}

#[derive(serde::Serialize)]
pub(crate) struct MomentJson {
    /// The result moment as ISO wall-clock in `zone_bias_min`.
    iso: String,
    /// Result-zone bias in minutes (UTC = local + bias) - a `zone` step may move it off the session zone.
    zone_bias_min: i32,
    base: String,
    /// The intermediate result after each step, in order.
    steps: Vec<String>,
    formats: FormatsJson,
    metadata: MetadataJson,
    /// Stable significance tokens (Significance::key) - empty when the date hits no landmark.
    significance: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_format: Option<String>,
    /// Letter runs in the mask that are not format tokens, so a client can say so instead of showing
    /// raw mask letters as if they were a rendered date. Omitted when the mask was fully understood,
    /// which keeps the common result unchanged (an added field does not bump the schema, docs/04 3).
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_format_unknown: Option<Vec<String>>,
    /// Steps where a month-folding shift clamped the day to a shorter month. Omitted when none did.
    /// The GUI shows no intermediate steps, so without this a clamp is invisible there: the reader
    /// sees a day that changed and no reason for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    clamped_steps: Option<Vec<ClampedStepJson>>,
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
    /// null when no calendar was supplied, or when the date is before the years it covers
    /// (`calendar_valid_from`) - otherwise whether the date is a business day.
    business_day: Option<bool>,
    /// The holiday's English name, or null (no calendar, a date it does not cover, or not a holiday -
    /// business_day disambiguates).
    holiday: Option<String>,
    /// The calendar id that decided the two fields above, or null when none was supplied. Added because
    /// they are a JUDGEMENT rather than a fact: a Sunday is not a business day under the calendars we
    /// ship and is one under a calendar whose weekend is Friday and Saturday. The text output has always
    /// named it ("business day  no  (us-banking)") and the machine output did not, so a consumer reading
    /// the JSON could not tell which calendar it was reading. Additive, so no schema version (docs/04 3).
    calendar: Option<String>,
    /// The first year that calendar answers for, or null without one. A date before it has
    /// `business_day` and `holiday` null and the `before_calendar` mark (R4-S20). Additive.
    calendar_valid_from: Option<i64>,
    /// The day that calendar's holidays were checked against the law (`YYYY-MM-DD`), or null without
    /// one - what the `calendar_outdated` mark is measured from (R4/20). Additive.
    calendar_law_as_of: Option<String>,
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
    /// The session zone the analysis was read in (UTC = local + bias). An epoch reading is an instant,
    /// and its `iso` is a wall clock in this zone - without the field a client got that wall clock
    /// with no way to say whose it was (R4-N36). `days_from_today` counts from today in it too. A
    /// date written as fields does not move with it. Additive, so no schema version (docs/04 3).
    zone_bias_min: i32,
    readings: Vec<ReadingJson>,
}

#[derive(serde::Serialize)]
pub(crate) struct ReadingJson {
    /// Stable reading token (DateReading::key): iso / us_month_day / pl_day_month / epoch_seconds /
    /// epoch_millis.
    reading: &'static str,
    iso: String,
    /// Whether the reading is an instant (a number of seconds or milliseconds since 1970) - its wall
    /// clock is the analysis zone's, which a date written as fields is not. Additive (R4-S23).
    instant: bool,
    /// The time of day the reading has to show, `HH:MM:SS`, or null - the rule the text output follows
    /// (`reading_shows_time`), so a client shows what the CLI shows instead of deciding it again from
    /// `iso`, where every date reads as midnight. Additive (R4-S23).
    time: Option<String>,
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
    // A calendar says nothing about a date before its first year - not "a business day", not "no
    // holiday" - where its rules were not the ones in force (R4-S20).
    let (business_day, holiday) = match calendar.filter(|cal| cal.judges_year(civil.year)) {
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
        calendar: calendar.map(|cal| cal.id.clone()),
        calendar_valid_from: calendar.and_then(|cal| cal.valid_from),
        calendar_law_as_of: calendar.and_then(|cal| cal.law_as_of).map(|law| date_text(&law)),
    }
}

/// A civil date as `YYYY-MM-DD`, the way a calendar file writes `law_as_of`.
fn date_text(civil: &chrono_core::calc::CivilDateTime) -> String {
    let iso = civil.to_iso();
    iso.split('T').next().unwrap_or(&iso).to_string()
}

/// What `civil` lands on, with the calendar's say about itself. `before_calendar` comes from the core.
/// `calendar_outdated` is added here, where today is known: the calendar decided something after the day
/// its holidays were checked against the law, more than a year ago - about `civil` itself, or about
/// `reach`, the latest day a business-day step judged on the way (R4/20, the owner's case of data from
/// 2030 read in 2032).
pub(crate) fn significance_of(
    civil: &chrono_core::calc::CivilDateTime,
    bias: i32,
    calendar: Option<&chrono_core::calendar::Calendar>,
    today: &chrono_core::calc::CivilDateTime,
    reach: Option<&chrono_core::calc::CivilDateTime>,
) -> Vec<chrono_core::calc::Significance> {
    let mut marks = chrono_core::calc::significance(civil, bias, calendar);
    if let Some(cal) = calendar {
        let judged_here = cal.judges_year(civil.year) && chrono_core::calendar::outdated(cal, today, civil);
        if judged_here || reach.is_some_and(|day| chrono_core::calendar::outdated(cal, today, day)) {
            marks.push(chrono_core::calc::Significance::CalendarOutdated);
        }
    }
    marks
}

pub(crate) fn significance_keys(
    civil: &chrono_core::calc::CivilDateTime,
    bias: i32,
    calendar: Option<&chrono_core::calendar::Calendar>,
    today: &chrono_core::calc::CivilDateTime,
    reach: Option<&chrono_core::calc::CivilDateTime>,
) -> Vec<&'static str> {
    significance_of(civil, bias, calendar, today, reach).iter().map(|s| s.key()).collect()
}

pub(crate) fn calc_moment_json(
    outcome: &chrono_core::calc::EvalOutcome,
    // Now, read in the RESULT's zone (`outcome.result_bias`), which is where `days_from_today` has to
    // count from - see `calc_run` (R4-N31).
    today: &chrono_core::calc::CivilDateTime,
    calendar: Option<&chrono_core::calendar::Calendar>,
    format_mask: Option<&str>,
    preset: Option<PresetJson>,
) -> String {
    let result = outcome.result();
    let bias = outcome.result_bias;
    let rendered_mask = format_mask.map(|m| chrono_core::calc::format_with_mask(&result, m));
    let moment = MomentJson {
        iso: result.to_iso(),
        zone_bias_min: bias,
        base: outcome.base.to_iso(),
        steps: outcome.after_each.iter().map(|c| c.to_iso()).collect(),
        formats: formats_json(&result, bias),
        metadata: metadata_json(&result, today, calendar),
        significance: significance_keys(&result, bias, calendar, today, outcome.calendar_reach.as_ref()),
        custom_format: rendered_mask.as_ref().map(|r| r.text.clone()),
        custom_format_unknown: rendered_mask
            .as_ref()
            .filter(|r| !r.unknown.is_empty())
            .map(|r| r.unknown.clone()),
        clamped_steps: (!outcome.clamped.is_empty()).then(|| {
            outcome
                .clamped
                .iter()
                .map(|(i, asked)| ClampedStepJson {
                    step: i + 1,
                    requested_day: *asked,
                    clamped_to: outcome.after_each[*i].day,
                })
                .collect()
        }),
        preset,
    };
    let doc = CalcJson { schema: CALC_SCHEMA, moment: Some(moment), analysis: None };
    serde_json::to_string(&doc).unwrap_or_else(|e| format!(r#"{{"error":"serialize: {e}"}}"#))
}

/// The calendar `--calendar` names, or the line refusing it, ending in a key that says which failure it
/// is (R5-D5, item 2). The window translates the key, and these came to it as a bare English sentence:
/// a calendar that is not there and one that is there and cannot be used need different hands, and an
/// id that cannot be a file name is an argument to fix.
fn load_named_calendar(id: &str) -> Result<chrono_core::calendar::Calendar, String> {
    let path = crate::calendar::find_calendar_file(id).map_err(|e| {
        if crate::calendar::is_valid_catalogue_id(id) {
            format!("chrono calc: {e} (calc.calendar_not_found)")
        } else {
            format!("chrono calc: {e} (calc.bad_argument)")
        }
    })?;
    crate::calendar::load_calendar_at(&path, id).map_err(|e| format!("chrono calc: {e} (calc.calendar_invalid)"))
}

/// The line refusing a preset, ending in its key (R5-D5, item 2). A file that is not there and a build
/// that does not resolve a part of it have a key of their own wherever they come from. A bad file has
/// two, because the same kind of error is fixed in the file at one stage and in the values at the other.
/// Each key stands inside its sentence, the shape `tests/wire_keys.rs` reads a calculator key by.
fn preset_refusal(e: &crate::preset::PresetError, stage: PresetStage) -> String {
    use crate::preset::PresetError;
    let m = e.message();
    match (e, stage) {
        (PresetError::NotFound(_), _) => format!("chrono calc: {m} (calc.preset_not_found)"),
        (PresetError::NotBuilt(_), _) => format!("chrono calc: {m} (calc.preset_not_built)"),
        (PresetError::BadFile(_), PresetStage::Parameters) => format!("chrono calc: {m} (calc.preset_parameter)"),
        (PresetError::BadFile(_), PresetStage::File) => format!("chrono calc: {m} (calc.preset_invalid)"),
    }
}

/// Count a preset's business days in its market's calendar when no `--calendar` named one, and say so.
///
/// The window's calculator selects that calendar when the preset is chosen and `run --preset` has used
/// it since R4-S12 (docs/05 3.5). `--calendar` still names another, as a calendar picked in the window
/// replaces the market's. `calc --preset` used to stop a market preset with "pass --calendar" while the
/// window computed the same preset - one catalogue, two answers. Returns the header line naming the
/// calendar and where it came from, empty when none was implied.
fn adopt_market_calendar(
    preset: &crate::preset::Preset,
    calendar: &mut Option<chrono_core::calendar::Calendar>,
) -> Result<String, String> {
    if calendar.is_some() {
        return Ok(String::new());
    }
    let Some(loaded) = load_market_calendar(&preset.id, preset.market.as_deref()) else {
        return Ok(String::new());
    };
    let implied = loaded?;
    let line = format!(
        "  calendar: {}, from the preset's market ({})\n",
        implied.id,
        preset.market.as_deref().unwrap_or_default()
    );
    *calendar = Some(implied);
    Ok(line)
}

pub(crate) fn calc_analysis_json(
    analysis: &chrono_core::calc::DateAnalysis,
    input: &str,
    now: &chrono_core::calc::CivilDateTime,
    zone_bias_min: Option<i32>,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    let bias = zone_bias_min.unwrap_or(0);
    let written = time_written(input);
    let readings = analysis
        .readings
        .iter()
        .map(|(reading, civil)| ReadingJson {
            reading: reading.key(),
            iso: civil.to_iso(),
            instant: reading.is_instant(),
            time: reading_shows_time(*reading, written)
                .then(|| format!("{:02}:{:02}:{:02}", civil.hour, civil.minute, civil.second)),
            significance: significance_keys(civil, bias, calendar, now, None),
            metadata: metadata_json(civil, now, calendar),
        })
        .collect();
    let doc = CalcJson {
        schema: CALC_SCHEMA,
        moment: None,
        analysis: Some(AnalysisJson {
            input: input.to_string(),
            ambiguous: analysis.is_ambiguous(),
            zone_bias_min: bias,
            readings,
        }),
    };
    serde_json::to_string(&doc).unwrap_or_else(|e| format!(r#"{{"error":"serialize: {e}"}}"#))
}

/// Refuse a second base flag. `--base` and `--base-utc` read the same text in different zones, so
/// letting the later one win would move the moment by the zone offset with nothing said - the exact
/// failure this pair of flags exists to prevent. Names both flags, including the repeat of one.
fn reject_second_base(seen: &mut Option<&'static str>, flag: &'static str) -> Result<(), String> {
    match seen {
        Some(first) => Err(format!(
            "{first} and {flag} both set the base - pass one (--base reads the session zone, --base-utc reads UTC)"
        )),
        None => {
            *seen = Some(flag);
            Ok(())
        }
    }
}

/// Keep a flag's value, refusing a second one. For these flags the last one used to win, which
/// dropped the first in silence - two zones, two calendars or two masks are a contradiction, not a
/// preference order (R4-N32). The step flags repeat on purpose (each is one step), and the base has
/// its own refusal that names both of its spellings.
fn set_once<T>(slot: &mut Option<T>, flag: &str, value: T) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{flag} is given twice - pass it once"));
    }
    *slot = Some(value);
    Ok(())
}

/// The flags that build a moment, as a refusal names them.
const STEP_FLAGS: &str = "--base/--base-utc/--shift/--set-time/--snap/--nearest/--to-zone";

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
    // Which base flag was seen, so a second one is refused rather than quietly overwriting the
    // first. The two read the same text in DIFFERENT zones, so "last one wins" would move the
    // moment by the zone offset without saying anything.
    let mut saw_base: Option<&str> = None;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--base" => {
                i += 1;
                reject_second_base(&mut saw_base, "--base")?;
                base = parse_base(flag_value(argv, i, "--base", "a value like today, now or 2026-01-31T00:00:00")?)?;
                saw_step_flag = true;
            }
            // The instant form of --base: the value is read in UTC, not in the session zone. For a
            // moment whose meaning is an instant (epoch 0, the 32-bit limit) rather than a local
            // wall-clock reading. Keeps the conversion in the core, so a caller that needs it - the
            // GUI unpacking such a preset - asks for it instead of computing its own.
            "--base-utc" => {
                i += 1;
                reject_second_base(&mut saw_base, "--base-utc")?;
                let raw = flag_value(argv, i, "--base-utc", "a value like 1970-01-01T00:00:00Z")?;
                let trimmed = raw.strip_suffix('Z').unwrap_or(raw);
                base = Base::AbsoluteUtc(chrono_core::calc::parse_civil_datetime(trimmed)?);
                saw_step_flag = true;
            }
            "--param" => {
                i += 1;
                let raw = flag_value(argv, i, "--param", "id=value like start_date=2026-01-01")?;
                let (id, value) = raw
                    .split_once('=')
                    .ok_or_else(|| format!("--param must be id=value, got '{raw}'"))?;
                if id.is_empty() {
                    return Err(format!("--param needs a non-empty id, got '{raw}'"));
                }
                if params.insert(id.to_string(), value.to_string()).is_some() {
                    return Err(format!("--param {id} is given twice - pass each parameter once"));
                }
            }
            "--preset" => {
                i += 1;
                set_once(&mut preset, "--preset", flag_value(argv, i, "--preset", "an id like month-end")?.to_string())?;
            }
            "--calendar" => {
                i += 1;
                let id = flag_value(argv, i, "--calendar", "an id like us-banking")?;
                set_once(&mut calendar, "--calendar", id.to_string())?;
            }
            "--analyze" => {
                i += 1;
                // Not through `cli::flag_value` - see there why pasted text is taken as it comes.
                let raw = argv.get(i).ok_or("--analyze needs a date like 04/08/2008")?;
                set_once(&mut analyze, "--analyze", raw.clone())?;
            }
            "--format" => {
                i += 1;
                let m = flag_value(argv, i, "--format", "a mask like yyyy-MM-dd")?;
                if m.is_empty() {
                    return Err("--format mask is empty".into());
                }
                set_once(&mut format, "--format", m.to_string())?;
            }
            "--zone" => {
                i += 1;
                let raw = flag_value(argv, i, "--zone", "a value like +02:00")?;
                set_once(&mut zone_bias_min, "--zone", parse_zone_to_bias(raw)?)?;
            }
            "--to-zone" => {
                i += 1;
                let raw = flag_value(argv, i, "--to-zone", "a value like +05:45")?;
                steps.push(Step::Zone(parse_zone_to_bias(raw)?));
                saw_step_flag = true;
            }
            "--shift" => {
                i += 1;
                steps.push(parse_shift(flag_value(argv, i, "--shift", "a value like +18years")?)?);
                saw_step_flag = true;
            }
            "--set-time" => {
                i += 1;
                steps.push(parse_set_time(flag_value(argv, i, "--set-time", "a value like 23:59:59")?)?);
                saw_step_flag = true;
            }
            "--snap" => {
                i += 1;
                steps.push(Step::Snap(parse_snap(flag_value(argv, i, "--snap", "a target like end-of-month")?)?));
                saw_step_flag = true;
            }
            "--nearest" => {
                i += 1;
                let target = flag_value(argv, i, "--nearest", "a target like next-business-day")?;
                steps.push(Step::Nearest(parse_nearest(target)?));
                saw_step_flag = true;
            }
            "--json" => json = true,
            other if other.starts_with("--") => return Err(format!("unknown flag '{other}'")),
            other => return Err(format!("unexpected argument '{other}'")),
        }
        i += 1;
    }

    // A preset supplies its own moment (base + steps) - combining it with step flags would mean two
    // sources for one moment. Reject it rather than pick one silently. `--analyze` is a different
    // mode entirely (it reads a date, builds nothing), so it cannot ride with `--preset` either.
    if preset.is_some() && saw_step_flag {
        return Err(format!("--preset builds the moment on its own - it cannot be combined with {STEP_FLAGS}"));
    }
    if preset.is_some() && analyze.is_some() {
        return Err("--preset and --analyze are different modes - use one at a time".into());
    }
    // `--analyze` used to drop these in silence, so `--analyze 04/08/2008 --shift +1d` answered a
    // question that was not the one asked, with nothing to say the step had gone nowhere (R4-N32).
    // `--zone` and `--calendar` do apply to it and stay allowed.
    if analyze.is_some() && saw_step_flag {
        return Err(format!("--analyze reads a date and builds none, so it cannot be combined with {STEP_FLAGS}"));
    }
    if analyze.is_some() && format.is_some() {
        return Err("--format shapes a computed moment - a custom format for --analyze is not built yet".into());
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
    civil_at(now_filetime_utc(), zone_bias_min.unwrap_or(0))
}

/// One instant as a civil date-time in a zone. Apart from reading the clock, so a caller that needs
/// the same "now" in two zones reads the clock once.
fn civil_at(ft_utc: i64, zone_bias_min: i32) -> Result<chrono_core::calc::CivilDateTime, String> {
    chrono_core::calc::parse_civil_datetime(&filetime_utc_to_wall(ft_utc, zone_bias_min))
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
        | EvalError::BaseOverflow
        | EvalError::BaseNotACivilDate
        | EvalError::YearOutOfRange { .. }
        | EvalError::TooManyBusinessDays { .. }
        // The same kind of refusal as the year band: the step is built, the calendar holds no rules for
        // the years it reached (R4-S20).
        | EvalError::BeforeCalendar { .. } => 1,
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
        // Names the base, never a step number: this base converts before step 1 runs, and an
        // expression with no steps can still reach here.
        EvalError::BaseOverflow => {
            "chrono calc: the UTC base names an instant outside the representable range (calc.base_overflow)".into()
        }
        // Unreachable from any surface here (they all parse the base), and named anyway: this maps a
        // public API's refusal, and a match arm that does not exist is a compile error the day it is.
        EvalError::BaseNotACivilDate => {
            "chrono calc: the base is not a real date and time - check its month, day and time of day (calc.base_not_a_date)".into()
        }
        // Deliberately not phrased as an overflow: nothing overflowed. The step produced an exact
        // year that this build does not compute calendars on, and naming it that way is the whole
        // reason it is a separate variant.
        EvalError::YearOutOfRange { index } => format!(
            "chrono calc: step {} lands on a year outside the range this build computes on ({}..={}) (calc.year_out_of_range)",
            index + 1,
            chrono_core::CIVIL_YEAR_MIN,
            chrono_core::CIVIL_YEAR_MAX
        ),
        EvalError::TooManyBusinessDays { index } => format!(
            "chrono calc: step {} takes the business days of this calculation past {limit} - the most one calculation counts, all its steps together. Lower the counts so they add up to {limit} or less (calc.business_days_limit)",
            index + 1,
            limit = chrono_core::calendar::MAX_BUSINESS_DAYS
        ),
        EvalError::BeforeCalendar { index, first_year } => format!(
            "chrono calc: step {} reaches a day before {first_year}, the first year the calendar covers - pick a later date or another calendar (calc.before_calendar)",
            index + 1
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
    // Now, read in the RESULT's zone, which is where "days from now" counts from (R4-N31).
    today: &chrono_core::calc::CivilDateTime,
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
        // Shows BOTH the UTC instant the preset names and where it lands in the session zone. The
        // second line without the first would look like an ordinary local base that happens to sit
        // at an odd time, and the reader would have no way to tell the moment is anchored.
        Base::AbsoluteUtc(utc) => out.push_str(&format!(
            "  base:    {}  ({}Z in UTC, shown in session zone {zone})\n",
            outcome.base.to_iso(),
            utc.to_iso()
        )),
    }
    for (i, step) in expr.steps.iter().enumerate() {
        out.push_str(&format!("  step {}:  {}  -> {}\n", i + 1, describe_step(step), outcome.after_each[i].to_iso()));
        // A clamped day is correct and surprising at the same time: `31 January + 1 month` is
        // 28 February, and `29 February + 1 year` is 28 February. The step line alone shows a day
        // that changed for no visible reason, and a reader deciding whether a fiscal year really
        // ends where the tool says cannot tell the rule from a defect. Say which it was.
        if let Some((_, asked)) = outcome.clamped.iter().find(|(idx, _)| *idx == i) {
            out.push_str(&format!(
                "           ^ day clamped from {asked} to {} - that month is shorter\n",
                outcome.after_each[i].day
            ));
        }
    }
    out.push_str(&format!("  result:  {}\n", outcome.result().to_iso()));
    // Formats and significance follow the RESULT's zone, which a `zone` step may have moved away
    // from the session zone. Without a `zone` step this equals the session zone, so the output is
    // unchanged from before.
    let result_bias = outcome.result_bias;
    out.push_str(&render_formats(&outcome.result(), result_bias));
    out.push_str(&render_metadata(&outcome.result(), today, calendar));
    let marks = significance_of(&outcome.result(), result_bias, calendar, today, outcome.calendar_reach.as_ref());
    out.push_str(&render_significance(&outcome.result(), &marks, calendar));
    out
}

/// Render the "what this date lands on" block (7.3): the test-relevant landmarks the result lands
/// on, one per line, ready to read at a glance. This is the calculator's differentiator over an
/// online date calculator (6.2). With a calendar it also names the weekend / holiday / observed-
/// holiday landmarks. Omitted entirely when the date hits nothing notable - the block is a
/// positive signal, never a "no landmark" line to scan past.
///
/// 🔴 IT DOES NOT SAY "what this date TESTS", and the difference is the whole point of the wording.
/// What a date tests is what the tester came for, and a preset already states it in its own words on
/// the `explains:` line above. What this block holds is what the ARITHMETIC happened to land on,
/// which is often incidental to it: the age-of-majority preset run on one particular birth date
/// answered "weekend - not a business day", under a heading claiming to say what the date tests, two
/// lines under an `explains:` that said something else entirely. Both were true and the reader had to
/// pick which one to believe.
pub(crate) fn render_significance(
    civil: &chrono_core::calc::CivilDateTime,
    marks: &[chrono_core::calc::Significance],
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    if marks.is_empty() {
        return String::new();
    }
    let mut out = String::from("  what this date lands on:\n");
    for m in marks {
        out.push_str(&format!("    {}\n", m.label()));
    }
    if let Some(cal) = calendar {
        out.push_str(&format!("    ({})\n", calendar_line(civil, cal)));
    }
    out
}

/// Which calendar decided the weekend and holiday marks, named beside them rather than left to be looked
/// up. It is stated whether or not any of those marks fired, because their ABSENCE is a judgement of the
/// same calendar: a date with no weekend mark has none according to this file, and would have one under a
/// calendar whose weekend falls elsewhere in the week. A calendar read from a file also says when its
/// holidays were checked against the law, and a date before its first year says that the calendar did not
/// judge it at all (R4-S20, R4/20).
fn calendar_line(civil: &chrono_core::calc::CivilDateTime, cal: &chrono_core::calendar::Calendar) -> String {
    if let Some(first) = cal.valid_from.filter(|_| !cal.judges_year(civil.year)) {
        return format!("the {} calendar starts in {first} and does not judge this date", cal.id);
    }
    match cal.law_as_of {
        Some(law) => format!(
            "weekend and holiday marks follow the {} calendar, its holidays as the law stood on {}",
            cal.id,
            date_text(&law)
        ),
        None => format!("weekend and holiday marks follow the {} calendar", cal.id),
    }
}

/// Render a reverse-analysis result (7.3): the input, then each reading with its resolved date,
/// weekday, and the "what this date lands on" markers. Two readings mean an ambiguous numeric order
/// (04/08 is April 8 in the US, August 4 in Poland) - both are shown rather than one chosen silently.
pub(crate) fn render_analysis(
    analysis: &chrono_core::calc::DateAnalysis,
    input: &str,
    now: &chrono_core::calc::CivilDateTime,
    zone_bias_min: Option<i32>,
    // True when the caller named no `--zone` and this is the host's offset, said beside it (rule 2).
    zone_from_host: bool,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    let mut out = String::from("Chrono Mock - date analysis\n");
    let instant = analysis.readings.iter().any(|(r, _)| r.is_instant());
    if analysis.is_ambiguous() {
        // Name the RIGHT ambiguity. Two readings mean either a month/day order that differs by
        // locale, or a bare number that carries no unit - and telling a reader their epoch is a
        // locale problem sends them to change a setting that has nothing to do with it.
        let reason = if instant { "a bare number carries no unit" } else { "month/day order differs by locale" };
        out.push_str(&format!("  input:   {input}  (ambiguous - {reason})\n"));
    } else {
        out.push_str(&format!("  input:   {input}\n"));
    }
    let bias = zone_bias_min.unwrap_or(0);
    // A bare number is an instant, and an instant has a wall clock only in some zone. The readings
    // showed that wall clock with no zone anywhere near it (R4-N36), so the zone is printed above
    // them - and only for them: a date written as fields does not move with the zone.
    if instant {
        let from_host = if zone_from_host { ", from the host" } else { "" };
        // A date with its own offset is an instant for another reason than a number is (R5-D5, item 5).
        let offset = analysis.readings.iter().any(|(r, _)| *r == chrono_core::calc::DateReading::IsoOffset);
        let what = if offset { "the date carries its own UTC offset, so it is an instant" } else { "the number is an instant" };
        out.push_str(&format!("  zone:    {}{from_host}  ({what}, shown in this zone)\n", format_bias(bias)));
    }
    let written = time_written(input);
    for (reading, civil) in &analysis.readings {
        let weekday = chrono_core::calc::metadata(civil, now).weekday;
        out.push_str(&format!(
            "  {}:  {}  ({weekday})\n",
            reading.label(),
            reading_text(*reading, civil, written)
        ));
        for m in significance_of(civil, bias, calendar, now, None) {
            out.push_str(&format!("      {}\n", m.label()));
        }
    }
    out
}

/// One reading as the analysis prints it.
///
/// The year goes through `to_iso`, which writes a year before 1 CE the way the parser reads it back:
/// `{:04}` gave `-009` (R4-N36). The time of day is printed whenever the reading has one to show -
/// always for an epoch reading, which names an instant down to the second and used to be cut to its
/// date, and for an ISO input that wrote a time, midnight included (`time_written`). A date written
/// without a time keeps the short form.
fn reading_text(
    reading: chrono_core::calc::DateReading,
    civil: &chrono_core::calc::CivilDateTime,
    time_written: bool,
) -> String {
    let iso = civil.to_iso();
    if reading_shows_time(reading, time_written) {
        return iso;
    }
    iso.split('T').next().unwrap_or(&iso).to_string()
}

/// Whether the input wrote a time of day, read the way `analyze_date` reads it. A time written as
/// midnight is still a time someone wrote, and the reading repeats it rather than dropping it.
fn time_written(input: &str) -> bool {
    input.trim().contains(['T', ' '])
}

/// Whether a reading has a time of day to show: always for an epoch reading, which names an instant
/// down to the second, and for an ISO reading when the input wrote a time. One rule for the text output
/// and for the `time` field of the JSON one, which the window shows.
fn reading_shows_time(reading: chrono_core::calc::DateReading, time_written: bool) -> bool {
    match reading {
        chrono_core::calc::DateReading::Iso => time_written,
        _ => reading.is_instant(),
    }
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
        // Before the calendar's first year the calendar has no answer, and "no" would be one (R4-S20).
        if let Some(first) = cal.valid_from.filter(|_| !cal.judges_year(civil.year)) {
            out.push_str(&format!(
                "    business day  not judged - the {} calendar starts in {first}\n",
                cal.id
            ));
            out.push_str("    holiday       not judged\n");
            return out;
        }
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

    /// A date with its own offset is shown as the instant it is, in the session zone, with the zone
    /// line saying why - not the line written for a bare number (R5-D5, item 5).
    #[test]
    fn a_date_with_an_offset_is_analysed_as_an_instant() {
        let input = "2026-01-01T10:00:00+02:00";
        let analysis = chrono_core::calc::analyze_date(input, 0).unwrap();
        let now = chrono_core::calc::parse_civil_datetime("2026-06-01T00:00:00").unwrap();
        let text = render_analysis(&analysis, input, &now, Some(0), false, None);
        assert!(text.contains("ISO 8601 with UTC offset:  2026-01-01T08:00:00"), "{text}");
        assert!(text.contains("the date carries its own UTC offset, so it is an instant"), "{text}");
        assert!(!text.contains("the number is an instant"), "{text}");
        let json = calc_analysis_json(&analysis, input, &now, Some(0), None);
        assert!(json.contains(r#""reading":"iso_offset""#) && json.contains(r#""instant":true"#), "{json}");
    }

    /// A calendar or preset failure ends in a key the window translates (R5-D5, item 2), and the key
    /// says which failure it is. A bad file and a bad value for its parameters are two keys, because
    /// one is fixed in the file and the other on the command line or in the window's fields.
    #[test]
    fn calendar_and_preset_failures_end_in_their_own_key() {
        use crate::preset::PresetError;
        let line = |e: PresetError, stage| preset_refusal(&e, stage);
        assert!(line(PresetError::NotFound("gone".into()), PresetStage::File).ends_with("gone (calc.preset_not_found)"));
        assert!(line(PresetError::NotBuilt("later".into()), PresetStage::Parameters).ends_with("(calc.preset_not_built)"));
        assert!(line(PresetError::BadFile("bad".into()), PresetStage::File).ends_with("bad (calc.preset_invalid)"));
        assert!(line(PresetError::BadFile("no value".into()), PresetStage::Parameters).ends_with("(calc.preset_parameter)"));

        // The test runs where no calendar folder answers, so a valid id is not found, and an id that
        // cannot be a file name is an argument to fix.
        let missing = load_named_calendar("no-such-calendar").expect_err("not found");
        assert!(missing.starts_with("chrono calc: ") && missing.ends_with("(calc.calendar_not_found)"), "{missing}");
        let bad_id = load_named_calendar("../pl").expect_err("refused");
        assert!(bad_id.ends_with("(calc.bad_argument)"), "{bad_id}");
    }

    #[test]
    fn calc_exit_codes_split_bad_input_from_needs_data() {
        // Not built / needs data -> code 5 - bad input -> usage 1.
        assert_eq!(calc_error_exit_code(&EvalError::StepUnsupported { kind: "zone", index: 0 }), 5);
        assert_eq!(calc_error_exit_code(&EvalError::NeedsCalendar { index: 0 }), 5);
        // R2-S10: a degenerate calendar is BAD INPUT (exit 1), not an unbuilt operation (exit 5).
        // It used to travel as `NotFound` -> 5 from `--nearest` and as `Overflow` -> 1 from `+Nbd`:
        // two answers, one cause, and the 5 told a script the feature does not exist yet.
        assert_eq!(calc_error_exit_code(&EvalError::DegenerateCalendar { index: 0 }), 1);
        assert_eq!(calc_error_exit_code(&EvalError::Overflow { index: 0 }), 1);
        assert_eq!(calc_error_exit_code(&EvalError::BadSetTime { index: 0 }), 1);
        // R4-N35: past the business-day limit is bad input too, and it says so by the limit's own key.
        assert_eq!(calc_error_exit_code(&EvalError::TooManyBusinessDays { index: 0 }), 1);
        let msg = describe_calc_error(&EvalError::TooManyBusinessDays { index: 1 });
        assert!(
            msg.contains("step 2") && msg.contains("1000000") && msg.contains("(calc.business_days_limit)"),
            "got: {msg}"
        );
        assert!(!msg.contains("overflow"), "the number is fine - only the limit is past, got: {msg}");
        assert!(msg.contains("Lower the counts so they add up to 1000000 or less"), "says what to do: {msg}");
    }

    #[test]
    fn calc_error_message_carries_key_and_one_based_step() {
        let msg = describe_calc_error(&EvalError::NeedsCalendar { index: 0 });
        assert!(msg.contains("step 1"), "1-based step number, got: {msg}");
        assert!(msg.contains("calc.needs_calendar"), "stable key, got: {msg}");
        let msg = describe_calc_error(&EvalError::StepUnsupported { kind: "zone", index: 2 });
        assert!(msg.contains("step 3") && msg.contains("calc.step_unsupported"), "got: {msg}");
    }

    /// `--base-utc` reads its value as an instant, and the resulting moment must be the SAME instant
    /// in every session zone - the whole reason the flag exists. Asserted as epoch seconds, because
    /// the wall-clock reading legitimately differs per zone while the instant may not.
    #[test]
    fn base_utc_names_one_instant_whatever_the_session_zone_is() {
        for bias in [0, -120, 300, -345] {
            let ca = parse_calc_args(&["--base-utc".into(), "1970-01-01T00:00:00Z".into()]).unwrap();
            let expr = MomentExpr { base: ca.base, steps: ca.steps };
            let now = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
            let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: bias, calendar: None }).unwrap();
            assert_eq!(
                chrono_core::calc::formats(&out.result(), bias).epoch_seconds,
                Some(0),
                "bias {bias} must still name epoch 0"
            );
        }

        // The plain --base is unchanged: it reads the session zone, so the same text is a DIFFERENT
        // instant per zone. Without this the two flags could quietly become one.
        let plain = parse_calc_args(&["--base".into(), "1970-01-01T00:00:00".into()]).unwrap();
        let expr = MomentExpr { base: plain.base, steps: plain.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: -120, calendar: None }).unwrap();
        assert_eq!(chrono_core::calc::formats(&out.result(), -120).epoch_seconds, Some(-7200));
    }

    /// Two base flags are a contradiction, not a preference order - "last one wins" would move the
    /// moment by the zone offset in silence.
    #[test]
    fn two_base_flags_are_refused_naming_both() {
        for argv in [
            vec!["--base".to_string(), "2026-01-01T00:00:00".into(), "--base-utc".into(), "2026-01-01T00:00:00".into()],
            vec!["--base-utc".to_string(), "2026-01-01T00:00:00".into(), "--base".into(), "2026-01-01T00:00:00".into()],
            // A repeat of the same flag is the same mistake, and the same silence.
            vec!["--base".to_string(), "2026-01-01T00:00:00".into(), "--base".into(), "2027-01-01T00:00:00".into()],
        ] {
            let err = parse_calc_args(&argv).err().expect("a second base must be refused");
            assert!(err.contains("--base"), "the message must name the flags: {err}");
        }
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
            valid_from: None,
            law_as_of: None,
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

    /// R4-S20 and R4/20 in the text: a date before the calendar's first year is "not judged", with the
    /// year, where it used to read "no" - an answer the calendar had no rules for - and a calendar read
    /// from a file names the day it was checked against the law under the marks it decided.
    #[test]
    fn the_text_says_what_a_calendar_does_not_judge_and_when_it_was_checked() {
        let day = |d: u32| chrono_core::calc::CivilDateTime { year: 2026, month: 7, day: d, hour: 0, minute: 0, second: 0 };
        let later = chrono_core::calendar::Calendar { valid_from: Some(2030), ..test_calendar() };
        let text = render_metadata(&day(4), &day(4), Some(&later));
        assert!(text.contains("business day  not judged - the us-test calendar starts in 2030"), "got:\n{text}");
        assert!(text.contains("holiday       not judged"), "got:\n{text}");
        assert!(!text.contains("Independence Day"), "got:\n{text}");
        let marks = significance_of(&day(4), 0, Some(&later), &day(4), None);
        assert_eq!(marks, vec![chrono_core::calc::Significance::BeforeCalendar]);
        let block = render_significance(&day(4), &marks, Some(&later));
        assert!(block.contains("(the us-test calendar starts in 2030 and does not judge this date)"), "got:\n{block}");

        let checked = chrono_core::calendar::Calendar { law_as_of: Some(day(1)), ..test_calendar() };
        let marks = significance_of(&day(4), 0, Some(&checked), &day(4), None);
        let block = render_significance(&day(4), &marks, Some(&checked));
        assert!(
            block.contains("(weekend and holiday marks follow the us-test calendar, its holidays as the law stood on 2026-07-01)"),
            "got:\n{block}"
        );
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
        assert!(text.contains("what this date lands on:"), "got:\n{text}");
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
        assert!(!text.contains("what this date lands on:"), "got:\n{text}");
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
        assert!(text.contains("what this date lands on:"), "got:\n{text}");
        assert!(text.contains("weekend - not a business day"), "got:\n{text}");
        assert!(text.contains("public holiday"), "got:\n{text}");
        // Without a calendar the same date names no calendar landmark (and here nothing at all).
        assert!(!render_calc(&expr, &out, Some(0), false, &now, None, None).contains("what this date lands on:"));
    }

    #[test]
    fn the_block_names_the_calendar_that_judged_the_weekend_and_holiday_marks() {
        // "weekend - not a business day" is a judgement, not a fact: the same Saturday is a business
        // day under a calendar whose weekend falls elsewhere. So the block says whose judgement it is.
        let ca = parse_calc_args(&["--base".into(), "2026-07-04T00:00:00".into()]).unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let cal = test_calendar();
        let text = render_calc(&expr, &out, Some(0), false, &now, Some(&cal), None);
        assert!(
            text.contains("(weekend and holiday marks follow the us-test calendar)"),
            "the block does not say which calendar judged it, got:\n{text}"
        );

        // A date that hits a landmark of its own but no calendar landmark still gets the note, because
        // the ABSENCE of a weekend mark is that calendar's judgement too.
        let ca = parse_calc_args(&["--base".into(), "2026-12-31T00:00:00".into()]).unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, Some(0), false, &now, Some(&cal), None);
        assert!(text.contains("last day of the year"), "got:\n{text}");
        assert!(text.contains("marks follow the us-test calendar"), "got:\n{text}");

        // With no calendar there is nothing to attribute, so the note is absent rather than vague.
        let text = render_calc(&expr, &out, Some(0), false, &now, None, None);
        assert!(!text.contains("marks follow"), "got:\n{text}");
    }

    #[test]
    fn the_block_does_not_claim_to_say_what_a_preset_tests() {
        // The two claims used to share one name. A preset states what its date TESTS in its own words
        // on the explains line, and this block states what the arithmetic LANDED ON - which is often
        // incidental to it. Both appear in one render, so they must not both be called the same thing.
        let ca = parse_calc_args(&["--base".into(), "2026-07-04T00:00:00".into()]).unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let cal = test_calendar();
        let header = "  preset:   demo - Demo\n  explains: Does the app do the thing?\n";
        let text = render_calc(&expr, &out, Some(0), false, &now, Some(&cal), Some(header));
        assert!(text.contains("explains: Does the app do the thing?"), "got:\n{text}");
        assert!(text.contains("what this date lands on:"), "got:\n{text}");
        assert!(!text.contains("what this date tests"), "got:\n{text}");
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
        let analysis = chrono_core::calc::analyze_date("04/08/2008", 0).unwrap();
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 25, hour: 0, minute: 0, second: 0 };
        let text = render_analysis(&analysis, "04/08/2008", &now, Some(0), false, None);
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
        // No calendar supplied, so the calendar-dependent metadata is null (never guessed) - including
        // the calendar name itself, which is null rather than an empty string a reader could mistake
        // for a calendar with no id.
        assert!(v["moment"]["metadata"]["business_day"].is_null());
        assert!(v["moment"]["metadata"]["calendar"].is_null());
        let sig = v["moment"]["significance"].as_array().unwrap();
        assert!(sig.iter().any(|s| s == "end_of_quarter"), "got: {sig:?}");
    }

    #[test]
    fn calc_moment_json_names_the_calendar_that_decided_the_business_day() {
        // The text output has always named it beside the verdict, and the machine output did not, so
        // a GUI reading business_day over the wire could not say whose rule it was showing.
        let base = chrono_core::calc::CivilDateTime { year: 2026, month: 7, day: 4, hour: 0, minute: 0, second: 0 };
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let expr = MomentExpr { base: Base::Absolute(base), steps: vec![] };
        let cal = test_calendar();
        let outcome =
            chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let json = calc_moment_json(&outcome, &now, Some(&cal), None, None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["moment"]["metadata"]["calendar"], "us-test");
        assert_eq!(v["moment"]["metadata"]["business_day"], false);
    }

    #[test]
    fn calc_analysis_json_shows_both_readings_with_stable_keys() {
        let analysis = chrono_core::calc::analyze_date("04/08/2008", 0).unwrap();
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
        let analysis = chrono_core::calc::analyze_date("07/04/2026", 0).unwrap();
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let cal = test_calendar();
        let text = render_analysis(&analysis, "07/04/2026", &now, Some(0), false, Some(&cal));
        assert!(text.contains("public holiday"), "got:\n{text}");
    }

    #[test]
    fn parse_calc_reads_the_format_mask_and_rejects_empty() {
        let ca = parse_calc_args(&["--format".into(), "dd.MM.yyyy".into()]).unwrap();
        assert_eq!(ca.format.as_deref(), Some("dd.MM.yyyy"));
        assert!(parse_calc_args(&["--format".into(), String::new()]).is_err());
    }

    /// `--preset` supplies its own moment, so combining it with a step flag (or --analyze) is a
    /// usage error - alone it parses.
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

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| (*w).to_string()).collect()
    }

    /// R4-N32: `--format --json` took `--json` as the mask and printed text where JSON was asked for.
    /// A value is the word after its flag, and a word that is itself a flag is named as one.
    #[test]
    fn a_flag_where_a_value_should_be_is_refused_by_name() {
        for flag in [
            "--base", "--base-utc", "--param", "--preset", "--calendar", "--format", "--zone", "--to-zone",
            "--shift", "--set-time", "--snap", "--nearest",
        ] {
            let err = parse_calc_args(&words(&[flag, "--json"]))
                .err()
                .unwrap_or_else(|| panic!("{flag} --json must be refused"));
            assert!(
                err.contains(&format!("{flag} needs")) && err.contains("the flag '--json'"),
                "{flag}: {err}"
            );
        }
        // One dash is a sign, not a flag.
        assert_eq!(parse_calc_args(&words(&["--shift", "-1d"])).unwrap().steps.len(), 1);
        assert_eq!(parse_calc_args(&words(&["--zone", "-05:00"])).unwrap().zone_bias_min, Some(300));
        // A mask that has to open with two dashes quotes them, so nothing a mask can say is lost.
        assert_eq!(
            parse_calc_args(&words(&["--format", "'--'yyyy"])).unwrap().format.as_deref(),
            Some("'--'yyyy")
        );
    }

    /// `--analyze` takes pasted text as it comes: a typo with two dashes in front of a date is the
    /// analyser's to refuse, under the key the analysis panel translates.
    #[test]
    fn analyze_takes_pasted_text_as_it_comes() {
        let ca = parse_calc_args(&words(&["--analyze", "--2026-01-01"])).unwrap();
        assert_eq!(ca.analyze.as_deref(), Some("--2026-01-01"));
    }

    /// R4-N32: for these flags the last one won and the first was dropped without a word.
    #[test]
    fn a_flag_given_twice_is_refused_not_overwritten() {
        for (flag, first, second) in [
            ("--zone", "+01:00", "+02:00"),
            ("--calendar", "pl", "us-banking"),
            ("--preset", "month-end", "quarter-end"),
            ("--format", "yyyy", "dd"),
            ("--analyze", "04/08/2008", "05/05/2008"),
        ] {
            let err = parse_calc_args(&words(&[flag, first, flag, second]))
                .err()
                .unwrap_or_else(|| panic!("{flag} twice must be refused"));
            assert!(err.contains(&format!("{flag} is given twice")), "{err}");
        }
        let err = parse_calc_args(&words(&["--preset", "p", "--param", "a=1", "--param", "a=2"]))
            .err()
            .expect("one parameter given two values must be refused");
        assert!(err.contains("--param a is given twice"), "{err}");

        // What was never a contradiction stays allowed: two parameters, two steps of one kind.
        assert_eq!(parse_calc_args(&words(&["--preset", "p", "--param", "a=1", "--param", "b=2"])).unwrap().params.len(), 2);
        assert_eq!(parse_calc_args(&words(&["--shift", "+1d", "--shift", "+1d"])).unwrap().steps.len(), 2);
    }

    /// R4-N32: `--analyze` dropped the build flags and `--format` without a word, so the answer was to a
    /// question nobody asked. What does apply to an analysis - the zone and the calendar - still does.
    #[test]
    fn analyze_refuses_what_it_used_to_ignore() {
        for extra in [
            ["--shift", "+1d"],
            ["--base", "today"],
            ["--base-utc", "1970-01-01T00:00:00"],
            ["--set-time", "12:00:00"],
            ["--snap", "eom"],
            ["--nearest", "nbd"],
            ["--to-zone", "+05:45"],
            ["--format", "yyyy"],
        ] {
            for argv in [
                [&["--analyze", "04/08/2008"][..], &extra[..]].concat(),
                [&extra[..], &["--analyze", "04/08/2008"][..]].concat(),
            ] {
                let err = parse_calc_args(&words(&argv)).err().unwrap_or_else(|| panic!("{argv:?}"));
                assert!(!err.contains(';'), "rule 13 holds in refusals too: {err}");
            }
        }
        let ca = parse_calc_args(&words(&["--analyze", "0", "--zone", "+02:00", "--calendar", "pl", "--json"])).unwrap();
        assert_eq!((ca.zone_bias_min, ca.calendar.as_deref(), ca.json), (Some(-120), Some("pl"), true));
    }

    /// R4-N36: a negative year printed `-009`, which the parser cannot read back, and an epoch reading
    /// was cut to its date while the JSON beside it carried the time.
    #[test]
    fn an_analysis_prints_the_year_in_full_and_an_instant_with_its_time() {
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let past = chrono_core::calc::analyze_date("-0009-05-05", 0).unwrap();
        let text = render_analysis(&past, "-0009-05-05", &now, Some(0), false, None);
        assert!(text.contains("ISO 8601:  -0009-05-05  ("), "got:\n{text}");

        let epoch = chrono_core::calc::analyze_date("1740607200", -120).unwrap();
        let text = render_analysis(&epoch, "1740607200", &now, Some(-120), true, None);
        assert!(text.contains("Unix epoch seconds:  2025-02-27T00:00:00  ("), "got:\n{text}");
        assert!(text.contains("zone:    +02:00, from the host"), "the zone of the instant is named: {text}");

        // A date written as fields has no time of its own and no zone to name.
        let date = chrono_core::calc::analyze_date("04/08/2008", -120).unwrap();
        let text = render_analysis(&date, "04/08/2008", &now, Some(-120), true, None);
        assert!(text.contains("US MM/DD/YYYY:  2008-04-08  ("), "got:\n{text}");
        assert!(!text.contains("zone:"), "got:\n{text}");

        // An ISO input keeps the time it wrote - midnight included - and one without a time gets none.
        for (input, shown) in [
            ("2026-01-31T13:45:00", "2026-01-31T13:45:00"),
            ("2026-01-31T00:00:00", "2026-01-31T00:00:00"),
            ("2026-01-31 00:00:00", "2026-01-31T00:00:00"),
            ("2026-01-31", "2026-01-31"),
        ] {
            let analysis = chrono_core::calc::analyze_date(input, 0).unwrap();
            let text = render_analysis(&analysis, input, &now, Some(0), false, None);
            assert!(text.contains(&format!("ISO 8601:  {shown}  (")), "{input}, got:\n{text}");
        }
    }

    /// R4-N36: the JSON analysis carried wall clocks in the session zone and no zone at all.
    #[test]
    fn the_analysis_json_names_its_zone() {
        let analysis = chrono_core::calc::analyze_date("0", -120).unwrap();
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let v: serde_json::Value =
            serde_json::from_str(&calc_analysis_json(&analysis, "0", &now, Some(-120), None)).unwrap();
        assert_eq!(v["analysis"]["zone_bias_min"], -120);
        assert_eq!(v["analysis"]["readings"][0]["iso"], "1970-01-01T02:00:00");
    }

    /// R4-S23: the window cut every reading to its date, so an epoch reading lost the time it names and
    /// nothing said it was an instant. Each reading now says both, by the rule the text output prints by.
    #[test]
    fn each_reading_says_whether_it_is_an_instant_and_which_time_to_show() {
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let readings = |input: &str| {
            let analysis = chrono_core::calc::analyze_date(input, -120).unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&calc_analysis_json(&analysis, input, &now, Some(-120), None)).unwrap();
            v["analysis"]["readings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| (r["reading"].as_str().unwrap().to_string(), r["instant"].clone(), r["time"].clone()))
                .collect::<Vec<_>>()
        };
        let instant = |key: &str, time: &str| (key.to_string(), serde_json::json!(true), serde_json::json!(time));
        let date = |key: &str| (key.to_string(), serde_json::json!(false), serde_json::Value::Null);

        assert_eq!(
            readings("1740607200"),
            [instant("epoch_seconds", "00:00:00"), instant("epoch_millis", "05:30:07")],
            "an instant always shows its time, midnight included"
        );
        assert_eq!(readings("2026-01-31T13:45:00"), [("iso".to_string(), serde_json::json!(false), serde_json::json!("13:45:00"))]);
        assert_eq!(readings("2026-01-31T00:00:00"), [("iso".to_string(), serde_json::json!(false), serde_json::json!("00:00:00"))]);
        assert_eq!(readings("2026-01-31"), [date("iso")]);
        assert_eq!(readings("04/08/2008"), [date("us_month_day"), date("pl_day_month")]);
    }
}
