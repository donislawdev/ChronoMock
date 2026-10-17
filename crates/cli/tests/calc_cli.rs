//! The calculator as its callers meet it: the built binary, one invocation at a time.
//!
//! These are the behaviours that live in `calc_run` rather than in a function a unit test can call -
//! what a refused argument looks like on the error stream, and which zone "today" is read in. The GUI
//! reads both, so a regression here shows on its panel and nowhere in the unit tests.

use std::process::{Command, Output};

fn calc(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_chrono"))
        .arg("calc")
        .args(args)
        .output()
        .expect("the tool must run")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_string()
}

/// R4-S23: a refused argument used to be a keyless sentence followed by the whole usage, and the GUI
/// showed that usage as the "detail" under its message. With `--json` (which the GUI always passes,
/// last) the refusal is one line ending in its key, and nothing else.
///
/// The cases are the ones the calculator panel can produce from its own fields: an amount that is
/// empty, fractional or signed, a time without seconds, a zone outside the band or in another shape.
#[test]
fn a_refused_argument_is_one_keyed_line_when_json_is_asked() {
    let cases: [&[&str]; 6] = [
        &["--shift", "+d"],
        &["--shift", "+1.5d"],
        &["--shift", "+-5d"],
        &["--set-time", "23:59"],
        &["--to-zone", "+15:00"],
        &["--to-zone", "+5"],
    ];
    for case in cases {
        let argv = [&["--base", "today"][..], case, &["--json"][..]].concat();
        let out = calc(&argv);
        let said = stderr(&out);
        assert_eq!(out.status.code(), Some(1), "{case:?}: {said}");
        assert!(out.stdout.is_empty(), "{case:?}: a refusal writes no result");
        assert_eq!(said.lines().count(), 1, "{case:?} must be one line: {said}");
        assert!(said.starts_with("chrono calc: "), "{case:?}: {said}");
        assert!(said.ends_with("(calc.bad_argument)"), "{case:?} must end in its key: {said}");
    }
}

/// The usage names the calendar ids of the folder `--calendar` reads, not three written by hand (R5-F3),
/// and a missing calendar is refused with the folder and the ids on offer, ending in its key (R5-D5,
/// items 1 and 2). Run from the repository root, where the shipped calendars are.
#[test]
fn the_calendar_ids_come_from_the_folder() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run = |args: &[&str]| Command::new(env!("CARGO_BIN_EXE_chrono")).current_dir(&root).arg("calc").args(args).output().expect("runs");

    let usage = stderr(&run(&["--help"]));
    assert!(usage.contains("[--calendar <id>]") && !usage.contains("<us-banking|"), "{usage}");
    assert!(usage.contains("--calendar ids: pl, us-banking, us-federal (from "), "{usage}");

    let out = run(&["--base", "today", "--calendar", "nowhere", "--json"]);
    let said = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{said}");
    assert!(said.contains("which holds pl, us-banking, us-federal") && said.ends_with("(calc.calendar_not_found)"), "{said}");
}

/// A person at a terminal still gets the usage under the refusal - it is only the machine caller that
/// does not want it.
#[test]
fn without_json_the_usage_follows_the_refusal() {
    let out = calc(&["--shift", "+abcd"]);
    let said = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{said}");
    let first = said.lines().next().unwrap_or_default();
    assert!(first.ends_with("(calc.bad_argument)"), "{said}");
    assert!(said.contains("usage: chrono calc"), "{said}");
}

/// R4-N31: "days from now" compared the result's date, read in the zone after `--to-zone`, with today
/// read in the session zone. From -12:00 to +14:00 the very same instant was a day away. Twenty-six
/// hours apart, the two zones never share a date, so the old reading can never pass this.
#[test]
fn days_from_now_counts_from_today_in_the_result_zone() {
    for (session, result) in [("-12:00", "+14:00"), ("+14:00", "-12:00")] {
        let out = calc(&["--zone", session, "--base", "now", "--to-zone", result, "--json"]);
        assert_eq!(out.status.code(), Some(0), "{session} -> {result}: {}", stderr(&out));
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("calc --json is JSON");
        assert_eq!(
            doc["moment"]["metadata"]["days_from_today"], 0,
            "now re-expressed from {session} in {result} is still today: {doc}"
        );

        // The text output reads the same field.
        let out = calc(&["--zone", session, "--base", "now", "--to-zone", result]);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("days from now today"), "{session} -> {result}:\n{text}");
    }
}

