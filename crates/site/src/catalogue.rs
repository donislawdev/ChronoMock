//! The scenarios and the holiday calendars of the documentation.
//!
//! The scenarios come from `site/data/presets.json`, the program's own answer to `chrono presets
//! --json`. Only the program reads preset files: it fills in what a file leaves to it, such as the
//! choices of a variant and the calendar of a market, so a second reader here would be a second
//! idea of the format. `crates/cli/tests/site_reference.rs` holds that file to what the program
//! reads today, and the build checks that it names the same presets as the folder.
//!
//! The calendars are read from `calendars/*.json` themselves, because the program's list of them
//! names each calendar and nothing more. A rule of a kind this file does not know stops the build
//! rather than leaving a holiday off the page.
//!
//! The words around both are in `site/data/catalogue.json`, in the languages of the documentation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

use crate::reference::inline;
use crate::{esc, json_files, parse_json, read_to_string, Result};

// ------------------------------------------------------------------- scenarios --

/// `site/data/presets.json`.
#[derive(Debug, Deserialize)]
pub struct PresetList {
    pub presets: Vec<Preset>,
}

/// One preset in the form `chrono presets --json` gives it.
#[derive(Debug, Deserialize)]
pub struct Preset {
    pub id: String,
    pub applies_to: String,
    pub calendar: Option<String>,
    pub name: BTreeMap<String, String>,
    pub explains: BTreeMap<String, String>,
    pub time_mode: Option<TimeMode>,
    pub parameters: Vec<Parameter>,
    pub moment: Moment,
}

#[derive(Debug, Deserialize)]
pub struct TimeMode {
    pub multiplier: u64,
    pub scale_duration_clock: bool,
}

#[derive(Debug, Deserialize)]
pub struct Parameter {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub label: BTreeMap<String, String>,
    pub default: Option<serde_json::Value>,
    pub default_hint: Option<String>,
    #[serde(default)]
    pub choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub label: String,
}

#[derive(Debug, Deserialize)]
pub struct Moment {
    pub base: Base,
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Base {
    Today,
    Now,
    Absolute { at: String },
    AbsoluteUtc { at: String },
    Parameter { parameter: String },
}

/// A step in the codes of the calculator's own options, which is the form the program lists.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    Shift {
        sign: Option<String>,
        amount: Option<i64>,
        unit: Option<String>,
        parameter: Option<String>,
    },
    Snap {
        target: String,
    },
    Nearest {
        target: String,
    },
    SetTime {
        time: String,
    },
    Zone {
        offset: String,
    },
}

// ------------------------------------------------------------------- calendars --

/// A calendar file, as much of it as the page shows.
#[derive(Debug, Deserialize)]
pub struct Calendar {
    pub id: String,
    pub name: Names,
    pub weekend: Vec<String>,
    pub observed: String,
    pub valid_from: i32,
    pub law_as_of: String,
    pub source: String,
    pub holidays: Vec<Holiday>,
}

#[derive(Debug, Deserialize)]
pub struct Names {
    pub en: String,
    pub local: String,
}

