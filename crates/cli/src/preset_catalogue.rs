//! `chrono presets` - the preset catalogue, read by the engine for every surface (R4/18, ADR-22).
//!
//! The window used to read the preset files itself, with its own copy of the step grammar, and the
//! copy had drifted: a default of one `month` became one day, a variant it did not know became
//! `day_before`, a step the engine refuses was computed, and a file the engine reads was offered as
//! "fill in the parameters" with no parameters to fill (R4-S22). Two readers of one file format give
//! two answers sooner or later, so now there is one. This module lists a catalogue folder through
//! [`load_preset_at`] - the same function `--preset` uses once it has found a file - and hands each
//! file on in one of two shapes:
//!
//! * a preset in its CANONICAL form: every word written the way the step grammar reads it, derived
//!   from the engine's own types rather than copied from the file, so the window maps words to its
//!   controls and never has to interpret one, or
//! * a refusal, with the sentence `--preset` gives for the same file.
//!
//! The invariant is the point: a file is listed exactly when `calc --preset` and `run --preset`
//! accept it, with values for its parameters or without, as the file decides. Everything the moment
//! says that does not depend on a parameter's value is checked here by the functions that check it
//! when the preset is used, with a stand-in value of the declared type wherever a parameter goes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono_core::calc::{Base, CivilDateTime, Step, Unit};
use serde::Serialize;

use crate::calendar::{find_catalogue_dir, is_valid_catalogue_id, parse_catalogue_args};
use crate::grammar::{nearest_code, sign_code, snap_code, unit_code};
use crate::output::{diag, out, outln};
use crate::preset::{
    base_from, calendar_for_market, load_preset_at, step_from, variant_label, BaseDto, ParamKind, ParamValue,
    Parameter, Preset, PresetError, StepDto, ShiftDto, VARIANTS,
};
use crate::zone::format_bias;

/// The machine shape of the catalogue - additive fields only, like every other `chronomock.*/1`.
const PRESETS_SCHEMA: &str = "chronomock.presets/1";

/// The whole catalogue: where it was read from, the presets it holds, and the files it left out.
#[derive(Debug, Serialize)]
pub(crate) struct CatalogueJson {
    schema: &'static str,
    dir: String,
    presets: Vec<PresetEntryJson>,
    refused: Vec<RefusedJson>,
}

/// One preset, ready for a surface that shows it: the texts in every language the file has, which
/// module it is for, the calendar its market implies, and its parameters and moment in canonical form.
#[derive(Debug, Serialize)]
pub(crate) struct PresetEntryJson {
    file: String,
    id: String,
    applies_to: String,
    market: Option<String>,
    /// The calendar a business-day step counts in when nobody names one - the pairs of
    /// `calendar_for_market`, so no surface keeps a copy of that table.
    calendar: Option<&'static str>,
    name: BTreeMap<String, String>,
    explains: BTreeMap<String, String>,
    time_mode: TimeModeJson,
    parameters: Vec<ParameterJson>,
    moment: MomentJson,
}

#[derive(Debug, Serialize)]
struct TimeModeJson {
    multiplier: i64,
    scale_duration_clock: bool,
}

/// A parameter as a surface needs it to offer an input: its type, its default in canonical form (a
/// date as ISO, a duration as an amount and a unit code, a variant as its label), and for a variant
/// the choices with the day offset each one moves the boundary by.
#[derive(Debug, Serialize)]
struct ParameterJson {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    /// What a reader calls it, per language - always present, and empty when the file names none, in
    /// which case a surface shows the id (R4/19).
    label: BTreeMap<String, String>,
    default: serde_json::Value,
    default_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    choices: Option<Vec<ChoiceJson>>,
}

#[derive(Debug, Serialize)]
struct ChoiceJson {
    label: &'static str,
    days: i64,
}

