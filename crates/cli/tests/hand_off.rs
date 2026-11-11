//! A windowed program that hands its work to a copy of itself after the guard window is reported by the
//! copy it went to, and the same program is not reported when it showed a window, when no copy runs, or
//! when the session was cut off before the program ended (ADR-4, 2026-11-11).
//!
//! The target is the Windows Script Host, `wscript.exe`, a windowed program that comes with every Windows
//! installation, running a script that waits without showing anything and then ends on its own - the
//! shape of the second copy of a single-instance application that took longer than the guard window to
//! hand its work on. A second `wscript.exe` this test starts outside the session is the copy. The JScript
//! engine is named on the command line, because a machine can lack the program association of the `.js`
//! extension while the engine is there (measured on the development machine 2026-11-11).

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// Held by every test here that starts the core. The core allows one session at a time, and the tests of
/// one file run on parallel threads.
static REAL_SESSION: Mutex<()> = Mutex::new(());

fn one_real_session_at_a_time() -> MutexGuard<'static, ()> {
    REAL_SESSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll")
}

/// The script host from the system folder, by full path, because the target is never searched for on PATH.
fn script_host() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    format!(r"{root}\System32\wscript.exe")
}

/// A folder of this test's own for its scripts, gone when the test is.
struct Scripts(PathBuf);

impl Scripts {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("chrono-hand-off-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the temp folder is writable");
        Scripts(dir)
    }

    /// A script of `body`, written under `name`.
    fn write(&self, name: &str, body: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, body).expect("the script is written");
        path
    }
}

impl Drop for Scripts {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The arguments that run `script` with the JScript engine, without the host's own messages.
fn script_args(script: &Path) -> String {
    format!("//B //Nologo //E:jscript \"{}\"", script.display())
}

/// A copy of the script host running outside the session, ended when the test is.
struct OutsideCopy(Child);

impl OutsideCopy {
    /// Started with a script that waits a minute, and checked to be running: a host that ends at once
    /// (script hosting turned off on the machine) would leave every finding below unexplained.
    fn start(scripts: &Scripts) -> Self {
        let script = scripts.write("outside.js", "WScript.Sleep(60000);");
        let child = Command::new(script_host())
            .args(["//B", "//Nologo", "//E:jscript"])
            .arg(&script)
            .spawn()
            .expect("the script host starts");
        let mut copy = OutsideCopy(child);
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            copy.0.try_wait().expect("the copy can be asked").is_none(),
            "the script host outside the session ended at once - is script hosting turned off on this machine?"
        );
        copy
    }

    fn pid(&self) -> u64 {
        u64::from(self.0.id())
    }
}