#[derive(Debug, Deserialize)]
pub struct Holiday {
    pub name: Names,
    pub rule: Rule,
    #[serde(default)]
    pub valid_from: Option<i32>,
    #[serde(default)]
    pub valid_to: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Rule {
    Fixed { month: usize, day: u32 },
    NthWeekday { month: usize, weekday: String, order: i32 },
    EasterOffset { offset: i32 },
}

// ----------------------------------------------------------------------- words --

/// `site/data/catalogue.json`.
#[derive(Debug, Deserialize)]
pub struct CatalogueWords {
    pub words: BTreeMap<String, Words>,
}

/// The words of both pages in one language. A text may hold `code` between backticks and a
/// `{name}` where a value goes.
#[derive(Debug, Deserialize)]
pub struct Words {
    pub link: String,
    pub name_to_type: String,
    pub in_window: String,
    pub on_command_line: String,
    pub asks_for: String,
    pub computed_like: String,
    pub business_days: String,
    pub speed: String,
    pub window: WindowWords,
    pub date_value: String,
    pub duration_value: String,
    pub variant_value: String,
    pub when_left_out: String,
    pub from_file: String,
    pub counted_in: String,
    pub years: String,
    pub from_year: String,
    pub checked: String,
    pub weekend: String,
    pub on_weekend: String,
    pub sources: String,
    pub holiday: String,
    pub date: String,
    pub every_year: String,
    pub since: String,
    pub until: String,
    pub only: String,
    pub between: String,
    pub and: String,
    pub months: Vec<String>,
    pub weekdays: BTreeMap<String, Weekday>,
    pub ordinals: BTreeMap<String, BTreeMap<String, String>>,
    pub fixed: String,
    pub nth_weekday: String,
    pub easter: String,
    pub after_easter: Plural,
    pub before_easter: Plural,
    pub long_date: String,
    pub observed: BTreeMap<String, String>,
}

/// Where in the window a scenario is offered, by what it is for and whether it asks for a value.
#[derive(Debug, Deserialize)]
pub struct WindowWords {
    pub both: String,
    pub session: String,
    pub calculator: String,
    pub calculator_first: String,
    pub none: String,
}

/// A day of the week, with the form its ordinal takes. In Polish the word for "third" agrees with
/// the day it counts, so Monday and Wednesday take two different words for it.
#[derive(Debug, Deserialize)]
pub struct Weekday {
    pub name: String,
    pub form: String,
}

#[derive(Debug, Deserialize)]
pub struct Plural {
    pub one: String,
    pub other: String,
}

// -------------------------------------------------------------------- loading --

pub struct Catalogue {
    pub words: BTreeMap<String, Words>,
    pub presets: Vec<Preset>,
    pub calendars: Vec<Calendar>,
    pub preset_example: String,
    pub holiday_example: String,
}

/// The page the calendars are listed on, which a scenario that counts business days links to.
pub const CALENDARS_PAGE: &str = "holiday-calendars";

/// The example preset the scenarios page shows. `crates/cli/tests/site_reference.rs` checks that
/// the program takes every file of its folder.
pub const EXAMPLE_PRESET: &str = "site/examples/presets/invoice-overdue.json";

/// The example holiday the calendars page shows. The same test adds it to a copy of `pl.json`, the
/// way the page tells the reader to, and checks that the program takes that calendar.
pub const EXAMPLE_HOLIDAY: &str = "site/examples/calendars/holiday.json";

/// Read the words, the program's list of presets and the calendar files.
pub fn load(root: &Path) -> Result<Catalogue> {
    let data = root.join("site").join("data");
    let words: CatalogueWords = parse_json(&data.join("catalogue.json"))?;
    let list: PresetList = parse_json(&data.join("presets.json"))?;
    check_presets(&list.presets, &stems(&root.join("presets"))?)?;
    let mut calendars = Vec::new();
    for path in json_files(&root.join("calendars"))? {
        calendars.push(parse_json::<Calendar>(&path)?);
    }
    Ok(Catalogue {
        words: words.words,
        presets: list.presets,
        calendars,
        preset_example: read_to_string(&root.join(EXAMPLE_PRESET))?,
        holiday_example: read_to_string(&root.join(EXAMPLE_HOLIDAY))?,
    })
}

fn stems(dir: &Path) -> Result<BTreeSet<String>> {
    Ok(json_files(dir)?
        .iter()
        .filter_map(|path| path.file_stem().and_then(|s| s.to_str()).map(str::to_lowercase))
        .collect())
}

/// The program's list must name the presets the folder holds: a preset added, renamed or removed
/// without writing the list again would otherwise be missing from the page, or still on it.
fn check_presets(presets: &[Preset], files: &BTreeSet<String>) -> Result<()> {
    let listed: BTreeSet<String> = presets.iter().map(|p| p.id.to_lowercase()).collect();
    if &listed == files {
        return Ok(());
    }
    let missing: Vec<&String> = files.difference(&listed).collect();
    let extra: Vec<&String> = listed.difference(files).collect();
    Err(format!(
        "site/data/presets.json is not the list of presets/: not listed {missing:?}, listed without a file {extra:?}. \
         Write it again: set CHRONO_WRITE_SITE_DATA=1 and run `cargo test -p chrono-cli --test site_reference`"
    ))
}

// ------------------------------------------------------------------- rendering --

/// Put the values into the places a text names. Every place must get a value, and a value is not
/// read again for places, so a name that holds braces stays as it is.
fn fill(template: &str, values: &[(&str, &str)], whence: &str) -> Result<String> {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..]
            .find('}')
            .map(|at| open + at)
            .ok_or_else(|| format!("{whence}: a '{{' is never closed in \"{template}\""))?;
        let key = &rest[open + 1..close];
        let value = values
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
            .ok_or_else(|| format!("{whence}: \"{template}\" has a place for {{{key}}}, and nothing fills it"))?;
        out.push_str(value);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A text of the words with its code marked and its places filled. The values are markup already.
fn say(template: &str, values: &[(&str, &str)], whence: &str) -> Result<String> {
    fill(&inline(template, whence)?, values, whence)
}

/// The text in a language, the English one when that language has none, the way the window does.
fn in_language<'t>(text: &'t BTreeMap<String, String>, lang: &str, fallback: &'t str) -> &'t str {
    text.get(lang)
        .or_else(|| text.get("en"))
        .map(String::as_str)
        .unwrap_or(fallback)
}

fn fact(h: &mut String, label: &str, value: &str) {
    h.push_str(&format!("<dt>{}</dt><dd>{value}</dd>\n", esc(label)));
}

fn heading(h: &mut String, anchor: &str, name: &str, alternative: Option<&str>, words: &Words) {
    h.push_str(&format!(
        "<section class=\"entry\" id=\"{anchor}\">\n<h3>{}<a class=\"anchor\" href=\"#{anchor}\" aria-label=\"{} {}\">#</a>",
        esc(name),
        esc(&words.link),
        esc(name)
    ));
    if let Some(other) = alternative {
        h.push_str(&format!("<span class=\"alt\">{}</span>", esc(other)));
    }
    h.push_str("</h3>\n");
}

/// Where the window offers a scenario. The main window lists the ones it can turn into a date
/// with one click, and the calculator the ones it can compute, with a field for each value.
fn window_words<'w>(preset: &Preset, words: &'w Words) -> Result<&'w str> {
    let (run, calc) = match preset.applies_to.as_str() {
        "substitution" => (true, false),
        "calculator" => (false, true),
        "both" => (true, true),
        other => return Err(format!("site/data/presets.json: {} is for '{other}'", preset.id)),
    };
    let one_click = run && preset.parameters.is_empty();
    Ok(match (one_click, calc, run) {
        (true, true, _) => &words.window.both,
        (true, false, _) => &words.window.session,
        (false, true, true) => &words.window.calculator_first,
        (false, true, false) => &words.window.calculator,
        (false, false, _) => &words.window.none,
    })
}

