//! The Chromium/Electron session: the second substitution mechanism, beside the native one.
//!
//! Where the native path injects a DLL and hooks 36 time channels, this one drives a debug port and
//! evaluates a time shim in every JS context (ADR-8, ADR-9). It speaks the same protocol and returns
//! the same verdicts, so the interface cannot tell which mechanism ran - only the coverage report can.
//!
//! Same boundary as `cdp_probe.rs`: `cdp/` is the transport client, this is the product using it.


use std::collections::HashMap;
use std::io::BufReader;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono_core::Verdict;
use chrono_proto::{
    Command, CoveredChannel, Event, TargetSpec, TimeSpec,
    PROTOCOL_VERSION,
};

use crate::cdp;
use crate::zone::{epoch_ms_to_wall, now_epoch_ms};
use crate::events::{
    command_id, emit, ended_after_launch, ended_clean, unsupported_command,
};
use crate::wire::spawn_command_reader;
use crate::cdp_clock::{cdp_resolve_jump, CdpClock};
/// One shimmed JS context of a Chromium target: the coverage unit of a CDP session (rule 4 - never
/// summed across contexts).
pub(crate) struct CdpContext {
    index: u32,
    session_id: String,
    ty: String,
    /// The CDP targetId, kept because `Target.targetDestroyed` names a target, not a session.
    target_id: String,
}

/// Read each live context's per-API call counts and merge them (by max, so a peak survives a reload)
/// into `counts`, keyed by `(context index, "type api")`. Returns whether any context answered - a
/// dead context (the app closed) simply errors and is skipped, so the audit stays honest.
pub(crate) fn poll_counts(
    client: &mut cdp::CdpClient,
    contexts: &[CdpContext],
    counts: &mut std::collections::BTreeMap<(u32, String), u64>,
) -> bool {
    let mut any = false;
    for c in contexts {
        let read = client.call(
            "Runtime.evaluate",
            serde_json::json!({ "expression": cdp::COUNTS_EXPR, "returnByValue": true }),
            Some(&c.session_id),
        );
        if let Ok(v) = read
            && let Some(obj) = v.get("result").and_then(|x| x.get("value")).and_then(serde_json::Value::as_object) {
                any = true;
                for (api, key) in [("setInterval", "si"), ("setTimeout", "st"), ("Date.now", "now"), ("performance.now", "perf")] {
                    if let Some(n) = obj.get(key).and_then(serde_json::Value::as_u64) {
                        let entry = counts.entry((c.index, format!("{} {}", c.ty, api))).or_insert(0);
                        *entry = (*entry).max(n);
                    }
                }
            }
    }
    any
}

