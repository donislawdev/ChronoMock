//! Discovery: the DevTools port an embedded web engine opened somewhere in the session's family.
//!
//! The engine was told to open one (the variables in `embedded::engine_env`), but not where, and
//! not when - a WebView2 host creates its browser process when it first shows a page, which can be
//! a minute after the host started, and a Qt host opens the port inside itself. So this is a thread
//! that keeps looking, in the spirit of ADR-10: every second it reads the machine's listening
//! sockets (mech), keeps the loopback ones owned by a pid in the family, and asks each new one
//! whether it is a DevTools endpoint (`/json/version` names a browser WebSocket URL). A yes goes to
//! the caller as [`Discovered`]. A no is remembered, with a few retries spaced out - an endpoint
//! bound a moment ago may not answer HTTP yet - and forgotten once the socket leaves the table, so a
//! port reused later starts fresh.
//!
//! Its own thread, because the HTTP probe has a ten-second read timeout and the application's own
//! HTTP server (a listener in the family that is not an engine) would otherwise stall the session
//! loop for that long. The family is the caller's to define - a hooked session has its registry, a
//! probe without the hook walks the process tree - and it is pushed here whenever it changes.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::cdp;
use crate::embedded::is_devtools_version;

/// A DevTools endpoint found in the family: the pid that holds it, its port, and what the engine
/// calls itself in `/json/version` (cleaned, for the report).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Discovered {
    pub(crate) pid: u32,
    pub(crate) port: u16,
    pub(crate) browser: String,
}

/// What the discovery thread has to say besides a find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Notice {
    Found(Discovered),
    /// The table could not be read at all - said once, then the thread ends. The session goes on
    /// without the channel, and the caller reports the loss (untouchable rule 6).
    Unavailable(String),
}

/// How often the table is read.
const SWEEP: Duration = Duration::from_secs(1);
/// How many times a listener that did not answer as DevTools is asked again, and how far apart.
/// WebView2 binds its port about three quarters of a second before it serves HTTP on it.
const RETRIES: u32 = 3;
const RETRY_GAP: Duration = Duration::from_secs(2);

/// The caller's end: push the family's pids as they change, pull notices as they come. Dropping it
/// ends the thread.
pub(crate) struct Discovery {
    pids: Sender<Vec<u32>>,
    notices: Receiver<Notice>,
}

impl Discovery {
    /// Start the thread with an initial family.
    pub(crate) fn start(family: Vec<u32>) -> Discovery {
        let (pid_tx, pid_rx) = mpsc::channel::<Vec<u32>>();
        let (notice_tx, notice_rx) = mpsc::channel::<Notice>();
        let _ = pid_tx.send(family);
        thread::Builder::new()
            .name("chrono-discover".into())
            .spawn(move || run(&pid_rx, &notice_tx))
            .expect("the discovery thread starts");
        Discovery { pids: pid_tx, notices: notice_rx }
    }

    /// The family as of now. The thread uses the latest set it has received.
    pub(crate) fn update_family(&self, family: Vec<u32>) {
        let _ = self.pids.send(family);
    }

    /// The next notice, if one is waiting. Never blocks.
    pub(crate) fn try_recv(&self) -> Option<Notice> {
        self.notices.try_recv().ok()
    }
}

