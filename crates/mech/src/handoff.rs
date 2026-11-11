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

use std::collections::HashSet;

use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible};

use crate::tree::{process_entries, ProcessEntry};

/// A process outside the family that runs the same executable file as a windowed member of it: its pid,
/// and the file name of the executable as the process list gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningCopy {
    pub pid: u32,
    pub image: String,
}

/// Whether any of `pids` owns a visible top-level window on this desktop right now.
///
/// A window that is not visible does not count: every program with a message loop owns a few hidden
/// ones (input method, clipboard, a hidden main window before it is shown), and none of them is a
/// window the tester could have used. A window on another virtual desktop is visible, and counts.
pub fn shows_window(pids: &HashSet<u32>) -> bool {
    if pids.is_empty() {
        return false;
    }
    let mut look = WindowLook { pids, found: false };
    // SAFETY: the callback runs on this thread before `EnumWindows` returns, and the pointer it gets is
    // to `look`, which lives across the call. The call fails when the callback stops the walk early,
    // which is how a find ends it, so its result says nothing `look.found` does not.
    unsafe {
        let _ = EnumWindows(Some(visit_window), LPARAM(&raw mut look as isize));
    }
    look.found
}

/// What one walk over the top-level windows looks for and whether it found it.
struct WindowLook<'a> {
    pids: &'a HashSet<u32>,
    found: bool,
}

/// One top-level window of the walk: stop at the first visible one a process of the family owns.
///
/// # Safety
/// `lparam` is the address of the `WindowLook` that `shows_window` passed, alive for the whole walk.
unsafe extern "system" fn visit_window(window: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: see the function's contract. Nothing else holds the look while the walk runs.
    let look = unsafe { &mut *(lparam.0 as *mut WindowLook) };
    // SAFETY: both calls only read about the window, and a window that closed meanwhile answers no.
    let visible = unsafe { IsWindowVisible(window) }.as_bool();
    if visible {
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(window, Some(&raw mut owner)) };
        if owner != 0 && look.pids.contains(&owner) {
            look.found = true;
            return BOOL(0);
        }
    }
    BOOL(1)
}

/// The processes outside `family` that run one of `images`, the full paths of the executables of the
/// family's windowed members. An error is the process list's own: the caller says the copies were not
/// looked for, rather than reporting none.
///
/// The list is matched on the file name first, which the snapshot carries, and only a process with a
/// matching name is opened for its full path. A process that cannot be asked is not counted: a copy is a
/// claim about the work going elsewhere, and a name alone is not enough to make it.
pub fn running_copies(images: &[String], family: &HashSet<u32>) -> Result<Vec<RunningCopy>, String> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
    let entries = process_entries()?;
    Ok(copies_among(&entries, images, family, std::process::id(), full_path_of))
}

/// The copies of `images` among `entries`: a process that is not in `family`, is not `me`, carries the
/// file name of one of the images and, asked through `path_of`, runs that very file. Pure over its
/// inputs, so each of those conditions is tested on a made-up list.
///
/// Only the topmost process of each copy is named - one whose parent is not itself a match. A
/// multi-process application (every Chromium and Electron one) runs its helpers from the same file, and
/// a list of a dozen helpers would bury the one process the tester recognises and can close.
pub(crate) fn copies_among(
    entries: &[ProcessEntry],
    images: &[String],
    family: &HashSet<u32>,
    me: u32,
    path_of: impl Fn(u32) -> Option<String>,
) -> Vec<RunningCopy> {
    let wanted: HashSet<String> = images.iter().map(|path| fold_case(path)).collect();
    let names: HashSet<String> = images.iter().map(|path| fold_case(file_name(path))).collect();
    let matched: Vec<&ProcessEntry> = entries
        .iter()
        .filter(|e| e.pid != 0 && e.pid != me && !family.contains(&e.pid))
        .filter(|e| names.contains(&fold_case(&e.image)))
        .filter(|e| path_of(e.pid).is_some_and(|path| wanted.contains(&fold_case(&path))))
        .collect();
    let pids: HashSet<u32> = matched.iter().map(|e| e.pid).collect();
    matched
        .into_iter()
        .filter(|e| e.parent == e.pid || !pids.contains(&e.parent))
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
fn full_path_of(pid: u32) -> Option<String> {
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

    /// A copy is the same file, whatever case either side spells it in, outside the family and not this
    /// process. Another file of the same name, a process that will not say its path, a member of the
    /// family and this process itself are not copies.
    #[test]
    fn a_copy_is_the_same_file_running_outside_the_family() {
        let list = [entry(10, "APP.exe"), entry(11, "app.exe"), entry(12, "app.exe"), entry(20, "app.exe"), entry(30, "app.exe"), entry(40, "other.exe")];
        let family = HashSet::from([11]);
        let copies = copies_among(&list, &[APP.to_string()], &family, 12, paths);
        assert_eq!(copies, vec![RunningCopy { pid: 10, image: "APP.exe".to_string() }]);
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

    /// The process running these tests is found as a copy of itself when it is not in the family, and
    /// not when it is - the whole path through the process list and the full path of a live process.
    #[test]
    fn this_process_is_found_as_a_copy_of_its_own_executable() {
        let me = std::process::id();
        let own = std::env::current_exe().expect("this test knows its executable");
        let image = own.to_string_lossy().into_owned();
        let entries = process_entries().expect("the snapshot is readable");
        let found = copies_among(&entries, std::slice::from_ref(&image), &HashSet::new(), 0, full_path_of);
        assert!(found.iter().any(|c| c.pid == me), "this process was not found among {found:?}");
        let family = HashSet::from([me]);
        let found = copies_among(&entries, std::slice::from_ref(&image), &family, 0, full_path_of);
        assert!(found.iter().all(|c| c.pid != me), "a member of the family was taken for a copy");
    }

    #[test]
    fn an_empty_family_shows_no_window() {
        assert!(!shows_window(&HashSet::new()));
        // A pid no process holds owns no window.
        assert!(!shows_window(&HashSet::from([u32::MAX - 3])));
    }
}
