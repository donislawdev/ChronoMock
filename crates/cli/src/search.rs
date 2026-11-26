//! What the random properties of this crate share: numbers drawn from their edges as well as from
//! anywhere, a preset as its author may write one - with one flaw by name or without - and the canary
//! of the weekly long run.
//!
//! Compiled for tests only (`#[cfg(test)] mod search;` in main.rs). The properties stand beside the
//! code they search, each in a `mod properties`, so `cargo test properties` selects every one of
//! them - the weekly run in .github/workflows/search.yml relies on that name. chrono-core has a file
//! of the same name and the same kind: a crate cannot reach another crate's test code.

use std::collections::HashMap;

use chronomock::calc::{Base, CivilDateTime, MomentExpr, NearestTarget, Sign, SnapTarget, Step, Unit};
use proptest::prelude::*;
use proptest::strategy::Union;

use crate::preset::{ParamKind, ParamValue};

/// Any `u64`, with its edges drawn as often as the middle. A uniform draw lands among the last few
/// values of a 64-bit range about once in 2^60 draws, and the length that panicked the WebSocket
/// parser sat exactly there (S-4) - so the edges are asked for by name.
pub(crate) fn edge_u64() -> impl Strategy<Value = u64> {
    prop_oneof![
        4 => any::<u64>(),
        2 => 0u64..=1_000,
        1 => (u64::MAX - 16)..=u64::MAX,
    ]
}

/// `around` and the few values either side of it - a limit is wrong on its edge, not far from it.
pub(crate) fn near(around: u64, by: u64) -> impl Strategy<Value = u64> {
    around.saturating_sub(by)..=around.saturating_add(by)
}

/// Unit words a preset may write and the unit each one is - the contract restated, with the trap of
/// `m` (minutes) beside `mo` and `month` (months) named.
const UNIT_WORDS: [(&str, Unit); 12] = [
    ("s", Unit::Seconds),
    ("m", Unit::Minutes),
    ("minutes", Unit::Minutes),
    ("h", Unit::Hours),
    ("d", Unit::Days),
    ("days", Unit::Days),
    ("w", Unit::Weeks),
    ("mo", Unit::Months),
    ("month", Unit::Months),
    ("q", Unit::Quarters),
    ("y", Unit::Years),
    ("bd", Unit::BusinessDays),
];

const SNAP_WORDS: [(&str, SnapTarget); 4] = [
    ("eom", SnapTarget::EndOfMonth),
    ("start-of-quarter", SnapTarget::StartOfQuarter),
    ("soy", SnapTarget::StartOfYear),
    ("end-of-year", SnapTarget::EndOfYear),
];

const NEAREST_WORDS: [(&str, NearestTarget); 3] = [
    ("nbd", NearestTarget::NextBusinessDay),
    ("previous-business-day", NearestTarget::PrevBusinessDay),
    ("next-feb-29", NearestTarget::NextLeapDay),
];

/// The boundary variants (docs/05 3.6) and the day each moves the boundary by.
const VARIANT_DAYS: [(&str, i64); 3] = [("day_before", -1), ("on_day", 0), ("day_after", 1)];

/// Parameter ids, two of them the same but for case - `--param` compares with case.
const PARAMETER_IDS: [&str; 6] = ["start", "Start", "length", "edge", "x_1", "d-2"];

fn sign_text(sign: Sign) -> &'static str {
    match sign {
        Sign::Plus => "+",
        Sign::Minus => "-",
    }
}

/// A value a parameter is filled with: a date (written with a time of day or without, which is
/// midnight), an amount and a unit word, or a variant label.
#[derive(Debug, Clone)]
pub(crate) enum DrawnValue {
    Date { at: CivilDateTime, with_time: bool },
    Duration { amount: i64, unit: usize },
    Variant(usize),
}

impl DrawnValue {
    fn json(&self) -> serde_json::Value {
        match *self {
            DrawnValue::Date { at, with_time } => {
                let date = format!("{:04}-{:02}-{:02}", at.year, at.month, at.day);
                let written = if with_time { format!("{date}T{:02}:{:02}:{:02}", at.hour, at.minute, at.second) } else { date };
                written.into()
            }
            DrawnValue::Duration { amount, unit } => serde_json::json!({ "amount": amount, "unit": UNIT_WORDS[unit].0 }),
            DrawnValue::Variant(label) => VARIANT_DAYS[label].0.into(),
        }
    }

