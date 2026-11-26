//! Strings that must never appear in the repository, whatever a session is in the middle of writing.
//!
//! Two kinds, one mechanism. Untouchable rule 26: no application we measure Chrono Mock on is ever
//! named here. And the personal data in the code-signing certificate's subject: a certificate issued
//! to an individual carries their name, town and province, every signed file carries it with them,
//! and that exposure is the certificate's to make rather than the repository's - only the fingerprint
//! belongs in `packaging/codesign.json`.
//!
//! The rule was written down and nothing checked it, which is how it came to be broken in seventeen
//! places across ten committed files - including the public README and both language versions of a
//! live website page - for as long as the repository has been public. A rule in a document with no
//! guard in the code promises a protection that does not exist (rule 12).
//!
//! WHY THE LIST IS HASHED. A guard that spelled the forbidden names out would put them back in the
//! repository itself, which is the thing it exists to prevent. So it carries FNV-1a of each name in
//! lowercase, and a failure reports the file, the line and WHAT SORT of name it is, never the token
//! it matched - CI logs on a public repository are public too.
//!
//! FNV-1a is not a security choice and does not need to be one. Nothing here resists an attacker,
//! it only avoids writing the names down. A 64-bit collision would show up as a false positive on
//! one word, which a person then reads and dismisses.
//!
//! WHAT MAY BE NAMED, and this is the line the code already drew for itself in `report.rs`: engines,
//! runtimes and protocols. `Chromium`, `Electron`, `.NET`, `Java`, `Python`, `Unity` are what the
//! product's own features and warnings are defined in terms of, and a warning that will not say
//! which runtime it is about cannot be acted on. What may never be named is the application someone
//! pointed the tool at.
//!
//! ADDING A NAME. Hash it with the helper in `tools/` and add the pair below. Two things to check
//! first: the name must not be an ordinary English word (the scan is a whole-word match, so a common
//! word would fire on prose everywhere), and the hint must describe the category rather than the
//! product.
//!
//! EXCEPTIONS are whole lines, not names, and they are the maintainer's to make - rule 26 is
//! untouchable, so a session that finds itself wanting one asks instead of adding it. Each exception
//! is one exact line in one file (`ALLOWED_LINES`), so the same name written anywhere else, or that
//! line changed by a single character, fires the scan again. The ceiling on how many there are only
//! comes down.

use std::path::{Path, PathBuf};

/// Committed trees, mirroring `hygiene.rs`. A file outside them is not in anyone's checkout.
const TRACKED_TREES: &[&str] = &["crates", "gui", "site", "packaging", ".github", "tests"];

const SKIP_DIRS: &[&str] = &["target", "obj", "bin", "node_modules", ".vs", ".git"];

/// Committed files in the root. The README is here because that is where the rule was broken most
/// publicly.
const ROOT_FILES: &[&str] = &[
    "README.md",
    "CHANGELOG.md",
    "CONTRIBUTING.md",
    "SECURITY.md",
    "CODE_OF_CONDUCT.md",
    "THIRD-PARTY-NOTICES.md",
];

/// Text the rule applies to: everything a reader of the repository can see.
const SCANNED_EXTENSIONS: &[&str] = &["rs", "cs", "md", "html", "json", "xaml", "ps1", "yml", "yaml", "toml"];

