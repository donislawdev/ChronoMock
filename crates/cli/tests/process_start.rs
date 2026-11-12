//! A program under a session is started by its own main thread, and so is every program it starts (ADR-25).
//!
//! The target is created suspended and the hook goes in before its entry point. When the remote thread that
//! loads the hook was the first thread to run in it, that thread did the process's start: the loader told
//! the program's TLS callbacks "the process is attaching" on it, and on the main thread only "a thread is
//! attaching". A runtime that takes the first call for its main thread set its state up on a thread that
//! ended a moment later - measured with a Free Pascal application, which crashed under every session, also
//! with the remote thread loading nothing but a library already there.
//!
//! The target here is this test binary. The TLS callback below records the thread the loader told the
//! process is attaching, and notes when that thread ends. The probe ends with 0 when that thread is still
//! there, with 3 when it has ended, and with 4 when the callback never ran, which would make the rest blind.
//! The canary starts the probe the old way, with a remote thread first, and requires the 3.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use windows::core::{s, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{VirtualAllocEx, MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE};
use windows::Win32::System::Threading::{
    CreateProcessW, CreateRemoteThread, GetCurrentThreadId, GetExitCodeProcess, ResumeThread, TerminateProcess,
    WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTUPINFOW,
};

/// The probe's mode, set for it. Unset, the probe returns at once.
const PROBE_ON: &str = "CHRONO_PROCESS_START_PROBE";

/// The probe's own name, which is how the binary is asked to run it and nothing else.
const PROBE: &str = "probe_reports_the_thread_that_started_it";

/// The probe's answers.
const STARTED_BY_ITS_OWN_THREAD: u32 = 0;
const STARTER_ENDED: u32 = 3;
const START_NOT_SEEN: u32 = 4;

/// What the loader passes a TLS callback (MS Learn, the PE format, TLS callback functions).
const DLL_PROCESS_ATTACH: u32 = 1;
const DLL_THREAD_DETACH: u32 = 3;

/// The thread the loader told the process is attaching, and whether it has ended since.
static STARTED_BY: AtomicU32 = AtomicU32::new(0);
static STARTER_GONE: AtomicBool = AtomicBool::new(false);

/// The program's own TLS callback, run by the loader in every process of this binary.
unsafe extern "system" fn on_tls_event(_module: *mut c_void, reason: u32, _reserved: *mut c_void) {
    // SAFETY: a call that reads the calling thread's id, nothing else.
    let me = unsafe { GetCurrentThreadId() };
    match reason {
        DLL_PROCESS_ATTACH => STARTED_BY.store(me, Ordering::SeqCst),
        DLL_THREAD_DETACH if me == STARTED_BY.load(Ordering::SeqCst) => STARTER_GONE.store(true, Ordering::SeqCst),
        _ => {}
    }
}

/// The callback, where the C runtime's TLS directory collects the program's callbacks (`.CRT$XLA` to
/// `.CRT$XLZ`, the standard library's own sits at `.CRT$XLB`).
#[used]
#[unsafe(link_section = ".CRT$XLM")]
static TLS_EVENT: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) = on_tls_event;

/// What this process saw of its own start.
fn own_start() -> u32 {
    if STARTED_BY.load(Ordering::SeqCst) == 0 {
        START_NOT_SEEN
    } else if STARTER_GONE.load(Ordering::SeqCst) {
        STARTER_ENDED
    } else {
        STARTED_BY_ITS_OWN_THREAD
    }
}

