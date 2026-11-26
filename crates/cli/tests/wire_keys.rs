//! Guard for the wire-key contract: every translation key the core puts on the NDJSON wire must
//! exist in BOTH shipped translations. A key that reaches the panel without one renders as raw
//! jargon - the RELEASE-002 class, which a pre-release audit already had to close once by hand.
//!
//! Why this guard exists at all: the GUI side keeps `CoreWireKeys` in `LocalizationTests.cs`, a
//! MANUALLY maintained list whose own comment says "when the core adds a rendered key, add it
//! here". Nothing checked that list against reality, and it had already drifted - the audit of
//! 2026-09-05 found `moment.unsupported_kind`, emitted from a live jump path, missing from the
//! list and from both translation files. A guard written in prose but absent from code is worse
//! than none (untouchable rule 12), so this is a real test that fails on a real gap.
//!
//! Two sets, checked by two tests, because the core names its failures in two different SHAPES.
//!
//! The first is keys that travel over the wire as an event field, written as bare string literals.
//! The second is the calculator's, which the engine writes INSIDE an English sentence on stderr -
//! `chrono calc: step 1 needs a calendar - pass --calendar (calc.needs_calendar)`. The shape scan
//! cannot see those: the literal it finds is the whole sentence, which is not key-shaped, so every
//! `calc.*` key was invisible to this guard.
//!
//! That invisibility had a cost. Eight of the nine calculator keys had no translation at all and the
//! panel showed the engine's raw sentence - wrong language, a process name, and the contract key in
//! brackets - while this file's own header said calculator errors "are not translation keys". They
//! are: the GUI maps `calc.X` to `calc.err.X` (CalcErrorText), which is what the second test below
//! holds it to.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Source text with the inline test module removed, so fixture keys invented by tests
/// (`some.future_key`, `cleanup.something_new`) never look like production emissions.
///
/// The cut is anchored on `#[cfg(test)] mod tests`, not on `#[cfg(test)]` alone, and the difference
/// is not cosmetic. A crate-root `#[cfg(test)] mod testutil;` declaration sits among the other module
/// declarations at the TOP of `main.rs`, and cutting on the bare attribute threw the whole file away -
/// the scanner dropped from more than fifty keys to eighteen while still passing every other check.
/// The canary below caught it. This is the fix, not the canary's removal.
fn production_source(path: &Path) -> String {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let mut from = 0;
    while let Some(hit) = text[from..].find("#[cfg(test)]") {
        let at = from + hit;
        let after = text[at + "#[cfg(test)]".len()..].trim_start();
        if after.starts_with("mod tests") {
            return text[..at].to_string();
        }
        from = at + "#[cfg(test)]".len();
    }
    text
}

/// A wire key looks like `area.detail` - lowercase words joined by dots and underscores. This
/// shape check is what keeps file names (`icudtl.dat`) and JS expressions (`performance.now`)
/// out of the set without needing a hand-written blocklist.
fn is_key_shaped(s: &str) -> bool {
    // File names share the shape. The list is short, explicit and only ever grows when a new kind of
    // file name shows up in the sources (icudtl.dat, snapshot_blob.bin, v8_context_snapshot.bin, an
    // Electron application's app.asar, and evidence.txt, the name a `--report` refusal suggests).
    const FILE_SUFFIXES: [&str; 9] = [".dat", ".bin", ".exe", ".dll", ".json", ".js", ".now", ".asar", ".txt"];
    has_key_shape(s) && !FILE_SUFFIXES.iter().any(|suffix| s.ends_with(suffix))
}

/// The shape alone, without the file names `is_key_shaped` keeps out of the Rust scan. The window's own
/// scan needs this one: `action.now` is a key there, and `.now` is on that list only for `performance.now`
/// in the JavaScript the core sends to a page - measured, the stricter shape called three live texts dead.
fn has_key_shape(s: &str) -> bool {
    for ch in s.chars() {
        match ch {
            '.' | 'a'..='z' | '0'..='9' | '_' => {}
            _ => return false,
        }
    }
    let segments: Vec<&str> = s.split('.').collect();
    if segments.len() < 2 || s.len() < 5 {
        return false;
    }
    // Every segment must be non-empty and contain a letter. This is what rejects "127.0.0.1"
    // without a hand-written blocklist - an address is all digits, a key never is.
    if segments
        .iter()
        .any(|seg| seg.is_empty() || !seg.chars().any(|c| c.is_ascii_lowercase()))
    {
        return false;
    }
    true
}

/// Every `.rs` file under `dir`, recursively. The canary is part of the helper: a walk that finds
/// nothing looks exactly like a codebase with no keys in it.
fn rust_sources_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries =
            std::fs::read_dir(&d).unwrap_or_else(|e| panic!("cannot read {}: {e}", d.display()));
        for entry in entries {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    assert!(
        out.len() >= 5,
        "the walk found only {} Rust sources under {} - it is reading the wrong place",
        out.len(),
        dir.display()
    );
    out
}

