//! The scan that polices the register, rather than replacing it.
//!
//! `packaging/components.json` declares everything Chrono Mock ships that somebody else wrote. It is
//! written by hand on purpose: a scanner reads what packaging left behind, cannot tell a build-time
//! crate from a linked one, and assigns licences by guessing. What a scan IS good for is catching the
//! register when it goes stale, which is what this file does.
//!
//! It went stale. Measured on 2026-09-08, before any of this existed: THIRD-PARTY-NOTICES.md declared
//! `log 0.4.33` while Cargo.lock and the shipped binary carried 0.4.34. A legal document stating what
//! is inside a binary had been wrong for a week and nothing could notice.
//!
//! Three directions, because one is never enough:
//!   * every rust crate the register declares is in Cargo.lock at exactly that version,
//!   * every third-party crate in Cargo.lock is either declared or listed below as build-time only,
//!   * every component the register declares is named in THIRD-PARTY-NOTICES.md, so the machine list
//!     and the legal document cannot drift apart.
//!
//! The managed half (WPF-UI, the .NET runtime packs) is checked where it can be: against the
//! `deps.json` a publish produces, in `packaging/build-dist.ps1`. That file does not exist in a
//! checkout, so it cannot be checked here, and a test that silently skipped would be worse than one
//! that does not claim the ground at all.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Crates in the lockfile that are NOT in the shipped binaries, each with the reason.
///
/// This list only ever shrinks. A crate added here is a claim that it never reaches a user's disk,
/// and the claim is checked the only way it can be - by reading `cargo tree -e normal,no-proc-macro`
/// for `chrono-cli` and `chrono-hook`, which is how the register itself was derived.
const BUILD_ONLY: &[(&str, &str)] = &[
    ("cc", "compiles the vendored MinHook C source at build time"),
    ("equivalent", "a trait crate under indexmap, which is under toml"),
    ("find-msvc-tools", "locates the MSVC toolchain for cc"),
    ("hashbrown", "the map behind indexmap, build-time only through toml"),
    ("indexmap", "the ordered map inside toml, build-time only"),
    ("proc-macro2", "the proc-macro toolkit, runs in the compiler"),
    ("quote", "a proc-macro toolkit crate, expanded in the compiler and never linked"),
    ("serde_derive", "the derive macro for serde, expanded at compile time"),
    ("serde_spanned", "toml's span type, used by the site generator's build"),
    ("shlex", "argument splitting inside cc, which runs at build time"),
    ("syn", "the parser every derive macro uses, two versions of it"),
    ("toml", "read by the site generator, which is a build tool and ships nothing"),
    ("toml_datetime", "part of the toml family the site generator reads"),
    ("toml_parser", "part of the toml family the site generator reads"),
    ("toml_writer", "part of the toml family the site generator reads"),
    ("tracing-attributes", "a proc-macro crate for tracing"),
    ("unicode-ident", "identifier tables for proc-macro2, compile time only"),
    ("version_check", "a build script helper, never present at runtime"),
    ("windows-implement", "a proc-macro crate for the windows bindings"),
    ("windows-interface", "a proc-macro crate for the windows bindings, alongside windows-implement"),
    ("winnow", "the parser under toml, build-time only"),
    ("winresource", "stamps the version resource onto the exe at build time"),
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn register() -> serde_json::Value {
    let path = repo_root().join("packaging").join("components.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the register must be readable at {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("the register must be valid JSON: {e}"))
}

/// Every component in the register as (name, version, kind).
fn components(doc: &serde_json::Value) -> Vec<(String, String, String)> {
    doc["components"]
        .as_array()
        .expect("the register must carry a components array")
        .iter()
        .map(|c| {
            let field = |key: &str| {
                c[key]
                    .as_str()
                    .unwrap_or_else(|| panic!("a component is missing '{key}': {c}"))
                    .to_string()
            };
            (field("name"), field("version"), field("kind"))
        })
        .collect()
}

/// Every `[[package]]` in Cargo.lock as (name, version).
///
/// Parsed by hand rather than with a TOML crate: a dev-dependency is a rule-8 licence-sieve event,
/// and this format is three lines of it. `name = ` is matched UNINDENTED, which is what separates a
/// package's own name from the entries of a `dependencies = [` list below it.
fn lockfile_packages() -> Vec<(String, String)> {
    let path = repo_root().join("Cargo.lock");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cargo.lock must be readable at {}: {e}", path.display()));

    let mut out = Vec::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("name = ") {
            name = quoted(rest);
        } else if let Some(rest) = line.strip_prefix("version = ")
            && let (Some(n), Some(v)) = (name.take(), quoted(rest))
        {
            out.push((n, v));
        }
    }
    out
}

fn quoted(raw: &str) -> Option<String> {
    raw.trim().strip_prefix('"')?.strip_suffix('"').map(str::to_string)
}

/// Our own crates, which are not third-party and are not declared as components.
fn workspace_crates() -> BTreeSet<&'static str> {
    ["chrono-cli", "chrono-core", "chrono-ctl", "chrono-hook", "chrono-mech", "chrono-proto", "chrono-site"]
        .into_iter()
        .collect()
}

