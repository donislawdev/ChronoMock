//! Guards for the shape ceilings: the guard that the guard exists.
//!
//! The ceilings themselves are enforced by clippy under `-D warnings` and their numbers live in
//! `clippy.toml`. Nothing here re-measures the code - a second, hand-written metric beside clippy's
//! would be a second thing to get wrong, and the first draft of one written for this project
//! already scored `mech::gather_coverage` at 7 because `-> Coverage { unsafe {` is two braces on
//! one line and one indent on screen.
//!
//! What this file guards is everything AROUND those numbers, which clippy cannot see:
//!
//! * that the lints are still switched on, for every crate,
//! * that the tightened copy is exactly one below the real one on every key, which is what makes
//!   the CI inversion run a proof that each ceiling IS the measurement rather than headroom,
//! * that CI still runs that inversion at all,
//! * and that the escape hatches out of the argument-count lint only ever get rarer.
//!
//! 🔴 The reason the third one is here and not left to common sense: a threshold configured but
//! never run is a gate nobody runs, and a gate that has vanished cannot fail to announce itself.

use std::path::{Path, PathBuf};

/// The shape lints, recorded here as well as in the root manifest.
///
/// Growing this list is free - add the lint in both places. Shrinking it reddens, which is the
/// point: nothing otherwise stops a change from deleting a lint to make a red build green, and the
/// check that vanished cannot fail.
const SHAPE_LINTS: &[&str] = &["too_many_lines", "cognitive_complexity", "excessive_nesting"];

/// Escape hatches out of `clippy::too_many_arguments`, measured 2026-09-06.
///
/// The four are `chrono-ctl::write_anchor_full` and three detours in `chrono-hook` that mirror
/// Win32 entry points: `h_ntcup` is NtCreateUserProcess, `h_cpw` and `h_cpa` are CreateProcessW and
/// CreateProcessA. Their parameter lists are Microsoft's, not ours to split, which is why the width
/// axis is not a ratchet in this project and this count is one instead. Pinned exactly rather than
/// bounded: a count left standing above the truth grants a free allow nobody decided to grant.
const ARGUMENT_ALLOWS: usize = 4;

fn repo_root() -> PathBuf {
    // Duplicated from tests/hygiene.rs on purpose. Integration tests are separate binaries, and a
    // shared module would be a third file to keep in step for four lines of path arithmetic.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn manifest(path: &Path) -> toml::Table {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{} is not valid TOML: {e}", path.display()))
}

/// The configuration key that carries a lint's threshold. Derived rather than listed, so a lint
/// added to `SHAPE_LINTS` cannot arrive without its number.
fn threshold_key(lint: &str) -> String {
    format!("{}-threshold", lint.replace('_', "-"))
}

