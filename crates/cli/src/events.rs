//! Writing protocol events onto stdout, and the small constructors both mechanisms share.
//!
//! `emit` is the single writer: stdout is the protocol and stderr is diagnostics (docs/08), and one
//! function owning that split is what keeps a stray `println!` from corrupting a client's stream.
//!
//! The rest are shapes the native core and the Chromium session both produce. They live together
//! because an event that means one thing on one path and another on the other would be a contract
//! break no test could see - the interface cannot tell which mechanism ran, and must not need to.


use std::io::Write;

use chronomock::calc::EvalError;
use chrono_core::{SessionMomentError, TimeMode};
use chronomock::filetime_utc_to_wall;
use chrono_proto::{Clock, Command, CoveredChannel, Event, TimeModeKind, TimeSpec, PROTOCOL_VERSION};
/// Translation key for a relative-jump eval error (docs/08 section 10). A session has no calendar,
/// so a business-day step needs one. A step or a base that lands outside the range a session clock
/// can hold is `moment.out_of_range`, the same key a start that far out gets (R4-S7c) - the rest is an
/// invalid moment. Honest, never silent (rule 6).
///
/// Every variant is named, with no catch-all (R5-D5, item 4): a variant added to `EvalError` is a
/// compile error here, where it used to become `moment.invalid` without anyone deciding so. The ones a
/// jump cannot produce today - it builds one shift from the session clock, with no calendar - are
/// still named, so the day one of them can is a decision too.
pub(crate) fn jump_error_key(e: EvalError) -> &'static str {
    match e {
        EvalError::NeedsCalendar { .. } => "moment.needs_calendar",
        EvalError::Overflow { .. }
        | EvalError::YearOutOfRange { .. }
        | EvalError::BaseYearOutOfRange
        | EvalError::BaseOverflow => "moment.out_of_range",
        EvalError::StepUnsupported { .. }
        | EvalError::DegenerateCalendar { .. }
        | EvalError::BadSetTime { .. }
        | EvalError::BaseNotACivilDate
        | EvalError::TooManyBusinessDays { .. }
        | EvalError::BeforeCalendar { .. } => "moment.invalid",
    }
}

/// Translation key for a moment the session gate refused, on a start or on a jump.
pub(crate) fn moment_error_key(e: &SessionMomentError) -> &'static str {
    match e {
        SessionMomentError::BadZone => "time.bad_zone",
        SessionMomentError::NotAMoment(_) => "moment.invalid",
        SessionMomentError::OutOfRange => "moment.out_of_range",
    }
}

/// The time mode a `start` asks for, checked the same way for BOTH mechanisms (R4-S9).
///
/// The Chromium session used to read `mode` on its own and never went through this check: a mistyped
/// mode ran as flow, a negative multiplier as x1, one above the limit was taken as it came, and
/// `{mode: multiplier, multiplier: 0}` accelerated at x1 where the native session froze. Zero is the
/// wire spelling of freeze in both now, as it is for `set_multiplier`.
pub(crate) fn start_time_mode(time: &TimeSpec) -> Result<TimeMode, &'static str> {
    match time.mode {
        TimeModeKind::Flow => Ok(TimeMode::Flow),
        TimeModeKind::Frozen => Ok(TimeMode::Frozen),
        TimeModeKind::Multiplier => {
            // The core reads NDJSON from whatever client is on the other end, so it validates for
            // itself rather than trusting the friendly CLI to have done it. It did not, and the two
            // surfaces had drifted: `--mode` required >= 1 while the protocol took any i64, so a
            // negative rate ran the target's clock CONTINUOUSLY BACKWARD and an enormous one walked
            // it out of the representable range - both reported as `works` (R2-K2, R2-K3).
            let m = time.multiplier.unwrap_or(1);
            if chrono_core::multiplier_in_range(m) {
                Ok(TimeMode::Multiplier(m))
            } else {
                Err("time.bad_multiplier")
            }
        }
        TimeModeKind::Unknown => Err("time.bad_mode"),
    }
}

/// Emit one event line and flush immediately - a piped stdout is block-buffered,
/// so without the flush the driver would hang waiting for `ready`.
pub(crate) fn emit(ev: &Event) {
    // The line and its newline as one buffer handed over at once: `writeln!` gave stdout's line
    // writer the event and the newline in pieces, and a line past its buffer went out in two writes,
    // leaving room for anyone else on the pipe to land between them (R4-W1).
    let mut line = ev.to_ndjson();
    line.push('\n');
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(line.as_bytes());
    let _ = lock.flush();
}

