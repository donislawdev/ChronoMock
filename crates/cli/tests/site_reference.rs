//! The command line reference on the website against the program itself.
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

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

fn data_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../site/data/cli.json")
}

/// The options of every command, as the documentation lists them.
fn documented() -> BTreeMap<String, BTreeSet<String>> {
    let path = data_file();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let data: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
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
