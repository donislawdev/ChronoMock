//! The sandbox of an application on Mozilla's Gecko engine (ADR-28).
//!
//! Gecko runs its pages in processes its sandbox broker starts under a restricted token, which the hook
//! does not enter, so the pages of a Gecko application read the real clock under a session. Gecko
//! starts each kind of process without its sandbox when one variable is set, and `--no-gecko-sandbox`
//! has the session set them for the target, whose processes inherit them. These tests hold what the
//! tester is promised either way: the variables reach the program only when asked for, and the report
//! says so - and for a Gecko program run without the flag, that its pages stay on the real clock.
//! Whether a real Gecko application is then covered is measured on one (tools/probes/gecko-start),
//! which CI does not have.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

/// The variables Gecko reads, in the order the core sets them (`gecko::SANDBOX_VARIABLES`).
const VARIABLES: [&str; 7] = [
    "MOZ_DISABLE_CONTENT_SANDBOX",
    "MOZ_DISABLE_GMP_SANDBOX",
    "MOZ_DISABLE_GPU_SANDBOX",
    "MOZ_DISABLE_RDD_SANDBOX",
    "MOZ_DISABLE_SOCKET_PROCESS_SANDBOX",
    "MOZ_DISABLE_UTILITY_SANDBOX",
    "MOZ_DISABLE_VR_SANDBOX",
];

/// How the report names a session that switched the sandbox off.
const SANDBOX_OFF: &str = "(session.gecko_sandbox_off)";

/// How the report names a Gecko program whose pages stay on the real clock.
const PAGES_SANDBOXED: &str = "(session.gecko_pages_sandboxed)";

/// Set for the probe, naming the file it writes. Unset, the probe returns at once.
const PROBE_FILE: &str = "CHRONO_GECKO_SANDBOX_PROBE_FILE";

/// The probe's own name, which is how the binary is asked to run it and nothing else.
const PROBE: &str = "probe_writes_the_sandbox_variables_it_was_given";

/// Writes down which of the variables it was started with, and with what value.
#[test]
#[ignore = "the target of the session tests below, not a test on its own"]
fn probe_writes_the_sandbox_variables_it_was_given() {
    let Some(file) = std::env::var_os(PROBE_FILE) else {
        return;
    };
    let seen: Vec<String> = VARIABLES
        .iter()
        .map(|name| format!("{name}={}", std::env::var(name).unwrap_or_else(|_| "unset".to_string())))
        .collect();
    std::fs::write(file, seen.join("|")).expect("the probe's file");
}

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono")).parent().expect("the binary under test lives in a directory").join("chrono_hook.dll")
}

/// The core allows one session at a time, and the tests of one file run on parallel threads.
static REAL_SESSION: Mutex<()> = Mutex::new(());

fn one_real_session_at_a_time() -> MutexGuard<'static, ()> {
    REAL_SESSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chrono-gecko-sandbox-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// What one run of the probe saw: the line it wrote, the session's report, and both for messages.
struct Seen {
    written: String,
    report: String,
    context: String,
}

/// Runs `program` - this binary or a copy of it - as the probe, under a session when `flags` is `Some`
/// (with those flags) and without one when it is `None`. The tester's own environment carries none of the
/// variables, so whatever the probe sees came from the session.
fn run_probe(program: &Path, dir: &Path, flags: Option<&[&str]>) -> Seen {
    let file = dir.join("seen.txt");
    let _ = std::fs::remove_file(&file);
    let probe_args = format!("--ignored --exact {PROBE} --test-threads 1");
    let mut command = match flags {
        Some(flags) => {
            let mut c = Command::new(env!("CARGO_BIN_EXE_chrono"));
            c.args(["run", &program.display().to_string(), "--at", "2091-06-15T12:00:00", "--args", &probe_args]).args(flags);
            c
        }
        None => {
            let mut c = Command::new(program);
            c.args(probe_args.split(' '));
            c
        }
    };
    command.env(PROBE_FILE, &file);
    for name in VARIABLES {
        command.env_remove(name);
    }
    let out = command.output().expect("the probe must start");
    let written = std::fs::read_to_string(&file).unwrap_or_default();
    let report = String::from_utf8_lossy(&out.stdout).into_owned();
    let context = format!("The probe wrote {written:?}. stdout: {report} stderr: {}", String::from_utf8_lossy(&out.stderr));
    Seen { written, report, context }
}

