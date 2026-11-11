//! What the session said, gathered from the event stream and nothing else.
//!
//! The driver decides what happens next: when to change speed, when to jump, when to end. This side
//! only records what already happened, which is why it can be tested without starting a process at
//! all - feed it events, read the report. Until it was pulled out, every rule in here (the latest
//! snapshot for a process wins, counts are never summed across processes, warnings are one union)
//! was reachable only through a live run.

use std::collections::HashMap;

use chrono_proto::Event;

use crate::report::{CutShort, ProcessCoverage, SessionReport, UnitId};

/// The session's evidence as it arrives, one field per thing the report states. Nothing here is
/// derived twice, so the report cannot claim more than the core actually said (untouchable rule 4).
#[derive(Default)]
pub(super) struct Collector {
    verdict_line: Option<(String, String)>,  // parent (start) verdict, fallback
    session_line: Option<(String, String, u32)>,  // family: (verdict, reason_key, count)
    vanished: Option<(String, u64)>,  // (reason_key, lived_ms)
    errors: Vec<(String, String)>,  // (key, origin) - why a session did not start
    warnings: Vec<String>,
    // Children the family spawned without the hook, from `session_verdict` (SLOWNIK `uncoveredChild`).
    uncovered_children: Vec<chrono_proto::UncoveredChild>,
    uncovered_children_total: u32,
    // Coverage per process, LATEST snapshot wins. The core emits one event per process as it is
    // discovered and a final one for every process at the end, because the first is sampled inside
    // the guard window and its call counts never move again (R2-X8). Appending every event instead
    // would print each channel twice, once with a number from the session's first blink.
    cov_by_unit: HashMap<UnitId, ProcessCoverage>,
    unit_order: Vec<UnitId>,  // first-seen order, so the parent still leads the report
    // JS contexts covered beside the processes, and the engines they were reached through - both
    // from `session_verdict` (docs/09).
    context_count: u32,
    engines: Vec<chrono_proto::ReachedEngine>,
    // The processes the session went on for after the target closed, from `session_verdict` (ADR-16).
    followed: Vec<chrono_proto::FollowedProcess>,
    // The copies outside the session the target most likely handed its work to, from `session_verdict`
    // (ADR-4, 2026-11-11).
    copies_outside: Vec<chrono_proto::FollowedProcess>,
    // How long the machine slept during the session, from `session_verdict` (R5-P1).
    host_slept_ms: u64,
    // What the browser of a Chromium session calls itself, from `session_verdict` (R5-D44).
    browser: Option<String>,
    // Set when the opening verdict refused the session, with what the core could not end (R4-S5), and
    // whether it could not look for all of the application's processes.
    refused: Option<Vec<chrono_proto::FollowedProcess>>,
    refusal_incomplete: bool,
    timing: Option<(String, i64, i64)>,  // (fake wall reached, real ms, fake ms) from `ended`
    // The target's own exit code and whatever teardown could not remove, both from `ended`. The wire
    // has carried them since the session report grew a duration, and the GUI panel has shown them
    // since 7446a59 - the human CLI report dropped them on the floor, so a plain `chrono run` could
    // not tell "the app closed itself with code 3" from "we ended the session" (rule 6).
    target_exit: Option<i32>,
    residue: Vec<String>,
    // Whether the core closed the session with `ended`, and whether it had said anything that opens
    // one. Together they tell a core that died mid-session from one that refused the first command,
    // which is the only answer a core gives without an `ended` after it (`core::read_start`).
    ended: bool,
    opened: bool,
}

