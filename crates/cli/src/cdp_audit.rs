//! What a Chromium session covered, and the verdict that follows from it.
//!
//! The coverage unit here is a JS context rather than an operating-system process, and the rule is
//! the one the native path obeys as well: a count belongs to the context that made it and is never
//! summed across contexts (untouchable rule 4). Everything in this module is a pure function over
//! what the session observed, so those rules can be checked without a browser - which matters,
//! because the bug `cdp_verdict` exists to pin needed a real Chromium and a gracefully closed
//! window to reproduce.

use std::collections::{BTreeMap, HashMap};

use chrono_core::Verdict;
use chrono_proto::{CoveredChannel, Event, PROTOCOL_VERSION};

/// The channels the session can honestly call covered, in a stable order.
pub(crate) fn covered_channels(counts: BTreeMap<(u32, String), u64>) -> Vec<(u32, String, u64)> {
    // Coverage = APIs the app actually called (count > 0), per context - honest "covered", like native.
    let mut covered: Vec<(u32, String, u64)> = counts
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|((idx, ch), n)| (idx, ch, n))
        .collect();
    covered.sort();
    covered
}

/// The verdict of a CDP session, from the three facts that decide it. Pulled out of the session loop
/// so it can be tested without a browser - the bug it exists to pin needed a real Chromium and a
/// gracefully closed window to reproduce (R2-W1).
///
/// `shimmed` counts every context the session EVER covered, not the ones still attached. Chromium
/// destroys its targets while shutting down, so a healthy session with full coverage could reach the
/// end with an empty live list; counting those, it reported `fails` with exit code 11 and emitted no
/// coverage at all. Measured on Pomotroid: closing the window mid-session turned a `works` run with
/// four covered APIs into `DID NOT TAKE EFFECT (contexts: 0)`, exit 11. What a session covered does
/// not stop being true when the app closes.
pub(crate) fn cdp_verdict(shimmed: usize, any_covered: bool, failed: usize) -> Verdict {
    if shimmed == 0 {
        // Nothing was ever shimmed: the substitution genuinely never reached the app.
        Verdict::Fails
    } else if !any_covered {
        // Shimmed, but the app never called a time API - honest "we do not know", never a fake works.
        Verdict::Undetermined
    } else if failed > 0 {
        Verdict::Partial
    } else {
        Verdict::Works
    }
}

/// The wire token and the reason key of a verdict, decided in one place, so the token a client
/// branches on and the key it renders cannot come apart.
pub(crate) fn verdict_keys(verdict: &Verdict) -> (&'static str, &'static str) {
    match verdict {
        Verdict::Works => ("works", "chromium.contexts_covered"),
        Verdict::Partial => ("partial", "chromium.contexts_partial"),
        Verdict::Fails => ("fails", "chromium.no_contexts"),
        Verdict::Undetermined => ("undetermined", "chromium.no_time_calls"),
    }
}

/// What a finished CDP session has to say about itself beyond the coverage numbers.
///
/// The launch is invasive by construction (our own profile, a debug port), so that one is always
/// said. The other two are honest caveats rather than failures, and both would be invisible to the
/// reader if they were left out (rule 6).
pub(crate) fn session_warnings(app_closed: bool, audited: bool, rate_changed_in_flight: bool) -> Vec<String> {
    let mut warnings = vec!["chromium.launched_with_debug_port".to_string()];
    if app_closed && !audited {
        warnings.push("chromium.app_closed_before_audit".to_string());
    }
    if rate_changed_in_flight {
        // Honest caveat: a rate change reaches Date.now/new Date/performance.now and every NEW timer at
        // once, but a setInterval already scheduled at the old rate keeps its old cadence - the JS engine
        // had already queued it (rule 4). The native hook has no equivalent gap (it divides Ctl live).
        warnings.push("chromium.rate_change_affects_running_timers".to_string());
    }
    warnings
}

