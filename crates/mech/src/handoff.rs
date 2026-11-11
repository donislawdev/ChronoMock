//! What a hand-off to a copy that is already running looks like from outside the application (ADR-4,
//! 2026-11-11).
//!
//! A single-instance application started a second time passes its work to the copy that runs already
//! and ends. Inside the guard window that is caught at the start of the session (exit code 12). A
//! second instance that takes longer - measured 2 s for a probe and 2.6 s for a real editor - ended
//! with the session reporting `works`, while the work it handed on ran in the copy, on the real clock.
//!
//! No process says it handed something on, so the session reads the situation instead: the family
//! ended on its own, not one of its processes ever showed a window, and outside the family a process
//! runs the same executable file as a windowed member of it. Each of those alone is ordinary. The three
//! together are what a hand-off looks like, and the session says the work most likely went to that copy.
//! A window the tester saw is the line between the two: an application that was used and closed while
//! another copy of it runs is not a hand-off.
//!
//! Both questions are about the session's own processes, and a pid alone does not say whose a process
//! is: the system hands a pid on once its process is gone. A process counts as the session's when the
//! session holds a handle on it, which keeps its pid from being handed on, or when it is a child the
//! hook never entered that still runs the executable it was named with.

use std::collections::HashSet;

use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible};

use crate::tree::{process_entries, ProcessEntry};
use crate::UncoveredChild;

/// A process outside the family that runs the same executable file as a windowed member of it: its pid,
/// and the file name of the executable as the process list gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningCopy {
    pub pid: u32,
    pub image: String,
}

/// The owners of the visible top-level windows on this desktop right now, each once.
///
/// A window that is not visible does not count: every program with a message loop owns a few hidden
/// ones (input method, clipboard, a hidden main window before it is shown), and none of them is a
/// window the tester could have used. A window on another virtual desktop is visible, and counts.
pub(crate) fn visible_window_owners() -> HashSet<u32> {
    let mut owners = HashSet::new();
    // SAFETY: the callback runs on this thread before `EnumWindows` returns, and the pointer it gets is
    // to `owners`, which lives across the call. The call fails only when a callback stops the walk,
    // which this one never does.
    unsafe {
        let _ = EnumWindows(Some(note_owner), LPARAM(&raw mut owners as isize));
    }
    owners
}

/// One top-level window of the walk: note its owner when the window is visible.
///
/// # Safety
/// `lparam` is the address of the set `visible_window_owners` passed, alive for the whole walk.
unsafe extern "system" fn note_owner(window: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: see the function's contract. Nothing else holds the set while the walk runs.
    let owners = unsafe { &mut *(lparam.0 as *mut HashSet<u32>) };
    // SAFETY: both calls only read about the window, and a window that closed meanwhile answers no.
    if unsafe { IsWindowVisible(window) }.as_bool() {
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(window, Some(&raw mut owner)) };
        if owner != 0 {
            owners.insert(owner);
        }
    }
    BOOL(1)
}

/// Whether one of `owners`, the owners of the visible windows, is a process of the session: one in
/// `held`, the pids the session holds a handle on, or a child the hook never entered (`unhooked`) that is
/// still the one it was named for. Only a child that owns a window is asked who it is, so a family of
/// many children costs a question per window owner, not one per child.
pub(crate) fn owned_by_member(
    owners: &HashSet<u32>,
    held: &HashSet<u32>,
    unhooked: &[UncoveredChild],
    image_now: impl Fn(u32) -> Option<String>,
) -> bool {
    owners.iter().any(|pid| held.contains(pid))
        || unhooked.iter().any(|child| owners.contains(&child.pid) && still_the_child(child, &image_now))
}

/// Whether `child`, a process the hook never entered, is still the one it was named for: its executable
/// has the file name now that it had then. A child named without a file name, or one that will not say
/// its own now - gone, or not this user's to ask - cannot be told from a stranger holding its pid, and
/// is not counted as the session's.
pub(crate) fn still_the_child(child: &UncoveredChild, image_now: &impl Fn(u32) -> Option<String>) -> bool {
    match (&child.image, image_now(child.pid)) {
        (Some(then), Some(now)) => fold_case(file_name(&now)) == fold_case(file_name(then)),
        _ => false,
    }
}

/// The processes outside the session that run one of `images`, the full paths of the executables of the
/// family's windowed members. `members` are the processes known to be the session's own. An error is the
/// process list's own: the caller says the copies were not looked for, rather than reporting none.
///
/// The list is matched on the file name first, which the snapshot carries, and only a process with a
/// matching name is opened for its full path. A process that cannot be asked is not counted: a copy is a
/// claim about the work going elsewhere, and a name alone is not enough to make it.
pub(crate) fn running_copies(images: &[String], members: &HashSet<u32>) -> Result<Vec<RunningCopy>, String> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
    let entries = process_entries()?;
    Ok(copies_among(&entries, images, members, std::process::id(), full_path_of))
}