/// Keys emitted onto the wire by the core, gathered from the shapes this codebase actually uses.
fn emitted_keys(root: &Path) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();

    // Every production source of the CLI crate, FOUND rather than listed. A hand-kept list has to be
    // extended on every file split, and forgetting is invisible: the keys of the moved code leave the
    // set silently, and the canary below (more than twenty keys) does not notice a set that merely
    // shrank. Measured when `main.rs` was split into modules - the list named four files and the
    // crate had eight. A walk cannot forget.
    // Scanning only the emission SITES misses most of the set: many keys are not literals where
    // the event is built (`reason_key: reason.to_string()`), they arrive through helpers whose
    // match arms hold the literal (`session_reason_key`, `jump_error_key`, `describe_reason`).
    // Measured 2026-09-05: site-only scanning found 19 of them. So the scan is by SHAPE over
    // production code, and `is_key_shaped` plus the test-module cut carry the precision.
    let mut all = rust_sources_under(&root.join("crates/cli/src"));
    all.push(root.join("crates/proto/src/lib.rs"));
    all.push(root.join("crates/mech/src/lib.rs"));
    for path in &all {
        let text = production_source(path);
        for line in text.lines() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            keys.extend(keys_in_line(line));
        }
    }

    keys
}

/// Every key-shaped run of characters written between two double quotes on one line.
///
/// Every quote is tried as an opening one, rather than pairing them left to right. Pairing is what
/// Rust source looks like until a raw string holds quotes of its own: `to_ndjson`'s fallback is
/// `r#"{"type":"error",...,"key":"proto.serialize_failed",...}"#`, where left-to-right pairing lands
/// one quote out of step and the key falls BETWEEN two pairs - so the one key the protocol crate emits
/// on its own was invisible to all three tests below. Trying every quote costs nothing in precision:
/// the run has to be key-shaped all the way to the next quote, which text between two literals never is.
fn keys_in_line(line: &str) -> Vec<String> {
    quoted_runs(line).into_iter().filter(|run| is_key_shaped(run)).map(str::to_string).collect()
}

