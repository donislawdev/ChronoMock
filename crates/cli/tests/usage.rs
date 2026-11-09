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

/// Asking is answered: usage on standard output, nothing on standard error, exit 0, no word about
/// anything being unknown. The usage went to standard error with exit 0, so `chrono --help | more`
/// showed nothing and `chrono calc --help > usage.txt` wrote an empty file.
#[test]
fn every_way_of_asking_for_help_is_answered_with_usage_and_exit_zero() {
    let asks: [&[&str]; 12] = [
        &[],
        &["--help"],
        &["-h"],
        &["help"],
        &["run", "--help"],
        &["run", "-h"],
        &["calc", "--help"],
        &["calc", "-h"],
        &["calendars", "--help"],
        &["calendars", "-h"],
        &["presets", "--help"],
        &["presets", "-h"],
    ];
    for args in asks {
        let out = chrono(args);
        let said = text(&out);
        let usage = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {said}");
        assert!(out.stderr.is_empty(), "{args:?} is answered on stdout alone: {said}");
        assert!(!said.contains("unknown"), "{args:?} is a question, not a mistake: {said}");
        // Each command answers with its own usage, and `run` and the tool itself with the whole tool's,
        // which opens with `run` - a help routed to another command's usage is red here.
        match args.first() {
            Some(&command @ ("calc" | "presets" | "calendars")) => {
                assert!(usage.starts_with(&format!("usage: chrono {command} ")), "{args:?} must print its own usage: {said}");
                assert!(!usage.contains("chrono run <target>"), "{args:?} printed the whole tool's usage: {said}");
            }
            _ => assert!(usage.starts_with("usage: chrono run <target>"), "{args:?} must print the tool's usage: {said}"),
        }
    }
}

/// A refused command line is explained with the usage under the refusal, both on standard error, and
/// standard output stays empty - a script that sends it to a file gets an empty file, not a usage text
/// it would read as a result.
#[test]
fn a_refusal_keeps_the_usage_off_standard_output() {
    // Each with the sentence that names what was wrong, first, so the usage under it cannot stand in for
    // the refusal itself.
    let refused: [(&[&str], &str); 5] = [
        (&["bogus"], "chrono: unknown command 'bogus'"),
        (&["run"], "chrono: missing <target>"),
        (&["calc", "--shift", "+abcd"], "chrono calc: shift needs a number, got '+abcd' (calc.bad_argument)"),
        (&["presets", "--bogus"], "chrono presets: unknown argument '--bogus' for presets"),
        (&["calendars", "--bogus"], "chrono calendars: unknown argument '--bogus' for calendars"),
    ];
    for (args, why) in refused {
        let out = chrono(args);
        let said = text(&out);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {said}");
        assert!(out.stdout.is_empty(), "{args:?} wrote on stdout: {said}");
        assert_eq!(stderr.lines().next(), Some(why), "{args:?} must say what was wrong first: {said}");
        assert!(stderr.contains("usage: chrono"), "{args:?} must explain itself with the usage: {said}");
    }
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
