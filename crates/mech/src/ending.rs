//! Ending the family of a session that refused to go on (R4-S5).
//!
//! When the opening verdict says the substitution did not take effect, the core does not leave the
//! application running: every minute a tester spent in it would be evidence about the real clock that
//! looks like a time-shifted run. It used to end only the process it launched, and whatever that
//! process had started in its first moments ran on, on the real clock, with nothing to say so.
//!
//! The family is not in one place at that moment. The hook signs the processes it followed into the
//! registry, the ring names the children it could not follow, and a process the hook never saw at all
//! is only in the system's process list. That list alone cannot be trusted: a parent pid is a number
//! the system recycles, so a process whose parent ended long ago can name a pid our target holds
//! today, and ending it would end somebody else's work. So every step from a parent to a child is
//! checked by creation time - the child was created after its parent, and before the moment the list
//! was taken - and a process that cannot be asked when it was created is never ended.
//!
//! Every process found is held by a handle until the end, so no pid of the family changes hands
//! between rounds. The root ends first, before the first list, so it starts nothing more, and up to
//! three rounds catch what a member started while the round before was ending the others.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};

use crate::family::FamilyMember;
use crate::process_created;
use crate::tree::{process_entries, ProcessEntry};

/// How many times the process list is read. A member that was still running while the round before
/// ended the others can start a child in between, and the next list is what finds it. Bounded, so an
/// application that keeps starting processes faster than they end cannot hold the refusal open.
const ROUNDS: usize = 3;

/// How long the ended processes get, all together, to be gone before the ones still there are named.
/// Ending a process is asked for, not done on the spot - a thread stuck in the kernel finishes first.
const EXIT_WAIT: Duration = Duration::from_secs(2);

/// A process the family vouches for: its pid and when it was created, as one FILETIME number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Known {
    pub(crate) pid: u32,
    pub(crate) born: u64,
}

/// What ending a refused family left behind.
#[derive(Debug, Default)]
pub struct FamilyEnd {
    /// The processes of the application still running when it was over: ones this core had no right
    /// to end, ones that did not end in time, and children the hook named that could not be confirmed
    /// as the application's, which are left alone rather than risk ending somebody else's process.
    pub left_running: Vec<FamilyMember>,
    /// Why the family could not be looked for in full, when it could not: the process list would not
    /// be read, the launched process would not say when it was created, or the application was still
    /// starting processes after the last round. Processes it started may then run on unnamed, and the
    /// caller has to say so wherever it says the application was ended.
    pub incomplete: Option<String>,
}

/// What the rounds need from the system, behind one seam so the rounds can be tested on made-up lists:
/// the time, the process list, a process's creation time (opening it the first time), and ending one.
trait Processes {
    fn now(&mut self) -> u64;
    fn list(&mut self) -> Result<Vec<ProcessEntry>, String>;
    fn born_of(&mut self, pid: u32) -> Option<u64>;
    fn end(&mut self, pid: u32);
}

/// What the rounds found: the family (the launched process first), the ring children named and not
/// ended, the image names the lists gave, and why the search was not complete, when it was not.
struct Searched {
    family: Vec<Known>,
    named: Vec<u32>,
    images: HashMap<u32, String>,
    incomplete: Option<String>,
}

/// End the process the session launched and every process of its family this core can confirm, and
/// say which ones are still running afterwards.
///
/// `root` is the session's own handle on the launched process, which keeps its pid from being given
/// to anybody else. `slots` is the registry: every process the hook signed in, with the creation time
/// its hook recorded (0 when it recorded none). `ring` is every child the hook named without getting
/// into it.
pub(crate) fn end_family(root: HANDLE, root_pid: u32, slots: &[Known], ring: &[u32]) -> FamilyEnd {
    // SAFETY: the caller's handle on the launched process, with every right the launch gave it. Ending
    // a process that already exited fails harmlessly.
    let root_born = unsafe { process_created(root) };
    unsafe {
        let _ = TerminateProcess(root, 1);
    }
    // SAFETY: no arguments, no failure.
    let own = unsafe { GetCurrentProcessId() };
    let mut opened = Opened::default();
    let searched = search(&mut opened, root_pid, root_born, slots, ring, own);
    let images = &searched.images;
    let mut left_running = still_running(root, &searched.family, &opened, images);
    left_running.extend(searched.named.iter().map(|&pid| FamilyMember { pid, image: images.get(&pid).cloned() }));
    FamilyEnd { left_running, incomplete: searched.incomplete }
}

