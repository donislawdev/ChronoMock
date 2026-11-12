//! A program the application starts under its own debugger starts at once, on the session clock and by
//! its own main thread (R5-P25, ADR-25).
//!
//! A debugged child's first thread raises its first debug events and stands until the debugger answers
//! them. When the application is that debugger, the thread answering them is the very one that called
//! CreateProcess - and the hook used to wait for the load inside that call. Measured on both bitnesses
//! (tools/probes/r5/fala4/p25.ps1): the call stood the whole ten-second limit, the child was then
//! resumed before the hook had loaded, its program read the real clock, and the report named it a
//! process on the real clock. The hook now hands the wait to a thread of its own for such a child, and
//! the child's own main thread starts it and waits before its entry point while the hook is loaded.
//!
//! The application here is this test binary: one ignored probe debugs two children, one for each flag
//! that makes the caller a debugger, and the children are the same binary running a second probe that
//! writes the wall clock it reads and which thread started its process (`support/start_thread.rs`).

#[path = "support/start_thread.rs"]
mod start_thread;

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The folder the probes write into. Unset, both probes return at once.
const OUT: &str = "CHRONO_DEBUGGED_CHILD_PROBE_OUT";

/// The file one child writes its wall clock to, set by the debugging probe for each child.
const CHILD_FILE: &str = "CHRONO_DEBUGGED_CHILD_PROBE_FILE";

/// The debugging probe's own name, which is how the binary is asked to run it and nothing else.
const DEBUGGER_PROBE: &str = "probe_debugs_two_children_and_times_their_start";

/// The child probe's own name.
const CHILD_PROBE: &str = "probe_writes_the_wall_clock_it_reads";

/// The session's moment, 2077-01-01T00:00:00Z, in seconds since 1970. A child on the real clock reads
/// a date decades earlier.
const MOMENT_SECS: u64 = 3_376_684_800;

/// Half the ten seconds the hook gives a load, which is how long the start used to stand. A start on
/// the session's side of this line did not wait for the load in the debugger's own thread.
const AT_ONCE_MS: f64 = 5_000.0;

/// Well past the session's opening guard window of 300 ms (ADR-4), which a target gone sooner falls in.
const PAST_THE_GUARD: Duration = Duration::from_secs(1);

/// How long the debugging probe answers one child's events before it gives up on it.
const DEBUG_LIMIT_MS: f64 = 60_000.0;

/// The creation flags that make the caller the debugger of the child (MS Learn, process creation flags),
/// each with the name its child's file and the probe's result line carry.
const DEBUG_FLAGS: [(&str, u32); 2] = [("debug_only_this_process", 0x0000_0002), ("debug_process", 0x0000_0001)];

const EXCEPTION_DEBUG_EVENT: u32 = 1;
const CREATE_PROCESS_DEBUG_EVENT: u32 = 3;
const EXIT_PROCESS_DEBUG_EVENT: u32 = 5;
const LOAD_DLL_DEBUG_EVENT: u32 = 6;
const DBG_CONTINUE: u32 = 0x0001_0002;
const DBG_EXCEPTION_NOT_HANDLED: u32 = 0x8001_0001;
/// The breakpoint the loader raises in a debugged process, and its form for a 32-bit process.
const STATUS_BREAKPOINT: u32 = 0x8000_0003;
const STATUS_WX86_BREAKPOINT: u32 = 0x4000_001F;

/// `DEBUG_EVENT`: the event's code and the process and thread it is about, then a union that is
/// pointer-aligned, as `info` is here, and fits in it on either bitness. The union's first field is the
/// exception code of an exception event and the file handle of a create-process or load-library event.
#[repr(C)]
struct DebugEvent {
    code: u32,
    pid: u32,
    tid: u32,
    info: [usize; 32],
}

