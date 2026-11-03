//! Reading a shipped calendar catalogue off disk and turning it into the core's types.
//!
//! The wire shape (`CalendarDto` and friends) is separate from the engine types on purpose: a
//! catalogue is user-facing data that must be validated before the engine ever sees it, and a bad
//! rule has to name its own field rather than fail somewhere downstream.
//!
//! The catalogue lookup lives here too, including the identifier check that refuses path traversal -
//! an id names a shipped file, never a path.
//!
//! The consumer owns the I/O and serde - the core engine works over already-parsed rules. The JSON
//! schema is the contract (docs/04 section 5) and this is one reader of it. An unknown major schema
//! version is refused, and so is an unknown FIELD, at every level of the file (R4-N38, the owner's
//! decision R4-D11). A calendar is the catalogue people outside this project write by hand, and a
//! typo in an optional field - `valid_form` for `valid_from` - used to be dropped without a word, so
//! the holiday it was meant to limit counted in every year. Presets keep ignoring fields they do not
//! read, because a preset carries fields for the window that this reader has no use for.


use serde::Deserialize;

use chrono_core::calendar::{check_day, check_easter_offset, check_month, check_order, RuleError, EASTER_OFFSET_RANGE};

/// The schema field on its own, read before the rest of the file. A file written to a later version
/// has to be refused as that - read strictly first, it would be refused over whichever of its new
/// fields came first, and the reader would chase a typo that is not there.
#[derive(Deserialize)]
#[serde(expecting = "a calendar - an object with schema, id, country, weekend, observed, valid_from, law_as_of, source and holidays")]
struct SchemaDto {
    schema: String,
}

// 🔴 EVERY DTO HERE AND IN preset.rs NAMES WHAT IT EXPECTS IN THE FILE'S OWN WORDS (`expecting`), or
// serde names the Rust type instead - "expected struct NameDto", in front of somebody who wrote JSON.
// A test reads both files and refuses a derive without it. The rest of serde's words: `json_refusal`.
#[derive(Deserialize)]
#[serde(
    deny_unknown_fields,
    expecting = "a calendar - an object with schema, id, country, weekend, observed, valid_from, law_as_of, source and holidays"
)]
pub(crate) struct CalendarDto {
    // Read by `SchemaDto` before this struct is. Named here so the field itself is not unknown.
    #[serde(rename = "schema")]
    _schema: String,
    // Contract fields this reader has no use for (docs/04 section 5), named so that refusing unknown
    // fields does not refuse the shipped calendars: `stability` marks the format as not yet frozen,
    // `subregion` waits for states and regions, and `name` is the calendar's own title.
    #[serde(default, rename = "stability")]
    _stability: Option<String>,
    #[serde(default, rename = "subregion")]
    _subregion: Option<String>,
    #[serde(default, rename = "name")]
    _name: Option<NameDto>,
    id: String,
    country: String,
    weekend: Vec<String>,
    observed: String,
    // The years the calendar answers for and the day its holidays were checked against the law, both
    // required (R4-S20, docs/04 section 5): a file without them would answer for every year the
    // calculator computes, which is how Poland's Constitution Day came out a day off in 1985.
    valid_from: i64,
    law_as_of: String,
    // Where the range, the weekend and the observance come from. Each holiday carries its own.
    #[serde(rename = "source")]
    _source: String,
    holidays: Vec<HolidayDto>,
}

/// The earliest `valid_from` a calendar may declare: the first full year of the Gregorian calendar.
/// Every rule here is Gregorian - Easter by Meeus included - so a year before it would be answered
/// with a calendar nobody kept.
pub(crate) const FIRST_GREGORIAN_YEAR: i64 = 1583;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, expecting = "a holiday - an object with id, name, rule and source")]
pub(crate) struct HolidayDto {
    id: String,
    name: NameDto,
    rule: RuleDto,
    #[serde(default)]
    valid_from: Option<i64>,
    #[serde(default)]
    valid_to: Option<i64>,
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, expecting = "a name - an object with en and local")]
pub(crate) struct NameDto {
    en: String,
    local: String,
}

// `deny_unknown_fields` on an internally tagged enum was measured with this build's serde before it
// was relied on: a stray field in any rule is refused, the `type` tag itself is not, and it does not
// matter where in the object the tag stands.
#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    deny_unknown_fields,
    expecting = "a rule - an object with type (fixed, nth_weekday or easter_offset) and its fields"
)]
pub(crate) enum RuleDto {
    Fixed { month: u32, day: u32 },
    NthWeekday { month: u32, weekday: String, order: i32 },
    EasterOffset { offset: i32 },
}

/// Map a weekday name to a Sunday-based index 0..=6.
pub(crate) fn weekday_index(name: &str) -> Result<u32, String> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "sunday" => 0,
        "monday" => 1,
        "tuesday" => 2,
        "wednesday" => 3,
        "thursday" => 4,
        "friday" => 5,
        "saturday" => 6,
        other => return Err(format!("unknown weekday '{other}'")),
    })
}

pub(crate) fn observed_from(s: &str) -> Result<chrono_core::calendar::Observed, String> {
    use chrono_core::calendar::Observed;
    Ok(match s {
        "none" => Observed::None,
        "sat_to_fri_sun_to_mon" => Observed::SatToFriSunToMon,
        "sun_to_mon" => Observed::SunToMon,
        "weekend_to_mon" => Observed::WeekendToMon,
        other => return Err(format!("unknown observed rule '{other}'")),
    })
}

/// Validate and map one holiday rule. The engine trusts its inputs, and a calendar is a data file from
/// outside the build - the one documented extension point of this tool - so an out-of-range field must be
/// refused HERE, naming the field. Left unchecked it does not fail, which is worse: `days_from_civil`
/// happily rolls month 13 into the next January and day 40 into the following month, so the calendar
/// silently marks the wrong dates as holidays and every business-day answer built on it is quietly wrong.
pub(crate) fn rule_from(id: &str, dto: RuleDto) -> Result<chrono_core::calendar::HolidayRule, String> {
    use chrono_core::calendar::HolidayRule;
    Ok(match dto {
        RuleDto::Fixed { month, day } => {
            // The month's longest length in ANY year, not a flat 1..=31. Two different things look
            // alike here and only one of them is legal. February 29 is a rule a calendar may
            // legitimately name - the engine answers "not this year" in a common year (R2-N7) - so
            // the bound has to let it through. April 31 is a day that exists in no year at all, so
            // it is a typo, and the engine would answer "not this year" for EVERY year: a holiday
            // silently absent forever. Refused here, naming the month's real length, because a
            // calendar is a data file from outside the build.
            check_day(month, day).map_err(|e| rule_refusal(id, e))?;
            HolidayRule::Fixed { month, day }
        }
        RuleDto::NthWeekday { month, weekday, order } => {
            check_month(month).map_err(|e| rule_refusal(id, e))?;
            // -1 = "the last such weekday in the month" - 1..=5 counts from the start. A fifth exists
            // only in some months, and the engine now answers "this holiday does not fall in that
            // year" rather than borrowing a day from the next month - which is what it actually did
            // while this comment claimed otherwise (R2-N4). Anything outside the range would walk
            // past the end for every month, so it stays a load error.
            check_order(order).map_err(|e| rule_refusal(id, e))?;
            HolidayRule::NthWeekday { month, weekday: weekday_index(&weekday)?, order }
        }
        RuleDto::EasterOffset { offset } => {
            // Kept inside the year of its own Easter (R4-N39, the owner's decision R4-D12). Easter
            // falls between 22 March, day 81 of a common year, and 25 April, day 115 (116 in a leap
            // year), so -80 is 1 January at the earliest and +250 is 31 December at the latest, in
            // every year. The bound used to be a year either side, and a holiday pushed into the
            // neighbouring year was a day the engine described two ways at once: `holiday_on` looks
            // for it in the date's own year and finds nothing, while the day-off cache looks a year
            // either side and finds it, so a report called one day "not a holiday" and "a holiday
            // shifted here from a weekend". Real observances sit well inside (Corpus Christi is +60).
            check_easter_offset(offset).map_err(|e| rule_refusal(id, e))?;
            HolidayRule::EasterOffset { offset }
        }
    })
}

/// The loader's sentence for a rule field outside the dates a year has, naming the holiday and the field.
/// The bounds are the engine's (`chrono_core::calendar::RuleError`, R5-A11), the words are the loader's.
fn rule_refusal(id: &str, e: RuleError) -> String {
    match e {
        RuleError::Month(month) => format!("holiday '{id}': month {month} out of range (1..=12)"),
        RuleError::Day { month, day, longest } => {
            format!("holiday '{id}': day {day} out of range for month {month} (1..={longest})")
        }
        RuleError::Order(order) => format!("holiday '{id}': order {order} out of range (-1 for last, or 1..=5)"),
        // A file names its weekday as a word, read by `weekday_index`, so no file reaches this one.
        RuleError::Weekday(weekday) => format!("holiday '{id}': weekday {weekday} out of range (0..=6)"),
        RuleError::EasterOffset(offset) => format!(
            "holiday '{id}': easter offset {offset} out of range ({}..={} days, which keeps the holiday in the year of its Easter)",
            EASTER_OFFSET_RANGE.start(),
            EASTER_OFFSET_RANGE.end()
        ),
    }
}

/// Locate a calendar file: next to the executable (portable layout), else in ./calendars.
/// Whether a catalogue id (calendar / preset) is safe to turn into a file name: non-empty and made
/// only of letters, digits, '-' and '_'. This rejects any path separator, '..', drive letter, or
/// ADS colon BEFORE the id becomes a path, so `--preset ../../secret` cannot read a file outside the
/// catalogue directory (docs/04 4.1 - a shared catalogue entry can never smuggle a path).
pub(crate) fn is_valid_catalogue_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Locate `<kind>/<id>.json` - the shared lookup behind calendars and presets.
///
/// Next to the executable first, then the working directory. The second is what makes a dev
/// checkout work: `chrono.exe` is built into `target/<triple>/release/`, which has no `calendars/`
/// beside it, so every `cargo run -- calc --calendar` and all 133 harness scenarios resolve through
/// the working directory. It cannot simply be dropped.
///
/// What it must NOT do is rescue an INSTALLED layout. If the folder beside the executable exists but
/// does not hold the file, the answer is "missing", not "here is one from wherever you happened to
/// be standing" - otherwise `chrono calc --calendar us-banking`, run from a directory someone else
/// can write to, silently answers business-day questions from THEIR holidays, in a report a tester
/// then quotes as evidence (untouchable rule 4). Directories are taken as given so all three cases
/// are testable without touching the process's real working directory.
pub(crate) fn find_catalogue_in(exe_dir: Option<&std::path::Path>, cwd: &std::path::Path, kind: &str, id: &str) -> Option<std::path::PathBuf> {
    let name = format!("{id}.json");
    if let Some(installed) = exe_dir.and_then(|dir| installed_catalogue_dir(dir, kind)) {
        // An installed catalogue answers for itself, including "not here".
        let candidate = installed.join(&name);
        return candidate.is_file().then_some(candidate);
    }
    let local = cwd.join(kind).join(&name);
    local.is_file().then_some(local)
}

/// The `<kind>` folder that belongs to the executable in `exe_dir`, when there is one: the folder
/// beside it, or for a core inside the window's package (`core\x64\chrono.exe`, `core\x86\chrono.exe`)
/// the one at the root of that package, where the package keeps its one copy of the data.
///
/// The second is what made the rule above hold for the window's package (R5-B6). Its cores have no
/// catalogue beside them, so they fell through to the working directory - harmless from the window,
/// which starts them in the package root, and exactly the rescue the rule forbids for anyone running
/// `core\x64\chrono.exe calc --calendar us-banking` from a folder someone else can write to. Copying
/// the data beside each core instead would make three copies, and a calendar added by hand (the README
/// invites it) would have to be added to all of them. The layout is matched by its exact names, two
/// levels and no further, so no other folder above an executable is ever read.
fn installed_catalogue_dir(exe_dir: &std::path::Path, kind: &str) -> Option<std::path::PathBuf> {
    let beside = exe_dir.join(kind);
    if beside.is_dir() {
        return Some(beside);
    }
    let named = |dir: &std::path::Path, names: &[&str]| {
        dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| names.iter().any(|want| n.eq_ignore_ascii_case(want)))
    };
    let core = exe_dir.parent().filter(|core| named(exe_dir, &["x64", "x86"]) && named(core, &["core"]))?;
    let root = core.parent()?.join(kind);
    root.is_dir().then_some(root)
}

/// The `<kind>` directory a lookup by id would read from, by the same rule as [`find_catalogue_in`]:
/// the one that belongs to the executable when it exists, which then answers for itself, else the
/// working directory's. `None` when neither exists. The preset catalogue lists this directory, so a
/// file it lists is one `--preset` finds.
pub(crate) fn find_catalogue_dir_in(exe_dir: Option<&std::path::Path>, cwd: &std::path::Path, kind: &str) -> Option<std::path::PathBuf> {
    if let Some(installed) = exe_dir.and_then(|dir| installed_catalogue_dir(dir, kind)) {
        return Some(installed);
    }
    let local = cwd.join(kind);
    local.is_dir().then_some(local)
}

/// [`find_catalogue_dir_in`] against this process's real executable and working directory.
pub(crate) fn find_catalogue_dir(kind: &str) -> Option<std::path::PathBuf> {
    let exe = crate::own_path::own_exe().ok();
    let exe_dir = exe.as_ref().and_then(|e| e.parent());
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    find_catalogue_dir_in(exe_dir, &cwd, kind)
}

/// [`find_catalogue_in`] against this process's real executable and working directory.
pub(crate) fn find_catalogue_file(kind: &str, id: &str) -> Option<std::path::PathBuf> {
    let exe = crate::own_path::own_exe().ok();
    let exe_dir = exe.as_ref().and_then(|e| e.parent());
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    find_catalogue_in(exe_dir, &cwd, kind, id)
}

/// Where the lookup actually looked, for the "not found" message. The two layouts differ, and naming
/// `./<kind>` for an installed one that never consulted it would send the reader to fix the wrong
/// folder - in the single message they have to act on (rule 6).
pub(crate) fn catalogue_search_places(kind: &str) -> String {
    let exe = crate::own_path::own_exe().ok();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    catalogue_search_places_in(exe.as_ref().and_then(|e| e.parent()), &cwd, kind)
}

/// [`catalogue_search_places`] for given folders. The folders are named as the paths they are, and the
/// one that answered lists the ids it holds (R5-D5, item 1): the message said `looked in <exe>/calendars`,
/// with the placeholder printed as it stands, and gave no hint of which ids would have worked.
pub(crate) fn catalogue_search_places_in(exe_dir: Option<&std::path::Path>, cwd: &std::path::Path, kind: &str) -> String {
    let Some(dir) = find_catalogue_dir_in(exe_dir, cwd, kind) else {
        let beside = exe_dir.map(|d| format!("{} and ", d.join(kind).display())).unwrap_or_default();
        return format!("looked for {beside}{} - neither folder exists", cwd.join(kind).display());
    };
    match catalogue_ids(&dir).as_slice() {
        [] => format!("looked in {}, which holds no {kind}", dir.display()),
        ids => format!("looked in {}, which holds {}", dir.display(), ids.join(", ")),
    }
}

/// The ids a catalogue folder offers - the names of its `.json` files that are valid ids - sorted, and
/// at most [`LISTED_IDS`] of them with the count of the rest. Read only for a refusal, never on the way
/// to a result.
///
/// The smallest ids are kept while the folder is read and the rest only counted, so a folder of any
/// size is walked once and never held whole.
pub(crate) fn catalogue_ids(dir: &std::path::Path) -> Vec<String> {
    let mut kept = std::collections::BTreeSet::new();
    let mut rest = 0usize;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let is_json = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
        match path.file_stem().and_then(|s| s.to_str()) {
            Some(id) if is_json && is_valid_catalogue_id(id) => kept.insert(id.to_string()),
            _ => continue,
        };
        if kept.len() > LISTED_IDS {
            kept.pop_last();
            rest += 1;
        }
    }
    let mut ids: Vec<String> = kept.into_iter().collect();
    if rest > 0 {
        ids.push(format!("and {rest} more"));
    }
    ids
}