/// A scratch catalogue: `calendars/` and `presets/` holding the given files, byte for byte. Nothing
/// sits beside the binary under test, so it reads both from its working directory, which is this.
fn catalogue(name: &str, files: &[(&str, Vec<u8>)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("chrono-calc-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, bytes) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("the catalogue folder");
        std::fs::write(&path, bytes).expect("a catalogue file");
    }
    dir
}

fn calc_in(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_chrono"))
        .current_dir(dir)
        .arg("calc")
        .args(args)
        .args(["--zone", "+00:00"])
        .output()
        .expect("the tool must run")
}

/// R4-N37 and R4-N33 as a user meets them, through `--calendar` and `--preset`. A file saved with a
/// byte order mark loads (it used to be "expected value at line 1 column 1"), a file whose id is not
/// its name is refused naming both (it used to be loaded and quoted under the id inside it), a broken
/// preset names its file (a calendar's error always did), a preset that says two things at once is
/// refused instead of settled by whichever field the reader looked at first, and so is one whose moment
/// names a parameter it does not declare.
#[test]
fn a_catalogue_file_loads_as_saved_and_is_named_when_refused() {
    let calendar = |id: &str| {
        format!(
            r#"{{"schema":"chronomock.calendar/1","id":"{id}","country":"XX","weekend":["saturday","sunday"],"observed":"none","valid_from":2000,"law_as_of":"2026-01-01","source":"t","holidays":[]}}"#
        )
        .into_bytes()
    };
    let preset = |id: &str, base: &str| {
        format!(
            r#"{{"schema":"chronomock.preset/1","id":"{id}","name":{{"en":"n","pl":"n"}},"explains":{{"en":"e","pl":"e"}},"applies_to":"calculator","parameters":[{{"id":"d","type":"date","default":"2020-01-01"}}],"moment":{{"base":{base}}}}}"#
        )
        .into_bytes()
    };
    let with_bom = |bytes: Vec<u8>| [vec![0xEF, 0xBB, 0xBF], bytes].concat();
    let dir = catalogue(
        "load",
        &[
            ("calendars/bom.json", with_bom(calendar("bom"))),
            ("calendars/other.json", calendar("pl")),
            ("presets/bom.json", with_bom(preset("bom", r#"{"parameter":"d"}"#))),
            ("presets/renamed.json", preset("month-end", r#"{"parameter":"d"}"#)),
            ("presets/broken.json", br#"{"schema":"chronomock.preset/1","id":"broken""#.to_vec()),
            ("presets/both.json", preset("both", r#"{"absolute":"2030-01-01T00:00:00","parameter":"d"}"#)),
            ("presets/undeclared.json", preset("undeclared", r#"{"parameter":"e"}"#)),
        ],
    );

    let out = calc_in(&dir, &["--calendar", "bom", "--base", "2026-07-01T12:00:00", "--shift", "+1bd"]);
    assert_eq!(out.status.code(), Some(0), "a calendar with a byte order mark: {}", stderr(&out));
    let out = calc_in(&dir, &["--preset", "bom"]);
    assert_eq!(out.status.code(), Some(0), "a preset with a byte order mark: {}", stderr(&out));

    let out = calc_in(&dir, &["--calendar", "other", "--base", "2026-07-01T12:00:00"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("'pl'") && err.contains("'other'") && err.contains("other.json"), "{err}");
    let out = calc_in(&dir, &["--preset", "renamed"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("'month-end'") && err.contains("'renamed'"), "{err}");

    let out = calc_in(&dir, &["--preset", "broken"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("broken.json"), "a broken preset names its file: {err}");

    let out = calc_in(&dir, &["--preset", "both"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("'absolute' and 'parameter'"), "the contradiction is named: {err}");

    // A base naming a parameter the file does not declare used to say "has no value", and passing the
    // value was then refused as an unknown parameter. With or without it, the file is what is refused.
    for args in [&["--preset", "undeclared"][..], &["--preset", "undeclared", "--param", "e=2030-01-01"]] {
        let out = calc_in(&dir, args);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(1), "{err}");
        assert!(
            err.contains("parameter 'e'") && err.contains("does not declare") && err.contains("undeclared.json"),
            "the undeclared parameter and its file are named: {err}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// R4/18 (D5): a preset with a market counts its business days in that market's calendar, as the
/// window's calculator does when the preset is chosen and `run --preset` has since R4-S12. This path
/// used to stop with "pass --calendar" (exit 5) while the window computed the same preset. A calendar
/// named on the command line still wins, as one picked in the window does, and a preset with no market
/// still needs one.
#[test]
fn a_market_preset_counts_in_its_markets_calendar_unless_another_is_named() {
    // Two calendars that disagree about Saturday: in `us-banking` it is a weekend, in `pl` here it is
    // not. Friday 2026-07-03 plus one business day is then Monday in one and Saturday in the other.
    let calendar = |id: &str, weekend: &str| {
        format!(
            r#"{{"schema":"chronomock.calendar/1","id":"{id}","country":"XX","weekend":[{weekend}],"observed":"none","valid_from":2000,"law_as_of":"2026-01-01","source":"t","holidays":[]}}"#
        )
        .into_bytes()
    };
    let preset = |id: &str, market: &str| {
        format!(
            r#"{{"schema":"chronomock.preset/1","id":"{id}","name":{{"en":"n"}},"explains":{{"en":"e"}},"applies_to":"calculator",{market}"moment":{{"base":{{"absolute":"2026-07-03T00:00:00"}},"steps":[{{"shift":{{"sign":"+","amount":1,"unit":"business_days"}}}}]}}}}"#
        )
        .into_bytes()
    };
    let dir = catalogue(
        "market",
        &[
            ("calendars/us-banking.json", calendar("us-banking", r#""saturday","sunday""#)),
            ("calendars/pl.json", calendar("pl", r#""sunday""#)),
            ("presets/due-us.json", preset("due-us", r#""market":"us","#)),
            ("presets/due-nowhere.json", preset("due-nowhere", "")),
        ],
    );
    let moment = |out: &Output| {
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("calc --json is JSON");
        (doc["moment"]["iso"].as_str().unwrap_or_default().to_string(), doc["moment"]["metadata"]["calendar"].clone())
    };

    let out = calc_in(&dir, &["--preset", "due-us", "--json"]);
    assert_eq!(out.status.code(), Some(0), "the market's calendar is used: {}", stderr(&out));
    assert_eq!(moment(&out), ("2026-07-06T00:00:00".to_string(), serde_json::json!("us-banking")));

    let out = calc_in(&dir, &["--preset", "due-us", "--calendar", "pl", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(moment(&out), ("2026-07-04T00:00:00".to_string(), serde_json::json!("pl")), "a named calendar wins");

    // The text says which calendar it counted in and where that came from, since nobody named it.
    let out = calc_in(&dir, &["--preset", "due-us"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("calendar: us-banking, from the preset's market (us)"), "{text}");

    let out = calc_in(&dir, &["--preset", "due-nowhere", "--json"]);
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    assert!(stderr(&out).contains("calc.needs_calendar"), "{}", stderr(&out));

    let _ = std::fs::remove_dir_all(&dir);
}

/// R4-S20 and R4/20 as a user meets them. The shipped Polish calendar refuses a business-day step that
/// walks into 2001 (exit 1, with the year), answers for a date before 2002 without judging it (no
/// business day, no holiday, the `before_calendar` mark), and a calendar checked against the law long
/// before today marks a later result `calendar_outdated` - the case of a package updated once and used
/// for years. The old check date is 2020, so the last condition holds on any machine whose clock is
/// past 2021.
#[test]
fn a_calendar_answers_only_for_its_years_and_says_when_it_is_old() {
    let shipped = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../calendars/pl.json")).expect("the shipped pl");
    let old = old_polish_calendar(&shipped);
    let dir = catalogue("range", &[("calendars/pl.json", shipped.into_bytes()), ("calendars/old.json", old.into_bytes())]);

    let out = calc_in(&dir, &["--calendar", "pl", "--base", "2002-01-02T00:00:00", "--shift", "-1bd"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("(calc.before_calendar)") && stderr(&out).contains("2002"), "{}", stderr(&out));

    let out = calc_in(&dir, &["--calendar", "pl", "--base", "1985-05-03T00:00:00", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("calc --json is JSON");
    let metadata = &doc["moment"]["metadata"];
    assert!(metadata["business_day"].is_null() && metadata["holiday"].is_null(), "{doc}");
    assert_eq!(metadata["calendar_valid_from"], 2002, "{doc}");
    assert_eq!(doc["moment"]["significance"], serde_json::json!(["before_calendar"]), "{doc}");

    let out = calc_in(&dir, &["--calendar", "old", "--base", "2026-07-01T12:00:00", "--shift", "+1bd", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("calc --json is JSON");
    assert!(doc["moment"]["significance"].as_array().is_some_and(|marks| marks.contains(&serde_json::json!("calendar_outdated"))), "{doc}");
    assert_eq!(doc["moment"]["metadata"]["calendar_law_as_of"], "2020-01-01", "{doc}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// R4/20: a walk back from a date after the check can end before it and still have judged days the
/// check never saw - the mark follows the days the step walked over, not only where it landed.
#[test]
fn a_walk_over_days_after_an_old_check_is_marked_even_when_it_ends_before_it() {
    let shipped = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../calendars/pl.json")).expect("the shipped pl");
    let dir = catalogue("reach", &[("calendars/old.json", old_polish_calendar(&shipped).into_bytes())]);
    let out = calc_in(&dir, &["--calendar", "old", "--base", "2020-01-10T00:00:00", "--shift", "-10bd", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("calc --json is JSON");
    assert!(doc["moment"]["iso"].as_str().is_some_and(|iso| iso < "2020-01-01"), "the walk ends before the check: {doc}");
    assert!(doc["moment"]["significance"].as_array().is_some_and(|marks| marks.contains(&serde_json::json!("calendar_outdated"))), "{doc}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The shipped Polish calendar under the id `old`, checked against the law on 2020-01-01 - whatever day
/// the shipped file itself was last checked on, so the tests that use it do not move with that date.
fn old_polish_calendar(shipped: &str) -> String {
    let renamed = shipped.replacen(r#""id": "pl""#, r#""id": "old""#, 1);
    let key = r#""law_as_of": ""#;
    let at = renamed.find(key).expect("the shipped calendar has a check date") + key.len();
    let old = format!("{}2020-01-01{}", &renamed[..at], &renamed[at + "YYYY-MM-DD".len()..]);
    assert!(old.contains(r#""id": "old""#) && old != shipped, "the copy has to differ from the shipped file");
    old
}

/// Random properties of `chrono calc` as its callers meet it - the built binary, run from the
/// repository root so `--calendar` and `--preset` find the shipped files, with words drawn for every
/// flag it reads and some it does not.
mod properties {
    use proptest::prelude::*;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// Any `i64`, its edges drawn as often as the middle: a uniform draw almost never lands in the
    /// last values of the range, where the overflows wait.
    fn edge_i64() -> impl Strategy<Value = i64> {
        prop_oneof![
            4 => any::<i64>(),
            2 => -1_000i64..=1_000,
            1 => (i64::MAX - 8)..=i64::MAX,
            1 => i64::MIN..=(i64::MIN + 8),
        ]
    }

    /// One of `names`, now and then a word that is none of them. Mostly a name: a command line with
    /// a wrong word in it is refused while it is read, before the calendar is loaded or a step is
    /// worked out, so lines full of wrong words test the reading and little else (measured
    /// 2026-10-17: a calendar refused without its key passed 64 cases, MISSED).
    fn one_of(names: &'static [&'static str]) -> impl Strategy<Value = String> {
        prop_oneof![
            9 => proptest::sample::select(names).prop_map(str::to_string),
            1 => "[a-z0-9-]{0,12}",
        ]
    }

    /// A year as a command line writes one: the years in use, the ends of the band and one past
    /// each, or any 64-bit number.
    fn year() -> impl Strategy<Value = String> {
        prop_oneof![
            4 => (1600i64..=2400).prop_map(|y| format!("{y:04}")),
            2 => proptest::sample::select(vec![262_143i64, 262_144, -262_143, -262_144])
                .prop_map(|y| if y < 0 { format!("-{:04}", -y) } else { format!("{y:04}") }),
            1 => edge_i64().prop_map(|y| y.to_string()),
        ]
    }

    fn moment() -> impl Strategy<Value = String> {
        (year(), 0u32..=13, 0u32..=32, 0u32..=24, 0u32..=60, 0u32..=60)
            .prop_map(|(y, mo, d, h, mi, s)| format!("{y}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}"))
    }

    /// A count as a command line writes one: small, as many years as the band is wide, any 64-bit
    /// number, or more digits than any integer holds.
    fn amount() -> impl Strategy<Value = String> {
        prop_oneof![
            6 => (0u64..=400).prop_map(|n| n.to_string()),
            1 => (260_000u64..=530_000).prop_map(|n| n.to_string()),
            1 => edge_i64().prop_map(|n| n.unsigned_abs().to_string()),
            1 => "[0-9]{20,30}",
        ]
    }

    const UNITS: &[&str] = &[
        "s", "sec", "seconds", "m", "min", "minutes", "h", "hrs", "hours", "d", "days", "w", "weeks", "mo",
        "months", "q", "quarters", "y", "yr", "years", "bd", "business_days",
    ];
    const SNAPS: &[&str] = &["start-of-month", "eom", "start-of-quarter", "eoq", "start-of-year", "end-of-year"];
    const NEAREST: &[&str] = &["next-business-day", "pbd", "next-leap-day", "next-feb-29"];
    const CALENDARS: &[&str] = &["pl", "us-banking", "us-federal"];
    const PRESETS: &[&str] = &["month-end", "quarter-end", "feb-29", "year-2038", "payment-due-business-days"];

    /// A flag and its value.
    fn flag(name: &'static str, value: impl Strategy<Value = String> + 'static) -> BoxedStrategy<Vec<String>> {
        value.prop_map(move |v| vec![name.to_string(), v]).boxed()
    }

    /// A zone as `--zone` and `--to-zone` take one - mostly inside the band, sometimes past it.
    fn zone() -> impl Strategy<Value = String> {
        prop_oneof![4 => "[-+](0[0-9]|1[0-4]):[0-5][0-9]", 1 => "[-+][0-9]{1,2}:[0-9]{2}"]
    }

    /// A calendar id: one that ships, or a well-formed one that does not, often enough for the
    /// refusal of a missing calendar to be met.
    fn calendar_id() -> impl Strategy<Value = String> {
        prop_oneof![3 => proptest::sample::select(CALENDARS).prop_map(str::to_string), 1 => "[a-z][a-z0-9-]{0,11}"]
    }

    fn base() -> BoxedStrategy<Vec<String>> {
        prop_oneof![
            3 => flag("--base", prop_oneof![Just("today".to_string()), Just("now".to_string()), moment()]),
            1 => flag("--base-utc", moment().prop_map(|m| format!("{m}Z"))),
        ]
        .boxed()
    }

    /// One step, as each of the step flags writes it.
    fn step() -> BoxedStrategy<Vec<String>> {
        let shift = (prop_oneof![8 => "[-+]", 1 => Just(String::new())], amount(), one_of(UNITS))
            .prop_map(|(sign, n, unit)| format!("{sign}{n}{unit}"));
        let time = prop_oneof![
            4 => "(0[0-9]|1[0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]",
            1 => (0u32..=25, 0u32..=61, 0u32..=61).prop_map(|(h, m, s)| format!("{h:02}:{m:02}:{s:02}")),
        ];
        prop_oneof![
            5 => flag("--shift", shift),
            2 => flag("--snap", one_of(SNAPS)),
            2 => flag("--nearest", one_of(NEAREST)),
            1 => flag("--set-time", time),
            1 => flag("--to-zone", zone()),
        ]
        .boxed()
    }

    /// A command line built the way the reader reads one: a moment (a base at most once, steps, and
    /// a calendar, a zone and a format at most once each), a preset with its parameters, or a date
    /// to read. Any word may still be one the reader refuses, and then the line stops where it is
    /// read - the less common case, on purpose.
    fn well_formed() -> impl Strategy<Value = Vec<String>> {
        let once = |words: BoxedStrategy<Vec<String>>| proptest::option::of(words).prop_map(Option::unwrap_or_default);
        let moment_line = (
            once(base()),
            proptest::collection::vec(step(), 0..5),
            once(flag("--calendar", calendar_id())),
            once(flag("--zone", zone())),
            once(flag("--format", "[yMdHhmsft' :/-]{1,12}")),
        )
            .prop_map(|(b, steps, c, z, f)| [b, steps.concat(), c, z, f].concat());
        let preset_line = (
            flag("--preset", one_of(PRESETS)),
            proptest::collection::vec(flag("--param", "[a-z_]{1,10}=[0-9T:-]{0,12}"), 0..2),
            once(flag("--calendar", calendar_id())),
        )
            .prop_map(|(p, params, c)| [p, params.concat(), c].concat());
        let analyze_line = (flag("--analyze", "[0-9/.: TZ+-]{0,20}"), once(flag("--zone", zone())))
            .prop_map(|(a, z)| [a, z].concat());
        prop_oneof![6 => moment_line, 1 => preset_line, 1 => analyze_line, 2 => off_an_end()]
    }

    /// A base in the last years of the band, at either end, and shifts that walk off it - the place
    /// R3-1 was. Asked for by name, like the edges of an integer: drawn from the whole band and from
    /// every step at once, a line that leaves it came up in about one case of two hundred, and the
    /// band check put back passed 64 cases seven times in ten (measured 2026-10-17).
    fn off_an_end() -> impl Strategy<Value = Vec<String>> {
        // The last year mostly, and shifts in units that leave a year: a second or a day from the
        // last few years stays inside, and drawn evenly they kept nine lines in ten inside the band.
        let back = prop_oneof![3 => Just(0i64), 1 => 1i64..=2];
        let unit = prop_oneof![
            3 => proptest::sample::select(&["y", "years", "mo", "months", "q", "quarters"][..]).prop_map(str::to_string),
            1 => one_of(UNITS),
        ];
        (prop_oneof![Just(1i64), Just(-1i64)], back, 1u32..=12, 1u32..=28, proptest::collection::vec((1u64..=400, unit), 1..3))
            .prop_map(|(side, back, month, day, shifts)| {
                let (minus, sign) = if side > 0 { ("", "+") } else { ("-", "-") };
                let year = 262_143 - back;
                let mut line = vec!["--base".to_string(), format!("{minus}{year}-{month:02}-{day:02}T12:00:00")];
                for (n, unit) in shifts {
                    line.push("--shift".to_string());
                    line.push(format!("{sign}{n}{unit}"));
                }
                line
            })
    }

    /// Words with no grammar at all: any flag, in any order and count, and words it does not read.
    fn any_words() -> impl Strategy<Value = Vec<String>> {
        let word = prop_oneof![
            3 => base(),
            6 => step(),
            1 => flag("--zone", zone()),
            1 => flag("--calendar", calendar_id()),
            1 => flag("--analyze", "[0-9/.: TZ+-]{0,20}"),
            1 => flag("--format", "[yMdHhmsft' :/-]{0,12}"),
            1 => flag("--preset", one_of(PRESETS)),
            1 => flag("--param", "[a-z_]{1,10}=[0-9T:-]{0,12}"),
            1 => "--?[a-z-]{0,10}".prop_map(|w| vec![w]).boxed(),
        ];
        proptest::collection::vec(word, 0..7).prop_map(|words| words.concat())
    }

    /// Mostly a line the grammar allows, sometimes any words, and in half the lines `--json` last,
    /// the way the window always calls. Lines of free words alone were refused while being read so
    /// often that a step leaving the year band, and a calendar refused without its key, passed 64
    /// and then 256 cases (measured 2026-10-17, MISSED).
    fn command_line() -> impl Strategy<Value = Vec<String>> {
        (prop_oneof![3 => well_formed(), 1 => any_words()], any::<bool>()).prop_map(|(mut line, json)| {
            if json {
                line.push("--json".to_string());
            }
            line
        })
    }

    /// The year of a text that starts as an ISO date (`2026-01-31...`, `-0009-...`), or `None` for
    /// any other text.
    fn iso_year(text: &str) -> Option<i64> {
        let digits = text.strip_prefix('-').unwrap_or(text);
        let year = digits.split('-').next()?;
        let rest = &digits[year.len()..];
        let shaped = rest.len() >= 6 && rest.as_bytes()[0] == b'-' && rest.as_bytes()[3] == b'-';
        if year.len() < 4 || !year.bytes().all(|b| b.is_ascii_digit()) || !shaped {
            return None;
        }
        let year: i64 = year.parse().ok()?;
        Some(if text.starts_with('-') { -year } else { year })
    }

    /// Every ISO date in a JSON answer, at any depth.
    fn iso_years(value: &serde_json::Value, out: &mut Vec<i64>) {
        match value {
            serde_json::Value::String(text) => out.extend(iso_year(text)),
            serde_json::Value::Array(items) => items.iter().for_each(|item| iso_years(item, out)),
            serde_json::Value::Object(fields) => fields.values().for_each(|field| iso_years(field, out)),
            _ => {}
        }
    }

    /// What one run left: its exit code (`None` when it had to be stopped), and its two streams.
    struct Ran {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    }

    /// `chrono calc` with `args`, stopped when it has not ended in `limit` - a walk without end is
    /// a finding, not a hung test binary. Both streams are drained on threads of their own, so a
    /// full pipe cannot pass for a hang.
    fn calc_within(args: &[String], limit: Duration) -> Ran {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut child = Command::new(env!("CARGO_BIN_EXE_chrono"))
            .current_dir(root)
            .arg("calc")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the tool must start");
        let drain = |mut pipe: Box<dyn Read + Send>| {
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = pipe.read_to_string(&mut text);
                text
            })
        };
        let stdout = drain(Box::new(child.stdout.take().expect("piped")));
        let stderr = drain(Box::new(child.stderr.take().expect("piped")));
        let deadline = Instant::now() + limit;
        let code = loop {
            if let Some(status) = child.try_wait().expect("the tool can be waited on") {
                break status.code();
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        Ran { code, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() }
    }

    /// A process per case costs some milliseconds, so a run takes a quarter of the cases the others
    /// take - of `PROPTEST_CASES` when the weekly run sets it, of proptest's 256 when nothing does.
    fn quarter_of_the_cases() -> u32 {
        std::env::var("PROPTEST_CASES").ok().and_then(|n| n.parse::<u32>().ok()).unwrap_or(256).div_ceil(4)
    }

    proptest! {
        // A process that hangs is killed at its limit, so nothing leaks - but every shrink step that
        // still hangs waits the whole limit again, and 128 of them would be an hour. Capped in time.
        #![proptest_config(ProptestConfig {
            cases: quarter_of_the_cases(),
            max_shrink_iters: 128,
            max_shrink_time: 120_000,
            ..ProptestConfig::default()
        })]

        /// R3-1 and S-1 through the front door. Whatever its command line says, `chrono calc` ends
        /// in time with an exit code docs/08 gives it - 0 with an answer, 1 for input it refuses,
        /// 5 for a step that is not built - and a refusal carries the key the window translates.
        /// A panic is exit 101, and a walk without end is stopped and named. An answer in JSON
        /// names only dates inside the year band: a step that left it used to come back as an
        /// answer with exit 0, which the exit code alone never showed (MISSED, 2026-10-17).
        #[test]
        fn any_command_line_ends_with_a_documented_exit_code_and_a_keyed_refusal(argv in command_line()) {
            let ran = calc_within(&argv, Duration::from_secs(30));
            match ran.code {
                Some(0) => {
                    prop_assert!(!ran.stdout.trim().is_empty(), "exit 0 and no answer for {:?}", argv);
                    if argv.iter().any(|w| w == "--json") {
                        let doc: serde_json::Value = serde_json::from_str(&ran.stdout).map_err(|e| {
                            TestCaseError::fail(format!("not JSON for {argv:?}: {e}\n{}", ran.stdout))
                        })?;
                        prop_assert_eq!(doc["schema"].as_str(), Some("chronomock.calc/1"));
                        let mut years = Vec::new();
                        iso_years(&doc, &mut years);
                        let band = -262_143i64..=262_143;
                        prop_assert!(
                            years.iter().all(|year| band.contains(year)),
                            "a date outside the year band for {:?}: {:?}", argv, years
                        );
                    }
                }
                Some(1) | Some(5) => {
                    prop_assert!(ran.stdout.trim().is_empty(), "a refusal wrote a result for {:?}: {}", argv, ran.stdout);
                    prop_assert!(ran.stderr.contains("(calc."), "a refusal without its key for {:?}: {}", argv, ran.stderr);
                }
                other => prop_assert!(false, "exit {:?} for {:?}\n{}", other, argv, ran.stderr),
            }
        }
    }
}
