//! Code-hygiene guards that scale with the codebase.
//!
//! Nobody reads 18 000 lines of Rust and 11 000 of C# line by line. A rule written only in prose
//! erodes silently, and a guard promised in a document but absent from the code is worse than none
//! (untouchable rule 12). These are the rules this project could not otherwise keep.
//!
//! 🔴 **The file list is an ALLOW-list of trees, and that is the whole safety argument.** `tools/`,
//! `docs/`, `CLAUDE.md` and `CHANGELOG-DEV.md` exist on the maintainer's machine and in NO checkout -
//! they are outside git by design. A scan that walked the repository root would find a name used
//! here and dead in CI: green locally, red on the pull request, over a difference invisible in the
//! diff. Naming the trees that are actually committed makes that impossible rather than merely
//! guarded against. Measured 2026-09-06: restricting the scan to tracked trees changes nothing
//! today (the same single finding either way), so it costs nothing now and stops costing something
//! later.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// Shared: what "the repository" means to these guards
// ---------------------------------------------------------------------------------------------

/// Committed trees. Not `git ls-files`: shelling out to git would make the tests depend on a git
/// checkout, and a written list is also documentation of what the scan believes the repository is.
const TRACKED_TREES: &[&str] = &[
    "crates",
    "gui",
    "site",
    "calendars",
    "presets",
    "packaging",
    "assets",
    ".github",
];

/// Committed files in the root.
const ROOT_FILES: &[&str] = &[
    "README.md",
    "CHANGELOG.md",
    "THIRD-PARTY-NOTICES.md",
    "Cargo.toml",
    "deny.toml",
    "global.json",
];

