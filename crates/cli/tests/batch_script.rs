//! A batch script as the target, run for real: its arguments reach it as they were given, from a
//! folder whose name holds a space, an ampersand, brackets and a literal `%OS%`.
//!
//! Found 2026-09-25 by a script that writes down what it received. `CreateProcessW` starts the
//! command interpreter for a batch file itself, with no quotes around the line, and the interpreter
//! then strips the first quote and the last one. An argument with a space, or a folder with `&` in
//! its name, kept the script from starting while the session reported a target that vanished, an
//! empty argument vanished and shifted the rest, and `a&b` ran its second half as a separate
//! command. The launch now starts the interpreter itself (`chrono_mech`, `batch.rs`, ADR-15).
//!
//! The evidence is the file the script writes, not the exit code: a script that ends at once can
//! end inside the opening guard window, which is its own verdict (ADR-4). A script that starts a
//! program and ends is the other thing a batch target is for, and the session goes on for the
//! program it started (ADR-16).

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

/// Held by every test here that starts the core. The core allows one session at a time, and the
/// tests of one file run on parallel threads.
static REAL_SESSION: Mutex<()> = Mutex::new(());

fn one_real_session_at_a_time() -> MutexGuard<'static, ()> {
    REAL_SESSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The library the session injects, beside the binary under test. `cargo test` does not build it
/// (see `dry_run.rs`), so its absence is named rather than read as a failure of the launch.
fn require_injected_library() {
    let library = PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll");
    assert!(
        library.is_file(),
        "this drives a real session and needs {}, which `cargo test` does not build. Run `cargo \
         build --workspace` first - CI and tools/gates.ps1 both do that.",
        library.display()
    );
}

/// A folder named the way that broke the launch, with a script in it that writes what it got. Six
/// arguments, each read with `%~N`, which drops the quotes the launch put around it.
fn script_folder(name: &str) -> (PathBuf, PathBuf) {
    // `%OS%` is part of the folder's name, not a variable: the interpreter expands the whole line, the
    // script's path included, and a script in such a folder was not found (measured).
    let dir = std::env::temp_dir().join(format!("chrono batch {name} R&D (x86) %OS% {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let script = dir.join("probe script.bat");
    let body = [
        "@echo off",
        "setlocal EnableDelayedExpansion",
        "set \"a1=%~1\"",
        "set \"a2=%~2\"",
        "set \"a3=%~3\"",
        "set \"a4=%~4\"",
        "set \"a5=%~5\"",
        "set \"a6=%~6\"",
        "> \"%~dp0args.txt\" echo [!a1!][!a2!][!a3!][!a4!][!a5!][!a6!]",
        "",
    ]
    .join("\r\n");
    std::fs::write(&script, body).expect("the script");
    (dir, script)
}

#[test]
fn a_batch_script_gets_its_arguments_as_they_were_given() {
    require_injected_library();
    let _session = one_real_session_at_a_time();
    let (dir, script) = script_folder("args");
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args([
            "run",
            &script.display().to_string(),
            "--args",
            r#""one a" a&b 50% %OS% "" last"#,
            "--at",
            "2038-01-19T03:14:07",
        ])
        .output()
        .expect("the tool must run");
    let said = format!("stdout: {} stderr: {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let got = std::fs::read_to_string(dir.join("args.txt"))
        .unwrap_or_else(|e| panic!("the script never ran ({e}), so it wrote nothing. {said}"));
    // A space, an ampersand, a percent sign, a variable's name, an empty argument and one after it,
    // every one where it was given. `%OS%` stays those four characters, as it does for a program.
    assert_eq!(got.trim_end(), "[one a][a&b][50%][%OS%][][last]", "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two launches the interpreter would not run as given: an argument with a line break, which cuts
/// its line short, and a line longer than the 8 191 units it takes, which it answers with "The
/// command line is too long." Both used to end as a target that vanished. Both are refused before
/// anything starts, with the code of a target that could not be started, and the script never runs.
#[test]
fn a_launch_the_interpreter_would_not_run_is_refused_before_anything_starts() {
    require_injected_library();
    let _session = one_real_session_at_a_time();
    let (dir, script) = script_folder("refused");
    for (args, why) in [("\"a\nb\"".to_string(), "line break"), ("a".repeat(8200), "8191")] {
        let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
            .args(["run", &script.display().to_string(), "--args", &args, "--at", "2038-01-19T03:14:07"])
            .output()
            .expect("the tool must run");
        let said = format!("stdout: {} stderr: {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.status.code(), Some(2), "{why}: {said}");
        assert!(said.contains(why), "the refusal must say why: {said}");
        assert!(!dir.join("args.txt").exists(), "the script must not have run: {said}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A script that starts a program and ends, the most common starter there is. The session used to end
/// with the script: one that ended at once, inside the opening guard window, was reported as a
/// single-instance application, and one that ended later as a session that worked - and either way the
/// program it started was back on the real clock within a second (measured, both ways). The session
/// now goes on for the program (ADR-16). The evidence is the date the program writes down two seconds
/// after the script is gone, not the exit code.
#[test]
fn a_script_that_starts_a_program_and_ends_leaves_the_session_to_that_program() {
    require_injected_library();
    let _session = one_real_session_at_a_time();
    // Ending at once takes the guard-window path, ending after a second the heartbeat's.
    for (name, before) in [("at-once", ""), ("after-a-second", "ping -n 2 127.0.0.1 >nul")] {
        let dir = std::env::temp_dir().join(format!("chrono-starter-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let script = dir.join("starter.bat");
        // `start /b` returns at once and opens no window. The program waits two seconds on the
        // loopback address and writes the date it sees, by which time the script has long ended.
        let body = [
            "@echo off",
            "cd /d \"%~dp0\"",
            before,
            "start \"\" /b cmd /c \"ping -n 3 127.0.0.1 >nul & date /t >seen.txt\"",
            "",
        ]
        .join("\r\n");
        std::fs::write(&script, body).expect("the script");
        let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
            .args(["run", &script.display().to_string(), "--at", "2030-06-15T12:00:00"])
            .output()
            .expect("the tool must run");
        let said = format!("stdout: {} stderr: {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let seen = std::fs::read_to_string(dir.join("seen.txt"))
            .unwrap_or_else(|e| panic!("{name}: the program never wrote its date ({e}). {said}"));
        assert!(seen.contains("2030"), "{name}: the program saw {seen:?} after the script ended, not the session's date. {said}");
        assert_eq!(out.status.code(), Some(0), "{name}: {said}");
        assert!(said.contains("followed:"), "{name}: the report must name what the session went on for. {said}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