/// Reads the clock, and ends with what it saw of its own start. As `family`, it then starts two copies of
/// itself - one as a program would, one suspended and resumed a moment later, as a caller that prepares its
/// child does - and ends with the first answer that is not 0, a child's raised by 10 and 20.
#[test]
#[ignore = "the target of the session tests in this file, not a test on its own"]
fn probe_reports_the_thread_that_started_it() {
    let Some(mode) = std::env::var_os(PROBE_ON) else {
        return;
    };
    let _ = SystemTime::now();
    let own = own_start();
    if own != STARTED_BY_ITS_OWN_THREAD || mode != "family" {
        std::process::exit(own as i32);
    }
    let me = std::env::current_exe().expect("the probe knows its own path");
    let plain = Command::new(&me)
        .args(["--ignored", "--exact", PROBE, "--test-threads", "1"])
        .env(PROBE_ON, "self")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("the probe starts a copy of itself");
    let plain = plain.code().unwrap_or(-1) as u32;
    if plain != STARTED_BY_ITS_OWN_THREAD {
        std::process::exit((10 + plain) as i32);
    }
    let held = run_held(&me, Duration::from_millis(300));
    if held != STARTED_BY_ITS_OWN_THREAD {
        std::process::exit((20 + held) as i32);
    }
    std::process::exit(0);
}

/// Held by every test here that starts the core. The core allows one session at a time, and the tests of
/// one file run side by side.
static REAL_SESSION: Mutex<()> = Mutex::new(());

fn one_real_session_at_a_time() -> MutexGuard<'static, ()> {
    REAL_SESSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// This binary's environment with the probe's mode added, as `CreateProcessW` takes it: sorted by name
/// without regard to case (MS Learn, "Changing Environment Variables"), each entry ended, the block ended
/// once more.
fn environment_with_probe(mode: &str) -> Vec<u16> {
    let mut entries: Vec<(String, Vec<u16>)> = std::env::vars_os()
        .filter(|(k, _)| !k.eq_ignore_ascii_case(PROBE_ON))
        .map(|(k, v)| {
            let entry: Vec<u16> = k.encode_wide().chain([u16::from(b'=')]).chain(v.encode_wide()).collect();
            (k.to_string_lossy().to_uppercase(), entry)
        })
        .collect();
    entries.push((PROBE_ON.to_uppercase(), format!("{PROBE_ON}={mode}").encode_utf16().collect()));
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut block = Vec::new();
    for (_, entry) in entries {
        block.extend(entry);
        block.push(0);
    }
    block.push(0);
    block
}

/// The probe in `exe`, created suspended with the mode `self`, and its main thread.
fn start_probe_suspended(exe: &Path) -> PROCESS_INFORMATION {
    let mut line: Vec<u16> = format!("\"{}\" --ignored --exact {PROBE} --test-threads 1", exe.display())
        .encode_utf16()
        .chain([0])
        .collect();
    let app: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
    let env = environment_with_probe("self");
    let startup = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: every buffer is this function's own, alive through the call and terminated as the call needs.
    unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            Some(env.as_ptr().cast()),
            None,
            &startup,
            &mut info,
        )
        .expect("the probe starts suspended");
    }
    info
}

/// Wait for a started probe to end, and its exit code. A probe still running after the limit is ended and
/// reads as `u32::MAX`.
fn finish(info: PROCESS_INFORMATION) -> u32 {
    // SAFETY: the handles are this function's to close, each once.
    unsafe {
        let ended = WaitForSingleObject(info.hProcess, 60_000) == WAIT_OBJECT_0;
        let mut code = u32::MAX;
        if ended {
            GetExitCodeProcess(info.hProcess, &mut code).expect("an exit code");
        } else {
            let _ = TerminateProcess(info.hProcess, u32::MAX);
        }
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
        code
    }
}

/// The probe started suspended, resumed after `held` - a caller that prepares its child before it runs -
/// and its answer.
fn run_held(exe: &Path, held: Duration) -> u32 {
    let info = start_probe_suspended(exe);
    std::thread::sleep(held);
    // SAFETY: the main thread of the process just created.
    assert_ne!(unsafe { ResumeThread(info.hThread) }, u32::MAX, "the probe's main thread resumes");
    finish(info)
}