/// One `coverage` event per context the session covered.
///
/// Built rather than emitted, so the rules below can be read off a return value instead of a
/// stream: exactly one event per context, each carrying only its own counts, and the warnings on
/// the first event alone.
pub(crate) fn coverage_events(
    seen: &[u32],
    covered: &[(u32, String, u64)],
    mut warnings: Vec<String>,
) -> Vec<Event> {
    // Emit one `coverage` per attached context (pid = context index), never summed across contexts
    // (rule 4). The invasive-launch warning rides on the FIRST event, and if no context attached at
    // all we still emit one bare coverage - so the warning is never lost for an idle or zero-context app.
    if seen.is_empty() {
        return vec![Event::Coverage {
            v: PROTOCOL_VERSION,
            pid: 0,
            covered: Vec::new(),
            observed: Vec::new(),
            uncovered: Vec::new(),
            warning_keys: warnings,
        }];
    }
    seen.iter()
        .map(|index| {
            let chans: Vec<CoveredChannel> = covered
                .iter()
                .filter(|(idx, _, _)| idx == index)
                .map(|(_, ch, n)| CoveredChannel { channel: ch.clone(), calls: *n })
                .collect();
            Event::Coverage {
                v: PROTOCOL_VERSION,
                pid: *index,
                covered: chans,
                observed: Vec::new(),
                uncovered: Vec::new(),
                warning_keys: std::mem::take(&mut warnings),
            }
        })
        .collect()
}

