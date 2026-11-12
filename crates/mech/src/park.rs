//! The target's own main thread starts the process, and waits before the program's entry point while the
//! hook is loaded (ADR-25).
//!
//! The target is created suspended. A remote thread started in it then is the first thread to run, and
//! the first thread of a process does the process's start: the loader maps the static imports, runs their
//! start-up code and the program's TLS callbacks, told the process is attaching. A runtime that takes that
//! call for the main thread sets its state up on a thread that ends a moment later - measured with a Free
//! Pascal application, which crashed in a lock its worker thread entered, never initialised, with the
//! remote thread loading nothing but a library already there.
//!
//! So the main thread goes first. Before it is resumed, a queue of asynchronous calls is put on it, which
//! the loader delivers when the process's start is done and before the entry point - the moment early
//! injection into a new process relies on, measured here: the main thread signals an event and waits on a
//! second one, and closes its handles to both once it goes on. The remote thread that loads the hook then
//! finds a process already started and only loads the hook, and the program starts when the second event
//! is set. Events rather than `SuspendThread`: a call queued and never waited for - a start that took
//! longer than the limit - passes through once it is delivered, where a suspension would hold the
//! application for good.
//!
//! The calls go straight through `NtQueueApcThread`, each routine called with the three arguments the
//! dispatcher passes, the ones it does not take null. Not `QueueUserAPC`: it wraps the call in
//! `RtlDispatchAPC` with the caller's active activation context, a pointer that means nothing in another
//! process (disassembled, KernelBase 10.0.26300), and `RtlDispatchAPC` is not exported by name on x64. On
//! x64 a routine ignores the arguments it does not take. On x86 `NtWaitForSingleObject` takes exactly three
//! (`ret 0Ch`), and `SetEvent` and `CloseHandle` one: the dispatcher keeps the context it goes on from in a
//! register the routine preserves and goes on through `NtContinue`, which sets the stack pointer back, so
//! what a routine leaves on the stack does not matter (`KiUserApcDispatcher` of the 32-bit ntdll,
//! disassembled). Every address is this process's, valid in a target of the same bitness, where the system
//! libraries sit at the same addresses - the assumption the remote `LoadLibraryW` start has always made.

use std::ffi::c_void;
use std::mem::ManuallyDrop;

use windows::core::{s, PCSTR};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetExitCodeProcess, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

pub(crate) use chrono_ctl::inject::{park_outcome, Park, ParkWait, ProcessAfter};

/// `NtQueueApcThread` as ntdll exports it: the thread, the routine, and the three arguments the dispatcher
/// passes to the routine.
type NtQueueApcThreadFn =
    unsafe extern "system" fn(HANDLE, *const c_void, *const c_void, *const c_void, *const c_void) -> i32;

/// The functions the queued calls run, at their addresses in this process.
struct Calls {
    queue: NtQueueApcThreadFn,
    wait: *const c_void,
    set_event: *const c_void,
    close_handle: *const c_void,
}

impl Calls {
    /// `None` when one of them is not exported - not on any Windows this runs on, and then the target is
    /// refused rather than started the old way.
    fn resolve() -> Option<Self> {
        // SAFETY: two modules every process has loaded, asked by name for exports whose signatures are the
        // documented ones - `NtQueueApcThread` is the one the address is cast to, the others stay addresses.
        unsafe {
            let ntdll = GetModuleHandleA(s!("ntdll.dll")).ok()?;
            let kernel32 = GetModuleHandleA(s!("kernel32.dll")).ok()?;
            let entry = |module, name: PCSTR| GetProcAddress(module, name).map(|f| f as *const c_void);
            Some(Calls {
                queue: std::mem::transmute::<*const c_void, NtQueueApcThreadFn>(entry(ntdll, s!("NtQueueApcThread"))?),
                wait: entry(ntdll, s!("NtWaitForSingleObject"))?,
                set_event: entry(kernel32, s!("SetEvent"))?,
                close_handle: entry(kernel32, s!("CloseHandle"))?,
            })
        }
    }