fn commands(preset: &Preset) -> String {
    let mut lines = Vec::new();
    for (command, used) in [
        ("run", preset.applies_to != "calculator"),
        ("calc", preset.applies_to != "substitution"),
    ] {
        if used {
            // A long name makes the command wider than a phone, so it may break at its spaces.
            lines.push(format!("<code class=\"long\">chrono {command} --preset {}</code>", esc(&preset.id)));
        }
    }
    lines.join("<br>")
}

/// A default value as `--param` takes it: a date, a variant, or a length such as `30d`.
fn default_text(value: &serde_json::Value, whence: &str) -> Result<String> {
    match value {
        serde_json::Value::String(text) => Ok(text.clone()),
        serde_json::Value::Object(length) => match (length.get("amount"), length.get("unit")) {
            (Some(amount), Some(serde_json::Value::String(unit))) => Ok(format!("{amount}{unit}")),
            _ => Err(format!("{whence}: a default of an unknown shape: {value}")),
        },
        other => Err(format!("{whence}: a default of an unknown shape: {other}")),
    }
}

fn parameter(p: &Parameter, lang: &str, words: &Words, whence: &str) -> Result<String> {
    let whence = format!("{whence}, parameter {}", p.id);
    let kind = match p.kind.as_str() {
        "date" => say(&words.date_value, &[], &whence)?,
        "duration" => say(&words.duration_value, &[], &whence)?,
        "variant" => {
            let choices: Vec<String> = p.choices.iter().map(|c| format!("<code>{}</code>", esc(&c.label))).collect();
            say(&words.variant_value, &[("choices", &choices.join(", "))], &whence)?
        }
        other => return Err(format!("{whence}: a parameter of type '{other}'")),
    };
    let mut parts = vec![kind];
    if let Some(value) = p.default.as_ref().filter(|v| !v.is_null()) {
        parts.push(say(&words.when_left_out, &[("value", &esc(&default_text(value, &whence)?))], &whence)?);
    }
    match p.default_hint.as_deref() {
        None => {}
        Some("target_file_creation") => parts.push(say(&words.from_file, &[], &whence)?),
        Some(other) => return Err(format!("{whence}: a default hint '{other}'")),
    }
    Ok(format!(
        "<li>{} <code>{}</code>: {}.</li>",
        esc(in_language(&p.label, lang, &p.id)),
        esc(&p.id),
        parts.join(". ")
    ))
}