/// A Chromium/Electron session driven over the Chrome DevTools Protocol, speaking the SAME machine
/// protocol as the native core (ADR-9). It is the inverse of the old human-report `driver_run_cdp`:
/// launch + shim + audit, but every outcome travels as `state`/`coverage`/`session_verdict`/`ended`,
/// so the GUI and CLI consume a CDP session exactly like a native one. Interactive commands (`query`
/// now, `set_multiplier`/`jump` in slice C7) arrive on the same stdin the native session reads.
///
/// `--ticks` is a DRIVER concern (the driver counts `state` heartbeats and sends `end`), so this loop
/// runs until `end`, stdin EOF, or the app closing - exactly like `run_session`.
pub(crate) fn cdp_session(target: TargetSpec, time: TimeSpec, reader: BufReader<std::io::Stdin>) -> i32 {
    // Resolve the fake-clock origin ONCE, in Unix-epoch ms, and share it with both the shim and every
    // `state` event, so the panel's fake clock matches what the app's own `Date.now()` reads (no skew
    // from the launch+attach duration). The moment is absolute here (the driver resolved a relative
    // --at before spawning); an out-of-range moment is an honest error, never a silent fall-back to
    // real time (untouchable rule 4).
    let real_start_ms = now_epoch_ms();
    let bias = time.moment.tz_bias_min.unwrap_or(0);
    // The live session clock, computed Rust-side and kept in step with the shim: set_multiplier and
    // jump re-anchor it here and push the new origin to every context. The shim is built PER ATTACH from
    // this clock's current origin (below), so a context that attaches after an in-flight change starts on
    // the same clock as the others. A flag records whether a rate change happened in flight, so the end
    // report can carry the honest running-timer caveat (rule 4).
    let mut clock = match CdpClock::from_time_spec(&time, real_start_ms) {
        Ok(c) => c,
        Err(key) => {
            emit(&Event::Error {
                v: PROTOCOL_VERSION,
                id: Some(1),
                code: 1,
                key: key.into(),
                origin: "core".into(),
            });
            emit(&ended_clean());
            return 1;
        }
    };
    let mut rate_changed_in_flight = false;

    // Launch under our own isolated profile + debug port, then attach. Any failure is an honest error
    // event plus exit 2 (could not launch/attach), never a faked verdict.
    // The heartbeat starts here, not after the attach. Everything between accepting `start` and the
    // first `state` used to be silence, bounded by the port wait (15 s) - and the client's idle
    // watchdog is 15 s, so a slow Electron and a dead core looked the same to it. Measured on
    // Pomotroid the gap is about 1.6 s, so this is about the tail, not the common case (R3-3).
    let mut last_beat = std::time::Instant::now();
    let mut launched = match cdp::launch_chromium(&target.path, &target.args, target.cwd.as_deref(), || {
        if last_beat.elapsed() >= std::time::Duration::from_secs(1) {
            last_beat = std::time::Instant::now();
            emit(&clock.state_event_at(now_epoch_ms()));
        }
    }) {
        Ok(l) => l,
        Err(e) => {
            emit(&Event::Error {
                v: PROTOCOL_VERSION,
                id: Some(1),
                code: 2,
                key: "target.launch_failed".into(),
                origin: "mechanism".into(),
            });
            eprintln!("chrono core: {e}");
            emit(&ended_clean());
            return 2;
        }
    };
    let mut client = match cdp::CdpClient::connect_to_port("127.0.0.1", launched.port) {
        Ok(c) => c,
        Err(e) => {
            let residue = launched.shutdown_with_residue();
            emit(&Event::Error {
                v: PROTOCOL_VERSION,
                id: Some(1),
                code: 2,
                key: "target.attach_failed".into(),
                origin: "mechanism".into(),
            });
            eprintln!("chrono core: cannot attach over CDP: {e}");
            emit(&ended_after_launch(residue));
            return 2;
        }
    };
    if let Err(e) = client.call(
        "Target.setAutoAttach",
        serde_json::json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
        None,
    ) {
        let residue = launched.shutdown_with_residue();
        emit(&Event::Error {
            v: PROTOCOL_VERSION,
            id: Some(1),
            code: 2,
            key: "target.attach_failed".into(),
            origin: "mechanism".into(),
        });
        eprintln!("chrono core: cannot set up auto-attach: {e}");
        emit(&ended_after_launch(residue));
        return 2;
    }
    // No start verdict for CDP: unlike the native guard window, at start there is nothing to judge yet
    // (contexts attach asynchronously and have made no time calls). The authoritative verdict is the
    // family `session_verdict` at end - emitting "undetermined" now would read as "not working" when it
    // only means "not audited yet".

    // A stdin reader thread turns command lines into `Command`s (end/query/set_multiplier/jump) so the
    // main loop can interleave them with CDP polling and the heartbeat without blocking on read_line.
    let rx = spawn_command_reader(reader);

    // Install the shim into every context as it attaches (page and its Web Workers), beat a ~1 s
    // `state` heartbeat, and sample per-context call counts, until `end`, stdin EOF, or the app closes.
    let mut contexts: Vec<CdpContext> = Vec::new();
    // Every context index this session ever shimmed, in attach order. Append-only, so evidence
    // outlives the context that produced it (R2-W1) - see the note where a context is attached.
    let mut seen: Vec<u32> = Vec::new();
    let mut counts: std::collections::BTreeMap<(u32, String), u64> = std::collections::BTreeMap::new();
    let mut failed = 0usize;
    let mut next_index = 0u32;
    // targetId -> context index, so a re-attached context keeps the identity it already had.
    let mut index_by_target: HashMap<String, u32> = HashMap::new();
    let mut app_closed = false;
    let heartbeat = Duration::from_secs(1);
    let mut deadline = Instant::now() + heartbeat;
    let mut last_audit = Instant::now();

    'session: loop {
        if !launched.is_running() {
            app_closed = true;
            break;
        }
        // Drain protocol commands without blocking.
        loop {
            match rx.try_recv() {
                Ok(Command::End { .. }) => break 'session,
                Ok(Command::Query { id, .. }) => {
                    emit(&clock.state_event_at(now_epoch_ms()));
                    emit(&Event::Ack { v: PROTOCOL_VERSION, id });
                }
                Ok(Command::SetMultiplier { id, multiplier, .. }) => {
                    // Re-anchor the clock (wall and duration both continue from now at the new rate) and
                    // push the new origin + rate to every context. New timers pick up the rate at once;
                    // Date.now/new Date/performance.now reflect it immediately - only an already-queued
                    // setInterval keeps its old cadence, which the end report warns about (rule 4).
                    if !chrono_core::multiplier_in_range(multiplier) {
                        emit(&Event::Error {
                            v: PROTOCOL_VERSION,
                            id: Some(id),
                            code: 1,
                            key: "time.bad_multiplier".into(),
                            origin: "core".into(),
                        });
                        continue;
                    }
                    let now = now_epoch_ms();
                    let (fake0, real0, m) = clock.set_multiplier_at(multiplier, now);
                    cdp_broadcast(&mut client, &contexts, &cdp_set_multiplier_expr(fake0, real0, m));
                    rate_changed_in_flight = true;
                    emit(&Event::Ack { v: PROTOCOL_VERSION, id });
                    emit(&clock.state_event_at(now_epoch_ms()));
                }
                Ok(Command::Jump { id, to, .. }) => {
                    // Move the wall to a new fake instant (absolute, or a relative delta on the current
                    // fake time), leaving the duration axis untouched (rule 3). A bad moment is an honest
                    // error, never a silent no-op (rule 6).
                    let now = now_epoch_ms();
                    match cdp_resolve_jump(&clock, &to, now) {
                        Ok(new_fake) => {
                            let (fake0, real0) = clock.jump_to_at(new_fake, now);
                            cdp_broadcast(&mut client, &contexts, &cdp_jump_expr(fake0, real0));
                            emit(&Event::Ack { v: PROTOCOL_VERSION, id });
                            emit(&clock.state_event_at(now_epoch_ms()));
                        }
                        Err(key) => emit(&Event::Error {
                            v: PROTOCOL_VERSION,
                            id: Some(id),
                            code: 1,
                            key: key.into(),
                            origin: "core".into(),
                        }),
                    }
                }
                // A command this loop does not handle here (a second `start`, say). Answered rather
                // than dropped, and with the id, so a client waiting on `ack` learns the outcome.
                Ok(other) => emit(&unsupported_command(command_id(&other))),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'session, // stdin closed (EOF)
            }
        }
        // Poll CDP (bounded by the WS read timeout) for a newly attached context, and shim it.
        match client.poll() {
            Ok(Some(cdp::Msg::Event { method, params, .. })) if method == "Target.attachedToTarget" => {
                let sid = params["sessionId"].as_str().unwrap_or("").to_string();
                let ty = params["targetInfo"]["type"].as_str().unwrap_or("").to_string();
                let tid = params["targetInfo"]["targetId"].as_str().unwrap_or("").to_string();
                if !sid.is_empty() && cdp::is_shimmable(&ty) {
                    // The context index is keyed by the CDP targetId, which Chromium keeps across
                    // re-attaches, not by a counter that ticks once per attach. A worker that is
                    // recycled - an ordinary pattern in Electron apps, and the very shape the CDP
                    // mechanism was built for - re-attaches under the SAME targetId, and the counter
                    // gave it a new identity every time: `process_count` grew with the length of the
                    // session rather than describing the application, `counts` gained four entries
                    // per recycle and released none, and the end-of-session emit walked seen x
                    // covered. The merge rule for counts is already "max, so a peak survives a
                    // reload" - it was only ever missing a stable key (R3-7).
                    //
                    // A target that names no id keeps the old behaviour (a fresh index): with no
                    // identity to match on, treating it as new is the honest choice, not a guess.
                    let index = context_index_for(&tid, &mut index_by_target, &mut next_index);
                    // Build the shim from the clock's CURRENT origin, not the session's initial values, so
                    // a context attaching after an in-flight rate change or jump starts on the same clock
                    // as every other context (one absolute origin; rule 3). Before any change this is
                    // identical to the initial shim.
                    let (shim_fake0, shim_real0, shim_mult) = clock.shim_origin();
                    let shim = cdp::build_shim(shim_fake0, shim_real0, shim_mult);
                    let injected = if cdp::is_worker(&ty) {
                        cdp::inject_worker(&mut client, &sid, &shim)
                    } else {
                        cdp::inject_page(&mut client, &sid, &shim)
                    };
                    match injected {
                        Ok(()) => {
                            // Two lists on purpose. `contexts` is who we still TALK to - polling or
                            // broadcasting to a dead session costs the full read deadline inside the
                            // session loop. `seen` is who this session ever COVERED, and it only grows:
                            // the audit is a record of what happened, not of what is still open, so a
                            // context that reloaded or closed keeps its evidence (R2-W1).
                            // Append-only, and now once per CONTEXT rather than once per attach.
                            if !seen.contains(&index) {
                                seen.push(index);
                            }
                            contexts.push(CdpContext {
                                index,
                                session_id: sid,
                                // The target named its own context type, and that name becomes a
                                // coverage key in the report and on the wire. Cleaned here, at the
                                // one place a context is built, rather than at the one place the key
                                // is formatted - a second use added later would otherwise carry raw
                                // target text without anyone noticing.
                                ty: cdp::sanitise_target_text(&ty),
                                target_id: tid,
                            });
                        }
                        Err(_) => failed += 1,
                    }
                }
            }
            // A context that went away - a reload, a closed window, a recycled worker. Drop it from the
            // poll list: nothing else did, so the list only ever grew, and every dead entry still got a
            // Runtime.evaluate every second. That inflated the reported context count, and a command to a
            // dead session that draws no reply at all costs the full 20 s deadline INSIDE the session
            // loop - no heartbeat, no `end`, no liveness check for that whole time. Its counts stay in
            // `counts` and its index in `seen`, so the audit still reports it - which this comment used
            // to claim while the emitting loop walked the LIVE list and dropped it (R2-W1).
            Ok(Some(cdp::Msg::Event { method, params, .. }))
                if method == "Target.detachedFromTarget" || method == "Target.targetDestroyed" =>
            {
                let sid = params["sessionId"].as_str().unwrap_or("");
                let tid = params["targetId"].as_str().unwrap_or("");
                contexts.retain(|c| {
                    let gone = (!sid.is_empty() && c.session_id == sid)
                        || (!tid.is_empty() && c.target_id == tid);
                    !gone
                });
            }
            Ok(_) => {}
            Err(_) => {
                app_closed = true; // the connection dropped, i.e. the app exited
                break;
            }
        }
        // ~1 s heartbeat (also when frozen) and ~1 s coverage sampling.
        if Instant::now() >= deadline {
            emit(&clock.state_event_at(now_epoch_ms()));
            deadline = Instant::now() + heartbeat;
        }
        if last_audit.elapsed() >= Duration::from_secs(1) {
            poll_counts(&mut client, &contexts, &mut counts);
            last_audit = Instant::now();
        }
    }
    poll_counts(&mut client, &contexts, &mut counts); // final best-effort read
    let audited = !counts.is_empty();

    // Coverage = APIs the app actually called (count > 0), per context - honest "covered", like native.
    let mut covered: Vec<(u32, String, u64)> = counts
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|((idx, ch), n)| (idx, ch, n))
        .collect();
    covered.sort();

    let verdict = cdp_verdict(seen.len(), !covered.is_empty(), failed);
    let (token, reason) = match &verdict {
        Verdict::Works => ("works", "chromium.contexts_covered"),
        Verdict::Partial => ("partial", "chromium.contexts_partial"),
        Verdict::Fails => ("fails", "chromium.no_contexts"),
        Verdict::Undetermined => ("undetermined", "chromium.no_time_calls"),
    };

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

    // Emit one `coverage` per attached context (pid = context index), never summed across contexts
    // (rule 4). The invasive-launch warning rides on the FIRST event, and if no context attached at
    // all we still emit one bare coverage - so the warning is never lost for an idle or zero-context app.
    if seen.is_empty() {
        emit(&Event::Coverage {
            v: PROTOCOL_VERSION,
            pid: 0,
            covered: Vec::new(),
            observed: Vec::new(),
            uncovered: Vec::new(),
            warning_keys: std::mem::take(&mut warnings),
        });
    } else {
        for index in &seen {
            let chans: Vec<CoveredChannel> = covered
                .iter()
                .filter(|(idx, _, _)| idx == index)
                .map(|(_, ch, n)| CoveredChannel { channel: ch.clone(), calls: *n })
                .collect();
            emit(&Event::Coverage {
                v: PROTOCOL_VERSION,
                pid: *index,
                covered: chans,
                observed: Vec::new(),
                uncovered: Vec::new(),
                warning_keys: std::mem::take(&mut warnings),
            });
        }
    }

    emit(&Event::SessionVerdict {
        v: PROTOCOL_VERSION,
        verdict: token.to_string(),
        reason_key: reason.to_string(),
        process_count: seen.len() as u32,
        // No PID registry on this path: a CDP session tracks JS contexts, not injected processes, and
        // its per-context warnings already travel on the coverage events above.
        warning_keys: Vec::new(),
    });

    // Session timing from the live clock (correct across any in-flight rate changes and jumps): real is
    // the whole session, fake is the elapsed duration, and the end wall is where the fake clock landed.
    // Then tear down our instance (kill + remove the temp profile), reporting any cleanup residue
    // honestly via `ended.residue_keys` (rules 4, 6).
    let end_now = now_epoch_ms();
    let real_ms = clock.elapsed_real_ms(end_now);
    let fake_ms = clock.elapsed_fake_ms(end_now);
    let fake_end_wall = epoch_ms_to_wall(clock.fake_wall_ms(end_now), bias);
    let residue = launched.shutdown_with_residue();
    emit(&Event::Ended {
        v: PROTOCOL_VERSION,
        clean: residue.is_empty(),
        residue_keys: residue,
        target_exit_code: None,
        elapsed_real_ms: real_ms,
        elapsed_fake_ms: fake_ms,
        fake_end_wall: Some(fake_end_wall),
    });
    verdict.exit_code()
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


