//! Applications installed by Squirrel.Windows, an installer many Electron and .NET applications ship
//! with.
//!
//! Such an installer keeps each version of the application in a folder of its own, `app-<version>`, beside
//! `Update.exe` and a starter named like the application. The starter only starts the newest version and
//! closes. Read in its source (`src/StubExecutable/StubExecutable.cpp`, 2026-11-17): it takes the folders
//! whose names start with `app-`, skips one holding a `.not-finished` file (a version still being written),
//! reads the rest of each name as a Semantic Versioning 2.0.0 version, and starts the program named like
//! itself in the highest one above 0.0.0, with its own arguments unchanged and that folder as the working
//! folder. It does not wait for the program to end.
//!
//! A session started on the starter reached only the starter. The starter is a 32-bit program, so a
//! 64-bit application it started was out of reach of the session's hook, and an Electron application was
//! started as a native program, where its windows are out of reach (measured 2026-11-11). The session
//! starts the program the starter would start instead, and says so (`KEY_STARTER_SKIPPED`).

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use chrono_proto::TargetSpec;

/// The session started the program a Squirrel starter would start, in place of the starter.
pub(crate) const KEY_STARTER_SKIPPED: &str = "session.squirrel_starter_skipped";

/// The program the installer keeps beside its starters, which only an installer's starter has beside it.
const UPDATER: &str = "Update.exe";

/// The start of the name of a folder holding one version of the application.
const VERSION_FOLDER_PREFIX: &str = "app-";

/// The file a version folder holds while the installer is still writing it.
const NOT_FINISHED: &str = ".not-finished";

/// The program a Squirrel starter at `target` would start, if `target` is one: `Update.exe` beside it,
/// and the program of the same name in the newest finished version folder beside it. `None` when there is
/// no such program - the starter itself fails to start anything then, and the session starts the starter
/// and says what it finds, as it does for any other program. Also `None` for a path that is not valid
/// UTF-16 text, which the wire could not carry.
pub(crate) fn program_behind(target: &Path) -> Option<PathBuf> {
    let dir = target.parent()?;
    let name = target.file_name()?;
    if name.eq_ignore_ascii_case(UPDATER) || !dir.join(UPDATER).is_file() {
        return None;
    }
    let program = newest_version_folder(dir)?.join(name);
    (program.is_file() && program.to_str().is_some()).then_some(program)
}

/// Start the program behind a Squirrel starter in place of the starter, the way the starter would: its
/// arguments unchanged, and its own folder as the working folder unless the tester named one. Returns
/// whether it did. The driver asks this too, before the core starts, so its report and evidence say what
/// the core started and where.
pub(crate) fn skip_starter(target: &mut TargetSpec) -> bool {
    let Some(program) = program_behind(Path::new(&target.path)) else {
        return false;
    };
    let Some(path) = program.to_str() else {
        return false;
    };
    if target.cwd.is_none() {
        target.cwd = program.parent().and_then(Path::to_str).map(str::to_string);
    }
    target.path = path.to_string();
    true
}

/// The newest finished version folder in `dir`, as the starter picks it. A folder that cannot be read
/// has none.
fn newest_version_folder(dir: &Path) -> Option<PathBuf> {
    let names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| version_text(name).is_some())
        .filter(|name| {
            let folder = dir.join(name);
            folder.is_dir() && !folder.join(NOT_FINISHED).exists()
        })
        .collect();
    newest_version(names.iter().map(String::as_str)).map(|name| dir.join(name))
}

/// The version part of a version folder's name, its prefix matched without regard to case as Windows
/// matches the starter's `app-*`.
fn version_text(name: &str) -> Option<&str> {
    let prefix = name.get(..VERSION_FOLDER_PREFIX.len())?;
    prefix.eq_ignore_ascii_case(VERSION_FOLDER_PREFIX).then(|| &name[VERSION_FOLDER_PREFIX.len()..])
}

/// Of the version folders named, the one the starter starts: the highest version above 0.0.0, the first
/// named of equal ones. A name whose version does not read as Semantic Versioning 2.0.0 is passed over.
fn newest_version<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let zero = Version { core: [0, 0, 0], pre: Vec::new() };
    let mut best: Option<(Version<'a>, &'a str)> = None;
    for name in names {
        let Some(version) = version_text(name).and_then(Version::parse) else {
            continue;
        };
        let floor = best.as_ref().map_or(&zero, |(version, _)| version);
        if version > *floor {
            best = Some((version, name));
        }
    }
    best.map(|(_, name)| name)
}

/// A Semantic Versioning 2.0.0 version, held for its precedence: the build part is read and then left out,
/// as precedence leaves it out.
#[derive(Debug, PartialEq, Eq)]
struct Version<'a> {
    core: [u64; 3],
    pre: Vec<&'a str>,
}

impl<'a> Version<'a> {
    /// `major.minor.patch`, then `-` and dot-separated pre-release identifiers, then `+` and dot-separated
    /// build identifiers. Numbers have no leading zero, identifiers are ASCII letters, digits and hyphens.
    fn parse(text: &'a str) -> Option<Version<'a>> {
        let (rest, build) = match text.split_once('+') {
            Some((rest, build)) => (rest, Some(build)),
            None => (text, None),
        };
        if let Some(build) = build
            && !build.split('.').all(is_identifier)
        {
            return None;
        }
        let (core_text, pre_text) = match rest.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (rest, None),
        };
        let mut parts = core_text.split('.');
        let mut core = [0u64; 3];
        for slot in &mut core {
            *slot = number(parts.next()?)?;
        }
        if parts.next().is_some() {
            return None;
        }
        let pre: Vec<&str> = match pre_text {
            Some(pre) => pre.split('.').collect(),
            None => Vec::new(),
        };
        let pre_sound = pre.iter().all(|id| is_identifier(id) && (!is_digits(id) || number(id).is_some()));
        pre_sound.then_some(Version { core, pre })
    }
}