/// The moment as the calculator's options would write it, a value to give in angle brackets.
/// Each option is its own `code`, so a long one breaks between options on a phone.
fn computed(preset: &Preset) -> String {
    let mut parts = vec![match &preset.moment.base {
        Base::Today => "--base today".to_string(),
        Base::Now => "--base now".to_string(),
        Base::Absolute { at } => format!("--base {at}"),
        Base::AbsoluteUtc { at } => format!("--base-utc {at}"),
        Base::Parameter { parameter } => format!("--base <{parameter}>"),
    }];
    for step in &preset.moment.steps {
        parts.push(match step {
            Step::Shift { sign, amount: Some(amount), unit: Some(unit), .. } => {
                format!("--shift {}{amount}{unit}", sign.as_deref().unwrap_or("+"))
            }
            Step::Shift { sign, parameter, .. } => format!(
                "--shift {}<{}>",
                sign.as_deref().unwrap_or_default(),
                parameter.as_deref().unwrap_or_default()
            ),
            Step::Snap { target } => format!("--snap {target}"),
            Step::Nearest { target } => format!("--nearest {target}"),
            Step::SetTime { time } => format!("--set-time {time}"),
            Step::Zone { offset } => format!("--to-zone {offset}"),
        });
    }
    let codes: Vec<String> = parts.iter().map(|p| format!("<code>{}</code>", esc(p))).collect();
    codes.join(" ")
}

/// The speed `chrono run --preset` starts at, when it is not the real one.
fn speed(preset: &Preset) -> Option<String> {
    let mode = preset.time_mode.as_ref()?;
    if mode.multiplier == 1 && !mode.scale_duration_clock {
        return None;
    }
    let mut h = format!("<code>--mode x{}</code>", mode.multiplier);
    if mode.scale_duration_clock {
        h.push_str(" <code>--scale-duration</code>");
    }
    Some(h)
}

fn scenario(preset: &Preset, lang: &str, words: &Words, calendars_page: &str) -> Result<String> {
    let whence = format!("site/data/presets.json: {}", preset.id);
    let anchor = format!("scenario-{}", preset.id);
    let mut h = String::new();
    heading(&mut h, &anchor, in_language(&preset.name, lang, &preset.id), None, words);
    h.push_str(&format!(
        "<p>{}</p>\n<dl class=\"facts\">\n",
        esc(in_language(&preset.explains, lang, ""))
    ));
    fact(&mut h, &words.name_to_type, &format!("<code>{}</code>", esc(&preset.id)));
    fact(&mut h, &words.in_window, &say(window_words(preset, words)?, &[], &whence)?);
    fact(&mut h, &words.on_command_line, &commands(preset));
    if !preset.parameters.is_empty() {
        let mut list = String::from("<ul>");
        for p in &preset.parameters {
            list.push_str(&parameter(p, lang, words, &whence)?);
        }
        list.push_str("</ul>");
        fact(&mut h, &words.asks_for, &list);
    }
    fact(&mut h, &words.computed_like, &computed(preset));
    if let Some(calendar) = &preset.calendar {
        let link = format!(
            "<a href=\"{calendars_page}#calendar-{0}\"><code>{0}</code></a>",
            esc(calendar)
        );
        fact(&mut h, &words.business_days, &say(&words.counted_in, &[("calendar", &link)], &whence)?);
    }
    if let Some(value) = speed(preset) {
        fact(&mut h, &words.speed, &value);
    }
    h.push_str("</dl>\n</section>\n");
    Ok(h)
}

// ------------------------------------------------------------- calendar words --

fn month(words: &Words, number: usize, whence: &str) -> Result<String> {
    number
        .checked_sub(1)
        .and_then(|i| words.months.get(i))
        .map(|m| esc(m))
        .ok_or_else(|| format!("{whence}: month {number}"))
}

