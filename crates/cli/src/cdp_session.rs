//! The Chromium/Electron session: the second substitution mechanism, beside the native one.
//!
//! Where the native path injects a DLL and hooks 36 time channels, this one drives a debug port and
//! evaluates a time shim in every JS context (ADR-8, ADR-9). It speaks the same protocol and returns
//! the same verdicts, so the interface cannot tell which mechanism ran - only the coverage report can.
//!
//! Same boundary as `cdp_probe.rs`: `cdp/` is the transport client, this is the product using it.


use std::io::BufReader;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono_proto::{
    Command, Event, TargetSpec, TimeSpec,
    PROTOCOL_VERSION,
};

use crate::cdp;
use crate::zone::{epoch_ms_to_wall, now_epoch_ms};
use crate::events::{
    command_id, emit, ended_after_launch, ended_clean, unsupported_command,
};
use crate::wire::spawn_command_reader;
use crate::cdp_attach::{Attacher, Pumped};
use crate::cdp_clock::{cdp_jump_expr, cdp_resolve_jump, cdp_set_multiplier_expr, CdpClock};
use crate::cdp_audit::{
    covered_channels, coverage_events, cdp_verdict, session_warnings,
    verdict_keys,
};

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
    // --at before spawning) - an out-of-range moment is an honest error, never a silent fall-back to
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
    // the measured Electron app the gap is about 1.6 s, so this is about the tail, not the common case (R3-3).
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
    // The contexts, their counts and their indexes live in the attacher (cdp_attach) - the index
    // counter stays here, because it is the session's: several attachers in one session must hand
    // out disjoint indexes, and that is the shape the embedded-engine channel needs (docs/09).
    let mut next_index = 0u32;
    // The attacher connects to the browser endpoint, arms auto-attach and attaches by name to the
    // pages the browser already has - measured, auto-attach delivers those too, and the by-name pass
    // is the belt for an engine where it does not. Any failure is one honest error here, as before.
    let mut attacher = match Attacher::connect("127.0.0.1", launched.port)
        .and_then(|mut a| a.attach_existing(clock.shim_origin(), &mut next_index).map(|_| a))
    {
        Ok(a) => a,
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
    // No start verdict for CDP: unlike the native guard window, at start there is nothing to judge yet
    // (contexts attach asynchronously and have made no time calls). The authoritative verdict is the
    // family `session_verdict` at end - emitting "undetermined" now would read as "not working" when it
    // only means "not audited yet".

    // A stdin reader thread turns command lines into `Command`s (end/query/set_multiplier/jump) so the
    // main loop can interleave them with CDP polling and the heartbeat without blocking on read_line.
    let rx = spawn_command_reader(reader);

    // Install the shim into every context as it attaches (page and its Web Workers), beat a ~1 s
    // `state` heartbeat, and sample per-context call counts, until `end`, stdin EOF, or the app closes.
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
                    // push the new origin + rate to every context. New timers pick up the rate at once -
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
                    attacher.broadcast(&cdp_set_multiplier_expr(fake0, real0, m, m));
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
                            attacher.broadcast(&cdp_jump_expr(fake0, real0));
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
        // Poll CDP (bounded by the WS read timeout) for a newly attached context and shim it, or
        // drop one that went away. The shim is built from the clock's CURRENT origin, so a context
        // attaching after an in-flight rate change or jump starts on the same clock as every other
        // context (one absolute origin - rule 3).
        if attacher.pump(clock.shim_origin(), &mut next_index) == Pumped::Closed {
            app_closed = true; // the connection dropped, i.e. the app exited
            break;
        }
        // ~1 s heartbeat (also when frozen) and ~1 s coverage sampling.
        if Instant::now() >= deadline {
            emit(&clock.state_event_at(now_epoch_ms()));
            deadline = Instant::now() + heartbeat;
        }
        if last_audit.elapsed() >= Duration::from_secs(1) {
            attacher.poll_counts();
            last_audit = Instant::now();
        }
    }
    attacher.poll_counts(); // final best-effort read
    let seen = attacher.seen().to_vec();
    // A context the shim did not take in and a context refused past the ceiling are the same fact
    // to the verdict: a context that ran on the real clock (untouchable rule 4). The ceiling gets
    // its own warning so the reader learns WHY, not just that some were missed.
    let failed = attacher.failed();
    let past_ceiling = attacher.overflow();
    let counts = attacher.into_counts();
    let audited = !counts.is_empty();

    let covered = covered_channels(counts);

    let verdict = cdp_verdict(seen.len(), !covered.is_empty(), failed + past_ceiling);
    let (token, reason) = verdict_keys(&verdict);

    for event in coverage_events(
        &seen,
        &covered,
        session_warnings(app_closed, audited, rate_changed_in_flight, past_ceiling > 0),
    ) {
        emit(&event);
    }

    emit_cdp_session_verdict(token, reason, seen.len() as u32);

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




/// The family verdict of a CDP session. No PID registry on this path: a CDP session tracks JS
/// contexts, not injected processes, so its per-context warnings already travel on the coverage
/// events, and it spawns nothing the hook could fail to follow - the two child fields stay empty.
/// `process_count` has carried the context count since this session existed and keeps doing so for
/// the clients that read it there. `context_count` says the same number under its own name.
fn emit_cdp_session_verdict(token: &str, reason: &str, contexts: u32) {
    emit(&Event::SessionVerdict {
        v: PROTOCOL_VERSION,
        verdict: token.to_string(),
        reason_key: reason.to_string(),
        process_count: contexts,
        warning_keys: Vec::new(),
        uncovered_children: Vec::new(),
        uncovered_children_total: 0,
        context_count: contexts,
        engines: Vec::new(),
    });
}