/// Emit a `coverage` event for one process (parent or a child), tagged with its pid.
/// Coverage is reliable (never coalesced) - one event per process, never summed. `extra_warnings` are
/// driver-side warning keys (e.g. the detected runtime, B1) appended to the core's own, de-duplicated.
pub(crate) fn emit_coverage(pid: u32, cov: &chrono_core::Coverage, extra_warnings: &[String]) {
    let to_wire = |cs: &[chrono_core::ChannelCoverage]| -> Vec<CoveredChannel> {
        cs.iter()
            .map(|c| CoveredChannel { channel: c.channel.clone(), calls: c.calls, capped: c.capped })
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
        kind: chrono_proto::UNIT_PROCESS.to_string(),
        covered: to_wire(&cov.covered),
        observed: to_wire(&cov.observed),
        uncovered: cov.uncovered.clone(),
        unobserved: cov.unobserved.clone(),
        installed_late: cov.installed_late.clone(),
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

/// `error`, the one place it is built (R5-A3): `id` names the command it answers, absent for a line
/// that was no command, `code` is the exit code the refusal stands for (docs/08 section 8), `key` is the
/// translation key and `origin` says who refused - `core` for the protocol and the session's own
/// checks, `mechanism` for the substitution underneath.
pub(crate) fn error_event(id: Option<u64>, code: i32, key: &str, origin: &str) -> Event {
    Event::Error { v: PROTOCOL_VERSION, id, code, key: key.into(), origin: origin.into() }
}

/// `error` for a well-formed command the running session does not act on. The session continues -
/// this reports an outcome, it does not end anything.
pub(crate) fn unsupported_command(id: u64) -> Event {
    error_event(Some(id), 1, "protocol.unsupported_command", "core")
}

/// The shortest sleep of the machine a session tells about (R5-P1). The sleep bias moves only when the
/// machine wakes, so any growth is a real sleep, and the threshold only keeps a resume of a fraction of a
/// second out of the log - it is three orders of magnitude above the scatter that a difference of two clocks
/// would have had (plan R5, 4.5). The native session and the Chromium session alike (R5-F6).
pub(crate) const SLEPT_REPORTED_MS: i64 = 1_000;

/// The machine slept during the session, and the session clock stood still for it (ADR-5, docs/01 3.0a).
pub(crate) const KEY_HOST_SLEPT: &str = "time.host_slept";

/// How much sleep the log owes: everything since the last announcement, once it reaches the threshold.
pub(crate) fn sleep_to_announce(announced_ms: i64, total_ms: i64) -> Option<i64> {
    let new = total_ms.saturating_sub(announced_ms);
    (new >= SLEPT_REPORTED_MS).then_some(new)
}

/// The total sleep as `session_verdict` carries it: 0, which the wire leaves out, below the threshold.
pub(crate) fn reported_sleep_ms(total_ms: i64) -> u64 {
    if total_ms >= SLEPT_REPORTED_MS { total_ms as u64 } else { 0 }
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
/// rejected start, an unparsable moment - and there it is the truth, not a shortcut. The native core
/// uses this one for a start that did not become a running session once it had set a registry value,
/// and the residue is that value when it could not be taken away.
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_core::step_target;
    use chronomock::calc::{Sign, Step, Unit};

    fn days(sign: Sign, amount: i64) -> Step {
        Step::Shift { sign, amount, unit: Unit::Days }
    }

    /// R4-S7c on the native jump: `jump_step` hands `step_target`'s refusal to this mapping, so a jump
    /// past either end of the session range reaches the tester as the range, not as a malformed date.
    #[test]
    fn a_jump_out_of_the_session_range_is_named_as_the_range() {
        let now = chrono_core::moment_to_filetime_utc(&chrono_core::Moment {
            local: "2026-01-01T00:00:00".into(),
            tz_bias_min: Some(0),
        })
        .unwrap();
        let back = step_target(now, 0, &days(Sign::Minus, 300_000)).unwrap_err();
        assert_eq!(jump_error_key(back), "moment.out_of_range");
        let past_top = step_target(chrono_core::FAKE_WALL_MAX, 0, &days(Sign::Plus, 1)).unwrap_err();
        assert_eq!(jump_error_key(past_top), "moment.out_of_range");
        let far = step_target(now, 0, &Step::Shift { sign: Sign::Plus, amount: 300_000, unit: Unit::Years }).unwrap_err();
        assert_eq!(jump_error_key(far), "moment.out_of_range");
        // The two that are not about the range keep their own keys.
        let bd = step_target(now, 0, &Step::Shift { sign: Sign::Plus, amount: 5, unit: Unit::BusinessDays }).unwrap_err();
        assert_eq!(jump_error_key(bd), "moment.needs_calendar");
        assert_eq!(jump_error_key(EvalError::BadSetTime { index: 0 }), "moment.invalid");
        // A base outside the range is the range too, where the catch-all called it invalid (R5-D5, item 4).
        assert_eq!(jump_error_key(EvalError::BaseOverflow), "moment.out_of_range");
        assert_eq!(jump_error_key(EvalError::BaseYearOutOfRange), "moment.out_of_range");
    }

    /// The session gate's three refusals, each under its own key (R4-S7, R4-S9).
    #[test]
    fn each_session_moment_refusal_has_its_own_key() {
        assert_eq!(moment_error_key(&SessionMomentError::BadZone), "time.bad_zone");
        assert_eq!(moment_error_key(&SessionMomentError::OutOfRange), "moment.out_of_range");
        assert_eq!(moment_error_key(&SessionMomentError::NotAMoment(String::new())), "moment.invalid");
    }
}