fn weekday<'w>(words: &'w Words, name: &str, whence: &str) -> Result<&'w Weekday> {
    words
        .weekdays
        .get(name)
        .ok_or_else(|| format!("{whence}: no words for the day '{name}'"))
}

/// The date a holiday falls on, in words: "1 January", "third Monday of January", "49 days after
/// Easter Sunday".
pub fn rule_text(rule: &Rule, words: &Words, whence: &str) -> Result<String> {
    match rule {
        Rule::Fixed { month: m, day } => say(
            &words.fixed,
            &[("day", &day.to_string()), ("month", &month(words, *m, whence)?)],
            whence,
        ),
        Rule::NthWeekday { month: m, weekday: day, order } => {
            let day = weekday(words, day, whence)?;
            let nth = words
                .ordinals
                .get(&day.form)
                .and_then(|forms| forms.get(&order.to_string()))
                .ok_or_else(|| format!("{whence}: no ordinal {order} of the form '{}'", day.form))?;
            say(
                &words.nth_weekday,
                &[("nth", &esc(nth)), ("weekday", &esc(&day.name)), ("month", &month(words, *m, whence)?)],
                whence,
            )
        }
        Rule::EasterOffset { offset: 0 } => say(&words.easter, &[], whence),
        Rule::EasterOffset { offset } => {
            let plural = if *offset > 0 { &words.after_easter } else { &words.before_easter };
            let n = offset.unsigned_abs();
            let template = if n == 1 { &plural.one } else { &plural.other };
            say(template, &[("n", &n.to_string())], whence)
        }
    }
}

/// The years a holiday is kept, within the years the calendar covers: a law older than the
/// calendar's first year is no news to the reader.
pub fn years_text(holiday: &Holiday, first_year: i32, words: &Words, whence: &str) -> Result<String> {
    let from = holiday.valid_from.filter(|year| *year > first_year);
    match (from, holiday.valid_to) {
        (None, None) => say(&words.every_year, &[], whence),
        (Some(year), None) => say(&words.since, &[("year", &year.to_string())], whence),
        (None, Some(year)) => say(&words.until, &[("year", &year.to_string())], whence),
        (Some(a), Some(b)) if a == b => say(&words.only, &[("year", &a.to_string())], whence),
        (Some(a), Some(b)) => say(&words.between, &[("from", &a.to_string()), ("to", &b.to_string())], whence),
    }
}

fn long_date(text: &str, words: &Words, whence: &str) -> Result<String> {
    let parts: Vec<&str> = text.split('-').collect();
    let number = |i: usize| parts.get(i).and_then(|p| p.parse::<usize>().ok());
    match (parts.len(), number(0), number(1), number(2)) {
        (3, Some(year), Some(m), Some(day)) => say(
            &words.long_date,
            &[("day", &day.to_string()), ("month", &month(words, m, whence)?), ("year", &year.to_string())],
            whence,
        ),
        _ => Err(format!("{whence}: '{text}' is not a date written YYYY-MM-DD")),
    }
}

/// The name in the reader's language first: the local name on a page in another language than
/// English, the English one on the English page, and the other one beside it when they differ.
fn names<'n>(names: &'n Names, lang: &str) -> (&'n str, Option<&'n str>) {
    let (first, second) = if lang == "en" {
        (&names.en, &names.local)
    } else {
        (&names.local, &names.en)
    };
    (first, (second != first).then_some(second.as_str()))
}

fn weekend_text(calendar: &Calendar, words: &Words, whence: &str) -> Result<String> {
    let days: Vec<String> = calendar
        .weekend
        .iter()
        .map(|day| weekday(words, day, whence).map(|d| esc(&d.name)))
        .collect::<Result<_>>()?;
    Ok(match days.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} {} {last}", rest.join(", "), esc(&words.and)),
        _ => days.join(""),
    })
}

/// A cell or a value of its own starts with a capital, while the words stay usable inside a
/// sentence: "third Monday of January" is also "the third Monday of January".
fn capital(text: String) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