/// Every run of key characters that a quote opens and a quote closes, on one line, whatever its shape.
fn quoted_runs(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    for (open, _) in line.match_indices('"') {
        let start = open + 1;
        let end = start
            + bytes[start..]
                .iter()
                .take_while(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_'))
                .count();
        if bytes.get(end) == Some(&b'"') && end > start {
            found.push(&line[start..end]);
        }
    }
    found
}

/// The functions in `report.rs` that turn a key into prose for the CLI report. A key the core emits
/// must have an arm in one of them, or the report prints it raw. `describe_chromium_warning` holds the
/// `chromium.*` keys `describe_warning` hands it (R4/16), and `describe_session_warning` the `session.*`
/// ones (R5-D3), and `describe_thread_warning` the key about calls let by on a thread without thread-local
/// storage (2026-10-28), all split for the length ceiling.
const GLOSSING_FUNCTIONS: [&str; 8] = [
    "describe_reason",
    "describe_error",
    "describe_warning",
    "describe_chromium_warning",
    "describe_session_warning",
    "describe_thread_warning",
    "describe_residue",
    "vanish_cause",
];

/// The bodies of [`GLOSSING_FUNCTIONS`], comment lines dropped so a key MENTIONED in a comment is not
/// taken for an arm.
///
/// A body ends at the first closing brace in column 0, not by counting braces, because the prose in
/// the arms is free to hold a brace of its own. The canaries are part of it: a renamed function would
/// leave the guard checking fewer bodies than it names, and a body that ran on into the next function
/// would count that function's arms as glosses - both look exactly like a report that explains
/// everything.
fn gloss_bodies(report: &str) -> String {
    let mut out = String::new();
    for name in GLOSSING_FUNCTIONS {
        let start = report
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("report.rs has no `fn {name}(` - a glossing function was renamed and this guard went blind"));
        let len = report[start..]
            .find("\n}")
            .unwrap_or_else(|| panic!("`fn {name}` in report.rs has no closing brace in column 0"));
        let body = &report[start..start + len];
        assert!(
            !body.contains("\nfn ") && !body.contains("\npub"),
            "the body cut for `fn {name}` ran into the next function - the cut is wrong, not the report"
        );
        for line in body.lines() {
            if !line.trim_start().starts_with("//") {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

/// Whether `key` is a match arm pattern in `bodies`: `"key" =>`, or one alternative of `"a" | "b" =>`.
/// What follows the key decides it - every alternative is followed by either `|` or `=>`, the last
/// one included - so a key that only appears as an arm's VALUE is not taken for a pattern.
///
/// An arm whose text is empty is not a gloss either: every one of these functions treats "" as "no
/// words for this key" and prints the key raw, so `"key" => ""` would have passed this scan while the
/// report said exactly what it said with no arm at all.
fn has_arm(bodies: &str, key: &str) -> bool {
    let quoted = format!("\"{key}\"");
    bodies.match_indices(&quoted).any(|(at, _)| {
        let after = bodies[at + quoted.len()..].trim_start();
        match after.strip_prefix("=>") {
            Some(value) => !value.trim_start().starts_with("\"\""),
            None => after.starts_with('|'),
        }
    })
}

/// The calculator's stable keys, which the engine writes INSIDE its stderr sentences as a trailing
/// `(calc.something)` rather than as a bare literal.
///
/// Anchored on that parenthesis, exactly as the GUI reads it (`CalcErrorText::KeyOf`), so the two
/// sides agree on what a key is. A `calc.` mentioned mid-prose is not one, and neither is a
/// parenthesis that holds ordinary English.
fn calc_keys(root: &Path) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for path in rust_sources_under(&root.join("crates/cli/src")) {
        collect_calc_keys(&production_source(&path), &mut keys);
    }
    keys
}

/// The calculator keys the GUI's own client writes, in the same `(calc.something)` shape.
///
/// Two failures are born on that side of the process boundary, not in the engine: the engine that
/// could not be started at all, and the one that ran past the client's limit. `CalcErrorText` reads
/// them exactly like an engine key, and the scan above never saw them, so `calc.timeout` showed a
/// Polish window an English sentence for as long as it existed (R4/13).
fn gui_calc_keys(root: &Path) -> BTreeSet<String> {
    let dir = root.join("gui/ChronoMock.Protocol");
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    let mut keys = BTreeSet::new();
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.extension().is_some_and(|e| e == "cs") {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            collect_calc_keys(&text, &mut keys);
        }
    }
    keys
}

/// The key families an engine sentence can end in, as the window's `CalcErrorText` reads them: the
/// calculator's, since R4/18 the preset catalogue's (`chrono presets`) and since R5/6a the calendar
/// catalogue's (`chrono calendars`), translated the same way.
const ENGINE_KEY_FAMILIES: [&str; 3] = ["(calc.", "(presets.", "(calendars."];

fn collect_calc_keys(text: &str, keys: &mut BTreeSet<String>) {
    for family in ENGINE_KEY_FAMILIES {
        let mut rest = text;
        while let Some(open) = rest.find(family) {
            let after = &rest[open + 1..];
            match after.find(')') {
                Some(close) => {
                    let candidate = &after[..close];
                    if is_key_shaped(candidate) {
                        keys.insert(candidate.to_string());
                    }
                    rest = &after[close..];
                }
                None => break,
            }
        }
    }
}

/// Top-level key names present in a translation file. Parsed rather than substring-matched so a
/// key appearing inside a translated SENTENCE is not mistaken for a defined key.
fn translation_keys(path: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    // The files carry // comments (see the stability audit), which strict JSON rejects, so strip
    // whole-line comments before parsing. This mirrors what LocalizationService does at runtime.
    let cleaned: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let value: serde_json::Value =
        serde_json::from_str(&cleaned).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()));
    value
        .as_object()
        .expect("translation file is a JSON object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn every_wire_key_has_both_translations() {
    let root = repo_root();
    let emitted = emitted_keys(&root);
    assert!(
        emitted.len() > 20,
        "the scanner found only {} keys - the emission shapes changed and this guard went blind",
        emitted.len()
    );

    let en = translation_keys(&root.join("gui/ChronoMock.App/Localization/Strings.en.json"));
    let pl = translation_keys(&root.join("gui/ChronoMock.App/Localization/Strings.pl.json"));

    let missing: Vec<&String> = emitted
        .iter()
        .filter(|k| !en.contains(*k) || !pl.contains(*k))
        .collect();

    assert!(
        missing.is_empty(),
        "these keys reach the wire but have no translation in en and/or pl, so the panel would \
         show raw jargon: {missing:?}\n\
         Add them to gui/ChronoMock.App/Localization/Strings.{{en,pl}}.json (and to CoreWireKeys \
         in LocalizationTests.cs, which mirrors this set for the GUI side)."
    );
}

/// The GUI keeps its own mirror of this set. Two hand-maintained lists drift apart in one
/// direction or the other, so the guard checks that the mirror still covers what the core emits.
#[test]
fn the_gui_mirror_lists_every_emitted_key() {
    let root = repo_root();
    let emitted = emitted_keys(&root);
    let mirror = std::fs::read_to_string(root.join("gui/ChronoMock.App.Tests/LocalizationTests.cs"))
        .expect("LocalizationTests.cs is readable");

    let missing: Vec<&String> = emitted
        .iter()
        .filter(|k| !mirror.contains(&format!("\"{k}\"")))
        .collect();

    assert!(
        missing.is_empty(),
        "CoreWireKeys in LocalizationTests.cs does not list: {missing:?}\n\
         That list is what protects the GUI from a key living only in the core."
    );
}

/// Every calculator refusal the engine names must have a translation the GUI can show.
///
/// The mapping is `calc.X` -> `calc.err.X`, which is what `CalcErrorText.Describe` does. A key with
/// no `calc.err.` entry falls back to the engine's English sentence, which is the state this test
/// exists to stop coming back: eight of nine keys were in it, on a panel that had already decided
/// (rule 15) that user-facing text is a translation key.
#[test]
fn every_calculator_key_has_both_translations() {
    let root = repo_root();
    let mut keys = calc_keys(&root);
    // A literal, not a count derived from the list this checks: a scan that stops finding keys looks
    // exactly like a codebase with none. Nine were measured on 2026-09-09, and the number only grows.
    assert!(
        keys.len() >= 9,
        "the calculator-key scan found only {} keys ({keys:?}) - the emission shape changed and this \
         guard went blind",
        keys.len()
    );
    // The same canary for the client's own two, measured on 2026-10-04.
    let client = gui_calc_keys(&root);
    assert!(
        client.len() >= 2,
        "the scan of the GUI's calculator client found only {} keys ({client:?}) - it went blind",
        client.len()
    );
    keys.extend(client);

    let en = translation_keys(&root.join("gui/ChronoMock.App/Localization/Strings.en.json"));
    let pl = translation_keys(&root.join("gui/ChronoMock.App/Localization/Strings.pl.json"));

    // `family.something` -> `family.err.something`, the mapping `CalcErrorText.Describe` makes. Each
    // catalogue's family has to be found too, or this guard would hold part of what it claims.
    for family in ["presets.", "calendars."] {
        assert!(
            keys.iter().any(|k| k.starts_with(family)),
            "no `{family}` key was found - that catalogue's refusals are no longer scanned"
        );
    }
    let missing: Vec<String> = keys
        .iter()
        .map(|k| {
            let (family, rest) = k.split_once('.').expect("a key has a family");
            format!("{family}.err.{rest}")
        })
        .filter(|k| !en.contains(k) || !pl.contains(k))
        .collect();

    assert!(
        missing.is_empty(),
        "these calculator refusals have no interface text, so the panel falls back to the engine's \
         English sentence: {missing:?}\n\
         Add them to gui/ChronoMock.App/Localization/Strings.{{en,pl}}.json."
    );
}

/// Every key the core emits must be explained by the CLI report too, not only by the GUI.
///
/// The two tests above hold the GUI to it and nothing held the other client: seven keys reached the
/// wire with both translations and a mirror entry, and `chrono run` printed each of them raw - the
/// jump refusals, both ways a dropped command leaves the session running and both ways the command
/// stream ends it. One more, the vanish most sessions end in, was explained only by a catch-all arm
/// that said the same thing of every reason, known or not.
///
/// What this does NOT check: that the arm stands in the function the key actually travels through. An
/// error key explained only in `describe_warning` passes here and still prints raw. Which function a
/// key reaches depends on the event that carries it, and that is not visible from the key.
///
/// No exception list, on purpose. Every event that carries a key reaches the report through one of
/// these functions, so there is no key the CLI cannot show - only keys it has not explained yet.
#[test]
fn every_wire_key_has_a_gloss_in_the_cli_report() {
    let root = repo_root();
    let emitted = emitted_keys(&root);
    assert!(
        emitted.len() > 20,
        "the scanner found only {} keys - the emission shapes changed and this guard went blind",
        emitted.len()
    );
    let report = production_source(&root.join("crates/cli/src/report.rs"));
    let bodies = gloss_bodies(&report);

    let missing: Vec<&String> = emitted.iter().filter(|k| !has_arm(&bodies, k)).collect();

    assert!(
        missing.is_empty(),
        "these keys reach the wire but the CLI report has no words for them, so `chrono run` prints \
         them raw: {missing:?}\n\
         Add an arm for each to the report.rs function its event goes through (describe_error for an \
         `error` event, describe_warning for warning_keys, describe_reason for a verdict reason, \
         describe_residue for ended.residue_keys, vanish_cause for a vanish)."
    );
}

/// The scan reads a key wherever it is quoted, including inside a raw string that holds JSON - the
/// shape that hid `proto.serialize_failed` - and still refuses text that is not a key.
/// How many times `source` builds an `Event::Error`: the variant with a brace body whose fields do not end
/// with `..`. A match names the fields it binds and ends with `..`, a construction names every field, in
/// whatever order - reading for `{ v:` alone missed one that put another field first (review round of
/// #131).
fn error_constructions(source: &str) -> usize {
    let mut count = 0;
    for (at, _) in source.match_indices("Event::Error") {
        let Some(body) = source[at + "Event::Error".len()..].trim_start().strip_prefix('{') else {
            continue;
        };
        let mut depth = 1;
        let mut end = body.len();
        for (i, c) in body.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !body[..end].trim_end().trim_end_matches(',').trim_end().ends_with("..") {
            count += 1;
        }
    }
    count
}

/// The `error` event is built in one function, `events::error_event` (R5-A3). Sixteen hand-built copies
/// had to agree on the version, the field names and the origin, and the two sessions' copies of the
/// command answers had already drifted apart once (R5-P18, R5-P19). A match on the variant is left
/// alone, and so is a test.
#[test]
fn an_error_event_is_built_in_one_place() {
    let root = repo_root();
    let mut built = Vec::new();
    for path in rust_sources_under(&root.join("crates/cli/src")) {
        let count = error_constructions(&production_source(&path));
        if count > 0 {
            let name = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            built.push((name, count));
        }
    }
    assert_eq!(
        built,
        [("crates/cli/src/events.rs".to_string(), 1)],
        "an error event is built by hand outside events::error_event - build it there, so its shape has one place"
    );
}

/// A construction is found whatever the order of its fields and however it is laid out, and a match is not
/// one.
#[test]
fn an_error_event_is_found_built_whatever_the_order_of_its_fields() {
    assert_eq!(error_constructions("let e = Event::Error { id: None, code: 1, v: 1, key: k, origin: o };"), 1);
    assert_eq!(error_constructions("sink(&Event::Error {\n    v: 1,\n    id: None,\n    code: 1,\n    key: k.into(),\n    origin: o.into(),\n});"), 1);
    assert_eq!(error_constructions("match e { Event::Error { key, origin, .. } => {} }"), 0);
    assert_eq!(error_constructions("if let Event::Error { key, .. } = e {}"), 0);
    assert_eq!(error_constructions("Event::Error { v: 1, ..base }"), 1, "struct update builds one too");
}

#[test]
fn the_key_scan_reads_keys_inside_raw_json_strings() {
    let raw = r###"r#"{"type":"error","v":1,"key":"proto.serialize_failed","origin":"proto"}"#,"###;
    assert!(
        keys_in_line(raw).iter().any(|k| k == "proto.serialize_failed"),
        "got {:?}",
        keys_in_line(raw)
    );
    assert_eq!(keys_in_line(r#"key: "moment.invalid".into(),"#), ["moment.invalid"]);
    // Between two literals there is never a key-shaped run all the way to the next quote.
    assert!(keys_in_line(r#"f("a b", "icudtl.dat", "127.0.0.1")"#).is_empty());
}

/// The arm matcher finds a key as a whole arm and as either alternative of a joined one, and a key
/// that is only an arm's value is not a pattern.
#[test]
fn an_arm_is_a_match_pattern_not_a_mention() {
    let bodies = "match k {\n \"a.one\" => \"x\",\n \"a.two\" | \"a.three\" => {\n \"y\"\n }\n \"a.five\" => \"\",\n _ => \"a.four\",\n}\n";
    for key in ["a.one", "a.two", "a.three"] {
        assert!(has_arm(bodies, key), "{key}");
    }
    assert!(!has_arm(bodies, "a.four"), "a key in an arm's VALUE is not an arm");
    assert!(!has_arm(bodies, "a.five"), "an arm with no words prints the key raw, so it is not a gloss");
}

// --- The other direction: every text in the translation files is reached from somewhere -----------
//
// PR A finding (f). Everything above asks whether a key that is USED has its text. Nothing asked
// whether a text that EXISTS is used, so a text whose control was taken away stayed behind in both
// languages - and went on being translated, reviewed and reworded. R4/21 found four by a one-off
// script, and this guard's first run found eighteen more, all from the move to three phases
// (e0bbbae). One of them was not dead text but a lost feature, the "(missing)" mark on the
// recent-application list, held in `UNREACHED` below until PR B put it back on the row.

/// Every production file with this extension under `dir`, build output left out. The canary is part of
/// it, as for the Rust walk: a walk that finds nothing looks like a window with no texts.
fn gui_files_under(dir: &Path, extension: &str, at_least: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).unwrap_or_else(|e| panic!("cannot read {}: {e}", d.display()));
        for entry in entries {
            let path = entry.expect("directory entry").path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if path.is_dir() && name != "bin" && name != "obj" {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == extension) {
                out.push(path);
            }
        }
    }
    out.sort();
    assert!(
        out.len() >= at_least,
        "the walk found only {} .{extension} files under {} - it is reading the wrong place",
        out.len(),
        dir.display()
    );
    out
}

/// Every key a view names as `{DynamicResource key}` or `{StaticResource key}`, or hands a converter as
/// `ConverterParameter=key` - the template `KeyFill` fills with a bound value ("Remove step {0}", R5-D82).
fn keys_in_markup(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for extension in ["{DynamicResource ", "{StaticResource "] {
        for (at, _) in text.match_indices(extension) {
            let rest = &text[at + extension.len()..];
            if let Some(close) = rest.find('}') {
                let key = rest[..close].trim();
                if has_key_shape(key) {
                    found.push(key.to_string());
                }
            }
        }
    }
    for (at, _) in text.match_indices("ConverterParameter=") {
        let rest = &text[at + "ConverterParameter=".len()..];
        let end = rest.find(['}', ',', '"']).unwrap_or(rest.len());
        let key = rest[..end].trim();
        if has_key_shape(key) {
            found.push(key.to_string());
        }
    }
    found
}

/// The markup with its comments taken out, so a reference left in a comment keeps no text alive.
fn markup_without_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        rest = rest[open..].find("-->").map_or("", |close| &rest[open + close + "-->".len()..]);
    }
    out.push_str(rest);
    out
}

/// C# with its comments taken out and its strings kept: `//` to the end of the line and `/* */`, when they
/// stand outside a string or a character literal. Regular, verbatim (`@"`) and raw (`"""`) strings are kept
/// whole, so a `//` inside an address stays text. Line breaks are kept, so the code keeps its lines.
fn csharp_without_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut kept = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let from = at;
        at = match &bytes[at..] {
            [b'/', b'/', ..] => end_of_line(bytes, at),
            [b'/', b'*', ..] => bytes[at + 2..].windows(2).position(|w| w == b"*/").map_or(bytes.len(), |p| at + 2 + p + 2),
            [b'"', b'"', b'"', ..] => end_of_raw_string(bytes, at),
            [b'@', b'"', ..] => end_of_quoted(bytes, at + 1, true),
            [b'"', ..] => end_of_quoted(bytes, at, false),
            [b'\'', ..] => end_of_char_literal(bytes, at),
            _ => at + 1,
        };
        let comment = bytes[from..].starts_with(b"//") || bytes[from..].starts_with(b"/*");
        if comment {
            kept.extend(bytes[from..at].iter().filter(|b| **b == b'\n'));
        } else {
            kept.extend_from_slice(&bytes[from..at]);
        }
    }
    String::from_utf8(kept).expect("only whole comments were taken out")
}