    /// The value this is, worked out from the model.
    pub(crate) fn value(&self) -> ParamValue {
        match *self {
            DrawnValue::Date { at, .. } => ParamValue::Date(at),
            DrawnValue::Duration { amount, unit } => ParamValue::Duration { amount, unit: UNIT_WORDS[unit].1 },
            DrawnValue::Variant(label) => ParamValue::Variant(VARIANT_DAYS[label].1),
        }
    }
}

fn drawn_value(kind: ParamKind) -> BoxedStrategy<DrawnValue> {
    match kind {
        ParamKind::Date => (1i64..=9999, 1u32..=12, 1u32..=28, 0u32..24, 0u32..60, 0u32..60, any::<bool>())
            .prop_map(|(year, month, day, hour, minute, second, with_time)| {
                let (hour, minute, second) = if with_time { (hour, minute, second) } else { (0, 0, 0) };
                DrawnValue::Date { at: CivilDateTime { year, month, day, hour, minute, second }, with_time }
            })
            .boxed(),
        ParamKind::Duration => (prop_oneof![0i64..=1000, Just(i64::MAX)], 0..UNIT_WORDS.len())
            .prop_map(|(amount, unit)| DrawnValue::Duration { amount, unit })
            .boxed(),
        ParamKind::Variant => (0..VARIANT_DAYS.len()).prop_map(DrawnValue::Variant).boxed(),
    }
}

/// A parameter a preset declares, and the value it is filled with when its moment is resolved.
#[derive(Debug, Clone)]
pub(crate) struct DrawnParameter {
    pub(crate) id: String,
    pub(crate) kind: ParamKind,
    pub(crate) default: Option<DrawnValue>,
    /// `target_file_creation`, the one hint built, on a date.
    pub(crate) hint: bool,
    pub(crate) label: bool,
    pub(crate) value: DrawnValue,
}

impl DrawnParameter {
    fn json(&self) -> serde_json::Value {
        let mut json = serde_json::json!({ "id": self.id, "type": self.kind.name() });
        if let Some(default) = &self.default {
            json["default"] = default.json();
        }
        if self.hint {
            json["default_hint"] = "target_file_creation".into();
        }
        if self.label {
            json["label"] = serde_json::json!({ "en": format!("Label of {}", self.id), "pl": format!("Etykieta {}", self.id) });
        }
        json
    }
}

fn drawn_parameter(id: String) -> impl Strategy<Value = DrawnParameter> {
    prop_oneof![Just(ParamKind::Date), Just(ParamKind::Duration), Just(ParamKind::Variant)].prop_flat_map(move |kind| {
        let id = id.clone();
        (prop::option::of(drawn_value(kind)), any::<bool>(), any::<bool>(), drawn_value(kind)).prop_map(
            move |(default, hint, label, value)| DrawnParameter {
                id: id.clone(),
                kind,
                default,
                hint: hint && kind == ParamKind::Date,
                label,
                value,
            },
        )
    })
}

#[derive(Debug, Clone)]
pub(crate) enum DrawnBase {
    Today,
    Now,
    /// Wall-clock in the session zone, with a space for the `T` when `space`.
    Absolute { at: CivilDateTime, space: bool },
    /// An instant, with a trailing `Z` when `zulu`.
    AbsoluteUtc { at: CivilDateTime, zulu: bool },
    /// The date parameter at this position.
    Parameter(usize),
}

#[derive(Debug, Clone)]
pub(crate) enum DrawnStep {
    Shift { sign: Sign, amount: i64, unit: usize },
    /// Sized by the duration parameter at this position.
    ByDuration { sign: Sign, parameter: usize },
    /// Moved by the variant parameter at this position, which carries its own direction.
    ByVariant(usize),
    SetTime(u32, u32, u32),
    Snap(usize),
    Nearest(usize),
    Zone { sign: Sign, hours: u32, minutes: u32 },
}

/// A preset as its author may write one, every field within what the schema allows (docs/04 4.2).
#[derive(Debug, Clone)]
pub(crate) struct DrawnPreset {
    pub(crate) id: String,
    pub(crate) applies_to: &'static str,
    pub(crate) market: Option<&'static str>,
    /// `time_mode`, with its multiplier when it writes one and `scale_duration_clock`.
    pub(crate) time_mode: Option<(Option<i64>, bool)>,
    /// The name and the "what it tests" line in Polish too, and in one more language as something
    /// other than text, which is left out.
    pub(crate) translated: bool,
    pub(crate) parameters: Vec<DrawnParameter>,
    pub(crate) base: DrawnBase,
    pub(crate) steps: Vec<DrawnStep>,
    /// With a field this reader does not use.
    pub(crate) extra: bool,
}