fn set_to(written: &str, name: &str) -> Option<String> {
    written.split('|').find_map(|part| part.strip_prefix(name)?.strip_prefix('=').map(str::to_string))
}

/// Asked for, every variable reaches the program with a value Gecko reads as set, and the report says the
/// session switched the sandbox off - whatever the program is, because the session did set them. The
/// control first: the probe, run without a session, writes every variable as unset, so a probe that
/// never ran or always wrote them cannot pass this.
#[test]
fn asked_for_the_variables_reach_the_program_and_the_report_says_so() {
    assert!(injected_library().is_file(), "this test drives a real session and needs {}", injected_library().display());
    let me = std::env::current_exe().expect("the test binary knows its own path");
    let dir = scratch("asked");
    let control = run_probe(&me, &dir, None);
    for name in VARIABLES {
        assert_eq!(set_to(&control.written, name).as_deref(), Some("unset"), "the probe without a session. {}", control.context);
    }

    let seen = {
        let _session = one_real_session_at_a_time();
        run_probe(&me, &dir, Some(&["--no-gecko-sandbox"]))
    };
    let _ = std::fs::remove_dir_all(&dir);
    for name in VARIABLES {
        assert_eq!(set_to(&seen.written, name).as_deref(), Some("1"), "{name} under the session. {}", seen.context);
    }
    assert!(seen.report.contains(SANDBOX_OFF), "the report does not say the sandbox was switched off. {}", seen.context);
    assert!(!seen.report.contains(PAGES_SANDBOXED), "and it does not say the pages stay sandboxed. {}", seen.context);
}

/// Not asked for, no variable reaches a Gecko program, and the report says before anything runs that its
/// pages stay on the real clock. The program is a copy of this binary with an empty xul.dll beside it,
/// which is how the core tells a Gecko application, and the same copy without it gets no such word.
#[test]
fn a_gecko_program_run_as_it_is_keeps_its_sandbox_and_the_report_says_its_pages_stay_real() {
    assert!(injected_library().is_file(), "this test drives a real session and needs {}", injected_library().display());
    let me = std::env::current_exe().expect("the test binary knows its own path");
    let dir = scratch("as-it-is");
    let program = dir.join("gecko-probe.exe");
    std::fs::copy(&me, &program).expect("a copy of the probe");

    let (plain, gecko) = {
        let _session = one_real_session_at_a_time();
        let plain = run_probe(&program, &dir, Some(&[]));
        std::fs::write(dir.join("xul.dll"), b"").expect("the library that marks a Gecko application");
        (plain, run_probe(&program, &dir, Some(&[])))
    };
    let _ = std::fs::remove_dir_all(&dir);
    for seen in [&plain, &gecko] {
        assert!(set_to(&seen.written, VARIABLES[0]).is_some(), "the probe never ran under the session. {}", seen.context);
        for name in VARIABLES {
            assert_eq!(set_to(&seen.written, name).as_deref(), Some("unset"), "{name} reached a program run as it is. {}", seen.context);
        }
        assert!(!seen.report.contains(SANDBOX_OFF), "{}", seen.context);
    }
    assert!(gecko.report.contains(PAGES_SANDBOXED), "the report does not say the Gecko program's pages stay real. {}", gecko.context);
    assert!(!plain.report.contains(PAGES_SANDBOXED), "a program without xul.dll is not Gecko. {}", plain.context);
}
