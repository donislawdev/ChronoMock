//! The documentation on the website against the program itself.
//!
//! The tables of the documentation are rendered from `site/data/cli.json`. The page they replaced
//! was written by hand from the argument parser and fell behind within a release: `chrono
//! calendars` shipped and the page never named it. So the options of each command in that file are
//! compared here with the usage lines the program prints for `chrono --help`, both ways round. An
//! option added to the program and not to the file fails, and so does one the file still lists
//! after the program dropped it.
//!
//! What this does not cover: an option the parser accepts that the usage line does not name. The
//! usage lines are the program's own promise and other tests hold the parser to them, and scanning
//! the parser's sources for strings that start with two dashes finds the options of the target
//! programs too (the browser switches a session refuses), so it would be a list of exceptions
//! rather than a check. Exit codes are not compared either: in the program they are numbers
//! returned from many places, with no table to read.
//!
//! The scenarios page is rendered from `site/data/presets.json`, which is the program's own answer
//! to `chrono presets --json` rather than the preset files: the program fills in what a file leaves
//! to it, such as the choices of a variant and the calendar of a market, and the website building
//! on Linux cannot run it. The answer is compared here with what the program reads today, and the
//! example preset the page shows is checked to be one the program takes.
//!
//! The JSON page names every field of the program's `--json` answers in tables marked with their
//! schema, and every kind of line `chrono run --json` writes in a table of its own. The fields are
//! compared both ways with what the program writes for a set of commands that between them fill
//! every field, and the lines with the protocol's `Event` enum. Not compared: the words a field can
//! hold, such as the marks of `significance` and the keys of warnings, which the page gives in prose.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The shipped calendars and presets, which live with the date library.
fn catalogue() -> PathBuf {
    repository().join("crates/dates/catalogue")
}

fn read_json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The options of every command, as the documentation lists them.
fn documented() -> BTreeMap<String, BTreeSet<String>> {
    let data = read_json(&repository().join("site/data/cli.json"));
    let mut found = BTreeMap::new();
    for command in data["commands"].as_array().expect("commands") {
        let name = command["name"].as_str().expect("name").to_string();
        let mut flags = BTreeSet::new();
        for group in command["groups"].as_array().expect("groups") {
            for flag in group["flags"].as_array().expect("flags") {
                let flag = flag["flag"].as_str().expect("flag").to_string();
                assert!(flags.insert(flag.clone()), "cli.json lists {name} {flag} twice");
            }
        }
        assert!(found.insert(name.clone(), flags).is_none(), "cli.json lists the command {name} twice");
    }
    found
}

/// The options a usage line names, from the part before its first double space, which is where
/// the synopsis ends and the explanation starts: `chrono presets [--dir <folder>] [--json]   every
/// preset ... the way --preset reads it` names two options, not three.
fn options_in(line: &str) -> BTreeSet<String> {
    let synopsis = line.split("  ").next().unwrap_or_default();
    synopsis
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|word| word.starts_with("--") && word.len() > 2)
        .map(str::to_string)
        .collect()
}

/// The options of every command, as `chrono --help` prints them: the `usage: chrono <command>`
/// line and the `or: chrono <command>` lines after it.
fn printed(help: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for line in help.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("usage: chrono ").or_else(|| line.strip_prefix("or: chrono ")) else {
            continue;
        };
        let command = rest.split_whitespace().next().unwrap_or_default().to_string();
        found.entry(command).or_default().extend(options_in(rest));
    }
    found
}

#[test]
fn the_documented_options_are_the_ones_the_program_prints() {
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .arg("--help")
        .output()
        .expect("the tool must run");
    let help = String::from_utf8_lossy(&out.stdout);
    let program = printed(&help);
    let docs = documented();

    // The parse must have found something, or the comparison below proves nothing.
    assert!(program.len() >= 6 && program["run"].len() >= 20, "the usage lines were not read: {program:?}");

    let commands: BTreeSet<&String> = program.keys().chain(docs.keys()).collect();
    let mut problems = Vec::new();
    for command in commands {
        let empty = BTreeSet::new();
        let in_program = program.get(command).unwrap_or(&empty);
        let in_docs = docs.get(command).unwrap_or(&empty);
        if !program.contains_key(command) {
            problems.push(format!("cli.json documents `chrono {command}`, which `chrono --help` does not print"));
        }
        if !docs.contains_key(command) {
            problems.push(format!("`chrono {command}` is not in site/data/cli.json"));
        }
        for flag in in_program.difference(in_docs) {
            problems.push(format!("chrono {command} {flag} is not in site/data/cli.json"));
        }
        for flag in in_docs.difference(in_program) {
            problems.push(format!("site/data/cli.json documents chrono {command} {flag}, which the usage line does not name"));
        }
    }
    assert!(problems.is_empty(), "the documentation and the program disagree:\n{}", problems.join("\n"));
}