fn end_of_line(bytes: &[u8], at: usize) -> usize {
    bytes[at..].iter().position(|b| *b == b'\n').map_or(bytes.len(), |p| at + p)
}

/// Past the closing quote of a string opening at `open`. In a verbatim string `""` is a quote, elsewhere a
/// backslash takes the next character with it.
fn end_of_quoted(bytes: &[u8], open: usize, verbatim: bool) -> usize {
    let mut at = open + 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' if !verbatim => at += 2,
            b'"' if verbatim && bytes.get(at + 1) == Some(&b'"') => at += 2,
            b'"' => return at + 1,
            _ => at += 1,
        }
    }
    bytes.len()
}

/// Past the end of a raw string opening at `open` with three quotes or more: the same run of quotes again.
fn end_of_raw_string(bytes: &[u8], open: usize) -> usize {
    let quotes = bytes[open..].iter().take_while(|b| **b == b'"').count();
    let fence = vec![b'"'; quotes];
    bytes[open + quotes..].windows(quotes).position(|w| w == fence.as_slice()).map_or(bytes.len(), |p| open + quotes + p + quotes)
}

/// Past a character literal opening at `open` - `'x'`, `'\''` - so a quote inside one opens no string.
fn end_of_char_literal(bytes: &[u8], open: usize) -> usize {
    let body = if bytes.get(open + 1) == Some(&b'\\') { 2 } else { 1 };
    match bytes.get(open + 1 + body) {
        Some(b'\'') => open + body + 2,
        _ => open + 1,
    }
}