/// The thread body: sweep, probe what is new, report, sleep, until the caller is gone.
fn run(pids: &Receiver<Vec<u32>>, notices: &Sender<Notice>) {
    let mut family: HashSet<u32> = HashSet::new();
    let mut memory = Memory::default();
    loop {
        // The latest family wins, and a closed channel means the caller dropped its handle.
        loop {
            match pids.try_recv() {
                Ok(set) => family = set.into_iter().collect(),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let table = match chrono_mech::listening_sockets() {
            Ok(t) => t,
            Err(e) => {
                let _ = notices.send(Notice::Unavailable(e));
                return;
            }
        };
        let now = Instant::now();
        let candidates = memory.due(candidates(&table, &family), now);
        for (pid, port) in candidates {
            match probe(port) {
                Some(browser) => {
                    memory.found(pid, port);
                    if notices.send(Notice::Found(Discovered { pid, port, browser })).is_err() {
                        return;
                    }
                }
                None => memory.refused(pid, port, now),
            }
        }
        thread::sleep(SWEEP);
    }
}

/// The loopback listeners owned by the family, as `(pid, port)` pairs. Pure over the table, so the
/// join is tested on a made-up machine.
fn candidates(table: &[chrono_mech::Listener], family: &HashSet<u32>) -> Vec<(u32, u16)> {
    table
        .iter()
        .filter(|l| l.loopback && family.contains(&l.pid))
        .map(|l| (l.pid, l.port))
        .collect()
}

/// Ask a port whether it is a DevTools endpoint. The browser name is the engine's own text, cleaned
/// on the way to the report.
fn probe(port: u16) -> Option<String> {
    let reply = cdp::http_get_json("127.0.0.1", port, "/json/version").ok()?;
    is_devtools_version(&reply).then(|| {
        cdp::sanitise_target_text(reply.get("Browser").and_then(serde_json::Value::as_str).unwrap_or(""))
    })
}

/// What each `(pid, port)` pair has already said, so a found endpoint is reported once, a refusing
/// one is asked again a bounded number of times, and a pair that left the table is forgotten.
#[derive(Default)]
struct Memory {
    found: HashSet<(u32, u16)>,
    refused: HashMap<(u32, u16), (u32, Instant)>,
}

impl Memory {
    /// The pairs worth asking now: never the found ones, refused ones only when their gap has
    /// passed and their retries are not spent. Pairs no longer in the table drop out of memory.
    fn due(&mut self, present: Vec<(u32, u16)>, now: Instant) -> Vec<(u32, u16)> {
        let live: HashSet<(u32, u16)> = present.iter().copied().collect();
        self.found.retain(|pair| live.contains(pair));
        self.refused.retain(|pair, _| live.contains(pair));
        present
            .into_iter()
            .filter(|pair| !self.found.contains(pair))
            .filter(|pair| match self.refused.get(pair) {
                None => true,
                Some((attempts, last)) => *attempts <= RETRIES && now.duration_since(*last) >= RETRY_GAP,
            })
            .collect()
    }

    fn found(&mut self, pid: u32, port: u16) {
        self.refused.remove(&(pid, port));
        self.found.insert((pid, port));
    }

    fn refused(&mut self, pid: u32, port: u16, now: Instant) {
        let entry = self.refused.entry((pid, port)).or_insert((0, now));
        entry.0 += 1;
        entry.1 = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_mech::Listener;

    fn listener(pid: u32, port: u16, loopback: bool) -> Listener {
        Listener { pid, port, loopback }
    }

    #[test]
    fn only_loopback_listeners_of_the_family_are_candidates() {
        let table = [
            listener(10, 9222, true),
            listener(10, 8080, false),
            listener(99, 9333, true),
            listener(11, 9444, true),
        ];
        let family: HashSet<u32> = [10, 11].into_iter().collect();
        assert_eq!(candidates(&table, &family), vec![(10, 9222), (11, 9444)]);
        assert!(candidates(&table, &HashSet::new()).is_empty());
    }

    #[test]
    fn a_found_endpoint_is_asked_once_and_a_refusing_one_a_bounded_number_of_times() {
        let mut memory = Memory::default();
        let t0 = Instant::now();
        let pair = (10, 9222);

        assert_eq!(memory.due(vec![pair], t0), vec![pair]);
        memory.found(10, 9222);
        assert!(memory.due(vec![pair], t0).is_empty(), "a found endpoint is not asked again");

        let refusing = (10, 8080);
        memory.refused(10, 8080, t0);
        assert!(memory.due(vec![refusing], t0).is_empty(), "not before the gap");
        let later = t0 + RETRY_GAP;
        assert_eq!(memory.due(vec![refusing], later), vec![refusing], "asked again after the gap");
        for n in 1..=RETRIES {
            memory.refused(10, 8080, later + RETRY_GAP * n);
        }
        let much_later = later + RETRY_GAP * (RETRIES + 2);
        assert!(memory.due(vec![refusing], much_later).is_empty(), "retries are spent");
    }

    #[test]
    fn a_pair_that_left_the_table_is_forgotten_and_asked_afresh_when_it_returns() {
        let mut memory = Memory::default();
        let t0 = Instant::now();
        memory.found(10, 9222);
        assert!(memory.due(vec![], t0).is_empty());
        // Gone from the table for one sweep, then back: a new engine on a reused port.
        assert_eq!(memory.due(vec![(10, 9222)], t0), vec![(10, 9222)]);
    }

    #[test]
    fn a_port_that_never_answers_is_no_endpoint() {
        // A port nobody listens on: the connection is refused at once, so this is a fast no.
        let free = cdp::free_loopback_port().expect("a free port");
        assert_eq!(probe(free), None);
    }
}