/// The context index for a CDP target, stable across re-attaches.
///
/// Keyed by the CDP targetId, which Chromium keeps when a context re-attaches, rather than by a
/// counter that ticks once per attach. A recycled worker - an ordinary pattern in Electron apps,
/// and the very shape the CDP mechanism was built for - re-attaches under the SAME targetId, and
/// the counter gave it a new identity every time: `process_count` grew with the LENGTH of the
/// session instead of describing the application, `counts` gained entries per recycle and released
/// none, and the end-of-session emit walked seen x covered. The merge rule for counts is already
/// "max, so a peak survives a reload" - it was only ever missing a stable key (R3-7).
///
/// A target that names no id gets a fresh index: with no identity to match on, treating it as new
/// is the honest answer rather than a guess that would fold two contexts into one.
pub(crate) fn context_index_for(
    target_id: &str,
    index_by_target: &mut HashMap<String, u32>,
    next_index: &mut u32,
) -> u32 {
    if !target_id.is_empty()
        && let Some(existing) = index_by_target.get(target_id)
    {
        return *existing;
    }
    *next_index += 1;
    if !target_id.is_empty() {
        index_by_target.insert(target_id.to_string(), *next_index);
    }
    *next_index
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker that is recycled re-attaches under the same targetId, and used to be counted as a
    /// new context each time - so a long accelerated session (the flagship use case: "a day a
    /// minute") reported hundreds of contexts for an application with one worker (R3-7).
    #[test]
    fn a_reattached_context_keeps_the_index_it_already_had() {
        let mut map = HashMap::new();
        let mut next = 0u32;

        let page = context_index_for("T-page", &mut map, &mut next);
        let worker = context_index_for("T-worker", &mut map, &mut next);
        assert_eq!((page, worker), (1, 2));

        // The worker is recycled twice: same target, same index, and the counter does not move.
        assert_eq!(context_index_for("T-worker", &mut map, &mut next), worker);
        assert_eq!(context_index_for("T-worker", &mut map, &mut next), worker);
        assert_eq!(next, 2, "a re-attach must not mint a new context index");

        // A genuinely new target still gets one.
        assert_eq!(context_index_for("T-other", &mut map, &mut next), 3);

        // No id means no identity to match on, so each one is new rather than folded together -
        // two anonymous contexts are two contexts, and pretending otherwise would under-report.
        let a = context_index_for("", &mut map, &mut next);
        let b = context_index_for("", &mut map, &mut next);
        assert_ne!(a, b);
    }

    #[test]
    fn cdp_verdict_counts_every_context_the_session_covered_not_the_survivors() {
        // R2-W1, the case that needed a real browser to reproduce: Chromium destroys its targets while
        // shutting down, so a healthy session could reach the verdict with an empty LIVE context list.
        // Counting survivors called it `fails` with exit code 11 and dropped the coverage entirely -
        // measured on Pomotroid, a closing window turned a four-channel `works` into
        // "DID NOT TAKE EFFECT (contexts: 0)". Two contexts shimmed and covered stays `works` however
        // many of them are still attached, because the argument is what the session covered.
        assert_eq!(cdp_verdict(2, true, 0), Verdict::Works);

        // Nothing ever shimmed is the one genuine failure: the substitution never reached the app.
        assert_eq!(cdp_verdict(0, false, 0), Verdict::Fails);

        // Shimmed but never asked the time: honest "we do not know", never a fake works (rule 4).
        assert_eq!(cdp_verdict(1, false, 0), Verdict::Undetermined);

        // Some contexts failed to take the shim: covered in part, and said so.
        assert_eq!(cdp_verdict(3, true, 1), Verdict::Partial);
    }

    fn counts(entries: &[(u32, &str, u64)]) -> BTreeMap<(u32, String), u64> {
        entries.iter().map(|(i, ch, n)| ((*i, (*ch).to_string()), *n)).collect()
    }

    fn channels(event: &Event) -> Vec<(String, u64)> {
        match event {
            Event::Coverage { covered, .. } => {
                covered.iter().map(|c| (c.channel.clone(), c.calls)).collect()
            }
            _ => panic!("expected a coverage event"),
        }
    }

    /// Untouchable rule 4 on the Chromium path: the same API called in two contexts is two events
    /// with their own counts, never one event with the calls added up. A summed number would claim
    /// coverage for one context out of another context's evidence.
    #[test]
    fn coverage_is_one_event_per_context_and_never_summed() {
        let covered = covered_channels(counts(&[(0, "page Date.now", 7), (1, "page Date.now", 5)]));
        let events = coverage_events(&[0, 1], &covered, Vec::new());

        assert_eq!(events.len(), 2, "two contexts, two events");
        assert_eq!(channels(&events[0]), vec![("page Date.now".to_string(), 7)]);
        assert_eq!(channels(&events[1]), vec![("page Date.now".to_string(), 5)]);
    }

    /// An API the app never called is not coverage. Reporting a zero as covered would be the audit
    /// claiming an effect it did not measure.
    #[test]
    fn an_api_that_was_never_called_is_not_reported_as_covered() {
        let covered = covered_channels(counts(&[(0, "page Date.now", 0), (0, "page setInterval", 3)]));
        let events = coverage_events(&[0], &covered, Vec::new());

        assert_eq!(channels(&events[0]), vec![("page setInterval".to_string(), 3)]);
    }

    /// The warnings ride the FIRST event only, so a reader sees each one once rather than once per
    /// context.
    #[test]
    fn the_warnings_ride_the_first_event_only() {
        let covered = covered_channels(counts(&[(0, "page Date.now", 1), (1, "worker Date.now", 1)]));
        let events = coverage_events(&[0, 1], &covered, vec!["chromium.launched_with_debug_port".into()]);

        let keys = |e: &Event| match e {
            Event::Coverage { warning_keys, .. } => warning_keys.clone(),
            _ => panic!("expected a coverage event"),
        };
        assert_eq!(keys(&events[0]), vec!["chromium.launched_with_debug_port"]);
        assert!(keys(&events[1]).is_empty(), "the second event does not repeat the warning");
    }

    /// A session where nothing ever attached still emits one bare coverage event, because the
    /// warnings travel on it - an idle or zero-context app would otherwise lose them entirely.
    #[test]
    fn a_session_with_no_context_still_carries_its_warnings() {
        let events = coverage_events(&[], &[], vec!["chromium.launched_with_debug_port".into()]);

        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::Coverage { pid, covered, warning_keys, .. } => {
                assert_eq!(*pid, 0);
                assert!(covered.is_empty());
                assert_eq!(warning_keys, &vec!["chromium.launched_with_debug_port".to_string()]);
            }
            _ => panic!("expected a coverage event"),
        }
    }

    /// The two caveats are conditional and the invasive-launch note is not. An app that closed before
    /// anything could be audited says so, and a rate changed in flight says so, because a running
    /// setInterval keeps its old cadence (rule 4).
    #[test]
    fn the_session_warnings_say_only_what_happened() {
        assert_eq!(session_warnings(false, true, false), vec!["chromium.launched_with_debug_port"]);
        assert_eq!(
            session_warnings(true, false, false),
            vec!["chromium.launched_with_debug_port", "chromium.app_closed_before_audit"]
        );
        assert_eq!(
            session_warnings(true, true, true),
            vec!["chromium.launched_with_debug_port", "chromium.rate_change_affects_running_timers"],
            "an app that closed AFTER being audited has nothing to apologise for"
        );
    }
}
