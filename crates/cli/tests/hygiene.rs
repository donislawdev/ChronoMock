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
    (definitions, mentions, files_read)
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

/// Polish diacritics. Untouchable rule 9 makes the criterion the PLACE, not the reader: everything
/// in the repository is English, everything outside it is Polish.
const POLISH_LETTERS: &[char] = &[
    'ą', 'ć', 'ę', 'ł', 'ń', 'ó', 'ś', 'ź', 'ż', 'Ą', 'Ć', 'Ę', 'Ł', 'Ń', 'Ó', 'Ś', 'Ź', 'Ż',
];

fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with("<!--") || trimmed.starts_with("///")
}

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
        for (number, line) in text.lines().enumerate() {
            if is_comment(line) && line.chars().any(|c| POLISH_LETTERS.contains(&c)) {
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