/// The comparison is only as good as the reading of the usage lines, so the reading is checked on
/// lines whose answer is known, including the traps: a description that names another command's
/// option, and the parenthesised aliases of `version`.
#[test]
fn a_usage_line_is_read_up_to_its_explanation() {
    let help = "usage: chrono presets [--dir <folder>] [--json]   every preset the way --preset reads it\n\
                usage: chrono version  (also --version, -V)   the build\n\
                usage: chrono calc [--base <today>] [--shift <±N<unit>>]...\n\
                \x20      or: chrono calc --preset <id> [--param id=value]...   (named moment, e.g. month-end)\n\
                \x20      --json emits machine output\n";
    let got = printed(help);
    let set = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    assert_eq!(got["presets"], set(&["--dir", "--json"]));
    assert_eq!(got["version"], set(&[]));
    assert_eq!(got["calc"], set(&["--base", "--shift", "--preset", "--param"]));
    assert_eq!(got.len(), 3, "an explanation line is not a command: {got:?}");
}

/// Set to write `site/data/presets.json` again from the program, after a preset changed.
const WRITE: &str = "CHRONO_WRITE_SITE_DATA";

/// The first key of `site/data/presets.json`, which says where the file comes from.
const PRESETS_NOTE: &str = "Written by crates/cli/tests/site_reference.rs from `chrono presets --json`: the presets the program reads from presets/, in the form it gives them, without the folder and the file names. The scenarios page of the documentation is rendered from it. Not edited by hand: after a change to a preset, run `cargo test -p chrono-cli --test site_reference` with CHRONO_WRITE_SITE_DATA=1 set, and the test writes it again.";

/// The program's answer to `chrono presets --json --dir <dir>`.
fn presets_in(dir: &Path) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["presets", "--json", "--dir"])
        .arg(dir)
        .output()
        .expect("the tool must run");
    assert!(
        out.status.success(),
        "chrono presets --dir {} ended with {:?}: {}",
        dir.display(),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("chrono presets --json prints JSON")
}

/// The presets of an answer, without the file each came from: the name of a file is its id, and
/// the folder in the answer is a path on the machine that ran it, which a committed file must not
/// carry.
fn listed(answer: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut presets = answer["presets"].as_array().expect("a list of presets").clone();
    for preset in &mut presets {
        preset.as_object_mut().expect("a preset is an object").remove("file");
    }
    presets
}