/// Evaluate a JS expression in every attached context (best-effort: a context that just closed errors
/// and is skipped, so an in-flight update stays honest for the rest).
pub(crate) fn cdp_broadcast(client: &mut cdp::CdpClient, contexts: &[CdpContext], expr: &str) {
    for ctx in contexts {
        let _ = client.call(
            "Runtime.evaluate",
            serde_json::json!({ "expression": expr, "returnByValue": true }),
            Some(&ctx.session_id),
        );
    }
}

/// The JS to push a new wall origin AND rate into a context's `__chronomock`, re-anchoring its local
/// duration axis first so `performance.now` stays continuous across the rate change (rule 3). The wall
/// origin (fake0, real0, mult) is the driver's, identical for every context, so all contexts stay in
/// step.
pub(crate) fn cdp_set_multiplier_expr(fake0: i64, real0: i64, mult: i64) -> String {
    format!(
        "(function(){{var S=globalThis.__chronomock;if(!S)return 'no-shim';\
         var p=S._realPerf?S._realPerf():0;S.perfBase=(S.perfBase||0)+(p-S.perfAnchorReal)*(S.M||1);\
         S.perfAnchorReal=p;S.fakeStart={fake0};S.realStart={real0};S.M={mult};return 'ok';}})()"
    )
}

/// The JS to push a new wall origin into a context's `__chronomock` for a jump - wall only; the rate
/// and the duration axis are untouched, so a backward jump never rewinds elapsed time (rule 3).
pub(crate) fn cdp_jump_expr(fake0: i64, real0: i64) -> String {
    format!(
        "(function(){{var S=globalThis.__chronomock;if(!S)return 'no-shim';\
         S.fakeStart={fake0};S.realStart={real0};return 'ok';}})()"
    )
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
}
