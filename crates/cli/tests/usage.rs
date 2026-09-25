//! What a person gets when they ask the tool for help, or leave a flag without its value: an answer
//! in words that match what they typed.
//!
//! Found by the release checks, which drive the packaged tool the way a person would: `chrono help`
//! said "unknown command", `chrono calc --help` said "unknown flag" and exited 1 while `chrono --help`
//! beside it exited 0, and `--at --dry-run` was answered with a sentence about a "shift" nobody wrote.

use std::process::{Command, Output};

fn chrono(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(args)
        .output()
        .expect("the tool must run")
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

/// Asking is answered: usage, exit 0, no word about anything being unknown.
#[test]
fn every_way_of_asking_for_help_is_answered_with_usage_and_exit_zero() {
    let asks: [&[&str]; 7] = [
        &["--help"],
        &["-h"],
        &["help"],
        &["run", "--help"],
        &["run", "-h"],
        &["calc", "--help"],
        &["calc", "-h"],
    ];
    for args in asks {
        let out = chrono(args);
        let said = text(&out);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {said}");
        assert!(said.contains("usage:"), "{args:?} must print the usage: {said}");
        assert!(!said.contains("unknown"), "{args:?} is a question, not a mistake: {said}");
    }
    // The calculator's help is the calculator's usage, not the whole tool's.
    let calc = text(&chrono(&["calc", "--help"]));
    assert!(calc.contains("chrono calc") && !calc.contains("chrono run <target>"), "{calc}");
}

/// Only the first word after a command asks for its help. Further along, `--help` can belong to the
/// target, and taking it as a question would replace the session with a usage text.
#[test]
fn a_help_flag_meant_for_the_target_is_passed_on_and_not_answered() {
    let out = chrono(&["run", r"C:\definitely\not\here\app.exe", "--args", "--help", "--dry-run"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "the plan must be resolved (and refuse the missing target), not replaced by usage: {}",
        text(&out)
    );
}

/// `--at` takes the next word, so `--at --dry-run` gave it a flag. The refusal names the flag.
#[test]
fn a_flag_where_the_moment_should_be_is_named_as_a_flag() {
    let out = chrono(&["run", r"C:\definitely\not\here\app.exe", "--at", "--dry-run"]);
    let said = text(&out);
    assert_eq!(out.status.code(), Some(1), "{said}");
    assert!(said.contains("--at needs a moment") && said.contains("'--dry-run'"), "{said}");
    assert!(!said.contains("shift needs"), "{said}");
}