#[test]
fn the_scenarios_on_the_website_are_the_presets_the_program_reads() {
    let answer = presets_in(&catalogue().join("presets"));
    assert_eq!(answer["refused"], serde_json::json!([]), "a shipped preset is refused");
    let presets = listed(&answer);
    // The comparison below proves nothing over an empty list.
    assert!(presets.len() >= 10, "the shipped presets were not read: {answer}");

    let path = repository().join("site/data/presets.json");
    if std::env::var_os(WRITE).is_some() {
        let file = serde_json::json!({ "//": PRESETS_NOTE, "presets": presets });
        let text = serde_json::to_string_pretty(&file).expect("serialise") + "\n";
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
    let on_site = read_json(&path);
    assert!(
        on_site["presets"] == serde_json::Value::Array(presets),
        "site/data/presets.json is not what the program reads from presets/ now. Write it again: \
         set {WRITE}=1 and run `cargo test -p chrono-cli --test site_reference`"
    );
}

#[test]
fn the_example_preset_on_the_website_is_one_the_program_takes() {
    let dir = repository().join("site/examples/presets");
    let files = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count();
    let answer = presets_in(&dir);
    assert_eq!(answer["refused"], serde_json::json!([]), "the example preset is refused");
    assert!(files >= 1, "{} has no example", dir.display());
    assert_eq!(listed(&answer).len(), files, "every example file is listed: {answer}");
}

/// The calendars page tells the reader to copy `pl.json`, rename it and add a holiday shaped like
/// its example. This does the same and asks the program whether it takes the result.
#[test]
fn the_example_holiday_on_the_website_makes_a_calendar_the_program_takes() {
    let mut calendar = read_json(&catalogue().join("calendars/pl.json"));
    let holiday = read_json(&repository().join("site/examples/calendars/holiday.json"));
    calendar["id"] = "pl-office".into();
    calendar["holidays"].as_array_mut().expect("a list of holidays").push(holiday);

    let dir = std::env::temp_dir().join(format!("chrono-site-calendar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    std::fs::write(dir.join("pl-office.json"), calendar.to_string()).expect("write the calendar");
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["calendars", "--json", "--dir"])
        .arg(&dir)
        .output()
        .expect("the tool must run");
    let _ = std::fs::remove_dir_all(&dir);

    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).expect("chrono calendars --json prints JSON");
    assert_eq!(answer["refused"], serde_json::json!([]), "the calendar with the example holiday is refused");
    assert_eq!(answer["calendars"].as_array().map(Vec::len), Some(1), "{answer}");
}

/// The JSON page of the documentation, in both of its languages.
const JSON_PAGES: [&str; 2] = ["site/pages/json-output/en.html", "site/pages/json-output/pl.html"];

/// Every path in a JSON value into `all`: `moment.formats.us`, with `[]` for the objects of a list
/// (`presets[].id`). The paths nothing hangs under, such as a number, a text, null, a list of plain
/// values or an empty list, also go into `ends`.
fn paths_of(value: &serde_json::Value, at: &str, all: &mut BTreeSet<String>, ends: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => {
            if !at.is_empty() {
                all.insert(at.to_string());
            }
            for (key, inner) in map {
                let below = if at.is_empty() { key.clone() } else { format!("{at}.{key}") };
                paths_of(inner, &below, all, ends);
            }
        }
        serde_json::Value::Array(items) if items.iter().any(serde_json::Value::is_object) => {
            all.insert(at.to_string());
            for item in items.iter().filter(|item| item.is_object()) {
                paths_of(item, &format!("{at}[]"), all, ends);
            }
        }
        _ => {
            all.insert(at.to_string());
            ends.insert(at.to_string());
        }
    }
}

/// Whether `path` lies under `parent`: `moment.formats.us` under `moment.formats`, `presets[].id`
/// under `presets`.
fn lies_under(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent).is_some_and(|rest| rest.starts_with('.') || rest.starts_with("[]"))
}

/// What does not match between the fields a page names and the paths the program wrote. A field the
/// page names must be one the program writes. Every end the program writes must be named, or lie
/// under a field the page describes as a whole, naming nothing under it (a list of small objects told
/// in one sentence), or lie above a field the page names (an empty list whose objects the page
/// describes).
fn mismatches(named: &BTreeSet<String>, all: &BTreeSet<String>, ends: &BTreeSet<String>) -> Vec<String> {
    let wholes: Vec<&String> = named.iter().filter(|n| !named.iter().any(|other| lies_under(other, n))).collect();
    let mut wrong = Vec::new();
    for end in ends {
        let told = named.contains(end)
            || wholes.iter().any(|whole| lies_under(end, whole))
            || named.iter().any(|n| lies_under(n, end));
        if !told {
            wrong.push(format!("the program writes `{end}`, and the page does not say what it is"));
        }
    }
    for n in named.iter().filter(|n| !all.contains(*n)) {
        wrong.push(format!("the page names `{n}`, which the program does not write"));
    }
    wrong
}

/// The text of every `<code>` in `html`, in order.
fn codes(html: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<code>") {
        let after = &rest[start + "<code>".len()..];
        let Some(end) = after.find("</code>") else { break };
        found.push(after[..end].to_string());
        rest = &after[end + "</code>".len()..];
    }
    found
}