fn civil_text(at: &CivilDateTime, between: char) -> String {
    format!("{:04}-{:02}-{:02}{between}{:02}:{:02}:{:02}", at.year, at.month, at.day, at.hour, at.minute, at.second)
}

impl DrawnPreset {
    fn base_json(&self) -> serde_json::Value {
        match &self.base {
            DrawnBase::Today => "today".into(),
            DrawnBase::Now => "now".into(),
            DrawnBase::Absolute { at, space } => serde_json::json!({ "absolute": civil_text(at, if *space { ' ' } else { 'T' }) }),
            DrawnBase::AbsoluteUtc { at, zulu } => {
                serde_json::json!({ "absolute_utc": civil_text(at, 'T') + if *zulu { "Z" } else { "" } })
            }
            DrawnBase::Parameter(at) => serde_json::json!({ "parameter": self.parameters[*at].id }),
        }
    }

    fn step_json(&self, step: &DrawnStep) -> serde_json::Value {
        match *step {
            DrawnStep::Shift { sign, amount, unit } => {
                serde_json::json!({ "shift": { "sign": sign_text(sign), "amount": amount, "unit": UNIT_WORDS[unit].0 } })
            }
            DrawnStep::ByDuration { sign, parameter } => {
                serde_json::json!({ "shift": { "sign": sign_text(sign), "parameter": self.parameters[parameter].id } })
            }
            DrawnStep::ByVariant(parameter) => serde_json::json!({ "shift": { "parameter": self.parameters[parameter].id } }),
            DrawnStep::SetTime(hour, minute, second) => serde_json::json!({ "set_time": format!("{hour:02}:{minute:02}:{second:02}") }),
            DrawnStep::Snap(word) => serde_json::json!({ "snap": SNAP_WORDS[word].0 }),
            DrawnStep::Nearest(word) => serde_json::json!({ "nearest": NEAREST_WORDS[word].0 }),
            DrawnStep::Zone { sign, hours, minutes } => {
                serde_json::json!({ "zone": format!("{}{hours:02}:{minutes:02}", sign_text(sign)) })
            }
        }
    }

    /// The preset file.
    pub(crate) fn json(&self) -> serde_json::Value {
        let text = |what: &str| {
            let mut text = serde_json::json!({ "en": format!("{what} of {}", self.id) });
            if self.translated {
                text["pl"] = format!("{what} po polsku").into();
                text["de"] = 5.into();
            }
            text
        };
        let mut json = serde_json::json!({
            "schema": "chronomock.preset/1",
            "id": self.id,
            "name": text("Name"),
            "explains": text("Explains"),
            "applies_to": self.applies_to,
            "parameters": self.parameters.iter().map(DrawnParameter::json).collect::<Vec<_>>(),
            "moment": { "base": self.base_json(), "steps": self.steps.iter().map(|s| self.step_json(s)).collect::<Vec<_>>() },
        });
        if let Some((multiplier, scale)) = self.time_mode {
            json["time_mode"] = serde_json::json!({ "scale_duration_clock": scale });
            if let Some(multiplier) = multiplier {
                json["time_mode"]["multiplier"] = multiplier.into();
            }
        }
        if let Some(market) = self.market {
            json["market"] = market.into();
        }
        if self.extra {
            json["notes"] = serde_json::json!({ "kept": "for the window" });
        }
        json
    }

    /// The value each parameter is filled with.
    pub(crate) fn values(&self) -> HashMap<String, ParamValue> {
        self.parameters.iter().map(|p| (p.id.clone(), p.value.value())).collect()
    }

