//! A session's duration axis has to reach the callers that never go through kernel32.
//!
//! The second half of what `session_clock.rs` guards. Windows keeps the tick count, the interrupt time
//! and QPC in kernelbase and ntdll, and kernel32 either stubs them or keeps a copy of its own. A caller
//! that imports the api-set (`msvcrt`, `user32`, `combase`, the network stacks) lands there directly,
//! and until 2026-09-23 it read the real tick count under a session scaling the duration axis x60 while
//! the audit counted nothing.
//!
//! No system program shows a value it computes from the tick count, the way `cmd.exe` shows `%DATE%`,
//! so the target here is this test binary itself: `probe_reads_the_tick_count_through_the_api_set`
//! does its work only when the variable below names a file. It needs no compiler and no file outside
//! the repository, and unlike an example target it exists whichever way `cargo test` was filtered.
//!
//! The probe resolves `api-ms-win-core-sysinfo-l1-1-0` at run time rather than importing from it. A
//! static import was the first version, and its revert probe showed it guarded nothing: the standard
//! library imports `GetTickCount64` from kernel32, the linker kept one import for the name, and the
//! test read kernel32's copy with and without the fix. Resolving the api-set lands exactly where an
//! importer lands, and the probe refuses to run if that address turns out to be kernel32's.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the probe writes what it measured. Unset, the probe returns at once, so running the ignored
/// tests by hand does nothing.
const PROBE_OUT: &str = "CHRONO_DURATION_AXIS_PROBE_OUT";

/// The probe's own name, which is how the binary is asked to run it and nothing else.
const PROBE: &str = "probe_reads_the_tick_count_through_the_api_set";

#[cfg_attr(target_arch = "x86", link(name = "kernel32", kind = "raw-dylib", import_name_type = "undecorated"))]
#[cfg_attr(not(target_arch = "x86"), link(name = "kernel32", kind = "raw-dylib"))]
unsafe extern "system" {
    fn LoadLibraryA(name: *const core::ffi::c_char) -> *mut core::ffi::c_void;
    fn GetModuleHandleA(name: *const core::ffi::c_char) -> *mut core::ffi::c_void;
    fn GetProcAddress(module: *mut core::ffi::c_void, name: *const core::ffi::c_char) -> *mut core::ffi::c_void;
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

/// How far the api-set tick count moves per real millisecond, over a 300 ms window spun on the reference
/// clock. The spin, not a sleep, because the session shortens sleeps and would shrink the window.
#[test]
#[ignore = "the target of `a_tick_count_read_through_the_api_set_follows_the_session_speed`, not a test on its own"]
fn probe_reads_the_tick_count_through_the_api_set() {
    let Some(out) = std::env::var_os(PROBE_OUT) else {
        return;
    };
    let name = c"GetTickCount64".as_ptr();
    // SAFETY: NUL-terminated names, and both modules stay loaded for the life of the process.
    let (through_api_set, kernel32s) = unsafe {
        (
            GetProcAddress(LoadLibraryA(c"api-ms-win-core-sysinfo-l1-1-0.dll".as_ptr()), name),
            GetProcAddress(GetModuleHandleA(c"kernel32.dll".as_ptr()), name),
        )
    };
    if through_api_set.is_null() || through_api_set == kernel32s {
        std::fs::write(&out, "kernel32").expect("the probe writes its result");
        return;
    }
    // SAFETY: the export's documented signature, no arguments and no state.
    let tick: unsafe extern "system" fn() -> u64 = unsafe { std::mem::transmute(through_api_set) };
    // SAFETY: as above.
    let before = unsafe { tick() };
    let start = real_ms();
    while real_ms() - start < 300.0 {}
    let window = real_ms() - start;
    // SAFETY: as above.
    let moved = unsafe { tick() } - before;
    std::fs::write(&out, format!("{:.1}", moved as f64 / window)).expect("the probe writes its result");
    // Alive past the session's opening guard window (ADR-4), spun on the unhooked clock.
    let settle = real_ms();
    while real_ms() - settle < 400.0 {}
}

/// The injected library, which `cargo test` does not build (`dry_run.rs` has the whole story).
fn injected_library() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_chrono"))
        .parent()
        .expect("the binary under test lives in a directory")
        .join("chrono_hook.dll")
}

/// The probe's result: api-set tick milliseconds per real millisecond, or `None` if it wrote nothing.
fn ratio(file: &Path) -> Option<f64> {
    std::fs::read_to_string(file).ok()?.trim().parse().ok()
}

/// A tick count read through the api-set moves at the session's speed. x60 gives about 60, and the
/// bar is 10 so that a loaded runner cannot fail it while a real clock (about 1) always does.
#[test]
fn a_tick_count_read_through_the_api_set_follows_the_session_speed() {
    let library = injected_library();
    assert!(
        library.is_file(),
        "this probe drives a real session and needs {}, which `cargo test` does not build. \
         Run `cargo build --workspace` first - CI and tools/gates.ps1 both do that.",
        library.display()
    );
    let me = std::env::current_exe().expect("the test binary knows its own path");
    let dir = std::env::temp_dir().join(format!("chrono-duration-axis-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let probe_args = ["--ignored", "--exact", PROBE, "--test-threads", "1"];

    // The control first: the same probe with no session must see the real clock. Without it a probe
    // that wrote a large number for any reason would pass the assertion below forever.
    let control = dir.join("control.txt");
    let run = Command::new(&me)
        .args(probe_args)
        .env(PROBE_OUT, &control)
        .output()
        .expect("the probe must run without a session");
    assert!(run.status.success(), "the probe failed without a session: {}", String::from_utf8_lossy(&run.stdout));
    let written = std::fs::read_to_string(&control).unwrap_or_default();
    assert_ne!(written, "kernel32", "the api-set resolved to kernel32's own entry, so this probe cannot see the other path");
    let real = ratio(&control).expect("the probe wrote nothing without a session");
    assert!(real < 2.0, "without a session the api-set tick count moved x{real}, so this probe cannot tell");

    let session = dir.join("session.txt");
    let out = Command::new(env!("CARGO_BIN_EXE_chrono"))
        .args(["run", &me.display().to_string(), "--mode", "x60", "--scale-duration", "--args", &probe_args.join(" ")])
        .env(PROBE_OUT, &session)
        .output()
        .expect("the tool must run");
    let seen = ratio(&session);
    assert!(
        seen.is_some_and(|x| x > 10.0),
        "under x60 --scale-duration the tick count read through the api-set moved x{seen:?} (x{real} \
         without a session), so a caller that imports the api-set still reads the real tick count. \
         stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