/// The copies of `images` among `entries`: a process that is not `me`, carries the file name of one of
/// the images and, asked through `path_of`, runs that very file, and is not one of `members`. Pure over
/// its inputs, so each of those conditions is tested on a made-up list.
///
/// Only the topmost process of each copy is named - one whose parent is not itself a match. A
/// multi-process application (every Chromium and Electron one) runs its helpers from the same file, and
/// a list of a dozen helpers would bury the one process the tester recognises and can close. A member is
/// matched like any process and left out only then, so the helpers it started are its own and not a copy.
pub(crate) fn copies_among(
    entries: &[ProcessEntry],
    images: &[String],
    members: &HashSet<u32>,
    me: u32,
    path_of: impl Fn(u32) -> Option<String>,
) -> Vec<RunningCopy> {
    let wanted: HashSet<String> = images.iter().map(|path| fold_case(path)).collect();
    let names: HashSet<String> = images.iter().map(|path| fold_case(file_name(path))).collect();
    let matched: Vec<&ProcessEntry> = entries
        .iter()
        .filter(|e| e.pid != 0 && e.pid != me)
        .filter(|e| names.contains(&fold_case(&e.image)))
        .filter(|e| path_of(e.pid).is_some_and(|path| wanted.contains(&fold_case(&path))))
        .collect();
    let pids: HashSet<u32> = matched.iter().map(|e| e.pid).collect();
    matched
        .into_iter()
        .filter(|e| e.parent == e.pid || !pids.contains(&e.parent))
        .filter(|e| !members.contains(&e.pid))
        .map(|e| RunningCopy { pid: e.pid, image: e.image.clone() })
        .collect()
}

/// The last part of a path, after its last separator.
fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// A path as the file system compares it: without regard to case, one character for one. A character
/// whose upper case is longer than itself (the sharp s) stays as it is, because the file system maps
/// one character to one and would not take two names that differ there for the same file.
fn fold_case(path: &str) -> String {
    path.chars()
        .map(|c| {
            let mut upper = c.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(one), None) => one,
                _ => c,
            }
        })
        .collect()
}