impl Ord for Version<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                let first_apart = self.pre.iter().zip(&other.pre).map(|(a, b)| identifier_order(a, b)).find(|order| order.is_ne());
                first_apart.unwrap_or_else(|| self.pre.len().cmp(&other.pre.len()))
            }
        })
    }
}

impl PartialOrd for Version<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Two pre-release identifiers: numbers by value and below every other identifier, the rest by their
/// ASCII text.
fn identifier_order(a: &str, b: &str) -> Ordering {
    match (number(a), number(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.as_bytes().cmp(b.as_bytes()),
    }
}

/// A number without a leading zero that fits 64 bits.
fn number(text: &str) -> Option<u64> {
    let leading_zero = text.len() > 1 && text.starts_with('0');
    (is_digits(text) && !leading_zero).then(|| text.parse().ok()).flatten()
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn is_identifier(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A starter's folder: `Update.exe`, the starter, and the version folders named, each holding the
    /// program unless its name ends in `!`, and holding `.not-finished` when it ends in `*`.
    fn installed(dir: &Path, starter: &str, folders: &[&str]) -> PathBuf {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(UPDATER), b"x").unwrap();
        let exe = dir.join(starter);
        std::fs::write(&exe, b"x").unwrap();
        for folder in folders {
            let name = folder.trim_end_matches(['!', '*']);
            std::fs::create_dir_all(dir.join(name)).unwrap();
            if !folder.ends_with('!') {
                std::fs::write(dir.join(name).join(starter), b"x").unwrap();
            }
            if folder.ends_with('*') {
                std::fs::write(dir.join(name).join(NOT_FINISHED), b"").unwrap();
            }
        }
        exe
    }

    /// The target as the wire brings it, every other field at its default.
    fn spec_for(path: &Path, cwd: Option<&str>) -> TargetSpec {
        serde_json::from_value(serde_json::json!({ "path": path, "cwd": cwd })).unwrap()
    }

    #[test]
    fn a_starter_starts_the_program_of_its_name_in_the_newest_finished_version_folder() {
        let dir = crate::testutil::unique_temp_dir("chrono-squirrel");
        let exe = installed(&dir, "Tool.exe", &["app-3.6.9", "app-3.6.11", "app-3.7.0*", "packages!"]);
        assert_eq!(program_behind(&exe), Some(dir.join("app-3.6.11").join("Tool.exe")), "3.6.11 is above 3.6.9, 3.7.0 is not finished");

        std::fs::write(dir.join("app-9.0.0"), b"a file, not a folder").unwrap();
        assert_eq!(program_behind(&exe), Some(dir.join("app-3.6.11").join("Tool.exe")), "a file named like a version folder is not one");

        let mut spec = spec_for(&exe, None);
        assert!(skip_starter(&mut spec));
        assert_eq!(Path::new(&spec.path), dir.join("app-3.6.11").join("Tool.exe"));
        assert_eq!(spec.cwd.as_deref().map(Path::new), Some(dir.join("app-3.6.11").as_path()), "the starter's working folder");

        let mut named = spec_for(&exe, Some("C:\\work"));
        assert!(skip_starter(&mut named));
        assert_eq!(named.cwd.as_deref(), Some("C:\\work"), "the tester's working folder stays");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_program_is_a_starter_only_beside_the_updater_with_its_program_in_the_newest_version() {
        let dir = crate::testutil::unique_temp_dir("chrono-squirrel-not");
        let exe = installed(&dir, "Tool.exe", &["app-1.0.0", "app-2.0.0!"]);
        assert_eq!(program_behind(&exe), None, "the newest version has no program of that name - the starter would fail, it gets no older one");
        assert_eq!(program_behind(&dir.join(UPDATER)), None, "the updater itself");
        let mut spec = spec_for(&exe, None);
        assert!(!skip_starter(&mut spec));
        assert_eq!((Path::new(&spec.path), spec.cwd.as_deref()), (exe.as_path(), None), "left as it was");

        let exe = installed(&dir, "Tool.exe", &["app-1.0.0"]);
        std::fs::remove_file(dir.join(UPDATER)).unwrap();
        assert_eq!(program_behind(&exe), None, "no updater beside it");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(program_behind(Path::new("Tool.exe")), None, "a bare name in a folder with no updater");
    }

    /// The cases the window's mirror reads too, so the two pick the same folder.
    #[test]
    fn the_newest_version_is_picked_as_the_shared_cases_say() {
        let cases = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/squirrel/newest-version.txt")).unwrap();
        let mut count = 0;
        for line in cases.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let (names, expected) = line.split_once("=>").expect("names => expected");
            let expected = expected.trim();
            let picked = newest_version(names.split_whitespace()).unwrap_or("-");
            assert_eq!(picked, expected, "{line}");
            count += 1;
        }
        assert!(count >= 20, "the shared cases were read: {count}");
    }
}