/// FNV-1a of each forbidden name in lowercase, with what to write instead.
///
/// The hint is what a failure prints, so it has to be enough to act on without naming the product.
const FORBIDDEN: &[(u64, &str)] = &[
    (0xacfe_1a24_fdac_740a, "an application used as a measurement target - write what it IS (\"an Electron app\"), never which one"),
    (0x68fa_3119_4ec9_3334, "an application used as a measurement target - write what it IS (\"a media player\"), never which one"),
    (0xb086_3083_c3e7_3472, "an application used as a measurement target - write what it IS (\"a media player\"), never which one"),
    (0x4207_1853_4183_b90f, "an application used as a measurement target - write what it IS (\"a hardened Electron app\"), never which one"),
    // The canary. A word no source would contain by accident, so the scanner can be caught working
    // on synthetic text without any real name being written down here.
    (0xd760_33fd_22c7_ed49, "the canary token, which exists so this scan can be seen to work"),
    // Personal data out of the code-signing certificate's subject. A certificate issued to an
    // individual carries their name, their town and their province, and every signed file carries it
    // with them - that exposure is the certificate's to make, not the repository's. Only the
    // fingerprint belongs here (packaging/codesign.json), never the subject, so pasting the subject
    // into a comment or a document fails this scan.
    (0x15a9_1a89_7eee_d3bc, "personal data from the code-signing certificate subject - the repository keeps the fingerprint and nothing else"),
    (0x8225_5530_32a3_d679, "personal data from the code-signing certificate subject - the repository keeps the fingerprint and nothing else"),
];

/// Lines that may carry a forbidden name: the file, FNV-1a of the line with its indentation trimmed,
/// and why the line is not what rule 26 is about.
///
/// The line is hashed for the reason the names are - spelling it out would put the name back in this
/// file. An exception has to be needed and has to be unique: a guard below fails when an entry stops
/// matching exactly one line of its file, or matches a line with no forbidden name in it, so an
/// exception cannot outlive the line it was made for.
const ALLOWED_LINES: &[(&str, u64, &str)] = &[(
    "README.md",
    0x4860_4916_0bde_2efe,
    "the community invite in the README's \"Follow me\" links - it says where people talk about the \
     project, and nothing about what the tool was measured on",
)];

/// How many exceptions there may be. A ceiling that only comes down: raising it is a decision about
/// an untouchable rule, which belongs to the maintainer and not to whoever is making a build pass.
const ALLOWED_LINES_CEILING: usize = 1;

/// FNV-1a, 64-bit. Fifteen lines rather than a dependency: this is a lookup key, not a digest.
fn fnv1a64(word: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in word.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// One finding: the line it is on, and the hint for the name that matched.
struct Finding {
    line: usize,
    hint: &'static str,
}

/// Every forbidden name in one text, as whole lowercase words.
///
/// A pure function over text, so the canary can point it at source that is in no checkout. Words are
/// runs of ASCII letters and digits, which is what splits `Product.exe` and `product-name` into parts
/// a name can be recognised in.
fn findings(source: &str) -> Vec<Finding> {
    findings_allowing(source, &[])
}

/// The same scan, passing over the lines whose trimmed text hashes to one of `allowed`.
fn findings_allowing(source: &str, allowed: &[u64]) -> Vec<Finding> {
    let mut out = Vec::new();
    for (number, line) in source.lines().enumerate() {
        if !allowed.is_empty() && allowed.contains(&fnv1a64(line.trim())) {
            continue;
        }
        for word in line.split(|c: char| !c.is_ascii_alphanumeric()) {
            if word.is_empty() {
                continue;
            }
            let hash = fnv1a64(&word.to_ascii_lowercase());
            if let Some((_, hint)) = FORBIDDEN.iter().find(|(h, _)| *h == hash) {
                out.push(Finding { line: number + 1, hint });
            }
        }
    }
    out
}

/// The exceptions made for one file, by its path from the repository root with forward slashes.
fn allowed_in(relative: &str) -> Vec<u64> {
    ALLOWED_LINES
        .iter()
        .filter(|(file, _, _)| *file == relative)
        .map(|(_, hash, _)| *hash)
        .collect()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn rel(path: &Path) -> String {
    path.strip_prefix(repo_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// This file names the canary token in order to test with it, and carries the hint texts. Scanning it
/// would find its own material - the one shape a guard must never mistake for a finding.
fn is_this_file(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some("names.rs")
}

fn scanned_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut out = Vec::new();
    for tree in TRACKED_TREES.iter().copied() {
        // A listed tree that is not there is a list a move left behind, read as clean if skipped.
        assert!(root.join(tree).is_dir(), "{tree} is a tracked tree and is not in the repository");
        let mut stack = vec![root.join(tree)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    if !SKIP_DIRS.contains(&name.as_str()) {
                        stack.push(path);
                    }
                } else if has_scanned_extension(&path) && !is_this_file(&path) {
                    out.push(path);
                }
            }
        }
    }
    for name in ROOT_FILES.iter().copied() {
        let path = root.join(name);
        if path.is_file() {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn has_scanned_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SCANNED_EXTENSIONS.contains(&ext))
}

/// The rule itself, over every committed file a reader can see.
#[test]
fn no_application_we_measured_on_is_named_anywhere_in_the_repository() {
    let files = scanned_files();
    let mut offenders: Vec<String> = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue; // a file that is not text cannot carry prose
        };
        for finding in findings_allowing(&text, &allowed_in(&rel(path))) {
            // The token is deliberately NOT printed. This failure can appear in a public CI log.
            offenders.push(format!("{}:{} names {}", rel(path), finding.line, finding.hint));
        }
    }

    // A literal floor, not a count taken from the list this test walks. A scan that quietly found no
    // files would otherwise report a clean repository, which is the failure this whole file is about.
    assert!(
        files.len() > 150,
        "the scan found only {} files, so a clean result would mean nothing",
        files.len()
    );

    assert!(
        offenders.is_empty(),
        "untouchable rule 26 - the repository must never name an application Chrono Mock was measured \
         on. Write what it IS, not which one: {offenders:?}"
    );
    println!("rule 26 scan: {} files, {} forbidden names known", files.len(), FORBIDDEN.len());
}

