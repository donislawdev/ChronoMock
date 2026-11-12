//! A windowed program that ends inside the guard window and is then held in its exit still ends inside
//! the window (ADR-4, 2026-11-12).
//!
//! The guard window reads a target gone within 300 ms of its release as the second instance of a
//! single-instance application handing its work to the first, exit 12. A process that has finished its
//! own work can still be held in the kernel on its way out: a display driver's process-exit callback held
//! such processes for up to 2.6 s on the machine it was measured on, so a target that ended inside the
//! window was sometimes still there at its end, and the session went on as if it ran. The hook now marks
//! the moment its process begins to exit, and a mark inside the window followed by the real end within a
//! grace counts as an end inside the window.
//!
//! The target is a copy of this test binary whose image header names it a windowed program, because only
//! a windowed family is suspected of handing off. A C runtime initializer, which runs right after the
//! release, ends it, and its own TLS callback holds the process at its exit, after the hook has detached
//! (the hook was loaded after the program's start, so the loader detaches it first): the same place a
//! driver holds it, as seen from the hook.
//!
//! A mark alone is never an end: a program held past the grace goes on as a session. And a launched
//! program gone by the end of the window still heads a session that looks for copies of it running
//! outside, which needs its path - read before it is let go, because the system no longer gives the path
//! of a process that has ended.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

/// The probe's mode, set for it. Unset, the program runs as the test binary it is.
const PROBE_ON: &str = "CHRONO_HELD_EXIT_PROBE";

/// The modes: end at once, end at once and be held at the exit, run past the window and be held at the
/// exit, end at once and be held past the grace, start another program and end at once, run as the
/// program a `launch` started, run as a copy outside the session.
const PLAIN: &str = "plain";
const HELD: &str = "held";
const HELD_LATE: &str = "late";
const STUCK: &str = "stuck";
const LAUNCH: &str = "launch";
const STARTED: &str = "started";
const OUTSIDE: &str = "outside";

/// How long a held process stands in its exit: well past the 300 ms window, well inside the core's grace.
const HOLD_MS: u32 = 1_000;

/// How long a `late` process runs before it ends: past the window, by more than the window itself.
const LATE_MS: u32 = 700;

/// How long a `stuck` process stands in its exit: past the window and the core's 3 s grace together, by
/// well over a second.
const STUCK_MS: u32 = 5_000;

/// How long the program a `launch` starts runs: past the window, so the session goes on for it.
const STARTED_MS: u32 = 1_500;

/// How long a copy outside the session runs when the test does not end it first.
const OUTSIDE_MS: u32 = 60_000;

/// Each mode with how long the program runs before it ends and how long it then stands in its exit.
const MODES: [(&str, u32, u32); 7] = [
    (PLAIN, 0, 0),
    (HELD, 0, HOLD_MS),
    (HELD_LATE, LATE_MS, HOLD_MS),
    (STUCK, 0, STUCK_MS),
    (LAUNCH, 0, 0),
    (STARTED, STARTED_MS, 0),
    (OUTSIDE, OUTSIDE_MS, 0),
];

/// The file of the program a `launch` starts, beside the windowed copy: another file, so the copy outside
/// is a copy of the launched program and of nothing else in the session.
const STARTED_FILE: &str = "started.exe";

/// `IMAGE_SUBSYSTEM_WINDOWS_GUI`, what the copy's header is made to say.
const SUBSYSTEM_WINDOWS: u16 = 2;

/// What the loader passes a TLS callback when the process detaches (MS Learn, the PE format, TLS callback
/// functions).
const DLL_PROCESS_DETACH: u32 = 0;

#[cfg_attr(target_arch = "x86", link(name = "kernel32", kind = "raw-dylib", import_name_type = "undecorated"))]
#[cfg_attr(not(target_arch = "x86"), link(name = "kernel32", kind = "raw-dylib"))]
unsafe extern "system" {
    fn GetEnvironmentVariableW(name: *const u16, buffer: *mut u16, size: u32) -> u32;
    fn Sleep(ms: u32);
    fn ExitProcess(code: u32) -> !;
}

/// How long this process stands in its exit, 0 when it does not.
static HOLD_AT_EXIT_MS: AtomicU32 = AtomicU32::new(0);

/// Whether the probe's mode is `wanted`, read without allocating: the C runtime is not up yet.
fn mode_is(wanted: &str) -> bool {
    let mut name = [0u16; 32];
    for (to, from) in name.iter_mut().zip(PROBE_ON.encode_utf16()) {
        *to = from;
    }
    let mut value = [0u16; 8];
    // SAFETY: the name is ended by a zero, and the buffer is this function's own, of the length passed.
    let length = unsafe { GetEnvironmentVariableW(name.as_ptr(), value.as_mut_ptr(), value.len() as u32) };
    length as usize == wanted.len() && value.iter().copied().zip(wanted.encode_utf16()).all(|(seen, want)| seen == want)
}

