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
    Ok(descendants(root, &edges))
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
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|e| format!("CreateToolhelp32Snapshot failed: {e}"))?;
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
                    Err(format!("the process snapshot ended with error {}", error.0))
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
fn text_up_to_nul(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// The root and everything under it, each pid once. Pure over the edge list, so the walk is tested
/// on a made-up tree. The edges are indexed by parent first, so a snapshot of a few hundred
/// processes read once a second costs one pass over it, not one pass per family member. A parent
/// pid the system has recycled can point a stray process at the root - the snapshot cannot tell,
/// and neither can this, which is why a caller that has a hook registry prefers it.
fn descendants(root: u32, edges: &[(u32, u32)]) -> Vec<u32> {
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
            if seen.insert(pid) {
                family.push(pid);
                frontier.push(pid);
            }
        }
    }
    family
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_family_is_the_root_and_everything_under_it_and_nothing_beside_it() {
        // 1 -> 2 -> 4, 1 -> 3, and 9 -> 10 is another tree entirely.
        let edges = [(2, 1), (3, 1), (4, 2), (10, 9), (1, 0)];
        let mut family = descendants(1, &edges);
        family.sort_unstable();
        assert_eq!(family, [1, 2, 3, 4]);
        assert_eq!(descendants(4, &edges), [4]);
    }

    #[test]
    fn a_cycle_or_a_self_parent_in_the_snapshot_cannot_loop_the_walk() {
        // pid 0 is its own parent in a real snapshot, and a recycled parent pid can close a cycle.
        let edges = [(0, 0), (5, 6), (6, 5)];
        assert_eq!(descendants(0, &edges), [0]);
        let mut family = descendants(5, &edges);
        family.sort_unstable();
        assert_eq!(family, [5, 6]);
    }

    #[test]
    fn the_live_snapshot_lists_this_process() {
        let me = std::process::id();
        let family = family_of(me).expect("the snapshot is readable");
        assert_eq!(family[0], me);
    }
}