fn calendar_entry(calendar: &Calendar, lang: &str, words: &Words) -> Result<String> {
    let whence = format!("calendars/{}.json", calendar.id);
    let anchor = format!("calendar-{}", calendar.id);
    let (name, other) = names(&calendar.name, lang);
    let observed = words
        .observed
        .get(&calendar.observed)
        .ok_or_else(|| format!("{whence}: no words for observed '{}'", calendar.observed))?;
    let mut h = String::new();
    heading(&mut h, &anchor, name, other, words);
    h.push_str("<dl class=\"facts\">\n");
    fact(&mut h, &words.name_to_type, &format!("<code>{}</code>", esc(&calendar.id)));
    fact(
        &mut h,
        &words.years,
        &capital(say(&words.from_year, &[("year", &calendar.valid_from.to_string())], &whence)?),
    );
    fact(&mut h, &words.checked, &long_date(&calendar.law_as_of, words, &whence)?);
    fact(&mut h, &words.weekend, &capital(weekend_text(calendar, words, &whence)?));
    fact(&mut h, &words.on_weekend, &say(observed, &[], &whence)?);
    h.push_str("</dl>\n");
    h.push_str(&format!(
        "<div class=\"wrap\"><table>\n<thead><tr><th>{}</th><th>{}</th><th>{}</th></tr></thead>\n<tbody>\n",
        esc(&words.holiday),
        esc(&words.date),
        esc(&words.years)
    ));
    for holiday in &calendar.holidays {
        let (name, other) = names(&holiday.name, lang);
        let other = other.map(|o| format!("<span class=\"alt\">{}</span>", esc(o))).unwrap_or_default();
        h.push_str(&format!(
            "<tr><td>{}{other}</td><td>{}</td><td>{}</td></tr>\n",
            esc(name),
            capital(rule_text(&holiday.rule, words, &whence)?),
            capital(years_text(holiday, calendar.valid_from, words, &whence)?)
        ));
    }
    h.push_str("</tbody>\n</table></div>\n");
    // The sources are quoted as the file has them, in English: on a page in another language the
    // reader's tools are told so.
    let lang_attr = if lang == "en" { "" } else { " lang=\"en\"" };
    h.push_str(&format!(
        "<p class=\"src\"><strong>{}</strong> <span{lang_attr}>{}</span></p>\n</section>\n",
        esc(&words.sources),
        esc(&calendar.source)
    ));
    Ok(h)
}

// ---------------------------------------------------------------------- tokens --

