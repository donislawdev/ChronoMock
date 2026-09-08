//! Presets: a named moment with parameters, read from the shared catalogue (docs/04 section 4).
//!
//! Two layers, deliberately: the `*Dto` types are the on-disk schema `chronomock.preset/1`, and the
//! types above them are what the rest of the tool works with. A preset carries an EXPRESSION rather
//! than a date, so the same file answers differently on different days - `resolve_parameters` and
//! `resolve_moment` turn it into a concrete moment.
//!
//! The step grammar it accepts is `crate::grammar`, the same one the calculator flags use. As with
//! the calendar reader, this consumer owns the I/O and the serde and the core engine stays pure.
//!
//! 🔴 SECURITY (docs/04 4.1): a preset describes TIME, never a TARGET. The schema has no path field,
//! so a shared preset cannot smuggle an executable path - enforced structurally, because there is no
//! field to put it in, which `preset_ignores_a_path_field` pins. Unknown fields are ignored (additive
//! evolution, docs/04 section 3) - an unknown major schema version is refused (section 3.1).


use std::collections::HashMap;

use serde::Deserialize;

use chrono_core::calc::{Base, MomentExpr, Sign, Step, Unit};
use chrono_core::filetime_utc_to_wall;

use crate::calendar::{catalogue_search_places, find_catalogue_file, is_valid_catalogue_id};
use crate::grammar::{parse_nearest, parse_set_time, parse_snap, parse_unit};
use crate::zone::parse_zone_to_bias;
/// A parsed preset: its declared parameters and its RAW moment (docs/04 4.3), not yet resolved to a
/// `MomentExpr` - because a parametric base/shift needs values (`--param` / `default`) that the file
/// alone does not carry. `resolve_parameters` + `resolve_moment` turn it into a concrete moment.
/// A non-parametric preset (slices 16/17) has empty `parameters` and resolves trivially. Also carries
/// the human framing (calculator) and the time mode (substitution) - the calculator ignores time_mode.
#[derive(Debug)]
pub(crate) struct Preset {
    pub(crate) id: String,
    pub(crate) name_en: String,
    pub(crate) explains_en: String,
    /// `calculator` / `substitution` / `both` (docs/04 4.2). Each surface honours it.
    pub(crate) applies_to: String,
    pub(crate) parameters: Vec<Parameter>,
    pub(crate) moment: MomentDto,
    pub(crate) time_mode: PresetTimeMode,
}

/// A preset parameter (docs/04 4.2): a typed slot filled by `--param`, a file `default`, or (in a
/// substitution session, a later slice) a `default_hint` such as the target's file date.
#[derive(Debug)]
pub(crate) struct Parameter {
    id: String,
    kind: ParamKind,
    default: Option<ParamValue>,
    /// Where to propose a value from when neither `--param` nor `default` is given (docs/04 4.2).
    /// `target_file_creation` needs a target, so it is honoured only in `run` (a later slice) - the
    /// calculator, having no target, reports it as a value the user must supply.
    default_hint: Option<String>,
}

/// A parameter's type. `date` fills a base - `duration` and `variant` fill a shift (a `variant` by a
/// signed day offset - docs/05 3.6). (`int` is later.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParamKind {
    Date,
    Duration,
    Variant,
}

/// A resolved parameter value, ready to substitute into the moment.
#[derive(Debug, Clone)]
pub(crate) enum ParamValue {
    Date(chrono_core::calc::CivilDateTime),
    Duration { amount: i64, unit: Unit },
    /// A boundary variant (docs/05 3.6) resolved to a signed day offset: day_before -1, on_day 0,
    /// day_after +1. It fills a shift and carries its own direction, so that shift step needs no sign.
    Variant(i64),
}

/// A preset's time mode, resolved to the substitution surface's wire shape (the same `mode` /
/// `multiplier` / `scale_duration` a `run` session carries). The contract carries `multiplier` and
/// `scale_duration_clock` (docs/04 4.2) - `multiplier == 1` is real-time `flow`, `> 1` is `xN`.
#[derive(Debug, Clone)]
pub(crate) struct PresetTimeMode {
    /// Wire mode token: "flow" or "multiplier". (Presets do not express "frozen".)
    pub(crate) mode: String,
    pub(crate) multiplier: Option<i64>,
    pub(crate) scale_duration: bool,
}

impl Default for PresetTimeMode {
    /// A preset with no `time_mode` (e.g. a calculator-only one) runs at real speed.
    fn default() -> Self {
        Self { mode: "flow".into(), multiplier: None, scale_duration: false }
    }
}

/// Why a preset could not be loaded for the calculator. Enumerated, not a shared string, so the
/// exit code follows the cause: input problems are usage errors (1), while "the model has this in
/// it but calc does not resolve it yet" is the honest not-built code (5), same split as calc's own
/// error table (docs/08 section 9a).
#[derive(Debug)]
pub(crate) enum PresetError {
    /// No such preset file.
    NotFound(String),
    /// Bad JSON, unknown schema, or a malformed field.
    BadFile(String),
    /// A shape the model allows but this build does not resolve yet (parameters).
    NotBuilt(String),
}

