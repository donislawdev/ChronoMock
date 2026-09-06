//! Writing protocol events onto stdout, and the small constructors both mechanisms share.
//!
//! `emit` is the single writer: stdout is the protocol and stderr is diagnostics (docs/08), and one
//! function owning that split is what keeps a stray `println!` from corrupting a client's stream.
//!
//! The rest are shapes the native core and the Chromium session both produce. They live together
//! because an event that means one thing on one path and another on the other would be a contract
//! break no test could see - the interface cannot tell which mechanism ran, and must not need to.


use std::io::Write;

use chrono_core::calc::EvalError;
use chrono_core::filetime_utc_to_wall;
use chrono_proto::{Clock, Command, CoveredChannel, Event, PROTOCOL_VERSION};
/// Translation key for a relative-jump eval error (docs/08 section 10). Business days need a
/// calendar (not built yet); anything else is an invalid moment. Honest, never silent (rule 6).
pub(crate) fn jump_error_key(e: EvalError) -> &'static str {
    match e {
        EvalError::NeedsCalendar { .. } => "moment.needs_calendar",
        _ => "moment.invalid",
    }
}

/// Emit one event line and flush immediately - a piped stdout is block-buffered,
/// so without the flush the driver would hang waiting for `ready`.
pub(crate) fn emit(ev: &Event) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{}", ev.to_ndjson());
    let _ = lock.flush();
}

/// Emit a `coverage` event for one process (parent or a child), tagged with its pid.
/// Coverage is reliable (never coalesced) - one event per process, never summed. `extra_warnings` are
/// driver-side warning keys (e.g. the detected runtime, B1) appended to the core's own, de-duplicated.
pub(crate) fn emit_coverage(pid: u32, cov: &chrono_core::Coverage, extra_warnings: &[String]) {
    let to_wire = |cs: &[chrono_core::ChannelCoverage]| -> Vec<CoveredChannel> {
        cs.iter()
            .map(|c| CoveredChannel { channel: c.channel.clone(), calls: c.calls })
            .collect()
    };
    let mut warning_keys = cov.warning_keys.clone();
    for w in extra_warnings {
        if !warning_keys.contains(w) {
            warning_keys.push(w.clone());
        }
    }
    emit(&Event::Coverage {
        v: PROTOCOL_VERSION,
        pid,
        covered: to_wire(&cov.covered),
        observed: to_wire(&cov.observed),
        uncovered: cov.uncovered.clone(),
        warning_keys,
    });
}

/// The id carried by any command, so a refusal can name the command it refuses.
pub(crate) fn command_id(cmd: &Command) -> u64 {
    match cmd {
        Command::Start { id, .. }
        | Command::Query { id, .. }
        | Command::SetMultiplier { id, .. }
        | Command::Jump { id, .. }
        | Command::End { id, .. } => *id,
    }
}

/// `error` for a well-formed command the running session does not act on. The session continues -
/// this reports an outcome, it does not end anything.
pub(crate) fn unsupported_command(id: u64) -> Event {
    Event::Error {
        v: PROTOCOL_VERSION,
        id: Some(id),
        code: 1,
        key: "protocol.unsupported_command".into(),
        origin: "core".into(),
    }
}

pub(crate) fn ended_clean() -> Event {
    Event::Ended {
        v: PROTOCOL_VERSION,
        clean: true,
        residue_keys: vec![],
        target_exit_code: None,
        elapsed_real_ms: 0,
        elapsed_fake_ms: 0,
        fake_end_wall: None,
    }
}

/// `ended` for a start that failed AFTER the browser was launched. Same shape as [`ended_clean`] -
/// the session never ran, so both elapsed clocks stay zero and there is no target exit code to
/// report - except that it carries whatever cleanup could not remove.
///
/// The distinction is the whole point: a failed attach that ALSO left a locked profile on disk used
/// to announce the session as ended cleanly, because both error paths threw the residue away
/// (`shutdown()`) and then emitted a hard-coded `clean: true`. That is exactly the silence rules 4
/// and 6 exist to prevent, and exactly what the success path a hundred lines below already avoids.
/// `ended_clean` stays for the paths where nothing was ever launched - a missing hook DLL, a
/// rejected start, an unparsable moment - and there it is the truth, not a shortcut.
pub(crate) fn ended_after_launch(residue: Vec<String>) -> Event {
    Event::Ended {
        v: PROTOCOL_VERSION,
        clean: residue.is_empty(),
        residue_keys: residue,
        target_exit_code: None,
        elapsed_real_ms: 0,
        elapsed_fake_ms: 0,
        fake_end_wall: None,
    }
}

/// Build a `state` event from the session's current clocks.
pub(crate) fn state_event(session: &chrono_mech::Session) -> Event {
    state_event_from(&session.state())
}

/// The wire `state` for a state already sampled - so a caller that needs to LOOK at the sample
/// (the clamp check, R2-X2) reads the same one it reports, not a second sample taken next door.
pub(crate) fn state_event_from(s: &chrono_mech::SessionState) -> Event {
    Event::State {
        v: PROTOCOL_VERSION,
        fake: Clock {
            wall: filetime_utc_to_wall(s.fake_ft, s.tz_bias),
            zone_bias_min: s.tz_bias,
        },
        real: Clock {
            wall: filetime_utc_to_wall(s.real_ft, s.tz_bias),
            zone_bias_min: s.tz_bias,
        },
        multiplier: s.multiplier,
        elapsed_fake_ms: s.elapsed_fake_ms,
        elapsed_real_ms: s.elapsed_real_ms,
    }
}
