//! What a process saw of its own start, for the session tests that need every program started by its own
//! main thread (ADR-25). A TLS callback reads the thread the loader told "the process is attaching", and a
//! C runtime initializer, which the entry point runs before `main`, reads the thread that runs the program.
//! When the main thread started the process, the two are one thread. When another thread ran first - a
//! remote thread started in a process created suspended - the loader started the process on that one,
//! whether it is still alive by the time the program asks or not.
//!
//! Included by each test file that needs it, so each test binary carries its own callback.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::System::Threading::GetCurrentThreadId;

/// What a process saw of its own start, which the probes end with.
pub(crate) const STARTED_BY_ITS_OWN_THREAD: u32 = 0;
pub(crate) const STARTED_BY_ANOTHER_THREAD: u32 = 3;
pub(crate) const START_NOT_SEEN: u32 = 4;

/// What the loader passes a TLS callback when the process attaches (MS Learn, the PE format, TLS callback
/// functions).
const DLL_PROCESS_ATTACH: u32 = 1;

/// The thread the loader told the process is attaching, and the thread that runs the program.
static STARTED_BY: AtomicU32 = AtomicU32::new(0);
static ENTERED_BY: AtomicU32 = AtomicU32::new(0);

/// The program's own TLS callback, run by the loader in every process of the binary.
unsafe extern "system" fn on_tls_event(_module: *mut c_void, reason: u32, _reserved: *mut c_void) {
    if reason == DLL_PROCESS_ATTACH {
        // SAFETY: a call that reads the calling thread's id, nothing else.
        STARTED_BY.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    }
}

/// The callback, where the C runtime's TLS directory collects the program's callbacks (`.CRT$XLA` to
/// `.CRT$XLZ`, the standard library's own sits at `.CRT$XLB`).
#[used]
#[unsafe(link_section = ".CRT$XLM")]
static TLS_EVENT: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) = on_tls_event;

/// Run by the C runtime's start-up on the thread that entered the program, before `main`.
extern "C" fn on_program_start() {
    // SAFETY: a call that reads the calling thread's id, nothing else.
    ENTERED_BY.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
}

/// The initializer, where the C runtime collects the ones its start-up runs (`.CRT$XCA` to `.CRT$XCZ`, the
/// ones of a program at `.CRT$XCU`).
#[used]
#[unsafe(link_section = ".CRT$XCU")]
static PROGRAM_START: extern "C" fn() = on_program_start;

/// What this process saw of its own start. Two threads alive at once never share an id, and the main
/// thread exists from the moment the process is created, so a thread that started the process and ended
/// since cannot have left its id to the main thread.
pub(crate) fn own_start() -> u32 {
    match (STARTED_BY.load(Ordering::SeqCst), ENTERED_BY.load(Ordering::SeqCst)) {
        (0, _) | (_, 0) => START_NOT_SEEN,
        (started, entered) if started == entered => STARTED_BY_ITS_OWN_THREAD,
        _ => STARTED_BY_ANOTHER_THREAD,
    }
}