/// The rounds: read the time, take the list, confirm and end what is new, parents before children. A
/// list that could not be read ends the registry members all the same and is tried again next round.
/// The search is complete when a list that was read found nothing new - everything of the family still
/// running at that moment had been ended already.
///
/// A launched process that would not say when it was created vouches for nothing: 0 would let any older
/// process naming its pid through as a child, so its children are not looked for at all, and the
/// answer says so.
fn search(sys: &mut dyn Processes, root_pid: u32, root_born: Option<u64>, slots: &[Known], ring: &[u32], own: u32) -> Searched {
    let mut family = vec![Known { pid: root_pid, born: root_born.unwrap_or(u64::MAX) }];
    let mut images: HashMap<u32, String> = HashMap::new();
    let mut named = Vec::new();
    let mut unread = None;
    let mut settled = false;
    for _ in 0..ROUNDS {
        // Read before the list, so a process created after it cannot pass for one the list holds.
        let taken_at = sys.now();
        // A list read after one that failed makes the failure no longer the reason for anything.
        let listed = match sys.list() {
            Ok(list) => {
                unread = None;
                Some(list)
            }
            Err(e) => {
                unread = Some(e);
                None
            }
        };
        let entries = listed.as_deref().unwrap_or_default();
        images.extend(entries.iter().map(|e| (e.pid, e.image.clone())));
        let found = round(&mut family, slots, entries, taken_at, own, &mut |pid| sys.born_of(pid));
        if listed.is_some() {
            // Only from a list that was read: an empty one would forget the children named before.
            named = named_from_ring(ring, &family, entries, taken_at, own, &mut |pid| sys.born_of(pid));
        }
        for k in &found {
            sys.end(k.pid);
        }
        if listed.is_some() && found.is_empty() {
            settled = true;
            break;
        }
    }
    let incomplete = if root_born.is_none() {
        Some("the launched process would not say when it was created, so the processes it started were not looked for".to_string())
    } else if settled {
        None
    } else if let Some(e) = unread {
        Some(format!("the process list could not be read ({e}), so processes the application started may still be running"))
    } else {
        Some(format!("the application was still starting processes after {ROUNDS} rounds, so the latest may still be running"))
    };
    Searched { family, named, images, incomplete }
}

/// One round over one process list: the registry members not confirmed before, then every process
/// the family started, parents before children. `family` grows by what is found, and the same list is
/// returned so the caller ends exactly those.
fn round(
    family: &mut Vec<Known>,
    slots: &[Known],
    listed: &[ProcessEntry],
    taken_at: u64,
    own: u32,
    born_of: &mut dyn FnMut(u32) -> Option<u64>,
) -> Vec<Known> {
    let (members, ghosts) = split_registry(slots, born_of);
    let mut found: Vec<Known> =
        members.into_iter().filter(|m| m.pid != own && !family.iter().any(|k| k.pid == m.pid)).collect();
    family.extend(&found);
    let walked = confirm(family, &ghosts, listed, taken_at, own, born_of);
    family.extend(&walked);
    found.extend(walked);
    found
}

/// Split the registry into members and ghosts. A slot's process is a member when the process under its
/// pid now was created when the slot says. Otherwise the member has ended and the slot is a ghost: it
/// still vouches for the children that member started, but its pid is free or somebody else's. A slot
/// without a time is neither - the walk can still reach its process through the process that started it.
fn split_registry(slots: &[Known], born_of: &mut dyn FnMut(u32) -> Option<u64>) -> (Vec<Known>, Vec<Known>) {
    let mut members = Vec::new();
    let mut ghosts = Vec::new();
    for slot in slots.iter().filter(|s| s.born != 0) {
        if born_of(slot.pid) == Some(slot.born) {
            members.push(*slot);
        } else {
            ghosts.push(*slot);
        }
    }
    (members, ghosts)
}