impl PresetError {
    pub(crate) fn exit_code(&self) -> i32 {
        match self {
            PresetError::NotBuilt(_) => 5,
            PresetError::NotFound(_) | PresetError::BadFile(_) => 1,
        }
    }
    pub(crate) fn message(&self) -> &str {
        match self {
            PresetError::NotFound(m) | PresetError::BadFile(m) | PresetError::NotBuilt(m) => m,
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct PresetDto {
    schema: String,
    id: String,
    name: PresetTextDto,
    explains: PresetTextDto,
    applies_to: String,
    // Typed parameters (docs/04 4.2): each is filled by --param, a file default, or a default_hint.
    // The moment's parametric base/shift refer to these by id - resolve_parameters + resolve_moment
    // substitute them into a concrete MomentExpr (the core never learns a parameter existed).
    #[serde(default)]
    parameters: Vec<ParameterDto>,
    moment: MomentDto,
    // Only substitution/both presets carry a time mode - a calculator-only one may omit it (the
    // calculator ignores it either way). Absent = real-time flow (PresetTimeMode::default).
    #[serde(default)]
    time_mode: Option<TimeModeDto>,
}

/// A preset's `time_mode` object (docs/04 4.2): `{ "multiplier": N, "scale_duration_clock": bool }`.
#[derive(Deserialize)]
pub(crate) struct TimeModeDto {
    #[serde(default)]
    multiplier: Option<i64>,
    #[serde(default)]
    scale_duration_clock: bool,
}

/// A preset parameter as written in the file (docs/04 4.2): `{ "id", "type", "default"?, "default_hint"? }`.
/// `default` shape depends on `type` (a string for `date`, `{ amount, unit }` for `duration`), so it
/// stays a raw value here and is parsed against the type in `parse_parameter`.
#[derive(Deserialize)]
pub(crate) struct ParameterDto {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    default: Option<serde_json::Value>,
    #[serde(default)]
    default_hint: Option<String>,
}

/// A `duration` value/`default` object: `{ "amount": N, "unit": "days" }` (docs/04 4.2).
#[derive(Deserialize)]
pub(crate) struct DurationDto {
    amount: i64,
    unit: String,
}

/// The English text is what the CLI renders (rule 15 - CLI is English only) - `pl` rides in the file
/// for the GUI but this reader does not need it, so it is not a field here (unknown fields ignored).
#[derive(Deserialize)]
pub(crate) struct PresetTextDto {
    en: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MomentDto {
    base: BaseDto,
    #[serde(default)]
    steps: Vec<StepDto>,
}

/// A preset base: the keyword `today`/`now`, an `{ "absolute": "ISO" }` object, an
/// `{ "absolute_utc": "ISO" }` object, or a `{ "parameter": "name" }` object (docs/04 4.2)
/// resolved from a `date` parameter.
///
/// `absolute` and `absolute_utc` differ in the one way that matters: `absolute` is wall-clock in
/// the SESSION zone (rule 2), `absolute_utc` is an instant. A preset whose meaning is "9 AM local"
/// wants the first, a preset whose meaning is "epoch second 0" wants the second - and using the
/// first for the second is how these presets came to miss their target by exactly the zone offset.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum BaseDto {
    Keyword(String),
    Object {
        #[serde(default)]
        absolute: Option<String>,
        #[serde(default)]
        absolute_utc: Option<String>,
        #[serde(default)]
        parameter: Option<String>,
    },
}

/// One preset step, externally tagged exactly as docs/04 4.2 writes it: `{ "shift": {...} }`,
/// `{ "set_time": "HH:MM:SS" }`, `{ "snap": "end-of-month" }`, `{ "nearest": "next-business-day" }`,
/// `{ "zone": "+05:45" }`. The string forms reuse the CLI parsers, keeping one grammar.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StepDto {
    Shift(ShiftDto),
    SetTime(String),
    Snap(String),
    Nearest(String),
    Zone(String),
}

/// A `shift` step in a preset: a literal `{ sign, amount, unit }`, a parametric `{ sign, parameter }`
/// resolved from a `duration`, or a parametric `{ parameter }` resolved from a `variant` (docs/04 4.2).
/// The sign is optional because a `variant` parameter carries its own direction (docs/05 3.6).
#[derive(Debug, Deserialize)]
pub(crate) struct ShiftDto {
    #[serde(default)]
    sign: Option<String>,
    #[serde(default)]
    amount: Option<i64>,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    parameter: Option<String>,
}

/// Whether a preset's `applies_to` makes it a calculator question (docs/04 4.2, docs/05 3.1).
pub(crate) fn preset_targets_calculator(applies_to: &str) -> bool {
    matches!(applies_to, "calculator" | "both")
}

/// Map a preset base to the core `Base`. A parametric base is refused (not built), never silently
/// treated as `today`.
pub(crate) fn base_from(dto: BaseDto, values: &HashMap<String, ParamValue>) -> Result<Base, PresetError> {
    match dto {
        BaseDto::Keyword(k) => match k.as_str() {
            "today" => Ok(Base::Today),
            "now" => Ok(Base::Now),
            other => Err(PresetError::BadFile(format!(
                "unknown preset base '{other}' (use today, now, or an absolute/absolute_utc/parameter object)"
            ))),
        },
        // A parametric base takes its date from a `date` parameter (docs/04 4.2).
        BaseDto::Object { parameter: Some(id), .. } => match values.get(&id) {
            Some(ParamValue::Date(civil)) => Ok(Base::Absolute(*civil)),
            Some(ParamValue::Duration { .. }) => {
                Err(PresetError::BadFile(format!("base parameter '{id}' must be a date, not a duration")))
            }
            Some(ParamValue::Variant(_)) => {
                Err(PresetError::BadFile(format!("base parameter '{id}' must be a date, not a variant")))
            }
            None => Err(PresetError::BadFile(format!("base parameter '{id}' has no value"))),
        },
        // Both at once is a contradiction, not a preference order: the file says the moment is both
        // local wall-clock and a fixed instant, and picking one silently would make the preset land
        // somewhere the author did not write.
        BaseDto::Object { absolute: Some(_), absolute_utc: Some(_), parameter: None } => {
            Err(PresetError::BadFile(
                "preset base has both 'absolute' and 'absolute_utc' - a moment is either session-zone wall-clock or a UTC instant, not both".into(),
            ))
        }
        BaseDto::Object { absolute: Some(s), absolute_utc: None, parameter: None } => {
            let civil = chrono_core::calc::parse_civil_datetime(&s).map_err(PresetError::BadFile)?;
            Ok(Base::Absolute(civil))
        }
        // A trailing `Z` is accepted and required to BE UTC: the field already says so, and writing
        // an offset here (`+02:00`) would be a second, contradicting answer - refused rather than
        // ignored, because ignoring it moves the moment without saying so.
        BaseDto::Object { absolute: None, absolute_utc: Some(s), parameter: None } => {
            let trimmed = s.strip_suffix('Z').unwrap_or(&s);
            // Look for the offset in the TIME part only. A negative year is a legal civil date here
            // (the core computes on a band far wider than the epoch), and its leading `-` must not
            // read as an offset sign.
            let time_part = trimmed.split_once(['T', ' ']).map_or("", |(_, t)| t);
            if time_part.contains('+') || time_part.contains('-') {
                return Err(PresetError::BadFile(format!(
                    "preset base 'absolute_utc' carries an offset ('{s}') - it is already UTC, so write it without one (a trailing 'Z' is allowed)"
                )));
            }
            let civil = chrono_core::calc::parse_civil_datetime(trimmed).map_err(PresetError::BadFile)?;
            Ok(Base::AbsoluteUtc(civil))
        }
        BaseDto::Object { absolute: None, absolute_utc: None, parameter: None } => Err(PresetError::BadFile(
            "preset base object needs 'absolute', 'absolute_utc' or 'parameter'".into(),
        )),
    }
}

/// Map a preset step to a core `Step`, reusing the CLI parsers so a preset speaks the same step
/// grammar as the flags. A `parameter` shift is resolved from the values map.
pub(crate) fn step_from(dto: StepDto, values: &HashMap<String, ParamValue>) -> Result<Step, PresetError> {
    match dto {
        StepDto::Shift(s) => shift_from(s, values),
        StepDto::SetTime(raw) => parse_set_time(&raw).map_err(PresetError::BadFile),
        StepDto::Snap(raw) => parse_snap(&raw).map(Step::Snap).map_err(PresetError::BadFile),
        StepDto::Nearest(raw) => parse_nearest(&raw).map(Step::Nearest).map_err(PresetError::BadFile),
        StepDto::Zone(raw) => parse_zone_to_bias(&raw).map(Step::Zone).map_err(PresetError::BadFile),
    }
}

pub(crate) fn shift_from(s: ShiftDto, values: &HashMap<String, ParamValue>) -> Result<Step, PresetError> {
    // A parametric shift takes its shape from a parameter: a `duration` gives magnitude and unit (the
    // step's sign carries direction), a `variant` gives a signed day offset (carrying its own sign).
    if let Some(id) = &s.parameter {
        return match values.get(id) {
            Some(ParamValue::Duration { amount, unit }) => {
                Ok(Step::Shift { sign: parse_shift_sign(&s.sign)?, amount: *amount, unit: *unit })
            }
            Some(ParamValue::Variant(days)) => {
                let sign = if *days < 0 { Sign::Minus } else { Sign::Plus };
                Ok(Step::Shift { sign, amount: days.abs(), unit: Unit::Days })
            }
            Some(ParamValue::Date(_)) => {
                Err(PresetError::BadFile(format!("shift parameter '{id}' must be a duration or variant, not a date")))
            }
            None => Err(PresetError::BadFile(format!("shift parameter '{id}' has no value"))),
        };
    }
    let sign = parse_shift_sign(&s.sign)?;
    let amount = s.amount.ok_or_else(|| PresetError::BadFile("shift needs an amount or a parameter".into()))?;
    if amount < 0 {
        return Err(PresetError::BadFile("shift amount must be non-negative (the sign carries direction)".into()));
    }
    let unit_str = s.unit.ok_or_else(|| PresetError::BadFile("shift needs a unit".into()))?;
    let unit = parse_unit(&unit_str).ok_or_else(|| PresetError::BadFile(format!("unknown unit '{unit_str}' in shift")))?;
    Ok(Step::Shift { sign, amount, unit })
}

/// Parse a shift step's `sign` field (`+`/`-`). Required for a literal or `duration`-parametric shift -
/// a `variant`-parametric shift omits it (the variant carries its own direction), so this runs only
/// where a sign is actually needed.
pub(crate) fn parse_shift_sign(sign: &Option<String>) -> Result<Sign, PresetError> {
    match sign.as_deref() {
        Some("+") => Ok(Sign::Plus),
        Some("-") => Ok(Sign::Minus),
        Some(other) => Err(PresetError::BadFile(format!("shift sign must be + or -, got '{other}'"))),
        None => Err(PresetError::BadFile("shift needs a sign (+ or -)".into())),
    }
}

/// Parse a preset from JSON WITHOUT resolving its moment (pure - no I/O, no parameter values). The
/// moment stays raw because a parametric base/shift needs values `resolve_parameters` supplies later -
/// a non-parametric preset resolves trivially (empty values). Unknown major schema is refused.
pub(crate) fn parse_preset(text: &str) -> Result<Preset, PresetError> {
    let dto: PresetDto =
        serde_json::from_str(text).map_err(|e| PresetError::BadFile(format!("bad preset JSON: {e}")))?;
    if dto.schema != "chronomock.preset/1" {
        return Err(PresetError::BadFile(format!(
            "unsupported preset schema '{}' (this build reads chronomock.preset/1)",
            dto.schema
        )));
    }
    let parameters = dto.parameters.into_iter().map(parse_parameter).collect::<Result<Vec<_>, _>>()?;
    let time_mode = time_mode_from(dto.time_mode)?;
    Ok(Preset {
        id: dto.id,
        name_en: dto.name.en,
        explains_en: dto.explains.en,
        applies_to: dto.applies_to,
        parameters,
        moment: dto.moment,
        time_mode,
    })
}

/// Parse one file parameter declaration into a typed `Parameter`, checking the type and any default.
pub(crate) fn parse_parameter(dto: ParameterDto) -> Result<Parameter, PresetError> {
    let kind = match dto.kind.as_str() {
        "date" => ParamKind::Date,
        "duration" => ParamKind::Duration,
        "variant" => ParamKind::Variant,
        other => {
            return Err(PresetError::NotBuilt(format!(
                "parameter '{}' has type '{other}', which calc does not resolve yet (built: date, duration, variant)",
                dto.id
            )))
        }
    };
    let default = match dto.default {
        Some(v) => Some(param_value_from_json(&dto.id, kind, &v)?),
        None => None,
    };
    Ok(Parameter { id: dto.id, kind, default, default_hint: dto.default_hint })
}

/// Parse a parameter's file `default` (a JSON value) against its declared type.
pub(crate) fn param_value_from_json(id: &str, kind: ParamKind, v: &serde_json::Value) -> Result<ParamValue, PresetError> {
    match kind {
        ParamKind::Date => {
            let s = v
                .as_str()
                .ok_or_else(|| PresetError::BadFile(format!("parameter '{id}' default must be a date string")))?;
            Ok(ParamValue::Date(parse_param_date(s).map_err(PresetError::BadFile)?))
        }
        ParamKind::Duration => {
            let d: DurationDto = serde_json::from_value(v.clone())
                .map_err(|_| PresetError::BadFile(format!("parameter '{id}' default must be {{ amount, unit }}")))?;
            duration_value(id, d.amount, &d.unit)
        }
        ParamKind::Variant => {
            let s = v
                .as_str()
                .ok_or_else(|| PresetError::BadFile(format!("parameter '{id}' default must be a variant label")))?;
            Ok(ParamValue::Variant(variant_days(id, s)?))
        }
    }
}

/// Resolve every declared parameter to a value: `--param` first, then the file `default`, then a
/// `default_hint` (in `run`, where a target exists), else an error. `target_date` carries the
/// target's file creation date on the run path (`None` in calc, where a hint stays an honest request
/// to pass `--param`). A `--param` naming no declared parameter is rejected - a silently ignored typo
/// is a wrong result, not a warning.
pub(crate) fn resolve_parameters(
    params: &[Parameter],
    cli: &HashMap<String, String>,
    target_date: Option<chrono_core::calc::CivilDateTime>,
) -> Result<HashMap<String, ParamValue>, PresetError> {
    for id in cli.keys() {
        if !params.iter().any(|p| &p.id == id) {
            return Err(PresetError::BadFile(format!("unknown parameter '{id}' for this preset")));
        }
    }
    let mut out = HashMap::new();
    for p in params {
        let value = if let Some(raw) = cli.get(&p.id) {
            parse_param_value(&p.id, p.kind, raw)?
        } else if let Some(def) = &p.default {
            def.clone()
        } else if let Some(hint) = &p.default_hint {
            resolve_hint(&p.id, p.kind, hint, target_date)?
        } else {
            return Err(PresetError::BadFile(format!("parameter '{}' has no value - pass --param {}=<value>", p.id, p.id)));
        };
        out.insert(p.id.clone(), value);
    }
    Ok(out)
}

/// What a preset was actually filled with: every declared parameter, the value it resolved to, and
/// where that value came from, as `(id, value, source)`.
///
/// Lives here, beside [`resolve_parameters`], and asks the source question in the same order that
/// function takes its answers - the two cannot be read apart, and a plan naming a source for a
/// resolution that did not happen would be worse than a plan naming none.
///
/// A parameter missing from `values` is skipped rather than guessed at. That cannot happen after a
/// successful resolution, which fills every declared parameter or fails.
pub(crate) fn parameter_provenance(
    params: &[Parameter],
    cli: &HashMap<String, String>,
    values: &HashMap<String, ParamValue>,
) -> Vec<(String, String, String)> {
    params
        .iter()
        .filter_map(|p| {
            let value = values.get(&p.id)?;
            Some((p.id.clone(), param_value_text(value), parameter_source(p, cli)))
        })
        .collect()
}

/// Where a resolved parameter's value came from. The final arm cannot be reached after a successful
/// resolution - a parameter with no source at all is the error `resolve_parameters` raises first.
fn parameter_source(p: &Parameter, cli: &HashMap<String, String>) -> String {
    if cli.contains_key(&p.id) {
        "--param".to_string()
    } else if p.default.is_some() {
        "the preset's default".to_string()
    } else if let Some(hint) = &p.default_hint {
        match hint.as_str() {
            "target_file_creation" => "the target's file date".to_string(),
            // A hint built later names itself rather than being described as something it is not.
            other => format!("default_hint {other}"),
        }
    } else {
        "no source".to_string()
    }
}

/// A resolved parameter value as one short phrase, for a report that shows what a preset was filled
/// with. Not a wire format and not parsed back - the machine surface carries the resolved moment.
fn param_value_text(v: &ParamValue) -> String {
    match v {
        ParamValue::Date(d) => format!("{:04}-{:02}-{:02}", d.year, d.month, d.day),
        ParamValue::Duration { amount, unit } => format!("{amount} {}", unit.name()),
        // The three boundary variants, by the signed day offset each resolves to (docs/05 3.6).
        ParamValue::Variant(-1) => "day_before".to_string(),
        ParamValue::Variant(0) => "on_day".to_string(),
        ParamValue::Variant(1) => "day_after".to_string(),
        ParamValue::Variant(days) => format!("{days:+} days"),
    }
}

/// Resolve a parameter's `default_hint` to a value. Only `target_file_creation` is built (docs/04
/// 4.2): it fills a `date` parameter from the target's file date, available only in `run`. Without a
/// target it is an honest "not built" asking for `--param`, not a guess.
pub(crate) fn resolve_hint(
    id: &str,
    kind: ParamKind,
    hint: &str,
    target_date: Option<chrono_core::calc::CivilDateTime>,
) -> Result<ParamValue, PresetError> {
    match hint {
        "target_file_creation" => {
            if kind != ParamKind::Date {
                return Err(PresetError::BadFile(format!(
                    "parameter '{id}': hint target_file_creation fills a date, but the parameter is not a date"
                )));
            }
            match target_date {
                Some(date) => Ok(ParamValue::Date(date)),
                None => Err(PresetError::NotBuilt(format!(
                    "parameter '{id}' takes its value from the target file date (only available when running a target) - pass --param {id}=<value>"
                ))),
            }
        }
        other => Err(PresetError::NotBuilt(format!(
            "parameter '{id}' uses default_hint '{other}', which is not built yet (built: target_file_creation)"
        ))),
    }
}

/// Resolve a preset's raw moment to a concrete `MomentExpr`, substituting parameter values into a
/// parametric base/shift. This is where the parametric preset becomes an ordinary moment the core
/// evaluates - the core never sees a parameter.
pub(crate) fn resolve_moment(moment: MomentDto, values: &HashMap<String, ParamValue>) -> Result<MomentExpr, PresetError> {
    let base = base_from(moment.base, values)?;
    let steps = moment.steps.into_iter().map(|s| step_from(s, values)).collect::<Result<Vec<_>, _>>()?;
    Ok(MomentExpr { base, steps })
}

/// Parse a `--param` value string against the parameter's type. `date` accepts a bare date - a
/// `duration` is a magnitude and a unit with no sign (the shift carries the sign).
pub(crate) fn parse_param_value(id: &str, kind: ParamKind, raw: &str) -> Result<ParamValue, PresetError> {
    match kind {
        ParamKind::Date => Ok(ParamValue::Date(parse_param_date(raw).map_err(PresetError::BadFile)?)),
        ParamKind::Duration => {
            let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
            let (num, unit_str) = raw.split_at(split);
            let amount: i64 = num
                .parse()
                .map_err(|_| PresetError::BadFile(format!("parameter '{id}': bad duration '{raw}' (want e.g. 30days)")))?;
            duration_value(id, amount, unit_str)
        }
        ParamKind::Variant => Ok(ParamValue::Variant(variant_days(id, raw)?)),
    }
}

/// Map a boundary-variant label to its signed day offset (docs/05 3.6): the day before, on, or after
/// the boundary. The three labels are the contract - an unknown one is a usage error, never a guess.
pub(crate) fn variant_days(id: &str, label: &str) -> Result<i64, PresetError> {
    match label {
        "day_before" => Ok(-1),
        "on_day" => Ok(0),
        "day_after" => Ok(1),
        other => Err(PresetError::BadFile(format!(
            "parameter '{id}': unknown variant '{other}' (day_before, on_day, day_after)"
        ))),
    }
}

/// Build a `duration` value, mapping the unit token and rejecting a negative magnitude.
pub(crate) fn duration_value(id: &str, amount: i64, unit_str: &str) -> Result<ParamValue, PresetError> {
    if amount < 0 {
        return Err(PresetError::BadFile(format!("parameter '{id}' amount must be non-negative")));
    }
    let unit = parse_unit(unit_str)
        .ok_or_else(|| PresetError::BadFile(format!("parameter '{id}': unknown unit '{unit_str}'")))?;
    Ok(ParamValue::Duration { amount, unit })
}

/// Parse a date or date-time - a bare date gets midnight so `--param start_date=2026-01-01` works.
pub(crate) fn parse_param_date(s: &str) -> Result<chrono_core::calc::CivilDateTime, String> {
    let normalized =
        if s.contains('T') || s.contains(' ') { s.to_string() } else { format!("{s}T00:00:00") };
    chrono_core::calc::parse_civil_datetime(&normalized)
}

/// Map a preset's `time_mode` to the substitution wire shape. `multiplier == 1` (or absent) is
/// real-time `flow`, `> 1` is `xN`, and `< 1` is rejected. Presets do not express `frozen`.
pub(crate) fn time_mode_from(dto: Option<TimeModeDto>) -> Result<PresetTimeMode, PresetError> {
    let Some(dto) = dto else { return Ok(PresetTimeMode::default()) };
    let multiplier = dto.multiplier.unwrap_or(1);
    let (mode, multiplier) = match multiplier {
        1 => ("flow".to_string(), None),
        m if m > 1 => ("multiplier".to_string(), Some(m)),
        _ => return Err(PresetError::BadFile(format!("time_mode multiplier must be >= 1, got {multiplier}"))),
    };
    Ok(PresetTimeMode { mode, multiplier, scale_duration: dto.scale_duration_clock })
}

/// Whether a preset's `applies_to` makes it a substitution question (docs/04 4.2).
pub(crate) fn preset_targets_substitution(applies_to: &str) -> bool {
    matches!(applies_to, "substitution" | "both")
}

/// Read the target executable's creation date, expressed in the session zone, for a
/// `target_file_creation` hint (docs/04 4.2). `None` if the file's metadata cannot be read (the
/// launch will then fail plainly on its own), so a hint falls back to the honest "pass --param".
pub(crate) fn read_target_creation_date(
    target: &str,
    tz_bias_min: Option<i32>,
) -> Option<chrono_core::calc::CivilDateTime> {
    use std::os::windows::fs::MetadataExt;
    let meta = std::fs::metadata(target).ok()?;
    // creation_time() is a Windows FILETIME (100ns since 1601-01-01 UTC) - the same shape the
    // wall-clock conversion speaks - so express it in the session zone as a civil date.
    //
    // Zero means the file system does not record a creation time (some network and non-NTFS
    // volumes), and it is not a date: taken literally it hands the preset 1601-01-01 and the trial
    // computes from there without a word (R2-N13). None instead, which the caller already knows how
    // to report - "this preset needs a start date" beats a confident wrong one (rule 6).
    let created = meta.creation_time();
    if created == 0 {
        return None;
    }
    let wall = filetime_utc_to_wall(created as i64, tz_bias_min.unwrap_or(0));
    chrono_core::calc::parse_civil_datetime(&wall).ok()
}

/// Locate a preset file: next to the executable (portable layout), else in ./presets.
pub(crate) fn find_preset_file(id: &str) -> Result<std::path::PathBuf, PresetError> {
    if !is_valid_catalogue_id(id) {
        return Err(PresetError::NotFound(format!(
            "invalid preset id '{id}' (use letters, digits, '-' or '_')"
        )));
    }
    find_catalogue_file("presets", id)
        .ok_or_else(|| PresetError::NotFound(format!("preset '{id}' not found ({})", catalogue_search_places("presets"))))
}

/// Load and validate a preset by id.
pub(crate) fn load_preset(id: &str) -> Result<Preset, PresetError> {
    let path = find_preset_file(id)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| PresetError::BadFile(format!("cannot read {}: {e}", path.display())))?;
    parse_preset(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_core::calc::SnapTarget;
    use crate::calendar::calendar_from_text;
    use crate::testutil::read_data;

    /// The canonical trial preset (docs/04 4.2): a `date` parameter fills the base, a `duration`
    /// parameter fills a shift. With both values the moment substitutes to a concrete expression.
    const TRIAL_JSON: &str = r#"{
        "schema": "chronomock.preset/1", "id": "trial-first-day-after",
        "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
        "parameters": [
            { "id": "trial_length", "type": "duration", "default": { "amount": 30, "unit": "days" } },
            { "id": "start_date", "type": "date", "default_hint": "target_file_creation" }
        ],
        "moment": {
            "base": { "parameter": "start_date" },
            "steps": [
                { "shift": { "sign": "+", "parameter": "trial_length" } },
                { "shift": { "sign": "+", "amount": 1, "unit": "days" } },
                { "set_time": "00:00:01" }
            ]
        }
    }"#;

    #[test]
    fn shipped_presets_parse_and_hit_golden_dates() {
        use chrono_core::calc::{CivilDateTime, EvalContext};
        let now = CivilDateTime { year: 2026, month: 2, day: 15, hour: 12, minute: 0, second: 0 };

        // Every shipped preset parses (a malformed one, or a bad schema/field, fails here).
        let ids = [
            "month-end",
            "quarter-end",
            "epoch-zero",
            "year-2038",
            "trial-first-day-after",
            "trial-last-day",
            "year-rollover",
            "license-expired-year-ago",
            "date-before-install",
            "payment-due-business-days",
            "clock-skew-plus-90s",
            "feb-29",
            "age-of-majority",
            "fiscal-year-end",
        ];
        for id in ids {
            let text = read_data(&format!("presets/{id}.json"));
            parse_preset(&text).unwrap_or_else(|e| panic!("preset {id} parses: {}", e.message()));
        }

        // Evaluate a preset with its default parameters, through the real resolve + engine path.
        let eval_preset = |id: &str, ctx: &EvalContext| {
            let p = parse_preset(&read_data(&format!("presets/{id}.json"))).unwrap();
            let values = resolve_parameters(&p.parameters, &HashMap::new(), None).unwrap();
            let expr = resolve_moment(p.moment, &values).unwrap();
            chrono_core::calc::eval(&expr, ctx).unwrap().result()
        };

        let no_cal = EvalContext { now, zone_bias_min: 0, calendar: None };
        // Absolute-base presets are exact.
        assert_eq!(eval_preset("epoch-zero", &no_cal).to_iso(), "1970-01-01T00:00:00");
        assert_eq!(eval_preset("year-2038", &no_cal).to_iso(), "2038-01-19T03:14:07");
        // month-end snaps today (2026-02-15) to the last day of February - a common year, so the 28th.
        let month_end = eval_preset("month-end", &no_cal);
        assert_eq!((month_end.year, month_end.month, month_end.day), (2026, 2, 28));

        // clock-skew-plus-90s carries base "now" (not "today"): the +90 s shift is measured from the
        // current instant WITH its seconds, so 12:00:00 + 90 s crosses the minute to 12:01:30. A base of
        // "today" (midnight) would make the 2FA skew meaningless - this pins that the preset uses "now".
        assert_eq!(eval_preset("clock-skew-plus-90s", &no_cal).to_iso(), "2026-02-15T12:01:30");

        // feb-29 uses nearest next-leap-day, pure arithmetic with NO calendar: from 2026-02-15 the next
        // 29 February is 2028 (2026 and 2027 are common years). Proves the leap-day nearest target
        // resolves without a --calendar, unlike the business-day targets.
        assert_eq!(eval_preset("feb-29", &no_cal).to_iso(), "2028-02-29T00:00:00");

        // age-of-majority is parametric and calculator-only: a birth date + the default day_before
        // variant is the day before the 18th birthday. 2008-03-15 + 18y = 2026-03-15, day_before ->
        // 2026-03-14. Exercises the whole variant path (parse + resolve + sign-less shift).
        let aom = parse_preset(&read_data("presets/age-of-majority.json")).unwrap();
        let aom_vals = resolve_parameters(&aom.parameters, &param_map(&[("birth_date", "2008-03-15")]), None).unwrap();
        let aom_expr = resolve_moment(aom.moment, &aom_vals).unwrap();
        assert_eq!(chrono_core::calc::eval(&aom_expr, &no_cal).unwrap().result().to_iso(), "2026-03-14T00:00:00");

        // fiscal-year-end takes a fiscal-year start date and returns its last day (start + 1 year - 1 day),
        // never a hardcoded 31 December (docs/05 3.4). US federal 2025-10-01 -> 2026-09-30.
        let fye = parse_preset(&read_data("presets/fiscal-year-end.json")).unwrap();
        let fye_vals = resolve_parameters(&fye.parameters, &param_map(&[("fiscal_year_start", "2025-10-01")]), None).unwrap();
        let fye_expr = resolve_moment(fye.moment, &fye_vals).unwrap();
        assert_eq!(chrono_core::calc::eval(&fye_expr, &no_cal).unwrap().result().to_iso(), "2026-09-30T00:00:00");

        // payment-due-business-days is calendar-aware: +90 business days from today lands on a different
        // day per market, which is the whole point of a calendar-aware preset. Anchor at 2026-06-01 so the
        // window spans US Labor Day (first Monday of September, a US weekday holiday Poland does not have),
        // guaranteeing the two markets diverge - if the calendar were ignored they would be identical.
        let now_pay = CivilDateTime { year: 2026, month: 6, day: 1, hour: 12, minute: 0, second: 0 };
        let bank = calendar_from_text(&read_data("calendars/us-banking.json")).unwrap();
        let pl = calendar_from_text(&read_data("calendars/pl.json")).unwrap();
        let due_us = eval_preset("payment-due-business-days", &EvalContext { now: now_pay, zone_bias_min: 0, calendar: Some(&bank) });
        let due_pl = eval_preset("payment-due-business-days", &EvalContext { now: now_pay, zone_bias_min: 0, calendar: Some(&pl) });
        assert_ne!(due_us.to_iso(), due_pl.to_iso(), "US and PL must diverge over 90 business days");
    }

    /// A preset whose entire purpose is ONE INSTANT - the Unix epoch, the signed 32-bit `time_t`
    /// limit - has to reach that instant in the zone the tester actually runs in. The zone list is
    /// the point of this guard: every golden-date assertion above runs at `zone_bias_min: 0`, and in
    /// UTC these presets pass while missing their target everywhere else (R2-P1).
    ///
    /// It asserts the EPOCH SECOND, not just the marker, because `year_2038_boundary` means "at or
    /// past the limit": in a zone west of UTC a wrong moment lands PAST the limit and lights the
    /// marker anyway, so a marker-only assertion would pass for the wrong reason. The marker is
    /// checked too - it is what a tester reads - but the number is what pins the instant.
    #[test]
    fn instant_targeted_presets_hit_their_instant_in_every_zone() {
        use chrono_core::calc::{CivilDateTime, EvalContext, Significance, formats, significance};
        let now = CivilDateTime { year: 2026, month: 2, day: 15, hour: 12, minute: 0, second: 0 };

        // id, the epoch second the preset's own description promises, and the marker that says so.
        let targets = [
            ("epoch-zero", 0_i64, Significance::UnixEpoch),
            ("year-2038", 2_147_483_647_i64, Significance::Year2038Boundary),
        ];
        // UTC, then east and west of it, including a zone whose offset is not a whole hour. A bias is
        // UTC minus local, so -120 is UTC+02:00 and +300 is UTC-05:00.
        for zone_bias_min in [0, -120, 300, -345, 720] {
            for (id, want_epoch, marker) in targets {
                let ctx = EvalContext { now, zone_bias_min, calendar: None };
                let p = parse_preset(&read_data(&format!("presets/{id}.json"))).unwrap();
                let values = resolve_parameters(&p.parameters, &HashMap::new(), None).unwrap();
                let expr = resolve_moment(p.moment, &values).unwrap();
                let result = chrono_core::calc::eval(&expr, &ctx).unwrap().result();

                let got = formats(&result, zone_bias_min).epoch_seconds;
                assert_eq!(
                    got,
                    Some(want_epoch),
                    "preset '{id}' must land on epoch {want_epoch} at bias {zone_bias_min}, not {got:?}"
                );
                let sig = significance(&result, zone_bias_min, None);
                assert!(
                    sig.contains(&marker),
                    "preset '{id}' must carry {:?} at bias {zone_bias_min}, got {sig:?}",
                    marker
                );
            }
        }
    }

    /// Resolve a non-parametric preset's moment (empty parameter values) - the slice 16/17 path,
    /// now that parse and resolve are separate.
    fn resolve_no_params(p: Preset) -> Result<MomentExpr, PresetError> {
        let values = resolve_parameters(&p.parameters, &HashMap::new(), None)?;
        resolve_moment(p.moment, &values)
    }

    /// Build a --param map for tests.
    fn param_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A `both` preset over `today` with a snap step maps to the canonical moment and carries its
    /// English framing. `snap` speaks the same token as the `--snap` flag (one grammar).
    #[test]
    fn preset_maps_to_canonical_moment_with_framing() {
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "month-end",
            "name": { "en": "Last day of month", "pl": "x" },
            "explains": { "en": "Month-end close?", "pl": "x" },
            "applies_to": "both",
            "moment": { "base": "today", "steps": [ { "snap": "end-of-month" } ] }
        }"#;
        let p = parse_preset(json).unwrap();
        assert_eq!(p.id, "month-end");
        assert_eq!(p.name_en, "Last day of month");
        assert_eq!(p.explains_en, "Month-end close?");
        assert_eq!(p.applies_to, "both");
        let m = resolve_no_params(p).unwrap();
        assert_eq!(m.base, Base::Today);
        assert_eq!(m.steps, vec![Step::Snap(SnapTarget::EndOfMonth)]);
    }

