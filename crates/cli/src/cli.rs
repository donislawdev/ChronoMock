//! Command surface: the product version, the licence notice, the target bitness, and the usage texts.
//!
//! Split out of `main.rs` so the crate root is a dispatcher and nothing else. The usage strings are
//! CLI text, which is English-only by rule 15 - they are not translation keys and never will be.

use std::path::{Path, PathBuf};

use chrono_proto::PROTOCOL_VERSION;

pub(crate) const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The licence this build is distributed under, read from the manifest instead of retyped here.
/// Cargo resolves the workspace's `license` field before it sets this, so the answer `chrono license`
/// gives and the metadata `cargo deny check licenses` reads cannot drift apart.
pub(crate) const LICENSE_SPDX: &str = env!("CARGO_PKG_LICENSE");

/// The copyright holder. The GUI half states the same line in `gui/Directory.Build.props`, and a test
/// below reads that file, so the two halves of one product cannot end up claiming different years.
const COPYRIGHT: &str = "Copyright (C) 2026 DonislawDev";

/// Where the full text of the GNU GPL lives when the shipped copy is not on disk.
const GPL_URL: &str = "https://www.gnu.org/licenses/gpl-3.0.html";

/// Third-party components linked into THIS binary, and the licence each is taken under. Deliberately
/// not the whole notices file: the GUI bundles WPF-UI as well, and a CLI that named a component it
/// does not contain would be describing someone else's binary.
const BUNDLED: [(&str, &str); 2] = [
    ("Rust crates, statically linked", "MIT"),
    ("MinHook, a vendored C library inside chrono_hook.dll", "BSD-2-Clause"),
];

/// How far up from the executable the licence files are looked for. Three, because the shipped
/// layouts differ and all three are real: the CLI package puts both files beside `chrono.exe`, its
/// x86 core one level below that, and the GUI package keeps the cores in `core/x64` and `core/x86`
/// with the files at the package root (`packaging/build-dist.ps1`).
const LICENSE_SEARCH_DEPTH: usize = 3;

/// The register of everything shipped that somebody else wrote, compiled INTO the binary.
///
/// The same file the release SBOM is generated from and the same one `THIRD-PARTY-NOTICES.md` is
/// checked against, so the three cannot describe different builds. Embedded rather than read from
/// disk for the reason the whole product exists offline: a user with no internet, and a copy of
/// `chrono.exe` taken out of its folder, can still ask what is inside it.
const COMPONENT_REGISTER: &str = include_str!("../../../packaging/components.json");

/// The single line `chrono version` prints.
///
/// Built as a value rather than printed directly, so a test can assert on it without capturing
/// stdout. Three facts rather than one, because each answers a question a bug report otherwise has
/// to guess at: which build, which of the two cores this executable is, and which wire version it
/// speaks - the last one being what a "protocol version mismatch" is actually about.
pub(crate) fn version_line() -> String {
    format!("chrono {CORE_VERSION} ({}, protocol {PROTOCOL_VERSION})", this_bitness())
}

/// `chrono version`, and the `--version` and `-V` spellings of the same question.
///
/// On stdout and exit 0. Asking a question is a success rather than a usage error, and the answer
/// has to survive a pipe, which is the whole reason someone types it in a script.
pub(crate) fn print_version() {
    println!("{}", version_line());
}

/// Where a distribution's two licence files actually are, or `None` when they are not on disk near
/// this executable.
///
/// Looked up rather than assumed. Copying `chrono.exe` out of the package on its own is an ordinary
/// thing to do, and a notice pointing at a `LICENSE` that is not there would be the tool making a
/// promise the copy does not keep - the same honesty the rest of the reporting is held to.
pub(crate) struct LicenseFiles {
    license: Option<PathBuf>,
    notices: Option<PathBuf>,
}

/// Look for the licence files beside the running executable and in the directories above it.
pub(crate) fn find_license_files() -> LicenseFiles {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut next = exe.parent().map(Path::to_path_buf);
        while dirs.len() < LICENSE_SEARCH_DEPTH {
            let Some(dir) = next else { break };
            next = dir.parent().map(Path::to_path_buf);
            dirs.push(dir);
        }
    }
    LicenseFiles {
        license: first_file(&dirs, "LICENSE"),
        notices: first_file(&dirs, "THIRD-PARTY-NOTICES.md"),
    }
}

fn first_file(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter().map(|dir| dir.join(name)).find(|path| path.is_file())
}

/// One line naming a file the distribution should carry, or saying honestly that this copy does not
/// have it. `missing` names where to get it, since a reader who sees the line is exactly the reader
/// who cannot open the file.
fn file_line(label: &str, found: Option<&PathBuf>, missing: &str) -> String {
    match found {
        Some(path) => format!("{label}: {}\n", path.display()),
        None => format!("{label}: not next to this executable - {missing}\n"),
    }
}