    /// Queue `routine(first, second, third)` on `thread`.
    ///
    /// # Safety
    /// `thread` must be a thread handle, and `routine` one of these calls, valid in the thread's process
    /// (one of this process's bitness), with arguments that stay valid there until the call is delivered.
    unsafe fn put(&self, thread: HANDLE, routine: *const c_void, args: [*const c_void; 3]) -> Result<(), String> { unsafe {
        let status = (self.queue)(thread, routine, args[0], args[1], args[2]);
        if status >= 0 { Ok(()) } else { Err(format!("NtQueueApcThread failed (status 0x{:08X})", status as u32)) }
    }}

    /// Queue a call of `function(argument)`, a function of one argument.
    ///
    /// # Safety
    /// As `put`, with `argument` a handle of the thread's process.
    unsafe fn put_one(&self, thread: HANDLE, function: *const c_void, argument: HANDLE) -> Result<(), String> { unsafe {
        self.put(thread, function, [argument.0, std::ptr::null(), std::ptr::null()])
    }}
}

/// The two events of one park, this process's handles to them. Dropped, the second one is set, so the
/// main thread goes on whenever its calls are delivered, and both handles are closed.
pub(crate) struct Parking {
    parked: HANDLE,
    go: HANDLE,
}

impl Parking {
    /// Put the park on `thread`, the main thread of `process`, both created suspended and not resumed
    /// yet: the main thread signals `parked`, waits for `go`, then closes the two handles of its own. The
    /// thread is left suspended - the caller resumes it.
    ///
    /// # Safety
    /// `process` must be a process handle with handle-duplication access, of this process's bitness, and
    /// `thread` its main thread, with the access to queue a call on it, never resumed.
    pub(crate) unsafe fn put_on(process: HANDLE, thread: HANDLE) -> Result<Self, String> { unsafe {
        let calls = Calls::resolve().ok_or("the system does not export the calls a target's start is held with")?;
        let parked = CreateEventW(None, true, false, None).map_err(|e| crate::win32_detail("CreateEventW", &e))?;
        let go = match CreateEventW(None, true, false, None) {
            Ok(go) => go,
            Err(e) => {
                let _ = CloseHandle(parked);
                return Err(crate::win32_detail("CreateEventW", &e));
            }
        };
        let parking = Parking { parked, go };
        let parked_there = into(process, parked)?;
        let go_there = match into(process, go) {
            Ok(h) => h,
            Err(e) => {
                close_in(process, parked_there);
                return Err(e);
            }
        };
        // SAFETY of the four calls: the routines are this process's addresses of ntdll and kernel32, which a
        // target of the same bitness has at the same addresses (the module's notes). Each handle they take is
        // the target's own and stays open there until the last two calls close it, after the wait - nothing
        // else in the target knows it, so nothing closes it sooner.
        // Nothing is queued until the first call is, so a refusal of that one leaves nothing in the target.
        if let Err(e) = calls.put_one(thread, calls.set_event, parked_there) {
            close_in(process, parked_there);
            close_in(process, go_there);
            return Err(e);
        }
        // From here a call already queued may use either handle, so they stay in the target on a refusal:
        // two handles left in a process that is ended on the way out, against a call on a handle reused.
        calls.put(thread, calls.wait, [go_there.0, std::ptr::null(), std::ptr::null()])?;
        calls.put_one(thread, calls.close_handle, parked_there)?;
        calls.put_one(thread, calls.close_handle, go_there)?;
        Ok(parking)
    }}

    /// Wait until the main thread has parked, the process has ended, or `limit_ms` has passed, and read
    /// which, the process first (`park_outcome`).
    ///
    /// # Safety
    /// `process` must be the process the park was put in, still open.
    pub(crate) unsafe fn wait(&self, process: HANDLE, limit_ms: u32) -> Park { unsafe {
        let woke = WaitForMultipleObjects(&[self.parked, process], false, limit_ms);
        let wait = if woke == WAIT_OBJECT_0 {
            ParkWait::Parked
        } else if woke.0 == WAIT_OBJECT_0.0 + 1 {
            ParkWait::ProcessSignalled
        } else if woke == WAIT_TIMEOUT {
            ParkWait::TimedOut
        } else {
            ParkWait::Failed
        };
        // Signalled first, then the code: read the other way round, a process that ends between the two
        // reads would be a signalled process with the code of a live one.
        let signalled = WaitForSingleObject(process, 0) == WAIT_OBJECT_0;
        let mut code = 0u32;
        let process = ProcessAfter { signalled, code: GetExitCodeProcess(process, &mut code).is_ok().then_some(code) };
        park_outcome(wait, process)
    }}

