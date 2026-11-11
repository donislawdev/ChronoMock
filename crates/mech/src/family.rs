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
//! had its pid recycled before that first look - a child poll is 100 ms apart. It is closed by the
//! creation time the member's hook wrote into its slot: the process behind the handle either was
//! created then, or is somebody else under the same number (R4-N12). Where that cannot be asked, the
//! process snapshot answers instead, by who started the process: a recycled pid belongs to a process
//! started by somebody outside the family. The snapshot alone used to decide, and it turned away a
//! real member whose parent it did not know - a child started with a parent chosen for it
//! (`PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`), which the session then ended under.

use std::collections::HashSet;

use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

use crate::process_created;
use crate::tree::{process_entries, ProcessEntry};

/// One process that signed in to the session: its registry slot, its pid, the creation time its hook
/// recorded (0 when it recorded none), and whether it may show windows - any subsystem its hook recorded
/// but the console's, an unrecorded one included, as `only_console` keeps the doubt.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SignedIn {
    pub(crate) slot: usize,
    pub(crate) pid: u32,
    pub(crate) created: u64,
    pub(crate) windowed: bool,
}

/// What the creation time says about the process behind a handle opened by a member's pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Identity {
    /// Created when its slot says: the member itself.
    Same,
    /// Created at another time: somebody else got the pid.
    Other,
    /// Not known - the slot holds no time, or the handle does not allow the question.
    Unknown,
}

/// Compare the time a member's hook recorded with the time the process behind the handle was created.
fn identity(recorded: u64, now: Option<u64>) -> Identity {
    match now {
        _ if recorded == 0 => Identity::Unknown,
        None => Identity::Unknown,
        Some(t) if t == recorded => Identity::Same,
        Some(_) => Identity::Other,
    }
}

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
    /// Members that signed in and would not let this process wait on them, so the session cannot see
    /// them close and may end while they run.
    unwatchable: u32,
    /// The full paths of the executables of the windowed members, each once, taken when a member is
    /// first opened: a process that has ended may no longer say its path (ADR-4, 2026-11-11).
    windowed_images: Vec<String>,
}

impl Family {
    pub(crate) fn new(slots: usize, root_slot: Option<usize>) -> Self {
        Family { watch: (0..slots).map(|_| Watch::Unseen).collect(), root_slot, unwatchable: 0, windowed_images: Vec::new() }
    }

    /// The launched process's own slot, when it signed in.
    pub(crate) fn root_slot(&self) -> Option<usize> {
        self.root_slot
    }

    /// The executables of the windowed members other than the launched process, as `refresh` found them.
    pub(crate) fn windowed_images(&self) -> &[String] {
        &self.windowed_images
    }

    /// How many members signed in that this session could not watch (R5-D3).
    pub(crate) fn unwatchable(&self) -> u32 {
        self.unwatchable
    }