/// The tables of a page whose opening tag carries `marker`, each up to its `</table>`.
fn tables<'a>(page: &'a str, marker: &str) -> Vec<&'a str> {
    page.match_indices(marker)
        .map(|(at, _)| {
            let table = &page[at..];
            &table[..table.find("</table>").unwrap_or(table.len())]
        })
        .collect()
}

/// The fields a page names for `schema`: the first `<code>` of every row of the tables marked
/// `data-schema="<schema>"`.
fn named_fields(page: &str, schema: &str) -> BTreeSet<String> {
    let mut named = BTreeSet::new();
    for table in tables(page, &format!("data-schema=\"{schema}\"")) {
        for row in table.split("<tr").skip(1).filter(|row| !row.contains("<th")) {
            if let Some(first) = codes(row).into_iter().next() {
                named.insert(first);
            }
        }
    }
    named
}

/// The lines a page names for `chrono run --json`: each row of the table marked `data-events` gives
/// its type in `data-event` and its fields in the cell of class `keys`.
fn named_events(page: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut named = BTreeMap::new();
    for table in tables(page, "data-events") {
        for row in table.split("<tr").skip(1) {
            let Some(rest) = row.strip_prefix(" data-event=\"") else { continue };
            let event = rest[..rest.find('"').expect("a closed data-event")].to_string();
            let cell = row.split("class=\"keys\">").nth(1).unwrap_or("");
            let cell = &cell[..cell.find("</td>").unwrap_or(cell.len())];
            named.insert(event, codes(cell).into_iter().collect());
        }
    }
    named
}

/// `SessionVerdict` as the wire spells it, `session_verdict`.
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// The lines `chrono run --json` writes are the protocol's events, passed on as the core sends them.
/// Their types and fields are read from the `Event` enum itself, because a session that sends every
/// type of line (a sleep, a vanish, a failed start) cannot be staged here.
fn protocol_events() -> BTreeMap<String, BTreeSet<String>> {
    let path = repository().join("crates/proto/src/lib.rs");
    let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    read_events(&source).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The one attribute of `Event` this reading follows: each line names its type in `type`, in the
/// variant's name in snake case. Anything else on the enum (a different tag, `rename_all` of another
/// case, `rename_all_fields`) would put other names on the wire than the ones read here.
const EVENT_ATTRIBUTE: &str = r#"#[serde(tag = "type", rename_all = "snake_case")]"#;

/// Whether a line of code drops a field from what is written whatever its value: `skip` or
/// `skip_serializing` without `_if`. `skip_serializing_if` leaves the field out only when it is empty,
/// so the field is still one the line can carry, and `skip_deserializing` does not touch the output.
fn always_skipped(code: &str) -> bool {
    code.match_indices("skip").any(|(at, _)| {
        let rest = &code[at + "skip".len()..];
        let rest = rest.strip_prefix("_serializing").unwrap_or(rest);
        rest.trim_start().starts_with([',', ')'])
    })
}

/// The types and fields of the `Event` enum in `source`. A serde attribute that renames, skips or
/// flattens would make this reading wrong, so it is an error rather than a guess.
fn read_events(source: &str) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let start = source.find("pub enum Event {").ok_or("no `pub enum Event {`")?;
    let serde: Vec<&str> = source[..start]
        .lines()
        .rev()
        .map(str::trim)
        .take_while(|l| l.starts_with("#["))
        .filter(|l| l.starts_with("#[serde"))
        .collect();
    if serde != [EVENT_ATTRIBUTE] {
        return Err(format!("`Event` carries {serde:?}, and this reading follows {EVENT_ATTRIBUTE} only"));
    }
    let mut events = BTreeMap::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for line in source[start..].lines() {
        let code = line.split("//").next().unwrap_or("").trim();
        if ["rename", "flatten", "alias"].iter().any(|w| code.contains(w)) || always_skipped(code) {
            return Err(format!("`{code}` changes what is on the wire, which this reading does not follow"));
        }
        let (opens, closes) = (code.matches('{').count(), code.matches('}').count());
        if depth == 1 && opens > 0 {
            let name = code.split_whitespace().next().unwrap_or("");
            if !name.starts_with(|c: char| c.is_ascii_uppercase()) {
                return Err(format!("`{code}` is not a variant"));
            }
            // Its fields would stand on this line, which is read as the variant's name only.
            if closes > 0 {
                return Err(format!("`{code}` is a variant on one line, which this reading does not follow"));
            }
            current = snake(name);
            events.insert(current.clone(), BTreeSet::new());
        } else if depth == 1 && !code.is_empty() && !code.starts_with('#') && closes == 0 {
            return Err(format!("`{code}` is a variant without named fields, which this reading does not follow"));
        } else if depth == 2
            && !code.starts_with('#')
            && let Some((field, _)) = code.split_once(':')
        {
            let field = field.trim().trim_start_matches("pub ").trim();
            events.get_mut(&current).ok_or("a field outside a variant")?.insert(field.to_string());
        }
        depth = depth + opens - closes;
        if depth == 0 && opens + closes > 0 {
            break;
        }
    }
    Ok(events)
}