    /// The moment the preset says, filled with [`Self::values`], worked out from the model.
    pub(crate) fn moment(&self) -> MomentExpr {
        let base = match &self.base {
            DrawnBase::Today => Base::Today,
            DrawnBase::Now => Base::Now,
            DrawnBase::Absolute { at, .. } => Base::Absolute(*at),
            DrawnBase::AbsoluteUtc { at, .. } => Base::AbsoluteUtc(*at),
            DrawnBase::Parameter(p) => match self.parameters[*p].value.value() {
                ParamValue::Date(at) => Base::Absolute(at),
                other => unreachable!("a base filled by {other:?}"),
            },
        };
        let steps = self.steps.iter().map(|step| match *step {
            DrawnStep::Shift { sign, amount, unit } => Step::Shift { sign, amount, unit: UNIT_WORDS[unit].1 },
            DrawnStep::ByDuration { sign, parameter } => match self.parameters[parameter].value.value() {
                ParamValue::Duration { amount, unit } => Step::Shift { sign, amount, unit },
                other => unreachable!("a shift sized by {other:?}"),
            },
            DrawnStep::ByVariant(parameter) => match self.parameters[parameter].value.value() {
                ParamValue::Variant(days) => {
                    Step::Shift { sign: if days < 0 { Sign::Minus } else { Sign::Plus }, amount: days.abs(), unit: Unit::Days }
                }
                other => unreachable!("a shift moved by {other:?}"),
            },
            DrawnStep::SetTime(hour, minute, second) => Step::SetTime { hour, minute, second },
            DrawnStep::Snap(word) => Step::Snap(SNAP_WORDS[word].1),
            DrawnStep::Nearest(word) => Step::Nearest(NEAREST_WORDS[word].1),
            DrawnStep::Zone { sign, hours, minutes } => {
                let east = i32::try_from(hours * 60 + minutes).expect("a zone in the band");
                // UTC = local + bias, so a zone east of UTC has a negative bias.
                Step::Zone(if sign == Sign::Plus { -east } else { east })
            }
        });
        MomentExpr { base, steps: steps.collect() }
    }
}

fn drawn_civil() -> impl Strategy<Value = CivilDateTime> {
    (1i64..=9999, 1u32..=12, 1u32..=28, 0u32..24, 0u32..60, 0u32..60)
        .prop_map(|(year, month, day, hour, minute, second)| CivilDateTime { year, month, day, hour, minute, second })
}

fn drawn_step(durations: Vec<usize>, variants: Vec<usize>) -> BoxedStrategy<DrawnStep> {
    let sign = || prop_oneof![Just(Sign::Plus), Just(Sign::Minus)];
    let time_of_day = prop_oneof![Just((0u32, 0u32, 0u32)), Just((23, 59, 59)), (0u32..24, 0u32..60, 0u32..60)];
    let mut steps: Vec<BoxedStrategy<DrawnStep>> = vec![
        (sign(), prop_oneof![4 => 0i64..=1000, 1 => Just(i64::MAX)], 0..UNIT_WORDS.len())
            .prop_map(|(sign, amount, unit)| DrawnStep::Shift { sign, amount, unit })
            .boxed(),
        time_of_day.prop_map(|(hour, minute, second)| DrawnStep::SetTime(hour, minute, second)).boxed(),
        (0..SNAP_WORDS.len()).prop_map(DrawnStep::Snap).boxed(),
        (0..NEAREST_WORDS.len()).prop_map(DrawnStep::Nearest).boxed(),
        (sign(), prop_oneof![Just(0u32), Just(14), 0u32..=14], prop_oneof![Just(0u32), Just(59), 0u32..60])
            .prop_map(|(sign, hours, minutes)| DrawnStep::Zone { sign, hours, minutes })
            .boxed(),
    ];
    if !durations.is_empty() {
        steps.push(
            (sign(), prop::sample::select(durations))
                .prop_map(|(sign, parameter)| DrawnStep::ByDuration { sign, parameter })
                .boxed(),
        );
    }
    if !variants.is_empty() {
        steps.push(prop::sample::select(variants).prop_map(DrawnStep::ByVariant).boxed());
    }
    Union::new(steps).boxed()
}

