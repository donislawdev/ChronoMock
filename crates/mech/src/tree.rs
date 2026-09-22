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

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// The root pid and every process that descends from it, at this moment. An error is the
/// snapshot's own word, so the caller can say the family could not be read rather than pretend it
/// is the root alone.
pub fn family_of(root: u32) -> Result<Vec<u32>, String> {
    let edges = parent_edges()?;
    Ok(descendants(root, &edges))
}

/// Every `(pid, parent pid)` pair the snapshot holds.
fn parent_edges() -> Result<Vec<(u32, u32)>, String> {
    // SAFETY: the snapshot handle is closed on every path out, and the entry structure carries its
    // own size as the API requires. The walk stops at the first `Process32NextW` failure, which is
    // the documented end of the list.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|e| format!("CreateToolhelp32Snapshot failed: {e}"))?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut edges = Vec::new();
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                edges.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
        Ok(edges)
    }
}

/// The root and everything under it, breadth first, each pid once. Pure over the edge list, so the
/// walk is tested on a made-up tree. A parent pid the system has recycled can point a stray process
/// at the root - the snapshot cannot tell, and neither can this, which is why a caller that has a
/// hook registry prefers it.
fn descendants(root: u32, edges: &[(u32, u32)]) -> Vec<u32> {
    let mut family = vec![root];
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for &(pid, ppid) in edges {
            if ppid == parent && pid != parent && !family.contains(&pid) {
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