/// Every key the window writes out in full: in a view, or as a quoted literal in the application's and
/// the protocol client's C#. Comments are taken out first - a whole line, the end of one, a block, and
/// the markup's own - so a key a comment MENTIONS, or a reference commented out, keeps no dead text alive
/// (found in review: whole comment lines were the only ones left out). An interpolated key
/// (`$"calc.sig.{key}"`) is not a literal here - the run stops at the brace and is not key-shaped - and
/// is counted by its rule in `BUILT` instead.
fn keys_the_window_writes(root: &Path) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for path in gui_files_under(&root.join("gui/ChronoMock.App"), "xaml", 10) {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        keys.extend(keys_in_markup(&markup_without_comments(&text)));
    }
    let mut sources = gui_files_under(&root.join("gui/ChronoMock.App"), "cs", 20);
    sources.extend(gui_files_under(&root.join("gui/ChronoMock.Protocol"), "cs", 3));
    for path in sources {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        for line in csharp_without_comments(&text).lines() {
            keys.extend(quoted_runs(line).into_iter().filter(|run| has_key_shape(run)).map(str::to_string));
        }
    }
    keys
}

/// Every double-quoted word inside the first stretch of `source` that starts at the last of `anchors`
/// (each found after the one before) and ends at `end`.
fn quoted_words_in(source: &str, anchors: &[&str], end: &str) -> Vec<String> {
    let mut from = 0;
    for anchor in anchors {
        from += source[from..].find(anchor).unwrap_or_else(|| panic!("`{anchor}` is gone from the source - a rule's words went blind"));
    }
    let stretch = &source[from..];
    let stretch = &stretch[..stretch.find(end).unwrap_or_else(|| panic!("no `{end:?}` after `{}`", anchors[anchors.len() - 1]))];
    stretch.split('"').skip(1).step_by(2).map(str::to_string).collect()
}

