//! A Squirrel starter (`crate::squirrel` in the binary).
//!
//! An application installed by Squirrel.Windows is started through a starter beside `Update.exe`, which
//! only starts the program of its own name in the newest `app-<version>` folder and closes. A session on
//! the starter reached only the starter, so the session starts that program in its place. This test holds
//! it on a real session: the starter, the program in an older version folder and the program in the newest
//! one are all copies of this binary, each writing down where it runs from, so only the newest one writing
//! passes - and the report says the starter was skipped.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Set for the probe, naming the file it writes. Unset, the probe returns at once.
const PROBE_FILE: &str = "CHRONO_SQUIRREL_PROBE_FILE";

/// The probe's own name, which is how the binary is asked to run it and nothing else.
const PROBE: &str = "probe_writes_where_it_runs_from";

/// How the report names a session started in place of the starter.
const STARTER_SKIPPED: &str = "(session.squirrel_starter_skipped)";

/// Writes down the program it runs as and the folder it works in.
#[test]
#[ignore = "the target of the session test below, not a test on its own"]
fn probe_writes_where_it_runs_from() {
    let Some(file) = std::env::var_os(PROBE_FILE) else {
        return;
    };
    let exe = std::env::current_exe().expect("the probe knows its own path");
    let cwd = std::env::current_dir().expect("the probe knows its folder");
    std::fs::write(file, format!("{}|{}", exe.display(), cwd.display())).expect("the probe's file");
}

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono")).parent().expect("the binary under test lives in a directory").join("chrono_hook.dll")
}

#[test]
fn a_session_on_a_starter_runs_the_newest_version_in_its_own_folder_and_says_so() {
    assert!(injected_library().is_file(), "this test drives a real session and needs {}", injected_library().display());
    let me = std::env::current_exe().expect("the test binary knows its own path");
    let dir = std::env::temp_dir().join(format!("chrono-squirrel-starter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for folder in ["app-1.9.0", "app-1.10.0"] {
        std::fs::create_dir_all(dir.join(folder)).expect("a version folder");
        std::fs::copy(&me, dir.join(folder).join("Tool.exe")).expect("the program of that version");
    }
    std::fs::write(dir.join("Update.exe"), b"").expect("the updater beside the starter");
    let starter = dir.join("Tool.exe");
    std::fs::copy(&me, &starter).expect("the starter");
    let newest = dir.join("app-1.10.0");

    let file = dir.join("seen.txt");
    let evidence_file = dir.join("evidence.txt");
    let probe_args = format!("--ignored --exact {PROBE} --test-threads 1");
    // This command runs in the crate's folder, never the version folder, so the evidence names the folder
    // the program really worked in only if it was worked out the way the core works it out.
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &starter.display().to_string(), "--at", "2091-06-15T12:00:00", "--report", &evidence_file.display().to_string(), "--args", &probe_args])
        .env(PROBE_FILE, &file)
        .output()
        .expect("the session must start");
    let written = std::fs::read_to_string(&file).unwrap_or_default();
    let evidence = std::fs::read_to_string(&evidence_file).unwrap_or_default();
    let report = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    let context = format!("The probe wrote {written:?}. stdout: {report} stderr: {stderr} evidence: {evidence}");

    let (program, cwd) = written.split_once('|').unwrap_or_else(|| panic!("the probe wrote nothing. {context}"));
    assert!(Path::new(program).ends_with(newest.join("Tool.exe").strip_prefix(&dir).unwrap()), "the program of 1.10.0 ran, not the starter or 1.9.0. {context}");
    assert!(Path::new(cwd).ends_with("app-1.10.0"), "it worked in its own folder, as the starter would start it. {context}");
    assert!(report.contains(STARTER_SKIPPED), "the report says the starter was skipped. {context}");
    assert!(stderr.contains("only starts the newest version"), "stderr names both programs. {context}");
    // From the end, where the run's own lines stand below the report, which has a target line of its own.
    let line = |label: &str| evidence.lines().rev().find_map(|line| line.trim_start().strip_prefix(label)).map(str::trim).unwrap_or_default().to_string();
    assert!(
        line("target:").contains(&format!("the session started {} in its place", newest.join("Tool.exe").display())),
        "the evidence names the starter and the program started in its place. {context}"
    );
    assert_eq!(
        line("folder:"),
        format!("{} (the program's own folder, as the starter gives it)", newest.display()),
        "the evidence names the folder the program worked in, not this command's. {context}"
    );
}