/// The scan seen working, on text that is in no checkout. Without this the test above passes exactly
/// as well when `findings` returns nothing at all.
#[test]
fn the_scan_catches_a_forbidden_name_in_every_shape_it_appears_in() {
    let canary = "zzcanaryzz";
    // Each shape with the number of times the name is in it. The path shape carries it twice - a
    // folder and an executable - and a scan that reported one would be counting lines, not names.
    for (shape, expected) in [
        (format!("// measured on {canary} during the bring-up"), 1),
        (format!("run \"C:/Programs/{canary}/{canary}.exe\" --at 2038-01-01T00:00:00"), 2),
        (format!("<td>{}</td>", canary.to_uppercase()), 1),
        (format!("the {}-specific stimulus", canary[..1].to_uppercase() + &canary[1..]), 1),
    ] {
        assert_eq!(findings(&shape).len(), expected, "wrong count for: {shape}");
    }

    // Three lines, one on the middle one, so the reported line number is not simply always the first.
    let block = format!("clean line\nsecond line mentions {canary} here\nthird line");
    let found = findings(&block);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].line, 2, "the line number must point at the offending line");
}

/// The other direction, which matters more than it looks: this repository is full of words that must
/// keep passing. A guard that fired on "edge case" or on the engines the product is defined in terms
/// of would be turned off within a day, and then rule 26 would be unguarded again.
#[test]
fn ordinary_prose_and_the_names_that_are_allowed_raise_nothing() {
    let allowed = "\
        The edge case here is a Chromium or Electron target, launched with a debug port.\n\
        A Unity game caps its own simulation step, and .NET, Java and Python read elapsed time\n\
        from QueryPerformanceCounter. None of these is an application we measured on - they are\n\
        engines, runtimes and protocols, and a warning that will not name the runtime it is about\n\
        cannot be acted on.\n";
    assert!(
        findings(allowed).is_empty(),
        "the scan fired on a name the rule allows, which is how a guard gets disabled"
    );
}