/// How many ids a "not found" message lists before it counts the rest.
const LISTED_IDS: usize = 30;

pub(crate) fn find_calendar_file(id: &str) -> Result<std::path::PathBuf, String> {
    if !is_valid_catalogue_id(id) {
        return Err(format!("invalid calendar id '{id}' (use letters, digits, '-' or '_')"));
    }
    find_catalogue_file("calendars", id)
        .ok_or_else(|| format!("calendar '{id}' not found ({})", catalogue_search_places("calendars")))
}

/// Load and validate a calendar by id, mapping the JSON schema to the core engine's types.
pub(crate) fn load_calendar(id: &str) -> Result<chrono_core::calendar::Calendar, String> {
    let path = find_calendar_file(id)?;
    load_calendar_at(&path, id)
}

/// Load and validate the calendar file at `path`, found as `id`. Apart from the lookup, so a caller
/// can tell a calendar that is not there from one that is there and cannot be used (R5-D5, item 2).
pub(crate) fn load_calendar_at(path: &std::path::Path, id: &str) -> Result<chrono_core::calendar::Calendar, String> {
    read_calendar_at(path, id).map(|(calendar, _)| calendar)
}

/// What a person knows a calendar by, and counting has no use for: the country it is for and its own
/// name, in English and in the country's language. The calendar catalogue lists it (`chrono calendars`,
/// R5-D50). A file may leave the name out (docs/04 section 5), and a surface then shows the id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CalendarTitle {
    pub(crate) country: String,
    pub(crate) name: Option<(String, String)>,
}

// The title, read from a text the strict reader has already accepted, so this one names its two fields
// and nothing else - every other word of the file has been judged by `CalendarDto`.
#[derive(Deserialize)]
#[serde(expecting = "a calendar - an object with schema, id, country, weekend, observed, valid_from, law_as_of, source and holidays")]
struct TitleDto {
    country: String,
    #[serde(default)]
    name: Option<NameDto>,
}

/// The title of the calendar file at `path`, found as `id`, or the refusal `--calendar` gives for the
/// same file. Through the reader [`load_calendar_at`] uses, one read of the file, so a calendar has a
/// title here exactly when `calc --calendar <id>` can count in it.
pub(crate) fn calendar_title_at(path: &std::path::Path, id: &str) -> Result<CalendarTitle, String> {
    let (_, text) = read_calendar_at(path, id)?;
    let title: TitleDto =
        from_object(&text).map_err(|e| format!("{} (in {})", json_refusal("calendar", &e), path.display()))?;
    Ok(CalendarTitle { country: title.country, name: title.name.map(|name| (name.en, name.local)) })
}

/// Read, validate and match the id of the calendar file at `path`, keeping the text it was read from.
fn read_calendar_at(path: &std::path::Path, id: &str) -> Result<(chrono_core::calendar::Calendar, String), String> {
    let text = read_catalogue_file(path)?;
    let calendar = calendar_from_text(&text).map_err(|e| format!("{e} (in {})", path.display()))?;
    check_declared_id("calendar", path, &calendar.id, id)?;
    Ok((calendar, text))
}

/// What a catalogue command (`chrono presets`, `chrono calendars`) was asked for: a folder to read in
/// place of the one its lookup reads, and whether to answer in JSON.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CatalogueArgs {
    pub(crate) dir: Option<std::path::PathBuf>,
    pub(crate) json: bool,
}

/// The words after a catalogue command, each flag at most once. `command` names it in a refusal.
pub(crate) fn parse_catalogue_args(command: &str, args: &[String]) -> Result<CatalogueArgs, String> {
    let mut parsed = CatalogueArgs { dir: None, json: false };
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--json" if !parsed.json => parsed.json = true,
            "--dir" if parsed.dir.is_none() => {
                let value = words.next().filter(|v| !v.starts_with("--")).ok_or("--dir needs a folder")?;
                parsed.dir = Some(std::path::PathBuf::from(value));
            }
            "--json" | "--dir" => return Err(format!("{word} given twice")),
            other => return Err(format!("unknown argument '{other}' for {command}")),
        }
    }
    Ok(parsed)
}

/// Refuse a catalogue file whose `id` is not the name it was found by (R4-N37, the owner's decision
/// R4-D14).
///
/// The lookup goes by file name and the report quotes the id inside, so `other.json` declaring `pl`
/// was loaded as `--calendar other` and signed its answers "(pl)" - a business-day result attributed
/// to a calendar nobody asked for. The window drops such a preset from its list the same way it drops
/// any file it cannot use, so both surfaces show one catalogue. Compared without case, because that
/// is how Windows matches the file name: `--calendar PL` finds `pl.json`, and refusing it as a mismatch
/// would name a file name that is not on disk.
pub(crate) fn check_declared_id(kind: &str, path: &std::path::Path, declared: &str, requested: &str) -> Result<(), String> {
    if declared.eq_ignore_ascii_case(requested) {
        return Ok(());
    }

    Err(format!(
        "{} declares the {kind} id '{declared}', but it is found as '{requested}' - a {kind}'s id has to match its file name",
        path.display()
    ))
}

/// The largest catalogue file (calendar or preset) this build will read.
///
/// Two hundred times the largest shipped one, and a bound rather than a judgement about what a file
/// should contain: `read_to_string` on a file with no ceiling is the one unbounded input left in this
/// tool, and calendars are the one catalogue outsiders are invited to write. A generated file, or a
/// path that turns out to point at something that is not a catalogue at all, should be refused by
/// name rather than pulled into memory whole.
pub(crate) const MAX_CATALOGUE_BYTES: u64 = 4 * 1024 * 1024;

/// The largest number of holidays a calendar may declare - see the note where it is enforced. The
/// shipped calendars hold about fourteen, so this is two orders of magnitude of room.
pub(crate) const MAX_HOLIDAYS: usize = 2000;

/// Read a catalogue file, refusing one too large to be a catalogue.
///
/// The size is checked before the read, not after, which is the whole point - a file that cannot be a
/// catalogue never has to fit in memory to be rejected. Both go through one open handle, and the read
/// itself stops one byte past the limit. Asking the path for its size and then reading the path again
/// left a window in which the file could be swapped or grow, and the read had no ceiling of its own
/// (R5-B9, item 6).
pub(crate) fn read_catalogue_file(path: &std::path::Path) -> Result<String, String> {
    let cannot_read = |e: std::io::Error| format!("cannot read {}: {e}", path.display());
    let file = std::fs::File::open(path).map_err(cannot_read)?;
    let size = file.metadata().map_err(cannot_read)?.len();
    read_catalogue_text(file, size, path)
}

/// [`read_catalogue_file`] from the open file on, given the size the file said it had. Apart so that a
/// file which grows past that size while it is read can be stood in for in a test.
fn read_catalogue_text(file: impl std::io::Read, size: u64, path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let cannot_read = |e: std::io::Error| format!("cannot read {}: {e}", path.display());
    if size > MAX_CATALOGUE_BYTES {
        return Err(format!(
            "{} is {size} bytes, past the {MAX_CATALOGUE_BYTES}-byte limit for a catalogue file",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(MAX_CATALOGUE_BYTES + 1).read_to_end(&mut bytes).map_err(cannot_read)?;
    if bytes.len() as u64 > MAX_CATALOGUE_BYTES {
        return Err(format!(
            "{} grew past the {MAX_CATALOGUE_BYTES}-byte limit for a catalogue file while it was read",
            path.display()
        ));
    }
    // A UTF-8 byte order mark is skipped (R4-N37). Windows PowerShell 5.1 writes one for
    // `-Encoding UTF8`, and JSON parsers may ignore it (RFC 8259 section 8.1). serde does not, and read
    // it as a stray character before the first brace: "expected value at line 1 column 1", of a file
    // that looks perfect in any editor. The window's reader already skipped it, so the same preset
    // worked there and was refused here.
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bytes.drain(..3);
    }

    String::from_utf8(bytes).map_err(|e| format!("cannot read {}: it is not UTF-8 text ({e})", path.display()))
}

/// serde's own words for a shape, as the person who wrote the file would put them. Checked in order,
/// each one replaced wherever it stands.
const SERDE_WORDS: [(&str, &str); 9] = [
    // A step written as its bare name, `"snap"` where `{"snap": "end-of-month"}` belongs. Only a preset
    // step can produce it: the one enum in either reader whose variants carry a value.
    (
        "invalid type: unit variant, expected newtype variant",
        "invalid type: a step name on its own, expected the name with its value, like {\"snap\": \"end-of-month\"}",
    ),
    // A rule's `type` or a step's key that names nothing this build knows, and a rule `type` that is not
    // a text at all. "Variant" is the Rust word for both - found by the search in step 2d.
    ("unknown variant", "unknown name"),
    ("expected variant identifier", "expected a name, written as text"),
    ("invalid type: map", "invalid type: object"),
    ("invalid type: sequence", "invalid type: list"),
    ("expected a sequence", "expected a list"),
    ("expected i64", "expected a whole number"),
    ("expected i32", "expected a whole number"),
    ("expected u32", "expected a whole number, zero or more"),
];

/// A JSON refusal of a catalogue file - a preset or a calendar - in the words of whoever wrote it.
///
/// 🔴 serde names what it expected by the RUST TYPE it was building: "expected struct PresetTextDto",
/// "data did not match any variant of untagged enum BaseDto", "expected u32". That sentence went
/// straight to the CLI and to the window's list of skipped files, in front of somebody who wrote JSON
/// and has never seen the code. Every DTO carries its own `expecting` text for its half of it, and the
/// rest of serde's vocabulary, which no attribute reaches, is translated here.
///
/// The place in the file moves to the front, where it does not run on from a JSON example. And a list
/// given where an object belongs is called that: serde reads a struct from a list too, by position, so
/// it complained about a LENGTH ("invalid length 0, expected struct PresetDto with 9 elements") and
/// told the author that a preset could be an array.
pub(crate) fn json_refusal(kind: &str, error: &serde_json::Error) -> String {
    let full = error.to_string();
    let at = format!(" at line {} column {}", error.line(), error.column());
    // serde_json counts the characters read before the error, so a file refused on its very first
    // character - a list where an object belongs - comes back as column 0, which no editor shows.
    let (said, place) = match full.strip_suffix(&at) {
        Some(said) if error.line() > 0 => (said, format!(" at line {}, column {}", error.line(), error.column().max(1))),
        _ => (full.as_str(), String::new()),
    };
    let mut said = match said.strip_prefix("invalid length ").and_then(|rest| rest.split_once(", expected ")) {
        Some((_, expected)) => format!("invalid type: list, expected {expected}"),
        None => said.to_string(),
    };
    for (theirs, ours) in SERDE_WORDS {
        said = said.replace(theirs, ours);
    }

    // serde quotes an unknown field or variant as it found it, and two kinds of file reach this
    // sentence without [`refuse_hidden_characters`] having seen every text in them: one that is not
    // JSON at all, read up to the field before the fault, and one with a key given twice, whose first
    // value the walk never holds. A hidden character in such a quote is written as its code.
    format!("bad {kind} JSON{place}: {}", chrono_core::text::escape_hidden(&said))
}

/// A catalogue file's top level, which answers a struct only from an object.
///
/// serde reads a struct from a list too, by position, so a preset file holding nine values in field order
/// loaded as a preset, and a file holding a LIST of presets - the likelier mistake - was read as one
/// preset whose `schema` was the first object, and refused with "expected a string" (found in review,
/// measured on the binary). The derived `Deserialize` of a struct asks for a struct, and serde_json answers
/// that from an object or a list. This answers it from an object alone, so a list is refused in the
/// struct's own words at the list's place. Only the top level: a nested object written as a list, in field
/// order, is still read by position - wrapping every nested value would mean wrapping all of serde's access
/// traits, for a shape nobody writes by accident.
struct ObjectOnly<'a, 'de>(&'a mut serde_json::Deserializer<serde_json::de::StrRead<'de>>);

impl<'de> serde::Deserializer<'de> for ObjectOnly<'_, 'de> {
    type Error = serde_json::Error;

    fn deserialize_any<V: serde::de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        serde::Deserializer::deserialize_any(self.0, visitor)
    }

    fn deserialize_struct<V: serde::de::Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        serde::Deserializer::deserialize_map(self.0, visitor)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option unit
        unit_struct newtype_struct seq tuple tuple_struct map enum identifier ignored_any
    }
}

/// A catalogue file read as `T`, its top level as an object only (see [`ObjectOnly`]), and nothing after it.
pub(crate) fn from_object<'de, T: Deserialize<'de>>(text: &'de str) -> serde_json::Result<T> {
    let mut reader = serde_json::Deserializer::from_str(text);
    let value = T::deserialize(ObjectOnly(&mut reader))?;
    reader.end()?;
    Ok(value)
}