    /// Start watching every process that signed in since the last call, and stop watching the ones
    /// that ended. `published` is every process signed in so far, the launched one included.
    pub(crate) fn refresh(&mut self, root_pid: u32, published: &[SignedIn]) {
        let mut opened: Vec<(SignedIn, HANDLE, Identity)> = Vec::new();
        for member in published {
            let slot = member.slot;
            let Some(Watch::Unseen) = self.watch.get(slot) else {
                continue;
            };
            if Some(slot) == self.root_slot {
                continue;
            }
            // SAFETY: plain opens by pid. The handle is closed below once the process ends, or in `Drop`.
            match unsafe { open_member(member.pid, member.created) } {
                Ok((handle, id)) => opened.push((*member, handle, id)),
                Err(why) => {
                    if why == Unopened::Denied {
                        self.unwatchable += 1;
                    }
                    self.watch[slot] = Watch::Done;
                }
            }
        }
        if !opened.is_empty() {
            let known: HashSet<u32> = std::iter::once(root_pid).chain(published.iter().map(|m| m.pid)).collect();
            let entries = process_entries().ok();
            for (member, handle, id) in opened {
                let (slot, pid) = (member.slot, member.pid);
                self.watch[slot] = match membership(id, pid, &known, entries.as_deref()) {
                    Some(image) => {
                        self.note_windowed(member, handle);
                        Watch::Living { pid, handle, image }
                    }
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

    /// Keep the path of a windowed member's executable, once, read through the handle just opened on it.
    /// The handle allows limited queries when the open could ask for them, and without them the path is
    /// not there to read.
    fn note_windowed(&mut self, member: SignedIn, handle: HANDLE) {
        if !member.windowed {
            return;
        }
        // SAFETY: an open process handle, which the caller keeps for as long as this call.
        let path = unsafe { crate::image_path_of(handle) };
        if let Some(path) = path
            && !self.windowed_images.iter().any(|known| known.eq_ignore_ascii_case(&path))
        {
            self.windowed_images.push(path);
        }
    }

    /// Whether `slot` is the launched process's own, which the session watches through its own handle.
    pub(crate) fn is_root(&self, slot: usize) -> bool {
        Some(slot) == self.root_slot
    }

    /// Whether the process in `slot` may still be running: it was when `refresh` last looked, or it has
    /// not been looked at yet. A slot that ended, or could never be watched, may not.
    pub(crate) fn may_run(&self, slot: usize) -> bool {
        !matches!(self.watch.get(slot), Some(Watch::Done))
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

/// Why a member could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unopened {
    /// No process holds the pid any more: the member ended before the first look.
    Gone,
    /// A process holds it and will not let this one wait on it.
    Denied,
}

/// What a failed open says, from the code it failed with: only a refusal is a process still there.
fn unopened(code: windows::core::HRESULT) -> Unopened {
    if code == ERROR_ACCESS_DENIED.to_hresult() { Unopened::Denied } else { Unopened::Gone }
}

/// Open a member by pid for waiting, with what its creation time says about it, or why it cannot be
/// opened at all.
///
/// Waiting is what the handle is for, and the creation time is asked for beside it. A process that
/// grants waiting and denies the question stays watchable, just without the time. One that denies even
/// waiting is not watched: the session ends as it did before ADR-16, rather than holding on to a
/// process it can never see close. It used to count as ended, which it cannot be told from by a handle,
/// but it can by the code the open failed with, and the session says so (`session.member_unwatchable`,
/// R5-D3): it may end while that process still runs on its clock.
///
/// # Safety
/// A plain open by pid. The caller owns the handle it gets back.
unsafe fn open_member(pid: u32, created: u64) -> Result<(HANDLE, Identity), Unopened> { unsafe {
    if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
        return Ok((handle, identity(created, process_created(handle))));
    }
    match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
        Ok(handle) => Ok((handle, Identity::Unknown)),
        Err(e) => Err(unopened(e.code())),
    }
}}

/// Whether the process behind a freshly opened handle is the one that signed in, and its image name when
/// it is. The creation time decides when it can (R4-N12): the same time is the member whatever the
/// snapshot says about its parent, another time is a stranger. Without a time, `member_of_family` asks
/// the snapshot as it always did.
fn membership(id: Identity, pid: u32, known: &HashSet<u32>, entries: Option<&[ProcessEntry]>) -> Option<Option<String>> {
    match id {
        Identity::Same => Some(entries.and_then(|list| list.iter().find(|e| e.pid == pid)).map(|e| e.image.clone())),
        Identity::Other => None,
        Identity::Unknown => member_of_family(pid, known, entries),
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
pub(crate) fn member_of_family(pid: u32, known: &HashSet<u32>, entries: Option<&[ProcessEntry]>) -> Option<Option<String>> {
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

    /// Only a refusal is a member that is still there. The code a pid nobody holds fails with is asked
    /// of the system rather than assumed (R5-D3).
    #[test]
    fn only_a_refused_open_is_a_member_still_running() {
        assert_eq!(unopened(ERROR_ACCESS_DENIED.to_hresult()), Unopened::Denied);
        assert_eq!(unopened(windows::Win32::Foundation::ERROR_INVALID_PARAMETER.to_hresult()), Unopened::Gone);
        // SAFETY: a plain open by a pid nobody holds, nothing to close.
        let gone = unsafe { open_member(u32::MAX - 5, 0) };
        assert_eq!(gone.err(), Some(Unopened::Gone));
    }

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

    /// The creation time decides whenever the slot holds one (R4-N12): the same time is the member even
    /// when the snapshot names a parent outside the family, another time is a stranger even when the
    /// parent is known. Only a slot without a time falls back to the parent test.
    #[test]
    fn the_creation_time_decides_membership_before_the_parent_does() {
        let known = HashSet::from([10, 20]);
        let list = [entry(30, 99, "app.exe")];
        assert_eq!(membership(Identity::Same, 30, &known, Some(&list)), Some(Some("app.exe".to_string())));
        assert_eq!(membership(Identity::Same, 30, &known, None), Some(None));
        let adopted = [entry(30, 20, "app.exe")];
        assert_eq!(membership(Identity::Other, 30, &known, Some(&adopted)), None);
        assert_eq!(membership(Identity::Unknown, 30, &known, Some(&list)), None);
        assert_eq!(membership(Identity::Unknown, 30, &known, Some(&adopted)), Some(Some("app.exe".to_string())));
    }

    #[test]
    fn a_creation_time_names_the_process_only_when_both_sides_have_one() {
        assert_eq!(identity(5, Some(5)), Identity::Same);
        assert_eq!(identity(5, Some(6)), Identity::Other);
        assert_eq!(identity(5, None), Identity::Unknown);
        assert_eq!(identity(0, Some(5)), Identity::Unknown, "a slot without a time proved something");
    }

    fn signed(slot: usize, pid: u32, created: u64) -> SignedIn {
        SignedIn { slot, pid, created, windowed: false }
    }

    #[test]
    fn the_launched_process_and_a_slot_seen_before_are_left_alone() {
        // The launched process is the session's own to watch, and a slot is looked at once: neither
        // is opened here, so a pid of this test process in them proves nothing is kept.
        let me = std::process::id();
        let mut family = Family::new(4, Some(0));
        family.watch[1] = Watch::Done;
        family.refresh(me, &[signed(0, me, 0), signed(1, me, 0)]);
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
        family.refresh(parent, &[signed(0, parent, 0), signed(2, me, 0)]);
        let living = family.living();
        assert_eq!(living.len(), 1, "{living:?}");
        assert_eq!(living[0].pid, me);
        assert!(living[0].image.as_deref().is_some_and(|i| !i.is_empty()), "{living:?}");
        // Signed in by a parent outside the family, with no time to vouch for it: not a member, and no
        // handle kept.
        let mut stranger = Family::new(4, Some(0));
        stranger.refresh(u32::MAX - 1, &[signed(0, u32::MAX - 1, 0), signed(2, me, 0)]);
        assert!(stranger.living().is_empty());
        assert!(matches!(stranger.watch[2], Watch::Done));
    }

    /// ADR-4, 2026-11-11. A windowed member leaves the path of its executable when it is first opened,
    /// once however many slots name the same file, and a console member leaves nothing: only a windowed
    /// program hands its work to a copy.
    #[test]
    fn a_windowed_member_leaves_the_path_of_its_executable() {
        let me = std::process::id();
        // SAFETY: the pseudo-handle of this process.
        let created = unsafe { process_created(windows::Win32::System::Threading::GetCurrentProcess()) }
            .expect("this process can read its own creation time");
        let windowed = |slot| SignedIn { slot, pid: me, created, windowed: true };
        let mut family = Family::new(4, Some(0));
        family.refresh(u32::MAX - 1, &[signed(0, u32::MAX - 1, 0), windowed(1), windowed(2)]);
        let own = std::env::current_exe().expect("this test knows its executable").to_string_lossy().into_owned();
        assert_eq!(family.windowed_images().len(), 1, "{:?}", family.windowed_images());
        assert!(family.windowed_images()[0].eq_ignore_ascii_case(&own), "{:?} is not {own}", family.windowed_images());

        let mut console = Family::new(4, Some(0));
        console.refresh(u32::MAX - 1, &[signed(0, u32::MAX - 1, 0), signed(1, me, created)]);
        assert_eq!(console.living().len(), 1, "the console member is watched all the same");
        assert!(console.windowed_images().is_empty(), "a console member left a path");
    }

    /// A member whose parent the family never saw - started with a parent chosen for it - is still a
    /// member when its slot's creation time matches (R4-N12), and a time that does not match turns
    /// away even a process whose parent is known.
    #[test]
    fn a_member_with_an_unknown_parent_is_kept_by_its_creation_time() {
        let me = std::process::id();
        // SAFETY: the pseudo-handle of this process.
        let created = unsafe { process_created(windows::Win32::System::Threading::GetCurrentProcess()) }
            .expect("this process can read its own creation time");
        let mut adopted = Family::new(4, Some(0));
        adopted.refresh(u32::MAX - 1, &[signed(0, u32::MAX - 1, 0), signed(2, me, created)]);
        let living = adopted.living();
        assert_eq!(living.len(), 1, "a member vouched for by its creation time was turned away: {living:?}");
        assert!(living[0].image.as_deref().is_some_and(|i| !i.is_empty()), "{living:?}");

        let entries = process_entries().expect("the snapshot is readable");
        let parent = entries.iter().find(|e| e.pid == me).expect("this process is listed").parent;
        let mut recycled = Family::new(4, Some(0));
        recycled.refresh(parent, &[signed(0, parent, 0), signed(2, me, created + 1)]);
        assert!(recycled.living().is_empty(), "a process created at another time passed for the member");
        assert!(matches!(recycled.watch[2], Watch::Done));
    }
}
