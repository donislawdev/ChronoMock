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
use std::sync::{Mutex, MutexGuard};

/// Held by every test here that drives a REAL session. The core allows one session at a time, and
/// the tests of one file run on parallel threads, so two real runs side by side had the second
/// refused with "another session's core is running" - seen on the third run of this file after the
/// second real session came in, having passed the two before it.
static REAL_SESSION: Mutex<()> = Mutex::new(());

/// The lock, whether or not a test holding it before has failed - a failure there says nothing
/// about the next session, and a poisoned lock would turn one red test into several.
fn one_real_session_at_a_time() -> MutexGuard<'static, ()> {
    REAL_SESSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The command interpreter, by full path.
fn command_interpreter() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    format!(r"{root}\System32\cmd.exe")
}

/// The library this tool injects, where the binary under test looks for it: beside itself.
///
/// `cargo test` never produces it. A test build compiles `chrono-hook` as a test harness, so the
/// cdylib is not uplifted into `target/debug`, and the session below refuses before it starts
/// anything. On a machine where somebody has run `cargo build` the file is there from that, which
/// is why the probe below passed by hand and failed on every clean runner from the day it was
/// written. `is_file` rather than `exists`, to ask exactly what `core::hook_dll_in` asks.
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll")
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
    // Asked before the session rather than read out of its report afterwards. Without it the
    // failure arrives as the product's own message about an incomplete installation - the right
    // words for a user holding half a package, and the wrong ones for whoever is running the
    // tests, who has a complete checkout and a missing build step.
    let library = injected_library();
    assert!(
        library.is_file(),
        "this probe drives a real session and needs {}, which `cargo test` does not build. \
         Run `cargo build --workspace` first - CI and tools/gates.ps1 both do that now.",
        library.display()
    );

    let _session = one_real_session_at_a_time();
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

/// The probe above needs an artifact `cargo test` does not build, so whatever runs the workspace
/// tests has to build it first. That ordering is a precondition living in other files, and nothing
/// else would notice it leaving: `tools/gates.ps1` builds too, so the local gates would stay green
/// while every clean machine went red. This is how that was found in the first place.
///
/// It is checked in EVERY committed file that runs those tests, and not only in CI, because the
/// release packaging script was missed when CI was fixed. Phase A of a release calls that script,
/// so the first tagged release failed on precisely the fault this guard exists for, with the guard
/// green beside it. A guard that watches one of two doors reports on the door, not on the house.
///
/// The needle is the START of the line rather than the two words anywhere in it, because a bare
/// `cargo build` is also in the comments that explain these steps and in two release-build steps.
/// An assertion that prose about itself can satisfy is not an assertion.
#[test]
fn every_committed_runner_builds_the_debug_artifacts_before_it_runs_the_tests() {
    // Both doors into the workspace tests. `tools/gates.ps1` is deliberately absent: it lives
    // outside the repository, so a clean clone could not read it and this would fail for the
    // wrong reason - which is its own kind of lie about what is guarded.
    const RUNNERS: [&str; 2] = [".github/workflows/ci.yml", "packaging/build-dist.ps1"];

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");

    for runner in RUNNERS {
        let text = std::fs::read_to_string(root.join(runner))
            .unwrap_or_else(|e| panic!("{runner} is readable: {e}"));

        let mut build = None;
        let mut test = None;
        for (index, line) in text.lines().enumerate() {
            // A YAML step writes `- run: cargo ...` and a PowerShell script writes the command
            // bare. Stripping only those two prefixes keeps a comment, which starts with `#`,
            // out of reach of the match.
            let command = line
                .trim_start()
                .trim_start_matches("- ")
                .trim_start()
                .trim_start_matches("run: ")
                .trim_start();
            if build.is_none() && command.starts_with("cargo build --workspace") {
                build = Some(index);
            }
            if test.is_none() && command.starts_with("cargo test --workspace") {
                test = Some(index);
            }
        }

        let Some(test) = test else {
            panic!("{runner} no longer runs the workspace tests, so this guard is watching nothing")
        };
        let Some(build) = build else {
            panic!(
                "{runner} runs the workspace tests without building the debug artifacts first, so \
                 the probe that drives a real session fails on any clean machine with the \
                 product's message about an incomplete installation"
            )
        };
        assert!(
            build < test,
            "{runner} builds the debug artifacts AFTER running the tests, which is the same as \
             not building them: the session probe reads the directory as it is when it runs"
        );
    }
}

