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
//! evolution, docs/04 section 3) - an unknown major schema version is refused (section 3.1). A
//! calendar refuses unknown fields since R4-D11, and a preset deliberately does not: it carries fields
//! only the window reads (the Polish texts), and none of them can move the moment.


use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;

use chronomock::calc::{Base, MomentExpr, Sign, Step, Unit};
use chronomock::calendar::Calendar;
use chronomock::filetime_utc_to_wall;
use chrono_proto::TimeModeKind;

use crate::calendar::{catalogue_search_places, check_declared_id, find_catalogue_file, is_valid_catalogue_id};
use crate::grammar::{parse_nearest, parse_set_time, parse_snap, parse_unit, unknown_unit};
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
    /// The name and the "what it tests" line in every language the file wrote them in, English
    /// included - for the catalogue, which hands them to the window.
    pub(crate) name_texts: BTreeMap<String, String>,
    pub(crate) explains_texts: BTreeMap<String, String>,
    /// `calculator` / `substitution` / `both` (docs/04 4.2). Each surface honours it.
    pub(crate) applies_to: String,
    pub(crate) parameters: Vec<Parameter>,
    pub(crate) moment: MomentDto,
    pub(crate) time_mode: PresetTimeMode,
    /// `us` / `pl` / none (docs/04 4.2): the market whose calendar counts the preset's business days
    /// in `chrono run`, as it does in the panel's scenario list (`calendar_for_market`).
    pub(crate) market: Option<String>,
}

/// A preset parameter (docs/04 4.2): a typed slot filled by `--param`, a file `default`, or (in a
/// substitution session, a later slice) a `default_hint` such as the target's file date.
#[derive(Debug)]
pub(crate) struct Parameter {
    pub(crate) id: String,
    pub(crate) kind: ParamKind,
    pub(crate) default: Option<ParamValue>,
    /// Where to propose a value from when neither `--param` nor `default` is given (docs/04 4.2).
    /// `target_file_creation` needs a target, so it is honoured only in `run` (a later slice) - the
    /// calculator, having no target, reports it as a value the user must supply.
    pub(crate) default_hint: Option<String>,
    /// What a reader calls the parameter, in every language the file wrote it in (R4/19) - for the
    /// window, through the catalogue. Empty when the file names none, and the window then shows the id,
    /// which is also what `--param` takes on the command line.
    pub(crate) label_texts: BTreeMap<String, String>,
}

/// A parameter's type. `date` fills a base - `duration` and `variant` fill a shift (a `variant` by a
/// signed day offset - docs/05 3.6). (`int` is later.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParamKind {
    Date,
    Duration,
    Variant,
}

impl ParamKind {
    /// The `type` a file writes, and the one the catalogue hands on.
    pub(crate) fn name(self) -> &'static str {
        match self {
            ParamKind::Date => "date",
            ParamKind::Duration => "duration",
            ParamKind::Variant => "variant",
        }
    }

    fn from_name(name: &str) -> Option<ParamKind> {
        [ParamKind::Date, ParamKind::Duration, ParamKind::Variant].into_iter().find(|kind| kind.name() == name)
    }
}

/// A resolved parameter value, ready to substitute into the moment.
#[derive(Debug, Clone)]
pub(crate) enum ParamValue {
    Date(chronomock::calc::CivilDateTime),
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
    /// The time flow on the wire: flow or multiplier. Presets do not express frozen.
    pub(crate) mode: TimeModeKind,
    pub(crate) multiplier: Option<i64>,
    pub(crate) scale_duration: bool,
}

impl Default for PresetTimeMode {
    /// A preset with no `time_mode` (e.g. a calculator-only one) runs at real speed.
    fn default() -> Self {
        Self { mode: TimeModeKind::Flow, multiplier: None, scale_duration: false }
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

    /// The same failure, said to be in the step at `index` (from 0, named from 1). A preset's step that
    /// would not resolve was refused with no word of which one, in a file whose steps are a list (R5-D5,
    /// item 2). The catalogue says it the same way.
    pub(crate) fn at_step(self, index: usize) -> Self {
        let place = |m: String| format!("step {}: {m}", index + 1);
        match self {
            PresetError::NotFound(m) => PresetError::NotFound(place(m)),
            PresetError::BadFile(m) => PresetError::BadFile(place(m)),
            PresetError::NotBuilt(m) => PresetError::NotBuilt(place(m)),
        }
    }
}

// Each DTO names what it expects in the file's own words (`expecting`), never by its Rust name - see
// `calendar::json_refusal`. A test there holds every derive in this file to it.
#[derive(Deserialize)]
#[serde(expecting = "a preset - an object with schema, id, name, explains, applies_to and moment")]
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
    #[serde(default)]
    market: Option<String>,
}

/// A preset's `time_mode` object (docs/04 4.2): `{ "multiplier": N, "scale_duration_clock": bool }`.
#[derive(Deserialize)]
#[serde(expecting = "a time mode - an object like {\"multiplier\": 60}")]
pub(crate) struct TimeModeDto {
    #[serde(default)]
    multiplier: Option<i64>,
    #[serde(default)]
    scale_duration_clock: bool,
}

/// A preset parameter as written in the file (docs/04 4.2): `{ "id", "type", "label"?, "default"?,
/// "default_hint"? }`. `default` shape depends on `type` (a string for `date`, `{ amount, unit }` for
/// `duration`), so it stays a raw value here and is parsed against the type in `parse_parameter`.
#[derive(Deserialize)]
#[serde(expecting = "a parameter - an object with id and type")]
pub(crate) struct ParameterDto {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    /// One text per language like `name`, `en` required when the label is there at all. Optional,
    /// because the id names the parameter well enough on the command line, and a preset written
    /// before labels existed must keep loading.
    #[serde(default)]
    label: Option<PresetTextDto>,
    #[serde(default)]
    default: Option<serde_json::Value>,
    #[serde(default)]
    default_hint: Option<String>,
}

/// A `duration` value/`default` object: `{ "amount": N, "unit": "days" }` (docs/04 4.2).
#[derive(Deserialize)]
#[serde(expecting = "an amount and a unit - an object like {\"amount\": 2, \"unit\": \"months\"}")]
pub(crate) struct DurationDto {
    amount: i64,
    unit: String,
}

/// A preset's name or "what it tests" line, one text per language. `en` is required and is what the
/// CLI renders (rule 15 - the CLI is English only). The other languages ride along for the window,
/// which gets them from the catalogue (`chrono presets`) since it stopped reading the files itself. A
/// language whose value is not text is left out rather than refusing the file - the window's own
/// reader left it out too, and a file listed there has to stay usable here.
#[derive(Deserialize)]
#[serde(expecting = "a text in each language - an object like {\"en\": \"...\", \"pl\": \"...\"} with en required")]
pub(crate) struct PresetTextDto {
    en: String,
    #[serde(flatten)]
    other: BTreeMap<String, serde_json::Value>,
}

impl PresetTextDto {
    /// Every language the text was written in, English included.
    fn into_texts(self) -> BTreeMap<String, String> {
        let mut texts: BTreeMap<String, String> = self
            .other
            .into_iter()
            .filter_map(|(lang, value)| match value {
                serde_json::Value::String(text) => Some((lang, text)),
                _ => None,
            })
            .collect();
        texts.insert("en".to_string(), self.en);
        texts
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(expecting = "a moment - an object with base and steps")]
pub(crate) struct MomentDto {
    pub(crate) base: BaseDto,
    #[serde(default, deserialize_with = "steps_of_one_key")]
    pub(crate) steps: Vec<StepDto>,
}

/// A moment's steps, each read through [`OneKey`].
fn steps_of_one_key<'de, D: serde::Deserializer<'de>>(steps: D) -> Result<Vec<StepDto>, D::Error> {
    Ok(Vec::<OneStep>::deserialize(steps)?.into_iter().map(|step| step.0).collect())
}

/// One step, read so that a shape which is not a step is refused in the file's words.
///
/// serde_json reads an externally tagged enum from a text or an object, and answers every other kind
/// with "expected value" - its sentence for text that is not JSON - so `"steps": [5]` sent the author
/// looking for a missing comma in a file that had none. A step with a second key was refused with the
/// same sentence. Found by the search in step 2d.
struct OneStep(StepDto);

impl<'de> Deserialize<'de> for OneStep {
    fn deserialize<D: serde::Deserializer<'de>>(step: D) -> Result<Self, D::Error> {
        StepDto::deserialize(OneKey(step)).map(OneStep)
    }
}

/// The reader of one step: an object is read with one key and no second, a text is a step name on its
/// own (which the step's own reader refuses for its missing value), and any other kind is refused as
/// that kind, in the words of the step's `expecting`.
struct OneKey<D>(D);

impl<'de, D: serde::Deserializer<'de>> serde::Deserializer<'de> for OneKey<D> {
    type Error = D::Error;

    fn deserialize_any<V: serde::de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
        self.0.deserialize_any(visitor)
    }

    fn deserialize_enum<V: serde::de::Visitor<'de>>(
        self,
        _name: &'static str,
        names: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.0.deserialize_any(OneKeyVisitor { step: visitor, names })
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option unit
        unit_struct newtype_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

struct OneKeyVisitor<V> {
    step: V,
    names: &'static [&'static str],
}