/// How the window builds a key out of a word it was handed, instead of writing it out: the prefix, the
/// C# file and text that do the building, and where the words come from. A rule counts its keys only
/// while its builder is still in the code - a rule outliving its builder would keep its texts alive
/// after the window stopped showing them.
struct Built {
    prefix: &'static str,
    builder: (&'static str, &'static str),
    words: fn(&Path) -> Vec<String>,
}

fn read(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

const BUILT: [Built; 4] = [
    // A variant parameter's choices, by the labels the engine's `VARIANTS` table gives them.
    Built {
        prefix: "calc.variant.",
        builder: ("gui/ChronoMock.App/Calc/CalculatorViewModel.cs", "$\"calc.variant.{"),
        words: |root| quoted_words_in(&read(root, "crates/cli/src/preset.rs"), &["const VARIANTS"], "];"),
    },
    // The readings of a pasted date and the landmarks a date lands on, by the core's `key()`.
    Built {
        prefix: "calc.reading.",
        builder: ("gui/ChronoMock.App/Calc/CalculatorViewModel.cs", "$\"calc.reading.{"),
        words: |root| {
            let words = quoted_words_in(&read(root, "crates/dates/src/calc.rs"), &["impl DateReading {", "fn key(&self)"], "\n    }");
            words.into_iter().filter(|w| !w.contains(' ')).collect()
        },
    },
    Built {
        prefix: "calc.sig.",
        builder: ("gui/ChronoMock.App/Calc/CalculatorViewModel.cs", "$\"calc.sig.{"),
        words: |root| {
            let words = quoted_words_in(&read(root, "crates/dates/src/calc.rs"), &["impl Significance {", "fn key(&self)"], "\n    }");
            words.into_iter().filter(|w| !w.contains(' ')).collect()
        },
    },
    // The weekday of a result, by the core's own names, which the window lowers.
    Built {
        prefix: "calc.weekday.",
        builder: ("gui/ChronoMock.App/Calc/ResultProjection.cs", "$\"calc.weekday.{"),
        words: |root| {
            let names = quoted_words_in(&read(root, "crates/dates/src/calc.rs"), &["const DOW_FULL"], "];");
            names.iter().map(|n| n.to_lowercase()).collect()
        },
    },
];

/// Texts nothing reaches today that stay in the files, each with why and what makes it go. Checked from
/// both ends: an entry that is reached again, or is no longer in the files, has to come out. Only shrinks.
const UNREACHED: [(&str, &str); 0] = [];

/// Every key the window or the core can put on the screen, by every road this guard knows.
fn reached_keys(root: &Path) -> BTreeSet<String> {
    let mut reached = emitted_keys(root);
    reached.extend(keys_the_window_writes(root));

    // `family.something` -> `family.err.something`, as `CalcErrorText.Describe` builds it.
    let describe = read(root, "gui/ChronoMock.App/Calc/CalcErrorText.cs");
    assert!(describe.contains(".err."), "CalcErrorText no longer builds `family.err.` keys - the rule below is stale");
    let mut engine = calc_keys(root);
    engine.extend(gui_calc_keys(root));
    for key in engine {
        let (family, rest) = key.split_once('.').expect("a key has a family");
        reached.insert(format!("{family}.err.{rest}"));
    }

    for rule in &BUILT {
        let (file, text) = rule.builder;
        // In the code, not in a comment: a builder commented out builds nothing (found in review).
        assert!(
            csharp_without_comments(&read(root, file)).contains(text),
            "{file} no longer builds `{}` keys (`{text}`) - drop the rule, and the texts it kept alive",
            rule.prefix
        );
        let words = (rule.words)(root);
        assert!(words.len() >= 3, "the words of `{}` came back {words:?} - the rule went blind", rule.prefix);
        reached.extend(words.into_iter().map(|w| format!("{}{w}", rule.prefix)));
    }
    reached
}

/// PR A finding (f): every text in the translation files is reached by the window or the core. A text
/// nothing reaches is one nobody will ever see, still translated and reworded with the rest - the
/// round of PR #93 rewrote the Polish of `tip.freeze`, which had shown nowhere since e0bbbae.
#[test]
fn every_translation_is_reached_by_the_window_or_the_core() {
    let root = repo_root();
    let reached = reached_keys(&root);
    // One canary per road, so a road that went blind cannot pass as a window that uses fewer texts.
    for (canary, road) in [
        ("app.title", "a view"),
        ("mode.flow", "a C# literal"),
        ("coverage.session_clock_never_read", "the core's wire keys"),
        ("calc.err.needs_calendar", "an engine refusal"),
        ("presets.err.folder_missing", "a catalogue refusal"),
        ("calendars.err.folder_missing", "a calendar catalogue refusal"),
        ("calc.variant.on_day", "a variant label"),
        ("calc.sig.leap_day", "a landmark"),
        ("calc.weekday.monday", "a weekday"),
    ] {
        assert!(reached.contains(canary), "`{canary}` was not reached - the scan of {road} went blind");
    }

    let en = translation_keys(&root.join("gui/ChronoMock.App/Localization/Strings.en.json"));
    assert!(en.len() >= 500, "read only {} texts - the translation file is not the one this guard means", en.len());
    let excused: BTreeSet<&str> = UNREACHED.iter().map(|(key, _)| *key).collect();
    let orphans: Vec<&String> = en.iter().filter(|k| !reached.contains(*k) && !excused.contains(k.as_str())).collect();
    assert!(
        orphans.is_empty(),
        "these texts are in the translation files and nothing reaches them - no view, no C# literal, no key \
         from the core, no rule in BUILT: {orphans:?}\n\
         Remove them from gui/ChronoMock.App/Localization/Strings.{{en,pl}}.json, or, if the window builds them \
         from a word, name that rule in BUILT."
    );

    let problems = unreached_problems(&UNREACHED, &en, &reached);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// What is wrong with the texts `UNREACHED` excuses, read from both ends: one no longer in the files and
/// one reached again each have to come out. A function of its own so that the check is tested on a list
/// of its own - the list is empty today, and a check over an empty list passes whatever it does.
fn unreached_problems(excused: &[(&str, &str)], in_files: &BTreeSet<String>, reached: &BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    for (key, reason) in excused {
        if !in_files.contains(*key) {
            problems.push(format!("UNREACHED names `{key}`, which is no longer in the files - take it out ({reason})"));
        }
        if reached.contains(*key) {
            problems.push(format!("UNREACHED names `{key}`, which is reached again - take it out ({reason})"));
        }
    }
    problems
}

/// The list of unreached texts is checked from both ends: an entry whose text left the files and one whose
/// text is reached again are each named, and an entry that is neither passes.
#[test]
fn the_unreached_list_is_checked_from_both_ends() {
    let in_files: BTreeSet<String> = ["kept.text", "back.text"].map(String::from).into();
    let reached: BTreeSet<String> = ["back.text"].map(String::from).into();
    let excused = [("kept.text", "a reason"), ("gone.text", "a reason"), ("back.text", "a reason")];
    let problems = unreached_problems(&excused, &in_files, &reached);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].contains("`gone.text`") && problems[0].contains("no longer in the files"), "{problems:?}");
    assert!(problems[1].contains("`back.text`") && problems[1].contains("reached again"), "{problems:?}");
}

