//! How a remote load of the hook ended, read one way by both sides that inject it (R5-A7).
//!
//! The mechanism loads the hook into the target it launches, and the hook loads itself into each child
//! that target starts (ADR-3). Both write the library's path into the process, run `LoadLibraryW` there
//! on a remote thread and wait for it, and both have to answer the same three questions afterwards: did
//! the library load, did the process end while it was being loaded, and may the page holding the path
//! be freed. They used to answer them in two copies that had drifted apart. The mechanism read the
//! target's own exit code first (R4-S6) and would not take a loader status for a module, while the hook
//! took any non-zero thread code for a loaded library - so a child its loader ended, a missing import
//! say, counted as a process that ran on the real clock although it never ran an instruction of its
//! own. The reading lives here once, with no Windows types, so both sides and their tests share it.
//!
//! The process is created suspended, and its own main thread starts it before the hook comes in (ADR-25).
//! The first thread of a process does that process's own loading before its start routine: the static
//! imports and their start-up code, and the program's TLS callbacks told the process is attaching. When
//! the remote thread was that first thread, a program whose runtime takes "the process is attaching" to
//! mean "this is the main thread" set its state up on a thread that ended a moment later, and crashed
//! (measured with a Free Pascal application: an access violation in a lock its worker thread entered,
//! never initialised). So the main thread is resumed first with a queue of asynchronous calls that the
//! loader delivers when the process's start is done and before the program's entry point: they signal
//! that the thread is there and hold it on an event (a park, [`park_outcome`]). The remote thread then
//! only loads the hook. A process whose static import is missing, built for the other bitness, lacking
//! a function or refusing to initialise ends during that start, with the loader's status as its exit
//! code (measured on x64 and x86, `tools/probes/r4-9`), and the park reads it the way [`outcome`] does.

/// `STILL_ACTIVE` (259): the exit code Windows reports for a thread or a process that has not finished.
/// Read as "not known yet", never as a loaded module.
pub const STILL_ACTIVE_CODE: u32 = 259;

/// How long either side waits for the main thread's park, and then for the remote `LoadLibraryW` thread
/// (ADR-25). The hook's `DllMain` does no heavy work (it defers the watcher off the loader lock, ADR-3),
/// so a load is quick - measured at about 68 ms for a child, start included - and a start or a load still
/// running past this means the process's loader is stuck (a library's start-up code waiting, or a loader
/// lock). Finite, so neither the core's start nor the parent's `CreateProcess` can hang for good (M-1,
/// RELEASE-009).
pub const INJECT_TIMEOUT_MS: u32 = 10_000;

/// How the wait for the remote `LoadLibraryW` thread ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// The thread finished - on its own, or because the process ended and took it along.
    ThreadDone,
    /// It did not, within the time allowed.
    TimedOut,
    /// The wait itself failed, so nothing is known about either.
    Failed,
}

/// The process as it stood right after that wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessAfter {
    /// Whether the process object was signalled, which is what says it has ended.
    pub signalled: bool,
    /// Its exit code as `GetExitCodeProcess` read it, `None` when the read failed.
    pub code: Option<u32>,
}

/// What the injection came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// `LoadLibraryW` returned a module.
    Loaded,
    /// `LoadLibraryW` returned NULL in a process that is still there.
    LibraryNotLoaded,
    /// The process ended while it was being loaded, with this exit code.
    TargetEnded(u32),
    /// The process ended and its exit code could not be read.
    TargetEndedUnread,
    /// The thread did not finish in time.
    TimedOut,
    /// The wait failed.
    WaitFailed,
}

/// Read the end of the injection wait.
///
/// The process's exit code comes first. Measured: when the loader ends a process, the remote thread is
/// signalled and the process's exit code already holds the loader's status, while the process object is
/// not signalled yet - so "is the process signalled" alone misses the very case this exists for, and the
/// thread's exit code (the same status) would read as a loaded module. A code other than `STILL_ACTIVE`
/// is an ending whatever the object says. A signalled object is an ending whatever the code is, because a
/// process may really end with 259 (R4-N19).
pub fn outcome(wait: Wait, process: ProcessAfter, thread_code: Option<u32>) -> Outcome {
    match (process.signalled, process.code) {
        (true, Some(code)) => return Outcome::TargetEnded(code),
        (true, None) => return Outcome::TargetEndedUnread,
        (false, Some(code)) if code != STILL_ACTIVE_CODE => return Outcome::TargetEnded(code),
        _ => {}
    }
    match wait {
        // The thread's exit code is the low 32 bits of the module `LoadLibraryW` returned, and a module
        // is aligned to 64 KiB. So a code with any of its low 16 bits set is not a module - a loader
        // status such as 0xC0000135 always has some - and 0 is the NULL of a library that did not load.
        Wait::ThreadDone => match thread_code {
            Some(code) if is_module(code) => Outcome::Loaded,
            _ => Outcome::LibraryNotLoaded,
        },
        Wait::TimedOut => Outcome::TimedOut,
        Wait::Failed => Outcome::WaitFailed,
    }
}