/// The tokens of both pages in one language: `scenario_list`, `calendar_list`, `preset_example`
/// and `holiday_example`. A language without words in `catalogue.json` gets none, so a page in it that
/// uses one fails on an unknown token. `calendars_page` is the address of the calendars page in
/// that language, which a scenario that counts business days links to.
pub fn tokens(catalogue: &Catalogue, lang: &str, calendars_page: Option<&str>) -> Result<BTreeMap<String, String>> {
    let mut t = BTreeMap::new();
    let Some(words) = catalogue.words.get(lang) else {
        return Ok(t);
    };
    let calendars_page = calendars_page
        .ok_or_else(|| format!("the page '{CALENDARS_PAGE}' has no '{lang}' version, and the scenarios link to it"))?;
    let mut scenarios = String::from("<div class=\"entries\">\n");
    for preset in &catalogue.presets {
        scenarios.push_str(&scenario(preset, lang, words, calendars_page)?);
    }
    scenarios.push_str("</div>");
    t.insert("scenario_list".into(), scenarios);

    let mut calendars = String::from("<div class=\"entries\">\n");
    for calendar in &catalogue.calendars {
        calendars.push_str(&calendar_entry(calendar, lang, words)?);
    }
    calendars.push_str("</div>");
    t.insert("calendar_list".into(), calendars);

    for (token, text) in [
        ("preset_example", &catalogue.preset_example),
        ("holiday_example", &catalogue.holiday_example),
    ] {
        t.insert(token.into(), format!("<pre class=\"term\">{}</pre>", esc(text.trim_end())));
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(lang: &str) -> Words {
        let all: CatalogueWords =
            parse_json(&crate::repo_root().join("site/data/catalogue.json")).expect("catalogue.json");
        all.words.into_iter().find(|(l, _)| l == lang).expect("language").1
    }

    #[test]
    fn a_value_goes_where_its_place_is_and_is_not_read_again() {
        let got = fill("from {year} to {to}", &[("year", "{to}"), ("to", "2020")], "t").expect("fill");
        assert_eq!(got, "from {to} to 2020");
    }

    #[test]
    fn a_place_nothing_fills_stops_the_build() {
        let err = fill("since {yaer}", &[("year", "2011")], "words.since").expect_err("must refuse");
        assert!(err.contains("{yaer}") && err.contains("words.since"), "{err}");
    }

    #[test]
    fn a_holiday_is_dated_in_words_with_the_ordinal_agreeing_in_polish() {
        let pl = words("pl");
        let rule = |json: &str| serde_json::from_str::<Rule>(json).expect("rule");
        let text = |json: &str| rule_text(&rule(json), &pl, "t").expect("text");
        assert_eq!(text(r#"{"type":"fixed","month":5,"day":3}"#), "3 maja");
        assert_eq!(
            text(r#"{"type":"nth_weekday","month":1,"weekday":"monday","order":3}"#),
            "trzeci poniedziałek stycznia"
        );
        assert_eq!(
            text(r#"{"type":"nth_weekday","month":11,"weekday":"wednesday","order":-1}"#),
            "ostatnia środa listopada"
        );
        assert_eq!(text(r#"{"type":"easter_offset","offset":1}"#), "dzień po Wielkanocy");
        assert_eq!(text(r#"{"type":"easter_offset","offset":-2}"#), "2 dni przed Wielkanocą");
        let en = words("en");
        assert_eq!(
            rule_text(&rule(r#"{"type":"nth_weekday","month":5,"weekday":"monday","order":-1}"#), &en, "t").expect("en"),
            "last Monday of May"
        );
    }

    #[test]
    fn a_rule_of_an_unknown_kind_is_an_error_not_a_missing_row() {
        assert!(serde_json::from_str::<Rule>(r#"{"type":"lunar","month":1}"#).is_err());
    }

    #[test]
    fn the_years_of_a_holiday_are_said_within_the_years_of_its_calendar() {
        let en = words("en");
        let years = |from: Option<i32>, to: Option<i32>| {
            let holiday = Holiday {
                name: Names { en: "h".into(), local: "h".into() },
                rule: Rule::EasterOffset { offset: 0 },
                valid_from: from,
                valid_to: to,
            };
            years_text(&holiday, 2002, &en, "t").expect("years")
        };
        assert_eq!(years(None, None), "every year");
        assert_eq!(years(Some(1990), None), "every year", "a law older than the calendar is not news");
        assert_eq!(years(Some(2011), None), "since 2011");
        assert_eq!(years(None, Some(1970)), "until 1970");
        assert_eq!(years(Some(2018), Some(2018)), "only in 2018");
        assert_eq!(years(Some(2005), Some(2007)), "2005 to 2007");
    }

    #[test]
    fn the_program_list_must_name_the_presets_of_the_folder() {
        let preset = |id: &str| -> Preset {
            serde_json::from_str(&format!(
                r#"{{"id":"{id}","applies_to":"both","calendar":null,"name":{{"en":"n"}},"explains":{{"en":"e"}},
                   "time_mode":null,"parameters":[],"moment":{{"base":{{"kind":"today"}},"steps":[]}}}}"#
            ))
            .expect("preset")
        };
        let files = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        check_presets(&[preset("month-end")], &files(&["month-end"])).expect("the same presets");
        let err = check_presets(&[preset("month-end")], &files(&["month-end", "feb-29"])).expect_err("one more file");
        assert!(err.contains("feb-29") && err.contains("CHRONO_WRITE_SITE_DATA"), "{err}");
    }

    #[test]
    fn a_scenario_is_offered_where_the_window_can_use_it() {
        let en = words("en");
        let preset = |applies_to: &str, parameters: &str| -> Preset {
            serde_json::from_str(&format!(
                r#"{{"id":"p","applies_to":"{applies_to}","calendar":null,"name":{{"en":"n"}},"explains":{{"en":"e"}},
                   "time_mode":null,"parameters":[{parameters}],"moment":{{"base":{{"kind":"today"}},"steps":[]}}}}"#
            ))
            .expect("preset")
        };
        let date = r#"{"id":"d","type":"date","label":{},"default":null,"default_hint":null}"#;
        let window = |applies_to: &str, parameters: &str| window_words(&preset(applies_to, parameters), &en).expect("words");
        assert_eq!(window("both", ""), en.window.both);
        assert_eq!(window("substitution", ""), en.window.session);
        assert_eq!(window("calculator", date), en.window.calculator);
        assert_eq!(window("both", date), en.window.calculator_first);
        assert_eq!(window("substitution", date), en.window.none);
        assert!(window_words(&preset("everything", ""), &en).is_err());
    }
}
