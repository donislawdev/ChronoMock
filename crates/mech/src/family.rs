//! The processes a session lasts for.
//!
//! A session used to end with the process it launched. A launcher, a restart after an update and a
//! batch script that runs `start app.exe` all end that process on purpose and leave the application
//! running, with the hook in it (ADR-3). Ending the session there let the application go back to the
//! real clock seconds after it started, and a launcher that ended inside the opening guard window
//! was even reported as a single-instance application (measured, tools/probes/starter). The session
//! now lasts for as long as any process on its clock is running: the one it launched, or any process
//! the hook followed into (ADR-16).
//!
//! A member is watched through a handle opened the first time its sign-in slot is seen, so a pid the
//! system recycles later cannot keep the session alive. The one gap left is a member that ended and
//! had its pid recycled before that first look - a child poll is 100 ms apart - and it is closed by
//! asking the process snapshot who started the process behind the handle: a recycled pid belongs to a
//! process started by somebody outside the family.

use std::collections::HashSet;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

use crate::tree::{process_entries, ProcessEntry};

/// A process of the family that was still running when the session last looked, named by the file
/// of its executable when the snapshot had it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyMember {
    pub pid: u32,
    pub image: Option<String>,
}

/// What the session knows about one sign-in slot.
enum Watch {
    /// Not looked at yet.
    Unseen,
    /// Running when last asked, through a handle this session owns.
    Living { pid: u32, handle: HANDLE, image: Option<String> },
    /// Ended, or never watchable: it could not be opened, or the snapshot said it is not ours.
    Done,
}

/// Every process of the family besides the one the session launched, which the session watches
/// through its own handle.
pub(crate) struct Family {
    watch: Vec<Watch>,
    /// The launched process's own slot, left to the session's handle on it.
    root_slot: Option<usize>,
}

impl Family {
    pub(crate) fn new(slots: usize, root_slot: Option<usize>) -> Self {
        Family { watch: (0..slots).map(|_| Watch::Unseen).collect(), root_slot }
    }

    /// Start watching every process that signed in since the last call, and stop watching the ones
    /// that ended. `published` is every `(slot, pid)` signed in so far, the launched one included.
    pub(crate) fn refresh(&mut self, root_pid: u32, published: &[(usize, u32)]) {
        let mut opened: Vec<(usize, u32, HANDLE)> = Vec::new();
        for &(slot, pid) in published {
            let Some(Watch::Unseen) = self.watch.get(slot) else {
                continue;
            };
            if Some(slot) == self.root_slot {
                continue;
            }
            // Waiting is all the handle is for, so waiting is all it asks: a process that grants that and
            // denies more stays watchable. One that denies even this cannot be told from one that ended
            // (both fail to open), so it counts as ended - the session then ends as it did before ADR-16,
            // rather than holding on to a process it can never see close.
            // SAFETY: a plain open by pid. The handle is closed below once the process ends, or in `Drop`.
            match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
                Ok(handle) => opened.push((slot, pid, handle)),
                Err(_) => self.watch[slot] = Watch::Done,
            }
        }
        if !opened.is_empty() {
            let known: HashSet<u32> = std::iter::once(root_pid).chain(published.iter().map(|&(_, pid)| pid)).collect();
            let entries = process_entries().ok();
            for (slot, pid, handle) in opened {
                self.watch[slot] = match member_of_family(pid, &known, entries.as_deref()) {
                    Some(image) => Watch::Living { pid, handle, image },
                    None => {
                        // SAFETY: opened above and owned by nobody else.
                        unsafe {
                            let _ = CloseHandle(handle);
                        }
                        Watch::Done
                    }
                };
            }
        }
        for watch in &mut self.watch {
            if let Watch::Living { handle, .. } = watch {
                // SAFETY: the handle is ours until it is closed right here or in `Drop`.
                let running = unsafe { WaitForSingleObject(*handle, 0) } == WAIT_TIMEOUT;
                if !running {
                    unsafe {
                        let _ = CloseHandle(*handle);
                    }
                    *watch = Watch::Done;
                }
            }
        }
    }

    /// The members still running when `refresh` last looked.
    pub(crate) fn living(&self) -> Vec<FamilyMember> {
        self.watch
            .iter()
            .filter_map(|w| match w {
                Watch::Living { pid, image, .. } => Some(FamilyMember { pid: *pid, image: image.clone() }),
                _ => None,
            })
            .collect()
    }
}