impl<'de, V: serde::de::Visitor<'de>> serde::de::Visitor<'de> for OneKeyVisitor<V> {
    type Value = V::Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.step.expecting(f)
    }

    fn visit_str<E: serde::de::Error>(self, name: &str) -> Result<V::Value, E> {
        self.step.visit_enum(serde::de::value::StrDeserializer::<E>::new(name))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<V::Value, A::Error> {
        self.step.visit_enum(OneKeyMap { map, names: self.names })
    }
}

/// An object read as one step: its first key names the step, and there is no second.
struct OneKeyMap<A> {
    map: A,
    names: &'static [&'static str],
}

impl<A> OneKeyMap<A> {
    fn refusal<E: serde::de::Error>(&self, this_one: std::fmt::Arguments) -> E {
        E::custom(format_args!("a step names exactly one of {} - this one {this_one}", self.names.join(", ")))
    }
}

impl<'de, A: serde::de::MapAccess<'de>> serde::de::EnumAccess<'de> for OneKeyMap<A> {
    type Error = A::Error;
    type Variant = Self;

    fn variant_seed<S: serde::de::DeserializeSeed<'de>>(mut self, seed: S) -> Result<(S::Value, Self), A::Error> {
        match self.map.next_key_seed(seed)? {
            Some(name) => Ok((name, self)),
            None => Err(self.refusal(format_args!("names none"))),
        }
    }
}

impl<'de, A: serde::de::MapAccess<'de>> serde::de::VariantAccess<'de> for OneKeyMap<A> {
    type Error = A::Error;

    fn unit_variant(self) -> Result<(), A::Error> {
        Err(serde::de::Error::invalid_type(serde::de::Unexpected::Map, &"a step name on its own"))
    }

    fn newtype_variant_seed<T: serde::de::DeserializeSeed<'de>>(mut self, seed: T) -> Result<T::Value, A::Error> {
        let value = self.map.next_value_seed(seed)?;
        match self.map.next_key::<String>()? {
            None => Ok(value),
            Some(second) => Err(self.refusal(format_args!("names a second, `{second}`"))),
        }
    }

    fn tuple_variant<W: serde::de::Visitor<'de>>(self, _len: usize, _visitor: W) -> Result<W::Value, A::Error> {
        Err(serde::de::Error::invalid_type(serde::de::Unexpected::Map, &"a step with a list"))
    }

    fn struct_variant<W: serde::de::Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        _visitor: W,
    ) -> Result<W::Value, A::Error> {
        Err(serde::de::Error::invalid_type(serde::de::Unexpected::Map, &"a step with fields"))
    }
}

/// A preset base: the keyword `today`/`now`, an `{ "absolute": "ISO" }` object, an
/// `{ "absolute_utc": "ISO" }` object, or a `{ "parameter": "name" }` object (docs/04 4.2)
/// resolved from a `date` parameter.
///
/// `absolute` and `absolute_utc` differ in the one way that matters: `absolute` is wall-clock in
/// the SESSION zone (rule 2), `absolute_utc` is an instant. A preset whose meaning is "9 AM local"
/// wants the first, a preset whose meaning is "epoch second 0" wants the second - and using the
/// first for the second is how these presets came to miss their target by exactly the zone offset.
///
/// Untagged, so serde's whole sentence for a base that fits no variant is the `expecting` text.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged, expecting = "base must be \"today\", \"now\" or an object with absolute, absolute_utc or parameter")]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", expecting = "a step - an object with one of shift, set_time, snap, nearest, zone")]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(expecting = "a shift - an object with sign, amount and unit, or with parameter")]
pub(crate) struct ShiftDto {
    #[serde(default)]
    sign: Option<String>,
    #[serde(default)]
    amount: Option<i64>,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    pub(crate) parameter: Option<String>,
}

/// The two places a preset is used (docs/04 4.2), each honouring the preset's `applies_to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// `chrono calc` and the window's calculator.
    Calculator,
    /// `chrono run` and the window's session.
    Substitution,
}

impl Surface {
    /// Whether a preset that `applies_to` this is a question for the surface (docs/05 3.1): a
    /// substitution-only preset such as year-rollover is no calculator question, and a calculator-only
    /// one is no moment to run.
    pub(crate) fn takes(self, applies_to: &str) -> bool {
        match self {
            Surface::Calculator => matches!(applies_to, "calculator" | "both"),
            Surface::Substitution => matches!(applies_to, "substitution" | "both"),
        }
    }
}

/// Why a preset could not be taken onto a surface: the file, or a preset that is for the other surface.
#[derive(Debug)]
pub(crate) enum PresetRefusal {
    Unreadable(PresetError),
    NotForSurface { id: String, applies_to: String },
}

/// Where a preset failed, which decides what a bad file means: the file itself (or its moment), or the
/// values given for its parameters on the command line or in the window's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresetStage {
    File,
    Parameters,
}

/// Load a preset for one surface (R5-A2): read and validated, and refused when its `applies_to` names
/// the other surface. The calculator and `run` each did these two steps on their own.
pub(crate) fn load_for(id: &str, surface: Surface) -> Result<Preset, PresetRefusal> {
    load_preset(id).map_err(PresetRefusal::Unreadable)?.for_surface(surface)
}

impl Preset {
    /// The preset, when its `applies_to` makes it a question for `surface`.
    fn for_surface(self, surface: Surface) -> Result<Preset, PresetRefusal> {
        if surface.takes(&self.applies_to) {
            Ok(self)
        } else {
            Err(PresetRefusal::NotForSurface { id: self.id, applies_to: self.applies_to })
        }
    }

    /// Fill the preset (R5-A2): every parameter resolved (`given`, then the file's default, then the
    /// target's file date where `target_date` carries one), then substituted into the moment. A
    /// non-parametric preset resolves to an empty map and its moment unchanged. A failure says at which
    /// stage, because a bad value is fixed where it was given and a bad moment in the file.
    pub(crate) fn fill(
        &self,
        given: &HashMap<String, String>,
        target_date: Option<chronomock::calc::CivilDateTime>,
    ) -> Result<(HashMap<String, ParamValue>, MomentExpr), (PresetError, PresetStage)> {
        let values = resolve_parameters(&self.parameters, given, target_date).map_err(|e| (e, PresetStage::Parameters))?;
        let moment = resolve_moment(self.moment.clone(), &values).map_err(|e| (e, PresetStage::File))?;
        Ok((values, moment))
    }
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
        // A parametric base takes its date from a `date` parameter (docs/04 4.2). Matched with the
        // other two fields absent, like every arm here: this arm used to match `parameter: Some(id), ..`
        // first, so a base naming a parameter AND an absolute date took the parameter and dropped the
        // date without a word (R4-N33).
        BaseDto::Object { absolute: None, absolute_utc: None, parameter: Some(id) } => match values.get(&id) {
            Some(ParamValue::Date(civil)) => Ok(Base::Absolute(*civil)),
            Some(ParamValue::Duration { .. }) => {
                Err(PresetError::BadFile(format!("base parameter '{id}' must be a date, not a duration")))
            }
            Some(ParamValue::Variant(_)) => {
                Err(PresetError::BadFile(format!("base parameter '{id}' must be a date, not a variant")))
            }
            None => Err(PresetError::BadFile(format!("base parameter '{id}' has no value"))),
        },
        BaseDto::Object { absolute: Some(s), absolute_utc: None, parameter: None } => {
            let civil = chronomock::calc::parse_civil_datetime(&s).map_err(PresetError::BadFile)?;
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
            let civil = chronomock::calc::parse_civil_datetime(trimmed).map_err(PresetError::BadFile)?;
            Ok(Base::AbsoluteUtc(civil))
        }
        BaseDto::Object { absolute: None, absolute_utc: None, parameter: None } => Err(PresetError::BadFile(
            "preset base object needs 'absolute', 'absolute_utc' or 'parameter'".into(),
        )),
        // More than one: `parse_preset` refuses the file before a base like this is resolved, and
        // this arm keeps the match honest for any caller that did not come through it.
        base @ BaseDto::Object { .. } => Err(base_contradiction(&base_fields(&base))),
    }
}

