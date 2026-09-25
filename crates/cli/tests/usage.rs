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
/// target, and taking it as a question would replace the session with a usage text. The plan names
/// the arguments the application would receive, so it shows `--help` arriving there - the tool itself
/// as the target, because the plan has to find a program to print one.
#[test]
fn a_help_flag_meant_for_the_target_is_passed_on_and_not_answered() {
    let out = chrono(&["run", env!("CARGO_BIN_EXE_chrono"), "--args", "--help", "--dry-run", "--json"]);
    let said = text(&out);
    assert_eq!(out.status.code(), Some(0), "the plan, not the usage: {said}");
    assert!(said.contains(r#""args":["--help"]"#), "the application must be handed --help: {said}");
    assert!(!said.contains("usage:"), "{said}");
}

/// `--at` takes the next word, so `--at --dry-run` gave it a flag, and `--at -h` the help flag, whose
/// dash made it a relative moment. The refusal names the flag.
#[test]
fn a_flag_where_the_moment_should_be_is_named_as_a_flag() {
    for flag in ["--dry-run", "-h"] {
        let out = chrono(&["run", r"C:\definitely\not\here\app.exe", "--at", flag]);
        let said = text(&out);
        assert_eq!(out.status.code(), Some(1), "{flag}: {said}");
        assert!(said.contains("--at needs a moment") && said.contains(&format!("'{flag}'")), "{flag}: {said}");
        assert!(!said.contains("shift needs"), "{flag}: {said}");
    }
}