impl Collector {
    /// Record one event. Returns true when the session is over and the driver should stop reading.
    ///
    /// A `state` heartbeat never reaches here. Counting heartbeats and acting on them is driving
    /// rather than collecting, and it needs the child's stdin, which this side deliberately has no
    /// access to.
    pub(super) fn record(&mut self, event: Event) -> bool {
        if matches!(
            event,
            Event::Verdict { .. } | Event::SessionVerdict { .. } | Event::Vanished { .. } | Event::Coverage { .. }
        ) {
            self.opened = true;
        }
        match event {
            Event::Verdict { verdict, reason_key, refuse_start, left_running, family_search_incomplete, .. } => {
                self.verdict_line = Some((verdict, reason_key));
                self.refused = refuse_start.then_some(left_running);
                self.refusal_incomplete = refuse_start && family_search_incomplete;
            }
            Event::SessionVerdict {
                verdict,
                reason_key,
                process_count,
                warning_keys,
                uncovered_children,
                uncovered_children_total,
                context_count,
                engines,
                followed,
                copies_outside,
                host_slept_ms,
                browser,
                ..
            } => {
                self.session_line = Some((verdict, reason_key, process_count));
                self.warn(warning_keys);
                self.uncovered_children = uncovered_children;
                self.uncovered_children_total = uncovered_children_total;
                self.context_count = context_count;
                self.engines = engines;
                self.followed = followed;
                self.copies_outside = copies_outside;
                self.host_slept_ms = host_slept_ms;
                self.browser = browser;
            }
            Event::Vanished { reason_key, lived_ms, .. } => {
                self.vanished = Some((reason_key, lived_ms));
            }
            Event::Error { key, origin, .. } => {
                self.errors.push((key, origin));
            }
            Event::Coverage {
                pid,
                kind,
                covered: cov,
                observed: obs,
                uncovered: unc,
                unobserved: unobs,
                installed_late: late,
                warning_keys,
                ..
            } => {
                self.warn(warning_keys);
                let unit = UnitId::from_wire(&kind, pid);
                if !self.cov_by_unit.contains_key(&unit) {
                    self.unit_order.push(unit);
                }
                self.cov_by_unit.insert(
                    unit,
                    ProcessCoverage {
                        covered: cov,
                        observed: obs,
                        uncovered: unc,
                        unobserved: unobs,
                        installed_late: late,
                    },
                );
            }
            Event::Ended {
                elapsed_real_ms,
                elapsed_fake_ms,
                fake_end_wall,
                target_exit_code,
                residue_keys,
                ..
            } => {
                if let Some(wall) = fake_end_wall {
                    self.timing = Some((wall, elapsed_real_ms, elapsed_fake_ms));
                }
                self.target_exit = target_exit_code;
                self.residue = residue_keys;
                self.ended = true;
                return true;
            }
            _ => {}
        }
        false
    }

    /// Add warning keys to the one de-duplicated list the report prints.
    ///
    /// Session-level warnings join the same list as the per-process ones: the report has one
    /// `warnings:` block, and where a warning came from is a wire detail, not something the reader
    /// should have to know. It is a union across every event as well, because a later snapshot for
    /// the same process carries no driver-side warning and dropping the earlier one would lose it.
    fn warn(&mut self, keys: Vec<String>) {
        for k in keys {
            if !self.warnings.contains(&k) {
                self.warnings.push(k);
            }
        }
    }

    /// Whether the core stopped without closing the session: no `ended`, unless all it said was a refusal
    /// of the first command. A core owes `ended` on every path but that one, which opens nothing and whose
    /// keys are `core::REFUSALS_WITHOUT_ENDED`. Any other error is followed by `ended` (a missing hook
    /// library, a bad moment, a launch that failed), so one without it is a core that stopped between the
    /// two, and its own exit code is no more the session's result than after a kill (R4/12a review round).
    ///
    /// Without this the report stood on whatever arrived first. The parent's verdict comes inside the
    /// guard window, so a core that died a second later left a WORKS headline over call counts from the
    /// session's first blink, and the evidence file carried it with no banner (R4-W3, measured with the
    /// core killed two seconds in).
    pub(super) fn core_left_session_open(&self) -> bool {
        let refused_to_start = !self.opened
            && !self.errors.is_empty()
            && self.errors.iter().all(|(key, _)| crate::core::REFUSALS_WITHOUT_ENDED.contains(&key.as_str()));
        !self.ended && !refused_to_start
    }