#[derive(Debug, Serialize)]
struct MomentJson {
    base: BaseJson,
    steps: Vec<StepJson>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BaseJson {
    Today,
    Now,
    Absolute { at: String },
    AbsoluteUtc { at: String },
    Parameter { parameter: String },
}

/// One step in canonical form. A shift is literal (`sign`, `amount`, `unit`), filled by a duration
/// parameter (`sign`, `parameter`), or filled by a variant parameter (`parameter` alone - a variant
/// carries its own direction). Absent fields are left out rather than written as null.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StepJson {
    Shift {
        #[serde(skip_serializing_if = "Option::is_none")]
        sign: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        amount: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        unit: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parameter: Option<String>,
    },
    Snap { target: &'static str },
    Nearest { target: &'static str },
    SetTime { time: String },
    Zone { offset: String },
}

/// A file the catalogue left out, with the sentence `--preset` gives for it and the exit code it would
/// end with. `id` is the name the file is found by, absent when that name cannot be one.
#[derive(Debug, Serialize)]
struct RefusedJson {
    file: String,
    id: Option<String>,
    reason: String,
    exit_code: i32,
}

/// Read every preset file in `dir`. `Err` only when the folder itself cannot be read - a file that
/// cannot be used is a refusal inside the answer, not a failure of it.
pub(crate) fn read_catalogue(dir: &Path) -> Result<CatalogueJson, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read the preset folder {}: {e}", dir.display()))?;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut refused = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                let is_json = path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
                if is_json && path.is_file() {
                    files.push(path);
                }
            }
            // Said, not skipped: a folder whose listing broke half way is not a folder with fewer files.
            Err(e) => refused.push(RefusedJson {
                file: String::new(),
                id: None,
                reason: format!("an entry of {} could not be read: {e}", dir.display()),
                exit_code: 1,
            }),
        }
    }
    // By file name without case, the way Windows orders and finds them - stable on every machine.
    files.sort_by_key(|path| file_name(path).to_lowercase());

    let mut presets = Vec::new();
    for path in files {
        match catalogue_entry(&path) {
            Ok(entry) => presets.push(entry),
            Err(refusal) => refused.push(refusal),
        }
    }
    Ok(CatalogueJson { schema: PRESETS_SCHEMA, dir: dir.display().to_string(), presets, refused })
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// One file, listed or refused.
fn catalogue_entry(path: &Path) -> Result<PresetEntryJson, RefusedJson> {
    let file = file_name(path);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
    // `--preset` takes an id, and an id is the file name - a name it refuses can never be used.
    if !is_valid_catalogue_id(&stem) {
        return Err(RefusedJson {
            reason: format!(
                "'{file}' cannot be named by --preset - a preset's file name is its id, made of letters, digits, '-' and '_'"
            ),
            file,
            id: None,
            exit_code: 1,
        });
    }
    let refusal = |e: PresetError, file: String, id: String| RefusedJson {
        reason: e.message().to_string(),
        exit_code: e.exit_code(),
        file,
        id: Some(id),
    };
    let preset = load_preset_at(path, &stem).map_err(|e| refusal(e, file.clone(), stem.clone()))?;
    canonical_entry(&preset, file.clone()).map_err(|e| refusal(e, file, stem))
}

/// A stand-in value of a parameter's type. The catalogue checks a moment before anybody has filled
/// it, and a stand-in lets the functions that resolve a preset check every piece that does not depend
/// on the value - a base filled by a duration is refused whatever the duration is.
fn stand_in(kind: ParamKind) -> ParamValue {
    match kind {
        ParamKind::Date => ParamValue::Date(CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 }),
        ParamKind::Duration => ParamValue::Duration { amount: 1, unit: Unit::Days },
        ParamKind::Variant => ParamValue::Variant(0),
    }
}

