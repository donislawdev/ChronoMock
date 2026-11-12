//! What Windows's loader did to the target while the hook was being injected (R4-S6, R4-N10).
//!
//! The target is created suspended, and its own main thread does the process's loading before the hook
//! comes in (ADR-25, `crate::park`): the static imports and their start-up code, then, with the thread
//! parked before the entry point, `LoadLibraryW` of the hook on a remote thread. A target whose static
//! import is missing, built for the other bitness, lacking a function or refusing to initialise ends during
//! that start, with the loader's status as its exit code. That used to read as a successful injection
//! followed by a target that vanished (exit 12, "suspected single-instance app"), or, when Windows showed
//! its error window and waited for an answer, as a loader lock ten seconds later. Both were measured on x64
//! and x86 (`tools/probes/r4-9`, when the remote thread did the loading), and neither was true.
//!
//! Two things fix it. The end of each wait is read in the order that tells the truth (`outcome` and
//! `park_outcome`, shared with the hook in `chrono_ctl::inject` since R5-A7): the target's own exit code
//! first. And Windows's error window is switched off for the target while it starts and loads and switched
//! back before its entry point ([`QuietLoader`]), so a failed start ends in milliseconds with its status
//! instead of waiting on a window nobody may be looking at, and no window outlives the start that raised
//! it.

use std::ffi::c_void;

use windows::core::s;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};

pub(crate) use chrono_ctl::inject::{outcome, page_released_safely, Outcome, ProcessAfter, Wait};

/// `ProcessDefaultHardErrorMode`: the information class that reads and sets whether Windows shows its
/// error window for a process's hard errors - what `SetErrorMode` changes for the calling process. Bit 0
/// set means the window is shown, the inverse of `SEM_FAILCRITICALERRORS`. Not on the documented list for
/// `NtQueryInformationProcess` or `NtSetInformationProcess`: the phnt / ReactOS number, an assessment
/// rather than a source (zasady/03 section 4), measured on Windows 11 for x64 and x86 targets from both
/// cores (`tools/probes/r4-9`). A wrong number is a status the kernel returns, never a fault, and reads
/// here as "leave the window as it is".
const PROCESS_DEFAULT_HARD_ERROR_MODE: u32 = 12;

/// The bit of that value that shows the window.
const SHOWS_THE_WINDOW: u32 = 1;

type NtSetInformationProcessFn = unsafe extern "system" fn(HANDLE, u32, *const c_void, u32) -> i32;

/// Windows's error window, switched off for a target while it loads (R4-S6).
///
/// A target inherits the error mode of the process that started it (MS Learn, "Inheritance" and
/// `SetErrorMode`), and a program started from Explorer - the window, in the ordinary case - has the
/// default, which shows the window. The loader then raises it for a missing library and waits for an
/// answer, so the remote thread stood until the ten-second limit and the session reported a loader lock,
/// with the window still on the desktop after the start was over. Off, the loader ends the process at
/// once with the status the window would have shown.
///
/// The mode goes back before the target's entry point, so the application runs with the mode it
/// inherited - measured with a program that reports its own mode. What runs under the switch is the
/// loading itself: the start-up code of the libraries it imports and its TLS callbacks (R4-N20).
pub(crate) struct QuietLoader {
    process: HANDLE,
    inherited: u32,
    quiet: u32,
}

impl QuietLoader {
    /// Switch the window off for `process`, which must not have run an instruction yet. `None` when it
    /// was off already, so there is nothing to give back, or when the system would not read or set the
    /// mode - the start then goes on exactly as it did before this existed, window and all.
    ///
    /// # Safety
    /// `process` must be an open process handle with query and set-information access.
    pub(crate) unsafe fn begin(process: HANDLE) -> Option<Self> { unsafe {
        let inherited = query_mode(process)?;
        if inherited & SHOWS_THE_WINDOW == 0 {
            return None;
        }
        let quiet = inherited & !SHOWS_THE_WINDOW;
        set_mode(process, quiet).ok()?;
        Some(Self { process, inherited, quiet })
    }}

    /// Give the target back the mode it inherited. Left alone when the value is no longer the one set
    /// here: a library the target loaded chose its own mode in its start-up code, and that choice belongs
    /// to the target (measured with a library that sets 0x8001). One choice cannot be told from this
    /// switch and is undone with it: a library that set exactly the value set here, which for a target
    /// that inherited the default is `SEM_FAILCRITICALERRORS` and nothing else. An `Err` means the
    /// application would run without the window it inherited, which the caller refuses rather than leave
    /// the change in place unsaid.
    ///
    /// # Safety
    /// The handle given to `begin` must still be open.
    pub(crate) unsafe fn end(self) -> Result<(), String> { unsafe {
        match query_mode(self.process) {
            Some(now) if now != self.quiet => Ok(()),
            _ => set_mode(self.process, self.inherited).map_err(|status| {
                format!(
                    "could not give the target back the error mode it inherited (status 0x{status:08X}), so it was not started"
                )
            }),
        }
    }}
}

/// The process's hard-error mode, or `None` when the system will not say.
///
/// # Safety
/// `process` must be an open process handle with query access.
unsafe fn query_mode(process: HANDLE) -> Option<u32> { unsafe {
    let ntdll = GetModuleHandleA(s!("ntdll.dll")).ok()?;
    let entry = GetProcAddress(ntdll, s!("NtQueryInformationProcess"))?;
    let query: crate::NtQueryInformationProcessFn = std::mem::transmute(entry);
    let mut mode: u32 = 0;
    let status = query(
        process,
        PROCESS_DEFAULT_HARD_ERROR_MODE,
        (&raw mut mode).cast(),
        size_of::<u32>() as u32,
        std::ptr::null_mut(),
    );
    (status >= 0).then_some(mode)
}}

/// Set the process's hard-error mode. `Err` carries the status, or `u32::MAX` when the entry point is not
/// there to call.
///
/// # Safety
/// `process` must be an open process handle with set-information access.
unsafe fn set_mode(process: HANDLE, mode: u32) -> Result<(), u32> { unsafe {
    let Ok(ntdll) = GetModuleHandleA(s!("ntdll.dll")) else {
        return Err(u32::MAX);
    };
    let Some(entry) = GetProcAddress(ntdll, s!("NtSetInformationProcess")) else {
        return Err(u32::MAX);
    };
    let set: NtSetInformationProcessFn = std::mem::transmute(entry);
    let status = set(process, PROCESS_DEFAULT_HARD_ERROR_MODE, (&raw const mode).cast(), size_of::<u32>() as u32);
    if status >= 0 { Ok(()) } else { Err(status as u32) }
}}