impl Drop for Family {
    fn drop(&mut self) {
        for watch in &self.watch {
            if let Watch::Living { handle, .. } = watch {
                // SAFETY: each living handle is ours and closed exactly once, here.
                unsafe {
                    let _ = CloseHandle(*handle);
                }
            }
        }
    }
}

/// Whether the process behind a freshly opened handle is the one that signed in, and its image name
/// when it is: `None` when the snapshot no longer lists it (it ended) or lists it with a parent
/// outside the family (its pid was recycled before the first look). A member's parent signed in
/// before starting it, so a parent missing from `known` is somebody else's.
///
/// A snapshot that could not be read (`entries` is `None`) answers yes, without a name. The handle
/// opened at first sight is the guard that matters, and ending a session under a running application
/// because a list was unreadable would be the failure this module exists to fix.
fn member_of_family(pid: u32, known: &HashSet<u32>, entries: Option<&[ProcessEntry]>) -> Option<Option<String>> {
    match entries {
        None => Some(None),
        Some(list) => list
            .iter()
            .find(|e| e.pid == pid)
            .filter(|e| known.contains(&e.parent))
            .map(|e| Some(e.image.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, parent: u32, image: &str) -> ProcessEntry {
        ProcessEntry { pid, parent, image: image.to_string() }
    }

    #[test]
    fn a_process_started_by_the_family_is_a_member_and_is_named() {
        let known = HashSet::from([10, 20]);
        let list = [entry(30, 20, "app.exe"), entry(40, 99, "other.exe")];
        assert_eq!(member_of_family(30, &known, Some(&list)), Some(Some("app.exe".to_string())));
    }

    #[test]
    fn a_recycled_pid_or_an_ended_process_is_not_a_member() {
        let known = HashSet::from([10, 20]);
        let list = [entry(40, 99, "other.exe")];
        // Pid 40 now belongs to a process started outside the family: its slot's process is gone.
        assert_eq!(member_of_family(40, &known, Some(&list)), None);
        // Not listed at all: it ended between the open and the snapshot.
        assert_eq!(member_of_family(50, &known, Some(&list)), None);
    }

    #[test]
    fn an_unreadable_snapshot_keeps_the_member_without_a_name() {
        let known = HashSet::from([10]);
        assert_eq!(member_of_family(30, &known, None), Some(None));
    }

    #[test]
    fn the_launched_process_and_a_slot_seen_before_are_left_alone() {
        // The launched process is the session's own to watch, and a slot is looked at once: neither
        // is opened here, so a pid of this test process in them proves nothing is kept.
        let me = std::process::id();
        let mut family = Family::new(4, Some(0));
        family.watch[1] = Watch::Done;
        family.refresh(me, &[(0, me), (1, me)]);
        assert!(family.living().is_empty());
        assert!(matches!(family.watch[0], Watch::Unseen));
        assert!(matches!(family.watch[1], Watch::Done));
    }

    #[test]
    fn a_running_member_is_watched_and_a_stranger_is_not() {
        // This test process stands in for a member: started by the process that runs the tests, which
        // is listed as signed in, so the snapshot vouches for it.
        let me = std::process::id();
        let entries = process_entries().expect("the snapshot is readable");
        let parent = entries.iter().find(|e| e.pid == me).expect("this process is listed").parent;
        let mut family = Family::new(4, Some(0));
        family.refresh(parent, &[(0, parent), (2, me)]);
        let living = family.living();
        assert_eq!(living.len(), 1, "{living:?}");
        assert_eq!(living[0].pid, me);
        assert!(living[0].image.as_deref().is_some_and(|i| !i.is_empty()), "{living:?}");
        // Signed in by a parent outside the family: not a member, and no handle kept.
        let mut stranger = Family::new(4, Some(0));
        stranger.refresh(u32::MAX - 1, &[(0, u32::MAX - 1), (2, me)]);
        assert!(stranger.living().is_empty());
        assert!(matches!(stranger.watch[2], Watch::Done));
    }
}