/// The listed processes the family started that it had not confirmed before, parents before children.
///
/// A process counts when the pid it names as its parent belongs to the family, it was created no
/// earlier than that parent, and no later than the moment the list was taken. The last bound is what
/// makes the list safe to act on after the fact: a listed process that ended before it was opened, with
/// its pid given to a newcomer, is found here as that newcomer, created after the list.
fn confirm(
    family: &[Known],
    ghosts: &[Known],
    listed: &[ProcessEntry],
    taken_at: u64,
    own: u32,
    born_of: &mut dyn FnMut(u32) -> Option<u64>,
) -> Vec<Known> {
    let mut vouched: Vec<Known> = family.iter().chain(ghosts).copied().collect();
    let mut confirmed: Vec<Known> = family.to_vec();
    let mut found = Vec::new();
    loop {
        let before = found.len();
        for entry in listed {
            if entry.pid == own || confirmed.iter().any(|k| k.pid == entry.pid) {
                continue;
            }
            // The parent first: asking a process when it was created opens it, and most of the list
            // has nothing to do with the family.
            if !vouched.iter().any(|k| k.pid == entry.parent) {
                continue;
            }
            let Some(born) = born_of(entry.pid).filter(|&b| b <= taken_at) else {
                continue;
            };
            if started_by_family(entry.parent, born, &vouched, &confirmed, listed, born_of) {
                let k = Known { pid: entry.pid, born };
                confirmed.push(k);
                vouched.push(k);
                found.push(k);
            }
        }
        if found.len() == before {
            return found;
        }
    }
}

/// Whether a process that names `parent` as its parent and was created at `born` was started by the
/// family.
///
/// The parent pid has to name a process the family vouches for, created no later than the child. The
/// latest such is the one that held the pid when the child was created - unless a process outside the
/// family took the pid in between, which only the process holding it now can tell: created after that
/// member and no later than the child, it is the child's real parent. A holder that cannot say when it
/// was created cannot rule that out, so the step is not taken.
fn started_by_family(
    parent: u32,
    born: u64,
    vouched: &[Known],
    confirmed: &[Known],
    listed: &[ProcessEntry],
    born_of: &mut dyn FnMut(u32) -> Option<u64>,
) -> bool {
    let Some(since) = vouched.iter().filter(|k| k.pid == parent && k.born <= born).map(|k| k.born).max() else {
        return false;
    };
    if !listed.iter().any(|e| e.pid == parent) {
        // Nobody holds the pid now, so nobody took it between the family's process and this child.
        return true;
    }
    match born_of(parent) {
        None => false,
        Some(holder) => confirmed.contains(&Known { pid: parent, born: holder }) || holder <= since || holder > born,
    }
}

/// The children the hook named without getting into them that the walk did not confirm, still running
/// and created within the session: not before the launched process, not after the list was taken.
///
/// The walk reaches such a child through the process that started it. One it does not reach was started
/// with a parent chosen for it, and the ring holds no creation time to tell it from a process that took
/// its pid after it ended. It is named, and never ended: a wrong name is the worse of the two only for
/// the reader, a wrong end is somebody else's work.
fn named_from_ring(
    ring: &[u32],
    family: &[Known],
    listed: &[ProcessEntry],
    taken_at: u64,
    own: u32,
    born_of: &mut dyn FnMut(u32) -> Option<u64>,
) -> Vec<u32> {
    let floor = family.first().map_or(0, |root| root.born);
    let mut named: Vec<u32> = Vec::new();
    for &pid in ring {
        if pid == own || named.contains(&pid) || family.iter().any(|k| k.pid == pid) {
            continue;
        }
        if !listed.iter().any(|e| e.pid == pid) {
            continue;
        }
        if born_of(pid).is_some_and(|b| floor <= b && b <= taken_at) {
            named.push(pid);
        }
    }
    named
}