/// A preset within its schema. `small` keeps it to three parameters and five steps, so a flaw can be
/// added to it without becoming a second one (the limits are 16 and 64). Otherwise the two limits
/// themselves are drawn by name.
pub(crate) fn drawn_preset(small: bool) -> impl Strategy<Value = DrawnPreset> {
    let parameters = if small {
        (0usize..=3).prop_flat_map(|n| prop::sample::subsequence(PARAMETER_IDS.to_vec(), n)).prop_map(|ids| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>()).boxed()
    } else {
        prop_oneof![
            8 => (0usize..=3).prop_flat_map(|n| prop::sample::subsequence(PARAMETER_IDS.to_vec(), n)).prop_map(|ids| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>()),
            1 => Just((0..16).map(|i| format!("p{i}")).collect::<Vec<_>>()),
        ]
        .boxed()
    };
    let parameters = parameters.prop_flat_map(|ids| ids.into_iter().map(drawn_parameter).collect::<Vec<_>>());
    let step_count = if small { (0usize..=5).boxed() } else { prop_oneof![8 => 0usize..=5, 1 => Just(64usize)].boxed() };
    (parameters, step_count).prop_flat_map(|(parameters, step_count)| {
        let of_kind = |kind| parameters.iter().enumerate().filter(|(_, p)| p.kind == kind).map(|(i, _)| i).collect::<Vec<_>>();
        let dates = of_kind(ParamKind::Date);
        let mut bases: Vec<BoxedStrategy<DrawnBase>> = vec![
            Just(DrawnBase::Today).boxed(),
            Just(DrawnBase::Now).boxed(),
            (drawn_civil(), any::<bool>()).prop_map(|(at, space)| DrawnBase::Absolute { at, space }).boxed(),
            (drawn_civil(), any::<bool>()).prop_map(|(at, zulu)| DrawnBase::AbsoluteUtc { at, zulu }).boxed(),
        ];
        if !dates.is_empty() {
            bases.push(prop::sample::select(dates).prop_map(DrawnBase::Parameter).boxed());
        }
        let steps = prop::collection::vec(drawn_step(of_kind(ParamKind::Duration), of_kind(ParamKind::Variant)), step_count);
        let id = prop::sample::select(vec!["drawn", "x", "A-1_b", "trial-last-day"]).prop_map(str::to_string);
        let applies_to = prop::sample::select(vec!["calculator", "substitution", "both"]);
        let market = prop::option::of(prop::sample::select(vec!["us", "pl", "xx"]));
        let multiplier = prop_oneof![Just(1i64), Just(2), Just(60), Just(chrono_core::MULTIPLIER_MAX), 1..=chrono_core::MULTIPLIER_MAX];
        let time_mode = prop::option::of((prop::option::of(multiplier), any::<bool>()));
        (Just(parameters), Union::new(bases), steps, id, applies_to, market, time_mode, any::<bool>(), any::<bool>()).prop_map(
            |(parameters, base, steps, id, applies_to, market, time_mode, translated, extra)| DrawnPreset {
                id,
                applies_to,
                market,
                time_mode,
                translated,
                parameters,
                base,
                steps,
                extra,
            },
        )
    })
}

