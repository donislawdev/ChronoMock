//! A program's start and the load of the hook into it share one budget of `INJECT_TIMEOUT_MS` (ADR-25),
//! in the core for the target it launches and in the hook for each child that target starts.
//!
//! The core resumes the target's main thread, which starts the process and parks before the program's
//! entry point, then loads the hook on a remote thread. Nothing reaches the client while it waits for
//! either, and the client calls the core unresponsive after fifteen silent seconds. Each wait used to have
//! ten seconds of its own, so a slow start followed by a slow load kept a healthy core silent for up to
//! twenty. The hook waits the same two ways for a child, inside the application's own call that starts it,
//! which then stood up to twenty seconds too.
//!
//! The target here is this test binary. In `slow` mode its own TLS callback sleeps while the process
//! attaches, which holds its start, and again the first time a thread attaches, which holds the loader
//! lock the remote thread needs before it can load the hook. Each stretch is well inside the budget, and
//! the two together run past it: with a budget per wait the injection succeeds, with one budget it does
//! not.
//!
//! The load a parent stops waiting for still ends in the child a moment later. A slow child therefore
//! waits until that load has come and gone and then says which clock it reads: the real one, because its
//! hook found the gate its parent abandoned and refused (`chrono_ctl::pass_load_gate`).

use std::ffi::c_void;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The probe's mode, set for it. Unset, the probe returns at once and its TLS callback does nothing.
const PROBE_ON: &str = "CHRONO_INJECTION_BUDGET_PROBE";

/// The modes. `slow` starts slowly and lets the hook load slowly. `family` starts a copy of itself in
/// `slow`, as a program does, and ends with how long that call took and what the child answered.
const SLOW: &str = "slow";
const FAMILY: &str = "family";

/// The probe's own name, which is how the binary is asked to run it and nothing else.
const PROBE: &str = "probe_starts_slowly_or_starts_a_slow_child";

/// How long a `slow` process holds its own start, and then the first thread that attaches. Each is well
/// inside the ten seconds an injection is given, and together they run two seconds past it - the margin
/// that tells one budget from two.
const START_MS: u32 = 4_000;
const LOAD_MS: u32 = 8_000;

/// The longest the start of a slow child may hold the call that starts it, in tenths of a second: the
/// budget and a second for the rest. With a budget per wait it took the two stretches together, twelve.
const CALL_LIMIT_TENTHS: u32 = 110;

/// The shortest it may hold it: the child's own slow start, or the child was not slow and nothing here
/// was measured.
const CALL_FLOOR_TENTHS: u32 = START_MS / 100;

/// What `family` ends with when the child could not be started at all, outside both bounds.
const NOT_STARTED: i32 = 9_999;

/// How long a `slow` child that runs waits before it reads the clock: well past the end of a load its
/// parent stopped waiting for, which a hook that joined late would have hooked by then.
const SETTLE_MS: u64 = 4_000;

/// The session's moment, 2077-01-01T00:00:00Z, in seconds since 1970. A program on the real clock reads a
/// date decades earlier.
const MOMENT_SECS: u64 = 3_376_684_800;

/// What a `slow` child ends with: it read the real clock, as its parent's report says - or the session's,
/// because a load its parent stopped waiting for hooked it halfway through its run.
const CHILD_ON_THE_REAL_CLOCK: u32 = 0;
const CHILD_HOOKED_LATE: u32 = 5;

/// The longest a session on a slow target may take to give up on it: the budget, and a second and a half
/// for the core to start and report. With a budget per wait the target was let in after twelve.
const GIVE_UP_LIMIT: Duration = Duration::from_millis(11_500);

/// What the loader passes a TLS callback (MS Learn, the PE format, TLS callback functions).
const DLL_PROCESS_ATTACH: u32 = 1;
const DLL_THREAD_ATTACH: u32 = 2;

#[cfg_attr(target_arch = "x86", link(name = "kernel32", kind = "raw-dylib", import_name_type = "undecorated"))]
#[cfg_attr(not(target_arch = "x86"), link(name = "kernel32", kind = "raw-dylib"))]
unsafe extern "system" {
    fn GetEnvironmentVariableW(name: *const u16, buffer: *mut u16, size: u32) -> u32;
    fn Sleep(ms: u32);
}

/// Whether this process runs in `slow` mode, and whether it has held a thread yet.
static SLOW_PROCESS: AtomicBool = AtomicBool::new(false);
static HELD_A_THREAD: AtomicBool = AtomicBool::new(false);

/// Whether the probe's mode is `slow`, read without allocating: the loader calls the TLS callback before
/// anything of the program has run.
fn runs_slow() -> bool {
    let mut name = [0u16; 32];
    for (to, from) in name.iter_mut().zip(PROBE_ON.encode_utf16()) {
        *to = from;
    }
    let mut value = [0u16; 8];
    // SAFETY: the name is ended by a zero, and the buffer is this function's own, of the length passed.
    let length = unsafe { GetEnvironmentVariableW(name.as_ptr(), value.as_mut_ptr(), value.len() as u32) };
    let wanted = SLOW.encode_utf16();
    length as usize == SLOW.len() && value.iter().copied().zip(wanted).all(|(seen, want)| seen == want)
}

/// The program's own TLS callback, run by the loader in every process of the binary. In a `slow` process
/// it holds the start while the process attaches, and the first thread that attaches after it, under the
/// loader lock - so the remote thread that loads the hook waits for it, whichever thread it is.
unsafe extern "system" fn on_tls_event(_module: *mut c_void, reason: u32, _reserved: *mut c_void) {
    match reason {
        DLL_PROCESS_ATTACH if runs_slow() => {
            SLOW_PROCESS.store(true, Ordering::SeqCst);
            // SAFETY: no pointers, it waits.
            unsafe { Sleep(START_MS) };
        }
        DLL_THREAD_ATTACH if SLOW_PROCESS.load(Ordering::SeqCst) && !HELD_A_THREAD.swap(true, Ordering::SeqCst) => {
            // SAFETY: no pointers, it waits.
            unsafe { Sleep(LOAD_MS) };
        }
        _ => {}
    }
}

