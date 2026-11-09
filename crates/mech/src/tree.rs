//! The process tree under a root pid, from a system snapshot.
//!
//! A session under the hook knows its family from the registry every hooked process signs into,
//! plus the ring of children the hook never got into. A probe that launches a host WITHOUT the hook
//! has neither, and the embedded-engine channel still needs to know which processes are the host's:
//! the DevTools port belongs to the family, and a port owned by anybody else is not ours to speak
//! to. `CreateToolhelp32Snapshot` is the one system-wide list of processes with parent pids, and
//! walking it from the root down is the family.
//!
//! Read every time it is asked, never cached: an engine spawns its processes seconds after the
//! host starts, and a snapshot from before then would say the family is the host alone.

use std::collections::{HashMap, HashSet};

use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_NO_MORE_FILES};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// The root pid and every process that descends from it, at this moment. An error is the
/// snapshot's own word, so the caller can say the family could not be read rather than pretend it
/// is the root alone.
pub fn family_of(root: u32) -> Result<Vec<u32>, String> {
    let edges: Vec<(u32, u32)> = process_entries()?.iter().map(|e| (e.pid, e.parent)).collect();
    Ok(descendants(root, &edges, |_| true))
}

/// Everything under `root` as (pid, declared parent), the root itself left out, from one snapshot.
/// For an elevated WebView2 host the engine's browser process is started by a system service and
/// only DECLARES the host as its parent, so the hook never sees it start and the tree is the one place
/// it shows (docs/09 section 12.19). A process older than the root cannot be its descendant, so one
/// that the snapshot attaches to a recycled parent pid is left out by its creation time, and so is
/// everything under it: what it started is not the root's either. A process whose creation time cannot
/// be read is kept, because naming an extra process is the safer error.
pub fn descendants_of(root: u32) -> Result<Vec<(u32, u32)>, String> {
    let entries = process_entries()?;
    let edges: Vec<(u32, u32)> = entries.iter().map(|e| (e.pid, e.parent)).collect();
    let parents: HashMap<u32, u32> = edges.iter().copied().collect();
    let root_created = created_of(root);
    Ok(descendants(root, &edges, |pid| not_older_than(created_of(pid), root_created))
        .into_iter()
        .filter(|&pid| pid != root)
        .map(|pid| (pid, parents.get(&pid).copied().unwrap_or(root)))
        .collect())
}

/// When a process was created, or `None` when it cannot be asked (it has gone, or does not allow the
/// question).
fn created_of(pid: u32) -> Option<u64> {
    // SAFETY: the handle is closed on every path out, and nothing is borrowed from the caller.
    unsafe {
        let handle = windows::Win32::System::Threading::OpenProcess(
            windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        )
        .ok()?;
        let created = crate::process_created(handle);
        let _ = CloseHandle(handle);
        created
    }
}

/// Whether a child can be a descendant of a root given when each was created. Unknown on either side
/// keeps the child. Pure, so the three cases are tested without processes.
fn not_older_than(child: Option<u64>, root: Option<u64>) -> bool {
    match (child, root) {
        (Some(child), Some(root)) => child >= root,
        _ => true,
    }
}

/// One process as the snapshot lists it: its pid, the pid of the process that started it, and the
/// file name of its executable.
pub(crate) struct ProcessEntry {
    pub(crate) pid: u32,
    pub(crate) parent: u32,
    pub(crate) image: String,
}

/// Every process the snapshot holds. The walk ends only on the one error that means "no more
/// entries" - any other failure is reported, because a list cut short would be handed on as a
/// family with members missing, and a family missing the process that holds the port is a search
/// that quietly finds nothing.
pub(crate) fn process_entries() -> Result<Vec<ProcessEntry>, String> {
    // SAFETY: the snapshot handle is closed on every path out, and the entry structure carries its
    // own size as the API requires. The last error is read right after the failing call, before
    // anything else can overwrite it.
    unsafe {
        // The code, not the system's sentence: `{e}` printed Windows's text in the system's language on
        // an English-only command line (R5-D3, as `win32_detail` says for the rest).
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|e| crate::win32_detail("CreateToolhelp32Snapshot", &e))?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut entries = Vec::new();
        let mut step = Process32FirstW(snapshot, &mut entry);
        let outcome = loop {
            if step.is_err() {
                let error = GetLastError();
                break if error == ERROR_NO_MORE_FILES {
                    Ok(())
                } else {
                    Err(crate::win32_detail(
                        "reading the process snapshot",
                        &windows::core::Error::from_hresult(error.to_hresult()),
                    ))
                };
            }
            entries.push(ProcessEntry {
                pid: entry.th32ProcessID,
                parent: entry.th32ParentProcessID,
                image: text_up_to_nul(&entry.szExeFile),
            });
            step = Process32NextW(snapshot, &mut entry);
        };
        let _ = CloseHandle(snapshot);
        outcome.map(|()| entries)
    }
}