/// The processes of the family still running once the ended ones had their time: the launched one and
/// every confirmed one that did not end, plus every confirmed one this core had no right to end and that
/// is still there - one that closed on its own is not left running, whoever could have ended it.
fn still_running(root: HANDLE, family: &[Known], opened: &Opened, images: &HashMap<u32, String>) -> Vec<FamilyMember> {
    let deadline = Instant::now() + EXIT_WAIT;
    let mut left = Vec::new();
    for (i, k) in family.iter().enumerate() {
        let running = if i == 0 {
            waits_out(root, deadline)
        } else {
            match opened.held.get(&k.pid) {
                Some(held) => waits_out(held.handle, if held.can_end { deadline } else { Instant::now() }),
                None => false,
            }
        };
        if running {
            left.push(FamilyMember { pid: k.pid, image: images.get(&k.pid).cloned() });
        }
    }
    left
}

/// Whether the process is still running when `deadline` comes, waiting for it until then.
pub(crate) fn waits_out(handle: HANDLE, deadline: Instant) -> bool {
    let left_ms = deadline.saturating_duration_since(Instant::now()).as_millis().min(u128::from(u32::MAX)) as u32;
    // SAFETY: a process handle this module or its caller holds open until it returns.
    unsafe { WaitForSingleObject(handle, left_ms) == WAIT_TIMEOUT }
}

