//! chrono started through a symbolic link finds what lies beside the program, not beside the link.
//!
//! A link to the program can stand in another folder: WinGet makes one for a portable package unless the
//! manifest asks for the folder on PATH instead, and anybody can make one with `mklink`. chrono used to
//! look beside the path it was started by, the link, so the hook, the calendars and the presets were all
//! missing there (measured 2026-11-02). Here a copy of the package is laid out in a scratch folder (this
//! build's `chrono.exe` and `chrono_hook.dll`, the repository's calendars and presets) with a link to the
//! program in another folder. Through the link, `chrono calendars` has to name the package's calendars,
//! and a real session on this test binary has to start: a run that cannot find the hook stops with
//! `core.hook_dll_missing` before the target is started at all. The last test keeps every lookup of the
//! program's own path going through the one function that follows a link.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Where the probe writes that it ran. Unset, the probe returns at once.
const PROBE_FILE: &str = "CHRONO_LINKED_START_PROBE_OUT";

/// The probe's own name, which is how this binary is asked to run it and nothing else.
const PROBE: &str = "probe_runs_under_the_linked_session";

/// How long the probe lives, in real milliseconds. Past the guard window (300 ms, ADR-4), so a target this
/// short is not taken for one that handed its work to another process.
const PROBE_LIVES_MS: u64 = 1_000;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// A scratch copy of the package, a link to its program in another folder, and a third folder to start
/// from, so neither the package nor the repository is the current directory. Removed when dropped.
struct Layout {
    root: PathBuf,
    package: PathBuf,
    link: PathBuf,
    elsewhere: PathBuf,
}

impl Drop for Layout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn layout(name: &str) -> Layout {
    let root = std::env::temp_dir().join(format!("chrono-linked-start-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let package = root.join("package");
    let links = root.join("links");
    let elsewhere = root.join("elsewhere");
    for dir in [&package, &links, &elsewhere] {
        std::fs::create_dir_all(dir).expect("a scratch folder");
    }
    let built = PathBuf::from(env!("CARGO_BIN_EXE_chrono"));
    std::fs::copy(&built, package.join("chrono.exe")).expect("this build's chrono.exe");
    let hook = built.parent().expect("the binary under test lives in a folder").join("chrono_hook.dll");
    assert!(
        hook.is_file(),
        "this test drives a real session and needs {}, which `cargo test` does not build (dry_run.rs has the whole story)",
        hook.display()
    );
    std::fs::copy(&hook, package.join("chrono_hook.dll")).expect("this build's hook");
    for kind in ["calendars", "presets"] {
        copy_catalogue(&repo().join(kind), &package.join(kind));
    }
    let link = links.join("chrono.exe");
    if let Err(e) = std::os::windows::fs::symlink_file(package.join("chrono.exe"), &link) {
        panic!(
            "a symbolic link could not be made ({e}). Windows lets an account make one with Developer Mode on or \
             with administrator rights, and this one has neither, so the case is not measured here. CI runs it"
        );
    }
    Layout { root, package, link, elsewhere }
}

fn copy_catalogue(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a catalogue folder");
    for entry in std::fs::read_dir(from).expect("the repository's catalogue").flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            std::fs::copy(&path, to.join(entry.file_name())).expect("a catalogue file");
        }
    }
}

fn said(out: &Output) -> String {
    format!(
        "exit {:?}, stdout: {} stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn chrono_started_through_a_link_reads_the_calendars_beside_the_program() {
    let place = layout("calendars");
    let out = Command::new(&place.link)
        .arg("calendars")
        .current_dir(&place.elsewhere)
        .output()
        .expect("chrono through the link must run");
    let context = said(&out);
    assert_eq!(out.status.code(), Some(0), "{context}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let folder = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("folder:"))
        .map(str::trim)
        .unwrap_or_else(|| panic!("the listing names no folder. {context}"));
    // Both sides canonical: a scratch folder can come back through a short 8.3 name on one side only.
    let want = std::fs::canonicalize(place.package.join("calendars")).expect("the package's calendars");
    let got = std::fs::canonicalize(folder).unwrap_or_else(|e| panic!("the folder it named is not there ({e}). {context}"));
    assert_eq!(got, want, "chrono through the link read another calendars folder. {context}");
}

#[test]
fn a_session_started_through_a_link_finds_the_hook_beside_the_program() {
    let place = layout("session");
    let file = place.root.join("probe.txt");
    let me = std::env::current_exe().expect("the test binary knows its own path");
    let out = Command::new(&place.link)
        .args([
            "run",
            &me.display().to_string(),
            "--at",
            "2061-03-01T09:00:00",
            "--zone",
            "+00:00",
            "--args",
            &format!("--ignored --exact {PROBE} --test-threads 1"),
        ])
        .env(PROBE_FILE, &file)
        .current_dir(&place.elsewhere)
        .output()
        .expect("chrono through the link must run");
    let context = said(&out);
    assert!(
        !context.contains("hook_dll_missing") && !context.contains("chrono_hook.dll not found"),
        "the core looked for the hook beside the link. {context}"
    );
    assert!(file.is_file(), "the target never ran under the session. {context}");
}

/// The target of the session above: it lives past the guard window and writes down that it ran.
#[test]
#[ignore = "the target of the session test above, not a test on its own"]
fn probe_runs_under_the_linked_session() {
    let Some(file) = std::env::var_os(PROBE_FILE) else {
        return;
    };
    std::thread::sleep(std::time::Duration::from_millis(PROBE_LIVES_MS));
    std::fs::write(file, "ran").expect("the probe's file");
}

/// Every lookup of the program's own path goes through `own_path::own_exe`, which follows a link. A new
/// `std::env::current_exe()` anywhere else in the program's code would bring the bug back for whatever it
/// looks up, so it is refused here with the file and the line. Test code after `#[cfg(test)]` may ask for
/// the test binary's own path, which is no link.
#[test]
fn the_program_asks_for_its_own_path_only_through_the_function_that_follows_a_link() {
    let src = repo().join("crates").join("cli").join("src");
    let mut files = Vec::new();
    collect_sources(&src, &mut files);
    let mut asked = Vec::new();
    let mut follower_asks = false;
    for path in &files {
        let text = std::fs::read_to_string(path).expect("a source file");
        let program = text.lines().enumerate().take_while(|(_, line)| !line.trim_start().starts_with("#[cfg(test)]"));
        for (number, line) in program {
            if line.trim_start().starts_with("//") || !line.contains("current_exe(") {
                continue;
            }
            if path.file_name().is_some_and(|name| name == "own_path.rs") {
                follower_asks = true;
            } else {
                asked.push(format!("{}:{}", path.display(), number + 1));
            }
        }
    }
    assert!(files.len() >= 20, "the scan read {} source files under {} - not the program", files.len(), src.display());
    assert!(follower_asks, "own_path.rs no longer asks for the path itself, so this scan would pass over nothing");
    assert!(asked.is_empty(), "the program's own path asked for without following a link (use own_path::own_exe): {asked:?}");
}

fn collect_sources(dir: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("a source folder").flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sources(&path, into);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            into.push(path);
        }
    }
}