/// Direction one: the register must not name a version the build does not have.
#[test]
fn every_crate_the_register_declares_is_in_the_lockfile_at_that_version() {
    let doc = register();
    let lock = lockfile_packages();
    let mut wrong: Vec<String> = Vec::new();

    let mut declared = 0usize;
    for (name, version, kind) in components(&doc) {
        if kind != "rust-crate" {
            continue;
        }
        declared += 1;
        let versions: Vec<&str> =
            lock.iter().filter(|(n, _)| *n == name).map(|(_, v)| v.as_str()).collect();
        if versions.is_empty() {
            wrong.push(format!("{name} {version} is declared and is in no lockfile entry"));
        } else if !versions.contains(&version.as_str()) {
            wrong.push(format!("{name}: register says {version}, the lockfile has {versions:?}"));
        }
    }

    // A literal floor. A register that lost its components would otherwise agree with the lockfile
    // perfectly, having claimed nothing at all.
    assert!(declared >= 20, "only {declared} rust crates declared - the register lost entries");
    assert!(wrong.is_empty(), "the register disagrees with Cargo.lock: {wrong:?}");
}

/// Direction two: the build must not carry a crate nobody declared or excused.
#[test]
fn every_third_party_crate_in_the_lockfile_is_declared_or_excused() {
    let doc = register();
    let declared: BTreeSet<String> = components(&doc)
        .into_iter()
        .filter(|(_, _, kind)| kind == "rust-crate")
        .map(|(name, _, _)| name)
        .collect();
    let excused: BTreeSet<&str> = BUILD_ONLY.iter().map(|(name, _)| *name).collect();
    let ours = workspace_crates();

    let mut undeclared: Vec<String> = Vec::new();
    for (name, version) in lockfile_packages() {
        if ours.contains(name.as_str()) || declared.contains(&name) || excused.contains(name.as_str()) {
            continue;
        }
        undeclared.push(format!("{name} {version}"));
    }

    assert!(
        undeclared.is_empty(),
        "a crate is in the build and in neither list. If it reaches a user's disk, declare it in \
         packaging/components.json AND in THIRD-PARTY-NOTICES.md. If it only runs at build time, add \
         it to BUILD_ONLY with the reason - checked with `cargo tree -e normal,no-proc-macro -p \
         chrono-cli` and `-p chrono-hook`: {undeclared:?}"
    );
}

/// The excuse list has to name crates that exist, or it is a list of things nobody has checked since
/// they were removed - the same rot the network register's own guard exists to catch.
#[test]
fn the_build_only_list_names_no_crate_that_is_gone() {
    let present: BTreeSet<String> = lockfile_packages().into_iter().map(|(n, _)| n).collect();
    let stale: Vec<&str> = BUILD_ONLY
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !present.contains(*name))
        .collect();
    assert!(stale.is_empty(), "BUILD_ONLY excuses crates that are no longer in the lockfile: {stale:?}");
    assert!(
        BUILD_ONLY.iter().all(|(_, why)| why.len() > 15),
        "every excuse has to say why, or it is not an excuse"
    );
}

/// Direction three: the machine register and the legal document describe the same set.
///
/// Two files, two audiences - a person reading THIRD-PARTY-NOTICES.md for the licence texts, and a
/// tool reading the SBOM built from the register. They may be rendered differently and they may not
/// disagree about WHAT is in the package.
#[test]
fn every_component_in_the_register_is_named_in_the_shipped_notices() {
    let path = repo_root().join("THIRD-PARTY-NOTICES.md");
    let notices = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the notices file must be readable at {}: {e}", path.display()));

    let mut missing: Vec<String> = Vec::new();
    for (name, version, _) in components(&register()) {
        if !notices.contains(&name) {
            missing.push(format!("{name} is declared and the notices file does not name it"));
            continue;
        }
        // NOASSERTION is what a component with no version of its own carries, and there is nothing
        // to check for it. Everything else must appear with its exact version.
        if version != "NOASSERTION" && !notices.contains(&version) {
            missing.push(format!("{name}: the notices file does not carry version {version}"));
        }
    }
    assert!(missing.is_empty(), "the register and the notices file disagree: {missing:?}");
}