    /// The finished report. `cdp` and `stopped_early` are the caller's, not ours: whether the target
    /// is a Chromium build is a fact about the path, and whether the run was cut short is a fact about
    /// the driver's own loop. This side only knows what the core said.
    pub(super) fn into_report(
        self,
        target: String,
        cdp: bool,
        stopped_early: Option<CutShort>,
    ) -> SessionReport {
        // Flatten the per-unit snapshots into report rows, parent first (first-seen order).
        let mut uncovered: Vec<(UnitId, String)> = Vec::new(); // (unit, channel) - the honest gaps
        let mut unobserved: Vec<(UnitId, String)> = Vec::new(); // (unit, channel) - watches that never started
        let mut installed_late: Vec<(UnitId, String)> = Vec::new(); // (unit, channel) - hooked once its module loaded
        let mut covered: Vec<(UnitId, String, u64)> = Vec::new(); // (unit, channel, calls) - what took effect
        let mut observed: Vec<(UnitId, String, u64)> = Vec::new(); // (unit, channel, calls) - hooked, left real
        let mut capped: Vec<(UnitId, String)> = Vec::new(); // (unit, channel) - counts that stopped at the ceiling
        for unit in &self.unit_order {
            if let Some(pc) = self.cov_by_unit.get(unit) {
                for ch in &pc.covered {
                    covered.push((*unit, ch.channel.clone(), ch.calls));
                }
                for ch in &pc.observed {
                    observed.push((*unit, ch.channel.clone(), ch.calls));
                }
                for ch in pc.covered.iter().chain(&pc.observed).filter(|ch| ch.capped) {
                    capped.push((*unit, ch.channel.clone()));
                }
                for ch in &pc.uncovered {
                    uncovered.push((*unit, ch.clone()));
                }
                for ch in &pc.unobserved {
                    unobserved.push((*unit, ch.clone()));
                }
                for ch in &pc.installed_late {
                    installed_late.push((*unit, ch.clone()));
                }
            }
        }

        SessionReport {
            target,
            session_verdict: self.session_line,
            parent_verdict: self.verdict_line,
            vanished: self.vanished,
            errors: self.errors,
            warnings: self.warnings,
            uncovered,
            unobserved,
            installed_late,
            covered,
            observed,
            capped,
            host_slept_ms: self.host_slept_ms,
            browser: self.browser,
            timing: self.timing,
            target_exit: self.target_exit,
            residue: self.residue,
            uncovered_children: self.uncovered_children,
            uncovered_children_total: self.uncovered_children_total,
            context_count: self.context_count,
            engines: self.engines,
            followed: self.followed,
            copies_outside: self.copies_outside,
            refused: self.refused,
            refusal_incomplete: self.refusal_incomplete,
            cdp,
            stopped_early,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_proto::CoveredChannel;

    fn coverage(pid: u32, calls: u64, warning_keys: Vec<String>) -> Event {
        Event::Coverage {
            v: 1,
            pid,
            kind: chrono_proto::UNIT_PROCESS.to_string(),
            covered: vec![CoveredChannel { channel: "GetSystemTimeAsFileTime".into(), calls, capped: false }],
            observed: Vec::new(),
            uncovered: Vec::new(),
            unobserved: Vec::new(),
            installed_late: Vec::new(),
            warning_keys,
        }
    }

    /// R2-X8: the core reports a process twice, once as it is discovered inside the guard window and
    /// once at the end, and only the later snapshot is true. Appending instead would print the
    /// channel twice, the second time with a number from the session's first blink.
    #[test]
    fn the_latest_snapshot_for_a_process_replaces_the_earlier_one() {
        let mut c = Collector::default();
        c.record(coverage(100, 1, Vec::new()));
        c.record(coverage(100, 4242, Vec::new()));

        let report = c.into_report("app.exe".into(), false, None);
        assert_eq!(report.covered.len(), 1, "one row per channel per process, not one per event");
        assert_eq!(report.covered[0], (UnitId::from_wire("process", 100), "GetSystemTimeAsFileTime".to_string(), 4242));
    }

    /// Untouchable rule 4 at the report level: a channel covered in two processes is two rows, each
    /// with its own pid, and never one row with the calls added together. A summed number would
    /// claim coverage for one process out of another process's evidence.
    #[test]
    fn coverage_is_never_summed_across_processes() {
        let mut c = Collector::default();
        c.record(coverage(100, 7, Vec::new()));
        c.record(coverage(200, 5, Vec::new()));

        let report = c.into_report("app.exe".into(), false, None);
        assert_eq!(report.covered.len(), 2, "two processes, two rows");
        assert_eq!(report.covered[0].0.id, 100, "first seen still leads the report");
        assert_eq!(report.covered[1].0.id, 200);
        assert_eq!(report.covered[0].2, 7);
        assert_eq!(report.covered[1].2, 5);
    }

    /// The report has one `warnings:` block, so a key arriving from a per-process event and again
    /// from the session-level one is listed once, and neither source drops the other's keys.
    #[test]
    fn warnings_are_one_deduplicated_union_across_events() {
        let mut c = Collector::default();
        c.record(coverage(100, 1, vec!["runtime.qpc_elapsed".into()]));
        c.record(coverage(100, 2, Vec::new()));
        c.record(Event::SessionVerdict {
            v: 1,
            verdict: "works".into(),
            reason_key: "verdict.works".into(),
            process_count: 1,
            warning_keys: vec!["runtime.qpc_elapsed".into(), "session.pid_registry_full".into()],
            uncovered_children: Vec::new(),
            uncovered_children_total: 0,
            context_count: 0,
            engines: Vec::new(),
            followed: vec![chrono_proto::FollowedProcess { pid: 200, image: Some("app.exe".into()) }],
            copies_outside: Vec::new(),
            host_slept_ms: 0,
            browser: None,
        });

        let report = c.into_report("app.exe".into(), false, None);
        assert_eq!(report.warnings, vec!["runtime.qpc_elapsed", "session.pid_registry_full"]);
        // The processes the session went on for reach the report as the core named them (ADR-16).
        assert_eq!(report.followed, vec![chrono_proto::FollowedProcess { pid: 200, image: Some("app.exe".into()) }]);
    }

    /// Only `ended` ends the read, and it is the one event carrying the target's own exit code and
    /// whatever teardown could not remove. A verdict or a vanish is recorded and read past.
    #[test]
    fn only_ended_stops_the_session_and_it_carries_the_closing_facts() {
        let mut c = Collector::default();
        assert!(!c.record(Event::Verdict {
            v: 1,
            id: None,
            verdict: "works".into(),
            refuse_start: false,
            reason_key: "verdict.works".into(),
            left_running: Vec::new(),
            family_search_incomplete: false,
        }));
        assert!(!c.record(Event::Vanished {
            v: 1,
            pid: 100,
            reason_key: "vanished.single_instance".into(),
            lived_ms: 12,
        }));
        assert!(c.record(Event::Ended {
            v: 1,
            clean: false,
            residue_keys: vec!["cdp.profile_locked".into()],
            target_exit_code: Some(3),
            elapsed_real_ms: 1_000,
            elapsed_fake_ms: 60_000,
            fake_end_wall: Some("2038-01-19T03:15:07".into()),
        }));

        assert!(!c.core_left_session_open(), "a session closed with ended is not one the core left open");

        let report = c.into_report("app.exe".into(), false, None);
        assert_eq!(report.target_exit, Some(3));
        assert_eq!(report.residue, vec!["cdp.profile_locked"]);
        assert_eq!(report.timing, Some(("2038-01-19T03:15:07".to_string(), 1_000, 60_000)));
        assert_eq!(report.parent_verdict.map(|(v, _)| v), Some("works".to_string()));
        assert_eq!(report.vanished.map(|(_, ms)| ms), Some(12));
        assert_eq!(report.refused, None, "a verdict that refused nothing reached the report as a refusal");
    }

    /// R4-S5: a refusal reaches the report with what the core could not end, and an empty list is still
    /// a refusal - the application was ended, which the report says.
    #[test]
    fn a_refusal_reaches_the_report_with_what_it_left_running() {
        let left = vec![chrono_proto::FollowedProcess { pid: 5150, image: Some("helper.exe".into()) }];
        let mut c = Collector::default();
        c.record(Event::Verdict {
            v: 1,
            id: Some(1),
            verdict: "fails".into(),
            refuse_start: true,
            reason_key: "coverage.time_channels_uncovered".into(),
            left_running: left.clone(),
            family_search_incomplete: false,
        });
        let report = c.into_report("app.exe".into(), false, None);
        assert_eq!(report.refused, Some(left));
        assert!(!report.refusal_incomplete, "a complete search reached the report as an incomplete one");

        // A search that could not look for everything reaches the report as that, whatever the list says.
        let mut c = Collector::default();
        c.record(Event::Verdict {
            v: 1,
            id: Some(1),
            verdict: "fails".into(),
            refuse_start: true,
            reason_key: "coverage.time_channels_uncovered".into(),
            left_running: Vec::new(),
            family_search_incomplete: true,
        });
        let report = c.into_report("app.exe".into(), false, None);
        assert_eq!(report.refused, Some(Vec::new()));
        assert!(report.refusal_incomplete, "the incomplete search was lost before the report");
    }

    fn works() -> Event {
        Event::Verdict {
            v: 1,
            id: None,
            verdict: "works".into(),
            refuse_start: false,
            reason_key: "verdict.works".into(),
            left_running: Vec::new(),
            family_search_incomplete: false,
        }
    }

    fn error(key: &str) -> Event {
        Event::Error { v: 1, id: None, code: 1, key: key.into(), origin: "core".into() }
    }

    /// R4-W3: a core that stops before `ended` has left the session open, and the report must not stand
    /// on the verdict it sent at the start. The one answer that owes no `ended` is the refusal of the
    /// first command, and it is told apart by giving a reason while opening nothing.
    #[test]
    fn a_core_that_stops_before_ended_left_the_session_open_unless_it_refused_the_first_command() {
        // Killed after the parent's verdict: the case measured, WORKS over the guard window's counts.
        let mut c = Collector::default();
        c.record(works());
        c.record(coverage(100, 7, Vec::new()));
        assert!(c.core_left_session_open(), "killed mid-session");

        // Killed before it said anything at all, during prepare.
        assert!(Collector::default().core_left_session_open(), "killed before the first event");

        // Killed after a command was refused mid-session: an error, but the session had opened.
        let mut c = Collector::default();
        c.record(works());
        c.record(error("session.bad_multiplier"));
        assert!(c.core_left_session_open(), "an error inside an open session is not a refusal to start");

        // The refusal of the first command: a reason, nothing opened, no ended owed.
        let mut c = Collector::default();
        c.record(error("protocol.version_mismatch"));
        assert!(!c.core_left_session_open(), "a core that refused to start owes no ended");

        // An error after a valid start is followed by ended, so without it the core stopped between the two
        // (R4/12a review round) - it is not a refusal to start, whatever it opened.
        let mut c = Collector::default();
        c.record(error("core.hook_dll_missing"));
        assert!(c.core_left_session_open(), "an error that owes an ended, without one");

        // A refusal key next to another error is not a refusal alone.
        let mut c = Collector::default();
        c.record(error("protocol.bad_command"));
        c.record(error("target.launch_failed"));
        assert!(c.core_left_session_open(), "not every error was a refusal to start");

        // A vanish opens the session too - the core reached the target.
        let mut c = Collector::default();
        c.record(Event::Vanished { v: 1, pid: 100, reason_key: "target.single_instance_suspected".into(), lived_ms: 12 });
        c.record(error("session.bad_multiplier"));
        assert!(c.core_left_session_open(), "a vanish opens the session");
    }

    /// One native session that has something to say in every section a session that ran can have: a
    /// parent verdict, a process and a page with every kind of channel list, a heartbeat, a refused speed
    /// change, a family verdict with a child the hook missed, an engine and a followed program, and an end
    /// with an exit code and something left behind. Two variants of what a section holds are in too, because
    /// the window says them its own way: a count that stopped at the hook's ceiling (the page's) and a sleep
    /// of the machine, which the report measures in its warning line. What a session that never ran says instead (a vanish,
    /// a refusal, a failed start, a cut) has headlines of its own, so it is not here.
    fn golden_session() -> Vec<Event> {
        let read = |channel: &str, calls| CoveredChannel { channel: channel.into(), calls, capped: false };
        vec![
            Event::Ready {
                v: 1,
                protocol: 1,
                core_version: "0.4.0".into(),
                bitness: "x64".into(),
                capabilities: Vec::new(),
            },
            Event::Verdict {
                v: 1,
                id: Some(1),
                verdict: "works".into(),
                refuse_start: false,
                reason_key: "coverage.time_channels_covered".into(),
                left_running: Vec::new(),
                family_search_incomplete: false,
            },
            Event::Coverage {
                v: 1,
                pid: 4200,
                kind: chrono_proto::UNIT_PROCESS.into(),
                covered: vec![read("GetSystemTimePreciseAsFileTime", 12)],
                observed: vec![read("QueryPerformanceCounter", 3)],
                uncovered: vec!["NtQuerySystemTime".into()],
                unobserved: vec!["timeGetTime".into()],
                installed_late: vec!["GetSystemTimePreciseAsFileTime".into()],
                warning_keys: vec!["wait.timeout_collapsed".into()],
            },
            Event::Coverage {
                v: 1,
                pid: 1,
                kind: chrono_proto::UNIT_CONTEXT.into(),
                covered: vec![CoveredChannel { channel: "page Date.now".into(), calls: 5, capped: true }],
                observed: Vec::new(),
                uncovered: Vec::new(),
                unobserved: Vec::new(),
                installed_late: Vec::new(),
                warning_keys: Vec::new(),
            },
            Event::State {
                v: 1,
                fake: chrono_proto::Clock { wall: "2038-01-19T03:14:08".into(), zone_bias_min: 0 },
                real: chrono_proto::Clock { wall: "2026-10-20T10:00:01".into(), zone_bias_min: -120 },
                multiplier: 1,
                elapsed_fake_ms: 1000,
                elapsed_real_ms: 1000,
            },
            Event::Error { v: 1, id: Some(12), code: 2, key: "time.bad_multiplier".into(), origin: "core".into() },
            // A session with a copy outside has the reason and the warning that go with it, as the core
            // sends them, so the window's test reads the warning in the report and in the summary both.
            Event::SessionVerdict {
                v: 1,
                verdict: "partial".into(),
                reason_key: "session.family_handed_to_copy".into(),
                process_count: 3,
                warning_keys: vec![
                    "wait.timeout_collapsed".into(),
                    "time.host_slept".into(),
                    "session.handed_to_copy".into(),
                ],
                uncovered_children: vec![chrono_proto::UncoveredChild {
                    pid: 4300,
                    parent_pid: 4200,
                    image: Some("helper.exe".into()),
                    role: Some("renderer".into()),
                }],
                uncovered_children_total: 2,
                context_count: 1,
                engines: vec![chrono_proto::ReachedEngine {
                    pid: 4400,
                    port: 9333,
                    browser: "Engine/1.0".into(),
                    context_count: 1,
                }],
                followed: vec![chrono_proto::FollowedProcess { pid: 4500, image: Some("app.exe".into()) }],
                copies_outside: vec![chrono_proto::FollowedProcess { pid: 4600, image: Some("app.exe".into()) }],
                host_slept_ms: 5_400_000,
                browser: None,
            },
            Event::Ended {
                v: 1,
                clean: true,
                residue_keys: vec!["cdp.profile_left".into()],
                target_exit_code: Some(3),
                elapsed_real_ms: 2000,
                elapsed_fake_ms: 2000,
                fake_end_wall: Some("2038-01-19T03:14:09".into()),
            },
        ]
    }

    /// The golden session and the CLI report of it, recorded in `tests/wire/` for the window (R5-D64): its
    /// test feeds the same events to the session panel and requires a line of the copied summary for every
    /// section of this report, so a section added to the report and never to the window turns it red. Read
    /// as a diff when recorded again with `CHRONO_BLESS=1`.
    #[test]
    fn the_golden_session_gives_the_report_recorded_for_the_window() {
        let events = golden_session();
        let stream: String = events.iter().map(|e| e.to_ndjson() + "\n").collect();
        let mut c = Collector::default();
        for event in events {
            c.record(event);
        }
        let report = crate::report::render_report(&c.into_report("app.exe".into(), false, None));

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/wire");
        for (name, got) in [("session.ndjson", &stream), ("session-report.txt", &report)] {
            let path = dir.join(name);
            if std::env::var_os("CHRONO_BLESS").is_some() {
                std::fs::write(&path, got).expect("tests/wire is writable");
                continue;
            }
            let want = std::fs::read_to_string(&path).expect("the recorded file exists").replace("\r\n", "\n");
            assert!(
                *got == want,
                "tests/wire/{name} no longer matches the golden session - record a deliberate change with \
                 CHRONO_BLESS=1, read the diff, and give the window a line for any section it adds"
            );
        }
    }
}