/// Refuse a catalogue file - a calendar or a preset - that holds a character which does not show as
/// itself anywhere in it, in a text or in a key (R5-B1, [`chrono_core::text::is_hidden_char`]).
///
/// Catalogue files are written to be shared, calendars by outsiders on purpose, and their texts reach a
/// terminal and the evidence a tester quotes. A preset whose `explains` held a carriage return and an
/// erase-line sequence printed a fake `result:` line over the real one, measured. Refused rather than
/// escaped when printed, because none of these characters has an honest use in a one-line name and
/// none of the shipped files holds one. The whole document is walked, before the typed read: many
/// refusals quote what the file says (an unknown schema, a value of `observed`, serde's unknown field),
/// so a check of the known text fields alone would still let such a character out through an error
/// message. A file that is not JSON at all passes here and the typed read names what is wrong with it,
/// in [`json_refusal`], which writes such a character as its code.
pub(crate) fn refuse_hidden_characters(kind: &str, text: &str) -> Result<(), String> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(text) else {
        return Ok(());
    };
    let refusal = |place: String, at: usize, c: char| {
        format!(
            "{kind} file: {place} holds U+{:04X} at character {at} - a character that does not show as itself, which a {kind} cannot carry because it can change what a terminal shows. Remove it, or retype the text in an editor that shows hidden characters",
            c as u32
        )
    };
    let mut pending = vec![(String::new(), &root)];
    while let Some((path, value)) = pending.pop() {
        match value {
            serde_json::Value::String(s) => {
                if let Some((at, c)) = chrono_core::text::first_hidden_char(s) {
                    return Err(refusal(format!("the text {}", json_place(&path)), at, c));
                }
            }
            serde_json::Value::Array(items) => {
                pending.extend(items.iter().enumerate().rev().map(|(i, item)| (format!("{path}[{i}]"), item)));
            }
            serde_json::Value::Object(map) => {
                for (key, item) in map.iter().rev() {
                    if let Some((at, c)) = chrono_core::text::first_hidden_char(key) {
                        return Err(refusal(format!("a key {}", json_place(&path)), at, c));
                    }
                    pending.push((json_child(&path, key), item));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Where in a catalogue file a JSON path points, as a refusal says it.
fn json_place(path: &str) -> String {
    if path.is_empty() { "at the top level".to_string() } else { format!("in `{path}`") }
}

/// The path of `key` inside the object at `path`, written the way the file's author reads it.
fn json_child(path: &str, key: &str) -> String {
    if path.is_empty() { key.to_string() } else { format!("{path}.{key}") }
}

/// Parse and validate a calendar from its JSON text, mapping the `chronomock.calendar/1` schema to the
/// engine's types. Separated from the on-disk lookup (symmetry with `parse_preset`) so the shipped
/// calendars can be golden-tested against the real engine without the file-resolution step.
pub(crate) fn calendar_from_text(text: &str) -> Result<chrono_core::calendar::Calendar, String> {
    refuse_hidden_characters("calendar", text)?;
    let schema: SchemaDto = from_object(text).map_err(|e| json_refusal("calendar", &e))?;
    // An unknown major schema version is refused, not half-understood (docs/04 section 3.1).
    if schema.schema != "chronomock.calendar/1" {
        return Err(format!(
            "unsupported calendar schema '{}' (this build reads chronomock.calendar/1)",
            schema.schema
        ));
    }
    let dto: CalendarDto = from_object(text).map_err(|e| json_refusal("calendar", &e))?;
    let mut weekend = dto.weekend.iter().map(|w| weekday_index(w)).collect::<Result<Vec<_>, _>>()?;
    // Duplicates are harmless to the engine (membership is a contains) but they hide a typo, and
    // they make the count below meaningless - so fold them away before counting.
    weekend.sort_unstable();
    weekend.dedup();
    // A holiday list has to stay a list of holidays. The cost of deciding "is this a business day" is
    // the number of holidays in force times three years, and a business-day WALK pays it again every
    // time it crosses a year - so the two multiply. Measured on this machine: 20 000 holidays and a
    // +200000bd walk took 4.6 s in release and 1 min 55 s in debug, against 0.47 s for the same walk
    // on a shipped calendar. The GUI's calc timeout would cut that off with a message, and the CLI has
    // no such ceiling. A number no real calendar approaches (the shipped ones hold about fourteen) is
    // cheap to refuse here, where the author can see it.
    if dto.holidays.len() > MAX_HOLIDAYS {
        return Err(format!(
            "calendar lists {} holidays, past the limit of {MAX_HOLIDAYS} this build reads",
            dto.holidays.len()
        ));
    }
    // A week with no working day leaves "+1 business day" with nothing to land on. The engine now
    // bounds its walk instead of hanging (S-1), but a file this broken should never reach it: say
    // which field is wrong, here, where the author can fix it.
    if weekend.len() >= 7 {
        return Err(
            "calendar 'weekend' lists all seven days - no business day would ever exist".to_string()
        );
    }
    // `observed` and `weekend` have to describe the SAME weekend. The engine's observance rules match
    // Saturday and Sunday by name - their variant names say so (`sun_to_mon`) - while `weekend` is a
    // free list of days. A file combining a Friday-Saturday weekend with a shift rule therefore
    // loaded cleanly and then did two wrong things quietly: a Friday holiday stayed put (the rule
    // never saw it) and a Sunday holiday moved to Monday although Sunday is a working day there. Both
    // come out as a wrong payment date with no message anywhere (R2-N8).
    //
    // Refused here rather than generalised in the engine. Generalising means new variant names - the
    // existing ones are ABOUT Saturday and Sunday, and reusing `weekend_to_mon` for a weekend that
    // ends on Saturday would produce a Sunday while the name promises Monday, which misleads worse
    // than the missing feature does. That wants a real market to define it, not a guess. A calendar
    // with a different weekend can still ship today with `"observed": "none"`.
    let observed = observed_from(&dto.observed)?;
    if observed != chrono_core::calendar::Observed::None && weekend != [0, 6] {
        return Err(format!(
            "calendar 'observed' is '{}', which is defined for a Saturday-Sunday weekend, but 'weekend' is {:?} - use \"observed\": \"none\" until an observance rule exists for that weekend",
            dto.observed, dto.weekend
        ));
    }
    let law_as_of = law_as_of_from(&dto.law_as_of)?;
    // The range has to be one this engine can keep and one the check could have looked at: a first year
    // after the day the list was checked would claim rules nobody has read yet.
    if dto.valid_from < FIRST_GREGORIAN_YEAR || dto.valid_from > law_as_of.year {
        return Err(format!(
            "calendar 'valid_from' is {}, outside {FIRST_GREGORIAN_YEAR}..={} (the Gregorian calendar up to the year of 'law_as_of')",
            dto.valid_from, law_as_of.year
        ));
    }

    // A set, because a list made this check quadratic: 2.3 ms of the 3.4 ms a 2 000-holiday file took
    // to load, paid on every calculation the window runs with it (R5-WY2).
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::with_capacity(dto.holidays.len());
    let holidays = dto
        .holidays
        .into_iter()
        .map(|h| {
            // A duplicate id makes the audit ambiguous - `holiday_on` names one of them and the reader
            // cannot tell which. Cheap to catch, impossible to diagnose later.
            if seen_ids.contains(&h.id) {
                return Err(format!("duplicate holiday id '{}'", h.id));
            }

            // An inverted window silently means "never a holiday", which reads as a missing entry
            // rather than as the mistake it is.
            if let (Some(from), Some(to)) = (h.valid_from, h.valid_to)
                && from > to {
                    return Err(format!(
                        "holiday '{}': valid_from {from} is after valid_to {to}",
                        h.id
                    ));
                }
            // A holiday that ended before the calendar's first year can never apply - a typo in a year,
            // or a range that was meant to reach further back and does not.
            if let Some(to) = h.valid_to
                && to < dto.valid_from {
                    return Err(format!(
                        "holiday '{}': valid_to {to} is before the calendar's valid_from {}, so it never applies",
                        h.id, dto.valid_from
                    ));
                }

            seen_ids.insert(h.id.clone());
            let rule = rule_from(&h.id, h.rule)?;
            Ok(chrono_core::calendar::Holiday {
                id: h.id,
                name_en: h.name.en,
                name_local: h.name.local,
                rule,
                valid_from: h.valid_from,
                valid_to: h.valid_to,
                source: h.source,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(chrono_core::calendar::Calendar {
        id: dto.id,
        country: dto.country,
        weekend,
        observed,
        valid_from: Some(dto.valid_from),
        law_as_of: Some(law_as_of),
        holidays,
    })
}

/// The calendar's `law_as_of`, a plain `YYYY-MM-DD` that names a real day, read through the same
/// civil-date parser as every other date here.
fn law_as_of_from(text: &str) -> Result<chrono_core::calc::CivilDateTime, String> {
    let refuse = || format!("calendar 'law_as_of' is '{text}', not a date written YYYY-MM-DD");
    if text.len() != 10 {
        return Err(refuse());
    }
    chrono_core::calc::parse_civil_datetime(&format!("{text}T00:00:00")).map_err(|_| refuse())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::read_data;

    /// S-1, first line. Calendars are the one catalogue outsiders are invited to write, so a file
    /// that makes "next business day" unanswerable has to be refused where its author can see it -
    /// not walked into by the engine. Duplicated weekend days are folded first, so the check counts
    /// distinct days and a repeated "saturday" is not mistaken for a full week.
    /// A holiday list has to stay a list of holidays.
    ///
    /// The cost of "is this a business day" is the number of holidays in force times three years, and a
    /// business-day walk pays it again on every year boundary it crosses - so the two multiply.
    /// Measured on this machine: 20 000 holidays and `+200000bd` took 4.6 s in release and 1 min 55 s
    /// in debug, against 0.47 s for the same walk on a shipped calendar. The GUI's calc timeout cuts
    /// that off with a message and the CLI has no such ceiling, so the refusal belongs here, where the
    /// file's author can see it. The shipped calendars hold about fourteen.
    #[test]
    fn a_calendar_with_an_implausible_number_of_holidays_is_refused() {
        let holiday = |i: usize| {
            format!(
                r#"{{"id":"h{i}","name":{{"en":"H","local":"H"}},"rule":{{"type":"easter_offset","offset":1}},"source":"probe"}}"#
            )
        };
        let with_holidays = |n: usize| {
            let list: Vec<String> = (0..n).map(holiday).collect();
            format!(
                r#"{{"schema":"chronomock.calendar/1","id":"probe","country":"XX","weekend":["saturday","sunday"],"observed":"none","valid_from":2000,"law_as_of":"2026-01-01","source":"t","holidays":[{}]}}"#,
                list.join(",")
            )
        };

        let err = calendar_from_text(&with_holidays(MAX_HOLIDAYS + 1))
            .expect_err("past the limit this build reads");
        assert!(err.contains("past the limit"), "the message names the limit, got: {err}");
        assert!(err.contains(&MAX_HOLIDAYS.to_string()), "and the number, got: {err}");

        // At the limit is fine - the bound is a ceiling on the implausible, not on the large.
        assert!(calendar_from_text(&with_holidays(MAX_HOLIDAYS)).is_ok());
    }

    /// A text or a key holding a character that does not show as itself is refused before anything
    /// else reads the file, by its place in the file and its code point (R5-B1). The characters go
    /// into the JSON as escapes, the way a shared file carries them, built at run time so this source
    /// holds none of them.
    #[test]
    fn a_calendar_text_or_key_with_a_hidden_character_is_refused_by_its_place() {
        let escape = |code: u32| format!("\\u{code:04x}");
        let calendar = |local: &str, extra: &str| {
            format!(
                r#"{{"schema":"chronomock.calendar/1","id":"probe","country":"XX","weekend":["saturday","sunday"],"observed":"none","valid_from":2000,"law_as_of":"2026-01-01","source":"t"{extra},"holidays":[{{"id":"h","name":{{"en":"H","local":"{local}"}},"rule":{{"type":"easter_offset","offset":1}},"source":"probe"}}]}}"#
            )
        };
        assert!(calendar_from_text(&calendar("Wielkanoc", "")).is_ok(), "a plain name passes");

        let err = calendar_from_text(&calendar(&format!("Wielka{}noc", escape(0x202E)), "")).unwrap_err();
        assert!(
            err.contains("the text in `holidays[0].name.local`") && err.contains("U+202E") && err.contains("character 7"),
            "{err}"
        );
        assert!(err.contains("Remove it, or retype the text"), "says what to do: {err}");
        // The measured attack: a carriage return and an erase-line sequence.
        let err = calendar_from_text(&calendar(&format!("ok{}{}[2K", escape(0x0D), escape(0x1B)), "")).unwrap_err();
        assert!(err.contains("U+000D"), "{err}");
        // A key too, because serde quotes an unknown field by its name.
        let err = calendar_from_text(&calendar("H", &format!(r#","x{}":1"#, escape(0x200B)))).unwrap_err();
        assert!(err.contains("a key at the top level") && err.contains("U+200B"), "{err}");
    }

    /// A catalogue file is outside input, and `read_to_string` on it was the last unbounded read in
    /// this tool. The size is checked BEFORE the read, so a file that cannot be a catalogue never has
    /// to fit in memory to be rejected.
    #[test]
    fn a_catalogue_file_past_the_size_limit_is_refused_without_being_read() {
        let dir = std::env::temp_dir().join(format!("chrono-cat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("huge.json");
        // One byte past the ceiling, written as zeros - never parsed, so the content is immaterial.
        std::fs::write(&path, vec![b'0'; MAX_CATALOGUE_BYTES as usize + 1]).expect("write");

        let err = read_catalogue_file(&path).expect_err("past the size limit");
        assert!(err.contains("past the"), "got: {err}");

        std::fs::write(&path, b"{}").expect("write");
        assert_eq!(read_catalogue_file(&path).expect("within the limit"), "{}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that said it was small and then went on past the limit while it was read is refused
    /// all the same (R5-B9, item 6): the read stops one byte past the limit, whatever the file said.
    #[test]
    fn a_catalogue_file_that_grows_past_the_limit_while_read_is_refused() {
        use std::io::Read;
        let growing = std::io::repeat(b' ').take(MAX_CATALOGUE_BYTES + 10);
        let err = read_catalogue_text(growing, 2, std::path::Path::new("grows.json")).expect_err("past the limit");
        assert!(err.contains("grew past"), "{err}");
        let small = std::io::repeat(b' ').take(2);
        assert_eq!(read_catalogue_text(small, 2, std::path::Path::new("small.json")).unwrap(), "  ");
    }

    /// R2-N8: `weekend` and `observed` must describe the same weekend. A Friday-Saturday weekend with
    /// a shift rule used to load and then be wrong twice over, silently - which is the one thing a
    /// calendar file must never do, since calendars are written by people outside this build.
    #[test]
    fn an_observance_rule_with_a_foreign_weekend_is_refused() {
        let cal = |weekend: &str, observed: &str| {
            format!(
                r#"{{"schema":"chronomock.calendar/1","id":"x","country":"XX","weekend":{weekend},
                "observed":"{observed}","valid_from":2000,"law_as_of":"2026-01-01","source":"t","holidays":[]}}"#
            )
        };

        for observed in ["sat_to_fri_sun_to_mon", "sun_to_mon", "weekend_to_mon"] {
            let err = calendar_from_text(&cal(r#"["friday","saturday"]"#, observed))
                .expect_err("a foreign weekend with an observance rule must be refused");
            assert!(err.contains("observed"), "the message must name the field: {err}");
            assert!(err.contains(observed), "the message must name the rule: {err}");
            assert!(err.contains("none"), "the message must say what to do instead: {err}");
        }

        // The same weekend with no observance rule is fine - this refuses a COMBINATION, and does not
        // ban weekends the project has not shipped a calendar for.
        calendar_from_text(&cal(r#"["friday","saturday"]"#, "none")).expect("a foreign weekend alone loads");
        // And the Saturday-Sunday weekend keeps every rule, in any listed order.
        for observed in ["sat_to_fri_sun_to_mon", "sun_to_mon", "weekend_to_mon", "none"] {
            calendar_from_text(&cal(r#"["sunday","saturday"]"#, observed))
                .unwrap_or_else(|e| panic!("{observed} on a Sat-Sun weekend must load: {e}"));
        }
    }

    #[test]
    fn calendar_with_every_day_as_weekend_is_refused() {
        let all_week = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["monday","tuesday","wednesday","thursday","friday","saturday","sunday"],
            "observed":"none","valid_from":2000,"law_as_of":"2026-01-01","source":"t","holidays":[]}"#;
        let err = calendar_from_text(all_week).expect_err("a week with no working day is refused");
        assert!(err.contains("weekend"), "the message must name the field: {err}");

        // Duplicates alone are not an error - they fold, and the calendar still works.
        let dupes = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["saturday","saturday","sunday"],"observed":"none","valid_from":2000,"law_as_of":"2026-01-01","source":"t","holidays":[]}"#;
        let cal = calendar_from_text(dupes).expect("duplicate weekend days fold");
        assert_eq!(cal.weekend.len(), 2);
    }

    /// S-17. Every rule field is refused out of range, naming the holiday and the field. Unchecked,
    /// none of these fail loudly - the civil-date maths rolls month 13 into January and day 40 into the
    /// next month, so the calendar quietly marks the WRONG dates and every business-day answer built on
    /// it is wrong with it. Calendars are the documented third-party extension point, so this is the
    /// place where a broken file has to stop.
    #[test]
    fn calendar_rules_out_of_range_are_refused_with_the_field_named() {
        let cal = |rule: &str| {
            format!(
                r#"{{"schema":"chronomock.calendar/1","id":"x","country":"XX","weekend":["saturday","sunday"],
                "observed":"none","valid_from":2000,"law_as_of":"2026-01-01","source":"t","holidays":[{{"id":"bad","name":{{"en":"B","local":"B"}},
                "rule":{rule},"source":"test"}}]}}"#
            )
        };

        for (rule, needle) in [
            (r#"{"type":"fixed","month":13,"day":1}"#, "month 13"),
            (r#"{"type":"fixed","month":1,"day":40}"#, "day 40"),
            // A day that exists in NO year is a typo, not a leap-year rule (R2-N7). The engine would
            // answer "not this year" for every year, so the holiday would be silently absent forever -
            // refused here instead, and the message names the month's real length.
            (r#"{"type":"fixed","month":4,"day":31}"#, "day 31"),
            (r#"{"type":"fixed","month":2,"day":30}"#, "day 30"),
            (r#"{"type":"nth_weekday","month":1,"weekday":"monday","order":9}"#, "order 9"),
            (r#"{"type":"nth_weekday","month":0,"weekday":"monday","order":1}"#, "month 0"),
            (r#"{"type":"easter_offset","offset":5000}"#, "5000"),
            // R4-N39: one day past either end already leaves the year of its Easter in some year.
            (r#"{"type":"easter_offset","offset":-81}"#, "-81"),
            (r#"{"type":"easter_offset","offset":251}"#, "251"),
        ] {
            let err = calendar_from_text(&cal(rule)).expect_err("out of range must be refused");
            assert!(err.contains("bad"), "the message must name the holiday: {err}");
            assert!(err.contains(needle), "the message must name the value: {err}");
        }

        // The legitimate neighbours of those bounds still parse. February 29 is the one that matters:
        // it is impossible in most years and legal in the schema, so the bound is the month's longest
        // length in any year, never the length of some particular year.
        for rule in [
            r#"{"type":"fixed","month":2,"day":29}"#,
            r#"{"type":"fixed","month":4,"day":30}"#,
            r#"{"type":"fixed","month":1,"day":31}"#,
            r#"{"type":"nth_weekday","month":12,"weekday":"monday","order":-1}"#,
            r#"{"type":"nth_weekday","month":5,"weekday":"monday","order":5}"#,
            r#"{"type":"easter_offset","offset":60}"#,
            r#"{"type":"easter_offset","offset":-80}"#,
            r#"{"type":"easter_offset","offset":250}"#,
        ] {
            calendar_from_text(&cal(rule)).unwrap_or_else(|e| panic!("{rule} should parse: {e}"));
        }
    }

    /// R4-N38, the owner's decision R4-D11: a field this reader does not know is refused, at every
    /// level of the file, and the message names it. `valid_form` for `valid_from` used to load, and the
    /// holiday meant to start in 2030 counted in every year.
    #[test]
    fn an_unknown_field_is_refused_at_every_level_and_named() {
        let cal = |root: &str, holiday: &str, name: &str, rule: &str| {
            format!(
                r#"{{"schema":"chronomock.calendar/1","id":"x","country":"XX","weekend":["saturday","sunday"],
                "observed":"none"{root},"valid_from":2000,"law_as_of":"2026-01-01","source":"t","holidays":[{{"id":"h","name":{{"en":"H","local":"H"{name}}},
                "rule":{{"type":"fixed","month":7,"day":1{rule}}},"source":"t"{holiday}}}]}}"#
            )
        };
        for (text, field) in [
            (cal(r#","obsreved":"none""#, "", "", ""), "obsreved"),
            (cal("", r#","valid_form":2030"#, "", ""), "valid_form"),
            (cal("", "", r#","pl":"H""#, ""), "pl"),
            (cal("", "", "", r#","dya":2"#), "dya"),
        ] {
            let err = calendar_from_text(&text).expect_err("an unknown field is refused");
            assert!(err.contains("unknown field"), "the message says what is wrong: {err}");
            assert!(err.contains(&format!("`{field}`")), "and names the field: {err}");
        }
        // The calendar's own title is checked too - a typo there is as much a typo as anywhere else.
        let titled = cal(r#","name":{"en":"X","local":"X","loacl":"X"}"#, "", "", "");
        assert!(calendar_from_text(&titled).expect_err("a typo in the title").contains("`loacl`"));

        // The contract's own fields are not unknown, even though this reader does not use them.
        let contract = cal(r#","stability":"unstable","subregion":null,"name":{"en":"X","local":"X"}"#, "", "", "");
        calendar_from_text(&contract).expect("the contract's fields load");
        calendar_from_text(&cal("", "", "", "")).expect("and a file without them loads as before");
    }

    /// PR A finding (g), the calendar half: a wrong JSON type anywhere in a calendar - the catalogue
    /// people outside the project are invited to write - is refused in the file's own words, place first.
    /// Measured before the fix: five positions named a Rust type (`SchemaDto`, `NameDto` twice,
    /// `HolidayDto`, "internally tagged enum RuleDto"), two said "i64" or "u32", two "map" or "sequence".
    #[test]
    fn a_wrong_type_anywhere_in_a_calendar_is_refused_in_the_files_own_words() {
        let shipped: serde_json::Value = serde_json::from_str(&read_data("calendars/pl.json")).expect("pl parses");
        let with = |change: &dyn Fn(&mut serde_json::Value)| {
            let mut calendar = shipped.clone();
            change(&mut calendar);
            calendar.to_string()
        };
        let cases = [
            ("a calendar as a list", "[]".to_string()),
            ("a weekend as a word", with(&|c| c["weekend"] = "saturday".into())),
            ("a first year as text", with(&|c| c["valid_from"] = "2002".into())),
            ("a title as plain text", with(&|c| c["name"] = "Poland".into())),
            ("holidays as an object", with(&|c| c["holidays"] = serde_json::json!({}))),
            ("a holiday as a word", with(&|c| c["holidays"] = serde_json::json!(["new-year"]))),
            ("a holiday name as plain text", with(&|c| c["holidays"][0]["name"] = "New Year".into())),
            ("a rule as a word", with(&|c| c["holidays"][0]["rule"] = "fixed".into())),
            ("a month as text", with(&|c| c["holidays"][0]["rule"]["month"] = "1".into())),
            // A file holding a list of calendars, which serde used to read as one calendar by position.
            ("a list of calendars in one file", format!("[{shipped},{shipped}]")),
            // Found by the search in step 2d: serde's "variant" for a rule type that is not text, and
            // for one it does not know.
            ("a rule type as a number", with(&|c| c["holidays"][0]["rule"]["type"] = 5.into())),
            ("a rule type it does not know", with(&|c| c["holidays"][0]["rule"]["type"] = "fxed".into())),
        ];
        for (what, text) in &cases {
            let refusal = calendar_from_text(text).expect_err(what);
            assert!(refusal.starts_with("bad calendar JSON at line "), "{what}: the place comes first - {refusal}");
            for word in ["Dto", "struct ", "enum ", "i64", "i32", "u32", "sequence", "map", "invalid length", "variant"] {
                assert!(!refusal.contains(word), "{what}: says {word:?} - {refusal}");
            }
        }

        let after_place = |text: &str| {
            let refusal = calendar_from_text(text).expect_err("refused");
            refusal.split_once(": ").map(|(_, said)| said.to_string()).unwrap_or_default()
        };
        assert_eq!(
            after_place(&cases[7].1),
            r#"invalid type: string "fixed", expected a rule - an object with type (fixed, nth_weekday or easter_offset) and its fields"#
        );
        assert_eq!(after_place(&cases[3].1), r#"invalid type: string "Poland", expected a name - an object with en and local"#);
        assert_eq!(after_place(&cases[8].1), r#"invalid type: string "1", expected a whole number, zero or more"#);
        assert_eq!(after_place(&cases[9].1), after_place(&cases[0].1), "a list of calendars is a list, not a calendar");
        assert!(after_place(&cases[0].1).starts_with("invalid type: list, expected a calendar - "), "{}", after_place(&cases[0].1));
        assert_eq!(after_place(&cases[10].1), "invalid type: integer `5`, expected a name, written as text");
        assert_eq!(after_place(&cases[11].1), "unknown name `fxed`, expected one of `fixed`, `nth_weekday`, `easter_offset`");
    }

    /// The guard behind both halves of finding (g): every DTO of a file people write by hand says what it
    /// expects in the file's words. Without `expecting`, a DTO added later would put its Rust name back in
    /// front of the author on the first wrong type, and no test of today's positions would notice.
    #[test]
    fn every_dto_of_a_hand_written_file_names_what_it_expects() {
        for (file, source, at_least) in [("calendar.rs", include_str!("calendar.rs"), 5), ("preset.rs", include_str!("preset.rs"), 9)] {
            let lines: Vec<&str> = source.lines().collect();
            let mut derives = 0;
            for (at, line) in lines.iter().enumerate() {
                if !(line.trim_start().starts_with("#[derive(") && line.contains("Deserialize")) {
                    continue;
                }
                derives += 1;
                let attributes: Vec<&str> = lines[at..]
                    .iter()
                    .take_while(|l| !l.contains("struct ") && !l.contains("enum "))
                    .copied()
                    .collect();
                assert!(
                    attributes.iter().any(|l| l.contains("expecting = ")),
                    "{file}:{}: a DTO without `expecting` - serde would name its Rust type to the author",
                    at + 1
                );
            }
            assert!(derives >= at_least, "{file}: found {derives} DTOs, expected at least {at_least} - is the scan reading the file?");
        }
    }

    /// A file written to a later schema is refused as that, not over its first new field. Read
    /// strictly in one pass, `chronomock.calendar/2` with an added field would be reported as a typo,
    /// and its author would go looking for one.
    #[test]
    fn a_later_schema_is_refused_as_a_schema_not_as_an_unknown_field() {
        let v2 = r#"{"schema":"chronomock.calendar/2","id":"x","country":"XX","weekend":[],
            "observed":"none","valid_from":2000,"law_as_of":"2026-01-01","source":"t","holidays":[],"range":[1990,2100]}"#;
        let err = calendar_from_text(v2).expect_err("a later schema");
        assert!(err.contains("unsupported calendar schema 'chronomock.calendar/2'"), "got: {err}");
        assert!(!err.contains("unknown field"), "got: {err}");
    }

    /// R4-N37. A catalogue file saved with a UTF-8 byte order mark - Windows PowerShell 5.1 does that
    /// for `-Encoding UTF8` - was refused with "expected value at line 1 column 1", while the window
    /// read the same file. A file that is not UTF-8 at all is still refused, and says so.
    #[test]
    fn a_byte_order_mark_is_skipped_and_other_bytes_are_still_refused() {
        let dir = crate::testutil::unique_temp_dir("chrono-bom");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("pl.json");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(read_data("calendars/pl.json").as_bytes());
        std::fs::write(&path, &bytes).expect("write");
        let text = read_catalogue_file(&path).expect("a file with a byte order mark reads");
        assert!(text.starts_with('{'), "the mark is gone: {:?}", text.chars().next());
        calendar_from_text(&text).expect("and the calendar in it loads");

        std::fs::write(&path, [0xFF, 0xFE, b'{', 0, b'}', 0]).expect("write");
        let err = read_catalogue_file(&path).expect_err("UTF-16 is not UTF-8");
        assert!(err.contains("not UTF-8"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R4-N37, the owner's decision R4-D14: a catalogue file's id is the name it is found by. The
    /// comparison ignores case, because the lookup does: `--calendar PL` finds `pl.json`.
    #[test]
    fn a_declared_id_other_than_the_file_name_is_refused() {
        let path = std::path::Path::new("calendars").join("other.json");
        let err = check_declared_id("calendar", &path, "pl", "other").expect_err("pl is not other");
        assert!(err.contains("'pl'") && err.contains("'other'"), "both ids are named: {err}");
        assert!(err.contains("other.json"), "and the file: {err}");
        check_declared_id("calendar", &path, "pl", "PL").expect("case is how Windows matches a file name");
    }

    /// Every calendar in `calendars/` loads the way `--calendar` loads it: read strictly and found by
    /// its own id. CONTRIBUTING.md tells an author that the suite parses and validates a new calendar,
    /// and until this test only the three files named in the golden test below were read - a fourth
    /// was checked by nothing (rule 12).
    #[test]
    fn every_shipped_calendar_loads_under_its_own_name() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../calendars");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("the calendars folder") {
            let path = entry.expect("an entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).expect("a file name").to_string();
            let text = read_catalogue_file(&path).unwrap_or_else(|e| panic!("{stem}: {e}"));
            let calendar = calendar_from_text(&text).unwrap_or_else(|e| panic!("{stem}: {e}"));
            check_declared_id("calendar", &path, &calendar.id, &stem).unwrap_or_else(|e| panic!("{e}"));
            seen += 1;
        }
        assert!(seen >= 3, "the three shipped calendars at least, found {seen}");
    }

    /// S-17, the whole-file checks: a repeated id makes the audit ambiguous (holiday_on names one of
    /// them and the reader cannot tell which), and an inverted validity window means "never a holiday",
    /// which reads as a missing entry instead of the mistake it is.
    #[test]
    fn duplicate_holiday_ids_and_inverted_validity_windows_are_refused() {
        let dupes = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["saturday","sunday"],"observed":"none","valid_from":2000,"law_as_of":"2026-01-01","source":"t","holidays":[
            {"id":"same","name":{"en":"A","local":"A"},"rule":{"type":"fixed","month":1,"day":1},"source":"t"},
            {"id":"same","name":{"en":"B","local":"B"},"rule":{"type":"fixed","month":2,"day":2},"source":"t"}]}"#;
        assert!(calendar_from_text(dupes).expect_err("duplicate id").contains("same"));

        let inverted = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["saturday","sunday"],"observed":"none","valid_from":2000,"law_as_of":"2026-01-01","source":"t","holidays":[
            {"id":"w","name":{"en":"A","local":"A"},"rule":{"type":"fixed","month":1,"day":1},
             "valid_from":2030,"valid_to":2020,"source":"t"}]}"#;
        assert!(calendar_from_text(inverted).expect_err("inverted window").contains("valid_from"));
    }

    /// An installed layout answers for its own catalogue, "not here" included. Without that, running
    /// `chrono calc --calendar us-banking` from a directory someone else can write to answered
    /// business-day questions out of THEIR file whenever the shipped one was missing - different
    /// holidays, different answers, no warning, in output a tester quotes as evidence.
    ///
    /// The working-directory fallback itself has to stay: `chrono.exe` is built into
    /// `target/<triple>/release/`, which has no `calendars/` beside it, so a dev checkout and all
    /// 133 harness scenarios resolve that way.
    #[test]
    fn an_installed_catalogue_is_not_rescued_from_the_working_directory() {
        let root = std::env::temp_dir().join(format!("chrono-catalogue-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let exe_dir = root.join("app");
        let cwd = root.join("elsewhere");
        std::fs::create_dir_all(exe_dir.join("calendars")).unwrap();
        std::fs::create_dir_all(cwd.join("calendars")).unwrap();
        std::fs::write(cwd.join("calendars").join("us-banking.json"), "{}").unwrap();

        // Installed layout, file absent from it: the one in the working directory is NOT used.
        assert_eq!(find_catalogue_in(Some(&exe_dir), &cwd, "calendars", "us-banking"), None);

        // Installed layout that does have it: that copy wins.
        let shipped = exe_dir.join("calendars").join("us-banking.json");
        std::fs::write(&shipped, "{}").unwrap();
        assert_eq!(find_catalogue_in(Some(&exe_dir), &cwd, "calendars", "us-banking"), Some(shipped));

        // No catalogue beside the executable at all - a dev checkout. The fallback still works, and
        // this is the case the harness runs in.
        let bare = root.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(
            find_catalogue_in(Some(&bare), &cwd, "calendars", "us-banking"),
            Some(cwd.join("calendars").join("us-banking.json"))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A core inside the window's package (`core\x64`, `core\x86`) reads the catalogue at the package
    /// root and is not rescued from the working directory either (R5-B6). Matched by the exact names, so
    /// a folder that only looks like the layout is not read.
    #[test]
    fn a_core_in_the_window_package_reads_the_package_catalogue_and_nothing_else() {
        let root = crate::testutil::unique_temp_dir("chrono-catalogue-package");
        let package = root.join("ChronoMock");
        let cwd = root.join("downloads");
        std::fs::create_dir_all(package.join("calendars")).unwrap();
        std::fs::create_dir_all(cwd.join("calendars")).unwrap();
        std::fs::write(cwd.join("calendars").join("us-banking.json"), "{}").unwrap();
        std::fs::write(cwd.join("calendars").join("planted.json"), "{}").unwrap();
        let shipped = package.join("calendars").join("us-banking.json");
        std::fs::write(&shipped, "{}").unwrap();

        for arch in ["x64", "x86", "X64"] {
            let core = package.join("core").join(arch);
            std::fs::create_dir_all(&core).unwrap();
            assert_eq!(find_catalogue_in(Some(&core), &cwd, "calendars", "us-banking"), Some(shipped.clone()), "{arch}");
            assert_eq!(find_catalogue_in(Some(&core), &cwd, "calendars", "planted"), None, "{arch}: not rescued");
            assert_eq!(find_catalogue_dir_in(Some(&core), &cwd, "calendars"), Some(package.join("calendars")));
        }
        // Not the layout: an `x64` under some other folder, and a folder deeper down.
        let other = package.join("tools").join("x64");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(find_catalogue_dir_in(Some(&other), &cwd, "calendars"), Some(cwd.join("calendars")));
        let deeper = package.join("core").join("x64").join("bin");
        std::fs::create_dir_all(&deeper).unwrap();
        assert_eq!(find_catalogue_dir_in(Some(&deeper), &cwd, "calendars"), Some(cwd.join("calendars")));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// "Not found" names the folders as the paths they are and lists the ids the folder that answered
    /// holds (R5-D5, item 1) - it printed `<exe>/calendars` as written and no id at all.
    #[test]
    fn not_found_names_the_real_folders_and_the_ids_on_offer() {
        let root = crate::testutil::unique_temp_dir("chrono-catalogue-places");
        let exe_dir = root.join("app");
        let cwd = root.join("work");
        std::fs::create_dir_all(exe_dir.join("calendars")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        for name in ["pl.json", "us-banking.json", "notes.txt", "bad name.json"] {
            std::fs::write(exe_dir.join("calendars").join(name), "{}").unwrap();
        }
        let said = catalogue_search_places_in(Some(&exe_dir), &cwd, "calendars");
        assert!(!said.contains("<exe>"), "{said}");
        assert!(said.contains(&exe_dir.join("calendars").display().to_string()), "{said}");
        assert!(said.ends_with("which holds pl, us-banking"), "only valid ids of .json files, sorted: {said}");

        let bare = root.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        let said = catalogue_search_places_in(Some(&bare), &cwd, "calendars");
        assert!(said.contains("neither folder exists") && said.contains(&cwd.join("calendars").display().to_string()), "{said}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A folder holding more ids than a message lists gives the smallest of them, sorted, and the
    /// count of the rest.
    #[test]
    fn a_long_catalogue_lists_its_smallest_ids_and_counts_the_rest() {
        let dir = crate::testutil::unique_temp_dir("chrono-catalogue-many");
        std::fs::create_dir_all(&dir).unwrap();
        for n in (0..=LISTED_IDS).rev() {
            std::fs::write(dir.join(format!("c{n:02}.json")), "{}").unwrap();
        }
        let ids = catalogue_ids(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        let expected: Vec<String> = (0..LISTED_IDS).map(|n| format!("c{n:02}")).chain(["and 1 more".to_string()]).collect();
        assert_eq!(ids, expected);
    }

    /// The folder the preset catalogue lists is the folder a lookup by id reads, by the same rule - so
    /// `chrono presets` never lists a file that `--preset` would not find, and never leaves out one it
    /// would.
    #[test]
    fn the_catalogue_folder_is_the_one_a_lookup_by_id_reads() {
        let root = std::env::temp_dir().join(format!("chrono-catalogue-dir-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let exe_dir = root.join("app");
        let cwd = root.join("elsewhere");
        let bare = root.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(cwd.join("presets")).unwrap();
        std::fs::write(cwd.join("presets").join("month-end.json"), "{}").unwrap();

        // No folder beside the executable: the working directory's, the one a lookup falls back to.
        assert_eq!(find_catalogue_dir_in(Some(&bare), &cwd, "presets"), Some(cwd.join("presets")));
        assert_eq!(
            find_catalogue_in(Some(&bare), &cwd, "presets", "month-end"),
            Some(cwd.join("presets").join("month-end.json"))
        );

        // An installed folder, even an empty one, answers for itself - the lookup does not fall back.
        std::fs::create_dir_all(exe_dir.join("presets")).unwrap();
        assert_eq!(find_catalogue_dir_in(Some(&exe_dir), &cwd, "presets"), Some(exe_dir.join("presets")));
        assert_eq!(find_catalogue_in(Some(&exe_dir), &cwd, "presets", "month-end"), None);

        // Neither: no folder at all, not an empty catalogue.
        assert_eq!(find_catalogue_dir_in(Some(&bare), &bare, "presets"), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shipped_calendars_parse_and_hit_golden_dates() {
        use chrono_core::calc::CivilDateTime;
        use chrono_core::calendar::{holiday_on, is_business_day};
        let d = |y: i64, m: u32, day: u32| CivilDateTime {
            year: y,
            month: m,
            day,
            hour: 0,
            minute: 0,
            second: 0,
        };

        let pl = calendar_from_text(&read_data("calendars/pl.json")).expect("pl parses");
        // Easter Monday 2026 = 2026-04-06 (Easter Sunday 2026-04-05): the easter_offset computus.
        assert_eq!(holiday_on(&d(2026, 4, 6), &pl).map(|h| h.id.as_str()), Some("easter_monday"));
        // Corpus Christi 2026 = Easter + 60 days = 2026-06-04: a larger easter_offset.
        assert_eq!(holiday_on(&d(2026, 6, 4), &pl).map(|h| h.id.as_str()), Some("corpus_christi"));
        // Epiphany was restored as a Polish public holiday from 2011 (valid_from:2011 - law of 24 Sept
        // 2010, abolished 1960): a holiday in 2026, NOT in 2010.
        assert_eq!(holiday_on(&d(2026, 1, 6), &pl).map(|h| h.id.as_str()), Some("epiphany"));
        assert!(holiday_on(&d(2010, 1, 6), &pl).is_none());
        // Christmas Eve became a non-working day from 2025 (valid_from:2025): a holiday in 2025, not 2024.
        assert_eq!(holiday_on(&d(2025, 12, 24), &pl).map(|h| h.id.as_str()), Some("christmas_eve"));
        assert!(holiday_on(&d(2024, 12, 24), &pl).is_none());

        let fed = calendar_from_text(&read_data("calendars/us-federal.json")).expect("us-federal parses");
        let bank = calendar_from_text(&read_data("calendars/us-banking.json")).expect("us-banking parses");
        // One country, two calendars: Independence Day 2026 falls on Saturday (Jul 4), so it is observed
        // on Friday Jul 3 federally (sat->fri) - not a business day - while banking (sun->mon only) does
        // not shift a Saturday holiday, so the same Friday IS a business day.
        assert!(!is_business_day(&d(2026, 7, 3), &fed));
        assert!(is_business_day(&d(2026, 7, 3), &bank));
        // Juneteenth is a federal holiday from 2021 (valid_from:2021): a holiday in 2021, not in 2020.
        assert_eq!(holiday_on(&d(2021, 6, 19), &fed).map(|h| h.id.as_str()), Some("juneteenth"));
        assert!(holiday_on(&d(2020, 6, 19), &fed).is_none());
        // MLK Day 2026 = the third Monday of January = 2026-01-19: an nth_weekday rule.
        assert_eq!(holiday_on(&d(2026, 1, 19), &fed).map(|h| h.id.as_str()), Some("mlk_day"));
        // And it is a holiday from 1986 (valid_from:1986). Public Law 98-144 was signed in 1983 with
        // effect from the first 1 January falling after a two-year period, so the third Monday of
        // January 1986 (the 20th) is the first observance and the third Monday of January 1985 (the
        // 21st) is an ordinary working day. Both calendars carry it, so both are asserted - the
        // entry read "always" until 2026-09-07, which made every date back to year 1 wrong.
        assert_eq!(holiday_on(&d(1986, 1, 20), &fed).map(|h| h.id.as_str()), Some("mlk_day"));
        assert!(holiday_on(&d(1985, 1, 21), &fed).is_none());
        // The banking calendar starts in 2008, so it carries the rule but judges neither year.
        assert!(!bank.judges_year(1986));
        // Banking observes a Sunday holiday on the Monday: New Year 2023-01-01 (Sun) -> Mon 2023-01-02.
        assert!(!is_business_day(&d(2023, 1, 2), &bank));
    }

    /// Four United States holidays have a history rather than one rule, and until 2026-09-07 both
    /// calendars answered with today's rule for every year back to year one. That put Washington's
    /// Birthday on the wrong February day for every date before 1971, invented a Columbus Day that
    /// did not exist, and missed the seven years Veterans Day spent in October.
    ///
    /// Public Law 90-363 (the Uniform Monday Holiday Act) took effect on 1971-01-01, and Public Law
    /// 94-97 returned Veterans Day to 11 November from 1978-01-01. Since R4-S20 only the federal
    /// calendar reaches back that far (from 1960) - the banking one starts in 2008, the oldest Federal
    /// Reserve schedule read, and judges none of these years.
    #[test]
    fn us_calendars_follow_the_uniform_monday_holiday_act() {
        use chrono_core::calc::CivilDateTime;
        use chrono_core::calendar::holiday_on;
        let d = |y: i64, m: u32, day: u32| CivilDateTime {
            year: y,
            month: m,
            day,
            hour: 0,
            minute: 0,
            second: 0,
        };

        let fed = calendar_from_text(&read_data("calendars/us-federal.json")).expect("us-federal parses");
        let bank = calendar_from_text(&read_data("calendars/us-banking.json")).expect("us-banking parses");
        assert!(fed.judges_year(1960) && !fed.judges_year(1959), "us-federal answers from 1960");
        assert!(bank.judges_year(2008) && !bank.judges_year(2007), "us-banking answers from 2008");

        {
            let (which, cal) = ("us-federal", &fed);
            let id_on = |date: CivilDateTime| {
                holiday_on(&date, cal).map(|h| h.id.clone()).unwrap_or_default()
            };

            // Washington's Birthday: 22 February through 1970, the third Monday from 1971. In 1971
            // the 22nd was itself a Monday, which is why it is asserted - a rule that had merely
            // slipped a week would still pass on the 15th alone.
            assert_eq!(id_on(d(1970, 2, 22)), "washingtons_birthday_pre_1971", "{which}");
            assert_eq!(id_on(d(1970, 2, 16)), "", "{which}");
            assert_eq!(id_on(d(1971, 2, 15)), "washingtons_birthday", "{which}");
            assert_eq!(id_on(d(1971, 2, 22)), "", "{which}");

            // Memorial Day: 30 May through 1970, the last Monday from 1971.
            assert_eq!(id_on(d(1970, 5, 30)), "memorial_day_pre_1971", "{which}");
            assert_eq!(id_on(d(1971, 5, 31)), "memorial_day", "{which}");

            // Columbus Day did not exist as a federal holiday before the Act created it.
            assert_eq!(id_on(d(1970, 10, 12)), "", "{which}");
            assert_eq!(id_on(d(1971, 10, 11)), "columbus_day", "{which}");

            // Veterans Day has three periods, and the middle one is the whole reason this test is
            // worth writing: for seven years 11 November was an ordinary working day.
            assert_eq!(id_on(d(1965, 11, 11)), "veterans_day_pre_1971", "{which}");
            assert_eq!(id_on(d(1975, 11, 11)), "", "{which}");
            assert_eq!(id_on(d(1975, 10, 27)), "veterans_day_october_monday", "{which}");
            assert_eq!(id_on(d(1978, 11, 11)), "veterans_day", "{which}");
            assert_eq!(id_on(d(2026, 11, 11)), "veterans_day", "{which}");

            // Both of its boundaries, on the exact years. The assertions above sit years away from
            // the switch, so a window off by one would pass every one of them - and this holiday has
            // TWO switches, which is twice the chance of that. 1970 is the last November before the
            // Act and 1971 the first October Monday under it, then 1977 is the last October Monday
            // and 1978 the return to 11 November.
            assert_eq!(id_on(d(1970, 11, 11)), "veterans_day_pre_1971", "{which}");
            assert_eq!(id_on(d(1971, 11, 11)), "", "{which}");
            assert_eq!(id_on(d(1971, 10, 25)), "veterans_day_october_monday", "{which}");
            assert_eq!(id_on(d(1977, 10, 24)), "veterans_day_october_monday", "{which}");
            assert_eq!(id_on(d(1977, 11, 11)), "", "{which}");
            assert_eq!(id_on(d(1978, 10, 23)), "", "{which}");
        }
    }

    /// Every weekday a calendar closes in a year, the way a holiday table lists them - weekends left out,
    /// because a table does not list them either.
    fn weekdays_off(cal: &chrono_core::calendar::Calendar, year: i64) -> Vec<(u32, u32)> {
        use chrono_core::calc::CivilDateTime;
        let mut out = Vec::new();
        for month in 1..=12u32 {
            for day in 1..=31u32 {
                let date = CivilDateTime { year, month, day, hour: 0, minute: 0, second: 0 };
                if chrono_core::calc::parse_civil_datetime(&date.to_iso()).is_err() {
                    continue; // 30 February and its kind
                }
                let weekend = chrono_core::calc::metadata(&date, &date).weekday == "Saturday"
                    || chrono_core::calc::metadata(&date, &date).weekday == "Sunday";
                if !weekend && !chrono_core::calendar::is_business_day(&date, cal) {
                    out.push((month, day));
                }
            }
        }
        out
    }

    /// R4-S20: the banking calendar against its source - the Federal Reserve's K.8 table for 2008-2012,
    /// the oldest schedule read and the first year the calendar answers for. Every weekday the Banks
    /// closed, and no other: 4 July 2009, 25 December 2010 and 1 January 2011 fell on a Saturday and
    /// moved nowhere, while a Sunday holiday closed the Monday (5 July 2010, 26 December 2011, 2 January
    /// and 12 November 2012).
    #[test]
    fn us_banking_matches_the_federal_reserve_table_from_its_first_year() {
        let bank = calendar_from_text(&read_data("calendars/us-banking.json")).expect("us-banking parses");
        let table: [(i64, &[(u32, u32)]); 5] = [
            (2008, &[(1, 1), (1, 21), (2, 18), (5, 26), (7, 4), (9, 1), (10, 13), (11, 11), (11, 27), (12, 25)]),
            (2009, &[(1, 1), (1, 19), (2, 16), (5, 25), (9, 7), (10, 12), (11, 11), (11, 26), (12, 25)]),
            (2010, &[(1, 1), (1, 18), (2, 15), (5, 31), (7, 5), (9, 6), (10, 11), (11, 11), (11, 25)]),
            (2011, &[(1, 17), (2, 21), (5, 30), (7, 4), (9, 5), (10, 10), (11, 11), (11, 24), (12, 26)]),
            (2012, &[(1, 2), (1, 16), (2, 20), (5, 28), (7, 4), (9, 3), (10, 8), (11, 12), (11, 22), (12, 25)]),
        ];
        for (year, closed) in table {
            assert_eq!(weekdays_off(&bank, year), closed.to_vec(), "{year}");
        }
    }

    /// R4-S20 and Z1 on the Polish calendar: it answers from 2002, the first full year of the five-day
    /// week in the Labour Code, and it knows 12 November 2018, a day off by its own act (Dz.U. 2018 poz.
    /// 2117) that the file used to count as a working day. The 2018 list is every weekday off that year.
    #[test]
    fn pl_answers_from_2002_and_knows_the_centenary_day_off() {
        let pl = calendar_from_text(&read_data("calendars/pl.json")).expect("pl parses");
        assert!(pl.judges_year(2002) && !pl.judges_year(2001));
        assert_eq!(
            weekdays_off(&pl, 2018),
            vec![(1, 1), (4, 2), (5, 1), (5, 3), (5, 31), (8, 15), (11, 1), (11, 12), (12, 25), (12, 26)]
        );
        // One day, one year: 12 November 2019 is a Tuesday like any other.
        assert!(!weekdays_off(&pl, 2019).contains(&(11, 12)));
    }

    /// R4-S20: the three fields that say which years a calendar answers for are required, and each is
    /// refused when it cannot be true - named, so the author of a calendar knows what to fix.
    #[test]
    fn a_calendar_says_which_years_it_answers_for() {
        let cal = |root: &str, holiday: &str| {
            format!(
                r#"{{"schema":"chronomock.calendar/1","id":"x","country":"XX","weekend":["saturday","sunday"],
                "observed":"none"{root},"holidays":[{{"id":"h","name":{{"en":"H","local":"H"}},
                "rule":{{"type":"fixed","month":7,"day":1}},"source":"t"{holiday}}}]}}"#
            )
        };
        let full = r#","valid_from":2002,"law_as_of":"2026-10-06","source":"s""#;
        let loaded = calendar_from_text(&cal(full, "")).expect("a calendar with all three loads");
        assert_eq!(loaded.valid_from, Some(2002));
        assert_eq!(loaded.law_as_of.map(|d| (d.year, d.month, d.day)), Some((2026, 10, 6)));

        for (root, field) in [
            (r#","law_as_of":"2026-10-06","source":"s""#, "valid_from"),
            (r#","valid_from":2002,"source":"s""#, "law_as_of"),
            (r#","valid_from":2002,"law_as_of":"2026-10-06""#, "source"),
        ] {
            let err = calendar_from_text(&cal(root, "")).expect_err("a required field is missing");
            assert!(err.contains(&format!("`{field}`")), "names {field}: {err}");
        }
        for (root, needle) in [
            (r#","valid_from":1582,"law_as_of":"2026-10-06","source":"s""#, "1582"),
            (r#","valid_from":2027,"law_as_of":"2026-10-06","source":"s""#, "2027"),
            (r#","valid_from":2002,"law_as_of":"2026-02-30","source":"s""#, "2026-02-30"),
            (r#","valid_from":2002,"law_as_of":"6 October 2026","source":"s""#, "6 October 2026"),
            (r#","valid_from":2002,"law_as_of":"2026-10-06T00:00:00","source":"s""#, "YYYY-MM-DD"),
            // The civil parser reads a year with a leading zero, so the length is what keeps it out.
            (r#","valid_from":2002,"law_as_of":"02026-10-06","source":"s""#, "02026-10-06"),
        ] {
            let err = calendar_from_text(&cal(root, "")).expect_err("an impossible range or date");
            assert!(err.contains(needle), "names the value: {err}");
        }
        // A holiday that ended before the calendar's first year can never apply.
        let err = calendar_from_text(&cal(full, r#","valid_to":2001"#)).expect_err("ends before the range");
        assert!(err.contains("valid_to 2001") && err.contains("valid_from 2002"), "got: {err}");
        calendar_from_text(&cal(full, r#","valid_to":2002"#)).expect("one that ends inside it loads");
    }

    #[test]
    fn catalogue_id_rejects_path_traversal() {
        // A catalogue id must never become a path escape: separators, '..', a drive colon, or empty
        // are refused before the id is turned into a file name (docs/04 4.1).
        assert!(is_valid_catalogue_id("month-end"));
        assert!(is_valid_catalogue_id("us_banking"));
        assert!(is_valid_catalogue_id("year-2038"));
        assert!(!is_valid_catalogue_id(""));
        assert!(!is_valid_catalogue_id(".."));
        assert!(!is_valid_catalogue_id("../secret"));
        assert!(!is_valid_catalogue_id("a/b"));
        assert!(!is_valid_catalogue_id("a\\b"));
        assert!(!is_valid_catalogue_id("c:evil"));
    }

    #[test]
    fn calendar_loader_maps_weekdays_and_observed() {
        assert_eq!(weekday_index("Monday").unwrap(), 1);
        assert_eq!(weekday_index("sunday").unwrap(), 0);
        assert!(weekday_index("funday").is_err());
        assert!(matches!(observed_from("sun_to_mon").unwrap(), chrono_core::calendar::Observed::SunToMon));
        assert!(observed_from("whenever").is_err());
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;
        use std::io::Read;

        /// The largest catalogue file, written out rather than read from `MAX_CATALOGUE_BYTES`, so a
        /// limit moved by mistake is a red case instead of an answer that moves with it.
        const LIMIT: u64 = 4 * 1024 * 1024;

        /// Bytes that are not UTF-8 wherever they stand: two bytes no UTF-8 text holds, an overlong
        /// encoding, a surrogate, a code point past U+10FFFF and a continuation byte with no lead.
        const BROKEN: [&[u8]; 6] = [&[0xFF], &[0xFE], &[0xC0, 0x80], &[0xED, 0xA0, 0x80], &[0xF4, 0x90, 0x80, 0x80], &[0x80]];

        /// A piece of a catalogue file: text, which is what it says, or bytes that are not text.
        #[derive(Debug, Clone)]
        enum Piece {
            Text(String),
            Broken(&'static [u8]),
        }

        /// How a file begins: as its pieces do, with the UTF-8 byte order mark once or twice, or with
        /// the UTF-16 one that a Windows editor writes for "Unicode".
        #[derive(Debug, Clone, Copy)]
        enum Start {
            Plain,
            Marks(usize),
            Wide,
        }

        /// The spaces after the pieces: a few, or as many as bring the file to the limit plus `k`.
        #[derive(Debug, Clone, Copy)]
        enum Filler {
            Few(u64),
            ToLimit(i64),
        }

        /// The size the file says it has: its own length and the bytes either side, or any number.
        #[derive(Debug, Clone, Copy)]
        enum Said {
            Truth(i64),
            Exactly(u64),
        }

        /// How the reading goes: smoothly, interrupted before every piece (which a reader retries),
        /// or failing at a byte - the first, the last, or any.
        #[derive(Debug, Clone, Copy)]
        enum Fault {
            Never,
            Interrupted,
            FailsAt(u64),
        }

        /// A catalogue file as bytes, described rather than held: the filler can be four megabytes,
        /// and a failing case prints this, not them.
        #[derive(Debug, Clone)]
        struct File {
            start: Start,
            pieces: Vec<Piece>,
            filler: Filler,
            /// The lead byte of a three-byte character and one of its continuations, as the very last
            /// thing in the file. Only last: a continuation drawn after it would complete it.
            cut_lead: bool,
            said: Said,
            /// The most a single read hands over.
            chunk: usize,
            fault: Fault,
        }

        impl File {
            fn head(&self) -> Vec<u8> {
                let mut bytes = match self.start {
                    Start::Plain => Vec::new(),
                    Start::Marks(n) => [0xEF, 0xBB, 0xBF].repeat(n),
                    Start::Wide => vec![0xFF, 0xFE],
                };
                for piece in &self.pieces {
                    match piece {
                        Piece::Text(text) => bytes.extend_from_slice(text.as_bytes()),
                        Piece::Broken(broken) => bytes.extend_from_slice(broken),
                    }
                }
                bytes
            }

            fn tail(&self) -> &'static [u8] {
                if self.cut_lead { &[0xE2, 0x82] } else { &[] }
            }

            fn filler(&self) -> u64 {
                let around = (self.head().len() + self.tail().len()) as u64;
                match self.filler {
                    Filler::Few(n) => n,
                    Filler::ToLimit(k) => LIMIT.saturating_add_signed(k).saturating_sub(around),
                }
            }

            fn length(&self) -> u64 {
                (self.head().len() + self.tail().len()) as u64 + self.filler()
            }

            fn said(&self) -> u64 {
                match self.said {
                    Said::Truth(by) => self.length().saturating_add_signed(by),
                    Said::Exactly(n) => n,
                }
            }

            /// The byte the read fails at, when it fails at one.
            fn fails_at(&self) -> Option<u64> {
                match (self.fault, self.length()) {
                    (Fault::FailsAt(_), 0) | (Fault::Never | Fault::Interrupted, _) => None,
                    (Fault::FailsAt(u64::MAX), length) => Some(length - 1),
                    (Fault::FailsAt(n), length) => Some(n % length),
                }
            }

            /// Whether every byte belongs to a character, as the model was drawn - not as a decoder
            /// would say.
            fn is_text(&self) -> bool {
                !matches!(self.start, Start::Wide) && !self.cut_lead && self.pieces.iter().all(|p| matches!(p, Piece::Text(_)))
            }

            /// What the file says, less one leading byte order mark: a mark is not part of the text,
            /// and a second one is (RFC 8259 section 8.1 allows a reader to skip one).
            fn text(&self) -> String {
                let mut text = match self.start {
                    Start::Marks(n) => char::from_u32(0xFEFF).expect("a scalar value").to_string().repeat(n),
                    Start::Plain | Start::Wide => String::new(),
                };
                for piece in &self.pieces {
                    if let Piece::Text(piece) = piece {
                        text.push_str(piece);
                    }
                }
                text.push_str(&" ".repeat(usize::try_from(self.filler()).expect("a filler fits in memory")));
                match text.strip_prefix(char::from_u32(0xFEFF).expect("a scalar value")) {
                    Some(rest) => rest.to_string(),
                    None => text,
                }
            }

            fn reader(&self) -> Awkward<impl Read> {
                let bytes = std::io::Cursor::new(self.head())
                    .chain(std::io::repeat(b' ').take(self.filler()))
                    .chain(self.tail());
                // A megabyte read a byte at a time would take minutes in a debug build, and says no
                // more than a small file read that way.
                let chunk = if self.length() > 1 << 16 { self.chunk.max(1 << 16) } else { self.chunk };
                Awkward {
                    bytes,
                    chunk,
                    interrupts: matches!(self.fault, Fault::Interrupted),
                    interrupted: false,
                    fails_at: self.fails_at(),
                    given: 0,
                }
            }
        }

        /// A reader that hands over at most `chunk` bytes a call, is interrupted before every piece
        /// when asked to, and fails at a byte when asked to.
        struct Awkward<R> {
            bytes: R,
            chunk: usize,
            interrupts: bool,
            interrupted: bool,
            fails_at: Option<u64>,
            given: u64,
        }

        impl<R: Read> Read for Awkward<R> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.interrupts && !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                self.interrupted = false;
                let mut room = buffer.len().min(self.chunk);
                if let Some(at) = self.fails_at {
                    if self.given >= at {
                        return Err(std::io::Error::other("the disk went away"));
                    }
                    room = room.min(usize::try_from(at - self.given).unwrap_or(usize::MAX));
                }
                let read = self.bytes.read(&mut buffer[..room])?;
                self.given += read as u64;
                Ok(read)
            }
        }

        fn piece() -> impl Strategy<Value = Piece> {
            // A character of two, three and four bytes, the byte order mark inside the text, and a
            // combining mark - all text, written as numbers so this source holds only ASCII.
            let character = prop::sample::select(vec![0x015B_u32, 0x20AC, 0x1F600, 0xFEFF, 0x0301])
                .prop_map(|code| Piece::Text(char::from_u32(code).expect("a scalar value").to_string()));
            prop_oneof![
                4 => prop::sample::select(vec!["{", "}", "\"id\": \"pl\"", " ", "\n"]).prop_map(|t| Piece::Text(t.to_string())),
                3 => character,
                2 => prop::sample::select(BROKEN.to_vec()).prop_map(Piece::Broken),
            ]
        }

        fn file() -> impl Strategy<Value = File> {
            let start = prop_oneof![4 => Just(Start::Plain), 2 => (1usize..=2).prop_map(Start::Marks), 1 => Just(Start::Wide)];
            let filler = prop_oneof![3 => (0u64..=64).prop_map(Filler::Few), 1 => (-2i64..=2).prop_map(Filler::ToLimit)];
            let said = prop_oneof![
                4 => (-3i64..=3).prop_map(Said::Truth),
                1 => prop::sample::select(vec![0, LIMIT - 1, LIMIT, LIMIT + 1, u64::MAX]).prop_map(Said::Exactly),
            ];
            let fault = prop_oneof![
                4 => Just(Fault::Never),
                1 => Just(Fault::Interrupted),
                1 => prop_oneof![Just(0u64), Just(u64::MAX), any::<u64>()].prop_map(Fault::FailsAt),
            ];
            let chunk = prop::sample::select(vec![1usize, 3, 4096, usize::MAX]);
            (start, prop::collection::vec(piece(), 0..8), filler, prop::bool::weighted(0.1), said, chunk, fault).prop_map(
                |(start, pieces, filler, cut_lead, said, chunk, fault)| File { start, pieces, filler, cut_lead, said, chunk, fault },
            )
        }

        proptest! {
            /// R4-N37 and R5-B9 item 6. A catalogue file is read whole as text exactly when it said a
            /// size within the limit, holds no more than the limit, could be read to its end and is
            /// UTF-8 - and then as what it says, less one leading byte order mark. Every other file is
            /// refused, by the first of those it fails, and the refusal says which.
            #[test]
            fn a_catalogue_file_is_its_text_or_a_refusal_that_says_why(file in file()) {
                let path = std::path::Path::new("drawn.json");
                let read = read_catalogue_text(file.reader(), file.said(), path);
                let reached = file.length().min(LIMIT + 1);
                let refusal = |read: &Result<String, String>| read.as_ref().err().cloned().unwrap_or_default();
                if file.said() > LIMIT {
                    let owed = format!("drawn.json is {} bytes, past the {LIMIT}-byte limit", file.said());
                    prop_assert!(refusal(&read).contains(&owed), "{:?}: {:?}", file.said(), read.map(|t| t.len()));
                } else if file.fails_at().is_some_and(|at| at < reached) {
                    prop_assert!(refusal(&read).starts_with("cannot read drawn.json: the disk went away"), "{:?}", read.map(|t| t.len()));
                } else if file.length() > LIMIT {
                    prop_assert!(refusal(&read).contains("grew past the"), "{:?}", read.map(|t| t.len()));
                } else if !file.is_text() {
                    prop_assert!(refusal(&read).contains("it is not UTF-8 text"), "{:?}", read.map(|t| t.len()));
                } else {
                    let text = read.map_err(|e| TestCaseError::fail(format!("refused a text file: {e}")))?;
                    let owed = file.text();
                    prop_assert!(text == owed, "read {} bytes, owed {}: {:?} against {:?}", text.len(), owed.len(),
                        text.chars().take(16).collect::<String>(), owed.chars().take(16).collect::<String>());
                }
            }
        }

        /// Characters that do not show as themselves, by name and written as numbers: both ends of
        /// the control ranges and the ones a terminal acts on, the format characters of the measured
        /// attacks, the two separators and format characters past the first plane.
        const HIDDEN: [u32; 30] = [
            0x00, 0x09, 0x0A, 0x0D, 0x1B, 0x1F, 0x7F, 0x80, 0x9B, 0x9F, 0xAD, 0x061C, 0x200B, 0x200E, 0x200F,
            0x202A, 0x202E, 0x2060, 0x2064, 0x2066, 0x206F, 0x2028, 0x2029, 0xFEFF, 0xFFF9, 0xFFFB, 0x1D173,
            0xE0001, 0xE0020, 0xE007F,
        ];

        /// Characters that show: ASCII with the two JSON has to escape, letters of the catalogues'
        /// languages, a combining mark, an emoji, and the neighbours of the hidden ranges.
        const SHOWN: [u32; 18] = [
            0x41, 0x7A, 0x20, 0x22, 0x5C, 0x7E, 0x0105, 0x015B, 0x0436, 0x6708, 0x0301, 0x1F600, 0xAC, 0xAE, 0x200A,
            0x2010, 0x2070, 0xFEFE,
        ];

        /// Keys a catalogue file is read by, so a drawn tree also reaches the fields a typed read
        /// quotes.
        const FIELDS: [&str; 12] =
            ["schema", "id", "name", "en", "local", "weekend", "holidays", "rule", "applies_to", "moment", "base", "steps"];

        fn is_hidden(c: char) -> bool {
            HIDDEN.contains(&(c as u32))
        }

        fn drawn_text(hidden_weight: u32) -> impl Strategy<Value = String> {
            let character = prop_oneof![
                8 => prop::sample::select(SHOWN.to_vec()),
                hidden_weight => prop::sample::select(HIDDEN.to_vec()),
            ];
            prop::collection::vec(character, 0..6)
                .prop_map(|codes| codes.into_iter().map(|code| char::from_u32(code).expect("a scalar value")).collect())
        }

        /// A JSON document as a catalogue file may hold one.
        #[derive(Debug, Clone)]
        enum Tree {
            Text(String),
            Number(i64),
            Flag(bool),
            Nothing,
            List(Vec<Tree>),
            Object(std::collections::BTreeMap<String, Tree>),
        }

        fn tree() -> impl Strategy<Value = Tree> {
            let leaf = prop_oneof![
                4 => drawn_text(1).prop_map(Tree::Text),
                1 => any::<i64>().prop_map(Tree::Number),
                1 => any::<bool>().prop_map(Tree::Flag),
                1 => Just(Tree::Nothing),
            ];
            let key = prop_oneof![2 => prop::sample::select(FIELDS.to_vec()).prop_map(str::to_string), 1 => drawn_text(1)];
            leaf.prop_recursive(4, 32, 4, move |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..4).prop_map(Tree::List),
                    prop::collection::btree_map(key.clone(), inner, 0..4).prop_map(Tree::Object),
                ]
            })
        }

        /// A text as JSON writes it: the quote, the backslash and every control character escaped,
        /// and with `ascii` every other character outside ASCII too, past the first plane as two
        /// UTF-16 halves - the way a shared file may carry it.
        fn write_text(text: &str, ascii: bool, out: &mut String) {
            out.push('"');
            for c in text.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    c if (c as u32) < 0x20 || (ascii && !c.is_ascii()) => {
                        let mut halves = [0u16; 2];
                        for half in c.encode_utf16(&mut halves) {
                            out.push_str(&format!("\\u{half:04x}"));
                        }
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
        }

        fn write_tree(tree: &Tree, ascii: bool, out: &mut String) {
            match tree {
                Tree::Text(text) => write_text(text, ascii, out),
                Tree::Number(n) => out.push_str(&n.to_string()),
                Tree::Flag(flag) => out.push_str(&flag.to_string()),
                Tree::Nothing => out.push_str("null"),
                Tree::List(items) => {
                    out.push('[');
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        write_tree(item, ascii, out);
                    }
                    out.push(']');
                }
                Tree::Object(map) => {
                    out.push('{');
                    for (i, (key, item)) in map.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        write_text(key, ascii, out);
                        out.push(':');
                        write_tree(item, ascii, out);
                    }
                    out.push('}');
                }
            }
        }

        /// Where a hidden character stands, as a refusal names it: the place, the code point and the
        /// position in characters from 1 - for every text and key of `tree` that holds one.
        fn hidden_places(tree: &Tree, path: &str, found: &mut Vec<String>) {
            let place = if path.is_empty() { "at the top level".to_string() } else { format!("in `{path}`") };
            let first_hidden = |text: &str| text.chars().enumerate().find(|&(_, c)| is_hidden(c)).map(|(at, c)| (at + 1, c as u32));
            match tree {
                Tree::Text(text) => {
                    if let Some((at, code)) = first_hidden(text) {
                        found.push(format!("the text {place} holds U+{code:04X} at character {at} "));
                    }
                }
                Tree::List(items) => {
                    for (i, item) in items.iter().enumerate() {
                        hidden_places(item, &format!("{path}[{i}]"), found);
                    }
                }
                Tree::Object(map) => {
                    for (key, item) in map {
                        if let Some((at, code)) = first_hidden(key) {
                            found.push(format!("a key {place} holds U+{code:04X} at character {at} "));
                        }
                        let child = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
                        hidden_places(item, &child, found);
                    }
                }
                Tree::Number(_) | Tree::Flag(_) | Tree::Nothing => {}
            }
        }

        proptest! {
            /// R5-B1. A catalogue file holding a character that does not show as itself, in any text or
            /// key at any depth, written as itself or as an escape, is refused by its place, its code
            /// point and its position - and a file without one is not. The place named is one where
            /// such a character stands, whichever the walk meets first.
            ///
            /// And no refusal of a calendar or a preset carries such a character as itself: a file cut
            /// short is not JSON, and one with a key given twice hides its first value from the walk, so
            /// what the typed read quotes from them is written as codes.
            #[test]
            fn a_hidden_character_anywhere_is_refused_by_its_place_and_never_repeated(
                tree in tree(),
                ascii in any::<bool>(),
                cut in any::<prop::sample::Index>(),
                hidden in drawn_text(3),
            ) {
                let mut text = String::new();
                write_tree(&tree, ascii, &mut text);
                let mut places = Vec::new();
                hidden_places(&tree, "", &mut places);
                match refuse_hidden_characters("drawn", &text) {
                    Ok(()) => prop_assert!(places.is_empty(), "{:?} passed, holding {:?}", text, places),
                    Err(refusal) => prop_assert!(
                        places.iter().any(|place| refusal.contains(place.as_str())),
                        "{:?}: {} - the places that hold one: {:?}", text, refusal, places
                    ),
                }

                let ends: Vec<usize> = text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
                let short = text[..ends[cut.index(ends.len())]].to_string();
                // The first `name` is a text where both readers want an object, so the typed read
                // quotes it - and only the second is in the document the walk sees.
                let mut twice = String::from(r#"{"schema":"chronomock.calendar/1","name":"#);
                write_text(&hidden, ascii, &mut twice);
                twice.push_str(r#","name":"#);
                twice.push_str(&text);
                twice.push('}');
                for file in [&short, &twice] {
                    let refusals = [calendar_from_text(file).err(), crate::preset::parse_preset(file).err().map(|e| e.message().to_string())];
                    for refusal in refusals.into_iter().flatten() {
                        prop_assert!(!refusal.chars().any(is_hidden), "{:?} is refused with a hidden character: {:?}", file, refusal);
                    }
                }
            }
        }

        /// What a field of a catalogue file holds, as docs/04 writes the two schemas - restated here as
        /// the contract, not read from the DTOs it checks.
        #[derive(Debug, Clone, Copy, PartialEq)]
        enum Holds {
            /// The file itself: an object, and a list there is refused like any other kind.
            File,
            Text,
            /// A whole number in this range.
            Number(i128, i128),
            Flag,
            List,
            Object,
            /// A preset's base: a keyword or an object.
            TextOrObject,
            /// A preset step: an object with one key.
            Step,
            /// A default, a text in a language other than English, a field a preset does not read.
            Anything,
        }

        const I64: Holds = Holds::Number(i64::MIN as i128, i64::MAX as i128);
        const I32: Holds = Holds::Number(i32::MIN as i128, i32::MAX as i128);
        const U32: Holds = Holds::Number(0, u32::MAX as i128);

        /// The calendar schema (docs/04 section 5), field by field, with `[]` for any item of a list.
        /// `None` for a path the schema does not have - a calendar refuses every unknown field.
        fn calendar_field(path: &[&str]) -> Option<(Holds, bool)> {
            use Holds::{File, List, Object, Text};
            Some(match path {
                [] => (File, false),
                ["schema" | "id" | "country" | "observed" | "law_as_of" | "source"] => (Text, false),
                ["stability" | "subregion"] => (Text, true),
                ["name"] => (Object, true),
                ["name", "en" | "local"] | ["holidays", "[]", "name", "en" | "local"] => (Text, false),
                ["weekend"] | ["holidays"] => (List, false),
                ["weekend", "[]"] => (Text, false),
                ["valid_from"] => (I64, false),
                ["holidays", "[]"] | ["holidays", "[]", "name" | "rule"] => (Object, false),
                ["holidays", "[]", "id" | "source"] | ["holidays", "[]", "rule", "type" | "weekday"] => (Text, false),
                ["holidays", "[]", "rule", "month" | "day"] => (U32, false),
                ["holidays", "[]", "rule", "order" | "offset"] => (I32, false),
                ["holidays", "[]", "valid_from" | "valid_to"] => (I64, true),
                _ => return None,
            })
        }

        /// The preset schema (docs/04 section 4.2). A preset reads past the fields it does not know,
        /// so everything outside the table holds anything.
        fn preset_field(path: &[&str]) -> (Holds, bool) {
            use Holds::{Anything, File, Flag, List, Object, Step, Text, TextOrObject};
            match path {
                [] => (File, false),
                ["schema" | "id" | "applies_to"] => (Text, false),
                ["name" | "explains"] | ["moment"] | ["parameters", "[]"] => (Object, false),
                ["name" | "explains", "en"] | ["parameters", "[]", "label", "en"] => (Text, false),
                ["parameters"] | ["moment", "steps"] => (List, false),
                ["parameters", "[]", "id" | "type"] => (Text, false),
                ["parameters", "[]", "label"] | ["time_mode"] => (Object, true),
                ["parameters", "[]", "default_hint"] | ["market"] => (Text, true),
                ["moment", "base"] => (TextOrObject, false),
                ["moment", "base", "absolute" | "absolute_utc" | "parameter"] => (Text, true),
                ["moment", "steps", "[]"] => (Step, false),
                ["moment", "steps", "[]", "shift"] => (Object, false),
                ["moment", "steps", "[]", "shift", "sign" | "unit" | "parameter"] => (Text, true),
                ["moment", "steps", "[]", "shift", "amount"] | ["time_mode", "multiplier"] => (I64, true),
                ["moment", "steps", "[]", "set_time" | "snap" | "nearest" | "zone"] => (Text, false),
                ["time_mode", "scale_duration_clock"] => (Flag, false),
                _ => (Anything, false),
            }
        }

        /// Whether a reader of this field has to refuse `value`. A null where the field may be left out
        /// is the field left out. A list where a nested object belongs is read by position (the note on
        /// `ObjectOnly`), so nothing is claimed for it, while the file itself as a list is refused.
        fn must_refuse(holds: Holds, optional: bool, value: &serde_json::Value) -> bool {
            use serde_json::Value;
            if holds == Holds::Anything || (optional && value.is_null()) {
                return false;
            }
            match (holds, value) {
                (Holds::Object | Holds::TextOrObject | Holds::Step, Value::Array(_)) => false,
                (Holds::File | Holds::Object | Holds::TextOrObject | Holds::Step, Value::Object(_))
                | (Holds::Text | Holds::TextOrObject, Value::String(_))
                | (Holds::Flag, Value::Bool(_))
                | (Holds::List, Value::Array(_)) => false,
                (Holds::Number(low, high), Value::Number(n)) => {
                    let whole = n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from));
                    !whole.is_some_and(|n| (low..=high).contains(&n))
                }
                _ => true,
            }
        }

        /// What serde says in the words of the Rust it builds - none of it may reach the person who
        /// wrote the file (finding (g) of PR A). A variant is the Rust name of a rule type and of a step,
        /// and a word of the preset schema too (a parameter type, `unknown variant 'x'` of its label), so
        /// only serde's phrases are listed - serde quotes a name with backticks.
        const RUST_WORDS: [&str; 18] = [
            "Dto", "struct ", "enum ", "i64", "i32", "u32", "u64", "sequence", "map", "invalid length", "untagged",
            "newtype", "unknown variant `", "unit variant", "variant identifier", "variant of", "Option", "unit value",
        ];

        /// What a JSON parser says of text that is not JSON. Every file here is JSON, so a refusal in
        /// these words sends its author looking for a missing comma that is not there.
        const NOT_JSON_WORDS: [&str; 6] =
            ["expected value", "EOF while parsing", "trailing c", "key must be a string", "expected `,`", "expected `:`"];

        /// A value of each kind JSON has, with the edges of every width a field is read in.
        fn replacement() -> impl Strategy<Value = serde_json::Value> {
            let past_i64_low: serde_json::Value = serde_json::from_str("-9223372036854775809").expect("a number");
            prop::sample::select(vec![
                serde_json::json!(""),
                serde_json::json!("x"),
                serde_json::json!("2002"),
                serde_json::json!(0),
                serde_json::json!(-1),
                serde_json::json!(13),
                serde_json::json!(2_147_483_648_i64),
                serde_json::json!(4_294_967_296_i64),
                serde_json::json!(9_223_372_036_854_775_808_u64),
                past_i64_low,
                serde_json::json!(1.5),
                serde_json::json!(2.0),
                serde_json::json!(true),
                serde_json::json!(null),
                serde_json::json!([]),
                serde_json::json!(["x"]),
                serde_json::json!([1]),
                serde_json::json!({}),
            ])
        }

        /// Every catalogue file that ships, read once: whether it is a calendar, and its document.
        fn shipped() -> &'static [(bool, serde_json::Value)] {
            static SHIPPED: std::sync::OnceLock<Vec<(bool, serde_json::Value)>> = std::sync::OnceLock::new();
            SHIPPED.get_or_init(|| {
                let mut files = Vec::new();
                for (folder, calendar) in [("calendars", true), ("presets", false)] {
                    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(folder);
                    let mut paths: Vec<_> = std::fs::read_dir(&dir).expect("a shipped folder").map(|e| e.expect("an entry").path()).collect();
                    paths.sort();
                    for path in paths.into_iter().filter(|p| p.extension().is_some_and(|x| x == "json")) {
                        let text = std::fs::read_to_string(&path).expect("a shipped file");
                        files.push((calendar, serde_json::from_str(&text).expect("a shipped file is JSON")));
                    }
                }
                assert!(files.len() >= 17, "the shipped catalogue looks empty: {}", files.len());
                files
            })
        }

        /// Every place in `value`, as the keys and list positions that reach it.
        fn places(value: &serde_json::Value, path: &mut Vec<String>, found: &mut Vec<Vec<String>>) {
            found.push(path.clone());
            let children: Vec<(String, &serde_json::Value)> = match value {
                serde_json::Value::Array(items) => items.iter().enumerate().map(|(i, item)| (i.to_string(), item)).collect(),
                serde_json::Value::Object(map) => map.iter().map(|(key, item)| (key.clone(), item)).collect(),
                _ => Vec::new(),
            };
            for (step, child) in children {
                path.push(step);
                places(child, path, found);
                path.pop();
            }
        }

        /// The place written as the schema tables read it: a list position as `[]`.
        fn schema_path<'a>(document: &serde_json::Value, path: &'a [String]) -> Vec<&'a str> {
            let mut at = document;
            let mut words = Vec::new();
            for step in path {
                if let serde_json::Value::Array(items) = at {
                    words.push("[]");
                    at = &items[step.parse::<usize>().expect("a list position")];
                } else {
                    words.push(step.as_str());
                    at = &at[step.as_str()];
                }
            }
            words
        }

        fn replace(document: &mut serde_json::Value, path: &[String], value: serde_json::Value) {
            let mut at = document;
            for step in path {
                at = match at {
                    serde_json::Value::Array(items) => &mut items[step.parse::<usize>().expect("a list position")],
                    other => &mut other[step.as_str()],
                };
            }
            *at = value;
        }

        proptest! {
            /// Finding (g) of PR A, for any place in any shipped file. A value of another kind than the
            /// field holds, or a number past the width it is read in, is refused - and no refusal of a
            /// catalogue file speaks of Rust types, while one that names a place in the file names a
            /// line and a column an editor shows.
            #[test]
            fn a_wrong_kind_anywhere_is_refused_in_the_files_own_words(
                which in any::<prop::sample::Index>(),
                at in any::<prop::sample::Index>(),
                value in replacement(),
            ) {
                let (calendar, original) = which.get(shipped());
                let mut found = Vec::new();
                places(original, &mut Vec::new(), &mut found);
                let path = at.get(&found);
                let words = schema_path(original, path);
                let (holds, optional) = if *calendar {
                    calendar_field(&words).ok_or_else(|| TestCaseError::fail(format!("{words:?} is not in the calendar schema table")))?
                } else {
                    preset_field(&words)
                };
                let mut document = original.clone();
                replace(&mut document, path, value.clone());
                let text = document.to_string();
                let (kind, read) = if *calendar {
                    ("calendar", calendar_from_text(&text).err())
                } else {
                    ("preset", crate::preset::parse_preset(&text).err().map(|e| e.message().to_string()))
                };
                if must_refuse(holds, optional, &value) {
                    prop_assert!(read.is_some(), "{}: {:?} at {:?} ({:?}) was read", kind, value, words, holds);
                }
                let Some(refusal) = read else { return Ok(()) };
                for word in RUST_WORDS.iter().chain(&NOT_JSON_WORDS) {
                    prop_assert!(!refusal.contains(word), "{} at {:?}: says {:?} - {}", kind, words, word, refusal);
                }
                if let Some(rest) = refusal.strip_prefix(&format!("bad {kind} JSON")) {
                    let place = rest.split_once(": ").map_or(rest, |(place, _)| place);
                    let column = place.strip_prefix(" at line ").and_then(|p| p.split_once(", column ")).map(|(_, c)| c);
                    prop_assert!(
                        place.is_empty() || column.and_then(|c| c.parse::<u64>().ok()).is_some_and(|c| c >= 1),
                        "{} at {:?}: the place is not one an editor shows - {}", kind, words, refusal
                    );
                }
            }
        }

        const WEEKDAYS: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];

        /// How long each month runs in a leap year - the longest it runs in any year, which is the
        /// bound a fixed holiday is held to (February 29 is a holiday a calendar may name, R2-N7).
        const LONGEST: [u32; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

        const OBSERVANCES: [(&str, chrono_core::calendar::Observed); 4] = [
            ("none", chrono_core::calendar::Observed::None),
            ("sat_to_fri_sun_to_mon", chrono_core::calendar::Observed::SatToFriSunToMon),
            ("sun_to_mon", chrono_core::calendar::Observed::SunToMon),
            ("weekend_to_mon", chrono_core::calendar::Observed::WeekendToMon),
        ];

        /// Saturday and Sunday, the one weekend an observance rule is defined for (R2-N8).
        const SATURDAY_SUNDAY: u8 = 0b100_0001;

        fn days_in(year: i64, month: u32) -> u32 {
            let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
            if month == 2 && !leap { 28 } else { LONGEST[month as usize - 1] }
        }

        /// A day's name as an author may write it: lower case, capitalised or in capitals.
        fn written(day: u32, case: u8) -> String {
            let name = WEEKDAYS[day as usize];
            match case % 3 {
                0 => name.to_string(),
                1 => name[..1].to_uppercase() + &name[1..],
                _ => name.to_uppercase(),
            }
        }

        #[derive(Debug, Clone)]
        enum DrawnRule {
            Fixed(u32, u32),
            Nth { month: u32, weekday: u32, case: u8, order: i32 },
            Easter(i32),
        }

        #[derive(Debug, Clone)]
        struct DrawnHoliday {
            rule: DrawnRule,
            from: Option<i64>,
            to: Option<i64>,
        }

        /// A calendar as its author writes it, every field within what the schema allows.
        #[derive(Debug, Clone)]
        struct DrawnCalendar {
            /// Each day off with the way its name is written - repeated, in any order.
            weekend: Vec<(u32, u8)>,
            observed: usize,
            valid_from: i64,
            law: (i64, u32, u32),
            holidays: Vec<DrawnHoliday>,
            /// With the contract's fields this reader does not use: a title, a stability, a subregion.
            titled: bool,
        }

        impl DrawnCalendar {
            fn json(&self) -> serde_json::Value {
                let holidays: Vec<serde_json::Value> = self.holidays.iter().enumerate().map(|(i, h)| holiday_json(i, h)).collect();
                let (year, month, day) = self.law;
                let mut calendar = serde_json::json!({
                    "schema": "chronomock.calendar/1",
                    "id": "drawn",
                    "country": "XX",
                    "weekend": self.weekend.iter().map(|&(day, case)| written(day, case)).collect::<Vec<_>>(),
                    "observed": OBSERVANCES[self.observed].0,
                    "valid_from": self.valid_from,
                    "law_as_of": format!("{year:04}-{month:02}-{day:02}"),
                    "source": "drawn",
                    "holidays": holidays,
                });
                if self.titled {
                    calendar["name"] = serde_json::json!({ "en": "Drawn", "local": "Drawn" });
                    calendar["stability"] = "unstable".into();
                    calendar["subregion"] = serde_json::Value::Null;
                }
                calendar
            }

            /// The calendar this file describes, worked out from the model.
            fn calendar(&self) -> chrono_core::calendar::Calendar {
                use chrono_core::calendar::{Calendar, Holiday, HolidayRule};
                let mut weekend: Vec<u32> = self.weekend.iter().map(|&(day, _)| day).collect();
                weekend.sort_unstable();
                weekend.dedup();
                let (year, month, day) = self.law;
                let holidays = self.holidays.iter().enumerate().map(|(i, h)| Holiday {
                    id: format!("h{i}"),
                    name_en: format!("Holiday {i}"),
                    name_local: format!("{}wi{}to {i}", char::from_u32(0x015A).expect("a letter"), char::from_u32(0x0119).expect("a letter")),
                    rule: match h.rule {
                        DrawnRule::Fixed(month, day) => HolidayRule::Fixed { month, day },
                        DrawnRule::Nth { month, weekday, order, .. } => HolidayRule::NthWeekday { month, weekday, order },
                        DrawnRule::Easter(offset) => HolidayRule::EasterOffset { offset },
                    },
                    valid_from: h.from,
                    valid_to: h.to,
                    source: format!("source {i}"),
                });
                Calendar {
                    id: "drawn".into(),
                    country: "XX".into(),
                    weekend,
                    observed: OBSERVANCES[self.observed].1,
                    valid_from: Some(self.valid_from),
                    law_as_of: Some(chrono_core::calc::CivilDateTime { year, month, day, hour: 0, minute: 0, second: 0 }),
                    holidays: holidays.collect(),
                }
            }
        }

        fn rule_json(rule: &DrawnRule) -> serde_json::Value {
            match *rule {
                DrawnRule::Fixed(month, day) => serde_json::json!({ "type": "fixed", "month": month, "day": day }),
                DrawnRule::Nth { month, weekday, case, order } => {
                    serde_json::json!({ "type": "nth_weekday", "month": month, "weekday": written(weekday, case), "order": order })
                }
                DrawnRule::Easter(offset) => serde_json::json!({ "type": "easter_offset", "offset": offset }),
            }
        }

        fn holiday_json(i: usize, holiday: &DrawnHoliday) -> serde_json::Value {
            let mut json = serde_json::json!({
                "id": format!("h{i}"),
                "name": { "en": format!("Holiday {i}"), "local": format!("{}wi{}to {i}", char::from_u32(0x015A).expect("a letter"), char::from_u32(0x0119).expect("a letter")) },
                "rule": rule_json(&holiday.rule),
                "source": format!("source {i}"),
            });
            if let Some(from) = holiday.from {
                json["valid_from"] = from.into();
            }
            if let Some(to) = holiday.to {
                json["valid_to"] = to.into();
            }
            json
        }

        fn drawn_rule() -> impl Strategy<Value = DrawnRule> {
            let fixed = (1u32..=12, any::<prop::sample::Index>(), any::<bool>()).prop_map(|(month, at, last)| {
                let longest = LONGEST[month as usize - 1];
                DrawnRule::Fixed(month, if last { longest } else { 1 + at.index(longest as usize) as u32 })
            });
            let nth = (1u32..=12, 0u32..7, any::<u8>(), prop_oneof![Just(-1i32), 1i32..=5])
                .prop_map(|(month, weekday, case, order)| DrawnRule::Nth { month, weekday, case, order });
            let easter = prop_oneof![Just(-80i32), Just(250), -80i32..=250].prop_map(DrawnRule::Easter);
            prop_oneof![fixed, nth, easter]
        }

        /// A holiday whose years fit the calendar's: a window that does not end before `first`, the
        /// calendar's first year, and does not end before it begins.
        fn drawn_holiday(first: i64) -> impl Strategy<Value = DrawnHoliday> {
            (drawn_rule(), prop::option::of(first - 40..=first + 40)).prop_flat_map(move |(rule, from)| {
                let lower = from.unwrap_or(first).max(first);
                prop::option::of(prop_oneof![Just(lower), lower..=lower + 40])
                    .prop_map(move |to| DrawnHoliday { rule: rule.clone(), from, to })
            })
        }

        fn drawn_calendar() -> impl Strategy<Value = DrawnCalendar> {
            let law = (prop_oneof![1583i64..=1600, 1900i64..=2100, 9990i64..=9999], 1u32..=12, any::<prop::sample::Index>())
                .prop_map(|(year, month, at)| (year, month, 1 + at.index(days_in(year, month) as usize) as u32));
            // Any weekend with a working day in it, Saturday and Sunday more often than the rest.
            let days_off = prop_oneof![2 => Just(SATURDAY_SUNDAY), 1 => 0u8..0x7F];
            (law, days_off, prop::collection::vec(any::<u8>(), 7), any::<bool>(), any::<bool>()).prop_flat_map(
                |(law, days_off, cases, repeated, titled)| {
                    let mut weekend: Vec<(u32, u8)> = (0..7u32).filter(|d| days_off & (1 << d) != 0).map(|d| (d, cases[d as usize])).collect();
                    if repeated && let Some(&(day, case)) = weekend.first() {
                        weekend.push((day, case.wrapping_add(1)));
                    }
                    let observed = if days_off == SATURDAY_SUNDAY { (0usize..4).boxed() } else { Just(0usize).boxed() };
                    let first = prop_oneof![Just(1583i64), Just(law.0), 1583i64..=law.0];
                    (Just(weekend).prop_shuffle(), observed, first).prop_flat_map(move |(weekend, observed, valid_from)| {
                        prop::collection::vec(drawn_holiday(valid_from), 0..=5).prop_map(move |holidays| DrawnCalendar {
                            weekend: weekend.clone(),
                            observed,
                            valid_from,
                            law,
                            holidays,
                            titled,
                        })
                    })
                },
            )
        }

        /// One thing wrong with a calendar, by name - each a refusal this reader owes, with its history.
        #[derive(Debug, Clone)]
        enum Flaw {
            /// S-1: a week with no working day.
            AllWeek,
            /// R2-N8: an observance rule over a weekend other than Saturday and Sunday.
            ForeignObserved { rule: usize, days_off: u8 },
            UnknownDay(&'static str),
            UnknownObserved(&'static str),
            /// S-17 and R2-N7: a month or a day past its range, an order past the fifth, and R4-N39: an
            /// Easter offset that leaves the year of its Easter.
            Month(u32),
            Day { month: u32, zero: bool },
            Order(i32),
            Easter(i32),
            SameId,
            Inverted,
            EndsEarly,
            /// R4-S20: a first year before the Gregorian calendar or after the law was read.
            FirstYear(i64),
            LawNotADate(&'static str),
            /// R4-N38: an unknown field, at every level.
            Unknown { level: u8, field: &'static str },
            Schema(&'static str),
        }

        fn flaw() -> impl Strategy<Value = Flaw> {
            prop_oneof![
                Just(Flaw::AllWeek),
                (1usize..4, (0u8..0x7F).prop_filter_map("Saturday and Sunday", |d| (d != SATURDAY_SUNDAY).then_some(d)))
                    .prop_map(|(rule, days_off)| Flaw::ForeignObserved { rule, days_off }),
                prop::sample::select(vec!["funday", "sat", "saturday "]).prop_map(Flaw::UnknownDay),
                prop::sample::select(vec!["Sun_To_Mon", "sun-to-mon", "weekend", ""]).prop_map(Flaw::UnknownObserved),
                prop::sample::select(vec![0u32, 13, u32::MAX]).prop_map(Flaw::Month),
                (1u32..=12, any::<bool>()).prop_map(|(month, zero)| Flaw::Day { month, zero }),
                prop::sample::select(vec![0i32, 6, -2, i32::MIN, i32::MAX]).prop_map(Flaw::Order),
                prop::sample::select(vec![-81i32, 251, i32::MIN, i32::MAX]).prop_map(Flaw::Easter),
                Just(Flaw::SameId),
                Just(Flaw::Inverted),
                Just(Flaw::EndsEarly),
                prop::sample::select(vec![1582i64, i64::MIN, -2026, 0]).prop_map(Flaw::FirstYear),
                Just(Flaw::FirstYear(i64::MAX)),
                prop::sample::select(vec!["2026-1-1", "2026-02-29", "2026-13-01", "26-01-01", "2026/01/01", "2026-01-01T00:00:00", "+2026-01-0", ""])
                    .prop_map(Flaw::LawNotADate),
                (0u8..5, prop::sample::select(vec!["obsreved", "valid_form", "loacl", "dya", "Id"]))
                    .prop_map(|(level, field)| Flaw::Unknown { level, field }),
                prop::sample::select(vec!["chronomock.calendar/2", "chronomock.calendar/10", "Chronomock.calendar/1", ""]).prop_map(Flaw::Schema),
            ]
        }

        /// `calendar` with `flaw` and nothing else wrong, and what the refusal has to say. A holiday-level
        /// flaw lands on the first holiday, which is added when the calendar has none.
        fn with_flaw(calendar: &DrawnCalendar, flaw: &Flaw) -> (serde_json::Value, String) {
            let mut json = calendar.json();
            let holidays = json["holidays"].as_array_mut().expect("a list");
            while holidays.len() < 2 {
                let extra = DrawnHoliday { rule: DrawnRule::Fixed(1, 1), from: None, to: None };
                holidays.push(holiday_json(holidays.len(), &extra));
            }
            let first = calendar.valid_from;
            let said = match *flaw {
                Flaw::AllWeek => {
                    json["weekend"] = (0..7).map(|d| written(d, 0)).collect::<Vec<_>>().into();
                    json["observed"] = "none".into();
                    "'weekend' lists all seven days".to_string()
                }
                Flaw::ForeignObserved { rule, days_off } => {
                    json["weekend"] = (0..7u32).filter(|d| days_off & (1 << d) != 0).map(|d| written(d, 1)).collect::<Vec<_>>().into();
                    json["observed"] = OBSERVANCES[rule].0.into();
                    format!("calendar 'observed' is '{}'", OBSERVANCES[rule].0)
                }
                Flaw::UnknownDay(name) => {
                    json["weekend"].as_array_mut().expect("a list").push(name.into());
                    format!("unknown weekday '{}'", name.to_ascii_lowercase())
                }
                Flaw::UnknownObserved(name) => {
                    json["observed"] = name.into();
                    format!("unknown observed rule '{name}'")
                }
                Flaw::Month(month) => {
                    json["holidays"][0]["rule"] = serde_json::json!({ "type": "fixed", "month": month, "day": 1 });
                    format!("holiday 'h0': month {month} out of range")
                }
                Flaw::Day { month, zero } => {
                    let day = if zero { 0 } else { LONGEST[month as usize - 1] + 1 };
                    json["holidays"][0]["rule"] = serde_json::json!({ "type": "fixed", "month": month, "day": day });
                    format!("holiday 'h0': day {day} out of range for month {month}")
                }
                Flaw::Order(order) => {
                    json["holidays"][0]["rule"] = serde_json::json!({ "type": "nth_weekday", "month": 5, "weekday": "monday", "order": order });
                    format!("holiday 'h0': order {order} out of range")
                }
                Flaw::Easter(offset) => {
                    json["holidays"][0]["rule"] = serde_json::json!({ "type": "easter_offset", "offset": offset });
                    format!("holiday 'h0': easter offset {offset} out of range")
                }
                Flaw::SameId => {
                    json["holidays"][1]["id"] = "h0".into();
                    "duplicate holiday id 'h0'".to_string()
                }
                Flaw::Inverted => {
                    json["holidays"][0]["valid_from"] = (first + 1).into();
                    json["holidays"][0]["valid_to"] = first.into();
                    format!("holiday 'h0': valid_from {} is after valid_to {first}", first + 1)
                }
                Flaw::EndsEarly => {
                    json["holidays"][0]["valid_from"] = serde_json::Value::Null;
                    json["holidays"][0]["valid_to"] = (first - 1).into();
                    format!("holiday 'h0': valid_to {} is before the calendar's valid_from {first}", first - 1)
                }
                Flaw::FirstYear(year) => {
                    for holiday in json["holidays"].as_array_mut().expect("a list") {
                        holiday["valid_from"] = serde_json::Value::Null;
                        holiday["valid_to"] = serde_json::Value::Null;
                    }
                    let year = if year == i64::MAX { calendar.law.0 + 1 } else { year };
                    json["valid_from"] = year.into();
                    format!("calendar 'valid_from' is {year}, outside")
                }
                Flaw::LawNotADate(text) => {
                    json["law_as_of"] = text.into();
                    format!("calendar 'law_as_of' is '{text}', not a date")
                }
                Flaw::Unknown { level, field } => {
                    let at = match level {
                        0 => &mut json,
                        1 => &mut json["holidays"][0],
                        2 => &mut json["holidays"][0]["name"],
                        3 => &mut json["holidays"][0]["rule"],
                        _ => {
                            json["name"] = serde_json::json!({ "en": "Drawn", "local": "Drawn" });
                            &mut json["name"]
                        }
                    };
                    at[field] = 1.into();
                    format!("unknown field `{field}`")
                }
                Flaw::Schema(schema) => {
                    json["schema"] = schema.into();
                    format!("unsupported calendar schema '{schema}'")
                }
            };
            (json, said)
        }

        proptest! {
            /// A calendar written within its schema loads as exactly the calendar it describes - its
            /// weekend however its days are written and repeated, its observance, its years, every
            /// holiday with its rule and its window. And the same calendar with ONE thing wrong, from
            /// the refusals this reader owes by name (S-1, S-17, R2-N7, R2-N8, R4-N38, R4-N39, R4-S20),
            /// is refused with a message that names that thing.
            #[test]
            fn a_calendar_reads_as_written_or_is_refused_for_its_one_flaw(
                calendar in drawn_calendar(),
                flaw in prop::option::weighted(0.6, flaw()),
            ) {
                match flaw {
                    None => {
                        let text = calendar.json().to_string();
                        let read = calendar_from_text(&text);
                        prop_assert_eq!(read, Ok(calendar.calendar()), "{}", text);
                    }
                    Some(flaw) => {
                        let (json, said) = with_flaw(&calendar, &flaw);
                        let text = json.to_string();
                        match calendar_from_text(&text) {
                            Ok(_) => prop_assert!(false, "{:?} loaded: {}", flaw, text),
                            Err(refusal) => prop_assert!(refusal.contains(&said), "{:?}: {} - owed {:?}", flaw, refusal, said),
                        }
                    }
                }
            }
        }
    }
}