/// Direction four: the bytes of every managed package we ship are written down.
///
/// Cargo.lock has carried a checksum for every third-party crate since the day there were crates. The
/// managed half had nothing at all until 2026-09-08: `WPF-UI 4.3.0` was fetched fresh on every restore
/// with no record of what arrived. `RestorePackagesWithLockFile` fixes that, and this makes sure the
/// record keeps existing - a lock file is the kind of thing that gets deleted to make a build go green.
///
/// The runtime packs are deliberately not here. They come from the SDK rather than from a
/// PackageReference, so no lock file can carry them, and the packaging script checks them against the
/// deps.json instead.
#[test]
fn every_nuget_component_the_register_declares_has_its_bytes_pinned() {
    let path = repo_root().join("gui").join("ChronoMock.App").join("packages.lock.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the NuGet lock file must exist at {} - without it nothing records which bytes a restore \
             pulled down: {e}",
            path.display()
        )
    });
    let lock: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("the lock file must be valid JSON: {e}"));

    // Two targets since the window package also publishes for a runtime identifier, and the lock
    // file nests every package under each of them. Looking through all of them is deliberate.
    let frameworks = lock["dependencies"].as_object().expect("the lock file must list dependencies");
    let mut checked = 0usize;
    let mut missing: Vec<String> = Vec::new();
    for (name, version, kind) in components(&register()) {
        if kind != "nuget" {
            continue;
        }
        checked += 1;
        let entry = frameworks.values().find_map(|packages| packages.get(&name));
        match entry {
            None => missing.push(format!("{name} is declared and the lock file does not carry it")),
            Some(entry) => {
                if entry["resolved"].as_str() != Some(version.as_str()) {
                    missing.push(format!(
                        "{name}: register says {version}, the lock file resolved {}",
                        entry["resolved"].as_str().unwrap_or("nothing")
                    ));
                }
                if entry["contentHash"].as_str().unwrap_or("").is_empty() {
                    missing.push(format!("{name} has no contentHash, so its bytes are not pinned"));
                }
            }
        }
    }

    assert!(checked >= 2, "only {checked} nuget components checked - the register lost entries");
    assert!(missing.is_empty(), "the register and the NuGet lock file disagree: {missing:?}");
}

/// The code-signing pin, which is the other half of "what exactly are we shipping".
///
/// The register says what is in the package. This says who signed it, and it is pinned by fingerprint
/// because a second code-signing certificate on the signing machine - a renewal, a test one, one from
/// another project - would sign just as willingly and the release page would look identical. The
/// signing script reads the certificate back OUT of each signed file and compares it to this value.
///
/// What must NOT be here is the certificate's subject: one issued to an individual carries their name,
/// town and province, and that exposure is the certificate's to make rather than the repository's.
/// `tests/names.rs` scans for it. This checks the shape of what IS here.
#[test]
fn the_code_signing_certificate_is_pinned_by_fingerprint_and_nothing_more() {
    let path = repo_root().join("packaging").join("codesign.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the signing pin must exist at {}: {e}", path.display()));
    let pin: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("the pin must be valid JSON: {e}"));

    assert_eq!(pin["schema"], "chronomock.codesign/1");

    let fingerprint = pin["certificate_sha256"].as_str().unwrap_or_default();
    assert_eq!(fingerprint.len(), 64, "a SHA-256 fingerprint is 64 characters, got {fingerprint:?}");
    assert!(
        fingerprint.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "the fingerprint must be lowercase hex, got {fingerprint:?}"
    );

    // 🔴 A timestamp is not optional. Without one the signature dies when the certificate expires, and
    // this certificate is valid for a year - so every release older than that would stop verifying.
    let timestamp = pin["timestamp_url"].as_str().unwrap_or_default();
    assert!(
        timestamp.starts_with("http://") || timestamp.starts_with("https://"),
        "the pin must name an RFC 3161 timestamp authority, got {timestamp:?}"
    );

    // The subject would be the natural thing to write next to a fingerprint, and it is the one thing
    // that must not be. Checked here as well as in the scan, because this is the file where somebody
    // would put it.
    for field in ["subject", "certificate_subject", "holder", "name"] {
        assert!(pin[field].is_null(), "the pin carries '{field}' - only the fingerprint belongs in it");
    }
}

