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

use std::collections::{BTreeMap, BTreeSet};
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
    "CONTRIBUTING.md",
    "SECURITY.md",
    "CODE_OF_CONDUCT.md",
    "THIRD-PARTY-NOTICES.md",
    "Cargo.toml",
    "clippy.toml",
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

// ---------------------------------------------------------------------------------------------
// H2. Code nothing calls any more (Rust)
// ---------------------------------------------------------------------------------------------
//
// Measured 2026-09-06, and it is why this exists at all: `clippy -D warnings` catches an unused
// PRIVATE item and says nothing about a `pub` one - in a library crate AND in a binary crate. The
// probe that proved it injected both at once, so the silence about `pub` could not be mistaken for
// clippy failing to run. 472 of the workspace's definitions are private and already guarded; these
// 278 were not guarded by anything.

/// One definition the scan knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Definition {
    name: String,
    file: String,
    line: usize,
}

/// Where a mention came from. `Tests` is deliberately not a consumer: a definition whose only
/// callers are its own tests is dead code with a test suite attached - it passes, it reads as
/// maintained, and nothing in the program would notice if it vanished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Site {
    /// Inside the definition at this index in the definition list.
    Inside(usize),
    Tests,
    External,
}

/// The pure half of the dead-code scan: life SPREADS FROM ROOTS, to a fixed point.
///
/// 🔴 The direction matters and is the one place this improves on the file it was modelled on.
/// Accumulating deadness from the leaves - start with everything alive, cross out what has no
/// living mention - handles a CHAIN (a dead caller stops keeping its callee alive) but not a
/// CYCLE: two functions that only name each other each see one live mention and both survive
/// forever. Starting from the roots instead, a cycle nothing outside it reaches is correctly
/// reported. Proven by `two_definitions_that_only_call_each_other_are_both_reported`, which was
/// red until this was turned round.
fn unreferenced(definitions: &[Definition], mentions: &BTreeMap<String, Vec<Site>>) -> Vec<usize> {
    let mut alive: Vec<bool> = definitions
        .iter()
        .map(|d| {
            mentions
                .get(&d.name)
                .is_some_and(|sites| sites.contains(&Site::External))
        })
        .collect();
    loop {
        let mut grew = false;
        for (index, definition) in definitions.iter().enumerate() {
            if alive[index] {
                continue;
            }
            let reached = mentions.get(&definition.name).is_some_and(|sites| {
                sites.iter().any(|site| match site {
                    Site::External => true,
                    // A test is not a consumer, and a definition naming itself is recursion.
                    Site::Tests => false,
                    Site::Inside(other) => *other != index && alive[*other],
                })
            });
            if reached {
                alive[index] = true;
                grew = true;
            }
        }
        if !grew {
            return alive
                .iter()
                .enumerate()
                .filter_map(|(i, a)| (!a).then_some(i))
                .collect();
        }
    }
}

/// 🔴 Unreferenced ON PURPOSE, each with the reason it stays. A RATCHET: it may shrink and may not
/// grow without somebody deciding that it should. A list that absorbs whatever the scan finds is
/// not a guard, it is a place to put things.
const KNOWN_UNUSED: &[(&str, &str)] = &[];

fn production_rust(text: &str) -> &str {
    let mut from = 0;
    while let Some(hit) = text[from..].find("#[cfg(test)]") {
        let at = from + hit;
        if text[at + "#[cfg(test)]".len()..].trim_start().starts_with("mod tests") {
            return &text[..at];
        }
        from = at + "#[cfg(test)]".len();
    }
    text
}

/// Top-level `pub` definitions of a Rust source, with the line each one starts on.
///
/// Trait implementations are skipped whole: the language calls `fmt`, `from` and `drop` for you, so
/// nothing in the codebase names them. The cost is named rather than discovered later - a method in
/// such a block that really did die is invisible here. Fewer false accusations, more misses, which
/// is the right way round for a guard people have to believe.
fn pub_definitions(rel_path: &str, text: &str) -> Vec<Definition> {
    let mut out = Vec::new();
    let mut in_trait_impl = false;
    let mut depth = 0i32;
    for (index, line) in text.lines().enumerate() {
        if !in_trait_impl && line.starts_with("impl ") && line.contains(" for ") {
            in_trait_impl = true;
            depth = 0;
        }
        if in_trait_impl {
            depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if depth <= 0 && line.contains('}') {
                in_trait_impl = false;
            }
            continue;
        }
        let Some(rest) = line.strip_prefix("pub ").or_else(|| {
            line.strip_prefix("pub(crate) ")
                .or_else(|| line.strip_prefix("pub(super) "))
        }) else {
            continue;
        };
        // An export the operating system calls by address: nothing in this repository names it.
        if index > 0 && text.lines().nth(index - 1).is_some_and(|l| l.contains("no_mangle")) {
            continue;
        }
        // 🔴 `const` is a MODIFIER only in `const fn`. Treating it as one unconditionally made the
        // scan skip every `pub const NAME` in the workspace - including `CTL_SECTION_NAME`, the one
        // finding the prototype had already produced. A scan that silently drops a whole kind of
        // definition is the failure this file's canaries exist to catch, and this one slipped past
        // them because the count stayed plausible.
        let mut words: Vec<&str> = rest.split_whitespace().collect();
        while let Some(first) = words.first().copied() {
            let is_modifier = matches!(first, "unsafe" | "async" | "default" | "extern")
                || first.starts_with('"')
                || (first == "const" && words.get(1) == Some(&"fn"));
            if is_modifier {
                words.remove(0);
            } else {
                break;
            }
        }
        let Some(kind) = words.first().copied() else { continue };
        let mut words = words.into_iter().skip(1);
        if !["fn", "struct", "enum", "trait", "union", "type", "const", "static"].contains(&kind) {
            continue;
        }
        let Some(raw) = words.next() else { continue };
        let name: String = raw
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() || name.starts_with('_') || name == "main" {
            continue;
        }
        out.push(Definition {
            name,
            file: rel_path.to_string(),
            line: index + 1,
        });
    }
    out
}