/// Run by the C runtime's start-up right after the program was let go, before `main`: in a probe mode it
/// ends the process there, so the end comes milliseconds after the release on any machine.
extern "C" fn on_program_start() {
    let Some(&(mode, run_ms, hold_ms)) = MODES.iter().find(|(mode, _, _)| mode_is(mode)) else {
        return;
    };
    if mode == LAUNCH {
        start_another_program();
    }
    HOLD_AT_EXIT_MS.store(hold_ms, Ordering::SeqCst);
    // SAFETY: no pointers - a wait, then the end of this process.
    unsafe {
        Sleep(run_ms);
        ExitProcess(0);
    }
}

/// Start the file beside this one in `started` mode, as a launcher starts its program. The standard
/// library's allocator is the system heap, which is up before any initializer runs.
fn start_another_program() {
    let Ok(me) = std::env::current_exe() else {
        return;
    };
    let _ = Command::new(me.with_file_name(STARTED_FILE))
        .env(PROBE_ON, STARTED)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// The initializer, where the C runtime collects the ones its start-up runs (`.CRT$XCA` to `.CRT$XCZ`, the
/// ones of a program at `.CRT$XCU`).
#[used]
#[unsafe(link_section = ".CRT$XCU")]
static PROGRAM_START: extern "C" fn() = on_program_start;

/// The program's own TLS callback: a held process stands here as it detaches, after the hook has.
unsafe extern "system" fn on_tls_event(_module: *mut c_void, reason: u32, _reserved: *mut c_void) {
    let hold_ms = HOLD_AT_EXIT_MS.load(Ordering::SeqCst);
    if reason == DLL_PROCESS_DETACH && hold_ms > 0 {
        // SAFETY: no pointers, it waits.
        unsafe { Sleep(hold_ms) };
    }
}

/// The callback, where the C runtime's TLS directory collects the program's callbacks (`.CRT$XLA` to
/// `.CRT$XLZ`, the standard library's own sits at `.CRT$XLB`).
#[used]
#[unsafe(link_section = ".CRT$XLM")]
static TLS_EVENT: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) = on_tls_event;

/// Held by every test here that starts the core. The core allows one session at a time, and the tests of
/// one file run side by side.
static REAL_SESSION: Mutex<()> = Mutex::new(());

fn one_real_session_at_a_time() -> MutexGuard<'static, ()> {
    REAL_SESSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The windowed copy and the scratch directory it sits in, which goes when the test ends - an assertion
