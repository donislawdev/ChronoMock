//! A session's clock has to reach the callers that never go through kernel32.
//!
//! Windows builds the time functions in kernelbase and keeps kernel32 as a stub that jumps there.
//! Anything that imports the api-set (`api-ms-win-core-sysinfo-l1-1-0` and friends) lands in
//! kernelbase directly, and a detour sitting on the kernel32 stub never sees it. That is not an
//! exotic caller: the dynamic C runtime is one (its `time()` and `localtime()`), and so is the
//! command interpreter itself, which formats `%DATE%` from a `GetLocalTime` it imports from the
//! api-set. Until 2026-09-23 the session in `dry_run.rs` ran on exactly that interpreter and never
//! once handed it the session date, while the audit reported the session as working.
//!
//! So the target here is the same interpreter, asked for its date. It needs no compiler and no
//! file outside the repository, which is why it can run on every clean runner - the C probes that
//! found this live in `tools/`, which CI does not have.

use std::path::PathBuf;
use std::process::Command;

/// A year no machine running this test will have on its real clock, so seeing it proves the
/// session reached the target and cannot be a coincidence of today's date.
const SESSION_YEAR: &str = "2091";

/// The command interpreter, by full path (`dry_run.rs` explains why a bare name is not enough).
fn command_interpreter() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    format!(r"{root}\System32\cmd.exe")
}

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll")
}

/// A scratch directory for one run of this test, removed afterwards.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chrono-session-clock-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// What `%DATE%` expanded to in `dir`, written there by the interpreter under test.
fn written_date(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("date.txt")).unwrap_or_default().trim().to_string()
}

/// The interpreter's own date, formatted from a `GetLocalTime` that it imports from the api-set, has
/// to carry the session's year.
///
/// The year is read as four digits, which holds for the short date format of every locale this
/// runs on today (en-US on the runners, pl-PL on the development machine). A locale with a two-digit
/// year would fail this for a reason that has nothing to do with the clock, and the message says so.
#[test]
fn a_target_that_reads_the_clock_through_the_api_set_sees_the_session_date() {
    let library = injected_library();
    assert!(
        library.is_file(),
        "this probe drives a real session and needs {}, which `cargo test` does not build. \
         Run `cargo build --workspace` first - CI and tools/gates.ps1 both do that.",
        library.display()
    );

    // The control first: the same command with no session must NOT show the session year, and must
    // show something. Without it a written file that always contained the year, or a probe that
    // never expands `%DATE%`, would pass the assertion below forever.
    let control = scratch("control");
    let status = Command::new(command_interpreter())
        .args(["/c", "echo %DATE%>date.txt"])
        .current_dir(&control)
        .status()
        .expect("the interpreter must run");
    assert!(status.success(), "the interpreter could not write its date without a session");
    let real = written_date(&control);
    assert!(!real.is_empty(), "%DATE% expanded to nothing, so this probe cannot see a year at all");
    assert!(!real.contains(SESSION_YEAR), "the real clock already says {SESSION_YEAR}: {real}");
    let _ = std::fs::remove_dir_all(&control);

    let dir = scratch("session");
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args([
            "run",
            &command_interpreter(),
            "--args",
            "/c echo %DATE%>date.txt",
            "--cwd",
            &dir.display().to_string(),
            "--at",
            &format!("{SESSION_YEAR}-06-15T12:00:00"),
            "--zone",
            "+00:00",
        ])
        .output()
        .expect("the tool must run");

    let seen = written_date(&dir);
    assert!(
        seen.contains(SESSION_YEAR),
        "the interpreter wrote {seen:?} under a session set to {SESSION_YEAR}, so a caller that goes \
         through the api-set still reads the real clock (or this locale writes a two-digit year). \
         Without the session it wrote {real:?}. stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}