fn is_identifier_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Every identifier on a line, as whole words.
///
/// 🔴 EVERY word counts, including one inside a comment or a string, and that is a deliberate trade
/// rather than laziness: prose that still names a symbol is a sign somebody thinks it is alive, and
/// a guard that accuses living code is a guard people learn to ignore. The price is the other
/// direction - a definition whose name is an ordinary English word survives on prose alone - so this
/// catches an abandoned helper with a distinctive name and is not a substitute for reading.
fn identifiers(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices() {
        match (is_identifier_char(c), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push(&line[s..i]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(&line[s..]);
    }
    out
}

/// Collect the whole workspace: definitions, and where every name is mentioned from.
fn collect_rust() -> (Vec<Definition>, BTreeMap<String, Vec<Site>>, usize) {
    let root = repo_root();
    let mut definitions = Vec::new();
    let mut sources = Vec::new();
    for path in tracked_files(&["rs"]) {
        let r = rel(&path);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let is_production_source = r.starts_with("crates/") && r.contains("/src/");
        if is_production_source {
            definitions.extend(pub_definitions(&r, production_rust(&text)));
        }
        sources.push((r, text, is_production_source));
    }
    // Non-Rust consumers: the GUI mirrors two Rust constants by name, the workflows name binaries,
    // the packaging script names files.
    for path in tracked_files(&["cs", "ps1", "yml", "xaml", "toml", "json"]) {
        if is_this_file(&path) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            sources.push((rel(&path), text, false));
        }
    }
    let _ = &root;

    // Which item encloses a line: the last TOP-LEVEL thing that starts at or before it.
    //
    // 🔴 The boundaries come from every top-level item, not only the tracked `pub` ones, and that is
    // a correctness fix rather than a refinement. With only `pub` boundaries, a mention inside a
    // plain `impl` block was attributed to whatever `pub fn` happened to precede it - so
    // `project_fake_ft`, called from a method eleven lines below its own definition, looked like it
    // was calling ITSELF and was reported dead. A boundary that maps to no tracked definition means
    // a production consumer this scan does not model, which counts as life: the safe direction.
    let mut boundaries: BTreeMap<&str, Vec<(usize, Option<usize>)>> = BTreeMap::new();
    for (r, text, is_production_source) in &sources {
        if !is_production_source {
            continue;
        }
        let entry = boundaries.entry(r.as_str()).or_default();
        for (index, line) in text.lines().enumerate() {
            if line.starts_with(|c: char| c.is_ascii_lowercase()) {
                entry.push((index + 1, None));
            }
        }
    }
    for (index, definition) in definitions.iter().enumerate() {
        boundaries
            .entry(definition.file.as_str())
            .or_default()
            .push((definition.line, Some(index)));
    }
    for spans in boundaries.values_mut() {
        // A tracked definition and a bare boundary can land on the same line - the definition wins.
        spans.sort_unstable_by_key(|(line, def)| (*line, def.is_none()));
        spans.dedup_by_key(|(line, _)| *line);
    }

    let names: std::collections::BTreeSet<&str> =
        definitions.iter().map(|d| d.name.as_str()).collect();
    let mut mentions: BTreeMap<String, Vec<Site>> = BTreeMap::new();
    let mut files_read = 0;
    for (r, text, is_production_source) in &sources {
        files_read += 1;
        let test_region = if *is_production_source {
            production_rust(text).len()
        } else {
            usize::MAX
        };
        // A test tree is read but its mentions are marked, never counted as life.
        let whole_file_is_tests = r.contains("/tests/") || r.contains(".Tests/");
        let mut offset = 0usize;
        for (index, line) in text.lines().enumerate() {
            let in_tests = whole_file_is_tests || offset > test_region;
            offset += line.len() + 1;
            for word in identifiers(line) {
                if !names.contains(word) {
                    continue;
                }
                let site = if in_tests {
                    Site::Tests
                } else if *is_production_source {
                    match boundaries.get(r.as_str()) {
                        Some(spans) => spans
                            .iter()
                            .rev()
                            .find(|(start, _)| *start <= index + 1)
                            .and_then(|(_, i)| *i)
                            .map_or(Site::External, Site::Inside),
                        None => Site::External,
                    }
                } else {
                    Site::External
                };
                mentions.entry(word.to_string()).or_default().push(site);
            }
        }
    }
    (definitions, mentions, files_read)
}

/// Nothing in the workspace is left over from a change that moved on without it.
///
/// A helper written in one session and superseded in the next keeps compiling, keeps passing, keeps
/// being read as something that matters, and the only thing that notices is a scan.
#[test]
fn no_public_definition_in_the_workspace_is_unreferenced() {
    let (definitions, mentions, files) = collect_rust();
    let dead = unreferenced(&definitions, &mentions);

    // The canary: a scan that read nothing finds no dead code and looks exactly like one that works.
    assert!(
        files >= 60 && definitions.len() >= 150,
        "the dead-code scan read {files} files and found {} definitions - it is reading the wrong \
         place, which is worse than not reading at all",
        definitions.len()
    );

    let known: Vec<&str> = KNOWN_UNUSED.iter().map(|(n, _)| *n).collect();
    let unexpected: Vec<String> = dead
        .iter()
        .map(|i| &definitions[*i])
        .filter(|d| !known.contains(&d.name.as_str()))
        .map(|d| format!("{} ({}:{})", d.name, d.file, d.line))
        .collect();
    assert!(
        unexpected.is_empty(),
        "these public definitions are named from nowhere that is alive - delete one, or add it to \
         KNOWN_UNUSED with the reason it stays: {unexpected:?}"
    );
}

/// A name that got a caller back must LEAVE the list, or the list rots.
///
/// Without this, an exception written once outlives its reason and the next session reads it as a
/// rule. The cheap direction is free, the other one is a decision.
#[test]
fn the_known_unused_list_only_ever_shrinks() {
    let (definitions, mentions, _files) = collect_rust();
    let dead: Vec<&str> = unreferenced(&definitions, &mentions)
        .iter()
        .map(|i| definitions[*i].name.as_str())
        .collect();
    let revived: Vec<&str> = KNOWN_UNUSED
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| !dead.contains(n))
        .collect();
    assert!(
        revived.is_empty(),
        "these names are used again, so they must come out of KNOWN_UNUSED: {revived:?}"
    );
}

// --- the guard for the guard ------------------------------------------------------------------
//
// The scan's own correctness, on synthetic input rather than on a real symbol. The file it was
// modelled on uses a live symbol as the probe, which works only for as long as that symbol exists.

fn fixture(name: &str, line: usize) -> Definition {
    Definition {
        name: name.to_string(),
        file: "fixture.rs".to_string(),
        line,
    }
}

#[test]
fn a_definition_only_its_tests_name_is_reported_unused() {
    let defs = vec![fixture("only_tested", 1)];
    let mentions = BTreeMap::from([("only_tested".to_string(), vec![Site::Tests, Site::Tests])]);
    assert_eq!(
        unreferenced(&defs, &mentions),
        vec![0],
        "a test mention is being counted as a consumer again"
    );
}

#[test]
fn a_definition_named_from_a_living_site_is_left_alone() {
    let defs = vec![fixture("used", 1), fixture("caller", 10)];
    let mentions = BTreeMap::from([
        ("used".to_string(), vec![Site::Inside(1)]),
        ("caller".to_string(), vec![Site::External]),
    ]);
    assert!(unreferenced(&defs, &mentions).is_empty());
}

#[test]
fn two_definitions_that_only_call_each_other_are_both_reported() {
    let defs = vec![fixture("a", 1), fixture("b", 10)];
    let mentions = BTreeMap::from([
        ("a".to_string(), vec![Site::Inside(1)]),
        ("b".to_string(), vec![Site::Inside(0)]),
    ]);
    assert_eq!(
        unreferenced(&defs, &mentions),
        vec![0, 1],
        "a dead caller is keeping its callee alive - the scan is not reaching a fixed point"
    );
}

#[test]
fn a_definition_that_only_names_itself_is_reported() {
    let defs = vec![fixture("recursive", 1)];
    let mentions = BTreeMap::from([("recursive".to_string(), vec![Site::Inside(0)])]);
    assert_eq!(
        unreferenced(&defs, &mentions),
        vec![0],
        "recursion is being counted as life"
    );
}

// ---------------------------------------------------------------------------------------------
// H3. Code nothing calls any more (C#)
// ---------------------------------------------------------------------------------------------
//
// Measured 2026-09-06: a `private static int UnusedProbe() => 42;` added to `ProtocolJson.cs`
// compiles with "Ostrzezenia: 0" despite `EnforceCodeStyleInBuild` and `TreatWarningsAsErrors`.
// IDE0051 does not reach warning severity here, so on the C# side NOTHING catches dead code - not
// private members, not public ones.
//
// 🔴 This lives in the Rust suite on purpose. The fixed point, the ratchet, the canary and the
// rule that a test is not a consumer are one algorithm, and writing it twice would give two
// implementations to keep in step - the very thing the rest of this file exists to prevent. The
// price is that a C#-only change gets its red from `cargo test`, which `tools/gates.ps1` runs
// anyway.

/// Members the framework calls, so nothing in the codebase names them. The same trade as skipping
/// Rust trait implementations, written as exact names rather than a pattern so a future method
/// cannot quietly join the list by being called something similar.
const CS_FRAMEWORK_MEMBERS: &[&str] = &[
    "Convert",
    "ConvertBack",
    "Dispose",
    "DisposeAsync",
    "ToString",
    "Equals",
    "GetHashCode",
    "InitializeComponent",
    "Main",
    "OnStartup",
    "OnExit",
    "OnClosed",
];