/// The base fields an object base names, in the order docs/04 lists them.
fn base_fields(dto: &BaseDto) -> Vec<&'static str> {
    match dto {
        BaseDto::Keyword(_) => Vec::new(),
        BaseDto::Object { absolute, absolute_utc, parameter } => [
            ("absolute", absolute.is_some()),
            ("absolute_utc", absolute_utc.is_some()),
            ("parameter", parameter.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect(),
    }
}

/// A base naming more than one field is a contradiction, not a preference order (R4-N33). Session-zone
/// wall-clock, a UTC instant and a date parameter are three different moments, and picking one of
/// them would put the preset somewhere its author did not write.
fn base_contradiction(fields: &[&str]) -> PresetError {
    let named = fields.iter().map(|f| format!("'{f}'")).collect::<Vec<_>>().join(" and ");
    PresetError::BadFile(format!(
        "preset base names {named} at once - a base is exactly one of 'absolute', 'absolute_utc' or 'parameter'"
    ))
}

/// Refuse a preset that says two things at once, before any value fills it (R4-N33).
///
/// Each of these used to be settled by the order of the reader's code, and so settled differently by
/// the two readers: a base with a date parameter and an absolute date took the parameter here and the
/// absolute date in the window, a parametric shift dropped the `amount` and `unit` beside it, a variant
/// shift dropped its `sign` (a variant carries its own direction), and a parameter id declared twice
/// was filled by whichever came last. A base or shift naming a parameter the file does not declare
/// was refused only when the preset was used, as having no value, and passing that value was then
/// refused as an unknown parameter. None of the shipped presets does any of it. The window skips a
/// file like this from its list, so the two surfaces refuse the same catalogue entries.
fn refuse_contradictions(parameters: &[Parameter], moment: &MomentDto) -> Result<(), PresetError> {
    for (i, p) in parameters.iter().enumerate() {
        if parameters[..i].iter().any(|earlier| earlier.id == p.id) {
            return Err(PresetError::BadFile(format!(
                "parameter '{}' is declared twice - a value can fill only one of them",
                p.id
            )));
        }
    }
    let fields = base_fields(&moment.base);
    if fields.len() > 1 {
        return Err(base_contradiction(&fields));
    }
    if let BaseDto::Object { parameter: Some(id), .. } = &moment.base {
        refuse_undeclared(parameters, "the base", id)?;
    }
    for step in &moment.steps {
        let StepDto::Shift(shift) = step else { continue };
        let Some(id) = &shift.parameter else { continue };
        refuse_undeclared(parameters, "a shift", id)?;
        if shift.amount.is_some() || shift.unit.is_some() {
            return Err(PresetError::BadFile(format!(
                "a shift with parameter '{id}' also names an amount or a unit - the parameter gives the size, so write one or the other"
            )));
        }
        let variant = parameters.iter().any(|p| &p.id == id && p.kind == ParamKind::Variant);
        if variant && shift.sign.is_some() {
            return Err(PresetError::BadFile(format!(
                "a shift with variant parameter '{id}' also names a sign - a variant carries its own direction, so the sign would be ignored"
            )));
        }
    }
    Ok(())
}

/// A moment naming a parameter the file does not declare. Ids compare with case, as `--param` does.
fn refuse_undeclared(parameters: &[Parameter], place: &str, id: &str) -> Result<(), PresetError> {
    if parameters.iter().any(|p| p.id == id) {
        return Ok(());
    }
    let declared = if parameters.is_empty() {
        "it declares none".to_string()
    } else {
        let ids = parameters.iter().map(|p| format!("'{}'", p.id)).collect::<Vec<_>>().join(", ");
        format!("it declares {ids}")
    };
    Err(PresetError::BadFile(format!(
        "{place} names parameter '{id}', which the preset does not declare in 'parameters' - {declared}"
    )))
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
    let unit = parse_unit(&unit_str).ok_or_else(|| PresetError::BadFile(format!("{} in shift", unknown_unit(&unit_str))))?;
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

/// The most steps a preset's moment may have. The shipped presets have at most three, so this is a
/// bound on the implausible, not on the large - the way `MAX_HOLIDAYS` bounds a calendar.
const MAX_PRESET_STEPS: usize = 64;

/// The most parameters a preset may declare. The shipped presets declare at most two.
const MAX_PARAMETERS: usize = 16;

/// Refuse a preset too long to be one (R5-B2). A preset is a file people share, and its only bound was
/// the 4 MiB of the file: 250 000 `snap` steps printed 15 MB, and 70 000 business-day steps would have
/// run for about 26 minutes, measured by extrapolation. The core bounds the business days one
/// calculation counts, this bounds the rest - and says so by name, where the author can fix it.
fn refuse_oversized(steps: usize, parameters: usize) -> Result<(), PresetError> {
    if steps > MAX_PRESET_STEPS {
        return Err(PresetError::BadFile(format!(
            "preset has {steps} steps, past the limit of {MAX_PRESET_STEPS} this build reads - remove steps until it has {MAX_PRESET_STEPS} or fewer"
        )));
    }
    if parameters > MAX_PARAMETERS {
        return Err(PresetError::BadFile(format!(
            "preset declares {parameters} parameters, past the limit of {MAX_PARAMETERS} this build reads - remove parameters until it has {MAX_PARAMETERS} or fewer"
        )));
    }
    Ok(())
}

/// Parse a preset from JSON WITHOUT resolving its moment (pure - no I/O, no parameter values). The
/// moment stays raw because a parametric base/shift needs values `resolve_parameters` supplies later -
/// a non-parametric preset resolves trivially (empty values). Unknown major schema is refused.
pub(crate) fn parse_preset(text: &str) -> Result<Preset, PresetError> {
    crate::calendar::refuse_hidden_characters("preset", text).map_err(PresetError::BadFile)?;
    let dto: PresetDto =
        crate::calendar::from_object(text).map_err(|e| PresetError::BadFile(crate::calendar::json_refusal("preset", &e)))?;
    if dto.schema != "chronomock.preset/1" {
        return Err(PresetError::BadFile(format!(
            "unsupported preset schema '{}' (this build reads chronomock.preset/1)",
            dto.schema
        )));
    }
    // Each surface used to judge an unknown value alone, by not being it: the calculator refused it as
    // "not for the calculator" and `run` as "not for substitution", so a typo read as a deliberate
    // choice in both messages. It is a bad file, and the catalogue leaves it out like one.
    if !matches!(dto.applies_to.as_str(), "calculator" | "substitution" | "both") {
        return Err(PresetError::BadFile(format!(
            "applies_to must be calculator, substitution or both, got '{}'",
            dto.applies_to
        )));
    }
    refuse_oversized(dto.moment.steps.len(), dto.parameters.len())?;
    let parameters = dto.parameters.into_iter().map(parse_parameter).collect::<Result<Vec<_>, _>>()?;
    refuse_contradictions(&parameters, &dto.moment)?;
    let time_mode = time_mode_from(dto.time_mode)?;
    let name_en = dto.name.en.clone();
    let explains_en = dto.explains.en.clone();
    let name_texts = dto.name.into_texts();
    let explains_texts = dto.explains.into_texts();
    Ok(Preset {
        id: dto.id,
        name_en,
        explains_en,
        name_texts,
        explains_texts,
        applies_to: dto.applies_to,
        parameters,
        moment: dto.moment,
        time_mode,
        market: dto.market,
    })
}

/// The calendar a market counts business days with - the same pairs the panel's scenario list uses
/// (`PresetInfo.CalendarIdForMarket` in the GUI, held equal by `RustConstantMirrorTests`). A market
/// this build has no calendar for, and no market at all, give none.
pub(crate) fn calendar_for_market(market: Option<&str>) -> Option<&'static str> {
    match market? {
        "us" => Some("us-banking"),
        "pl" => Some("pl"),
        _ => None,
    }
}

/// The calendar of a preset's market, loaded, or `None` when its market names none. `chrono run` has
/// no `--calendar`, and the panel counts a scenario's business days in its market's calendar, so the
/// run does the same rather than refuse a preset the panel runs (R4-S12).
pub(crate) fn load_market_calendar(preset_id: &str, market: Option<&str>) -> Option<Result<Calendar, String>> {
    calendar_for_market(market).map(|id| {
        crate::calendar::load_calendar(id).map_err(|e| format!("preset '{preset_id}' counts in the {id} calendar: {e}"))
    })
}

/// Parse one file parameter declaration into a typed `Parameter`, checking the type and any default.
pub(crate) fn parse_parameter(dto: ParameterDto) -> Result<Parameter, PresetError> {
    let Some(kind) = ParamKind::from_name(&dto.kind) else {
        return Err(PresetError::NotBuilt(format!(
            "parameter '{}' has type '{}', which calc does not resolve yet (built: date, duration, variant)",
            dto.id, dto.kind
        )));
    };
    let default = match dto.default {
        Some(v) => Some(param_value_from_json(&dto.id, kind, &v)?),
        None => None,
    };
    // A hint that fills a date, on a parameter that is no date, is a fault in the file, so it is refused with
    // the file (review round of #131). Found only when the hint was used, it went out as a fault in the values
    // given - the calculator said `calc.preset_parameter` to a user who gave none. A hint this build does not
    // know stays a question for the moment it is used, when a value given or a default can still stand in.
    if dto.default_hint.as_deref() == Some("target_file_creation") && kind != ParamKind::Date {
        return Err(PresetError::BadFile(format!(
            "parameter '{}': hint target_file_creation fills a date, but the parameter is not a date",
            dto.id
        )));
    }
    let label_texts = dto.label.map(PresetTextDto::into_texts).unwrap_or_default();
    Ok(Parameter { id: dto.id, kind, default, default_hint: dto.default_hint, label_texts })
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
    target_date: Option<chronomock::calc::CivilDateTime>,
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
        // A boundary variant by its label (docs/05 3.6), and an offset no variant has by its days.
        ParamValue::Variant(days) => variant_label(*days).map_or_else(|| format!("{days:+} days"), str::to_string),
    }
}