/// One thing wrong with a preset, by name - each a refusal its readers owe.
#[derive(Debug, Clone)]
pub(crate) enum PresetFlaw {
    Schema(&'static str),
    /// A typo read as a deliberate choice by each surface alone.
    AppliesTo(&'static str),
    /// R5-B2: past the limits on steps and parameters.
    TooManySteps,
    TooManyParameters,
    /// R4-N33: a preset that says two things at once.
    ParameterTwice,
    BaseTwoFields(usize),
    ShiftByParameterWithAmount,
    VariantWithSign,
    UndeclaredInBase,
    UndeclaredInShift,
    /// R4-S12: a time mode the session refuses.
    Multiplier(i64),
    /// A parameter type this build does not resolve - "not built", exit 5.
    UnknownType(&'static str),
    DateDefault(&'static str),
    NegativeDurationDefault,
    UnknownUnitDefault(&'static str),
    UnknownVariantDefault(&'static str),
    UtcWithOffset(&'static str),
    UnknownBase(&'static str),
    BaseByDuration,
    /// A step that does not resolve, at `at` among the others (R5-D5 item 2 - named by its number).
    Step { at: prop::sample::Index, step: StepFlaw },
}

#[derive(Debug, Clone)]
pub(crate) enum StepFlaw {
    UnknownSnap(&'static str),
    UnknownNearest(&'static str),
    Zone(&'static str),
    UnknownUnit(&'static str),
    NegativeAmount,
    Sign(Option<&'static str>),
    ShiftByDate,
    /// A time of day past the day - refused by the evaluator when the preset is used, and by the
    /// catalogue before that.
    SetTime(&'static str),
}

pub(crate) fn preset_flaw() -> impl Strategy<Value = PresetFlaw> {
    let step = prop_oneof![
        prop::sample::select(vec!["end-of-week", "EOM", "eom "]).prop_map(StepFlaw::UnknownSnap),
        prop::sample::select(vec!["next-holiday", "NBD"]).prop_map(StepFlaw::UnknownNearest),
        prop::sample::select(vec!["+15:00", "++05:00", "+05:60", "05:00", "+5", "+05:+30"]).prop_map(StepFlaw::Zone),
        prop::sample::select(vec!["Days", "mth", "", "dd"]).prop_map(StepFlaw::UnknownUnit),
        Just(StepFlaw::NegativeAmount),
        prop::sample::select(vec![None, Some("plus"), Some("")]).prop_map(StepFlaw::Sign),
        Just(StepFlaw::ShiftByDate),
        prop::sample::select(vec!["24:00:00", "12:60:00", "00:00:60", "99:99:99"]).prop_map(StepFlaw::SetTime),
    ];
    let whole = prop_oneof![
        prop::sample::select(vec!["chronomock.preset/2", "Chronomock.preset/1", ""]).prop_map(PresetFlaw::Schema),
        prop::sample::select(vec!["Calculator", "calc", "", "everywhere"]).prop_map(PresetFlaw::AppliesTo),
        Just(PresetFlaw::TooManySteps),
        Just(PresetFlaw::TooManyParameters),
        Just(PresetFlaw::ParameterTwice),
        (0usize..4).prop_map(PresetFlaw::BaseTwoFields),
        Just(PresetFlaw::ShiftByParameterWithAmount),
        Just(PresetFlaw::VariantWithSign),
        Just(PresetFlaw::UndeclaredInBase),
        Just(PresetFlaw::UndeclaredInShift),
        prop::sample::select(vec![0i64, -1, chrono_core::MULTIPLIER_MAX + 1, i64::MIN, i64::MAX]).prop_map(PresetFlaw::Multiplier),
        prop::sample::select(vec!["int", "Date", ""]).prop_map(PresetFlaw::UnknownType),
        prop::sample::select(vec!["2026-02-30", "2026-02-29", "2026-13-01", "2026/01/01", "tomorrow"]).prop_map(PresetFlaw::DateDefault),
        Just(PresetFlaw::NegativeDurationDefault),
        prop::sample::select(vec!["fortnight", "Days"]).prop_map(PresetFlaw::UnknownUnitDefault),
        prop::sample::select(vec!["day_befor", "Day_before", ""]).prop_map(PresetFlaw::UnknownVariantDefault),
        prop::sample::select(vec!["2026-01-01T00:00:00+02:00", "2026-01-01T00:00:00-05:00"]).prop_map(PresetFlaw::UtcWithOffset),
        prop::sample::select(vec!["tomorrow", "Today", "", "now "]).prop_map(PresetFlaw::UnknownBase),
        Just(PresetFlaw::BaseByDuration),
    ];
    // Nineteen flaws of the whole file and eight of a step, each step flaw about as often as one of
    // the whole file.
    prop_oneof![
        19 => whole,
        8 => (any::<prop::sample::Index>(), step).prop_map(|(at, step)| PresetFlaw::Step { at, step }),
    ]
}

/// What a reader of a flawed preset owes: the pieces its refusal says, and the exit code it ends with.
#[derive(Debug)]
pub(crate) struct Owed {
    pub(crate) said: Vec<String>,
    pub(crate) exit_code: i32,
    /// Refused only by what checks a step before it is used - the catalogue - and by the evaluator.
    pub(crate) unused_until_evaluated: bool,
    /// Values for the parameters the flaw declared, to resolve with.
    pub(crate) values: HashMap<String, ParamValue>,
}

/// The id of a parameter a flaw declares - outside the ids a preset is drawn with.
const FLAWED: &str = "flawed";

/// `preset` with `flaw` and nothing else wrong, and what is owed for it.
pub(crate) fn with_preset_flaw(preset: &DrawnPreset, flaw: &PresetFlaw) -> (serde_json::Value, Owed) {
    let mut json = preset.json();
    let mut owed = Owed { said: Vec::new(), exit_code: 1, unused_until_evaluated: false, values: preset.values() };
    let said = match flaw {
        PresetFlaw::Schema(schema) => {
            json["schema"] = (*schema).into();
            vec![format!("unsupported preset schema '{schema}'")]
        }
        PresetFlaw::AppliesTo(word) => {
            json["applies_to"] = (*word).into();
            vec![format!("applies_to must be calculator, substitution or both, got '{word}'")]
        }
        PresetFlaw::TooManySteps => {
            json["moment"]["steps"] = vec![serde_json::json!({ "snap": "eom" }); 65].into();
            vec!["preset has 65 steps, past the limit of 64".to_string()]
        }
        PresetFlaw::TooManyParameters => {
            let list = json["parameters"].as_array_mut().expect("a list");
            for i in 0..(17 - list.len()) {
                list.push(serde_json::json!({ "id": format!("p{i}"), "type": "date" }));
            }
            vec!["preset declares 17 parameters, past the limit of 16".to_string()]
        }
        PresetFlaw::ParameterTwice => {
            if preset.parameters.is_empty() {
                declare(&mut json, ParamKind::Date, &mut owed);
            }
            let list = json["parameters"].as_array_mut().expect("a list");
            let first = list[0].clone();
            let id = first["id"].as_str().expect("an id").to_string();
            list.push(first);
            vec![format!("parameter '{id}' is declared twice")]
        }
        PresetFlaw::BaseTwoFields(pair) => {
            declare(&mut json, ParamKind::Date, &mut owed);
            let fields: &[&str] = [&["absolute", "absolute_utc"][..], &["absolute", "parameter"], &["absolute_utc", "parameter"], &["absolute", "absolute_utc", "parameter"]][*pair];
            let mut base = serde_json::json!({});
            for field in fields {
                base[*field] = if *field == "parameter" { FLAWED.into() } else { "2026-01-01T00:00:00".into() };
            }
            json["moment"]["base"] = base;
            let named = fields.iter().map(|f| format!("'{f}'")).collect::<Vec<_>>().join(" and ");
            vec![format!("preset base names {named} at once")]
        }
        PresetFlaw::ShiftByParameterWithAmount => {
            declare(&mut json, ParamKind::Duration, &mut owed);
            push_step(&mut json, serde_json::json!({ "shift": { "sign": "+", "parameter": FLAWED, "amount": 1, "unit": "d" } }));
            vec![format!("a shift with parameter '{FLAWED}' also names an amount or a unit")]
        }
        PresetFlaw::VariantWithSign => {
            declare(&mut json, ParamKind::Variant, &mut owed);
            push_step(&mut json, serde_json::json!({ "shift": { "sign": "-", "parameter": FLAWED } }));
            vec![format!("a shift with variant parameter '{FLAWED}' also names a sign")]
        }
        PresetFlaw::UndeclaredInBase => {
            json["moment"]["base"] = serde_json::json!({ "parameter": "ghost" });
            vec!["the base names parameter 'ghost', which the preset does not declare".to_string()]
        }
        PresetFlaw::UndeclaredInShift => {
            push_step(&mut json, serde_json::json!({ "shift": { "sign": "+", "parameter": "ghost" } }));
            vec!["a shift names parameter 'ghost', which the preset does not declare".to_string()]
        }
        PresetFlaw::Multiplier(multiplier) => {
            json["time_mode"] = serde_json::json!({ "multiplier": multiplier });
            vec!["time_mode multiplier must be".to_string(), format!("got {multiplier}")]
        }
        PresetFlaw::UnknownType(kind) => {
            json["parameters"].as_array_mut().expect("a list").push(serde_json::json!({ "id": FLAWED, "type": kind }));
            owed.exit_code = 5;
            vec![format!("parameter '{FLAWED}' has type '{kind}', which calc does not resolve yet")]
        }
        PresetFlaw::DateDefault(text) => {
            json["parameters"].as_array_mut().expect("a list").push(serde_json::json!({ "id": FLAWED, "type": "date", "default": text }));
            vec![format!("'{text}'")]
        }
        PresetFlaw::NegativeDurationDefault => {
            let default = serde_json::json!({ "amount": -1, "unit": "d" });
            json["parameters"].as_array_mut().expect("a list").push(serde_json::json!({ "id": FLAWED, "type": "duration", "default": default }));
            vec![format!("parameter '{FLAWED}' amount must be non-negative")]
        }
        PresetFlaw::UnknownUnitDefault(unit) => {
            let default = serde_json::json!({ "amount": 1, "unit": unit });
            json["parameters"].as_array_mut().expect("a list").push(serde_json::json!({ "id": FLAWED, "type": "duration", "default": default }));
            vec![format!("parameter '{FLAWED}': unknown unit '{unit}'")]
        }
        PresetFlaw::UnknownVariantDefault(label) => {
            json["parameters"].as_array_mut().expect("a list").push(serde_json::json!({ "id": FLAWED, "type": "variant", "default": label }));
            vec![format!("parameter '{FLAWED}': unknown variant '{label}'")]
        }
        PresetFlaw::UtcWithOffset(at) => {
            json["moment"]["base"] = serde_json::json!({ "absolute_utc": at });
            vec![format!("carries an offset ('{at}')")]
        }
        PresetFlaw::UnknownBase(word) => {
            json["moment"]["base"] = (*word).into();
            vec![format!("unknown preset base '{word}'")]
        }
        PresetFlaw::BaseByDuration => {
            declare(&mut json, ParamKind::Duration, &mut owed);
            json["moment"]["base"] = serde_json::json!({ "parameter": FLAWED });
            vec![format!("base parameter '{FLAWED}' must be a date, not a duration")]
        }
        PresetFlaw::Step { at, step } => {
            let (written, said) = match step {
                StepFlaw::UnknownSnap(word) => (serde_json::json!({ "snap": word }), format!("unknown snap target '{word}'")),
                StepFlaw::UnknownNearest(word) => (serde_json::json!({ "nearest": word }), format!("unknown nearest target '{word}'")),
                StepFlaw::Zone(zone) => (serde_json::json!({ "zone": zone }), format!("'{zone}'")),
                StepFlaw::UnknownUnit(unit) => (
                    serde_json::json!({ "shift": { "sign": "+", "amount": 1, "unit": unit } }),
                    format!("unknown unit '{unit}'"),
                ),
                StepFlaw::NegativeAmount => (
                    serde_json::json!({ "shift": { "sign": "+", "amount": -1, "unit": "d" } }),
                    "shift amount must be non-negative".to_string(),
                ),
                StepFlaw::Sign(None) => (serde_json::json!({ "shift": { "amount": 1, "unit": "d" } }), "shift needs a sign".to_string()),
                StepFlaw::Sign(Some(sign)) => (
                    serde_json::json!({ "shift": { "sign": sign, "amount": 1, "unit": "d" } }),
                    format!("shift sign must be + or -, got '{sign}'"),
                ),
                StepFlaw::ShiftByDate => {
                    declare(&mut json, ParamKind::Date, &mut owed);
                    (
                        serde_json::json!({ "shift": { "sign": "+", "parameter": FLAWED } }),
                        format!("shift parameter '{FLAWED}' must be a duration or variant, not a date"),
                    )
                }
                StepFlaw::SetTime(time) => {
                    owed.unused_until_evaluated = true;
                    (serde_json::json!({ "set_time": time }), format!("set_time {time} is not a time of day"))
                }
            };
            let steps = json["moment"]["steps"].as_array_mut().expect("a list");
            let place = at.index(steps.len() + 1);
            steps.insert(place, written);
            vec![format!("step {}: ", place + 1), said]
        }
    };
    owed.said = said;
    (json, owed)
}

/// Declare the flaw's own parameter, of `kind`, and the value it is filled with.
fn declare(json: &mut serde_json::Value, kind: ParamKind, owed: &mut Owed) {
    json["parameters"].as_array_mut().expect("a list").push(serde_json::json!({ "id": FLAWED, "type": kind.name() }));
    let value = match kind {
        ParamKind::Date => ParamValue::Date(CivilDateTime { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 }),
        ParamKind::Duration => ParamValue::Duration { amount: 1, unit: Unit::Days },
        ParamKind::Variant => ParamValue::Variant(0),
    };
    owed.values.insert(FLAWED.to_string(), value);
}

fn push_step(json: &mut serde_json::Value, step: serde_json::Value) {
    json["moment"]["steps"].as_array_mut().expect("a list").push(step);
}

mod properties {
    use proptest::prelude::*;

    proptest! {
        /// The canary of the weekly run: red when CHRONO_SEARCH_CANARY is set, and a property that
        /// holds in every other run. Each part runs it first and stops if it does NOT see it fail,
        /// so a part that then finds nothing has shown it would have said so.
        #[test]
        fn the_canary_fails_only_when_asked_to(drawn in any::<u64>()) {
            let asked = std::env::var_os("CHRONO_SEARCH_CANARY").is_some();
            prop_assert!(!asked, "the canary failed, as it was asked to (drew {})", drawn);
        }
    }
}