/// A plan refuses what the run would refuse, with the code the run gives for it (docs/08 section 8):
/// a moment the core cannot read exits 1 in the core's own words, and a file Windows will not start
/// exits 2. Both used to exit 0 with a plan calling them sound - measured, while the real run refused
/// both. A batch script is the control: `CreateProcessW` starts one through the command interpreter,
/// so a plan that refused it would be wrong the other way.
#[test]
fn a_plan_refuses_what_the_run_would_refuse_and_nothing_else() {
    let target = command_interpreter();
    for (at, words) in [
        ("2038-13-45T00:00:00", "month out of range"),
        ("2030-02-30T00:00:00", "day 30 out of range for month 2"),
        ("2030-02-28T25:61:00", "hour 25 out of range"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
            .args(["run", &target, "--at", at, "--dry-run"])
            .output()
            .expect("the tool must run");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{at}: {stderr}");
        assert!(stderr.contains(words), "{at}: the refusal must name the field: {stderr}");
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("Nothing was started"),
            "{at}: no plan is printed for a moment that cannot be"
        );
    }

    let dir = scratch("not-a-program");
    let note = dir.join("note.txt");
    std::fs::write(&note, "a note, not a program").expect("a text file");
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &note.display().to_string(), "--at", "2038-01-19T03:14:07", "--dry-run"])
        .output()
        .expect("the tool must run");
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not a program Windows can start"),
        "the reason must be on stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let script = dir.join("script.bat");
    std::fs::write(&script, "@echo off\r\n").expect("a batch script");
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &script.display().to_string(), "--at", "2038-01-19T03:14:07", "--dry-run"])
        .output()
        .expect("the tool must run");
    assert_eq!(out.status.code(), Some(0), "a batch script is started by Windows: {}", String::from_utf8_lossy(&out.stderr));

    // The same script given an argument its launch refuses (a line break would cut the interpreter's
    // line short): the plan refuses it too, with the code the run gives (batch_script.rs).
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &script.display().to_string(), "--args", "\"a\nb\"", "--at", "2038-01-19T03:14:07", "--dry-run"])
        .output()
        .expect("the tool must run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("line break"), "{stderr}");

    // A library is a whole PE image, and still not a program: its header says so, and Windows
    // refuses it. The state, not only the code, because a missing file exits 2 as well.
    let library = injected_library();
    assert!(library.is_file(), "this needs {}, which `cargo test` does not build", library.display());
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &library.display().to_string(), "--at", "2038-01-19T03:14:07", "--dry-run", "--json"])
        .output()
        .expect("the tool must run");
    let plan = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "{plan}");
    assert!(plan.contains(r#""state":"not_a_program""#), "a library is not a program: {plan}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The premise under the refusal above, checked on a real run: the text file and the library really
/// do fail to launch with exit 2. Should the core ever start such a file, the plan's refusal would
/// become the lie, and this is what would say so.
#[test]
fn a_real_run_of_a_file_windows_will_not_start_exits_two() {
    let library = injected_library();
    assert!(
        library.is_file(),
        "this probe drives a real session and needs {}, which `cargo test` does not build. \
         Run `cargo build --workspace` first - CI and tools/gates.ps1 both do that now.",
        library.display()
    );
    let _session = one_real_session_at_a_time();
    let dir = scratch("real-not-a-program");
    let note = dir.join("note.txt");
    std::fs::write(&note, "a note, not a program").expect("a text file");
    for target in [note.display().to_string(), library.display().to_string()] {
        let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
            .args(["run", &target, "--at", "2038-01-19T03:14:07", "--ticks", "1"])
            .output()
            .expect("the tool must run");
        assert_eq!(
            out.status.code(),
            Some(2),
            "{target}: stdout: {} stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
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