/// The list is a list, and the hashes are the hashes. A pair mistyped into all-zeroes, or a list that
/// shrank to nothing, would leave the scan above green over a repository it no longer protects.
#[test]
fn the_forbidden_list_is_present_and_well_formed() {
    assert!(FORBIDDEN.len() >= 7, "the list lost entries: {}", FORBIDDEN.len());
    assert!(FORBIDDEN.iter().all(|(hash, _)| *hash != 0), "a zero hash matches nothing");
    assert!(FORBIDDEN.iter().all(|(_, hint)| hint.len() > 20), "a hint has to be actionable");

    let mut hashes: Vec<u64> = FORBIDDEN.iter().map(|(h, _)| *h).collect();
    hashes.sort_unstable();
    let before = hashes.len();
    hashes.dedup();
    assert_eq!(before, hashes.len(), "the same name is listed twice");

    // The hash function itself, against a value computed outside this file. If `fnv1a64` were ever
    // "simplified", every hash above would silently stop matching anything.
    assert_eq!(fnv1a64("zzcanaryzz"), 0xd760_33fd_22c7_ed49);
    assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325, "the empty string is the FNV offset basis");

    assert!(
        ALLOWED_LINES.len() <= ALLOWED_LINES_CEILING,
        "{} exceptions to rule 26 against a ceiling of {ALLOWED_LINES_CEILING} - an exception to an \
         untouchable rule is the maintainer's to make, and the ceiling only comes down",
        ALLOWED_LINES.len()
    );
    assert!(
        ALLOWED_LINES.iter().all(|(_, hash, reason)| *hash != 0 && reason.len() > 20),
        "an exception needs a real hash and a reason someone can read"
    );
}

/// Every exception still points at exactly one line of its file, and that line still carries a
/// forbidden name. An exception whose line was edited or removed is not narrow any more - it would sit
/// here waiting for any line that happens to hash the same - and one over a line with nothing to
/// forgive is dead weight that makes the list look longer than the rule allows.
#[test]
fn every_exception_still_matches_one_line_and_is_still_needed() {
    for (file, hash, _) in ALLOWED_LINES {
        let text = std::fs::read_to_string(repo_root().join(file))
            .unwrap_or_else(|e| panic!("the exception names {file}, which cannot be read: {e}"));
        let matching: Vec<&str> = text.lines().filter(|line| fnv1a64(line.trim()) == *hash).collect();
        assert_eq!(
            matching.len(),
            1,
            "an exception in {file} matches {} lines rather than one - the line it was made for was \
             edited, removed or copied, so the exception has to be made again or dropped",
            matching.len()
        );
        assert!(
            !findings(matching[0]).is_empty(),
            "the exception in {file} covers a line with no forbidden name in it, so it is not needed"
        );
    }
}

/// The exception seen working, on the canary: a line passes only word for word (indentation aside),
/// only in the file it was made for, and never takes the rest of the file with it.
#[test]
fn an_exception_passes_one_exact_line_in_one_file_and_nothing_else() {
    let canary = "zzcanaryzz";
    let line = format!("<a href=\"https://example.invalid/{canary}\">{canary}</a>");
    let allowed = [fnv1a64(&line)];

    assert!(findings_allowing(&line, &allowed).is_empty(), "the allowed line itself fired");
    assert!(
        findings_allowing(&format!("    {line}"), &allowed).is_empty(),
        "indentation alone must not undo an exception"
    );
    assert!(
        findings_allowing(&format!("{line} "), &allowed).is_empty(),
        "trailing space is indentation too, on the other side"
    );
    assert_eq!(
        findings_allowing(&line.replace("example", "examp1e"), &allowed).len(),
        2,
        "a changed line is a different line, and the scan must see both names in it again"
    );

    let block = format!("{line}\nand {canary} once more\n{line}");
    let found = findings_allowing(&block, &allowed);
    assert_eq!(found.len(), 1, "the exception swallowed more than its own line");
    assert_eq!(found[0].line, 2);

    for (file, hash, _) in ALLOWED_LINES {
        assert!(allowed_in(file).contains(hash), "{file} lost its own exception");
        for elsewhere in [format!("site/{file}"), format!("{file}.bak"), file.to_ascii_lowercase()] {
            if elsewhere != *file {
                assert!(
                    !allowed_in(&elsewhere).contains(hash),
                    "the exception for {file} also applies to {elsewhere}"
                );
            }
        }
    }
}