/// The notice `chrono license` prints.
///
/// Built as a value, and taking the file locations as data rather than reading the disk itself, so a
/// test can see both halves of it: the copy that ships with its files, and the lone executable that
/// does not.
pub(crate) fn license_text(files: &LicenseFiles) -> String {
    let mut out = format!(
        "Chrono Mock {CORE_VERSION} ({})\n{COPYRIGHT}\nLicense {LICENSE_SPDX} <{GPL_URL}>\n\n",
        this_bitness()
    );
    out.push_str(
        "This program comes with ABSOLUTELY NO WARRANTY, to the extent permitted by law. It is free\n\
         software, and you are welcome to redistribute it under the conditions of the GNU General\n\
         Public License version 3.\n\n",
    );
    out.push_str(&file_line("Full licence text", files.license.as_ref(), &format!("read it at {GPL_URL}")));
    out.push_str(&file_line(
        "Third-party notices",
        files.notices.as_ref(),
        "THIRD-PARTY-NOTICES.md ships with every release",
    ));
    out.push_str("\nThird-party components linked into this build:\n");
    for (component, licence) in BUNDLED {
        // Licence first and padded, because the question a reader brings to this list is which
        // licences are in the binary, not which components are.
        out.push_str(&format!("  {licence:<14}{component}\n"));
    }
    out.push_str(
        "The notices file above carries each component's own notice, and for the Rust crates every\n\
         version and copyright line.\n",
    );
    out
}

/// Every component the register declares, as the lines `chrono license --components` prints.
///
/// Grouped by which package carries it, because the two packages differ and a reader has exactly one
/// of them in front of them. Two lines per component rather than one: the longest name and version
/// together run past fifty characters, and a table that wraps is a table nobody reads.
pub(crate) fn component_lines(register: &str) -> Vec<String> {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(register) else {
        // Compiled in from a file a test parses on every run, so this cannot happen from a checkout.
        // Said rather than swallowed, because the alternative is an empty list that reads as "nothing
        // third-party in here" - the one answer this command must never give by accident.
        return vec!["the component register could not be read - see THIRD-PARTY-NOTICES.md".into()];
    };
    let components = doc["components"].as_array().map(Vec::as_slice).unwrap_or_default();

    let mut out = Vec::new();
    for (group, title) in [
        ("cli", "In both packages, compiled into the native tool:"),
        ("gui", "Only in the window package:"),
    ] {
        let mut first = true;
        for component in components {
            let in_cli = names_package(component, "cli");
            let wanted = if group == "cli" { in_cli } else { !in_cli && names_package(component, "gui") };
            if !wanted {
                continue;
            }
            if first {
                if !out.is_empty() {
                    out.push(String::new());
                }
                out.push(title.to_string());
                first = false;
            }
            let name = component["name"].as_str().unwrap_or("?");
            let version = component["version"].as_str().unwrap_or("");
            let version = if version == "NOASSERTION" { "(no version of its own)" } else { version };
            out.push(format!("  {name} {version}"));
            out.push(format!(
                "      {}, {}",
                component["license_concluded"].as_str().unwrap_or("?"),
                component["supplier"].as_str().unwrap_or("?")
            ));
        }
    }
    out
}

fn names_package(component: &serde_json::Value, package: &str) -> bool {
    component["in"]
        .as_array()
        .is_some_and(|list| list.iter().any(|p| p.as_str() == Some(package)))
}

/// `chrono license`, and the `--license` spelling of the same question.
///
/// On stdout and exit 0, for the same reason `chrono version` is: asking a question is a success, and
/// the answer has to survive a pipe. `--components` adds the full register, which is the offline half
/// of the SBOM published beside a release.
pub(crate) fn print_license(argv: &[String]) -> i32 {
    let mut components = false;
    for arg in argv {
        match arg.as_str() {
            "--components" => components = true,
            other => {
                eprintln!("chrono: unknown argument '{other}' for license");
                eprintln!("usage: chrono license [--components]");
                return 1;
            }
        }
    }

    print!("{}", license_text(&find_license_files()));
    if components {
        println!();
        for line in component_lines(COMPONENT_REGISTER) {
            println!("{line}");
        }
    } else {
        println!("\nRun `chrono license --components` for every component with its version and licence.");
    }
    0
}

