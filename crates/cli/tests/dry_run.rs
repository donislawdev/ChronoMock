//! The one promise `--dry-run` makes that no unit test can check: it starts nothing.
//!
//! Everything else about the flag is a pure rendering, tested where it is built. This is the other
//! half - the branch that returns before the spawn. A unit test cannot see that branch move, because
//! moving it below the spawn changes no value it could read. So this runs the real executable against
//! a target that would leave a mark on the filesystem, and looks for the mark.
//!
//! The target is the command interpreter with a `mkdir` into a scratch directory. Chosen because the
//! evidence is a directory rather than parsed output - present or absent, with nothing to interpret.
//!
//! It is named by FULL PATH, and that is not tidiness. The first version of this file wrote `cmd.exe`
//! and the control below failed: the mechanism hands the target to `CreateProcessW` as
//! `lpApplicationName`, which Microsoft documents as never using the search path. That failure is
//! what corrected `--dry-run` itself, which until then declined to judge a bare name.

use std::path::PathBuf;
use std::process::Command;

/// The command interpreter, by full path.
fn command_interpreter() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    format!(r"{root}\System32\cmd.exe")
}

/// A scratch directory for one run of this test, removed afterwards.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chrono-dry-run-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// The flag's whole contract. If the branch that returns before the spawn were ever moved below it,
/// the target would run, `cmd` would create the directory, and this is what would say so.
#[test]
fn a_dry_run_starts_no_target() {
    let dir = scratch("starts-nothing");
    let marker = dir.join("the-target-ran");

    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args([
            "run",
            &command_interpreter(),
            "--args",
            "/c mkdir the-target-ran",
            "--cwd",
            &dir.display().to_string(),
            "--at",
            "2038-01-19T03:14:07",
            "--dry-run",
        ])
        .output()
        .expect("the tool must run");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Nothing was started and nothing was written"),
        "the plan must be on stdout: {stdout}"
    );
    assert!(
        !marker.exists(),
        "a dry run started the target - it created {}",
        marker.display()
    );
    assert_eq!(out.status.code(), Some(0), "a valid plan exits 0: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The same command line without the flag, so the probe above is known to be able to fail. A guard
/// whose subject cannot happen is a guard nobody has seen work - here the target does run, creates
/// the directory, and the assertion above would catch it.
///
/// This one drives a real session on `cmd.exe`, which is what the tool does for a living.
#[test]
fn the_same_command_line_without_the_flag_does_start_the_target() {
    let dir = scratch("starts-something");
    let marker = dir.join("the-target-ran");

    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args([
            "run",
            &command_interpreter(),
            "--args",
            "/c mkdir the-target-ran",
            "--cwd",
            &dir.display().to_string(),
            "--at",
            "2038-01-19T03:14:07",
        ])
        .output()
        .expect("the tool must run");

    assert!(
        marker.exists(),
        "the probe cannot prove anything: without --dry-run the target did not run either. \
         stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A path with a directory component that holds no file is a fact the plan can establish without
/// starting anything, and it exits 2 - the code a real run gives for the same fact. That is what
/// makes `--dry-run` usable as a pre-flight check rather than a pretty printer.
#[test]
fn a_path_that_leads_to_no_file_exits_two_without_starting_anything() {
    let missing = std::env::temp_dir().join("chrono-dry-run-no-such-target.exe");
    let _ = std::fs::remove_file(&missing);

    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &missing.display().to_string(), "--dry-run"])
        .output()
        .expect("the tool must run");

    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("would not start anything"),
        "the reason must be on stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A bare name that IS on PATH is the case worth pinning, because it is the one a reader expects
    // to work. It does not: the mechanism resolves a target as a path and never through PATH, so the
    // plan exits 2 exactly as the real run does - measured, `chrono run notepad` exits 2 too.
    let on_path = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", "notepad", "--dry-run"])
        .output()
        .expect("the tool must run");
    assert_eq!(
        on_path.status.code(),
        Some(2),
        "a bare name on PATH is not resolved by the mechanism, and the plan must say the same: {}",
        String::from_utf8_lossy(&on_path.stdout)
    );
    assert!(
        String::from_utf8_lossy(&on_path.stdout).contains("never through PATH"),
        "the plan must say why: {}",
        String::from_utf8_lossy(&on_path.stdout)
    );
}
