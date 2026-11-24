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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
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
    let answer = presets_in(&repository().join("presets"));
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
    let mut calendar = read_json(&repository().join("calendars/pl.json"));
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