/// C# definitions of one file: types and members, at any nesting.
fn cs_definitions(rel_path: &str, text: &str) -> Vec<Definition> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line.starts_with("//") || line.starts_with('*') {
            continue;
        }
        let Some(rest) = line
            .strip_prefix("public ")
            .or_else(|| line.strip_prefix("internal "))
            .or_else(|| line.strip_prefix("protected "))
            .or_else(|| line.strip_prefix("private "))
        else {
            continue;
        };
        // A property or field the deserialiser fills is consumed by a caller nobody writes.
        let attributed = index > 0 && lines[index - 1].contains("[Json");
        if attributed || raw.contains("[Json") {
            continue;
        }
        let mut words = rest.split_whitespace().peekable();
        // An override answers a base class, and a partial half is not a definition of its own.
        let mut is_override = false;
        while let Some(word) = words.peek() {
            if ["static", "sealed", "abstract", "readonly", "virtual", "async", "new", "required",
                "partial", "const", "extern", "unsafe", "override", "event"]
                .contains(word)
            {
                if *word == "override" {
                    is_override = true;
                }
                words.next();
            } else {
                break;
            }
        }
        if is_override {
            continue;
        }
        let Some(first) = words.next() else { continue };
        // A constructor has no return type, so the name IS the first word and it carries the open
        // parenthesis. It is not a definition of its own either: `new Thing(...)` names the TYPE,
        // which is tracked, so counting the constructor separately reported its first PARAMETER as
        // dead code - `roleKey`, `fullPath` and `chronoPath` all arrived that way.
        if first.contains('(') {
            continue;
        }
        let name = if ["class", "record", "struct", "enum", "interface"].contains(&first) {
            let candidate = words.next().unwrap_or("");
            // `record struct X` and `record class X` put the shape between the two.
            if ["struct", "class"].contains(&candidate) {
                words.next().unwrap_or("")
            } else {
                candidate
            }
        } else {
            // `Type Name(...)`, `Type Name { get; }`, `Type Name =>`, `Type Name;`
            words.next().unwrap_or("")
        };
        let name: String = name
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() || CS_FRAMEWORK_MEMBERS.contains(&name.as_str()) {
            continue;
        }
        out.push(Definition {
            name,
            file: rel_path.to_string(),
            line: index + 1,
        });
    }
    out
}

/// 🔴 The C# ratchet. Same rules as the Rust one: a reason, not a shrug, and it may only shrink.
///
/// "Nobody has decided yet" is a reason as long as it SAYS so - the point of this list is that an
/// unused name carries one, and that the next session reads the reason rather than the silence.
const CS_KNOWN_UNUSED: &[(&str, &str)] = &[
    (
        "AvailableCultures",
        "Discovers which translation files ship, for a language picker that does not exist.          `App.xaml.cs` applies `DefaultCulture` at startup and nothing ever changes it, so the          shipped `Strings.pl.json` is unreachable from the interface. That gap is the decision,          not this method - it goes when the picker is built or when the owner rules PL out.",
    ),
    (
        "AvailableCulturesIn",
        "The folder-explicit half of `AvailableCultures`, split out so the odd-name cases can be          tested directly. It lives and dies with its pair.",
    ),
    (
        "QueryCommand",
        "Protocol surface that is deliberately unsent. The core knows `query`          (`chrono_proto::Command::Query`) and this is the client half of it, but the core already          beats `state` about once a real second, so no client has needed to ask. Deleting it          would make the C# client a subset of the contract rather than a mirror of it.",
    ),
    (
        "Launch",
        "`CoreClient.Launch` spawns the core and sends `start` in one step, for callers that do          not gate on `ready`. Its own XML comment says the conformance tests use it, and they are          its only callers - the GUI goes through `Connect` and waits. It exists FOR the tests,          which is a reason, and deleting it deletes what those five conformance tests drive.",
    ),
];

fn collect_cs() -> (Vec<Definition>, BTreeMap<String, Vec<Site>>, usize) {
    let mut definitions = Vec::new();
    let mut sources = Vec::new();
    for path in tracked_files(&["cs", "xaml", "csproj", "slnx"]) {
        let r = rel(&path);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let is_test = r.contains(".Tests/") || r.contains("TestTarget");
        if !is_test && r.ends_with(".cs") {
            definitions.extend(cs_definitions(&r, &text));
        }
        sources.push((r, text, is_test));
    }

    let mut boundaries: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for (index, definition) in definitions.iter().enumerate() {
        boundaries
            .entry(definition.file.as_str())
            .or_default()
            .push((definition.line, index));
    }
    for spans in boundaries.values_mut() {
        spans.sort_unstable();
    }

    let names: std::collections::BTreeSet<&str> =
        definitions.iter().map(|d| d.name.as_str()).collect();
    let mut mentions: BTreeMap<String, Vec<Site>> = BTreeMap::new();
    let mut files_read = 0;
    for (r, text, is_test) in &sources {
        files_read += 1;
        let owns_definitions = boundaries.contains_key(r.as_str());
        for (index, line) in text.lines().enumerate() {
            for word in identifiers(line) {
                if !names.contains(word) {
                    continue;
                }
                let site = if *is_test {
                    Site::Tests
                } else if owns_definitions {
                    boundaries[r.as_str()]
                        .iter()
                        .rev()
                        .find(|(start, _)| *start <= index + 1)
                        .map_or(Site::External, |(_, i)| Site::Inside(*i))
                } else {
                    Site::External
                };
                mentions.entry(word.to_string()).or_default().push(site);
            }
        }
    }
    reach_attached_properties(&definitions, &sources, &mut mentions);
    (definitions, mentions, files_read)
}

/// 🔴 A WPF ATTACHED PROPERTY IS REACHED BY CONVENTION, and this scan reads names. Markup that writes
/// `app:PartState.HasError="True"` never spells `GetHasError`, `SetHasError` or `HasErrorProperty` - the
/// XAML parser finds them by the shape of their names - so every attached property looked dead here. The
/// first answer was to list its accessors as known unused, one property at a time, and the list grew by
/// two with each new property while its own rule says it may only shrink. This reads the convention
/// instead: an accessor lives when non-test XAML uses the property it serves.
///
/// The prefix colon is deliberate. XAML can reach the property only through a namespace prefix
/// (`app:PartState.HasError`), and a comment that says "see PartState.HasError" is not a use.
fn reach_attached_properties(
    definitions: &[Definition],
    sources: &[(String, String, bool)],
    mentions: &mut BTreeMap<String, Vec<Site>>,
) {
    for definition in definitions {
        let Some(usage) = attached_property_usage(definition) else {
            continue;
        };
        let used = sources
            .iter()
            .any(|(path, text, is_test)| !*is_test && path.ends_with(".xaml") && names_whole(text, &usage));
        if used {
            mentions.entry(definition.name.clone()).or_default().push(Site::External);
        }
    }
}

/// How XAML spells a use of the attached property an accessor serves: `GetHasError`, `SetHasError` and
/// `HasErrorProperty` in `PartState.cs` all answer to `:PartState.HasError`. None for a name of no such shape.
fn attached_property_usage(definition: &Definition) -> Option<String> {
    let name = definition.name.as_str();
    let property = name
        .strip_suffix("Property")
        .or_else(|| name.strip_prefix("Get"))
        .or_else(|| name.strip_prefix("Set"))
        .filter(|property| !property.is_empty())?;
    let class = std::path::Path::new(&definition.file).file_stem()?.to_str()?;
    Some(format!(":{class}.{property}"))
}

/// Whether `needle` occurs in `text` as a whole name - `:PartState.HasError`, not `:PartState.HasErrorText`.
fn names_whole(text: &str, needle: &str) -> bool {
    text.match_indices(needle)
        .any(|(at, _)| text[at + needle.len()..].chars().next().is_none_or(|c| !is_identifier_char(c)))
}