/// Whether a remote thread's exit code can be the low half of a loaded module: not zero, and aligned to
/// the 64 KiB a module is mapped at. On x64 a module based exactly on a 4 GiB boundary reads as zero
/// too - astronomically rare, and refused in the safe direction, since a retry lands elsewhere.
fn is_module(thread_code: u32) -> bool {
    thread_code != 0 && thread_code & 0xFFFF == 0
}

/// Whether the remote page holding the library's path can be freed: only once nothing will read it
/// again (R4-N10). The thread that read it has finished, or the process that owns it has ended. A thread
/// still running may be inside `LoadLibraryW` with the path in hand, and freeing the page under it is a
/// use-after-free inside somebody else's application.
pub fn page_released_safely(outcome: Outcome) -> bool {
    matches!(
        outcome,
        Outcome::Loaded | Outcome::LibraryNotLoaded | Outcome::TargetEnded(_) | Outcome::TargetEndedUnread
    )
}

/// How the wait for the main thread's park ended (ADR-25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkWait {
    /// The event the parking call signals was signalled.
    Parked,
    /// The process object was signalled first.
    ProcessSignalled,
    /// Neither, within the time allowed.
    TimedOut,
    /// The wait itself failed, so nothing is known about either.
    Failed,
}

/// What the start of the process came to before the hook was loaded (ADR-25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Park {
    /// The main thread started the process and waits before the program's entry point.
    Parked,
    /// The process ended during its start, with this exit code - the loader's status for a missing or
    /// refusing import.
    TargetEnded(u32),
    /// The process ended during its start and its exit code could not be read.
    TargetEndedUnread,
    /// The start did not finish within the time allowed - a library's start-up code waiting.
    TimedOut,
    /// The wait failed.
    WaitFailed,
}

impl Park {
    /// What the injection came to when the start did not park, in the terms a load is counted in - a child
    /// that ended during its start never ran, one whose start ran out ran uncovered. `None` when the main
    /// thread parked, and the load that follows decides.
    pub fn unloaded(self) -> Option<Outcome> {
        match self {
            Park::Parked => None,
            Park::TargetEnded(code) => Some(Outcome::TargetEnded(code)),
            Park::TargetEndedUnread => Some(Outcome::TargetEndedUnread),
            Park::TimedOut => Some(Outcome::TimedOut),
            Park::WaitFailed => Some(Outcome::WaitFailed),
        }
    }
}