/// Build output and editor state, which are in no checkout either.
const SKIP_DIRS: &[&str] = &["target", "obj", "bin", "node_modules", ".vs", ".git"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Every committed file under the allow-listed trees whose name ends in one of `extensions`.
fn tracked_files(extensions: &[&str]) -> Vec<PathBuf> {
    let root = repo_root();
    let mut out = Vec::new();
    for tree in TRACKED_TREES.iter().copied() {
        let mut stack = vec![root.join(tree)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue; // a tree that does not exist yet is not a failure
            };
            for entry in entries {
                let path = entry.expect("directory entry").path();
                if path.is_dir() {
                    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    if !SKIP_DIRS.contains(&name.as_str()) {
                        stack.push(path);
                    }
                } else if has_extension(&path, extensions) {
                    out.push(path);
                }
            }
        }
    }
    for name in ROOT_FILES.iter().copied() {
        let path = root.join(name);
        if path.is_file() && has_extension(&path, extensions) {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    extensions.contains(&ext)
}

/// Path relative to the repository root, with forward slashes, for messages that read the same on
/// every machine.
fn rel(path: &Path) -> String {
    let root = repo_root().canonicalize().expect("repo root exists");
    let full = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    full.strip_prefix(&root)
        .unwrap_or(&full)
        .to_string_lossy()
        .replace('\\', "/")
}

/// A source line with its line comment removed, so a rule can be WRITTEN ABOUT in the very file it
/// governs. Deliberately naive about strings: a forbidden API named inside a string literal is
/// still worth a red test.
fn code_only(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with('#') && trimmed.starts_with("#!") {
        return "";
    }
    match line.find("//") {
        Some(cut) => &line[..cut],
        None => line,
    }
}

// ---------------------------------------------------------------------------------------------
// H1. The system clock is never set
// ---------------------------------------------------------------------------------------------

/// This test file, which several scans must skip: it names what it forbids.
fn is_this_file(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some("hygiene.rs")
}

/// Win32 and shell entry points that change the machine's own clock or time zone.
///
/// `Win32_System_Time` is already enabled in the `windows` crate features (the zone TYPES come from
/// there), so `SetSystemTime` is one line away from compiling at any moment.
const CLOCK_SETTERS: &[&str] = &[
    "SetSystemTime",
    "SetLocalTime",
    "SetSystemTimeAdjustment",
    "SetTimeZoneInformation",
    "SetDynamicTimeZoneInformation",
    "NtSetSystemTime",
    "w32tm",
];

/// Untouchable rule 1: the system clock is never changed, in any mode. That is not one rule among
/// several - it is the entire promise of the product (chrono-mock.md section 10), and until this
/// test existed nothing checked it.
#[test]
fn nothing_in_the_repository_sets_the_system_clock() {
    let mut offenders = Vec::new();
    let files = tracked_files(&["rs", "cs", "ps1", "yml", "xaml"]);
    for path in &files {
        // 🔴 Except this file, and it is not an optimisation. `CLOCK_SETTERS` lives here, so every
        // name the guard watches is also written here - and the guard would report ITSELF as the
        // violation while saying nothing about the seven names it is meant to watch. It disarms
        // itself in the act of listing what it forbids.
        if is_this_file(path) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            let code = code_only(line);
            for api in CLOCK_SETTERS.iter().copied() {
                if code.contains(api) {
                    offenders.push(format!("{}:{} calls {api}", rel(path), number + 1));
                }
            }
        }
    }

    // The canary every scan here carries: one that reads nothing finds no violation and looks
    // exactly like one that works.
    assert!(
        files.len() >= 40,
        "the clock-setter scan read only {} files - it is looking in the wrong place",
        files.len()
    );
    assert!(
        offenders.is_empty(),
        "untouchable rule 1 violated - Chrono Mock never changes the system clock: {offenders:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// H4. The injected detours neither allocate nor panic
// ---------------------------------------------------------------------------------------------

/// Things a detour must not do. Allocation and formatting are the performance half; panicking is
/// the correctness half, and the more serious one: a Rust panic unwinding across the `extern
/// "system"` boundary into the target's own code is undefined behaviour, which is why the root
/// `Cargo.toml` also turns overflow checks OFF for `chrono-hook` and `chrono-ctl` in release.
const DETOUR_FORBIDDEN: &[&str] = &[
    "format!",
    "String::",
    "Vec::",
    "vec!",
    "to_string()",
    "to_owned()",
    "Box::",
    "println!",
    "eprintln!",
    "panic!",
    ".unwrap()",
    ".expect(",
];

/// Every detour in the hook, by the naming convention the file already follows.
fn hook_detours(text: &str) -> BTreeMap<String, (usize, usize)> {
    let mut out = BTreeMap::new();
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let Some(rest) = line.strip_prefix("unsafe extern \"system\" fn h_") else {
            continue;
        };
        let name = format!(
            "h_{}",
            rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .next()
                .unwrap_or("")
        );
        let mut depth = 0i32;
        let mut seen = false;
        let mut end = i;
        for (j, l) in lines.iter().enumerate().skip(i) {
            depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
            if l.contains('{') {
                seen = true;
            }
            if seen && depth == 0 {
                end = j;
                break;
            }
        }
        out.insert(name, (i, end));
    }
    out
}

/// The hot path of the product: 36 detours running inside someone else's process.
///
/// Scoped to the detours rather than the file, because the INSTALLATION path (`DllMain`, hook
/// registration) legitimately formats diagnostic strings - measured, all twelve `format!` calls in
/// the hook live there. The rule is about what runs on every clock read.
#[test]
fn hook_detours_do_not_allocate_or_panic() {
    let path = repo_root().join("crates/hook/src/lib.rs");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let detours = hook_detours(&text);
    let lines: Vec<&str> = text.lines().collect();

    let mut offenders = Vec::new();
    for (name, (start, end)) in &detours {
        for (offset, line) in lines[*start..=*end].iter().enumerate() {
            let code = code_only(line);
            for bad in DETOUR_FORBIDDEN.iter().copied() {
                if code.contains(bad) {
                    offenders.push(format!("{name} (line {}) uses {bad}", start + offset + 1));
                }
            }
        }
    }

    // The canary, and it is doing more than usual here: it also catches a RENAMED convention. If
    // the detours stop being called `h_*`, the scan finds nothing and would pass silently.
    assert!(
        detours.len() >= 30,
        "found only {} detours in the hook - the naming convention changed and this guard went \
         blind; fix the scan, not this number",
        detours.len()
    );
    assert!(
        offenders.is_empty(),
        "a detour allocates or can panic, and a panic across the FFI boundary into the target is \
         undefined behaviour: {offenders:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// H5. The logical core never swallows an error
// ---------------------------------------------------------------------------------------------

/// `chrono-core` is the one layer that does no I/O and touches no Windows API: its result is a
/// returned value, never a side effect (docs/07 section 3). A discarded error there is always a
/// hidden defect, so the rule can be absolute - unlike in the mechanism and the CLI, where 74
/// `let _ =` are legitimate Win32 returns (measured 2026-09-06) and a blanket ban would need an
/// exception list longer than itself.
#[test]
fn the_logical_core_never_discards_an_error() {
    let mut files = 0;
    let mut offenders = Vec::new();
    let core = repo_root().join("crates/core/src");
    let mut stack = vec![core];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("chrono-core has a src directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !has_extension(&path, &["rs"]) {
                continue;
            }
            files += 1;
            let text = std::fs::read_to_string(&path).expect("core source is readable");
            // Tests may discard freely - they are asserting, not handling.
            let production = match text.find("#[cfg(test)]") {
                Some(cut) => &text[..cut],
                None => &text[..],
            };
            for (number, line) in production.lines().enumerate() {
                let code = code_only(line);
                for bad in ["let _ = ", ".ok();", "if let Err(_)"] {
                    if code.contains(bad) {
                        offenders.push(format!("{}:{} has `{bad}`", rel(&path), number + 1));
                    }
                }
            }
        }
    }

    assert!(files >= 2, "the core scan read only {files} files");
    assert!(
        offenders.is_empty(),
        "the logical core discarded an error - it does no I/O, so this is always a hidden bug, \
         never a Win32 return worth ignoring: {offenders:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// H6. The dependency direction, for the whole graph
// ---------------------------------------------------------------------------------------------

/// The layering docs/07 section 2 calls an invariant: dependencies point INWARD, and nothing in a
/// lower layer knows a higher one. `architecture.rs` guards one seventh of that sentence -
/// `chrono-core` - and this guards the rest.
///
/// Written as the allowed set rather than a forbidden one: a new crate with no entry here fails
/// until somebody decides where it belongs, which is the point.
const ALLOWED_DEPENDENCIES: &[(&str, &[&str])] = &[
    ("core", &[]),
    ("ctl", &[]),
    ("proto", &["serde", "serde_json"]),
    ("mech", &["chrono-core", "chrono-ctl", "windows"]),
    ("hook", &["chrono-ctl", "minhook", "windows"]),
    (
        "cli",
        &[
            "chrono-core",
            "chrono-mech",
            "chrono-proto",
            "serde",
            "serde_json",
            "windows",
            // Build-only: embeds the Windows version resource into the exe. Not linked into the
            // shipped binary, so it carries no third-party notice - its manifest says so.
            "winresource",
        ],
    ),
    // The site generator takes CHANNEL_COUNT from chrono-ctl at compile time rather than repeating
    // the number in prose; its own manifest carries the reasoning.
    ("site", &["chrono-ctl", "serde", "serde_json"]),
];

#[test]
fn every_crate_depends_only_on_the_layers_below_it() {
    let mut offenders = Vec::new();
    for (crate_name, allowed) in ALLOWED_DEPENDENCIES.iter().copied() {
        let path = repo_root()
            .join("crates")
            .join(crate_name)
            .join("Cargo.toml");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let doc: toml::Table = toml::from_str(&text).expect("crate manifest is valid TOML");
        for table in ["dependencies", "build-dependencies"] {
            let Some(deps) = doc.get(table).and_then(|v| v.as_table()) else {
                continue;
            };
            for name in deps.keys() {
                if !allowed.contains(&name.as_str()) {
                    offenders.push(format!(
                        "chrono-{crate_name} depends on '{name}' in [{table}]"
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the dependency direction of docs/07 section 2 is broken, or a dependency was added \
         without deciding which layer it belongs to: {offenders:?}"
    );
}

/// The guard for the guard above: the allowed set must cover every crate the workspace builds.
/// Without this, a new crate could be added and simply not be listed - and an unlisted crate is an
/// unguarded one, which is the failure this whole file exists to prevent.
#[test]
fn the_allowed_dependency_table_covers_every_crate() {
    let text = std::fs::read_to_string(repo_root().join("Cargo.toml"))
        .expect("workspace manifest is readable");
    let doc: toml::Table = toml::from_str(&text).expect("workspace manifest is valid TOML");
    let members: Vec<String> = doc
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .expect("the workspace lists its members")
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.trim_start_matches("crates/").to_string()))
        .collect();

    let listed: Vec<&str> = ALLOWED_DEPENDENCIES.iter().map(|(n, _)| *n).collect();
    let missing: Vec<&String> = members
        .iter()
        .filter(|m| !listed.contains(&m.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "these workspace crates have no entry in ALLOWED_DEPENDENCIES, so nothing guards their \
         dependency direction: {missing:?}"
    );
}