/// The reading above must refuse what it cannot follow, or the comparison of the page with the enum
/// compares the page with names the program does not write.
#[test]
fn the_event_reader_refuses_what_it_cannot_follow() {
    let enum_with = |attribute: &str, field: &str| {
        format!(
            "#[derive(Serialize)]\n{attribute}\npub enum Event {{\n    SessionVerdict {{\n        verdict: String,\n        {field}\n        followed: Vec<u32>,\n    }},\n    Ended {{\n        clean: bool,\n    }},\n}}\n"
        )
    };
    let read = read_events(&enum_with(EVENT_ATTRIBUTE, "#[serde(default, skip_serializing_if = \"Option::is_none\")]")).unwrap();
    let want: BTreeMap<String, BTreeSet<String>> = [
        ("session_verdict".to_string(), ["verdict", "followed"].map(String::from).into()),
        ("ended".to_string(), BTreeSet::from(["clean".to_string()])),
    ]
    .into();
    assert_eq!(read, want, "a field left out only when empty is still a field");

    let refused = [
        (r#"#[serde(tag = "type", rename_all = "camelCase")]"#, "", "camelCase"),
        (r#"#[serde(tag = "kind", rename_all = "snake_case")]"#, "", "tag = \\\"kind"),
        (r#"#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]"#, "", "rename_all_fields"),
        (EVENT_ATTRIBUTE, "#[serde(skip_serializing)]", "skip_serializing)"),
        (EVENT_ATTRIBUTE, "#[serde(default, skip)]", "skip)"),
        (EVENT_ATTRIBUTE, "#[serde(rename = \"why\")]", "rename"),
        (EVENT_ATTRIBUTE, "#[serde(flatten)]", "flatten"),
    ];
    for (attribute, field, named) in refused {
        let err = read_events(&enum_with(attribute, field)).expect_err(&format!("{attribute} {field} was read"));
        assert!(err.contains(named), "{attribute} {field}: {err}");
    }
    let one_line = enum_with(EVENT_ATTRIBUTE, "").replace("Ended {\n        clean: bool,\n    },", "Ended { clean: bool },");
    let err = read_events(&one_line).expect_err("a variant on one line was read");
    assert!(err.contains("on one line"), "{err}");
    assert!(!always_skipped("#[serde(skip_deserializing)]"));
    assert!(!always_skipped("skipped_count: u32,"));
}

/// The program, from the catalogue folder, so `--preset` and `--calendar` find the shipped folders.
fn json_of(args: &[&str]) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(args)
        .arg("--json")
        .current_dir(catalogue())
        .output()
        .expect("the tool must run");
    assert!(
        out.status.success(),
        "chrono {} --json ended with {:?}: {}",
        args.join(" "),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("chrono {} --json: {e}", args.join(" ")))
}

#[test]
fn the_json_page_names_every_field_the_program_writes() {
    let broken = std::env::temp_dir().join(format!("chrono-site-json-{}", std::process::id()));
    std::fs::create_dir_all(&broken).unwrap_or_else(|e| panic!("{}: {e}", broken.display()));
    std::fs::write(broken.join("broken.json"), "{\"schema\":").expect("write a broken file");
    let refused = broken.to_str().expect("a temporary folder named in UTF-8").to_string();
    let chrono = env!("CARGO_BIN_EXE_chrono");
    // Between them, these fill every field: a step clamped to a shorter month, a preset, a mask with
    // letters it does not know, a pasted date, a refused file, and a dry run of a preset that takes
    // its parameters from the target.
    let answers: [(&str, Vec<Vec<&str>>); 4] = [
        (
            "chronomock.calc/1",
            vec![
                vec!["calc", "--base", "2026-01-31T00:00:00", "--shift", "+1mo"],
                vec!["calc", "--preset", "month-end"],
                vec!["calc", "--base", "2026-01-31T00:00:00", "--format", "yyyy-MM-dd QQ"],
                vec!["calc", "--analyze", "04/08/2008"],
            ],
        ),
        ("chronomock.presets/1", vec![vec!["presets"], vec!["presets", "--dir", &refused]]),
        ("chronomock.calendars/1", vec![vec!["calendars"], vec!["calendars", "--dir", &refused]]),
        ("chronomock.plan/1", vec![vec!["run", chrono, "--preset", "trial-first-day-after", "--dry-run"]]),
    ];
    let mut wrong = Vec::new();
    for (schema, runs) in &answers {
        let (mut all, mut ends) = (BTreeSet::new(), BTreeSet::new());
        for args in runs {
            let answer = json_of(args);
            assert_eq!(answer["schema"], *schema, "chrono {} --json", args.join(" "));
            paths_of(&answer, "", &mut all, &mut ends);
        }
        for page in JSON_PAGES {
            let text = std::fs::read_to_string(repository().join(page)).unwrap_or_else(|e| panic!("{page}: {e}"));
            let named = named_fields(&text, schema);
            // The comparison proves nothing over a table that was not found.
            assert!(named.len() >= 3, "{page} has no table of {schema}");
            wrong.extend(mismatches(&named, &all, &ends).into_iter().map(|w| format!("{page}, {schema}: {w}")));
        }
    }
    let _ = std::fs::remove_dir_all(&broken);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn the_json_page_names_every_line_a_session_writes() {
    let mut expected = protocol_events();
    // Every line carries the version, and the page says so once rather than on every row.
    for fields in expected.values_mut() {
        assert!(fields.remove("v"), "a line without `v`: {fields:?}");
    }
    assert!(expected.len() >= 8, "the Event enum was not read: {expected:?}");
    for page in JSON_PAGES {
        let text = std::fs::read_to_string(repository().join(page)).unwrap_or_else(|e| panic!("{page}: {e}"));
        let named = named_events(&text);
        let mut wrong = Vec::new();
        for event in expected.keys().chain(named.keys()).collect::<BTreeSet<_>>() {
            match (expected.get(event), named.get(event)) {
                (Some(fields), Some(listed)) if fields != listed => wrong.push(format!(
                    "`{event}`: the program writes {:?}, the page lists {:?}",
                    fields.difference(listed).collect::<Vec<_>>(),
                    listed.difference(fields).collect::<Vec<_>>()
                )),
                (Some(_), None) => wrong.push(format!("`{event}` is a line the page does not name")),
                (None, Some(_)) => wrong.push(format!("`{event}` is named on the page, and no session writes it")),
                _ => {}
            }
        }
        assert!(wrong.is_empty(), "{page}:\n{}", wrong.join("\n"));
    }
}

/// The canary of the comparison: a field the page leaves out, an empty list it does not name and a
/// field the program no longer writes are all found, and a list the page describes as a whole is not.
#[test]
fn a_field_left_out_either_way_is_found() {
    let named: BTreeSet<String> = ["schema", "moment.iso", "moment.gone", "list"].map(String::from).into();
    let answer = serde_json::json!({ "schema": "s", "moment": { "iso": "x", "zone_bias_min": 0 }, "list": [{ "a": 1 }], "empty": [] });
    let (mut all, mut ends) = (BTreeSet::new(), BTreeSet::new());
    paths_of(&answer, "", &mut all, &mut ends);
    let wrong = mismatches(&named, &all, &ends);
    assert_eq!(wrong.len(), 3, "{wrong:?}");
    for field in ["`moment.zone_bias_min`", "`empty`", "`moment.gone`"] {
        assert!(wrong.iter().any(|w| w.contains(field)), "{field} was not found: {wrong:?}");
    }
}