    /// Let the main thread go on to the program's entry point. An `Err` leaves it parked, and the caller
    /// ends the target rather than hand back a session whose application never starts.
    pub(crate) fn release(self) -> Result<(), String> {
        let held = ManuallyDrop::new(self);
        // SAFETY: both handles are this value's own, and `held` is never dropped, so each is closed once.
        unsafe {
            let set = SetEvent(held.go);
            let _ = CloseHandle(held.parked);
            let _ = CloseHandle(held.go);
            set.map_err(|e| crate::win32_detail("SetEvent", &e))
        }
    }
}

impl Drop for Parking {
    fn drop(&mut self) {
        // SAFETY: both handles are this value's own and closed once, here.
        unsafe {
            let _ = SetEvent(self.go);
            let _ = CloseHandle(self.parked);
            let _ = CloseHandle(self.go);
        }
    }
}

/// A handle in `process` to the object `own` names here.
///
/// # Safety
/// `own` must be an open handle of this process, and `process` a process handle with handle-duplication
/// access.
unsafe fn into(process: HANDLE, own: HANDLE) -> Result<HANDLE, String> { unsafe {
    let mut there = HANDLE::default();
    DuplicateHandle(GetCurrentProcess(), own, process, &mut there, 0, false, DUPLICATE_SAME_ACCESS)
        .map(|()| there)
        .map_err(|e| crate::win32_detail("DuplicateHandle", &e))
}}

/// Close a handle of `process`'s own, which nothing there uses. With no target process the source is
/// closed and nothing is duplicated (MS Learn, `DUPLICATE_CLOSE_SOURCE`, which closes the source whatever
/// the call returns).
///
/// # Safety
/// `process` must be a process handle with handle-duplication access, and `there` a handle of its own
/// that nothing in it uses or will use.
unsafe fn close_in(process: HANDLE, there: HANDLE) { unsafe {
    let _ = DuplicateHandle(process, there, HANDLE::default(), std::ptr::null_mut(), 0, false, DUPLICATE_CLOSE_SOURCE);
}}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::{
        GetCurrentThreadId, GetProcessHandleCount, OpenThread, THREAD_QUERY_LIMITED_INFORMATION,
    };

    /// A park whose first call the thread refuses leaves no handle open, here or in the target: the two
    /// events, and the two handles to them put in the target, are all closed. The target is this process,
    /// so a handle left in it shows in its own count, and the thread a handle to this thread without the
    /// access to queue a call on it.
    #[test]
    fn a_park_whose_first_call_is_refused_leaves_no_handle_open() {
        const TRIES: u32 = 256;
        // SAFETY: the pseudo-handle of this process, and a handle to this thread that `put_on` cannot queue
        // a call on, so nothing is ever queued on it. Every handle that comes back is closed here.
        unsafe {
            let count = || {
                let mut n = 0u32;
                GetProcessHandleCount(GetCurrentProcess(), &mut n).expect("this process can count its handles");
                n
            };
            let thread = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, GetCurrentThreadId())
                .expect("a handle to this thread");
            let before = count();
            for _ in 0..TRIES {
                assert!(
                    Parking::put_on(GetCurrentProcess(), thread).is_err(),
                    "a call was queued on a thread handle without the access for it"
                );
            }
            // Other tests open and close handles meanwhile, so the bound is a quarter of what a leak would add.
            let grew = count().saturating_sub(before);
            let _ = CloseHandle(thread);
            assert!(grew < TRIES / 2, "{grew} handles stayed open after {TRIES} refused parks");
        }
    }
}