/// The markup scan takes a key from either resource extension and nothing that is not a key.
#[test]
fn the_markup_scan_reads_both_resource_extensions() {
    let markup = r#"<TextBlock Text="{DynamicResource app.title}" Style="{StaticResource PartHeading}" ToolTip="{StaticResource tip.zone }" />"#;
    assert_eq!(keys_in_markup(markup), ["app.title", "tip.zone"]);
}

/// A template handed to a converter is reached too, and a parameter that is not a key is not one.
#[test]
fn the_markup_scan_reads_a_key_handed_to_a_converter() {
    let markup = r#"<Button AutomationProperties.Name="{Binding Number, Converter={StaticResource KeyFill}, ConverterParameter=calc.remove_step}" Width="{Binding X, ConverterParameter=2}" />"#;
    assert_eq!(keys_in_markup(markup), ["calc.remove_step"]);
}

/// A reference in a comment is no reference: the markup's own comments, a line of C# comment, the end of
/// a line and a block all go, and the strings stay whole - an address with `//` in it, a verbatim string
/// with a doubled quote, a raw string, and a quote inside a character literal. Found in review: only whole
/// comment lines used to be left out, so `// "dead.key"` after code kept a dead text alive.
#[test]
fn a_key_in_a_comment_keeps_no_text_alive() {
    let markup = "<!-- {DynamicResource dead.markup} -->\n<TextBlock Text=\"{DynamicResource live.markup}\" /><!-- {DynamicResource dead.after}";
    assert_eq!(keys_in_markup(&markup_without_comments(markup)), ["live.markup"]);

    let csharp = concat!(
        "var a = \"live.first\"; // \"dead.trailing\"\n",
        "// \"dead.line\"\n",
        "/* \"dead.block\"\n \"dead.block_two\" */ var u = \"http://x.y/z\"; var b = \"live.after_url\";\n",
        "var v = @\"c:\\path \"\"quoted\"\" \"; var c = '\"'; var d = \"live.after_char\";\n",
        "var r = \"\"\"raw // not a comment \"\"\"; var e = \"live.after_raw\";\n",
        "var f = \"escaped \\\" // still text\"; var g = \"live.after_escape\";\n",
    );
    let code = csharp_without_comments(csharp);
    let keys: Vec<&str> = code.lines().flat_map(quoted_runs).filter(|run| has_key_shape(run)).collect();
    assert_eq!(keys, ["live.first", "live.after_url", "live.after_char", "live.after_raw", "live.after_escape"]);
    assert_eq!(code.lines().count(), csharp.lines().count(), "the code keeps its lines");

    // And a builder that is only a comment builds nothing.
    assert!(!csharp_without_comments("// Add($\"calc.sig.{key}\");\n").contains("$\"calc.sig.{"));
    assert!(csharp_without_comments("Add($\"calc.sig.{key}\");\n").contains("$\"calc.sig.{"));
}