pub(crate) fn print_usage() {
    eprintln!("usage: chrono run <target> [--at <local-moment>] [--preset <id>] [--param id=value]... [--zone <+HH:MM>] [--mode <flow|frozen|xN>] [--scale-duration] [--scale-qpc] [--ticks N] [--timeout <s>] [--set-after T:M] [--jump-after T:moment] [--args \"...\"] [--cwd <dir>] [--report <path>] [--force] [--dry-run] [--json]");
    eprintln!("       --dry-run prints what the session would be and starts nothing - the resolved moment and zone, what a preset filled its parameters with, and which mechanism the target would take");
    eprintln!("       without --at (or --preset) the session clock starts at the real current time, so `--mode xN` alone just runs the target faster");
    eprintln!("       --scale-qpc also scales the high-resolution counter, which is where Python 3.13+ monotonic, .NET Stopwatch and Java nanoTime read elapsed time");
    eprintln!("       --force runs on even when the opening verdict says the substitution did not take effect (the target is stopped otherwise)");
    eprintln!("       --timeout gives up after N seconds and exits 6, for a pipeline that must not hang; a core that stops answering for 15 s exits 6 on its own");
    eprintln!("       --cwd starts the target in that directory; without it the target inherits ours, and a directory that does not exist stops the session rather than looking like a broken target");
    eprintln!("       (--preset supplies the moment and mode from presets/<id>.json, exclusive of --at/--mode/--scale-duration; --param fills its parameters, a trial start_date defaults to the target's file date)");
    print_calc_usage();
    eprintln!("usage: chrono version   (also --version, -V)   the build, which core it is, and the protocol it speaks");
    eprintln!("usage: chrono license [--components]   (also --license)   the licence, the warranty disclaimer, and every bundled component with its version");
}

pub(crate) fn print_calc_usage() {
    eprintln!("usage: chrono calc [--base <today|now|YYYY-MM-DDTHH:MM:SS>] [--base-utc <YYYY-MM-DDTHH:MM:SS[Z]>] [--shift <±N<unit>>]... [--set-time <HH:MM:SS>] [--snap <target>] [--nearest <target>] [--to-zone <+HH:MM>] [--zone <+HH:MM>] [--calendar <us-banking|us-federal|pl>] [--format <mask>] [--json]");
    eprintln!("       or: chrono calc --preset <id> [--param id=value]...   (named moment, e.g. month-end, trial-first-day-after)");
    eprintln!("       or: chrono calc --analyze <pasted-date>   (interpret a date, e.g. 04/08/2008; shows both readings when ambiguous)");
    eprintln!("       --json emits machine output (chronomock.calc/1) for any of the above");
    eprintln!("       units: s m h d w mo q y bd (minute=m, month=mo)");
}