#[cfg_attr(target_arch = "x86", link(name = "kernel32", kind = "raw-dylib", import_name_type = "undecorated"))]
#[cfg_attr(not(target_arch = "x86"), link(name = "kernel32", kind = "raw-dylib"))]
unsafe extern "system" {
    fn WaitForDebugEvent(event: *mut DebugEvent, milliseconds: u32) -> i32;
    fn ContinueDebugEvent(pid: u32, tid: u32, status: u32) -> i32;
    fn DebugActiveProcessStop(pid: u32) -> i32;
    fn CloseHandle(handle: usize) -> i32;
}

#[cfg_attr(target_arch = "x86", link(name = "ntdll", kind = "raw-dylib", import_name_type = "undecorated"))]
#[cfg_attr(not(target_arch = "x86"), link(name = "ntdll", kind = "raw-dylib"))]
unsafe extern "system" {
    /// The reference clock. The hook never touches it (ADR-2), so it stays real under any session.
    fn NtQueryPerformanceCounter(counter: *mut i64, frequency: *mut i64) -> i32;
}

/// Real milliseconds by the unhooked reference clock.
fn real_ms() -> f64 {
    let (mut counter, mut frequency) = (0i64, 0i64);
    // SAFETY: both pointers are to live locals.
    unsafe { NtQueryPerformanceCounter(&mut counter, &mut frequency) };
    counter as f64 * 1000.0 / frequency as f64
}

/// Answer the debug events of the process this thread started until `child` exits. A breakpoint the
/// loader raises is passed over, every other exception goes back to the child's own handlers, and the
/// file handles an event hands the debugger are closed.
fn debug_until_exit(child: u32) -> Result<(), String> {
    let give_up_at = real_ms() + DEBUG_LIMIT_MS;
    loop {
        let mut event = DebugEvent { code: 0, pid: 0, tid: 0, info: [0; 32] };
        // SAFETY: a live event to fill, and this thread is the one that started the debugged child.
        if unsafe { WaitForDebugEvent(&mut event, 1_000) } == 0 {
            if real_ms() > give_up_at {
                return Err(format!("the child gave no end in {DEBUG_LIMIT_MS} ms"));
            }
            continue;
        }
        let status = match event.code {
            EXCEPTION_DEBUG_EVENT if matches!(event.info[0] as u32, STATUS_BREAKPOINT | STATUS_WX86_BREAKPOINT) => {
                DBG_CONTINUE
            }
            EXCEPTION_DEBUG_EVENT => DBG_EXCEPTION_NOT_HANDLED,
            CREATE_PROCESS_DEBUG_EVENT | LOAD_DLL_DEBUG_EVENT => {
                if event.info[0] != 0 {
                    // SAFETY: the debugger owns the file handle these two events carry.
                    unsafe { CloseHandle(event.info[0]) };
                }
                DBG_CONTINUE
            }
            _ => DBG_CONTINUE,
        };
        // SAFETY: the process and thread the event named, answered once.
        unsafe { ContinueDebugEvent(event.pid, event.tid, status) };
        if event.code == EXIT_PROCESS_DEBUG_EVENT && event.pid == child {
            return Ok(());
        }
    }
}

/// Starts this binary twice under its own debugger, once with each flag in `DEBUG_FLAGS`, answers each
/// child's events until it exits, and writes how long each start took, one line per flag.
#[test]
#[ignore = "the target of the session test in this file, not a test on its own"]
fn probe_debugs_two_children_and_times_their_start() {
    let Some(out) = std::env::var_os(OUT) else {
        return;
    };
    let out = PathBuf::from(out);
    let me = std::env::current_exe().expect("the probe knows its own path");
    let mut lines = Vec::new();
    for (name, flag) in DEBUG_FLAGS {
        let started = real_ms();
        let mut child = Command::new(&me)
            .args(["--ignored", "--exact", CHILD_PROBE, "--test-threads", "1"])
            .env(CHILD_FILE, out.join(format!("{name}.txt")))
            .creation_flags(flag)
            .spawn()
            .expect("the debugged child is created");
        let took = real_ms() - started;
        let ended = debug_until_exit(child.id());
        if ended.is_err() {
            // A debugged process that is ended waits for its debugger to answer that too, so the
            // debugger lets go of it first.
            // SAFETY: the pid of a process this thread debugs.
            unsafe { DebugActiveProcessStop(child.id()) };
            let _ = child.kill();
        }
        let _ = child.wait();
        lines.push(format!("{name} {took:.0} {}", ended.err().unwrap_or_else(|| "exited".to_string())));
    }
    std::fs::write(out.join("debugger.txt"), lines.join("\n")).expect("the probe writes its result");
    std::thread::sleep(PAST_THE_GUARD);
}