/// The probe started the way the hook used to be loaded: suspended, then a remote thread running
/// `LoadLibraryW` of a library that is there already, then resumed.
fn run_started_by_a_remote_thread(exe: &Path) -> u32 {
    let info = start_probe_suspended(exe);
    let path: Vec<u16> = "kernel32.dll".encode_utf16().chain([0]).collect();
    // SAFETY: the process was created suspended above and is of this binary's bitness, so the address of
    // `LoadLibraryW` here is its address there. Every handle is closed once.
    unsafe {
        let bytes = path.len() * 2;
        let remote = VirtualAllocEx(info.hProcess, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        assert!(!remote.is_null(), "a page in the probe");
        WriteProcessMemory(info.hProcess, remote, path.as_ptr().cast(), bytes, None).expect("the path written");
        let kernel32 = GetModuleHandleA(s!("kernel32.dll")).expect("kernel32 is loaded");
        let load = GetProcAddress(kernel32, s!("LoadLibraryW")).expect("LoadLibraryW is exported");
        let start = std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            unsafe extern "system" fn(*mut c_void) -> u32,
        >(load);
        let thread: HANDLE = CreateRemoteThread(info.hProcess, None, 0, Some(start), Some(remote), 0, None)
            .expect("a remote thread");
        assert_eq!(WaitForSingleObject(thread, 10_000), WAIT_OBJECT_0, "the remote load returns");
        let _ = CloseHandle(thread);
        assert_ne!(ResumeThread(info.hThread), u32::MAX, "the probe's main thread resumes");
    }
    finish(info)
}

/// The canary: the probe sees the start the hook used to give a program. Started suspended and resumed, it
/// says its own thread started it - and with a remote thread first, that the thread that started it ended.
/// Without the second answer the session tests below would be green over a probe that cannot fail.
#[test]
fn the_probe_tells_a_process_started_by_a_remote_thread() {
    let me = std::env::current_exe().expect("the test binary knows its own path");
    assert_eq!(run_held(&me, Duration::ZERO), STARTED_BY_ITS_OWN_THREAD, "a plain start reads as a foreign one");
    assert_eq!(
        run_started_by_a_remote_thread(&me),
        STARTER_ENDED,
        "the probe did not see that a remote thread started its process - the session tests here prove nothing"
    );
}

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll")
}

/// One session on the probe in `mode`, and the report.
fn session_on_probe(mode: &str) -> (String, String) {
    let library = injected_library();
    assert!(library.is_file(), "this test needs {} - run `cargo build --workspace` first", library.display());
    let me = std::env::current_exe().expect("the test binary knows its own path");
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
    (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The target the core launches is started by its own main thread, and the session still covers it.
#[test]
fn the_target_is_started_by_its_own_main_thread() {
    let _session = one_real_session_at_a_time();
    let (stdout, stderr) = session_on_probe("self");
    let context = format!("stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.contains("verdict:  WORKS"), "the probe did not run under the session. {context}");
    assert!(
        !stdout.contains(&format!("closed itself with code {STARTER_ENDED}\n")),
        "a thread other than the target's main thread started its process. {context}"
    );
    assert!(stdout.contains("the target closed itself with code 0\n"), "{context}");
}

/// A program the target starts is started by its own main thread too - one started as a program does it,
/// and one its caller asked for suspended and resumed itself - and both still join the session.
#[test]
fn the_programs_the_target_starts_are_started_by_their_own_main_threads() {
    let _session = one_real_session_at_a_time();
    let (stdout, stderr) = session_on_probe("family");
    let context = format!("stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.contains("verdict:  WORKS"), "the probe did not run under the session. {context}");
    assert!(!stdout.contains("inheritance.child"), "a child ran outside the session. {context}");
    assert!(
        stdout.contains("the target closed itself with code 0\n"),
        "13: the child the target started was started by another thread, 23: the child it held and resumed was \
         (a code of 10 or 20 plus the child's own answer). {context}"
    );
}