pub(crate) fn this_bitness() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "x64"
    } else {
        "x86"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_line_carries_the_build_the_bitness_and_the_protocol() {
        let line = version_line();

        // Each assertion stands for a question a bug report has to answer. Naming them separately
        // means a change that drops one of the three fails here rather than being noticed by
        // whoever ends up reading a report that no longer says which core it came from.
        assert!(line.starts_with("chrono "), "the line must name the tool first: {line}");
        assert!(line.contains(CORE_VERSION), "no build version in {line}");
        assert!(line.contains(this_bitness()), "no bitness in {line}");
        assert!(
            line.contains(&format!("protocol {PROTOCOL_VERSION}")),
            "no protocol version in {line}"
        );

        // One line, because `chrono version` in a script is read with a single capture and a second
        // line would silently become part of it.
        assert!(!line.contains('\n'), "the version answer must be one line: {line}");
    }

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// The licence expression is taken from the manifest, so this is the canary that says the value
    /// arrived at all AND that it is still the one the project ships under. A literal on purpose: an
    /// assertion written against the constant itself would pass over an empty string, which is
    /// exactly what a workspace field that failed to inherit would produce.
    #[test]
    fn the_notice_states_the_licence_the_warranty_and_the_bundled_components() {
        assert_eq!(LICENSE_SPDX, "GPL-3.0-only", "the shipped licence changed, or the manifest field did not reach the binary");

        let text = license_text(&LicenseFiles { license: None, notices: None });
        assert!(text.contains("Chrono Mock"), "the notice must name the product: {text}");
        assert!(text.contains(CORE_VERSION), "the notice must name the build: {text}");
        assert!(text.contains(LICENSE_SPDX), "the notice must name the licence: {text}");
        assert!(text.contains(COPYRIGHT), "the notice must carry the copyright line: {text}");
        // The GPL asks a program to say this out loud, and it is the one sentence a user of a QA tool
        // that injects code into their applications has a right to read without opening a file.
        assert!(text.contains("ABSOLUTELY NO WARRANTY"), "the warranty disclaimer is missing: {text}");
        for (component, licence) in BUNDLED {
            assert!(text.contains(component), "component '{component}' missing from the notice");
            assert!(text.contains(licence), "licence '{licence}' missing from the notice");
        }
    }

    /// A `chrono.exe` copied out of its package has neither file beside it. The notice then says so
    /// and says where to get them, rather than printing a path that leads nowhere - the same rule the
    /// session report is held to, applied to the one text whose whole job is to be citable.
    #[test]
    fn a_copy_without_its_files_says_so_instead_of_naming_a_path() {
        let lonely = license_text(&LicenseFiles { license: None, notices: None });
        assert!(lonely.contains("not next to this executable"), "a missing file must be admitted: {lonely}");
        assert!(lonely.contains(GPL_URL), "a missing licence text must say where to read it: {lonely}");

        let packaged = license_text(&LicenseFiles {
            license: Some(PathBuf::from(r"C:\pkg\LICENSE")),
            notices: Some(PathBuf::from(r"C:\pkg\THIRD-PARTY-NOTICES.md")),
        });
        assert!(packaged.contains(r"C:\pkg\LICENSE"), "a present file must be named by path: {packaged}");
        assert!(
            !packaged.contains("not next to this executable"),
            "a copy that has its files must not claim otherwise: {packaged}"
        );
    }

    /// The notice names components in prose, the notices file carries their legal text, and nothing
    /// links the two - so this does. A component swapped out in one place and left in the other is
    /// the drift this catches, and it is the kind that only shows up in a licence audit.
    ///
    /// What it does NOT prove: the match is a substring over the whole file, and the file legitimately
    /// mentions licences no component here is taken under (it says several crates are dual MIT OR
    /// Apache-2.0). So a licence name that already appears somewhere would pass. Which licences may
    /// enter the build at all is `cargo deny check licenses`, not this.
    #[test]
    fn every_component_the_notice_names_is_in_the_shipped_notices_file() {
        let path = repo_root().join("THIRD-PARTY-NOTICES.md");
        let notices = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("the register the notice points at must exist at {}: {e}", path.display()));

        assert!(notices.contains("MinHook"), "the notices file no longer carries the MinHook notice");
        for (_, licence) in BUNDLED {
            assert!(
                notices.contains(licence),
                "the notice claims '{licence}' for a bundled component, and the notices file does not mention it"
            );
        }
        // The product's own licence, which the notices file states before any component's.
        assert!(notices.contains("GPL-3.0"), "the notices file no longer names the product licence");
    }

    /// The offline half of the SBOM. A user with no internet, holding a copy of the tool, can ask what
    /// is inside it and get the same set of components the published SPDX document carries - because
    /// both are rendered from the one register, which `tests/components.rs` checks against Cargo.lock.
    #[test]
    fn the_component_list_names_every_component_the_register_declares() {
        let lines = component_lines(COMPONENT_REGISTER);
        let text = lines.join("\n");

        assert!(text.contains("In both packages"), "the native components need their group: {text}");
        assert!(text.contains("Only in the window package"), "the managed components need theirs: {text}");

        // One component from each kind, each with the licence the register concluded. Named
        // individually rather than counted, so a register that lost a whole family still fails.
        for (component, licence) in [
            ("serde ", "MIT"),
            ("MinHook ", "BSD-2-Clause"),
            ("WPF-UI ", "MIT"),
            ("Microsoft.Windows.SDK.NET.Ref ", "LicenseRef-Microsoft-Windows-SDK"),
        ] {
            assert!(text.contains(component), "'{component}' is missing from the list");
            assert!(text.contains(licence), "licence '{licence}' is missing from the list");
        }

        // The one component with no version of its own says so rather than printing an empty gap.
        assert!(text.contains("(no version of its own)"), "{text}");

        // A literal floor: the register held 28 components when this was written, and a list that
        // quietly shrank to nothing would otherwise pass every assertion above that it still matched.
        let named = lines.iter().filter(|l| l.starts_with("  ") && !l.starts_with("      ")).count();
        assert!(named >= 28, "only {named} components listed - the register or the grouping lost some");
    }

    /// The failure that must never be silent. An unreadable register would otherwise print an empty
    /// list, which a reader takes as "there is nothing third-party in here" - the one wrong answer
    /// this command can give (untouchable rule 6).
    #[test]
    fn an_unreadable_register_says_so_instead_of_listing_nothing() {
        let lines = component_lines("{ this is not json");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("could not be read"), "{lines:?}");
        assert!(lines[0].contains("THIRD-PARTY-NOTICES.md"), "it must point somewhere: {lines:?}");
    }

    /// One product, two halves, one copyright line. The GUI states it in its build properties and the
    /// CLI in the constant above, and nothing but this test stops the year in one of them from moving
    /// alone - the same two-places problem the version number already has.
    #[test]
    fn the_copyright_line_matches_the_one_the_gui_ships() {
        let path = repo_root().join("gui").join("Directory.Build.props");
        let props = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("the GUI build properties must be readable at {}: {e}", path.display()));
        assert!(
            props.contains(COPYRIGHT),
            "the CLI says '{COPYRIGHT}' and {} does not - one of the two halves moved alone",
            path.display()
        );
    }
}