impl Drop for OutsideCopy {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One session over the script host running `args`, with the protocol on stdout: its exit code, stdout
/// and stderr. `extra` goes on the command line after the target's arguments.
fn session(args: &str, extra: &[&str]) -> (Option<i32>, String, String) {
    let library = injected_library();
    assert!(
        library.is_file(),
        "this test drives a real session and needs {}, which `cargo test` does not build. \
         Run `cargo build --workspace` first - CI and tools/gates.ps1 both do that.",
        library.display()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &script_host(), "--at", "2091-06-15T12:00:00", "--zone", "+00:00", "--json", "--args", args])
        .args(extra)
        .output()
        .expect("the tool must run");
    (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The events of the protocol on `stdout`, each line that parses.
fn events(stdout: &str) -> Vec<serde_json::Value> {
    stdout.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok()).collect()
}

/// The family verdict of the session, which every session that started gives.
fn verdict(stdout: &str, context: &str) -> serde_json::Value {
    events(stdout)
        .into_iter()
        .find(|event| event["type"] == "session_verdict")
        .unwrap_or_else(|| panic!("the session gave no verdict. {context}"))
}

/// The pids the verdict names as copies outside the session.
fn copies(verdict: &serde_json::Value) -> Vec<u64> {
    verdict["copies_outside"].as_array().map_or_else(Vec::new, |list| list.iter().filter_map(|c| c["pid"].as_u64()).collect())
}

fn warned(verdict: &serde_json::Value, key: &str) -> bool {
    verdict["warning_keys"].as_array().is_some_and(|keys| keys.iter().any(|k| k == key))
}

/// The script host closes on its own a second and a half in, without a window, while another copy of it
/// runs outside the session: that copy is named, the reason and the warning say what most likely
/// happened, and the verdict counts it against the session.
#[test]
fn a_windowless_program_that_ends_while_a_copy_runs_outside_names_the_copy() {
    let _session = one_real_session_at_a_time();
    let scripts = Scripts::new("copy");
    let outside = OutsideCopy::start(&scripts);
    let target = scripts.write("target.js", "WScript.Sleep(1500);");
    let (code, stdout, stderr) = session(&script_args(&target), &[]);
    let context = format!("outside copy pid {}, exit {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}", outside.pid());
    assert!(!stdout.contains("\"vanished\""), "the program ended after the guard window and was called a vanish. {context}");
    let verdict = verdict(&stdout, &context);
    assert!(copies(&verdict).contains(&outside.pid()), "the copy outside the session was not named. {context}");
    assert_eq!(verdict["reason_key"], "session.family_handed_to_copy", "{context}");
    assert!(warned(&verdict, "session.handed_to_copy"), "the warning that says what to do is missing. {context}");
    assert!(matches!(code, Some(10 | 11)), "a copy outside counts against the verdict. {context}");
}

/// The same program with no copy outside the session is an ordinary session that ended: nothing named,
/// nothing warned.
#[test]
fn a_windowless_program_that_ends_with_no_copy_outside_names_nothing() {
    let _session = one_real_session_at_a_time();
    let scripts = Scripts::new("alone");
    let target = scripts.write("target.js", "WScript.Sleep(1500);");
    let (code, stdout, stderr) = session(&script_args(&target), &[]);
    let context = format!("exit {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    let verdict = verdict(&stdout, &context);
    assert!(copies(&verdict).is_empty(), "a copy was named with none running. {context}");
    assert!(!warned(&verdict, "session.handed_to_copy"), "{context}");
    assert_ne!(verdict["reason_key"], "session.family_handed_to_copy", "{context}");
}

/// A program that showed a window is one the tester had, and closing it while another copy runs is not
/// a hand-off. The script shows a message box that closes itself after two seconds - longer than the
/// heartbeat at which the session looks for a window - and then ends.
#[test]
fn a_program_that_showed_a_window_is_not_reported_with_a_copy_outside() {
    let _session = one_real_session_at_a_time();
    let scripts = Scripts::new("window");
    let outside = OutsideCopy::start(&scripts);
    let target = scripts.write("target.js", "new ActiveXObject(\"WScript.Shell\").Popup(\"Chrono Mock test\", 2, \"Chrono Mock test\", 0);");
    let (code, stdout, stderr) = session(&format!("//Nologo //E:jscript \"{}\"", target.display()), &[]);
    let context = format!("outside copy pid {}, exit {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}", outside.pid());
    let verdict = verdict(&stdout, &context);
    assert!(copies(&verdict).is_empty(), "a program that showed a window was reported as a hand-off. {context}");
    assert!(!warned(&verdict, "session.handed_to_copy"), "{context}");
}

/// A session cut off while the program still ran - here by `--ticks` - asks nothing about copies: an
/// application stopped from outside has handed nothing on. The program the cutoff left running is ended
/// by its pid, so it is not a copy outside the session of the next test.
#[test]
fn a_session_cut_off_while_the_program_runs_names_no_copy() {
    let _session = one_real_session_at_a_time();
    let scripts = Scripts::new("cut");
    let outside = OutsideCopy::start(&scripts);
    let target = scripts.write("target.js", "WScript.Sleep(8000);");
    let (code, stdout, stderr) = session(&script_args(&target), &["--ticks", "2"]);
    let context = format!("outside copy pid {}, exit {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}", outside.pid());
    for pid in events(&stdout).iter().filter(|e| e["type"] == "coverage" && e["kind"] == "process").filter_map(|e| e["pid"].as_u64()) {
        let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).output();
    }
    let verdict = verdict(&stdout, &context);
    assert!(copies(&verdict).is_empty(), "a session cut off from outside was reported as a hand-off. {context}");
    assert!(!warned(&verdict, "session.handed_to_copy"), "{context}");
}