/// The system time now, as one FILETIME number, to the precision creation times are recorded at.
fn precise_now() -> u64 {
    // SAFETY: no arguments, no failure.
    let ft = unsafe { GetSystemTimePreciseAsFileTime() };
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// A process this module opened: its handle, when it was created, and whether the handle may end it.
struct Held {
    handle: HANDLE,
    born: u64,
    can_end: bool,
}

/// Every process opened while looking for the family, held until the end so that no pid it has seen
/// changes hands in between. A process that would not open is asked again next time, since its pid is
/// not held and may name a newer process by then.
#[derive(Default)]
struct Opened {
    held: HashMap<u32, Held>,
}

/// The live system: the precise clock, the process list, and the processes opened here.
impl Processes for Opened {
    fn now(&mut self) -> u64 {
        precise_now()
    }

    fn list(&mut self) -> Result<Vec<ProcessEntry>, String> {
        process_entries()
    }

    /// When the process under `pid` was created, opening it the first time it is asked about, or `None`
    /// when it cannot be opened or asked.
    fn born_of(&mut self, pid: u32) -> Option<u64> {
        if let Some(held) = self.held.get(&pid) {
            return Some(held.born);
        }
        let held = open(pid)?;
        let born = held.born;
        self.held.insert(pid, held);
        Some(born)
    }

    /// Ask the system to end a process this module holds, when the handle allows it.
    fn end(&mut self, pid: u32) {
        if let Some(held) = self.held.get(&pid).filter(|h| h.can_end) {
            // SAFETY: a handle this module holds, opened with the right to end the process.
            unsafe {
                let _ = TerminateProcess(held.handle, 1);
            }
        }
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        for held in self.held.values() {
            // SAFETY: each handle was opened here and is closed exactly once, here.
            unsafe {
                let _ = CloseHandle(held.handle);
            }
        }
    }
}

/// Open a process to learn when it was created and, when the system allows it, to end it. A process
/// that grants the question but not the end - one running with more rights than this core - is still
/// opened, so it can be confirmed and named.
fn open(pid: u32) -> Option<Held> {
    let ask = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    // SAFETY: plain opens by pid. The handle is closed below when the question fails, or by `Opened`.
    unsafe {
        let (handle, can_end) = match OpenProcess(ask | PROCESS_TERMINATE, false, pid) {
            Ok(h) => (h, true),
            Err(_) => (OpenProcess(ask, false, pid).ok()?, false),
        };
        match process_created(handle) {
            Some(born) => Some(Held { handle, born, can_end }),
            None => {
                let _ = CloseHandle(handle);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(entries: &[(u32, u32)]) -> Vec<ProcessEntry> {
        entries.iter().map(|&(pid, parent)| ProcessEntry { pid, parent, image: format!("p{pid}.exe") }).collect()
    }

    /// Creation times as a made-up system would answer them: `None` for a process that cannot be asked.
    fn births(known: &[(u32, Option<u64>)]) -> impl FnMut(u32) -> Option<u64> {
        let map: HashMap<u32, Option<u64>> = known.iter().copied().collect();
        move |pid| map.get(&pid).copied().flatten()
    }

    const ROOT: Known = Known { pid: 10, born: 100 };
    const OWN: u32 = 1;

    fn pids(found: &[Known]) -> Vec<u32> {
        found.iter().map(|k| k.pid).collect()
    }

    #[test]
    fn the_launched_process_s_tree_is_found_parents_first_and_nothing_beside_it() {
        // 10 -> 30 -> 20, and 40 is started by somebody else.
        let list = listed(&[(20, 30), (30, 10), (40, 99)]);
        let mut born = births(&[(20, Some(120)), (30, Some(110)), (40, Some(105))]);
        let mut family = vec![ROOT];
        let found = round(&mut family, &[], &list, 200, OWN, &mut born);
        assert_eq!(pids(&found), [30, 20], "the tree, the parent before the child");
        assert_eq!(pids(&family), [10, 30, 20]);
    }

    /// A parent pid the system recycled: 50 names the launched process as its parent, but it was created
    /// before it, so its parent was an older process under the same number. Neither it nor anything it
    /// started is the application's.
    #[test]
    fn a_process_older_than_the_parent_it_names_is_not_the_family_s() {
        let list = listed(&[(50, 10), (60, 50)]);
        let mut born = births(&[(50, Some(90)), (60, Some(130))]);
        let found = round(&mut vec![ROOT], &[], &list, 200, OWN, &mut born);
        assert!(found.is_empty(), "{found:?}");
    }

    /// Created after the list was taken: a newcomer under a listed pid, or a real child that the next
    /// round will find. This round leaves it alone either way.
    #[test]
    fn a_process_created_after_the_list_was_taken_waits_for_the_next_round() {
        let list = listed(&[(20, 10)]);
        let found = round(&mut vec![ROOT], &[], &list, 200, OWN, &mut births(&[(20, Some(210))]));
        assert!(found.is_empty(), "{found:?}");
        let found = round(&mut vec![ROOT], &[], &list, 220, OWN, &mut births(&[(20, Some(210))]));
        assert_eq!(pids(&found), [20]);
    }

    /// A member the hook signed in, started with a parent chosen for it, is outside the launched
    /// process's tree. Its slot's creation time vouches for it, and for what it started.
    #[test]
    fn a_registry_member_outside_the_tree_is_found_with_what_it_started() {
        let list = listed(&[(70, 999), (80, 70)]);
        let mut born = births(&[(70, Some(150)), (80, Some(160))]);
        let found = round(&mut vec![ROOT], &[Known { pid: 70, born: 150 }], &list, 200, OWN, &mut born);
        assert_eq!(pids(&found), [70, 80]);
    }

    /// Member 70 ended and its pid went to a stranger created at 170. The stranger and what the stranger
    /// started are not the application's. The child the member started before it ended is.
    #[test]
    fn a_member_s_pid_taken_by_a_stranger_keeps_the_stranger_out_and_the_member_s_orphan_in() {
        let list = listed(&[(70, 999), (81, 70), (82, 70)]);
        let mut born = births(&[(70, Some(170)), (81, Some(160)), (82, Some(180))]);
        let found = round(&mut vec![ROOT], &[Known { pid: 70, born: 150 }], &list, 200, OWN, &mut born);
        assert_eq!(pids(&found), [81]);
    }

    /// The same, but the process holding the pid now cannot say when it was created: nothing rules out
    /// that it took the pid before the child was started, so the child is left alone.
    #[test]
    fn a_holder_that_cannot_say_when_it_was_created_closes_the_step_through_its_pid() {
        let list = listed(&[(70, 999), (81, 70)]);
        let mut born = births(&[(70, None), (81, Some(160))]);
        let found = round(&mut vec![ROOT], &[Known { pid: 70, born: 150 }], &list, 200, OWN, &mut born);
        assert!(found.is_empty(), "{found:?}");
        // A member that ended with nobody under its pid now vouches for its orphan.
        let gone = listed(&[(81, 70)]);
        let mut born = births(&[(81, Some(160))]);
        let found = round(&mut vec![ROOT], &[Known { pid: 70, born: 150 }], &gone, 200, OWN, &mut born);
        assert_eq!(pids(&found), [81]);
    }

    #[test]
    fn this_process_is_never_found() {
        let list = listed(&[(OWN, 10)]);
        let slots = [Known { pid: OWN, born: 150 }];
        let found = round(&mut vec![ROOT], &slots, &list, 200, OWN, &mut births(&[(OWN, Some(150))]));
        assert!(found.is_empty(), "{found:?}");
    }

    /// A process that cannot be asked when it was created cannot be told from a stranger with a recycled
    /// parent pid, so it is not ended - and neither is anything that names it as its parent.
    #[test]
    fn a_process_that_cannot_be_asked_is_left_alone_with_what_it_started() {
        let list = listed(&[(20, 10), (30, 20)]);
        let found = round(&mut vec![ROOT], &[], &list, 200, OWN, &mut births(&[(20, None), (30, Some(120))]));
        assert!(found.is_empty(), "{found:?}");
    }

    /// A slot without a time vouches for nothing, but its process is found through its parent like any
    /// other.
    #[test]
    fn a_slot_without_a_time_is_found_only_through_its_parent() {
        let list = listed(&[(20, 10), (21, 999)]);
        let slots = [Known { pid: 20, born: 0 }, Known { pid: 21, born: 0 }];
        let found = round(&mut vec![ROOT], &slots, &list, 200, OWN, &mut births(&[(20, Some(110)), (21, Some(110))]));
        assert_eq!(pids(&found), [20]);
    }

    /// A made-up system for the rounds: one scripted list per round (a round past the script reads an
    /// empty list), creation times from a table, a clock well past every creation time, and the pids it
    /// was asked to end, in order.
    struct Scripted {
        lists: Vec<Result<Vec<ProcessEntry>, String>>,
        births: HashMap<u32, Option<u64>>,
        clock: u64,
        ended: Vec<u32>,
    }

    impl Scripted {
        fn new(lists: Vec<Result<Vec<ProcessEntry>, String>>, births: &[(u32, Option<u64>)]) -> Self {
            Scripted { lists, births: births.iter().copied().collect(), clock: 1_000, ended: Vec::new() }
        }
    }

    impl Processes for Scripted {
        fn now(&mut self) -> u64 {
            self.clock += 100;
            self.clock
        }
        fn list(&mut self) -> Result<Vec<ProcessEntry>, String> {
            if self.lists.is_empty() { Ok(Vec::new()) } else { self.lists.remove(0) }
        }
        fn born_of(&mut self, pid: u32) -> Option<u64> {
            self.births.get(&pid).copied().flatten()
        }
        fn end(&mut self, pid: u32) {
            self.ended.push(pid);
        }
    }

    /// A list that fails after one that named a ring child keeps the child named, and the search says it
    /// was not complete - the last list it read was the one before the failure.
    #[test]
    fn a_list_that_fails_after_one_that_named_ring_children_keeps_them_named() {
        let mut sys = Scripted::new(
            vec![Ok(listed(&[(20, 10), (90, 555)])), Err("no list".into()), Err("no list".into())],
            &[(20, Some(110)), (90, Some(130))],
        );
        let s = search(&mut sys, ROOT.pid, Some(ROOT.born), &[], &[90], OWN);
        assert_eq!(sys.ended, [20]);
        assert_eq!(s.named, [90], "the ring child named by the list that was read was forgotten");
        assert!(s.incomplete.as_deref().is_some_and(|w| w.contains("could not be read")), "{:?}", s.incomplete);
    }

    /// A list that fails does not end the search: the registry members are ended all the same, the next
    /// round reads the list, and a list that finds nothing new makes the search complete.
    #[test]
    fn a_list_that_fails_and_then_reads_ends_the_registry_members_and_completes() {
        let mut sys = Scripted::new(vec![Err("no list".into()), Ok(listed(&[(70, 999)]))], &[(70, Some(150))]);
        let s = search(&mut sys, ROOT.pid, Some(ROOT.born), &[Known { pid: 70, born: 150 }], &[], OWN);
        assert_eq!(sys.ended, [70]);
        assert_eq!(s.incomplete, None);
    }

    /// Every round finds a process the one before did not: the application was still starting them, so
    /// the latest may be running, and the search says so.
    #[test]
    fn a_family_still_growing_after_the_last_round_is_not_complete() {
        let mut sys = Scripted::new(
            vec![
                Ok(listed(&[(20, 10)])),
                Ok(listed(&[(20, 10), (30, 20)])),
                Ok(listed(&[(20, 10), (30, 20), (40, 30)])),
            ],
            &[(20, Some(110)), (30, Some(120)), (40, Some(130))],
        );
        let s = search(&mut sys, ROOT.pid, Some(ROOT.born), &[], &[], OWN);
        assert_eq!(sys.ended, [20, 30, 40]);
        assert!(s.incomplete.as_deref().is_some_and(|w| w.contains("still starting")), "{:?}", s.incomplete);
    }

    /// A list that failed once and then read gives the reason the search really ended on: the family was
    /// still growing, not a list that could not be read.
    #[test]
    fn a_failure_followed_by_lists_that_read_is_not_given_as_the_reason() {
        let mut sys = Scripted::new(
            vec![Err("no list".into()), Ok(listed(&[(20, 10)])), Ok(listed(&[(20, 10), (30, 20)]))],
            &[(20, Some(110)), (30, Some(120))],
        );
        let s = search(&mut sys, ROOT.pid, Some(ROOT.born), &[], &[], OWN);
        assert_eq!(sys.ended, [20, 30]);
        assert!(s.incomplete.as_deref().is_some_and(|w| w.contains("still starting")), "{:?}", s.incomplete);
    }

    /// A launched process that would not say when it was created vouches for nothing - with 0 it would
    /// have vouched for every older process naming its pid - so its children are not looked for, and the
    /// search says so.
    #[test]
    fn a_launched_process_that_will_not_say_when_it_was_created_vouches_for_nothing() {
        let mut sys = Scripted::new(vec![Ok(listed(&[(20, 10), (90, 555)]))], &[(20, Some(110)), (90, Some(130))]);
        let s = search(&mut sys, ROOT.pid, None, &[], &[90], OWN);
        assert!(sys.ended.is_empty(), "{:?}", sys.ended);
        assert!(s.named.is_empty(), "{:?}", s.named);
        assert!(s.incomplete.as_deref().is_some_and(|w| w.contains("would not say")), "{:?}", s.incomplete);
    }

    /// The control: a first list with nothing of the family in it is a complete search.
    #[test]
    fn a_first_list_with_nothing_new_is_a_complete_search() {
        let mut sys = Scripted::new(vec![Ok(listed(&[(40, 99)]))], &[(40, Some(105))]);
        let s = search(&mut sys, ROOT.pid, Some(ROOT.born), &[], &[], OWN);
        assert!(sys.ended.is_empty());
        assert_eq!(s.incomplete, None);
    }

    /// A child the hook named that the walk does not reach is named, never ended - only while it runs and
    /// only when it was created within the session.
    #[test]
    fn a_ring_child_outside_the_tree_is_named_while_it_runs_within_the_session() {
        let list = listed(&[(90, 555), (91, 555), (20, 10)]);
        let mut born = births(&[(90, Some(130)), (91, Some(50)), (20, Some(110)), (92, Some(130))]);
        let family = [ROOT, Known { pid: 20, born: 110 }];
        // 90 is named, 91 was created before the launched process, 20 is the walk's, 92 is not running,
        // and the second 90 is the same child.
        let named = named_from_ring(&[90, 91, 20, 92, 90, OWN], &family, &list, 200, OWN, &mut born);
        assert_eq!(named, [90]);
    }
}