    /// An `{ "absolute": ... }` base resolves to a fixed civil moment - a malformed one is refused at
    /// resolve time (the base is not parsed until then), never normalized silently.
    #[test]
    fn preset_absolute_base_parses_and_rejects_bad_date() {
        let ok = r#"{
            "schema": "chronomock.preset/1", "id": "epoch-zero",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
            "moment": { "base": { "absolute": "1970-01-01T00:00:00" }, "steps": [] }
        }"#;
        let m = resolve_no_params(parse_preset(ok).unwrap()).unwrap();
        assert_eq!(
            m.base,
            Base::Absolute(chrono_core::calc::CivilDateTime {
                year: 1970, month: 1, day: 1, hour: 0, minute: 0, second: 0
            })
        );
        assert!(m.steps.is_empty());

        let bad = ok.replace("1970-01-01T00:00:00", "2025-02-31T00:00:00");
        assert!(matches!(resolve_no_params(parse_preset(&bad).unwrap()), Err(PresetError::BadFile(_))));
    }

    /// An unknown major schema version is refused (docs/04 3.1), as a usage error.
    #[test]
    fn preset_unknown_schema_is_refused() {
        let json = r#"{
            "schema": "chronomock.preset/2", "id": "x",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
            "moment": { "base": "today", "steps": [] }
        }"#;
        let e = parse_preset(json).unwrap_err();
        assert!(matches!(e, PresetError::BadFile(_)));
        assert_eq!(e.exit_code(), 1);
    }

    /// A parameter type not built yet (int) is the honest "not built" (exit 5) at parse time, never
    /// guessed. (variant is now built - slice for age-of-majority.)
    #[test]
    fn preset_unbuilt_param_type_is_not_built() {
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "age",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "calculator",
            "parameters": [ { "id": "count", "type": "int" } ],
            "moment": { "base": "today", "steps": [] }
        }"#;
        let e = parse_preset(json).unwrap_err();
        assert!(matches!(e, PresetError::NotBuilt(_)));
        assert_eq!(e.exit_code(), 5);
    }

    /// A variant parameter resolves its label to a signed day offset that fills a sign-less shift step
    /// (docs/05 3.6): day_before -1, on_day 0, day_after +1. The shift carries the variant's direction.
    #[test]
    fn variant_parameter_fills_a_signless_shift() {
        use chrono_core::calc::{eval, CivilDateTime, EvalContext};
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "b",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "calculator",
            "parameters": [ { "id": "boundary", "type": "variant", "default": "day_before" } ],
            "moment": { "base": { "absolute": "2026-03-15T00:00:00" },
                        "steps": [ { "shift": { "parameter": "boundary" } } ] }
        }"#;
        let ctx = EvalContext {
            now: CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 },
            zone_bias_min: 0,
            calendar: None,
        };
        // Re-parse per case because resolve_moment consumes the raw moment (MomentDto is not Clone).
        let resolve = |cli: &[(&str, &str)]| {
            let p = parse_preset(json).unwrap();
            let vals = resolve_parameters(&p.parameters, &param_map(cli), None).unwrap();
            let expr = resolve_moment(p.moment, &vals).unwrap();
            eval(&expr, &ctx).unwrap().result().to_iso()
        };
        assert_eq!(resolve(&[]), "2026-03-14T00:00:00"); // default day_before -> the day before
        assert_eq!(resolve(&[("boundary", "day_after")]), "2026-03-16T00:00:00");
        assert_eq!(resolve(&[("boundary", "on_day")]), "2026-03-15T00:00:00"); // on_day -> unchanged
    }

    /// docs/04 4.1: a preset describes TIME, never a TARGET. There is no path field in the model, so
    /// a smuggled `"path"` is simply ignored - it cannot reach the moment. Structural enforcement.
    #[test]
    fn preset_ignores_a_path_field() {
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "sneaky", "path": "C:/evil.exe",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
            "moment": { "base": "today", "steps": [] }
        }"#;
        // It loads (unknown fields ignored, docs/04 section 3) and the result has no way to carry a
        // path - `Preset` has no such field. The moment is exactly the declared one.
        let m = resolve_no_params(parse_preset(json).unwrap()).unwrap();
        assert_eq!(m.base, Base::Today);
        assert!(m.steps.is_empty());
    }

    /// The calculator honours `applies_to`: substitution-only presets are not calculator questions.
    #[test]
    fn preset_applies_to_gates_the_calculator() {
        assert!(preset_targets_calculator("calculator"));
        assert!(preset_targets_calculator("both"));
        assert!(!preset_targets_calculator("substitution"));
    }

    /// Bad JSON is a usage-level bad-file error, not a panic.
    #[test]
    fn preset_bad_json_is_reported() {
        assert!(matches!(parse_preset("{ not json"), Err(PresetError::BadFile(_))));
    }

    /// A preset's time_mode maps to the substitution wire shape: multiplier 1 (or absent) is flow,
    /// >1 is xN, <1 is refused. scale_duration_clock rides through.
    #[test]
    fn preset_time_mode_maps_to_wire_shape() {
        let none = time_mode_from(None).unwrap();
        assert_eq!(none.mode, "flow");
        assert_eq!(none.multiplier, None);
        assert!(!none.scale_duration);

        let flow = time_mode_from(Some(TimeModeDto { multiplier: Some(1), scale_duration_clock: false })).unwrap();
        assert_eq!((flow.mode.as_str(), flow.multiplier), ("flow", None));

        let xn = time_mode_from(Some(TimeModeDto { multiplier: Some(60), scale_duration_clock: true })).unwrap();
        assert_eq!((xn.mode.as_str(), xn.multiplier, xn.scale_duration), ("multiplier", Some(60), true));

        assert!(matches!(
            time_mode_from(Some(TimeModeDto { multiplier: Some(0), scale_duration_clock: false })),
            Err(PresetError::BadFile(_))
        ));
    }

    /// A preset carrying a time_mode object surfaces it on the loaded preset.
    #[test]
    fn preset_from_json_reads_time_mode() {
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "fast",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
            "moment": { "base": "today", "steps": [] },
            "time_mode": { "multiplier": 1440, "scale_duration_clock": true }
        }"#;
        let p = parse_preset(json).unwrap();
        assert_eq!(p.time_mode.mode, "multiplier");
        assert_eq!(p.time_mode.multiplier, Some(1440));
        assert!(p.time_mode.scale_duration);
    }

    /// The substitution surface honours applies_to: calculator-only presets are not run questions.
    #[test]
    fn preset_applies_to_gates_substitution() {
        assert!(preset_targets_substitution("substitution"));
        assert!(preset_targets_substitution("both"));
        assert!(!preset_targets_substitution("calculator"));
    }

    fn civil(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono_core::calc::CivilDateTime {
        chrono_core::calc::CivilDateTime { year: y, month: mo, day: d, hour: h, minute: mi, second: s }
    }

    #[test]
    fn param_date_base_and_duration_shift_substitute() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let values =
            resolve_parameters(&p.parameters, &param_map(&[("start_date", "2026-01-01"), ("trial_length", "30days")]), None)
                .unwrap();
        let m = resolve_moment(p.moment, &values).unwrap();
        assert_eq!(m.base, Base::Absolute(civil(2026, 1, 1, 0, 0, 0)));
        assert_eq!(
            m.steps,
            vec![
                Step::Shift { sign: Sign::Plus, amount: 30, unit: Unit::Days },
                Step::Shift { sign: Sign::Plus, amount: 1, unit: Unit::Days },
                Step::SetTime { hour: 0, minute: 0, second: 1 },
            ]
        );
    }

    /// A parameter with a file `default` (trial_length) may be omitted - the default is used.
    #[test]
    fn param_default_used_when_flag_absent() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let values = resolve_parameters(&p.parameters, &param_map(&[("start_date", "2026-01-01")]), None).unwrap();
        let m = resolve_moment(p.moment, &values).unwrap();
        assert_eq!(m.steps[0], Step::Shift { sign: Sign::Plus, amount: 30, unit: Unit::Days });
    }

    /// A required parameter with only a default_hint (start_date) is the honest "not built" in the
    /// calculator (exit 5) - the hint's target date is not available here. Never guessed.
    #[test]
    fn param_hint_only_needs_a_value_in_calc() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let e = resolve_parameters(&p.parameters, &param_map(&[]), None).unwrap_err();
        assert!(matches!(e, PresetError::NotBuilt(_)));
        assert_eq!(e.exit_code(), 5);
    }

    /// A --param naming no declared parameter is rejected (a silently ignored typo is a wrong result).
    #[test]
    fn param_unknown_id_is_rejected() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let e = resolve_parameters(&p.parameters, &param_map(&[("start_date", "2026-01-01"), ("nope", "1")]), None).unwrap_err();
        assert!(matches!(e, PresetError::BadFile(_)));
    }

    /// A --param value that does not parse against its type is a usage error, not a panic.
    #[test]
    fn param_bad_value_is_rejected() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        assert!(matches!(
            resolve_parameters(&p.parameters, &param_map(&[("start_date", "not-a-date")]), None),
            Err(PresetError::BadFile(_))
        ));
        let p2 = parse_preset(TRIAL_JSON).unwrap();
        assert!(matches!(
            resolve_parameters(&p2.parameters, &param_map(&[("start_date", "2026-01-01"), ("trial_length", "30frobs")]), None),
            Err(PresetError::BadFile(_))
        ));
    }

    /// A duration value in a date slot (or vice versa) is refused at substitution, not misread.
    #[test]
    fn param_wrong_type_for_slot_is_refused() {
        // Feed trial_length (a duration) where the base expects a date by pointing base at it.
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "mismatch",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
            "parameters": [ { "id": "d", "type": "duration", "default": { "amount": 5, "unit": "days" } } ],
            "moment": { "base": { "parameter": "d" }, "steps": [] }
        }"#;
        let p = parse_preset(json).unwrap();
        let values = resolve_parameters(&p.parameters, &param_map(&[]), None).unwrap();
        assert!(matches!(resolve_moment(p.moment, &values), Err(PresetError::BadFile(_))));
    }

    /// The target_file_creation hint fills a date parameter from the target's file date (run only) -
    /// without a target it is the honest not-built - a duration slot or an unbuilt hint is refused.
    #[test]
    fn hint_target_file_creation_resolves_only_with_a_target() {
        let d = civil(2025, 6, 15, 9, 30, 0);
        assert!(matches!(
            resolve_hint("start_date", ParamKind::Date, "target_file_creation", Some(d)),
            Ok(ParamValue::Date(x)) if x == d
        ));
        let e = resolve_hint("start_date", ParamKind::Date, "target_file_creation", None).unwrap_err();
        assert!(matches!(e, PresetError::NotBuilt(_)));
        assert_eq!(e.exit_code(), 5);
        assert!(matches!(
            resolve_hint("d", ParamKind::Duration, "target_file_creation", Some(d)),
            Err(PresetError::BadFile(_))
        ));
        assert!(matches!(
            resolve_hint("x", ParamKind::Date, "somewhere_else", Some(d)),
            Err(PresetError::NotBuilt(_))
        ));
    }

    /// In run, the trial's start_date resolves from the target date and trial_length from its default,
    /// with no --param at all - the flagship "trial in substitution" flow.
    #[test]
    fn param_hint_resolves_from_target_date_in_run() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let target = civil(2025, 1, 10, 0, 0, 0);
        let values = resolve_parameters(&p.parameters, &param_map(&[]), Some(target)).unwrap();
        assert!(matches!(values.get("start_date"), Some(ParamValue::Date(x)) if *x == target));
        assert!(matches!(
            values.get("trial_length"),
            Some(ParamValue::Duration { amount: 30, unit: Unit::Days })
        ));
    }

    /// --param wins over the hint even when a target date is available.
    #[test]
    fn param_flag_overrides_hint_in_run() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let target = civil(2025, 1, 10, 0, 0, 0);
        let values =
            resolve_parameters(&p.parameters, &param_map(&[("start_date", "2030-12-31")]), Some(target)).unwrap();
        assert!(matches!(values.get("start_date"), Some(ParamValue::Date(x)) if *x == civil(2030, 12, 31, 0, 0, 0)));
    }
}