/// The register's own shape, so a truncated or half-edited file fails here rather than three steps
/// later inside the SBOM generator, where the message would be about JSON.
#[test]
fn the_register_is_well_formed() {
    let doc = register();
    assert_eq!(doc["schema"], "chronomock.components/1");
    assert_eq!(doc["product"]["license"], "GPL-3.0-only");
    assert!(doc["packages"]["cli"]["zip"].is_string(), "each package must name its zip");
    assert!(doc["packages"]["gui"]["zip"].is_string(), "each package must name its zip");

    let all = components(&doc);
    assert!(all.len() >= 25, "only {} components - the register lost entries", all.len());

    let kinds: BTreeSet<&str> = ["rust-crate", "vendored-c", "nuget", "dotnet-runtime-pack"].into_iter().collect();
    for component in doc["components"].as_array().expect("components") {
        let name = component["name"].as_str().expect("a name");
        assert!(
            kinds.contains(component["kind"].as_str().expect("a kind")),
            "{name} has a kind nothing knows how to render"
        );
        for key in ["license_declared", "license_concluded", "supplier", "source"] {
            let value = component[key].as_str().unwrap_or("");
            assert!(!value.is_empty(), "{name} is missing {key}, which the SBOM cannot invent");
        }
        let packages = component["in"].as_array().expect("a component must say which package it is in");
        assert!(!packages.is_empty(), "{name} is in no package, so it ships nowhere");
        for package in packages {
            let id = package.as_str().expect("a package id");
            assert!(doc["packages"][id].is_object(), "{name} is in unknown package '{id}'");
        }
    }

    // Every name distinct: the SBOM turns these into SPDX identifiers, and two components sharing one
    // would produce a document that says different things about the same element.
    let names: BTreeSet<String> = all.iter().map(|(n, _, _)| n.clone()).collect();
    assert_eq!(names.len(), all.len(), "two components share a name");
}

/// Publishing the window package self-contained pulls the runtime packs, which changes the
/// dependency graph, so a locked restore needs that runtime identifier to be IN the lock file.
/// Locked mode is switched on by `CI` being set, which a runner does and a developer box does not,
/// so a lock file without it restores happily here and fails every runner with NU1004. That is
/// what killed the second attempt at tagging 0.2.0, one failure after the first.
///
/// The identifier is read out of the packaging script rather than written here twice. A guard that
/// keeps its own copy of the value it is guarding agrees with itself and with nothing else.
#[test]
fn the_gui_lock_files_carry_the_identifier_the_packaging_script_publishes_for() {
    let script_path = repo_root().join("packaging").join("build-dist.ps1");
    let script = std::fs::read_to_string(&script_path)
        .unwrap_or_else(|e| panic!("the packaging script must be readable: {e}"));

    // PowerShell wraps the publish onto a second line with a backtick, so the identifier is not on
    // the line that names the command. Join the continuation before looking for it.
    let lines: Vec<&str> = script.lines().collect();
    let Some(start) = lines
        .iter()
        .position(|line| line.trim_start().starts_with("dotnet publish"))
    else {
        panic!(
            "the packaging script no longer publishes the window package, so this guard is \
             watching nothing"
        )
    };
    let mut command = String::from(lines[start]);
    let mut index = start;
    while lines[index].trim_end().ends_with('`') && index + 1 < lines.len() {
        index += 1;
        command.push(' ');
        command.push_str(lines[index]);
    }

    let mut tokens = command.split_whitespace();
    let mut rid = None;
    while let Some(token) = tokens.next() {
        if token == "-r" || token == "--runtime" {
            rid = tokens.next().map(str::to_owned);
            break;
        }
    }
    let Some(rid) = rid else {
        panic!("the publish command names no runtime identifier, so nothing here can be checked")
    };

    // Both, because the library is restored along with the application it is referenced by, and the
    // first failure named both projects.
    for project in ["ChronoMock.App", "ChronoMock.Protocol"] {
        let path = repo_root().join("gui").join(project).join("packages.lock.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{project} must have a lock file: {e}"));
        let lock: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{project}'s lock file must be valid JSON: {e}"));
        let targets = lock["dependencies"]
            .as_object()
            .expect("the lock file must list dependencies");
        let suffix = format!("/{rid}");
        assert!(
            targets.keys().any(|target| target.ends_with(&suffix)),
            "{project}'s lock file carries no target for {rid}, so a locked restore for that \
             identifier fails with NU1004 - invisible on a developer box, fatal on every runner. \
             Regenerate with `dotnet restore gui/ChronoMock.slnx --force-evaluate` and commit the \
             lock files. Targets present: {:?}",
            targets.keys().collect::<Vec<_>>()
        );
    }
}
