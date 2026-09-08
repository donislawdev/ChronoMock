//! What the session said, gathered from the event stream and nothing else.
//!
//! The driver decides what happens next: when to change speed, when to jump, when to end. This side
//! only records what already happened, which is why it can be tested without starting a process at
//! all - feed it events, read the report. Until it was pulled out, every rule in here (the latest
//! snapshot for a process wins, counts are never summed across processes, warnings are one union)
//! was reachable only through a live run.

use std::collections::HashMap;

use chrono_proto::Event;

use crate::report::{ProcessCoverage, SessionReport};

/// The session's evidence as it arrives, one field per thing the report states. Nothing here is
/// derived twice, so the report cannot claim more than the core actually said (untouchable rule 4).
#[derive(Default)]
pub(super) struct Collector {
    verdict_line: Option<(String, String)>,  // parent (start) verdict, fallback
    session_line: Option<(String, String, u32)>,  // family: (verdict, reason_key, count)
    vanished: Option<(String, u64)>,  // (reason_key, lived_ms)
    errors: Vec<(String, String)>,  // (key, origin) - why a session did not start
    warnings: Vec<String>,
    // Coverage per process, LATEST snapshot wins. The core emits one event per process as it is
    // discovered and a final one for every process at the end, because the first is sampled inside
    // the guard window and its call counts never move again (R2-X8). Appending every event instead
    // would print each channel twice, once with a number from the session's first blink.
    cov_by_pid: HashMap<u32, ProcessCoverage>,
    pid_order: Vec<u32>,  // first-seen order, so the parent still leads the report
    timing: Option<(String, i64, i64)>,  // (fake wall reached, real ms, fake ms) from `ended`
    // The target's own exit code and whatever teardown could not remove, both from `ended`. The wire
    // has carried them since the session report grew a duration, and the GUI panel has shown them
    // since 7446a59 - the human CLI report dropped them on the floor, so a plain `chrono run` could
    // not tell "the app closed itself with code 3" from "we ended the session" (rule 6).
    target_exit: Option<i32>,
    residue: Vec<String>,
}

impl Collector {
    /// Record one event. Returns true when the session is over and the driver should stop reading.
    ///
    /// A `state` heartbeat never reaches here. Counting heartbeats and acting on them is driving
    /// rather than collecting, and it needs the child's stdin, which this side deliberately has no
    /// access to.
    pub(super) fn record(&mut self, event: Event) -> bool {
        match event {
            Event::Verdict { verdict, reason_key, .. } => {
                self.verdict_line = Some((verdict, reason_key));
            }
            Event::SessionVerdict { verdict, reason_key, process_count, warning_keys, .. } => {
                self.session_line = Some((verdict, reason_key, process_count));
                self.warn(warning_keys);
            }
            Event::Vanished { reason_key, lived_ms, .. } => {
                self.vanished = Some((reason_key, lived_ms));
            }
            Event::Error { key, origin, .. } => {
                self.errors.push((key, origin));
            }
            Event::Coverage {
                pid,
                covered: cov,
                observed: obs,
                uncovered: unc,
                unobserved: unobs,
                warning_keys,
                ..
            } => {
                self.warn(warning_keys);
                if !self.cov_by_pid.contains_key(&pid) {
                    self.pid_order.push(pid);
                }
                self.cov_by_pid
                    .insert(pid, ProcessCoverage { covered: cov, observed: obs, uncovered: unc, unobserved: unobs });
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

    /// The finished report. `cdp` and `stopped_early` are the caller's, not ours: whether the target
    /// is a Chromium build is a fact about the path, and whether the run was cut short is a fact about
    /// the driver's own loop. This side only knows what the core said.
    pub(super) fn into_report(
        self,
        target: String,
        cdp: bool,
        stopped_early: Option<&'static str>,
    ) -> SessionReport {
        // Flatten the per-process snapshots into report rows, parent first (first-seen order).
        let mut uncovered: Vec<(u32, String)> = Vec::new(); // (pid, channel) - the honest gaps
        let mut unobserved: Vec<(u32, String)> = Vec::new(); // (pid, channel) - watches that never started
        let mut covered: Vec<(u32, String, u64)> = Vec::new(); // (pid, channel, calls) - what took effect
        let mut observed: Vec<(u32, String, u64)> = Vec::new(); // (pid, channel, calls) - hooked, left real
        for pid in &self.pid_order {
            if let Some(pc) = self.cov_by_pid.get(pid) {
                for ch in &pc.covered {
                    covered.push((*pid, ch.channel.clone(), ch.calls));
                }
                for ch in &pc.observed {
                    observed.push((*pid, ch.channel.clone(), ch.calls));
                }
                for ch in &pc.uncovered {
                    uncovered.push((*pid, ch.clone()));
                }
                for ch in &pc.unobserved {
                    unobserved.push((*pid, ch.clone()));
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
            covered,
            observed,
            timing: self.timing,
            target_exit: self.target_exit,
            residue: self.residue,
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
            covered: vec![CoveredChannel { channel: "GetSystemTimeAsFileTime".into(), calls }],
            observed: Vec::new(),
            uncovered: Vec::new(),
            unobserved: Vec::new(),
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
        assert_eq!(report.covered[0], (100, "GetSystemTimeAsFileTime".to_string(), 4242));
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
        assert_eq!(report.covered[0].0, 100, "first seen still leads the report");
        assert_eq!(report.covered[1].0, 200);
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
        });

        let report = c.into_report("app.exe".into(), false, None);
        assert_eq!(report.warnings, vec!["runtime.qpc_elapsed", "session.pid_registry_full"]);
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

        let report = c.into_report("app.exe".into(), false, None);
        assert_eq!(report.target_exit, Some(3));
        assert_eq!(report.residue, vec!["cdp.profile_locked"]);
        assert_eq!(report.timing, Some(("2038-01-19T03:15:07".to_string(), 1_000, 60_000)));
        assert_eq!(report.parent_verdict.map(|(v, _)| v), Some("works".to_string()));
        assert_eq!(report.vanished.map(|(_, ms)| ms), Some(12));
    }
}