/// Resolve a parameter's `default_hint` to a value. Only `target_file_creation` is built (docs/04
/// 4.2): it fills a `date` parameter from the target's file date, available only in `run`. Without a
/// target it is an honest "not built" asking for `--param`, not a guess.
pub(crate) fn resolve_hint(
    id: &str,
    kind: ParamKind,
    hint: &str,
    target_date: Option<chronomock::calc::CivilDateTime>,
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
    let steps = moment
        .steps
        .into_iter()
        .enumerate()
        .map(|(i, s)| step_from(s, values).map_err(|e| e.at_step(i)))
        .collect::<Result<Vec<_>, _>>()?;
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

/// The boundary variants (docs/05 3.6) and the signed day offset each resolves to: the day before, on,
/// or after the boundary. One table, read by the parser, by the report and by the catalogue that hands
/// the window its choices - the window used to keep a copy of it, and a copy is a second answer.
pub(crate) const VARIANTS: [(&str, i64); 3] = [("day_before", -1), ("on_day", 0), ("day_after", 1)];

/// Map a boundary-variant label to its signed day offset. The labels are the contract - an unknown one
/// is a usage error, never a guess.
pub(crate) fn variant_days(id: &str, label: &str) -> Result<i64, PresetError> {
    VARIANTS.iter().find(|(name, _)| *name == label).map(|(_, days)| *days).ok_or_else(|| {
        let known = VARIANTS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ");
        PresetError::BadFile(format!("parameter '{id}': unknown variant '{label}' ({known})"))
    })
}

/// The label of a variant's day offset, or `None` for an offset no variant has.
pub(crate) fn variant_label(days: i64) -> Option<&'static str> {
    VARIANTS.iter().find(|(_, d)| *d == days).map(|(name, _)| *name)
}

/// Build a `duration` value, mapping the unit token and rejecting a negative magnitude.
pub(crate) fn duration_value(id: &str, amount: i64, unit_str: &str) -> Result<ParamValue, PresetError> {
    if amount < 0 {
        return Err(PresetError::BadFile(format!("parameter '{id}' amount must be non-negative")));
    }
    let unit = parse_unit(unit_str)
        .ok_or_else(|| PresetError::BadFile(format!("parameter '{id}': {}", unknown_unit(unit_str))))?;
    Ok(ParamValue::Duration { amount, unit })
}

/// Parse a date or date-time - a bare date gets midnight so `--param start_date=2026-01-01` works.
///
/// A refusal quotes what was written. It quoted the date with the midnight added, `'2026-13-01T00:00:00'`
/// for a `default` or a `--param` that said `2026-13-01` - text its author never wrote (found by the
/// search in step 2d, the class of R5-D5 item 5).
pub(crate) fn parse_param_date(s: &str) -> Result<chronomock::calc::CivilDateTime, String> {
    if s.contains('T') || s.contains(' ') {
        chronomock::calc::parse_civil_datetime(s)
    } else {
        chronomock::calc::parse_civil_datetime_as(&format!("{s}T00:00:00"), s)
    }
}

/// Map a preset's `time_mode` to the substitution wire shape. `multiplier == 1` (or absent) is
/// real-time `flow`, `> 1` is `xN`, and `< 1` is rejected. Presets do not express `frozen`. Past
/// `MULTIPLIER_MAX` is rejected too, the bound `--mode xN` has: a plan used to approve it with exit 0
/// and the core then refused the start (R4-S12).
pub(crate) fn time_mode_from(dto: Option<TimeModeDto>) -> Result<PresetTimeMode, PresetError> {
    let Some(dto) = dto else { return Ok(PresetTimeMode::default()) };
    let multiplier = dto.multiplier.unwrap_or(1);
    let (mode, multiplier) = match multiplier {
        1 => (TimeModeKind::Flow, None),
        m if m > chrono_core::MULTIPLIER_MAX => {
            return Err(PresetError::BadFile(format!(
                "time_mode multiplier must be <= {}, got {m}",
                chrono_core::MULTIPLIER_MAX
            )));
        }
        m if m > 1 => (TimeModeKind::Multiplier, Some(m)),
        _ => return Err(PresetError::BadFile(format!("time_mode multiplier must be >= 1, got {multiplier}"))),
    };
    Ok(PresetTimeMode { mode, multiplier, scale_duration: dto.scale_duration_clock })
}

/// Read the target executable's creation date, expressed in the session zone, for a
/// `target_file_creation` hint (docs/04 4.2). `None` if the file's metadata cannot be read (the
/// launch will then fail plainly on its own), so a hint falls back to the honest "pass --param".
pub(crate) fn read_target_creation_date(
    target: &str,
    tz_bias_min: Option<i32>,
) -> Option<chronomock::calc::CivilDateTime> {
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
    // The DATE, at midnight. A `date` parameter is a bare date (docs/04 4.2), and the calculator's
    // `--param install_date=...` is one. The time of day the file was written leaked through here into
    // every preset without a `set_time` step: `date-before-install` put the session at 23:00:35 the
    // day before, where the same preset with the same date given by hand gives midnight.
    chronomock::calc::parse_civil_datetime(&wall)
        .ok()
        .map(|d| chronomock::calc::CivilDateTime { hour: 0, minute: 0, second: 0, ..d })
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
    load_preset_at(&find_preset_file(id)?, id)
}

