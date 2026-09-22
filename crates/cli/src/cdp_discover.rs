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

/// A DevTools endpoint found in the family: the pid that holds it, the loopback host and port to
/// connect to (`::1` for an IPv6 socket, `127.0.0.1` otherwise), and what the engine calls itself in
/// `/json/version` (cleaned, for the report).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Discovered {
    pub(crate) pid: u32,
    pub(crate) host: &'static str,
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
    /// The port reserved for a Qt engine is held by something that is not a DevTools endpoint: a
    /// process outside the family bound it first, or one inside the family that never answered as
    /// DevTools (the application's own server landing on the same ephemeral port). Said once. The
    /// engine could not have bound it, so its pages stay unreached and the caller says why.
    PortTaken(u16),
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
    /// Start the thread with an initial family and, when a port was reserved for a Qt engine, that
    /// port - so the thread can say when something else took it. A thread that cannot be started
    /// (handles or memory exhausted) is the caller's to report - the session goes on without the
    /// channel, which is what `Notice::Unavailable` promises for the table, and a panic here would
    /// end it instead.
    pub(crate) fn start(family: Vec<u32>, reserved: Option<u16>) -> std::io::Result<Discovery> {
        let (pid_tx, pid_rx) = mpsc::channel::<Vec<u32>>();
        let (notice_tx, notice_rx) = mpsc::channel::<Notice>();
        let _ = pid_tx.send(family);
        thread::Builder::new()
            .name("chrono-discover".into())
            .spawn(move || run(&pid_rx, &notice_tx, reserved))?;
        Ok(Discovery { pids: pid_tx, notices: notice_rx })
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
fn run(pids: &Receiver<Vec<u32>>, notices: &Sender<Notice>, reserved: Option<u16>) {
    let mut family: HashSet<u32> = HashSet::new();
    let mut memory = Memory::default();
    let mut taken_said = false;
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
        if let Some(port) = reserved
            && !taken_said
            && reserved_port_taken(&table, &family, port, &memory)
        {
            taken_said = true;
            if notices.send(Notice::PortTaken(port)).is_err() {
                return;
            }
        }
        let candidates = memory.due(candidates(&table, &family), now);
        for candidate in candidates {
            let host = candidate.loopback_host();
            match probe(host, candidate.port) {
                Some(browser) => {
                    memory.found(candidate);
                    let found = Discovered { pid: candidate.pid, host, port: candidate.port, browser };
                    if notices.send(Notice::Found(found)).is_err() {
                        return;
                    }
                }
                None => memory.refused(candidate, now),
            }
        }
        thread::sleep(SWEEP);
    }
}

/// The loopback listeners owned by the family, whichever address family they are on. Pure over the
/// table, so the join is tested on a made-up machine.
fn candidates(table: &[chrono_mech::Listener], family: &HashSet<u32>) -> Vec<chrono_mech::Listener> {
    table.iter().filter(|l| l.loopback && family.contains(&l.pid)).copied().collect()
}

/// Whether the port reserved for a Qt engine is held by something that is not its DevTools endpoint:
/// a listener on it outside the family, or one inside the family whose retries as DevTools are spent.
/// Pure over the table and the memory, so both shapes are tested without a socket.
fn reserved_port_taken(
    table: &[chrono_mech::Listener],
    family: &HashSet<u32>,
    port: u16,
    memory: &Memory,
) -> bool {
    table
        .iter()
        .filter(|l| l.port == port)
        .any(|l| !family.contains(&l.pid) || memory.spent(l))
}

/// Ask a port on a loopback host whether it is a DevTools endpoint. The browser name is the
/// engine's own text, cleaned on the way to the report.
fn probe(host: &str, port: u16) -> Option<String> {
    let reply = cdp::http_get_json(host, port, "/json/version").ok()?;
    is_devtools_version(&reply).then(|| {
        cdp::sanitise_target_text(reply.get("Browser").and_then(serde_json::Value::as_str).unwrap_or(""))
    })
}

/// The identity of one listener in memory: pid, port and address family - the same port on both
/// families is two sockets, and each answers for itself.
type Key = (u32, u16, bool);

fn key(l: &chrono_mech::Listener) -> Key {
    (l.pid, l.port, l.v6)
}

/// What each listener has already said, so a found endpoint is reported once, a refusing one is
/// asked again a bounded number of times, and a listener that left the table is forgotten.
#[derive(Default)]
struct Memory {
    found: HashSet<Key>,
    refused: HashMap<Key, (u32, Instant)>,
}

impl Memory {
    /// The listeners worth asking now: never the found ones, refused ones only when their gap has
    /// passed and their retries are not spent. Listeners no longer in the table drop out of memory.
    fn due(&mut self, present: Vec<chrono_mech::Listener>, now: Instant) -> Vec<chrono_mech::Listener> {
        let live: HashSet<Key> = present.iter().map(key).collect();
        self.found.retain(|k| live.contains(k));
        self.refused.retain(|k, _| live.contains(k));
        present
            .into_iter()
            .filter(|l| !self.found.contains(&key(l)))
            .filter(|l| match self.refused.get(&key(l)) {
                None => true,
                Some((attempts, last)) => *attempts <= RETRIES && now.duration_since(*last) >= RETRY_GAP,
            })
            .collect()
    }