/// The callback, where the C runtime's TLS directory collects the program's callbacks (`.CRT$XLA` to
/// `.CRT$XLZ`, the standard library's own sits at `.CRT$XLB`).
#[used]
#[unsafe(link_section = ".CRT$XLM")]
static TLS_EVENT: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) = on_tls_event;

/// In `family`, starts a copy of itself in `slow`, waits for it, and ends with how long the call that
/// started it took, in tenths of a second, times ten, plus the child's answer. In `slow` it waits for a
/// late load to have come and gone, and ends with which clock it reads.
#[test]
#[ignore = "the target of the session tests in this file, not a test on its own"]
fn probe_starts_slowly_or_starts_a_slow_child() {
    let Some(mode) = std::env::var_os(PROBE_ON) else {
        return;
    };
    if mode != FAMILY {
        std::thread::sleep(Duration::from_millis(SETTLE_MS));
        let on_the_sessions_clock = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .is_ok_and(|since| since.as_secs() >= MOMENT_SECS);
        let answer = if on_the_sessions_clock { CHILD_HOOKED_LATE } else { CHILD_ON_THE_REAL_CLOCK };
        std::process::exit(answer as i32);
    }
    let me = std::env::current_exe().expect("the probe knows its own path");
    let began = Instant::now();
    let child = Command::new(&me)
        .args(["--ignored", "--exact", PROBE, "--test-threads", "1"])
        .env(PROBE_ON, SLOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let took = began.elapsed();
    let Ok(mut child) = child else {
        std::process::exit(NOT_STARTED);
    };
    let answer = child.wait().ok().and_then(|status| status.code()).map_or(9, |code| code.clamp(0, 9));
    let tenths = i32::try_from(took.as_millis() / 100).unwrap_or(i32::MAX / 20);
    std::process::exit(tenths.saturating_mul(10).saturating_add(answer));
}

/// Held by every test here that starts the core. The core allows one session at a time, and the tests of
/// one file run side by side.
static REAL_SESSION: Mutex<()> = Mutex::new(());

fn one_real_session_at_a_time() -> MutexGuard<'static, ()> {
    REAL_SESSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One session on the probe in `mode`: its report, its errors, its exit code and how long it took.
fn session_on_probe(mode: &str) -> (String, String, Option<i32>, Duration) {
    let library = std::path::PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll");
    assert!(library.is_file(), "this test needs {} - run `cargo build --workspace` first", library.display());
    let me = std::env::current_exe().expect("the test binary knows its own path");
    let began = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args([
            "run",
            &me.display().to_string(),
            "--at",
            "2077-01-01T00:00:00",
            "--zone",
            "+00:00",
            "--args",
            &format!("--ignored --exact {PROBE} --test-threads 1"),
        ])
        .env(PROBE_ON, mode)
        .output()
        .expect("the tool must run");
    let took = began.elapsed();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
        took,
    )
}

/// A target whose start and whose load each fit the budget, and together do not, is given up on once the
/// budget is spent - the load gets what the start left, not ten seconds of its own - so the core is never
/// silent past the client's watchdog.
#[test]
fn the_targets_start_and_the_hooks_load_share_one_budget() {
    let _session = one_real_session_at_a_time();
    let (stdout, stderr, code, took) = session_on_probe(SLOW);
    let context = format!("took {took:?}, exit {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("[target.inject_failed]"),
        "the slow target was let in - the load had a budget of its own. {context}"
    );
    assert!(
        stderr.contains("the hook did not load within the"),
        "the injection did not end on the load's share of the budget - the start ran out, or something else \
         failed. {context}"
    );
    assert_eq!(code, Some(2), "a target that could not be injected is exit 2. {context}");
    assert!(took < GIVE_UP_LIMIT, "the session took longer than one budget to give up. {context}");
}

/// A child the target starts gets the same one budget, so the application's own call that starts it is
/// held no longer than that, and a child the budget ran out on is said to run on the real clock - and
/// does: the load its parent stopped waiting for ends a moment later, and its hook refuses rather than
/// hook the program halfway through its run.
#[test]
fn a_childs_start_and_the_hooks_load_share_one_budget() {
    let _session = one_real_session_at_a_time();
    let (stdout, stderr, code, took) = session_on_probe(FAMILY);
    let context = format!("took {took:?}, exit {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    let ended_with: u32 = stdout
        .lines()
        .find_map(|line| line.split_once("the target closed itself with code ").map(|(_, code)| code))
        .and_then(|code| code.trim().parse().ok())
        .unwrap_or_else(|| panic!("the target did not end with how long its call took. {context}"));
    assert_ne!(ended_with, NOT_STARTED as u32, "the target could not start its child. {context}");
    let (call_tenths, child) = (ended_with / 10, ended_with % 10);
    assert_eq!(
        child, CHILD_ON_THE_REAL_CLOCK,
        "the child read the session's clock ({CHILD_HOOKED_LATE}): the load its parent stopped waiting for \
         hooked it halfway through its run. {context}"
    );
    assert!(
        (CALL_FLOOR_TENTHS..CALL_LIMIT_TENTHS).contains(&call_tenths),
        "starting the slow child held the application for {call_tenths} tenths of a second - over the limit \
         the load had a budget of its own, under the floor the child was not slow. {context}"
    );
    assert!(
        stdout.contains("inheritance.child"),
        "a child the budget ran out on was not named as running on the real clock. {context}"
    );
}
