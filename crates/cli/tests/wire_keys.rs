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
//! Scope, deliberately narrow: only keys that travel over the wire as an event field. Calculator
//! errors (`calc.*`, and the moment keys raised by `chrono calc`) travel as plain stderr text -
//! `CalcClient` surfaces stderr as the exception message - so they are not translation keys and
//! are not checked here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Source text with `#[cfg(test)]` modules removed, so fixture keys invented by tests
/// (`some.future_key`, `cleanup.something_new`) never look like production emissions.
fn production_source(path: &Path) -> String {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    match text.find("#[cfg(test)]") {
        Some(cut) => text[..cut].to_string(),
        None => text,
    }
}

/// A wire key looks like `area.detail` - lowercase words joined by dots and underscores. This
/// shape check is what keeps file names (`icudtl.dat`) and JS expressions (`performance.now`)
/// out of the set without needing a hand-written blocklist.
fn is_key_shaped(s: &str) -> bool {
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
    // File names share the shape. The list is short, jawne and only ever grows when a new kind of
    // file name shows up in the sources (icudtl.dat, snapshot_blob.bin, v8_context_snapshot.bin).
    const FILE_SUFFIXES: [&str; 7] = [".dat", ".bin", ".exe", ".dll", ".json", ".js", ".now"];
    !FILE_SUFFIXES.iter().any(|suffix| s.ends_with(suffix))
}

/// Keys emitted onto the wire by the core, gathered from the shapes this codebase actually uses.
fn emitted_keys(root: &Path) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();

    let sources = [
        root.join("crates/cli/src/main.rs"),
        root.join("crates/cli/src/cdp/mod.rs"),
        root.join("crates/cli/src/cdp/session.rs"),
        root.join("crates/cli/src/cdp/launch.rs"),
        root.join("crates/proto/src/lib.rs"),
    ];
    // Scanning only the emission SITES misses most of the set: many keys are not literals where
    // the event is built (`reason_key: reason.to_string()`), they arrive through helpers whose
    // match arms hold the literal (`session_reason_key`, `jump_error_key`, `describe_reason`).
    // Measured 2026-09-05: site-only scanning found 19 of them. So the scan is by SHAPE over
    // production code, and `is_key_shaped` plus the test-module cut carry the precision.
    let mut all = sources.to_vec();
    all.push(root.join("crates/mech/src/lib.rs"));
    for path in &all {
        let text = production_source(path);
        for line in text.lines() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            let mut rest = line;
            while let Some(open) = rest.find('"') {
                let after = &rest[open + 1..];
                let Some(close) = after.find('"') else { break };
                if is_key_shaped(&after[..close]) {
                    keys.insert(after[..close].to_string());
                }
                rest = &after[close + 1..];
            }
        }
    }

    keys
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