/// Read the end of the wait for the park, the process first, as [`outcome`] reads the end of a load: the
/// loader writes its status into the process's exit code before the process object is signalled, and a
/// process that has ended has not parked whatever the event says.
pub fn park_outcome(wait: ParkWait, process: ProcessAfter) -> Park {
    match (process.signalled, process.code) {
        (true, Some(code)) => return Park::TargetEnded(code),
        (true, None) => return Park::TargetEndedUnread,
        (false, Some(code)) if code != STILL_ACTIVE_CODE => return Park::TargetEnded(code),
        _ => {}
    }
    match wait {
        ParkWait::Parked => Park::Parked,
        // The process object said it was signalled and the read after it says it is not: the ending is
        // what was seen, and the code was not.
        ParkWait::ProcessSignalled => Park::TargetEndedUnread,
        ParkWait::TimedOut => Park::TimedOut,
        ParkWait::Failed => Park::WaitFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: ProcessAfter = ProcessAfter { signalled: false, code: Some(STILL_ACTIVE_CODE) };

    /// The case the process-first reading exists for, as measured: the thread is done with the loader's
    /// status as its code, the process code holds the same status, and the process object is not
    /// signalled yet. Read the old way it was a loaded module (a non-zero thread code).
    #[test]
    fn a_process_the_loader_ends_is_named_by_its_status_before_its_object_is_signalled() {
        let ending = ProcessAfter { signalled: false, code: Some(0xC000_0135) };
        assert_eq!(outcome(Wait::ThreadDone, ending, Some(0xC000_0135)), Outcome::TargetEnded(0xC000_0135));
        let ended = ProcessAfter { signalled: true, code: Some(0xC000_0142) };
        assert_eq!(outcome(Wait::ThreadDone, ended, Some(0xC000_0142)), Outcome::TargetEnded(0xC000_0142));
        // An ending outranks whatever the wait said about the thread.
        assert_eq!(outcome(Wait::TimedOut, ending, None), Outcome::TargetEnded(0xC000_0135));
        assert_eq!(outcome(Wait::Failed, ended, None), Outcome::TargetEnded(0xC000_0142));
    }

    /// A signalled process has ended even when its code is 259 (R4-N19), and a process whose code cannot
    /// be read is not taken for a loaded one.
    #[test]
    fn a_signalled_process_has_ended_whatever_its_code_says() {
        let ended_259 = ProcessAfter { signalled: true, code: Some(STILL_ACTIVE_CODE) };
        assert_eq!(outcome(Wait::ThreadDone, ended_259, Some(0x7FF0_0000)), Outcome::TargetEnded(STILL_ACTIVE_CODE));
        let unread = ProcessAfter { signalled: true, code: None };
        assert_eq!(outcome(Wait::ThreadDone, unread, Some(0x7FF0_0000)), Outcome::TargetEndedUnread);
    }

    /// A live process: the thread's code decides, and only a finished thread's code counts.
    #[test]
    fn a_live_process_is_read_from_its_finished_thread() {
        assert_eq!(outcome(Wait::ThreadDone, LIVE, Some(0x7FF0_0000)), Outcome::Loaded);
        assert_eq!(outcome(Wait::ThreadDone, LIVE, Some(0)), Outcome::LibraryNotLoaded);
        assert_eq!(outcome(Wait::ThreadDone, LIVE, None), Outcome::LibraryNotLoaded);
        let unreadable = ProcessAfter { signalled: false, code: None };
        assert_eq!(outcome(Wait::ThreadDone, unreadable, Some(0x7FF0_0000)), Outcome::Loaded);
        // A thread that ended with a status while its process lives on did not load anything.
        assert_eq!(outcome(Wait::ThreadDone, LIVE, Some(0xC000_0135)), Outcome::LibraryNotLoaded);
        assert_eq!(outcome(Wait::ThreadDone, LIVE, Some(STILL_ACTIVE_CODE)), Outcome::LibraryNotLoaded);
    }

    /// A wait that failed or timed out says nothing about the thread (R4-N10): a failed wait used to
    /// read the thread's code, 259 for a thread still running, and took it for a module.
    #[test]
    fn a_wait_that_failed_or_ran_out_is_never_a_loaded_module() {
        assert_eq!(outcome(Wait::TimedOut, LIVE, Some(STILL_ACTIVE_CODE)), Outcome::TimedOut);
        assert_eq!(outcome(Wait::Failed, LIVE, None), Outcome::WaitFailed);
    }

    /// A process the loader ends during its start never parks, and its ending is read before the event:
    /// the status first, as it stands before the object is signalled, then a signalled object.
    #[test]
    fn a_process_that_ends_during_its_start_is_named_by_its_status() {
        let ending = ProcessAfter { signalled: false, code: Some(0xC000_0135) };
        assert_eq!(park_outcome(ParkWait::TimedOut, ending), Park::TargetEnded(0xC000_0135));
        assert_eq!(park_outcome(ParkWait::Parked, ending), Park::TargetEnded(0xC000_0135));
        let ended = ProcessAfter { signalled: true, code: Some(0xC000_0142) };
        assert_eq!(park_outcome(ParkWait::ProcessSignalled, ended), Park::TargetEnded(0xC000_0142));
        let ended_259 = ProcessAfter { signalled: true, code: Some(STILL_ACTIVE_CODE) };
        assert_eq!(park_outcome(ParkWait::Parked, ended_259), Park::TargetEnded(STILL_ACTIVE_CODE));
        let unread = ProcessAfter { signalled: true, code: None };
        assert_eq!(park_outcome(ParkWait::ProcessSignalled, unread), Park::TargetEndedUnread);
        // The wait saw the process end, and the read after it did not: still an ending, never a park.
        assert_eq!(park_outcome(ParkWait::ProcessSignalled, LIVE), Park::TargetEndedUnread);
    }

    /// A live process parked only when the event said so, and a wait that ran out or failed is neither a
    /// park nor an ending.
    #[test]
    fn a_live_process_has_parked_only_when_its_event_says_so() {
        assert_eq!(park_outcome(ParkWait::Parked, LIVE), Park::Parked);
        let unreadable = ProcessAfter { signalled: false, code: None };
        assert_eq!(park_outcome(ParkWait::Parked, unreadable), Park::Parked);
        assert_eq!(park_outcome(ParkWait::TimedOut, LIVE), Park::TimedOut);
        assert_eq!(park_outcome(ParkWait::Failed, LIVE), Park::WaitFailed);
    }

    /// A start that did not park is counted the way a load that ended the same way is: an ending as an
    /// ending, never a module, and a park leaves the count to the load.
    #[test]
    fn a_start_that_did_not_park_is_counted_like_a_load_that_ended_that_way() {
        assert_eq!(Park::Parked.unloaded(), None);
        assert_eq!(Park::TargetEnded(0xC000_0135).unloaded(), Some(Outcome::TargetEnded(0xC000_0135)));
        assert_eq!(Park::TargetEndedUnread.unloaded(), Some(Outcome::TargetEndedUnread));
        assert_eq!(Park::TimedOut.unloaded(), Some(Outcome::TimedOut));
        assert_eq!(Park::WaitFailed.unloaded(), Some(Outcome::WaitFailed));
    }

    /// The remote page goes only when nothing will read it again (R4-N10).
    #[test]
    fn the_remote_page_is_freed_only_once_nothing_reads_it() {
        assert!(page_released_safely(Outcome::Loaded));
        assert!(page_released_safely(Outcome::LibraryNotLoaded));
        assert!(page_released_safely(Outcome::TargetEnded(0xC000_0135)));
        assert!(page_released_safely(Outcome::TargetEndedUnread));
        assert!(!page_released_safely(Outcome::TimedOut));
        assert!(!page_released_safely(Outcome::WaitFailed));
    }
}