fn canonical_entry(preset: &Preset, file: String) -> Result<PresetEntryJson, PresetError> {
    let stand_ins: HashMap<String, ParamValue> =
        preset.parameters.iter().map(|p| (p.id.clone(), stand_in(p.kind))).collect();
    let base = canonical_base(&preset.moment.base, &stand_ins)?;
    // A step is named by its number, as `--preset` names it: the window shows this refusal, and a
    // preset's steps are a list (found by the search in step 2d, the class of R5-D5 item 2).
    let steps = preset
        .moment
        .steps
        .iter()
        .enumerate()
        .map(|(i, step)| canonical_step(step, &stand_ins, &preset.parameters).map_err(|e| e.at_step(i)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PresetEntryJson {
        file,
        id: preset.id.clone(),
        applies_to: preset.applies_to.clone(),
        market: preset.market.clone(),
        calendar: calendar_for_market(preset.market.as_deref()),
        name: preset.name_texts.clone(),
        explains: preset.explains_texts.clone(),
        time_mode: TimeModeJson {
            multiplier: preset.time_mode.multiplier.unwrap_or(1),
            scale_duration_clock: preset.time_mode.scale_duration,
        },
        parameters: preset.parameters.iter().map(canonical_parameter).collect(),
        moment: MomentJson { base, steps },
    })
}

fn canonical_base(base: &BaseDto, stand_ins: &HashMap<String, ParamValue>) -> Result<BaseJson, PresetError> {
    let resolved = base_from(base.clone(), stand_ins)?;
    if let BaseDto::Object { parameter: Some(id), .. } = base {
        return Ok(BaseJson::Parameter { parameter: id.clone() });
    }
    Ok(match resolved {
        Base::Today => BaseJson::Today,
        Base::Now => BaseJson::Now,
        Base::Absolute(at) => BaseJson::Absolute { at: at.to_iso() },
        Base::AbsoluteUtc(at) => BaseJson::AbsoluteUtc { at: at.to_iso() },
    })
}

fn canonical_step(
    step: &StepDto,
    stand_ins: &HashMap<String, ParamValue>,
    parameters: &[Parameter],
) -> Result<StepJson, PresetError> {
    let typed = step_from(step.clone(), stand_ins)?;
    if let StepDto::Shift(ShiftDto { parameter: Some(id), .. }) = step {
        let variant = parameters.iter().any(|p| &p.id == id && p.kind == ParamKind::Variant);
        let sign = match typed {
            Step::Shift { sign, .. } if !variant => Some(sign_code(sign)),
            _ => None,
        };
        return Ok(StepJson::Shift { sign, amount: None, unit: None, parameter: Some(id.clone()) });
    }
    Ok(match typed {
        Step::Shift { sign, amount, unit } => {
            StepJson::Shift { sign: Some(sign_code(sign)), amount: Some(amount), unit: Some(unit_code(unit)), parameter: None }
        }
        Step::SetTime { hour, minute, second } => {
            let time = format!("{hour:02}:{minute:02}:{second:02}");
            // Refused at every use whatever the moment (the evaluator's own range check), so a file
            // carrying it is a file `--preset` never accepts.
            if !chrono_core::calc::is_time_of_day(hour, minute, second) {
                return Err(PresetError::BadFile(format!(
                    "set_time {time} is not a time of day (00:00:00 to 23:59:59)"
                )));
            }
            StepJson::SetTime { time }
        }
        Step::Snap(target) => StepJson::Snap { target: snap_code(target) },
        Step::Nearest(target) => StepJson::Nearest { target: nearest_code(target) },
        Step::Zone(bias) => StepJson::Zone { offset: format_bias(bias) },
    })
}

fn canonical_parameter(p: &Parameter) -> ParameterJson {
    let default = match &p.default {
        None => serde_json::Value::Null,
        Some(ParamValue::Date(date)) => serde_json::Value::String(date.to_iso()),
        Some(ParamValue::Duration { amount, unit }) => serde_json::json!({ "amount": amount, "unit": unit_code(*unit) }),
        Some(ParamValue::Variant(days)) => {
            variant_label(*days).map_or(serde_json::Value::Null, |label| serde_json::Value::String(label.to_string()))
        }
    };
    let choices = (p.kind == ParamKind::Variant)
        .then(|| VARIANTS.iter().map(|&(label, days)| ChoiceJson { label, days }).collect());
    ParameterJson {
        id: p.id.clone(),
        kind: p.kind.name(),
        label: p.label_texts.clone(),
        default,
        default_hint: p.default_hint.clone(),
        choices,
    }
}

/// `chrono presets [--dir <folder>] [--json]`. Exit 0 when the folder was read, whatever it held - a
/// refused file is part of the answer. Exit 1 for a bad argument or a folder that is missing or cannot
/// be read.
pub(crate) fn presets_run(args: &[String]) -> i32 {
    let parsed = match parse_catalogue_args("presets", args) {
        Ok(parsed) => parsed,
        Err(e) => {
            diag!("chrono presets: {e}");
            crate::cli::print_presets_usage(crate::output::Stream::Err);
            return 1;
        }
    };
    let Some(dir) = parsed.dir.or_else(|| find_catalogue_dir("presets")) else {
        diag!(
            "chrono presets: no preset folder ({}) (presets.folder_missing)",
            crate::calendar::catalogue_search_places("presets")
        );
        return 1;
    };
    let catalogue = match read_catalogue(&dir) {
        Ok(catalogue) => catalogue,
        Err(e) => {
            // Two keys, because the two need different hands: a folder that is not there is an install
            // to repair, a folder that is there and refuses to be read is a permission to look at. One
            // key for both told a reader with "Access is denied" that the folder was missing.
            // Written out in full, so the translation guard (wire_keys.rs) sees both.
            if dir.exists() {
                diag!("chrono presets: {e} (presets.folder_unreadable)");
            } else {
                diag!("chrono presets: {e} (presets.folder_missing)");
            }
            return 1;
        }
    };
    if parsed.json {
        match serde_json::to_string(&catalogue) {
            Ok(json) => outln!("{json}"),
            Err(e) => {
                diag!("chrono presets: cannot write the catalogue: {e}");
                return 1;
            }
        }
    } else {
        out!("{}", render_catalogue(&catalogue));
    }
    0
}

/// The catalogue for a person: one line per preset, then every file left out with its reason.
fn render_catalogue(catalogue: &CatalogueJson) -> String {
    let mut out = String::from("Chrono Mock - preset catalogue\n");
    out.push_str(&format!("  folder:  {}\n", catalogue.dir));
    out.push_str(&format!(
        "  presets: {} listed, {} left out\n",
        catalogue.presets.len(),
        catalogue.refused.len()
    ));
    let id_width = catalogue.presets.iter().map(|p| p.id.len()).max().unwrap_or(0);
    if !catalogue.presets.is_empty() {
        out.push('\n');
    }
    for p in &catalogue.presets {
        let name = p.name.get("en").map(String::as_str).unwrap_or_default();
        let mut extras = Vec::new();
        if let Some(market) = &p.market {
            extras.push(format!("market {market}"));
        }
        if !p.parameters.is_empty() {
            let ids = p.parameters.iter().map(|q| q.id.as_str()).collect::<Vec<_>>().join(", ");
            extras.push(format!("parameters: {ids}"));
        }
        let extras = if extras.is_empty() { String::new() } else { format!("   ({})", extras.join(", ")) };
        out.push_str(&format!("  {:<id_width$}  {:<12}  {name}{extras}\n", p.id, p.applies_to));
    }
    if !catalogue.refused.is_empty() {
        out.push_str("\n  left out - `--preset` refuses these too:\n");
        for r in &catalogue.refused {
            out.push_str(&format!("    {}  {}\n", r.file, r.reason));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::{parse_nearest, parse_set_time, parse_shift, parse_snap};
    use crate::preset::resolve_moment;
    use crate::zone::parse_zone_to_bias;
    use chrono_core::calc::{parse_civil_datetime, MomentExpr};

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| (*w).to_string()).collect()
    }

    /// Relative to the crate, where cargo runs its tests, so the folder and every reason that names a
    /// file read the same on every machine - the recorded answer below holds them.
    const TEST_CATALOGUE: &str = "tests/data/preset-catalogue";
    const RECORDED_ANSWER: &str = "tests/data/preset-catalogue.json";

    fn shipped_catalogue() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../presets")
    }

    /// The answer for `dir`, compared with the one recorded in `recorded` - or recorded there when
    /// `CHRONO_BLESS` is set, to be read as a diff before it is kept.
    fn gives_its_recorded_answer(dir: &str, recorded: &str) {
        let catalogue = read_catalogue(Path::new(dir)).expect("the catalogue is readable");
        let got = serde_json::to_string_pretty(&catalogue).expect("the catalogue serialises") + "\n";
        if std::env::var_os("CHRONO_BLESS").is_some() {
            std::fs::write(recorded, &got).expect("the recorded answer is writable");
            return;
        }
        let want = std::fs::read_to_string(recorded).expect("the recorded answer exists").replace("\r\n", "\n");
        if got != want {
            let line = got.lines().zip(want.lines()).position(|(g, w)| g != w).unwrap_or(got.lines().count().min(want.lines().count()));
            panic!(
                "{dir} no longer gives the answer recorded in {recorded}, first difference at line {} - got {:?}, recorded {:?}. \
                 Record a deliberate change with CHRONO_BLESS=1 and read the diff.",
                line + 1,
                got.lines().nth(line),
                want.lines().nth(line)
            );
        }
    }

    /// The test catalogue gives exactly the answer recorded beside it. That file is also what the
    /// window's tests read (`PresetCatalogueTests`), so this is one end of a bridge: a change to what the
    /// engine hands on turns this red, and a change to what the window makes of it turns that red.
    #[test]
    fn the_test_catalogue_gives_its_recorded_answer() {
        gives_its_recorded_answer(TEST_CATALOGUE, RECORDED_ANSWER);
    }

    /// The same for the SHIPPED catalogue, which the window's tests read as the real list - the panel's
    /// scenarios, the calculator's presets. A shipped preset changed without this answer recorded again
    /// turns this red, so the window's tests never run against a catalogue that no longer ships.
    #[test]
    fn the_shipped_catalogue_gives_its_recorded_answer() {
        gives_its_recorded_answer("../../presets", "tests/data/presets-shipped.json");
    }

    /// Every shipped preset is listed and none is refused - a shipped file the catalogue leaves out is a
    /// preset nobody can pick in the window.
    #[test]
    fn every_shipped_preset_is_listed() {
        let dir = shipped_catalogue();
        let catalogue = read_catalogue(&dir).expect("the shipped catalogue is readable");
        let files = std::fs::read_dir(&dir).unwrap().filter(|e| {
            e.as_ref().unwrap().path().extension().is_some_and(|x| x == "json")
        }).count();
        assert!(catalogue.refused.is_empty(), "refused: {:?}", catalogue.refused);
        assert_eq!(catalogue.presets.len(), files);
        assert!(files >= 10, "the shipped catalogue looks empty: {files}");
    }

    /// The moment a listed preset resolves to, rebuilt from its CANONICAL form through the step grammar
    /// alone - the words the window sends back as flags.
    fn moment_from_canonical(entry: &serde_json::Value) -> MomentExpr {
        let kind_of = |id: &str| {
            entry["parameters"].as_array().unwrap().iter().find(|p| p["id"] == id).unwrap()["type"].as_str().unwrap().to_string()
        };
        let at = |v: &serde_json::Value| parse_civil_datetime(v["at"].as_str().unwrap()).unwrap();
        let base_json = &entry["moment"]["base"];
        let base = match base_json["kind"].as_str().unwrap() {
            "today" => Base::Today,
            "now" => Base::Now,
            "absolute" => Base::Absolute(at(base_json)),
            "absolute_utc" => Base::AbsoluteUtc(at(base_json)),
            "parameter" => match stand_in(ParamKind::Date) {
                ParamValue::Date(date) => Base::Absolute(date),
                _ => unreachable!(),
            },
            other => panic!("unknown base kind {other}"),
        };
        let steps = entry["moment"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                let text = |key: &str| s[key].as_str().unwrap().to_string();
                match s["kind"].as_str().unwrap() {
                    "shift" => match s.get("parameter").and_then(|p| p.as_str()) {
                        // The stand-ins: one day for a duration, the zero-day choice for a variant.
                        Some(id) if kind_of(id) == "duration" => parse_shift(&format!("{}1d", text("sign"))).unwrap(),
                        Some(_) => parse_shift("+0d").unwrap(),
                        None => parse_shift(&format!("{}{}{}", text("sign"), s["amount"], text("unit"))).unwrap(),
                    },
                    "snap" => Step::Snap(parse_snap(&text("target")).unwrap()),
                    "nearest" => Step::Nearest(parse_nearest(&text("target")).unwrap()),
                    "set_time" => parse_set_time(&text("time")).unwrap(),
                    "zone" => Step::Zone(parse_zone_to_bias(&text("offset")).unwrap()),
                    other => panic!("unknown step kind {other}"),
                }
            })
            .collect();
        MomentExpr { base, steps }
    }

    /// What the canonical form says is what the engine resolves - for every preset listed in the
    /// shipped catalogue and in the test one. Without this, the catalogue could hand the window a word
    /// that reads as another unit, target or zone, and every surface would agree on the wrong date.
    #[test]
    fn the_canonical_form_says_what_the_engine_resolves() {
        for dir in [shipped_catalogue(), PathBuf::from(TEST_CATALOGUE)] {
            let catalogue = read_catalogue(&dir).unwrap();
            assert!(!catalogue.presets.is_empty());
            for entry in &catalogue.presets {
                let json = serde_json::to_value(entry).unwrap();
                let preset = load_preset_at(&dir.join(&entry.file), &entry.file[..entry.file.len() - 5]).unwrap();
                let stand_ins: HashMap<String, ParamValue> =
                    preset.parameters.iter().map(|p| (p.id.clone(), stand_in(p.kind))).collect();
                let engine = resolve_moment(preset.moment.clone(), &stand_ins).unwrap();
                assert_eq!(moment_from_canonical(&json), engine, "{} in {}", entry.id, dir.display());
            }
        }
    }

    #[test]
    fn a_missing_folder_is_an_error_not_an_empty_catalogue() {
        let missing = crate::testutil::unique_temp_dir("chrono-no-presets");
        let err = read_catalogue(&missing).unwrap_err();
        assert!(err.contains("cannot read the preset folder"), "{err}");
    }

    #[test]
    fn the_text_names_the_folder_counts_and_every_file_left_out() {
        let catalogue = read_catalogue(Path::new(TEST_CATALOGUE)).unwrap();
        let text = render_catalogue(&catalogue);
        assert!(text.contains(&format!("presets: {} listed, {} left out", catalogue.presets.len(), catalogue.refused.len())), "{text}");
        assert!(text.contains("left out - `--preset` refuses these too:"), "{text}");
        for refused in &catalogue.refused {
            assert!(text.contains(&refused.file), "{} is not named:\n{text}", refused.file);
        }
    }

    #[test]
    fn presets_takes_a_folder_and_json_once_each() {
        use crate::calendar::CatalogueArgs;
        let parse = |list: &[&str]| parse_catalogue_args("presets", &words(list));
        assert_eq!(parse(&[]).unwrap(), CatalogueArgs { dir: None, json: false });
        assert_eq!(parse(&["--dir", "x", "--json"]).unwrap(), CatalogueArgs { dir: Some(PathBuf::from("x")), json: true });
        assert!(parse(&["--dir"]).is_err());
        assert!(parse(&["--dir", "--json"]).is_err(), "a flag is not a folder");
        assert!(parse(&["--json", "--json"]).is_err());
        assert!(parse(&["--dir", "a", "--dir", "b"]).is_err());
        assert_eq!(parse(&["--preset"]).unwrap_err(), "unknown argument '--preset' for presets");
    }

    mod properties {
        use super::*;
        use crate::preset::{duration_value, parse_param_date, parse_preset, variant_days};
        use crate::search::{drawn_preset, preset_flaw, with_preset_flaw, DrawnPreset, PresetFlaw};
        use proptest::prelude::*;

        /// The calendar each market counts in - restated, not read from `calendar_for_market`.
        fn market_calendar(market: Option<&str>) -> serde_json::Value {
            match market {
                Some("us") => "us-banking".into(),
                Some("pl") => "pl".into(),
                _ => serde_json::Value::Null,
            }
        }

        fn fail(e: &PresetError) -> TestCaseError {
            TestCaseError::fail(e.message().to_string())
        }

        /// What the catalogue hands on for a preset with no flaw says what the engine resolves: the
        /// moment rebuilt from its words through the step grammar alone, every default read back from
        /// its canonical form, the choices of a variant and nothing else, and the market's calendar.
        fn listed_as_resolved(preset: &DrawnPreset) -> Result<(), TestCaseError> {
            let text = preset.json().to_string();
            let read = parse_preset(&text).map_err(|e| fail(&e))?;
            let entry = canonical_entry(&read, "drawn.json".to_string()).map_err(|e| fail(&e))?;
            let json = serde_json::to_value(&entry).expect("an entry serialises");
            let stand_ins: HashMap<String, ParamValue> = read.parameters.iter().map(|p| (p.id.clone(), stand_in(p.kind))).collect();
            let engine = crate::preset::resolve_moment(read.moment.clone(), &stand_ins).map_err(|e| fail(&e))?;
            prop_assert_eq!(moment_from_canonical(&json), engine, "{}", text);
            prop_assert_eq!(&json["calendar"], &market_calendar(preset.market));
            for (listed, drawn) in json["parameters"].as_array().expect("a list").iter().zip(&preset.parameters) {
                let id = drawn.id.as_str();
                let default = &listed["default"];
                let read_back = match (drawn.kind, default) {
                    (_, serde_json::Value::Null) => None,
                    (ParamKind::Date, serde_json::Value::String(at)) => parse_param_date(at).ok().map(ParamValue::Date),
                    (ParamKind::Duration, d) => {
                        d["amount"].as_i64().zip(d["unit"].as_str()).and_then(|(amount, unit)| duration_value(id, amount, unit).ok())
                    }
                    (ParamKind::Variant, serde_json::Value::String(label)) => variant_days(id, label).ok().map(ParamValue::Variant),
                    (_, other) => return Err(TestCaseError::fail(format!("{id}: a default written as {other}"))),
                };
                let owed = drawn.default.as_ref().map(|d| d.value());
                prop_assert_eq!(format!("{read_back:?}"), format!("{owed:?}"), "{}: the default reads back as another value", id);
                prop_assert_eq!(listed["choices"].is_array(), drawn.kind == ParamKind::Variant, "{}", id);
            }
            Ok(())
        }

        fn case() -> impl Strategy<Value = (DrawnPreset, Option<PresetFlaw>)> {
            prop_oneof![
                2 => drawn_preset(false).prop_map(|preset| (preset, None)),
                3 => (drawn_preset(true), preset_flaw()).prop_map(|(preset, flaw)| (preset, Some(flaw))),
            ]
        }

        proptest! {
            /// R4-S22 and R4/18. A preset is listed exactly when `--preset` accepts it: one with no flaw
            /// is listed in a canonical form that says what the engine resolves, and one with a flaw is
            /// left out with the sentence and the exit code `--preset` gives for it - a step by its
            /// number - plus the one flaw only the evaluator would meet, a time past the day.
            #[test]
            fn the_catalogue_lists_a_preset_exactly_when_it_can_be_used((preset, flaw) in case()) {
                let Some(flaw) = flaw else { return listed_as_resolved(&preset) };
                let (json, owed) = with_preset_flaw(&preset, &flaw);
                let text = json.to_string();
                let listed = parse_preset(&text).and_then(|read| canonical_entry(&read, "drawn.json".to_string()).map(|_| ()));
                let Err(left_out) = listed else {
                    return Err(TestCaseError::fail(format!("{flaw:?} is listed: {text}")));
                };
                for piece in &owed.said {
                    prop_assert!(left_out.message().contains(piece.as_str()), "{:?}: {} - owed {:?}", flaw, left_out.message(), piece);
                }
                prop_assert_eq!(left_out.exit_code(), owed.exit_code, "{:?}: {}", flaw, left_out.message());
                if !owed.unused_until_evaluated {
                    let used = parse_preset(&text).and_then(|read| crate::preset::resolve_moment(read.moment, &owed.values).map(|_| ()));
                    let refused = used.err().map(|e| e.message().to_string());
                    prop_assert_eq!(Some(left_out.message()), refused.as_deref(), "{:?}: the catalogue and --preset say different things", flaw);
                }
            }
        }
    }
}