/// Writes the wall clock this process reads, in seconds since 1970, and what it saw of its own start.
#[test]
#[ignore = "the child of the debugging probe in this file, not a test on its own"]
fn probe_writes_the_wall_clock_it_reads() {
    let Some(file) = std::env::var_os(CHILD_FILE) else {
        return;
    };
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let start = start_thread::own_start();
    std::fs::write(PathBuf::from(file), format!("{secs} {start}")).expect("the child writes its clock");
}

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll")
}

#[test]
fn a_child_started_under_the_applications_debugger_starts_at_once_and_on_the_session_clock() {
    let library = injected_library();
    assert!(
        library.is_file(),
        "this test drives a real session and needs {}, which `cargo test` does not build. \
         Run `cargo build --workspace` first - CI and tools/gates.ps1 both do that.",
        library.display()
    );
    let dir = std::env::temp_dir().join(format!("chrono-debugged-child-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let me = std::env::current_exe().expect("the test binary knows its own path");

    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &me.display().to_string(), "--at", "2077-01-01T00:00:00", "--zone", "+00:00", "--json"])
        .args(["--args", &format!("--ignored --exact {DEBUGGER_PROBE} --test-threads 1")])
        .env(OUT, &dir)
        .output()
        .expect("the tool must run");
    let debugger = std::fs::read_to_string(dir.join("debugger.txt")).unwrap_or_default();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let context = || {
        format!(
            "the debugger wrote {debugger:?}\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    };

    for (name, _) in DEBUG_FLAGS {
        let line = debugger
            .lines()
            .find(|l| l.split(' ').next() == Some(name))
            .unwrap_or_else(|| panic!("the debugger wrote nothing about its {name} child. {}", context()));
        let mut words = line.split(' ').skip(1);
        let took: f64 = words.next().and_then(|w| w.parse().ok()).unwrap_or(f64::MAX);
        assert_eq!(words.next(), Some("exited"), "the {name} child did not end under its debugger. {}", context());
        assert!(
            took < AT_ONCE_MS,
            "starting the {name} child stood {took} ms - the hook waited for its load in the debugger's own thread. {}",
            context()
        );
        let read = std::fs::read_to_string(dir.join(format!("{name}.txt"))).unwrap_or_default();
        let mut child_words = read.split_whitespace();
        let secs: u64 = child_words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
        let start: Option<u32> = child_words.next().and_then(|w| w.parse().ok());
        assert!(
            secs >= MOMENT_SECS,
            "the {name} child read {read:?} seconds since 1970, not the session's moment, so it ran before the hook. {}",
            context()
        );
        assert_eq!(
            start,
            Some(start_thread::STARTED_BY_ITS_OWN_THREAD),
            "the {name} child wrote {read:?}: 3 another thread than its own main thread started its process, \
             4 it did not see its start. {}",
            context()
        );
    }
    let verdict = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|event| event["type"] == "session_verdict")
        .unwrap_or_else(|| panic!("the session gave no verdict. {}", context()));
    assert_eq!(
        verdict["uncovered_children_total"], 0,
        "a debugged child was named a process on the real clock. {}",
        context()
    );
    assert_eq!(verdict["verdict"], "works", "the session is not a works session. {}", context());
    assert_eq!(out.status.code(), Some(0), "a works session is exit 0. {}", context());
    let _ = std::fs::remove_dir_all(&dir);
}