/// The full path of a running process's executable, or `None` when it cannot be asked.
pub(crate) fn full_path_of(pid: u32) -> Option<String> {
    // SAFETY: the handle is closed on the way out, and limited query access is all the question needs.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let path = crate::image_path_of(handle);
        let _ = CloseHandle(handle);
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, image: &str) -> ProcessEntry {
        ProcessEntry { pid, parent: 1, image: image.to_string() }
    }

    const APP: &str = r"C:\Program Files\Some App\app.exe";

    fn paths(pid: u32) -> Option<String> {
        match pid {
            10..=12 => Some(r"c:\program files\SOME APP\APP.EXE".to_string()),
            20 => Some(r"D:\elsewhere\app.exe".to_string()),
            _ => None,
        }
    }

    /// A copy is the same file, whatever case either side spells it in, not a member of the session and
    /// not this process. Another file of the same name, a process that will not say its path, a member and
    /// this process itself are not copies.
    #[test]
    fn a_copy_is_the_same_file_running_outside_the_family() {
        let list = [entry(10, "APP.exe"), entry(11, "app.exe"), entry(12, "app.exe"), entry(20, "app.exe"), entry(30, "app.exe"), entry(40, "other.exe")];
        let members = HashSet::from([11]);
        let copies = copies_among(&list, &[APP.to_string()], &members, 12, paths);
        assert_eq!(copies, vec![RunningCopy { pid: 10, image: "APP.exe".to_string() }]);
    }

    /// The helpers a member started from the same file are the member's own, not a copy outside the
    /// session: matched under the member, and left out with it.
    #[test]
    fn the_helpers_of_a_member_are_not_copies() {
        let at = |pid, parent| ProcessEntry { pid, parent, image: "app.exe".to_string() };
        let list = [at(10, 1), at(11, 10), at(12, 11), at(30, 2)];
        let copies = copies_among(&list, &[APP.to_string()], &HashSet::from([10]), 0, |_| Some(APP.to_string()));
        assert_eq!(copies.iter().map(|c| c.pid).collect::<Vec<_>>(), vec![30]);
    }

    fn unhooked(pid: u32, image: Option<&str>) -> UncoveredChild {
        UncoveredChild { pid, parent_pid: 1, image: image.map(str::to_string), command_line: None }
    }

    /// A child the hook never entered is the session's while its pid still runs the file it was named
    /// with, whatever the case. A pid that runs another file now was handed on to another program, and a
    /// child that has no name, or will not say one now, cannot be told from a stranger.
    #[test]
    fn a_pid_handed_on_is_not_the_child() {
        let now = |pid| match pid {
            50 => Some(r"C:\Program Files\Some App\HELPER.EXE".to_string()),
            51 => Some(r"C:\Windows\System32\notepad.exe".to_string()),
            _ => None,
        };
        assert!(still_the_child(&unhooked(50, Some("helper.exe")), &now));
        assert!(!still_the_child(&unhooked(51, Some("helper.exe")), &now), "a pid handed on was taken for the child");
        assert!(!still_the_child(&unhooked(50, None), &now), "a child named without a file name was confirmed");
        assert!(!still_the_child(&unhooked(52, Some("helper.exe")), &now), "a child that will not answer was confirmed");
    }

    /// A visible window is the session's when a process it holds owns it, or a child still the one it was
    /// named for - and not when the owner holds the pid of a child that has gone. Only the owners of
    /// windows are asked who they are.
    #[test]
    fn a_window_counts_for_the_session_only_when_its_owner_is_the_sessions() {
        let owners = HashSet::from([7, 50, 51]);
        let asked = std::cell::RefCell::new(Vec::new());
        let now = |pid| {
            asked.borrow_mut().push(pid);
            match pid {
                50 => Some(r"C:\a\helper.exe".to_string()),
                51 => Some(r"C:\b\stranger.exe".to_string()),
                _ => None,
            }
        };
        assert!(owned_by_member(&owners, &HashSet::from([7]), &[], now));
        assert!(owned_by_member(&owners, &HashSet::new(), &[unhooked(50, Some("helper.exe"))], now));
        assert!(!owned_by_member(&owners, &HashSet::new(), &[unhooked(51, Some("helper.exe"))], now), "a pid handed on owned the window");
        asked.borrow_mut().clear();
        assert!(!owned_by_member(&owners, &HashSet::from([8]), &[unhooked(60, Some("helper.exe"))], now));
        assert!(asked.borrow().is_empty(), "a child that owns no window was asked who it is: {:?}", asked.borrow());
        assert!(!owned_by_member(&HashSet::new(), &HashSet::from([7]), &[unhooked(50, Some("helper.exe"))], now));
    }

    /// Nothing is opened for a process whose file name matches no image, so a machine full of processes
    /// costs one pass over the list.
    #[test]
    fn only_a_process_with_a_matching_name_is_asked_for_its_path() {
        let list = [entry(10, "app.exe"), entry(40, "other.exe"), entry(50, "third.exe")];
        let asked = std::cell::RefCell::new(Vec::new());
        let copies = copies_among(&list, &[APP.to_string()], &HashSet::new(), 0, |pid| {
            asked.borrow_mut().push(pid);
            paths(pid)
        });
        assert_eq!(copies.len(), 1);
        assert_eq!(*asked.borrow(), vec![10]);
    }

    /// A copy that runs its helpers from the same file is named once, by its topmost process, and two
    /// separate copies are named twice.
    #[test]
    fn a_copy_is_named_by_its_topmost_process_only() {
        let at = |pid, parent| ProcessEntry { pid, parent, image: "app.exe".to_string() };
        let list = [at(10, 1), at(11, 10), at(12, 11), at(13, 10), at(30, 2), at(31, 30)];
        let copies = copies_among(&list, &[APP.to_string()], &HashSet::new(), 0, |_| Some(APP.to_string()));
        assert_eq!(copies.iter().map(|c| c.pid).collect::<Vec<_>>(), vec![10, 30]);
    }

    #[test]
    fn no_images_find_no_copies_and_pid_zero_is_never_one() {
        let list = [entry(0, "app.exe"), entry(10, "app.exe")];
        assert!(copies_among(&list, &[], &HashSet::new(), 0, paths).is_empty());
        let copies = copies_among(&list, &[APP.to_string()], &HashSet::new(), 99, |_| Some(APP.to_string()));
        assert_eq!(copies.iter().map(|c| c.pid).collect::<Vec<_>>(), vec![10]);
        assert!(running_copies(&[], &HashSet::new()).expect("no images ask nothing").is_empty());
    }

    #[test]
    fn case_folds_one_character_for_one() {
        assert_eq!(fold_case(r"C:\Données\Ärger.exe"), fold_case(r"c:\DONNÉES\ÄRGER.EXE"));
        assert_ne!(fold_case("stra\u{df}e.exe"), fold_case("STRASSE.EXE"), "one character became two");
        assert_eq!(file_name(r"C:\a\b\app.exe"), "app.exe");
        assert_eq!(file_name("app.exe"), "app.exe");
    }

    /// The process running these tests is found as a copy of itself when it is not a member, and not when
    /// it is - the whole path through the process list and the full path of a live process.
    #[test]
    fn this_process_is_found_as_a_copy_of_its_own_executable() {
        let me = std::process::id();
        let own = std::env::current_exe().expect("this test knows its executable");
        let image = own.to_string_lossy().into_owned();
        let entries = process_entries().expect("the snapshot is readable");
        let found = copies_among(&entries, std::slice::from_ref(&image), &HashSet::new(), 0, full_path_of);
        assert!(found.iter().any(|c| c.pid == me), "this process was not found among {found:?}");
        let members = HashSet::from([me]);
        let found = copies_among(&entries, std::slice::from_ref(&image), &members, 0, full_path_of);
        assert!(found.iter().all(|c| c.pid != me), "a member of the session was taken for a copy");
    }

    /// The walk over the real desktop names owners, and a pid no process holds owns no window.
    #[test]
    fn a_pid_no_process_holds_owns_no_window() {
        let owners = visible_window_owners();
        assert!(!owners.contains(&(u32::MAX - 3)), "{owners:?}");
        assert!(!owners.contains(&0), "the owner 0 of a window that closed meanwhile was kept");
    }
}