/// that fails included.
struct Scratch {
    dir: PathBuf,
    program: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A copy of the windowed program running outside the session, ended when the test is. Declared after the
/// `Scratch` it runs from, so it is ended before the directory goes.
struct Outside(Child);

impl Outside {
    fn start(program: &Path) -> Self {
        let child = Command::new(program)
            .env(PROBE_ON, OUTSIDE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the copy outside the session starts");
        Outside(child)
    }

    fn pid(&self) -> u64 {
        u64::from(self.0.id())
    }
}

impl Drop for Outside {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A copy of this test binary whose header names it a windowed program, in a scratch directory of its own.
fn windowed_copy(name: &str) -> Scratch {
    let library = PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll");
    assert!(library.is_file(), "this test needs {} - run `cargo build --workspace` first", library.display());
    let me = std::env::current_exe().expect("the test binary knows its own path");
    let mut image = std::fs::read(&me).expect("the test binary is readable");
    let pe = u32::from_le_bytes(image[0x3C..0x40].try_into().expect("four bytes")) as usize;
    assert_eq!(&image[pe..pe + 4], b"PE\0\0", "the test binary has a PE header where its DOS header says");
    let subsystem = pe + 24 + 68;
    image[subsystem..subsystem + 2].copy_from_slice(&SUBSYSTEM_WINDOWS.to_le_bytes());
    let dir = std::env::temp_dir().join(format!("chrono-held-exit-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let program = dir.join("windowed.exe");
    std::fs::write(&program, &image).expect("the windowed copy is written");
    Scratch { dir, program }
}

/// One session over the windowed copy in `mode`, with the protocol on stdout: its exit code, its events,
/// and everything said.
fn session(target: &Path, mode: &str) -> (Option<i32>, Vec<serde_json::Value>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &target.display().to_string(), "--at", "2091-06-15T12:00:00", "--zone", "+00:00", "--json"])
        .env(PROBE_ON, mode)
        .output()
        .expect("the tool must run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let events = stdout.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).collect();
    let context = format!("exit {:?}\nstdout:\n{stdout}\nstderr:\n{}", out.status.code(), String::from_utf8_lossy(&out.stderr));
    (out.status.code(), events, context)
}

/// The event of `kind`, when the session gave one.
fn event<'a>(events: &'a [serde_json::Value], kind: &str) -> Option<&'a serde_json::Value> {
    events.iter().find(|event| event["type"] == kind)
}

/// The control: a windowed program that ends at once is gone inside the window. Without it the test
/// below could be red for a probe that never ends in time.
#[test]
fn a_windowed_program_that_ends_at_once_is_gone_inside_the_window() {
    let _session = one_real_session_at_a_time();
    let scratch = windowed_copy("plain");
    let (code, events, context) = session(&scratch.program, PLAIN);
    assert!(event(&events, "vanished").is_some(), "a windowed program that ended at once was not seen gone. {context}");
    assert_eq!(code, Some(12), "{context}");
}

/// A windowed program that ends at once and is held in its exit for a second is gone inside the window
/// too, and has lived as long as its own work took, not as long as it was held.
#[test]
fn a_windowed_program_held_in_its_exit_is_gone_inside_the_window() {
    let _session = one_real_session_at_a_time();
    let scratch = windowed_copy("held");
    let (code, events, context) = session(&scratch.program, HELD);
    let vanished = event(&events, "vanished")
        .unwrap_or_else(|| panic!("a program held in its exit was taken for one that ran on. {context}"));
    assert_eq!(code, Some(12), "{context}");
    let lived = vanished["lived_ms"].as_u64().unwrap_or(u64::MAX);
    assert!(lived < 300, "it lived {lived} ms - the hold was counted as its life. {context}");
}

/// A windowed program that runs past the window and is then held in its exit ran on: its mark is after
/// the window, and the session goes on to its end as any other.
#[test]
fn a_windowed_program_that_ends_after_the_window_ran_on() {
    let _session = one_real_session_at_a_time();
    let scratch = windowed_copy("late");
    let (code, events, context) = session(&scratch.program, HELD_LATE);
    assert!(event(&events, "vanished").is_none(), "a program that ran past the window was seen gone inside it. {context}");
    assert_eq!(code, Some(0), "a session over a program that ran and ended is exit 0. {context}");
}

/// A windowed program that ends at once and stands in its exit past the core's grace has a mark inside the
/// window, but a mark is not an end: the session goes on to the real end, as for a program that ran.
#[test]
fn a_windowed_program_held_in_its_exit_past_the_grace_goes_on_as_a_session() {
    let _session = one_real_session_at_a_time();
    let scratch = windowed_copy("stuck");
    let (code, events, context) = session(&scratch.program, STUCK);
    assert!(event(&events, "vanished").is_none(), "the mark of an exit was taken for the end itself. {context}");
    assert_eq!(code, Some(0), "a session over a program that ran and ended is exit 0. {context}");
}

/// A windowed program that starts another program and ends at once, while a copy of it runs outside the
/// session: the session goes on for the program it started (ADR-16), and when that one ends without a
/// window, the copy outside is named. The launched program is gone long before, and the search for its
/// copies needs its path.
#[test]
fn a_launcher_gone_inside_the_window_still_names_a_copy_of_itself_outside() {
    let _session = one_real_session_at_a_time();
    let scratch = windowed_copy("launch");
    std::fs::copy(&scratch.program, scratch.dir.join(STARTED_FILE)).expect("the program the launcher starts");
    let outside = Outside::start(&scratch.program);
    let (code, events, context) = session(&scratch.program, LAUNCH);
    let context = format!("outside copy pid {}, {context}", outside.pid());
    assert!(event(&events, "vanished").is_none(), "a launcher whose program ran on was taken for a vanish. {context}");
    let verdict = event(&events, "session_verdict").unwrap_or_else(|| panic!("the session gave no verdict. {context}"));
    let copies: Vec<u64> = verdict["copies_outside"]
        .as_array()
        .map_or_else(Vec::new, |list| list.iter().filter_map(|copy| copy["pid"].as_u64()).collect());
    assert!(copies.contains(&outside.pid()), "the copy of the launched program outside was not named. {context}");
    assert_eq!(verdict["reason_key"], "session.family_handed_to_copy", "{context}");
    assert!(matches!(code, Some(10 | 11)), "a copy outside counts against the verdict. {context}");
}