/// Load and validate the preset file at `path`, found by the id `id`. The one way a preset file is
/// read: `--preset` comes here after finding the file, and the catalogue comes here for every file it
/// lists, so a file the catalogue offers is a file `--preset` accepts (R4/18).
pub(crate) fn load_preset_at(path: &std::path::Path, id: &str) -> Result<Preset, PresetError> {
    // Same ceiling as a calendar, for the same reason: a catalogue file is outside input, and this was
    // an unbounded read.
    let text = crate::calendar::read_catalogue_file(path).map_err(PresetError::BadFile)?;
    // Named here, where the id and the file are known: "time_mode multiplier must be >= 1" said what
    // was wrong and not in which of fourteen files (R4-S12), and then not where that file was - a
    // calendar's error always said so, a preset's never did (R4-N37).
    let in_file = |m: String| format!("preset '{id}': {m} (in {})", path.display());
    let preset = parse_preset(&text).map_err(|e| match e {
        PresetError::BadFile(m) => PresetError::BadFile(in_file(m)),
        PresetError::NotBuilt(m) => PresetError::NotBuilt(in_file(m)),
        other => other,
    })?;
    check_declared_id("preset", path, &preset.id, id).map_err(PresetError::BadFile)?;
    Ok(preset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chronomock::calc::SnapTarget;
    use crate::calendar::calendar_from_text;
    use crate::testutil::read_data;

    /// A trial-SHAPED preset (docs/04 4.2): a `date` parameter fills the base, a `duration` parameter
    /// fills a shift, and a literal shift sits beside the parametric one. Deliberately NOT a copy of
    /// a shipped file - it exercises the substitution machinery, and pinning it to a real preset's
    /// steps would make every business-rule change to that file look like a parser regression. The
    /// shipped presets have their own tests, against the real files.
    const TRIAL_JSON: &str = r#"{
        "schema": "chronomock.preset/1", "id": "trial-shaped-fixture",
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
        use chronomock::calc::{CivilDateTime, EvalContext};
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
            chronomock::calc::eval(&expr, ctx).unwrap().result()
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
        assert_eq!(chronomock::calc::eval(&aom_expr, &no_cal).unwrap().result().to_iso(), "2026-03-14T00:00:00");

        // fiscal-year-end takes a fiscal-year start date and returns its last day (start + 1 year - 1 day),
        // never a hardcoded 31 December (docs/05 3.4). US federal 2025-10-01 -> 2026-09-30.
        let fye = parse_preset(&read_data("presets/fiscal-year-end.json")).unwrap();
        let fye_vals = resolve_parameters(&fye.parameters, &param_map(&[("fiscal_year_start", "2025-10-01")]), None).unwrap();
        let fye_expr = resolve_moment(fye.moment, &fye_vals).unwrap();
        assert_eq!(chronomock::calc::eval(&fye_expr, &no_cal).unwrap().result().to_iso(), "2026-09-30T00:00:00");

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
        use chronomock::calc::{CivilDateTime, EvalContext, Significance, formats, significance};
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
                let result = chronomock::calc::eval(&expr, &ctx).unwrap().result();

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

    /// The trial pair's business rule, pinned against the REAL files: the install day is day one, so a
    /// 30-day trial started on 1 January still works through 30 January and is over on the 31st
    /// (docs/05 3.3). Decided by the owner on 2026-09-08 - before that both presets counted from the
    /// day AFTER install, which put `trial-last-day` past the boundary it exists to sit inside, for
    /// both usual implementations (calendar days, and 30x24h from midnight).
    ///
    /// The two are asserted TOGETHER because their whole value is the pair straddling one boundary:
    /// moving one without the other leaves a gap or an overlap, and either would be read as an app
    /// bug by the tester who ran them back to back.
    #[test]
    fn the_trial_pair_counts_the_install_day_as_day_one() {
        use chronomock::calc::{CivilDateTime, EvalContext};
        let now = CivilDateTime { year: 2026, month: 2, day: 15, hour: 12, minute: 0, second: 0 };
        let ctx = EvalContext { now, zone_bias_min: 0, calendar: None };

        let at = |id: &str| {
            let p = parse_preset(&read_data(&format!("presets/{id}.json"))).unwrap();
            let values =
                resolve_parameters(&p.parameters, &param_map(&[("start_date", "2026-01-01")]), None).unwrap();
            let expr = resolve_moment(p.moment, &values).unwrap();
            chronomock::calc::eval(&expr, &ctx).unwrap().result().to_iso()
        };

        assert_eq!(at("trial-last-day"), "2026-01-30T23:59:59");
        assert_eq!(at("trial-first-day-after"), "2026-01-31T00:00:01");

        // A different length moves both by the same amount - the rule is "length - 1 day", not a
        // constant nudged into place for 30.
        let at_len = |id: &str, len: &str| {
            let p = parse_preset(&read_data(&format!("presets/{id}.json"))).unwrap();
            let values = resolve_parameters(
                &p.parameters,
                &param_map(&[("start_date", "2026-01-01"), ("trial_length", len)]),
                None,
            )
            .unwrap();
            let expr = resolve_moment(p.moment, &values).unwrap();
            chronomock::calc::eval(&expr, &ctx).unwrap().result().to_iso()
        };
        assert_eq!(at_len("trial-last-day", "7days"), "2026-01-07T23:59:59");
        assert_eq!(at_len("trial-first-day-after", "7days"), "2026-01-08T00:00:01");
        // A one-day trial is the sharpest case: it begins and ends on the install day itself.
        assert_eq!(at_len("trial-last-day", "1days"), "2026-01-01T23:59:59");
        assert_eq!(at_len("trial-first-day-after", "1days"), "2026-01-02T00:00:01");
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

    /// A step that will not resolve is named by its place in the list, and a unit the grammar does not
    /// read is refused with the units it does (R5-D5, items 2 and 3).
    #[test]
    fn a_step_that_will_not_resolve_is_named_by_its_number() {
        let json = r#"{"schema": "chronomock.preset/1", "id": "s", "name": { "en": "n" }, "explains": { "en": "e" },
            "applies_to": "both", "moment": { "base": "today", "steps": [
                { "snap": "end-of-month" }, { "shift": { "sign": "+", "amount": 2, "unit": "mth" } } ] } }"#;
        let err = resolve_no_params(parse_preset(json).unwrap()).unwrap_err();
        assert!(err.message().starts_with("step 2: unknown unit 'mth' (use s, m, h, d, w, mo, q, y, bd"), "{}", err.message());
    }

    /// A preset longer than any real one is refused by its count, at the limit it is not (R5-B2).
    #[test]
    fn a_preset_past_the_step_or_parameter_limit_is_refused() {
        let preset = |steps: usize, parameters: usize| {
            let steps = vec![r#"{ "snap": "end-of-month" }"#; steps].join(",");
            let params: Vec<String> = (0..parameters).map(|i| format!(r#"{{ "id": "p{i}", "type": "date" }}"#)).collect();
            format!(
                r#"{{"schema": "chronomock.preset/1", "id": "long", "name": {{ "en": "n" }}, "explains": {{ "en": "e" }},
                    "applies_to": "both", "parameters": [{}], "moment": {{ "base": "today", "steps": [{steps}] }} }}"#,
                params.join(",")
            )
        };
        assert!(parse_preset(&preset(MAX_PRESET_STEPS, MAX_PARAMETERS)).is_ok(), "at both limits");
        let err = parse_preset(&preset(MAX_PRESET_STEPS + 1, 0)).unwrap_err();
        assert!(err.message().contains("65 steps") && err.message().contains("limit of 64"), "{}", err.message());
        assert!(err.message().contains("remove steps until it has 64 or fewer"), "says what to do: {}", err.message());
        let err = parse_preset(&preset(1, MAX_PARAMETERS + 1)).unwrap_err();
        assert!(err.message().contains("17 parameters") && err.message().contains("limit of 16"), "{}", err.message());
        assert!(err.message().contains("remove parameters until it has 16 or fewer"), "says what to do: {}", err.message());
    }

    /// The measured attack (R5-B1): an `explains` with a carriage return, an erase-line sequence and a
    /// hide-text sequence printed a fake `result:` line over the real one. Refused by place, in any
    /// language of the text, and in a parameter's label as much as in the name.
    #[test]
    fn a_preset_text_with_a_hidden_character_is_refused_by_its_place() {
        let escape = |code: u32| format!("\\u{code:04x}");
        let preset = |explains_pl: &str, label: &str| {
            format!(
                r#"{{"schema": "chronomock.preset/1", "id": "p", "name": {{ "en": "n" }},
                    "explains": {{ "en": "e", "pl": "{explains_pl}" }}, "applies_to": "both",
                    "parameters": [ {{ "id": "d", "type": "date", "label": {{ "en": "{label}" }} }} ],
                    "moment": {{ "base": {{ "parameter": "d" }}, "steps": [] }} }}"#
            )
        };
        assert!(parse_preset(&preset("ok", "Start date")).is_ok(), "plain texts pass");
        let attack = format!("{}{}[2K  result:  1999-12-31T23:59:59{}[8m", escape(0x0D), escape(0x1B), escape(0x1B));
        let err = parse_preset(&preset(&attack, "Start date")).unwrap_err();
        assert!(err.message().contains("the text in `explains.pl`") && err.message().contains("U+000D"), "{}", err.message());
        assert_eq!(err.exit_code(), 1, "a bad file, not a missing feature");
        let err = parse_preset(&preset("ok", &format!("Start{}date", escape(0x2066)))).unwrap_err();
        assert!(err.message().contains("`parameters[0].label.en`") && err.message().contains("U+2066"), "{}", err.message());
    }

    /// serde quotes an unknown step as it found it, and two files reach that quote with a hidden
    /// character the walk before it never saw (review round on PR #104): one that is not JSON past the
    /// step, and one whose `steps` is given twice, where the walk holds only the second. The refusal
    /// writes the character as its code.
    #[test]
    fn a_refusal_quoting_the_file_writes_a_hidden_character_as_its_code() {
        let escape = format!("\\u{:04x}", 0x1B);
        let bad_steps = format!(r#""steps": [ {{ "x{escape}[2K": 1 }} ]"#);
        let preset = |moment: &str, tail: &str| {
            format!(
                r#"{{"schema": "chronomock.preset/1", "id": "p", "name": {{ "en": "n" }}, "explains": {{ "en": "e" }},
                    "applies_to": "both", "moment": {{ "base": {{ "absolute": "2030-01-01T00:00:00" }}, {moment} }}{tail} }}"#
            )
        };
        for (case, text) in [
            ("not JSON past the step", preset(&bad_steps, ",")),
            ("the steps given twice", preset(&format!(r#"{bad_steps}, "steps": []"#), "")),
        ] {
            let said = parse_preset(&text).unwrap_err().message().to_string();
            assert!(said.contains("unknown name"), "{case}: {said}");
            assert!(!said.chars().any(chronomock::text::is_hidden_char), "{case}: {said:?}");
            assert!(said.contains("x\\u{001b}[2K"), "{case}: {said}");
        }
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
            Base::Absolute(chronomock::calc::CivilDateTime {
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
        use chronomock::calc::{eval, CivilDateTime, EvalContext};
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

    /// R4/19: a parameter's label is what the window shows above its input, so it travels in every
    /// language the file wrote it in, English required like a name, and a parameter without one still
    /// loads (the window then shows its id). A label that never reaches the catalogue would leave the
    /// window showing `trial length` in every language, which is what it did before labels existed.
    #[test]
    fn a_parameter_label_is_read_in_every_language_and_may_be_left_out() {
        let preset = |label: &str| {
            format!(
                r#"{{"schema":"chronomock.preset/1","id":"x","name":{{"en":"n"}},"explains":{{"en":"e"}},
                "applies_to":"calculator","parameters":[{{"id":"start","type":"date"{label}}}],
                "moment":{{"base":{{"parameter":"start"}},"steps":[]}}}}"#
            )
        };

        let labelled = parse_preset(&preset(r#","label":{"en":"Start","pl":"Początek","de":3}"#)).unwrap();
        let texts = &labelled.parameters[0].label_texts;
        assert_eq!(texts.get("en").map(String::as_str), Some("Start"));
        assert_eq!(texts.get("pl").map(String::as_str), Some("Początek"));
        assert!(!texts.contains_key("de"), "a language whose value is not text is left out, as for a name");

        assert!(parse_preset(&preset("")).unwrap().parameters[0].label_texts.is_empty());
        assert!(parse_preset(&preset(r#","label":{"pl":"Początek"}"#)).is_err(), "English is required");
        assert!(parse_preset(&preset(r#","label":"Start""#)).is_err(), "one text per language, not a string");
    }

    /// What serde says about a file when it names the Rust instead of the file: a type of ours, one of
    /// its own, or a word for a shape that JSON calls something else. Each one was in a measured refusal.
    const RUST_WORDS: [&str; 10] =
        ["Dto", "struct ", "enum ", "i64", "i32", "u32", "sequence", "map", "variant, expected", "invalid length"];

    /// PR A finding (g): a wrong JSON type anywhere in a preset is refused in the file's own words, with
    /// the place first. Measured before the fix on every one of these positions: nine of them named a
    /// Rust type ("expected struct PresetTextDto" for a label written as plain text, "untagged enum
    /// BaseDto"), and the rest said "i64", "map", "sequence" or "unit variant, expected newtype variant"
    /// to somebody who wrote JSON. The accepted wordings are pinned exactly, the rest by the words left out.
    #[test]
    fn a_wrong_type_anywhere_in_a_preset_is_refused_in_the_files_own_words() {
        let preset = |name: &str, params: &str, time_mode: &str, moment: &str| {
            format!(
                r#"{{"schema":"chronomock.preset/1","id":"x","name":{name},"explains":{{"en":"e"}},
                "applies_to":"both","parameters":{params}{time_mode},"moment":{moment}}}"#
            )
        };
        let name = r#"{"en":"n"}"#;
        let moment = |steps: &str| format!(r#"{{"base":"today","steps":{steps}}}"#);
        let cases = [
            ("a name as plain text", preset(r#""Start""#, "[]", "", &moment("[]"))),
            ("a list of words for parameters", preset(name, r#""x""#, "", &moment("[]"))),
            ("a parameter as a word", preset(name, r#"["x"]"#, "", &moment("[]"))),
            ("a label as plain text", preset(name, r#"[{"id":"a","type":"date","label":"Start"}]"#, "", &moment("[]"))),
            ("a time mode as a word", preset(name, "[]", r#","time_mode":"x60""#, &moment("[]"))),
            ("a multiplier as text", preset(name, "[]", r#","time_mode":{"multiplier":"60"}"#, &moment("[]"))),
            ("a moment as a word", preset(name, "[]", "", r#""now""#)),
            ("a base as a number", preset(name, "[]", "", r#"{"base":5,"steps":[]}"#)),
            ("an absolute base as a number", preset(name, "[]", "", r#"{"base":{"absolute":5},"steps":[]}"#)),
            ("steps as an object", preset(name, "[]", "", &moment(r#"{"snap":"end-of-month"}"#))),
            ("a step as its bare name", preset(name, "[]", "", &moment(r#"["snap"]"#))),
            ("a shift as a phrase", preset(name, "[]", "", &moment(r#"[{"shift":"+1 day"}]"#))),
            ("a shift amount as text", preset(name, "[]", "", &moment(r#"[{"shift":{"sign":"+","amount":"1","unit":"day"}}]"#))),
            ("a whole preset as a list", "[]".to_string()),
            // Nine values that serde took for the nine fields, by position - this one LOADED as a preset
            // until the top level was read as an object only (found in review, measured on the binary).
            (
                "a whole preset as a list of its nine values",
                r#"["chronomock.preset/1","x",{"en":"n"},{"en":"e"},"calculator",[],{"base":"today","steps":[]},null,null]"#
                    .to_string(),
            ),
            // The likelier mistake: a file holding a list of presets, which serde read as one preset whose
            // `schema` was the first object, and refused with "expected a string".
            ("a list of presets in one file", format!("[{},{}]", preset(name, "[]", "", &moment("[]")), preset(name, "[]", "", &moment("[]")))),
            // A NESTED object written as an empty list still goes through serde's length check, which is
            // the shape the "invalid length" words in `json_refusal` are there for.
            ("a moment as an empty list", preset(name, "[]", "", "[]")),
        ];
        for (what, text) in &cases {
            let refusal = match parse_preset(text) {
                Err(PresetError::BadFile(m)) => m,
                Err(other) => panic!("{what}: refused as {}, not as a bad file", other.message()),
                Ok(_) => panic!("{what}: loaded"),
            };
            assert!(refusal.starts_with("bad preset JSON at line "), "{what}: the place comes first - {refusal}");
            for word in RUST_WORDS {
                assert!(!refusal.contains(word), "{what}: says {word:?} - {refusal}");
            }
        }

        let refusal = |text: &str| parse_preset(text).err().map(|e| e.message().to_string()).unwrap_or_default();
        let after_place = |text: &str| refusal(text).split_once(": ").map(|(_, said)| said.to_string()).unwrap_or_default();
        assert_eq!(
            after_place(&cases[3].1),
            r#"invalid type: string "Start", expected a text in each language - an object like {"en": "...", "pl": "..."} with en required"#
        );
        assert_eq!(
            after_place(&cases[7].1),
            r#"base must be "today", "now" or an object with absolute, absolute_utc or parameter"#
        );
        assert_eq!(
            after_place(&cases[10].1),
            r#"invalid type: a step name on its own, expected the name with its value, like {"snap": "end-of-month"}"#
        );
        assert_eq!(after_place(&cases[5].1), r#"invalid type: string "60", expected a whole number"#);
        assert_eq!(
            after_place("[]"),
            "invalid type: list, expected a preset - an object with schema, id, name, explains, applies_to and moment"
        );
        assert_eq!(refusal("[]"), format!("bad preset JSON at line 1, column 1: {}", after_place("[]")));
        assert_eq!(cases.len(), 17, "the indices below name the cases above");
        assert_eq!(after_place(&cases[14].1), after_place("[]"), "a list of nine values is a list, whatever it holds");
        assert_eq!(after_place(&cases[15].1), after_place("[]"), "and so is a list of presets");
        assert_eq!(after_place(&cases[16].1), "invalid type: list, expected a moment - an object with base and steps");
    }

    /// Found by the search in step 2d: serde_json reads a step from a text or an object and answered
    /// every other kind with "expected value", its sentence for text that is not JSON, in a file that
    /// was JSON. A step with a second key got the same sentence, and an unknown step name was an
    /// "unknown variant". Each is refused now in the file's words, at the step's place.
    #[test]
    fn a_step_that_is_not_one_is_refused_in_the_files_own_words() {
        let with_steps = |steps: &str| {
            format!(
                r#"{{"schema":"chronomock.preset/1","id":"x","name":{{"en":"n"}},"explains":{{"en":"e"}},"applies_to":"both","moment":{{"base":"today","steps":{steps}}}}}"#
            )
        };
        let said = |steps: &str| match parse_preset(&with_steps(steps)) {
            Err(PresetError::BadFile(m)) => m.split_once(": ").map(|(place, said)| {
                assert!(place.starts_with("bad preset JSON at line 1, column "), "{steps}: {m}");
                said.to_string()
            }),
            other => panic!("{steps}: {other:?}"),
        };
        let expecting = "expected a step - an object with one of shift, set_time, snap, nearest, zone";
        for (steps, kind) in [("[5]", "integer `5`"), ("[true]", "boolean `true`"), ("[null]", "null"), ("[[]]", "list")] {
            assert_eq!(said(steps).as_deref(), Some(format!("invalid type: {kind}, {expecting}").as_str()), "{steps}");
        }
        let names = "shift, set_time, snap, nearest, zone";
        assert_eq!(said("[{}]").as_deref(), Some(format!("a step names exactly one of {names} - this one names none").as_str()));
        assert_eq!(
            said(r#"[{"snap":"eom","zone":"+01:00"}]"#).as_deref(),
            Some(format!("a step names exactly one of {names} - this one names a second, `zone`").as_str())
        );
        assert_eq!(
            said(r#"[{"to_zone":"+01:00"}]"#).as_deref(),
            Some("unknown name `to_zone`, expected one of `shift`, `set_time`, `snap`, `nearest`, `zone`")
        );
        // A step that is one still reads, and so does the bare name's refusal of before.
        assert!(parse_preset(&with_steps(r#"[{"snap":"eom"},{"zone":"+01:00"}]"#)).is_ok());
        assert!(said(r#"["snap"]"#).is_some_and(|s| s.starts_with("invalid type: a step name on its own")));
    }

    /// R4-N33. Every one of these used to load and be settled by the order of the reader's code: the
    /// parameter beat the absolute date, the parameter's size beat the `amount` and `unit` beside it,
    /// the variant's direction beat the written sign, and the second of two parameters with one id beat
    /// the first. Now each is a bad file naming what it says twice.
    #[test]
    fn a_preset_that_says_two_things_at_once_is_refused() {
        let preset = |parameters: &str, moment: &str| {
            format!(
                r#"{{"schema":"chronomock.preset/1","id":"x","name":{{"en":"n"}},"explains":{{"en":"e"}},
                "applies_to":"calculator","parameters":[{parameters}],"moment":{moment}}}"#
            )
        };
        let date = r#"{"id":"d","type":"date","default":"2020-01-01"}"#;
        let size = r#"{"id":"n","type":"duration","default":{"amount":1,"unit":"days"}}"#;
        let side = r#"{"id":"v","type":"variant","default":"day_after"}"#;
        for (text, needle) in [
            (preset(date, r#"{"base":{"absolute":"2030-01-01T00:00:00","parameter":"d"}}"#), "'absolute' and 'parameter'"),
            (preset(date, r#"{"base":{"absolute_utc":"2030-01-01T00:00:00Z","parameter":"d"}}"#), "'absolute_utc' and 'parameter'"),
            (
                preset("", r#"{"base":{"absolute":"2030-01-01T00:00:00","absolute_utc":"2030-01-01T00:00:00Z"}}"#),
                "'absolute' and 'absolute_utc'",
            ),
            (
                preset(size, r#"{"base":"today","steps":[{"shift":{"sign":"+","amount":100,"unit":"years","parameter":"n"}}]}"#),
                "an amount or a unit",
            ),
            (preset(size, r#"{"base":"today","steps":[{"shift":{"sign":"+","unit":"years","parameter":"n"}}]}"#), "an amount or a unit"),
            (preset(side, r#"{"base":"today","steps":[{"shift":{"sign":"-","parameter":"v"}}]}"#), "its own direction"),
            (preset(&format!("{date},{date}"), r#"{"base":{"parameter":"d"}}"#), "declared twice"),
        ] {
            let err = parse_preset(&text).expect_err("a contradiction is refused");
            assert!(matches!(err, PresetError::BadFile(_)), "{}", err.message());
            assert!(err.message().contains(needle), "{needle}: {}", err.message());
        }

        // The shapes the shipped presets use say one thing each: a duration shift with its sign, a
        // variant shift without one, and a base naming one field.
        for text in [
            preset(size, r#"{"base":"today","steps":[{"shift":{"sign":"+","parameter":"n"}}]}"#),
            preset(side, r#"{"base":"today","steps":[{"shift":{"parameter":"v"}}]}"#),
            preset(date, r#"{"base":{"parameter":"d"}}"#),
        ] {
            parse_preset(&text).unwrap_or_else(|e| panic!("{}", e.message()));
        }
    }

    /// A base or shift naming a parameter the file does not declare used to load, and was refused only
    /// when used, as "has no value" - and `--param` with that id was then refused as unknown, so the
    /// two messages sent the user in a circle. Now the file is refused, naming the id and what it does
    /// declare. Ids compare with case, as `--param` compares them.
    #[test]
    fn a_moment_naming_an_undeclared_parameter_is_refused() {
        let preset = |parameters: &str, moment: &str| {
            format!(
                r#"{{"schema":"chronomock.preset/1","id":"x","name":{{"en":"n"}},"explains":{{"en":"e"}},
                "applies_to":"calculator","parameters":[{parameters}],"moment":{moment}}}"#
            )
        };
        let date = r#"{"id":"d","type":"date","default":"2020-01-01"}"#;
        let size = r#"{"id":"n","type":"duration","default":{"amount":1,"unit":"days"}}"#;
        for (text, needle) in [
            (preset(size, r#"{"base":"today","steps":[{"shift":{"sign":"+","parameter":"m"}}]}"#), "a shift names parameter 'm'"),
            (preset("", r#"{"base":"today","steps":[{"shift":{"parameter":"v"}}]}"#), "a shift names parameter 'v'"),
            (preset(date, r#"{"base":{"parameter":"e"}}"#), "the base names parameter 'e'"),
            (preset(date, r#"{"base":{"parameter":"D"}}"#), "the base names parameter 'D'"),
        ] {
            let err = parse_preset(&text).expect_err("an undeclared parameter is refused");
            assert!(matches!(err, PresetError::BadFile(_)), "{}", err.message());
            assert!(err.message().contains(needle), "{needle}: {}", err.message());
            assert!(err.message().contains("does not declare"), "{}", err.message());
        }
        let err = parse_preset(&preset(&format!("{date},{size}"), r#"{"base":{"parameter":"e"}}"#)).unwrap_err();
        assert!(err.message().contains("it declares 'd', 'n'"), "{}", err.message());
        let err = parse_preset(&preset("", r#"{"base":{"parameter":"e"}}"#)).unwrap_err();
        assert!(err.message().contains("it declares none"), "{}", err.message());
    }

    /// Every preset in `presets/` loads the way `--preset` loads it, found by its own id (R4-D14).
    /// The golden test above names fourteen files, so a fifteenth was read by nothing.
    #[test]
    fn every_shipped_preset_loads_under_its_own_name() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../presets");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("the presets folder") {
            let path = entry.expect("an entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).expect("a file name").to_string();
            let text = crate::calendar::read_catalogue_file(&path).unwrap_or_else(|e| panic!("{stem}: {e}"));
            let preset = parse_preset(&text).unwrap_or_else(|e| panic!("{stem}: {}", e.message()));
            check_declared_id("preset", &path, &preset.id, &stem).unwrap_or_else(|e| panic!("{e}"));
            seen += 1;
        }
        assert!(seen >= 14, "the fourteen shipped presets at least, found {seen}");
    }

    /// The calculator honours `applies_to`: substitution-only presets are not calculator questions.
    #[test]
    fn preset_applies_to_gates_the_calculator() {
        assert!(Surface::Calculator.takes("calculator"));
        assert!(Surface::Calculator.takes("both"));
        assert!(!Surface::Calculator.takes("substitution"));
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
        assert_eq!(none.mode, TimeModeKind::Flow);
        assert_eq!(none.multiplier, None);
        assert!(!none.scale_duration);

        let flow = time_mode_from(Some(TimeModeDto { multiplier: Some(1), scale_duration_clock: false })).unwrap();
        assert_eq!((flow.mode, flow.multiplier), (TimeModeKind::Flow, None));

        let xn = time_mode_from(Some(TimeModeDto { multiplier: Some(60), scale_duration_clock: true })).unwrap();
        assert_eq!((xn.mode, xn.multiplier, xn.scale_duration), (TimeModeKind::Multiplier, Some(60), true));

        assert!(matches!(
            time_mode_from(Some(TimeModeDto { multiplier: Some(0), scale_duration_clock: false })),
            Err(PresetError::BadFile(_))
        ));

        // The same ceiling as --mode xN, on both sides of it (R4-S12).
        let max = chrono_core::MULTIPLIER_MAX;
        let top = time_mode_from(Some(TimeModeDto { multiplier: Some(max), scale_duration_clock: false })).unwrap();
        assert_eq!(top.multiplier, Some(max));
        assert!(matches!(
            time_mode_from(Some(TimeModeDto { multiplier: Some(max + 1), scale_duration_clock: false })),
            Err(PresetError::BadFile(_))
        ));
    }

    /// Every calendar a market names ships and parses - a pair naming a missing file would turn a preset
    /// the panel runs into a refusal in `chrono run` (R4-S12). The pairs themselves are held equal to
    /// the panel's by `RustConstantMirrorTests`.
    #[test]
    fn every_market_calendar_ships() {
        for market in ["us", "pl"] {
            let id = calendar_for_market(Some(market)).expect("a market with a calendar");
            calendar_from_text(&read_data(&format!("calendars/{id}.json"))).expect("the calendar parses");
        }
        assert_eq!(calendar_for_market(None), None);
        assert_eq!(calendar_for_market(Some("de")), None, "a market with no calendar names none");
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
        assert_eq!(p.time_mode.mode, TimeModeKind::Multiplier);
        assert_eq!(p.time_mode.multiplier, Some(1440));
        assert!(p.time_mode.scale_duration);
    }

    /// The substitution surface honours applies_to: calculator-only presets are not run questions.
    #[test]
    fn preset_applies_to_gates_substitution() {
        assert!(Surface::Substitution.takes("substitution"));
        assert!(Surface::Substitution.takes("both"));
        assert!(!Surface::Substitution.takes("calculator"));
    }

    /// R5-A2: both surfaces take a preset the same way - one for the other surface is refused with its
    /// id and what it applies to, and a fill that fails says whether the values or the file were wrong.
    #[test]
    fn a_preset_is_taken_onto_a_surface_and_filled_the_same_way_for_both() {
        let preset = |applies_to: &str, steps: &str| {
            parse_preset(&format!(
                r#"{{"schema": "chronomock.preset/1", "id": "p", "name": {{ "en": "n" }}, "explains": {{ "en": "e" }},
                    "applies_to": "{applies_to}",
                    "parameters": [{{ "id": "start", "type": "date", "label": {{ "en": "Start" }}, "default": "2026-01-01" }}],
                    "moment": {{ "base": {{ "parameter": "start" }}, "steps": [{steps}] }}}}"#
            ))
            .expect("a preset the reader takes")
        };
        let refused = preset("substitution", "").for_surface(Surface::Calculator).unwrap_err();
        assert!(
            matches!(&refused, PresetRefusal::NotForSurface { id, applies_to } if id == "p" && applies_to == "substitution"),
            "{refused:?}"
        );
        let calculator = preset("calculator", "").for_surface(Surface::Calculator).expect("a calculator preset");

        let (values, _) = calculator.fill(&param_map(&[]), None).expect("the default fills it");
        assert_eq!(values.len(), 1);
        let (bad_value, stage) = calculator.fill(&param_map(&[("start", "not-a-date")]), None).unwrap_err();
        assert_eq!(stage, PresetStage::Parameters, "{}", bad_value.message());
        let (unknown, stage) = calculator.fill(&param_map(&[("nope", "1")]), None).unwrap_err();
        assert_eq!(stage, PresetStage::Parameters, "{}", unknown.message());

        // A duration where the base wants a date: the values are fine, the file is not.
        let mismatch = parse_preset(
            r#"{"schema": "chronomock.preset/1", "id": "m", "name": { "en": "n" }, "explains": { "en": "e" },
                "applies_to": "both",
                "parameters": [ { "id": "d", "type": "duration", "default": { "amount": 5, "unit": "days" } } ],
                "moment": { "base": { "parameter": "d" }, "steps": [] }}"#,
        )
        .expect("a preset the reader takes");
        let (in_file, stage) = mismatch.fill(&param_map(&[]), None).unwrap_err();
        assert_eq!(stage, PresetStage::File, "{}", in_file.message());

        // A date hint on a duration is the file's fault too, and refused with the file - before any value is
        // asked for, so it never reads as a bad value the user gave.
        let hinted = parse_preset(
            r#"{"schema": "chronomock.preset/1", "id": "h", "name": { "en": "n" }, "explains": { "en": "e" },
                "applies_to": "both",
                "parameters": [ { "id": "d", "type": "duration", "default_hint": "target_file_creation" } ],
                "moment": { "base": "today", "steps": [] }}"#,
        );
        assert!(
            matches!(&hinted, Err(PresetError::BadFile(m)) if m.contains("target_file_creation")),
            "{:?}",
            hinted.as_ref().err().map(PresetError::message)
        );
    }

    fn civil(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> chronomock::calc::CivilDateTime {
        chronomock::calc::CivilDateTime { year: y, month: mo, day: d, hour: h, minute: mi, second: s }
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

    /// Found by the search in step 2d: a date that is not one was refused quoting it with the midnight
    /// added - `'2026-13-01T00:00:00'` for a `--param` or a `default` that said `2026-13-01`. Both quote
    /// what was written now, and a date written with its time is quoted as it was too.
    #[test]
    fn a_date_that_is_not_one_is_refused_quoting_what_was_written() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        for (written, said) in [("2026-13-01", "month 13"), ("2026-02-29", "day 29"), ("2026-02-30 08:00:00", "day 30")] {
            let refusal = resolve_parameters(&p.parameters, &param_map(&[("start_date", written)]), None).unwrap_err();
            assert!(refusal.message().contains(&format!("'{written}'")) && refusal.message().contains(said), "{}", refusal.message());
            assert!(written.contains(' ') || !refusal.message().contains("T00:00:00"), "{}", refusal.message());
        }
        let with_default = TRIAL_JSON.replacen(r#""type": "date""#, r#""type": "date", "default": "2026-13-01""#, 1);
        assert_ne!(with_default, TRIAL_JSON, "the trial preset declares a date parameter");
        let refusal = parse_preset(&with_default).unwrap_err();
        assert!(refusal.message().ends_with("in '2026-13-01'"), "{}", refusal.message());
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

    /// The file date is a DATE: midnight of the day the file was created, in the session zone, the
    /// same value `--param install_date=YYYY-MM-DD` gives. It used to carry the time of day too.
    ///
    /// The file is given a creation time half an hour from midnight UTC on either side, so the day
    /// itself moves with the zone: a date that ignored the zone, or read its sign backwards, lands on
    /// the wrong day for at least one of the three.
    #[test]
    fn the_target_file_date_is_midnight_of_the_day_it_was_created() {
        use std::os::windows::fs::FileTimesExt;
        use std::time::{Duration, UNIX_EPOCH};
        let dir = crate::testutil::unique_temp_dir("chrono-preset-file-date");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let file = dir.join("app.exe");
        std::fs::write(&file, b"MZ").expect("file");
        // 2030-06-15T23:30:00Z and 2030-06-15T00:30:00Z. Bias is UTC minus local: -330 is UTC+05:30,
        // 480 is UTC-08:00.
        let late = 1_907_796_600;
        for (created, days) in [(late, [(0, 15), (-330, 16), (480, 15)]), (late - 23 * 3600, [(0, 15), (-330, 15), (480, 14)])] {
            let times = std::fs::FileTimes::new().set_created(UNIX_EPOCH + Duration::from_secs(created));
            std::fs::File::options().write(true).open(&file).expect("open").set_times(times).expect("set the creation time");
            for (bias, day) in days {
                let d = read_target_creation_date(&file.display().to_string(), Some(bias)).expect("a creation date");
                assert_eq!((d.year, d.month, d.day, d.hour, d.minute, d.second), (2030, 6, day, 0, 0, 0), "created {created}, bias {bias}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
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

    mod properties {
        use super::*;
        use crate::search::{drawn_preset, preset_flaw, with_preset_flaw, DrawnPreset, PresetFlaw};
        use proptest::prelude::*;

        /// Whether two parameter values are the same value.
        fn same(a: Option<&ParamValue>, b: Option<&ParamValue>) -> bool {
            match (a, b) {
                (None, None) => true,
                (Some(ParamValue::Date(a)), Some(ParamValue::Date(b))) => a == b,
                (Some(ParamValue::Duration { amount: a, unit: u }), Some(ParamValue::Duration { amount: b, unit: v })) => a == b && u == v,
                (Some(ParamValue::Variant(a)), Some(ParamValue::Variant(b))) => a == b,
                _ => false,
            }
        }

        /// The texts a preset was written with, as the model wrote them: English, Polish when it was
        /// translated, and not the language given as something other than text.
        fn texts(preset: &DrawnPreset, what: &str) -> BTreeMap<String, String> {
            let mut texts = BTreeMap::from([("en".to_string(), format!("{what} of {}", preset.id))]);
            if preset.translated {
                texts.insert("pl".to_string(), format!("{what} po polsku"));
            }
            texts
        }

        /// A preset with no flaw, or one with a flaw - drawn small, so the flaw is its only one.
        fn case() -> impl Strategy<Value = (DrawnPreset, Option<PresetFlaw>)> {
            prop_oneof![
                2 => drawn_preset(false).prop_map(|preset| (preset, None)),
                3 => (drawn_preset(true), preset_flaw()).prop_map(|(preset, flaw)| (preset, Some(flaw))),
            ]
        }

        fn reads_as_written(preset: &DrawnPreset) -> Result<(), TestCaseError> {
            let text = preset.json().to_string();
            let read = parse_preset(&text).map_err(|e| TestCaseError::fail(format!("refused: {} - {text}", e.message())))?;
            prop_assert_eq!(&read.id, &preset.id);
            prop_assert_eq!(read.applies_to.as_str(), preset.applies_to);
            prop_assert_eq!(read.market.as_deref(), preset.market);
            prop_assert_eq!(&read.name_texts, &texts(preset, "Name"));
            prop_assert_eq!(&read.explains_texts, &texts(preset, "Explains"));
            prop_assert_eq!(&read.name_en, &format!("Name of {}", preset.id));
            let (mode, multiplier) = match preset.time_mode {
                None | Some((None | Some(1), _)) => (TimeModeKind::Flow, None),
                Some((Some(m), _)) => (TimeModeKind::Multiplier, Some(m)),
            };
            prop_assert_eq!(read.time_mode.mode, mode);
            prop_assert_eq!(read.time_mode.multiplier, multiplier);
            prop_assert_eq!(read.time_mode.scale_duration, preset.time_mode.is_some_and(|(_, scale)| scale));
            prop_assert_eq!(read.parameters.len(), preset.parameters.len());
            for (got, drawn) in read.parameters.iter().zip(&preset.parameters) {
                prop_assert_eq!(&got.id, &drawn.id);
                prop_assert_eq!(got.kind, drawn.kind);
                let owed = drawn.default.as_ref().map(|d| d.value());
                prop_assert!(same(got.default.as_ref(), owed.as_ref()), "{}: default {:?}, owed {:?}", drawn.id, got.default, owed);
                prop_assert_eq!(got.default_hint.as_deref(), drawn.hint.then_some("target_file_creation"));
                let label = if drawn.label {
                    BTreeMap::from([("en".to_string(), format!("Label of {}", drawn.id)), ("pl".to_string(), format!("Etykieta {}", drawn.id))])
                } else {
                    BTreeMap::new()
                };
                prop_assert_eq!(&got.label_texts, &label);
            }
            let moment = resolve_moment(read.moment, &preset.values()).map_err(|e| TestCaseError::fail(e.message().to_string()))?;
            prop_assert_eq!(moment, preset.moment(), "{}", text);
            Ok(())
        }

        proptest! {
            /// A preset written within its schema reads as what it says: its texts in every language
            /// written as text, its module, market and time mode, every parameter with its type,
            /// default, hint and label, and a moment that, filled with values, is the moment the
            /// model works out - every base, step, unit word and zone as written. And the same preset
            /// with ONE thing wrong, from the refusals its readers owe by name (R4-N33, R4-S12, R5-B2,
            /// R5-D5), is refused when it is read or when it is used, with the exit code of that
            /// refusal and a message that names the thing - a step by its number.
            #[test]
            fn a_preset_reads_as_written_or_is_refused_for_its_one_flaw((preset, flaw) in case()) {
                let Some(flaw) = flaw else { return reads_as_written(&preset) };
                let (json, owed) = with_preset_flaw(&preset, &flaw);
                // Refused by the evaluator, which this does not run - the catalogue's property holds it.
                if owed.unused_until_evaluated {
                    return Ok(());
                }
                let text = json.to_string();
                let refused = match parse_preset(&text) {
                    Err(e) => Some(e),
                    Ok(read) => resolve_moment(read.moment, &owed.values).err(),
                };
                let Some(refusal) = refused else {
                    return Err(TestCaseError::fail(format!("{flaw:?} was read and resolved: {text}")));
                };
                for piece in &owed.said {
                    prop_assert!(refusal.message().contains(piece.as_str()), "{:?}: {} - owed {:?}", flaw, refusal.message(), piece);
                }
                prop_assert_eq!(refusal.exit_code(), owed.exit_code, "{:?}: {}", flaw, refusal.message());
            }
        }
    }
}