#[test]
fn no_definition_in_the_gui_is_unreferenced() {
    let (definitions, mentions, files) = collect_cs();
    let dead = unreferenced(&definitions, &mentions);

    assert!(
        files >= 30 && definitions.len() >= 200,
        "the C# scan read {files} files and found {} definitions - it is reading the wrong place",
        definitions.len()
    );

    let known: Vec<&str> = CS_KNOWN_UNUSED.iter().map(|(n, _)| *n).collect();
    let unexpected: Vec<String> = dead
        .iter()
        .map(|i| &definitions[*i])
        .filter(|d| !known.contains(&d.name.as_str()))
        .map(|d| format!("{} ({}:{})", d.name, d.file, d.line))
        .collect();
    assert!(
        unexpected.is_empty(),
        "these definitions are named from nowhere that is alive - delete one, or add it to \
         CS_KNOWN_UNUSED with the reason it stays: {unexpected:?}"
    );
}

#[test]
fn the_cs_known_unused_list_only_ever_shrinks() {
    let (definitions, mentions, _files) = collect_cs();
    let dead: Vec<&str> = unreferenced(&definitions, &mentions)
        .iter()
        .map(|i| definitions[*i].name.as_str())
        .collect();
    let revived: Vec<&str> = CS_KNOWN_UNUSED
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| !dead.contains(n))
        .collect();
    assert!(
        revived.is_empty(),
        "these names are used again, so they must come out of CS_KNOWN_UNUSED: {revived:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// H7 and H8. What the repository is written in, and how
// ---------------------------------------------------------------------------------------------

/// The kinds of file whose comments start with `#`. The language scan read them for years with
/// `comment_part`, which knows `//` and `<!--` and nothing else, so a Polish `# comment` in a build
/// script or a workflow passed it untouched (found 2026-09-23, reversal probe in `CHANGELOG-DEV.md`).
const HASH_COMMENTED: &[&str] = &["ps1", "yml", "toml"];

/// The comment part of every line of a `#`-commented file, in order: from a `#` that stands outside
/// any string and after whitespace or at the start of the line, and every line of a PowerShell block
/// comment (`<#` to `#>`). The whitespace rule is YAML's own (a `#` inside a plain scalar, as in a
/// URL, is not a comment), and it costs nothing in PowerShell or TOML, where a comment written
/// anywhere else is rare enough not to matter.
///
/// 🔴 A string is ONE active quote, and a string that spans lines is state carried to the next line.
/// The first version counted each kind of quote on its own line, so the apostrophe in `"don't"` hid
/// the comment after it, and the lines of a PowerShell here-string or a TOML `"""` string read as
/// code (found in review, 2026-09-23). The escapes are each language's own: a backtick in PowerShell,
/// where a backslash is an ordinary character in every path, and a backslash in YAML and TOML. A
/// quote opens a string only where one can start - after whitespace or punctuation - because in a
/// YAML plain scalar ("Don't do it") an apostrophe is a letter.
fn hash_comments(text: &str, powershell: bool) -> Vec<Option<&str>> {
    let mut state = HashState::default();
    text.lines()
        .map(|line| {
            if state.block {
                state.block = !line.contains("#>");
                return Some(line);
            }
            let from = match state.closing {
                None => 0,
                Some(close) => {
                    let found = if powershell {
                        let indent = line.len() - line.trim_start().len();
                        line.trim_start().starts_with(close).then_some(indent)
                    } else {
                        line.find(close)
                    };
                    let at = found?;
                    state.closing = None;
                    at + close.len()
                }
            };
            hash_comment_from(line, from, powershell, &mut state)
        })
        .collect()
}

/// What a `#`-commented file is inside of, carried from one line to the next.
#[derive(Default)]
struct HashState {
    /// A PowerShell block comment is open.
    block: bool,
    /// A string that spans lines is open, and this closes it: `"@`, `'@`, `"""` or `'''`.
    closing: Option<&'static str>,
}

/// The comment on one line from byte `from` on, opening a block comment or a string that spans lines
/// when the line leaves one open.
fn hash_comment_from<'a>(line: &'a str, from: usize, powershell: bool, state: &mut HashState) -> Option<&'a str> {
    let escape = if powershell { '`' } else { '\\' };
    let mut quote: Option<char> = None;
    let mut previous = ' ';
    let mut i = from;
    while let Some(c) = line[i..].chars().next() {
        let rest = &line[i..];
        let mut step = c.len_utf8();
        match quote {
            Some('"') if c == escape => step += rest[1..].chars().next().map_or(0, char::len_utf8),
            // A doubled quote is the quote itself in PowerShell and in YAML, not the end of the string.
            Some(q) if c == q && rest[1..].starts_with(q) => step += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if powershell && rest.starts_with("<#") => {
                state.block = !rest.contains("#>");
                return Some(rest);
            }
            None if c == '#' && previous.is_whitespace() => return Some(rest),
            None if powershell && matches!(rest.trim_end(), "@\"" | "@'") => {
                state.closing = Some(if rest.starts_with("@\"") { "\"@" } else { "'@" });
                return None;
            }
            None if !powershell && (rest.starts_with("\"\"\"") || rest.starts_with("'''")) => {
                let delimiter = if rest.starts_with("\"\"\"") { "\"\"\"" } else { "'''" };
                match rest[3..].find(delimiter) {
                    Some(at) => step = 3 + at + 3,
                    None => {
                        state.closing = Some(delimiter);
                        return None;
                    }
                }
            }
            None if (c == '"' || c == '\'') && (previous.is_whitespace() || "=([{,:;|&!".contains(previous)) => {
                quote = Some(c);
            }
            None => {}
        }
        previous = c;
        i += step;
    }
    None
}

/// The comment scan on the shapes that fooled the first version, and on the ones it must leave alone.
#[test]
fn hash_comments_are_read_where_each_language_puts_them() {
    let comments = |text: &str, powershell: bool| -> Vec<Option<String>> {
        hash_comments(text, powershell).into_iter().map(|c| c.map(str::to_string)).collect()
    };
    let one = |text: &str, powershell: bool| comments(text, powershell).remove(0);
    assert_eq!(one("run: echo \"don't stop\" # after", false).as_deref(), Some("# after"));
    assert_eq!(one("name: Don't do it # after", false).as_deref(), Some("# after"));
    assert_eq!(one("$a = \"It's\" # after", true).as_deref(), Some("# after"));
    assert_eq!(one("$p = \"C:\\tools\\\" # after", true).as_deref(), Some("# after"));
    assert_eq!(one("$q = 'can''t' # after", true).as_deref(), Some("# after"));
    assert_eq!(one("$q = 'it''s # inside' # after", true).as_deref(), Some("# after"));
    assert_eq!(one("x = \"a \\\" # not\" # after", false).as_deref(), Some("# after"));
    assert_eq!(one("url: https://example.com/#anchor", false), None);
    assert_eq!(one("$s = \"a # not\"", true), None);

    let here = comments("$s = @\"\n# inside the string\n\"@ # after", true);
    assert_eq!(here, [None, None, Some("# after".to_string())]);
    let toml = comments("s = \"\"\"\n# inside the string\n\"\"\" # after\nt = \"\"\"one line\"\"\" # too", false);
    assert_eq!(toml, [None, None, Some("# after".to_string()), Some("# too".to_string())]);
    let block = comments("<#\ninside\n#>\n$x = 1", true);
    assert_eq!(block, [Some("<#".to_string()), Some("inside".to_string()), Some("#>".to_string()), None]);
}

/// Polish diacritics. Untouchable rule 9 makes the criterion the PLACE, not the reader: everything
/// in the repository is English, everything outside it is Polish.
const POLISH_LETTERS: &[char] = &[
    'ą', 'ć', 'ę', 'ł', 'ń', 'ó', 'ś', 'ź', 'ż', 'Ą', 'Ć', 'Ę', 'Ł', 'Ń', 'Ó', 'Ś', 'Ź', 'Ż',
];

fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with("<!--") || trimmed.starts_with("///")
}