    fn found(&mut self, l: chrono_mech::Listener) {
        self.refused.remove(&key(&l));
        self.found.insert(key(&l));
    }

    fn refused(&mut self, l: chrono_mech::Listener, now: Instant) {
        let entry = self.refused.entry(key(&l)).or_insert((0, now));
        entry.0 += 1;
        entry.1 = now;
    }

    /// Whether a listener has been asked every time it will be and never answered as DevTools.
    fn spent(&self, l: &chrono_mech::Listener) -> bool {
        self.refused.get(&key(l)).is_some_and(|(attempts, _)| *attempts > RETRIES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_mech::Listener;

    fn listener(pid: u32, port: u16, loopback: bool) -> Listener {
        Listener { pid, port, loopback, v6: false }
    }

    fn listener6(pid: u32, port: u16) -> Listener {
        Listener { pid, port, loopback: true, v6: true }
    }

    #[test]
    fn only_loopback_listeners_of_the_family_are_candidates_on_either_family() {
        let table = [
            listener(10, 9222, true),
            listener(10, 8080, false),
            listener(99, 9333, true),
            listener(11, 9444, true),
            listener6(11, 9555),
        ];
        let family: HashSet<u32> = [10, 11].into_iter().collect();
        let found = candidates(&table, &family);
        assert_eq!(found, vec![listener(10, 9222, true), listener(11, 9444, true), listener6(11, 9555)]);
        assert_eq!(found[2].loopback_host(), "::1");
        assert!(candidates(&table, &HashSet::new()).is_empty());
    }

    /// The port reserved for a Qt engine is taken when something outside the family listens on it,
    /// or when a family process on it never answered as DevTools in all its retries - either way the
    /// engine could not have bound it (docs/09 section 12.17 point 4). A family listener still being
    /// asked is not taken yet.
    #[test]
    fn the_reserved_port_is_taken_by_a_stranger_or_by_a_family_socket_that_is_not_devtools() {
        let family: HashSet<u32> = [10].into_iter().collect();
        let mut memory = Memory::default();
        let t0 = Instant::now();

        let stranger = [listener(99, 40000, true)];
        assert!(reserved_port_taken(&stranger, &family, 40000, &memory));
        assert!(!reserved_port_taken(&stranger, &family, 40001, &memory), "another port is not ours");

        let own = listener(10, 40000, true);
        assert!(!reserved_port_taken(&[own], &family, 40000, &memory), "still being asked");
        for n in 0..=RETRIES {
            memory.refused(own, t0 + RETRY_GAP * n);
        }
        assert!(reserved_port_taken(&[own], &family, 40000, &memory), "retries spent, never DevTools");
    }

    #[test]
    fn a_found_endpoint_is_asked_once_and_a_refusing_one_a_bounded_number_of_times() {
        let mut memory = Memory::default();
        let t0 = Instant::now();
        let one = listener(10, 9222, true);

        assert_eq!(memory.due(vec![one], t0), vec![one]);
        memory.found(one);
        assert!(memory.due(vec![one], t0).is_empty(), "a found endpoint is not asked again");

        let refusing = listener(10, 8080, true);
        memory.refused(refusing, t0);
        assert!(memory.due(vec![refusing], t0).is_empty(), "not before the gap");
        let later = t0 + RETRY_GAP;
        assert_eq!(memory.due(vec![refusing], later), vec![refusing], "asked again after the gap");
        for n in 1..=RETRIES {
            memory.refused(refusing, later + RETRY_GAP * n);
        }
        let much_later = later + RETRY_GAP * (RETRIES + 2);
        assert!(memory.due(vec![refusing], much_later).is_empty(), "retries are spent");
    }

    #[test]
    fn the_same_port_on_the_other_family_is_another_listener() {
        let mut memory = Memory::default();
        let t0 = Instant::now();
        let v4 = listener(10, 9222, true);
        let v6 = listener6(10, 9222);
        memory.found(v4);
        assert_eq!(memory.due(vec![v4, v6], t0), vec![v6]);
    }

    #[test]
    fn a_listener_that_left_the_table_is_forgotten_and_asked_afresh_when_it_returns() {
        let mut memory = Memory::default();
        let t0 = Instant::now();
        let one = listener(10, 9222, true);
        memory.found(one);
        assert!(memory.due(vec![], t0).is_empty());
        // Gone from the table for one sweep, then back: a new engine on a reused port.
        assert_eq!(memory.due(vec![one], t0), vec![one]);
    }

    #[test]
    fn a_port_that_never_answers_is_no_endpoint_on_either_family() {
        // A port nobody listens on: the connection is refused at once, so this is a fast no.
        let free = cdp::free_loopback_port().expect("a free port");
        assert_eq!(probe("127.0.0.1", free), None);
        assert_eq!(probe("::1", free), None);
    }
}