/// A fixed-size UTF-16 buffer as the text before its first zero.
pub(crate) fn text_up_to_nul(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// The root and everything under it, each pid once. Pure over the edge list, so the walk is tested
/// on a made-up tree. The edges are indexed by parent first, so a snapshot of a few hundred
/// processes read once a second costs one pass over it, not one pass per family member. A parent
/// pid the system has recycled can point a stray process at the root - the snapshot cannot tell,
/// and neither can this, which is why a caller that has a hook registry prefers it. `keep` is asked of
/// each process on the way down: one it refuses is not entered, so nothing under it is either.
fn descendants(root: u32, edges: &[(u32, u32)], keep: impl Fn(u32) -> bool) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, ppid) in edges {
        if pid != ppid {
            children.entry(ppid).or_default().push(pid);
        }
    }
    let mut family = vec![root];
    let mut seen: HashSet<u32> = HashSet::from([root]);
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for &pid in children.get(&parent).map(Vec::as_slice).unwrap_or_default() {
            if seen.insert(pid) && keep(pid) {
                family.push(pid);
                frontier.push(pid);
            }
        }
    }
    family
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The pid of a process named `image` that `parent` started, looked for in the process list for up
    /// to five seconds - a parent can be told it started a child before the list shows the child. Shared
    /// by the tests of the crate that start a tree and need its second level.
    pub(crate) fn started_by(parent: u32, image: &str) -> Option<u32> {
        (0..50).find_map(|_| {
            let found = process_entries()
                .ok()
                .and_then(|entries| entries.into_iter().find(|e| e.parent == parent && e.image.eq_ignore_ascii_case(image)))
                .map(|e| e.pid);
            if found.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            found
        })
    }

    #[test]
    fn the_family_is_the_root_and_everything_under_it_and_nothing_beside_it() {
        // 1 -> 2 -> 4, 1 -> 3, and 9 -> 10 is another tree entirely.
        let edges = [(2, 1), (3, 1), (4, 2), (10, 9), (1, 0)];
        let mut family = descendants(1, &edges, |_| true);
        family.sort_unstable();
        assert_eq!(family, [1, 2, 3, 4]);
        assert_eq!(descendants(4, &edges, |_| true), [4]);
    }

    /// A process the walk refuses is not entered, so what it started is not a descendant either. Filtering
    /// the finished list instead would keep 4 and 5: they are not older than the root, and they are
    /// reached only through 2, which is not the root's.
    #[test]
    fn nothing_under_a_process_the_walk_refuses_is_a_descendant() {
        // 1 -> 2 -> 4, 2 -> 5, 1 -> 3, and 2 stands for a process older than the root that declares the
        // root's recycled pid as its parent.
        let edges = [(2, 1), (3, 1), (4, 2), (5, 2), (1, 0)];
        let mut family = descendants(1, &edges, |pid| pid != 2);
        family.sort_unstable();
        assert_eq!(family, [1, 3]);
    }

    #[test]
    fn a_cycle_or_a_self_parent_in_the_snapshot_cannot_loop_the_walk() {
        // pid 0 is its own parent in a real snapshot, and a recycled parent pid can close a cycle.
        let edges = [(0, 0), (5, 6), (6, 5)];
        assert_eq!(descendants(0, &edges, |_| true), [0]);
        let mut family = descendants(5, &edges, |_| true);
        family.sort_unstable();
        assert_eq!(family, [5, 6]);
    }

    #[test]
    fn the_live_snapshot_lists_this_process() {
        let me = std::process::id();
        let family = family_of(me).expect("the snapshot is readable");
        assert_eq!(family[0], me);
    }

    /// A child is never older than its root: the creation time drops a process the snapshot attached
    /// to a recycled parent pid, and an unreadable time on either side keeps the child.
    #[test]
    fn a_child_older_than_its_root_is_not_a_descendant() {
        assert!(not_older_than(Some(10), Some(10)), "created at the same instant counts");
        assert!(not_older_than(Some(11), Some(10)));
        assert!(!not_older_than(Some(9), Some(10)));
        assert!(not_older_than(None, Some(10)), "unknown keeps it");
        assert!(not_older_than(Some(9), None), "unknown keeps it");
        assert!(not_older_than(None, None));
    }

    /// The live walk, on a real child: a process started here is under this one, the root is not in
    /// its own list, and the declared parent is the root for a direct child.
    #[test]
    fn a_process_started_by_this_one_is_found_under_it() {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let path = format!(r"{system_root}\System32\cmd.exe");
        let ping = format!(r"{system_root}\System32\PING.EXE");
        let args = ["/c".to_string(), ping, "-n".into(), "30".into(), "127.0.0.1".into()];
        let target = crate::Target { path: &path, args: &args, cwd: None, env: &[], stdio: crate::TargetStdio::Discarded };
        let child = crate::launch_plain(&target).expect("cmd.exe launches");
        let me = std::process::id();

        // The grandchild, once cmd.exe has started it: found under this process with cmd.exe as its parent.
        let found = (0..50).find_map(|_| {
            let under = descendants_of(me).ok()?;
            let grandchild = under.iter().find(|&&(_, parent)| parent == child.pid).copied();
            if grandchild.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            grandchild.map(|g| (under, g))
        });
        let (under, grandchild) = found.expect("the snapshot shows cmd.exe's own child within five seconds");
        assert!(under.iter().all(|&(pid, _)| pid != me), "the root is not its own descendant");
        assert!(under.contains(&(child.pid, me)), "the child has this process as its parent: {under:?}");
        assert_eq!(grandchild.1, child.pid);
    }
}