/// The comment part of a line, whether it starts the line or trails code.
///
/// 🔴 `is_comment` above answers "does this line BEGIN with a comment", and the language scan used it
/// as if it answered "does this line contain one". A trailing `// ...` after code was therefore
/// outside the guard entirely - it could say anything in any language and this file would pass.
///
/// 🔴 The first version of this split on the first `//` and claimed in a comment that a `//` inside a
/// string literal was a safe false positive, "because the URLs in this codebase hold no Polish
/// letters". Running it said otherwise at once: a CDP probe builds a URL with `"ó".repeat(60)` as a
/// deliberate over-long path, and `site/i18n/pl.json` uses `"//"` as a KEY for its note fields, whose
/// values are Polish because that file is the Polish half of the site. Three false alarms in the
/// first run, from an assumption written down as a fact.
///
/// So a `//` with an odd number of quotes before it is inside a string and is not a comment. Naive
/// against an escaped quote, and that stays naive on purpose: the alternative is a string parser per
/// language in a guard whose job is to read prose.
fn comment_part(line: &str) -> Option<&str> {
    if is_comment(line) {
        return Some(line);
    }
    let bytes = line.as_bytes();
    let mut quotes = 0usize;
    let mut i = 0;
    while i + 1 < bytes.len() {
        match bytes[i] {
            b'"' => quotes += 1,
            b'/' if bytes[i + 1] == b'/' && quotes.is_multiple_of(2) => return Some(&line[i..]),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether `haystack` holds `word` as a WHOLE word. Substring matching would redden on English that
/// merely contains the letters ("sonda" inside a longer identifier), and a guard with false alarms is
/// a guard that gets suppressed.
fn contains_word(haystack: &str, word: &str) -> bool {
    haystack.split(|c: char| !c.is_alphanumeric()).any(|w| w == word)
}

/// Polish words that survive without their diacritics, for a scan that would otherwise miss them.
///
/// 🔴 The letter scan below catches `zażółć` and nothing about `plasterek`. Both are Polish in a
/// repository whose language is English (rules 9 and 14), and the second kind is what actually
/// accumulated: three uses of `plasterek` and one `jawne` dropped into an English sentence, all four
/// invisible to a guard that was green the whole time. A guard that is green and does not look is the
/// shape rule 12 calls worse than none.
///
/// The list only grows, like `FILE_SUFFIXES` above it. Every entry has to be a word that cannot be
/// English - `to`, `me` and `pole` are Polish too, and are not here, because a list that reddens on
/// English prose gets suppressed rather than fixed.
///
/// The words of this project's own Polish (the first twelve) are here because no translation uses
/// them - they are the words of the work, not of the interface. The next two are names a review found
/// in test code, which the translation vocabulary of H7b does not hold, the next five are the
/// commonest Polish words of all, under that vocabulary's length floor, and the last two stood in a
/// doc comment of `cdp/mod.rs` as "rdzeni<->interfejs" for as long as this scan existed - found by
/// running the translation vocabulary over the comments once (2026-09-23).
const POLISH_WORDS_WITHOUT_DIACRITICS: &[&str] = &[
    "plasterek", "jawne", "sonda", "straznik", "bramka", "wlasciciel", "zmierzone", "cisza",
    "wiec", "dlatego", "poniewaz", "kolejnosc", "sekcji", "urwany", "nie", "czy", "dla", "jak",
    "tak", "rdzeni", "interfejs",
];

/// Untouchable rules 9 and 14: comments in the repository are English, without exception.
///
/// VALUES are a different question and are deliberately not checked. `Strings.pl.json` is Polish
/// from end to end and must be, and `calendars/pl.json` carries `Święto Trzech Króli` because the
/// locale of DATA is not the language of the INTERFACE (rule 15). Only the comments are English.
///
/// Measured when this was written: 37 Polish comment lines in `Strings.pl.json`, all of them
/// translations of comments that already existed in English in the file beside it. They were
/// aligned by key and the values were left untouched - proven by comparing the parsed objects
/// before and after, 285 keys, deep-equal.
#[test]
fn every_comment_in_the_repository_is_english() {
    let files = tracked_files(&["rs", "cs", "xaml", "json", "ps1", "yml", "toml", "md"]);
    let mut offenders = Vec::new();
    for path in &files {
        if is_this_file(path) {
            continue; // POLISH_LETTERS lives here
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let hashed = has_extension(path, HASH_COMMENTED)
            .then(|| hash_comments(&text, has_extension(path, &["ps1"])));
        for (number, line) in text.lines().enumerate() {
            let comment = match &hashed {
                Some(comments) => comments[number],
                None => comment_part(line),
            };
            let Some(comment) = comment else { continue };
            let by_letter = comment.chars().any(|c| POLISH_LETTERS.contains(&c));
            // Words too, because a Polish word with no diacritics in it looks exactly like English to
            // the letter scan - and that is the kind this repository actually accumulated.
            let lowered = comment.to_ascii_lowercase();
            let by_word = POLISH_WORDS_WITHOUT_DIACRITICS
                .iter()
                .any(|w| contains_word(&lowered, w));
            if by_letter || by_word {
                offenders.push(format!("{}:{}", rel(path), number + 1));
            }
        }
    }
    assert!(files.len() >= 60, "the language scan read only {} files", files.len());
    assert!(
        offenders.is_empty(),
        "a comment in the repository is not English (untouchable rules 9 and 14 - the criterion is \
         the PLACE, not the reader): {offenders:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// H7b. Names in code are English too
// ---------------------------------------------------------------------------------------------

/// Words the Polish translations use that are English words as well, so a name made of them is not
/// Polish. Each one met a real English name in the code when this guard was written, and the list
/// grows the same way: when a translation brings in a word an English name already uses. An entry is
/// a word that is PLAINLY English, never a way to let a Polish name through.
const ALSO_ENGLISH: &[&str] = &["problem", "stale"];

/// The shortest word the translation vocabulary keeps. Shorter Polish words are English ones too
/// often ("ten", "pod", "sam"), and the few short ones that matter are in the hand list.
const TRANSLATED_WORD_MIN: usize = 4;

/// A Polish word in the ASCII form a name would carry it in.
fn fold_polish(word: &str) -> String {
    word.chars()
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            other => other,
        })
        .collect()
}

/// The lowercase words of one language's interface text: the GUI strings and the product site, both
/// committed, both translated by hand.
fn interface_words(language: &str) -> BTreeSet<String> {
    let root = repo_root();
    let mut text = String::new();
    // JSON with `//` comment lines - which are English in both files, so they go.
    let strings = std::fs::read_to_string(root.join(format!("gui/ChronoMock.App/Localization/Strings.{language}.json")))
        .expect("GUI strings");
    let data: Vec<&str> = strings.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
    let strings: serde_json::Value = serde_json::from_str(&data.join("\n")).expect("GUI strings are JSON");
    let site = std::fs::read_to_string(root.join(format!("site/i18n/{language}.json"))).expect("site strings");
    let site: serde_json::Value = serde_json::from_str(&site).expect("site strings are JSON");
    for value in [strings, site].iter().filter_map(serde_json::Value::as_object).flat_map(|o| o.values()) {
        text.push_str(value.as_str().unwrap_or_default());
        text.push(' ');
    }
    for page in std::fs::read_dir(root.join("site/pages")).expect("site pages").flatten() {
        if let Ok(html) = std::fs::read_to_string(page.path().join(format!("{language}.html"))) {
            let mut in_tag = false;
            text.extend(html.chars().map(|c| {
                in_tag = (in_tag || c == '<') && c != '>';
                if in_tag || c == '>' { ' ' } else { c }
            }));
        }
    }
    text.split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The Polish words a name might carry, in the ASCII form it would carry them in: the hand list, and
/// every word of the Polish interface that the English one does not use.
///
/// 🔴 Built from the repository's own translations rather than written out, because the leak this
/// guard exists for is ORDINARY Polish - `nazwa_sekcji`, `inny`, `obok`, four names in test code in
/// one pull request, every gate green - and a hand list only ever holds the words somebody already
/// thought of. The translations hold thousands, grow with the interface, and are Polish by
/// definition. Subtracting the English interface takes out the words both share (product names,
/// "moment", "format"), which is also why a name from the English interface can never trip this.
///
/// For NAMES only. Run once over the comments it found 39 words, and all but one were not Polish
/// prose: paths into `docs/zasady/`, the English "alarm", the language's own name in the site's
/// code. The one that was - "rdzeni<->interfejs" - went into the hand list, which the comments keep.
fn polish_vocabulary() -> BTreeSet<String> {
    let english = interface_words("en");
    let mut words: BTreeSet<String> = POLISH_WORDS_WITHOUT_DIACRITICS.iter().map(|w| (*w).to_string()).collect();
    for word in interface_words("pl") {
        let folded = fold_polish(&word);
        if folded.chars().count() >= TRANSLATED_WORD_MIN
            && !english.contains(&word)
            && !english.contains(&folded)
            && !ALSO_ENGLISH.contains(&folded.as_str())
        {
            words.insert(folded);
        }
    }
    words
}

/// Split a name into its words, lowercase: at underscores and digits, and where the case turns
/// (`nazwaSekcji`, `HTTPServer` into `http` and `server`).
fn name_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (k, &c) in chars.iter().enumerate() {
        let prev = k.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(k + 1).copied();
        let turns = c.is_uppercase()
            && prev.is_some_and(|p| p.is_lowercase() || (p.is_uppercase() && next.is_some_and(char::is_lowercase)));
        if !c.is_alphabetic() || turns {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            if !c.is_alphabetic() {
                continue;
            }
        }
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Why a name is Polish, or `None` when it is not.
fn polish_in_name(name: &str, vocabulary: &BTreeSet<String>) -> Option<String> {
    if name.chars().any(|c| POLISH_LETTERS.contains(&c)) {
        return Some("a Polish letter".to_string());
    }
    name_words(name).into_iter().find(|w| vocabulary.contains(w)).map(|w| format!("the Polish word \"{w}\""))
}

/// The names in a Rust or C# source, each with its line, and nothing from its comments, strings or
/// character literals.
///
/// 🔴 A small lexer rather than line matching, because what has to be skipped spans lines: a raw
/// string of test data, a verbatim string, a block comment, and an interpolated C# string whose holes
/// hold quotes of their own - `$"{string.Join(", ", x)}"` ends at the wrong quote for any scan that
/// only counts them, and from there it reads every string as code and every name as a string. Names
/// inside an interpolation hole are skipped with the hole, which is the one blind spot, and a small
/// one: a hole holds an expression, not a declaration.
fn source_names(text: &str, csharp: bool) -> Vec<(usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut lines = Vec::with_capacity(chars.len());
    let mut line = 1;
    for &c in &chars {
        lines.push(line);
        if c == '\n' {
            line += 1;
        }
    }
    let mut names = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = skipped_at(&chars, i, csharp) {
            i = end;
            continue;
        }
        let c = chars[i];
        if c.is_ascii_digit() {
            i = run_end(&chars, i);
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let end = run_end(&chars, i);
            let name: String = chars[i..end].iter().collect();
            match (!csharp).then(|| rust_prefixed_literal(&chars, end, &name)).flatten() {
                Some(after) => i = after,
                None => {
                    names.push((lines[i], name));
                    i = end;
                }
            }
            continue;
        }
        i += 1;
    }
    names
}

/// The end of the letters, digits and underscores starting at `from`.
fn run_end(chars: &[char], from: usize) -> usize {
    chars[from..].iter().position(|c| !(c.is_alphanumeric() || *c == '_')).map_or(chars.len(), |p| from + p)
}

/// Where a comment or a literal that starts at `i` ends, or `None` when nothing to skip starts there.
fn skipped_at(chars: &[char], i: usize, csharp: bool) -> Option<usize> {
    let at = |k: usize| chars.get(k).copied().unwrap_or('\0');
    match (at(i), at(i + 1)) {
        ('/', '/') => Some(chars[i..].iter().position(|&c| c == '\n').map_or(chars.len(), |p| i + p)),
        ('/', '*') => Some(block_comment_end(chars, i, !csharp)),
        ('"', _) if csharp => Some(cs_string_end(chars, i, false, false)),
        ('"', _) => Some(escaped_end(chars, i + 1, '"')),
        ('\'', _) => Some(char_or_lifetime_end(chars, i)),
        ('@' | '$', _) if csharp => cs_prefixed_string_end(chars, i),
        _ => None,
    }
}

/// The end of a block comment, nested when the language nests them (Rust does, C# does not).
fn block_comment_end(chars: &[char], from: usize, nested: bool) -> usize {
    let mut depth = 0usize;
    let mut j = from;
    while j + 1 < chars.len() {
        match (chars[j], chars[j + 1]) {
            ('/', '*') if nested || depth == 0 => {
                depth += 1;
                j += 2;
            }
            ('*', '/') => {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    return j;
                }
            }
            _ => j += 1,
        }
    }
    chars.len()
}

/// The end of a literal closed by `quote`, where a backslash escapes the next character.
fn escaped_end(chars: &[char], from: usize, quote: char) -> usize {
    let mut j = from;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 2,
            c if c == quote => return j + 1,
            _ => j += 1,
        }
    }
    chars.len()
}

/// A character literal - `'x'`, `'\n'`, `'\''` - or, in Rust, the quote of a lifetime or a label,
/// which is only the quote itself: the name after it is read as a name.
fn char_or_lifetime_end(chars: &[char], i: usize) -> usize {
    match (chars.get(i + 1), chars.get(i + 2)) {
        (Some('\\'), _) => escaped_end(chars, i + 1, '\''),
        (Some(_), Some('\'')) => i + 3,
        _ => i + 1,
    }
}

/// A Rust literal behind a prefix that the lexer first read as a name: `r"..."`, `r#"..."#`, `br"..."`,
/// `cr"..."`, `b"..."`, `c"..."` and `b'x'`. `None` when the name is just a name (a raw identifier
/// such as `r#type` included).
fn rust_prefixed_literal(chars: &[char], after: usize, prefix: &str) -> Option<usize> {
    let next = chars.get(after).copied();
    match prefix {
        "r" | "br" | "cr" => {
            let hashes = chars[after..].iter().take_while(|&&c| c == '#').count();
            if chars.get(after + hashes) != Some(&'"') {
                return None;
            }
            let mut j = after + hashes + 1;
            while j < chars.len() {
                if chars[j] == '"' && chars[j + 1..].iter().take(hashes).filter(|&&c| c == '#').count() == hashes {
                    return Some(j + 1 + hashes);
                }
                j += 1;
            }
            Some(chars.len())
        }
        "b" | "c" if next == Some('"') => Some(escaped_end(chars, after + 1, '"')),
        "b" if next == Some('\'') => Some(char_or_lifetime_end(chars, after)),
        _ => None,
    }
}

/// A C# string behind its `@` or `$` prefixes, or `None` when the prefix is something else (`@class`,
/// a verbatim identifier).
fn cs_prefixed_string_end(chars: &[char], i: usize) -> Option<usize> {
    let prefix = chars[i..].iter().take_while(|&&c| c == '@' || c == '$').count();
    if chars.get(i + prefix) != Some(&'"') {
        return None;
    }
    let letters = &chars[i..i + prefix];
    Some(cs_string_end(chars, i + prefix, letters.contains(&'@'), letters.contains(&'$')))
}

/// The end of a C# string whose opening quote is at `i`: a raw one (three quotes or more) closes on
/// the same run of quotes, a verbatim one doubles its quotes instead of escaping them, and an
/// interpolated one has holes, each skipped whole with whatever quotes it holds.
fn cs_string_end(chars: &[char], i: usize, verbatim: bool, interpolated: bool) -> usize {
    let quotes = chars[i..].iter().take_while(|&&c| c == '"').count();
    if quotes >= 3 {
        let mut j = i + quotes;
        while j < chars.len() {
            if chars[j..].iter().take_while(|&&c| c == '"').count() >= quotes {
                return j + quotes;
            }
            j += 1;
        }
        return chars.len();
    }
    let mut j = i + 1;
    while j < chars.len() {
        match (chars[j], chars.get(j + 1).copied()) {
            ('"', Some('"')) if verbatim => j += 2,
            ('"', _) => return j + 1,
            ('\\', _) if !verbatim => j += 2,
            ('{', Some('{')) | ('}', Some('}')) if interpolated => j += 2,
            ('{', _) if interpolated => j = cs_hole_end(chars, j + 1),
            _ => j += 1,
        }
    }
    chars.len()
}

/// The end of an interpolation hole that opened just before `from`: braces counted, and strings and
/// characters inside it skipped whole, so their quotes and braces do not count.
fn cs_hole_end(chars: &[char], from: usize) -> usize {
    let mut depth = 1usize;
    let mut j = from;
    while j < chars.len() {
        if let Some(end) = skipped_at(chars, j, true) {
            j = end;
            continue;
        }
        match chars[j] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    chars.len()
}

/// The names a XAML file gives its elements and resources, each with its line. A value in braces is
/// a markup extension (`{x:Type Button}`), not a name.
fn xaml_names(text: &str) -> Vec<(usize, String)> {
    let mut names = Vec::new();
    for (number, line) in text.lines().enumerate() {
        // The plain `Name` does the same job as `x:Name` on a framework element. The leading space keeps
        // `TargetName`, `SourceName` and `DisplayName` out.
        for attribute in ["x:Name=\"", "x:Key=\"", " Name=\""] {
            for (at, _) in line.match_indices(attribute) {
                let value = &line[at + attribute.len()..];
                let value = &value[..value.find('"').unwrap_or(value.len())];
                if !value.starts_with('{') {
                    names.push((number + 1, value.to_string()));
                }
            }
        }
    }
    names
}

/// Untouchable rule 14: all code is English, names included.
///
/// 🔴 The comment scan above is deliberately blind to code, because values may be Polish (rule 15),
/// and nothing looked at NAMES at all: `nazwa_sekcji`, `inny`, `obok` and `urwany` went into test code
/// in one pull request (2026-09-23) with every gate green, and a review found them. This reads every
/// name the Rust and C# sources declare or use, and every `x:Name` and `x:Key` of the XAML, against
/// the Polish letters and the vocabulary above. Reversal probes in `CHANGELOG-DEV.md`.
///
/// Not read, said rather than left to be discovered: the names in the PowerShell scripts (a handful of
/// build and release scripts, whose comments the language scan does read) and the names inside a C#
/// interpolation hole (see `source_names`).
#[test]
fn every_name_in_the_code_is_english() {
    let vocabulary = polish_vocabulary();
    // Two canaries: a vocabulary that read nothing would pass every name, and one that read the wrong
    // file would pass the ones that matter. The count is a literal and the words are the leaks above.
    assert!(vocabulary.len() >= 1500, "the Polish vocabulary has only {} words", vocabulary.len());
    for leak in ["nazwa", "sekcji", "inny", "obok", "urwany"] {
        assert!(vocabulary.contains(leak), "the Polish vocabulary does not hold \"{leak}\"");
    }

    let mut offenders = BTreeSet::new();
    let mut names = 0usize;
    for path in tracked_files(&["rs", "cs", "xaml"]) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let found = if has_extension(&path, &["xaml"]) {
            xaml_names(&text)
        } else {
            source_names(&text, has_extension(&path, &["cs"]))
        };
        names += found.len();
        for (line, name) in found {
            if let Some(why) = polish_in_name(&name, &vocabulary) {
                offenders.insert(format!("{}:{line}: {name} ({why})", rel(&path)));
            }
        }
    }
    assert!(names >= 100_000, "the name scan read only {names} names - it is reading the wrong files");
    println!("name scan: {names} names against {} Polish words", vocabulary.len());
    assert!(
        offenders.is_empty(),
        "a name in the code is Polish (untouchable rule 14). Rename it in English - or, if the word is \
         plainly English as well, add it to ALSO_ENGLISH: {offenders:?}"
    );
}

/// The lexer on the shapes that break a line scan, in both languages: what comes back is the names and
/// only the names, with the lines they are on.
#[test]
fn the_name_scan_reads_code_and_skips_comments_and_literals() {
    let rust = "fn nazwa() {\n    let s = \"obok\"; // inny\n    let r = r#\"multi\nline \"quoted\" obok\"#;\n    \
                /* outer /* nested */ inny */ let c = '\\''; let q = b'x';\n    fn f<'a>(x: &'a str) {}\n}";
    let names: Vec<String> = source_names(rust, false).into_iter().map(|(_, n)| n).collect();
    assert_eq!(names, ["fn", "nazwa", "let", "s", "let", "r", "let", "c", "let", "q", "fn", "f", "a", "x", "a", "str"]);
    let lines: Vec<usize> = source_names(rust, false).into_iter().map(|(l, _)| l).collect();
    assert_eq!(lines[5], 3, "the name in front of the raw string, on its own line");
    assert_eq!(lines[6], 5, "the name after a string that spans a line is counted on its own line");

    let csharp = "var a = $\"{string.Join(\", \", obok)} inny\"; var b = @\"say \"\"urwany\"\"\";\n\
                  var c = \"\"\"\nraw \"sekcji\"\n\"\"\"; var d = '\"'; var @class = 1;";
    let names: Vec<String> = source_names(csharp, true).into_iter().map(|(_, n)| n).collect();
    assert_eq!(names, ["var", "a", "var", "b", "var", "c", "var", "d", "var", "class"]);
}

/// The name test itself, on the real vocabulary and in both directions: a Polish word or letter
/// anywhere in a name, and English names, including the words the vocabulary lets go as English.
#[test]
fn a_polish_word_is_found_wherever_it_sits_in_a_name() {
    let vocabulary = polish_vocabulary();
    for name in ["nazwa_sekcji", "NazwaSekcji", "nazwaSekcji", "SEKCJI_COUNT", "rowObok", "zażółć"] {
        assert!(polish_in_name(name, &vocabulary).is_some(), "{name}");
    }
    for name in ["section_name", "HTTPServer", "obokeh", "unazwa", "stale_problem"] {
        assert!(polish_in_name(name, &vocabulary).is_none(), "{name}");
    }
    assert_eq!(name_words("HTTPServerName2x"), ["http", "server", "name", "x"]);
}

// ---------------------------------------------------------------------------------------------
// H7c. Nothing in a file that nobody can see
// ---------------------------------------------------------------------------------------------

/// A character that takes no visible place where it stands, or changes how the text around it is shown:
/// the private-use area (icon-font glyphs), the zero-width space, joiners and the two direction marks
/// (U+200B to U+200F), the word joiner and the invisible operators (U+2060 to U+2064), a byte order
/// mark, a soft hyphen (U+00AD), the Mongolian vowel separator (U+180E), the Arabic letter mark
/// (U+061C), and the bidirectional embeddings, overrides and isolates (U+202A to U+202E, U+2066 to
/// U+2069) - the reordering that shows code other than what compiles (CVE-2021-42574). rustc refuses
/// those in Rust, and nothing refuses them in C#, XAML, YAML, PowerShell or JSON.
///
/// Written as numbers, because the ranges spelled as escapes are exactly what turned into the
/// characters themselves once.
fn is_invisible(c: char) -> bool {
    let code = u32::from(c);
    (0xE000..=0xF8FF).contains(&code)
        || (0x200B..=0x200F).contains(&code)
        || (0x2060..=0x2064).contains(&code)
        || (0x202A..=0x202E).contains(&code)
        || (0x2066..=0x2069).contains(&code)
        || matches!(code, 0xFEFF | 0x00AD | 0x180E | 0x061C)
}

/// No committed text file carries a character that cannot be seen.
///
/// 🔴 It happened and every gate was green: a C# comparison meant to hold the escapes for the first
/// and last private-use code points reached the file as the two characters themselves (the tool
/// writing it decoded the escapes), so the line showed as `c is < '' or > ''` - it compiled, it
/// worked, and nobody reading it could tell what it compared (2026-09-23). A byte order mark is caught too, which `dotnet format` rejects with a
/// charset error that does not say BOM. Reversal probe in `CHANGELOG-DEV.md`.
#[test]
fn no_file_carries_a_character_nobody_can_see() {
    let files = tracked_files(&["rs", "cs", "xaml", "ps1", "json", "md", "yml", "toml", "html", "css", "js"]);
    let mut offenders = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        for (number, line) in text.lines().enumerate() {
            if let Some(c) = line.chars().find(|&c| is_invisible(c)) {
                offenders.push(format!("{}:{} U+{:04X}", rel(path), number + 1, u32::from(c)));
            }
        }
    }
    assert!(files.len() >= 60, "the invisible-character scan read only {} files", files.len());
    assert!(
        offenders.is_empty(),
        "these lines carry a character that shows as nothing - write it as an escape or a number: {offenders:?}"
    );
}

/// Untouchable rule 13: a flat hyphen, and no semicolons in prose.
///
/// The weakest guard here and it says so: the rule is kept without effort today and breaking it is
/// cosmetic rather than functional. It is in because a ratchet costs a few lines - if anything on
/// this list ever has to go, this goes first.
///
/// Scoped to comments and Markdown prose. A semicolon is syntax in every language in this tree, and
/// an en dash inside a string may be someone's data.
#[test]
fn prose_uses_a_flat_hyphen_and_no_semicolons() {
    let files = tracked_files(&["rs", "cs", "xaml", "ps1", "md"]);
    let mut offenders = Vec::new();
    let mut semicolons_in_prose = 0usize;
    for path in &files {
        if is_this_file(path) {
            continue;
        }
        let markdown = has_extension(path, &["md"]);
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut in_fence = false;
        for (number, line) in text.lines().enumerate() {
            if markdown && line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            let prose = if markdown {
                !in_fence && !line.starts_with("    ")
            } else {
                is_comment(line)
            };
            if !prose {
                continue;
            }
            for (bad, what) in [('\u{2014}', "an em dash"), ('\u{2013}', "an en dash")] {
                if line.contains(bad) {
                    offenders.push(format!("{}:{} has {what}", rel(path), number + 1));
                }
            }
            // A semicolon inside inline code, a URL or an XML entity is not prose.
            let mut outside_code = String::new();
            let mut inside = false;
            for c in line.chars() {
                if c == '`' {
                    inside = !inside;
                } else if !inside {
                    outside_code.push(c);
                }
            }
            let entities = outside_code.matches('&').count();
            let semicolons = outside_code.matches(';').count();
            if semicolons > entities && !outside_code.contains("http") {
                semicolons_in_prose += 1;
                offenders.push(format!("{}:{} has a semicolon in prose", rel(path), number + 1));
            }
        }
    }
    assert!(files.len() >= 60, "the punctuation scan read only {} files", files.len());

    // 🔴 One assertion now, because both halves of rule 13 finally hold. The semicolon half used to
    // be a RATCHET pinned at 282, and the honest reason was that the plan for this guard called H8
    // green after measuring only dashes in code and semicolons in the two root Markdown files. It
    // had never looked inside COMMENTS, where 282 lines were carrying one - real prose, not XML
    // entities, spread across carefully written doc comments. Rewriting them was a large edit to
    // prose nobody had asked to change, so the count was allowed to fall and not rise while the
    // owner decided.
    //
    // Decided and done on 2026-09-07: all 282 were rewritten, every one of them by replacing the
    // semicolon with the flat hyphen this project uses everywhere else for the same job. With
    // nothing left to hold, a ratchet is worse than an assertion - it would quietly permit the
    // first new one. The offender list is shared with the dash half so a failure names the line
    // rather than only the count.
    assert!(
        offenders.is_empty(),
        "untouchable rule 13 - a flat hyphen, never an em or en dash, and no semicolons in prose: \
         {offenders:?}"
    );
    println!("punctuation scan: {} files, {semicolons_in_prose} semicolons in prose", files.len());
}

// ---------------------------------------------------------------------------------------------
// H9. Every channel from an optional module survives a late load
// ---------------------------------------------------------------------------------------------

/// The channels that live in user32, winmm or ws2_32, by the `CH_*` constant `CHANNELS` names them
/// with. Read out of the table's source rather than linked in, because `chrono-ctl` is deliberately
/// not a dependency of the CLI's tests.
fn optional_module_channels(ctl_src: &str) -> Vec<String> {
    ctl_src
        .lines()
        .filter(|l| l.contains("ChannelDef {"))
        .filter(|l| {
            l.contains("ChannelModule::User32")
                || l.contains("ChannelModule::Winmm")
                || l.contains("ChannelModule::Ws2_32")
        })
        .filter_map(|l| {
            let after = l.split("bit: ").nth(1)?;
            Some(after.split(',').next()?.trim().to_string())
        })
        .collect()
}

/// The body of one free function in the hook, by brace depth - the same shape scan `hook_detours`
/// uses, so a rename shows up as an empty body rather than as a silent pass.
fn hook_fn_body(text: &str, signature: &str) -> Option<String> {
    let start = text.lines().position(|l| l.starts_with(signature))?;
    let lines: Vec<&str> = text.lines().collect();
    let mut depth = 0i32;
    let mut seen = false;
    for (i, l) in lines.iter().enumerate().skip(start) {
        depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
        if l.contains('{') {
            seen = true;
        }
        if seen && depth == 0 {
            return Some(lines[start..=i].join("\n"));
        }
    }
    None
}

/// A channel whose module can arrive after `DllMain` must be installable AFTER `DllMain`.
///
/// Measured 2026-09-07, which is why this guard exists at all: a video player and a flash runtime
/// both ran with winmm mapped while `timeGetTime` and `timeSetEvent` were never hooked, and the
/// report did not carry the channel at all - not covered, not uncovered, absent. `make_hook` resolves
/// these three modules once, at `DllMain`, and for a runtime that loads later that is too early.
/// `late_scan` is the second chance, and this checks that every channel which needs one gets one.
///
/// The failure it is built to catch is a channel ADDED to `CHANNELS` in an optional module and not
/// added to `late_scan` - which today would be silent twice over, because such a channel is dropped
/// from the report rather than listed as a gap (untouchable rule 4).
#[test]
fn every_optional_module_channel_can_be_installed_late() {
    let root = repo_root();
    let ctl_src = std::fs::read_to_string(root.join("crates/ctl/src/lib.rs")).expect("ctl source");
    let hook_src = std::fs::read_to_string(root.join("crates/hook/src/lib.rs")).expect("hook source");

    let channels = optional_module_channels(&ctl_src);

    // Canary, with a LITERAL rather than a count derived from the same list it checks: six channels
    // live in optional modules today (timeGetTime, timeSetEvent, SetTimer, both message waits, the
    // socket wait). Fewer means the scan stopped matching the table's shape and went blind. The
    // connection observer left the list for ntdll on 2026-09-23, when the socket wait already made
    // the count seven.
    assert!(
        channels.len() >= 6,
        "found only {} channels in optional modules - the CHANNELS table changed shape and this \
         guard went blind, fix the scan rather than this number: {channels:?}",
        channels.len()
    );

    let body = hook_fn_body(&hook_src, "unsafe fn late_scan()")
        .expect("late_scan not found in the hook - late module installation is gone, or renamed");

    let missing: Vec<String> = channels
        .iter()
        .map(|ch| ch.replacen("CH_", "IDX_", 1))
        .filter(|idx| !body.contains(idx.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "these channels live in a module the target may load AFTER DllMain, but late_scan never \
         installs them - they would be missing from the report entirely, not reported as a gap: \
         {missing:?}"
    );
    println!("late-load guard: {} optional-module channels, all reachable", channels.len());
}
