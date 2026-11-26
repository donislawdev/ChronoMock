//! A reader that goes away must not crash the tool (R4-S11).
//!
//! `println!` and `eprintln!` panic when the write fails, and a pipe whose reader has closed fails
//! every write. So `chrono run app.exe --json | head -n 3` ended in a panic and exit code 101, a
//! number no table documents, and the core spoke through the same macros: a panel that died took the
//! core down in the middle of `close_session`, before it put the family back on rate 1 (ADR-14).
//!
//! Each case hands the binary a pipe whose read end is already closed. That is deterministic where a
//! real `head` would race the first write, and it is exactly what the child sees once any reader has
//! gone: every write fails. The exit code is then checked against the one the same command line
//! gives with somebody reading, because a closed output changes what reaches the caller, not what
//! the tool found.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard};

/// Held by every test here that starts the core. The core allows one session at a time, and tests
/// of one file run on parallel threads (the same lock as in `dry_run.rs`, for the same reason).
static CORE: Mutex<()> = Mutex::new(());

fn one_core_at_a_time() -> MutexGuard<'static, ()> {
    CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A pipe nobody reads: the write end goes to the child, the read end is dropped before it starts.
fn closed_pipe() -> std::io::PipeWriter {
    let (reader, writer) = std::io::pipe().expect("a pipe");
    drop(reader);
    writer
}

/// Which of the two outputs the case closes.
#[derive(Clone, Copy, Debug)]
enum Closed {
    Stdout,
    Stderr,
}

/// Run the built binary with one output closed and the other one read.
fn run_with(closed: Closed, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_chrono"));
    command.args(args).stdin(Stdio::null());
    match closed {
        Closed::Stdout => command.stdout(closed_pipe()).stderr(Stdio::piped()),
        Closed::Stderr => command.stdout(Stdio::piped()).stderr(closed_pipe()),
    };
    command.output().expect("the tool must run")
}

/// The command interpreter, by full path (see `dry_run.rs` for why a bare name will not do).
fn command_interpreter() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    format!(r"{root}\System32\cmd.exe")
}

/// The library the core injects, beside the binary under test. `cargo test` does not build it.
fn injected_library_is_built() {
    let library = PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll");
    assert!(
        library.is_file(),
        "this test starts the core and needs {}, which `cargo test` does not build. \
         Run `cargo build --workspace` first - CI does that before the tests.",
        library.display()
    );
}

fn describe(out: &Output) -> String {
    format!(
        "exit {:?}, stdout: {} stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Every command that answers at once, with nobody reading the answer: each still exits with the
/// code it exits with when read, and says once, on the output that is still open, that the answer
/// was not delivered (a run that did less than it promised says so, rule 6).
#[test]
fn a_closed_standard_output_leaves_every_answer_its_exit_code() {
    let interpreter = command_interpreter();
    let calendars = concat!(env!("CARGO_MANIFEST_DIR"), "/../dates/catalogue/calendars");
    let cases: [&[&str]; 8] = [
        &["version"],
        &["license"],
        &["calendars", "--dir", calendars],
        &["calendars", "--dir", calendars, "--json"],
        &["calc", "--base", "2030-01-01T00:00:00", "--zone", "+00:00"],
        &["calc", "--base", "2030-01-01T00:00:00", "--zone", "+00:00", "--json"],
        &["run", &interpreter, "--at", "2030-01-01T00:00:00", "--zone", "+00:00", "--dry-run"],
        &["run", &interpreter, "--at", "2030-01-01T00:00:00", "--zone", "+00:00", "--dry-run", "--json"],
    ];
    // Every case is run before anything is asserted, so a failure names all of them at once.
    let mut wrong = Vec::new();
    for args in cases {
        let out = run_with(Closed::Stdout, args);
        let said = String::from_utf8_lossy(&out.stderr).matches("standard output").count();
        if out.status.code() != Some(0) || said != 1 {
            wrong.push(format!("{args:?} said it {said} times, {}", describe(&out)));
        }
    }
    assert!(
        wrong.is_empty(),
        "each must exit 0 and say exactly once that its answer was not delivered: {wrong:#?}"
    );
}

/// A refusal whose explanation nobody reads is still a refusal: usage errors keep exit code 1.
#[test]
fn a_closed_standard_error_leaves_a_refusal_its_exit_code() {
    let mut wrong = Vec::new();
    for args in [&["calc", "--no-such-flag"][..], &["run"][..], &["no-such-command"][..]] {
        let out = run_with(Closed::Stderr, args);
        if out.status.code() != Some(1) {
            wrong.push(format!("{args:?}: {}", describe(&out)));
        }
    }
    assert!(wrong.is_empty(), "each must exit 1: {wrong:#?}");
}

/// The core writes its human-side detail to the standard error it shares with the driver. A target
/// that cannot be started is exit code 2 from the core, and it has to stay 2 when nobody reads that
/// detail - not the driver's 3 for a core that died, and not a panic in the driver itself.
#[test]
fn the_core_keeps_its_exit_code_when_its_standard_error_is_closed() {
    injected_library_is_built();
    let _core = one_core_at_a_time();
    let missing = std::env::temp_dir().join("chrono-closed-output-no-such-dir").join("missing.exe");
    let missing = missing.display().to_string();
    let out = run_with(Closed::Stderr, &["run", &missing, "--at", "2030-01-01T00:00:00", "--zone", "+00:00"]);
    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
}

/// A session whose reader left after the first line: the driver stops copying events, keeps
/// collecting them, and ends the session the way it would have ended anyway. The evidence file is
/// the proof it got that far, since nothing it printed can be read.
#[test]
fn a_session_runs_to_its_end_when_nobody_reads_its_output() {
    injected_library_is_built();
    let _core = one_core_at_a_time();
    let dir = std::env::temp_dir().join(format!("chrono-closed-output-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let evidence = dir.join("evidence.txt");

    let out = run_with(
        Closed::Stdout,
        &[
            "run",
            &command_interpreter(),
            "--args",
            "/c ping -n 2 127.0.0.1 >nul",
            "--at",
            "2030-01-01T00:00:00",
            "--zone",
            "+00:00",
            "--json",
            "--report",
            &evidence.display().to_string(),
        ],
    );

    let written = std::fs::read_to_string(&evidence).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        written.contains("verdict"),
        "the session did not reach its end - no evidence was written. {}",
        describe(&out)
    );
    let code = out.status.code();
    assert!(
        matches!(code, Some(0 | 4 | 10 | 11 | 12)),
        "the exit code must still be the session verdict (docs/08 section 8): {}",
        describe(&out)
    );
}