fn workspace_members() -> Vec<String> {
    manifest(&repo_root().join("Cargo.toml"))
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .expect("the workspace lists its members")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

#[test]
fn the_workspace_still_switches_every_shape_lint_on() {
    let root = manifest(&repo_root().join("Cargo.toml"));
    let table = root
        .get("workspace")
        .and_then(|w| w.get("lints"))
        .and_then(|l| l.get("clippy"))
        .and_then(|c| c.as_table())
        .expect("the workspace manifest has a [workspace.lints.clippy] table");

    let lost: Vec<&&str> = SHAPE_LINTS
        .iter()
        .filter(|lint| !table.contains_key(**lint))
        .collect();
    assert!(
        lost.is_empty(),
        "a shape lint has quietly left the workspace manifest, so nothing measures that axis any \
         more: {lost:?} - dropping one is a decision, so change SHAPE_LINTS too"
    );

    let gained: Vec<&String> = table
        .keys()
        .filter(|key| !SHAPE_LINTS.contains(&key.as_str()))
        .collect();
    assert!(
        gained.is_empty(),
        "a newly enabled lint is not recorded here: {gained:?} - add it to SHAPE_LINTS, that is \
         what makes it stick"
    );

    // "warn" and not "deny" on purpose: `-D warnings` in CI and in tools/gates.ps1 is what makes
    // these a gate, and it makes ALL of them a gate at once. Denying here as well would mean a
    // local `cargo clippy` could not be run without the ceilings stopping it mid-edit.
    for lint in SHAPE_LINTS {
        assert_eq!(
            table.get(*lint).and_then(|v| v.as_str()),
            Some("warn"),
            "{lint} is enabled at an unexpected level"
        );
    }
}

#[test]
fn every_crate_opts_in_to_the_workspace_lints() {
    let members = workspace_members();
    // The canary this whole file needs: an empty member list satisfies the loop below perfectly and
    // looks exactly like a guard that works.
    assert!(
        members.len() >= 7,
        "the member scan read only {} crates",
        members.len()
    );

    let mut missing = Vec::new();
    for member in &members {
        let path = repo_root().join(member).join("Cargo.toml");
        let opted_in = manifest(&path)
            .get("lints")
            .and_then(|l| l.get("workspace"))
            .and_then(toml::Value::as_bool)
            == Some(true);
        if !opted_in {
            missing.push(member.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "these crates have no `[lints] workspace = true`, so the shape ceilings do not apply to \
         them and nothing would say so: {missing:?}"
    );
}

#[test]
fn the_tightened_configuration_is_exactly_one_below_every_ceiling() {
    let real = manifest(&repo_root().join("clippy.toml"));
    let tight = manifest(
        &repo_root()
            .join(".github")
            .join("clippy-tight")
            .join("clippy.toml"),
    );

    // Every lint that is on must carry a number, or it silently runs at clippy's own default -
    // which for `cognitive_complexity` is 25, the same as ours today, so the loss would be
    // invisible until the day somebody changed one of them.
    for lint in SHAPE_LINTS {
        let key = threshold_key(lint);
        let ceiling = real
            .get(&key)
            .and_then(toml::Value::as_integer)
            .unwrap_or_else(|| panic!("clippy.toml has no {key}, so {lint} runs at clippy's own default"));
        let lowered = tight
            .get(&key)
            .and_then(toml::Value::as_integer)
            .unwrap_or_else(|| panic!(".github/clippy-tight/clippy.toml has no {key}"));
        assert_eq!(
            lowered,
            ceiling - 1,
            "{key} is {ceiling} but the tightened copy says {lowered}. The inversion run only \
             proves the ceiling is the measurement when it is exactly one lower"
        );
    }

    let extra: Vec<&String> = tight
        .keys()
        .filter(|key| !real.contains_key(*key))
        .collect();
    assert!(
        extra.is_empty(),
        "the tightened copy tightens something the real one does not set: {extra:?}"
    );
}

#[test]
fn ci_still_runs_the_inversion_that_pins_the_ceilings() {
    // A threshold that is configured but never run is a gate nobody runs. Without this, deleting
    // the CI step would leave `clippy.toml` looking exactly as guarded as it does today, and the
    // ceilings could drift above the measurement again with every test in this file still green.
    let workflow = std::fs::read_to_string(
        repo_root()
            .join(".github")
            .join("workflows")
            .join("ci.yml"),
    )
    .expect("the CI workflow is readable");

    // The needle is the YAML mapping line, not the two words in it. A bare `contains("CLIPPY_CONF_DIR")`
    // would be satisfied by the comment that explains the step, and by the error message the step
    // prints when it fails - an assertion that can be satisfied by prose about itself is not an
    // assertion. Aim at the construction.
    assert!(
        workflow.contains("CLIPPY_CONF_DIR: .github/clippy-tight"),
        "no CI step points clippy at the tightened configuration any more, so nothing checks that \
         the shape ceilings are still the measurement rather than headroom. The ceilings in \
         clippy.toml would keep passing while standing above the code"
    );
}

/// Every `.rs` file under `crates/`, with build output skipped.
fn rust_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![repo_root().join("crates")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|n| n.to_str()) != Some("target") {
                    stack.push(path);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out
}

#[test]
fn the_argument_escape_hatch_only_ever_gets_rarer() {
    let files = rust_sources();
    assert!(
        files.len() >= 30,
        "the escape-hatch scan read only {} files, and an empty scan counts zero of anything",
        files.len()
    );

    let mut found = Vec::new();
    for path in &files {
        // This file names what it counts, so counting it would count the rule as a breach of
        // itself. The same skip tests/hygiene.rs makes, for the same reason.
        if path.file_name().and_then(|n| n.to_str()) == Some("shape.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            if line.contains("allow(clippy::too_many_arguments)") {
                found.push(format!("{}:{}", path.display(), number + 1));
            }
        }
    }

    assert_eq!(
        found.len(),
        ARGUMENT_ALLOWS,
        "the argument escape hatches measured {} against a frozen {ARGUMENT_ALLOWS}. Fewer means \
         lower the number in the same change. More means a fifth signature grew past seven \
         arguments and reached for an allow instead of a smaller call: {found:?}",
        found.len()
    );
    println!("clippy::too_many_arguments escape hatches: {}", found.len());
}
