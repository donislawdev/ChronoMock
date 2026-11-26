//! The commands a running session takes, answered once for both mechanisms (R5-A3).
//!
//! `query`, `set_multiplier` and `jump` mean the same on the native path and on the Chromium one, and
//! the interface cannot tell which ran (docs/08). They used to be answered by two functions that
//! agreed by copying - the refusal of a rate out of range, the reading of a jump target, the order of
//! `ack` and `state` - and the copies had drifted before (R5-P18, R5-P19). The protocol half is written
//! here once, and each mechanism says only how its clock moves, through [`SessionClock`].

use chronomock::calc::Step;
use chrono_proto::{Command, Event, MomentKind, MomentSpec, PROTOCOL_VERSION};

use crate::events::{command_id, emit, error_event, unsupported_command};
use crate::grammar::parse_shift;

/// A running session's clock, as the commands see it: what it reads now, and how it moves.
///
/// Each method answers whether the session's loop notes the move. What a loop notes is its own
/// business - the native one notes every move for the timers pending when the clock moved (R5-D2),
/// the Chromium one a rate change under a live context for its timers already running (R5-P19) - so
/// the answer comes from the mechanism and the protocol half only hands it on.
pub(crate) trait SessionClock {
    /// The `state` event for the clock as it stands now.
    fn state_event(&self) -> Event;
    /// Run at `multiplier` from now. The value is already checked to be in range.
    fn set_multiplier(&mut self, multiplier: i64) -> bool;
    /// Move the wall to `to`, leaving the duration axis where it is (rule 3), or say why not with the
    /// key of the refusal.
    fn jump(&mut self, to: &JumpTo<'_>) -> Result<bool, &'static str>;
}

/// Where a `jump` goes, read off the wire once for both mechanisms.
#[derive(Debug, PartialEq)]
pub(crate) enum JumpTo<'a> {
    /// One step from the session's fake now, in the grammar `calc` and `--at` share - so `jump`
    /// accepts the same calendar units. Only the session knows its live fake clock, so it resolves
    /// the step itself.
    Step(Step),
    /// A wall-clock moment, read in the zone `bias` names or in the session's when it names none
    /// (R4-S9).
    At { local: &'a str, bias: Option<i32> },
}

/// Read a jump's target, or the key of the refusal: a relative jump needs a `delta` the step grammar
/// reads, an absolute one a `local`, and a kind this build does not know is `moment.unsupported_kind`
/// (R5-P18).
///
/// An absolute jump without `local` is `moment.invalid` before its zone is looked at. The native
/// session used to read the missing moment as an empty one and check the zone first, so the same
/// command got `time.bad_zone` there and `moment.invalid` over CDP (R5-A3).
pub(crate) fn read_jump(to: &MomentSpec) -> Result<JumpTo<'_>, &'static str> {
    match to.kind {
        MomentKind::Relative => {
            let delta = to.delta.as_deref().ok_or("moment.invalid")?;
            parse_shift(delta).map(JumpTo::Step).map_err(|_| "moment.invalid")
        }
        MomentKind::Absolute => {
            let local = to.local.as_deref().ok_or("moment.invalid")?;
            Ok(JumpTo::At { local, bias: to.tz_bias_min })
        }
        MomentKind::Unknown => Err("moment.unsupported_kind"),
    }
}

/// Act on one command that arrived mid-session, and answer it.
///
/// `end` never reaches here. Whether the loop keeps running is the loop's business rather than a
/// command's effect on the session, so that one arm stays where the decision is made. Everything
/// else is answered: a command this state cannot act on (a second `start`) gets `unsupported` WITH its
/// id, so a client waiting on `ack` learns the outcome instead of waiting for ever.
///
/// A refused command never ends the session: it keeps the rate and the moment it had, and the client
/// is told why (rule 6). Answers what the clock's move answered, `false` for anything that did not
/// move it.
pub(crate) fn apply_command(clock: &mut impl SessionClock, command: Command) -> bool {
    match command {
        Command::Query { id, .. } => {
            emit(&clock.state_event());
            emit(&Event::Ack { v: PROTOCOL_VERSION, id });
            false
        }
        Command::SetMultiplier { id, multiplier, .. } => {
            // Bounded here as well as at start: a rate change in flight reaches the same anchor
            // arithmetic, so an unchecked one walks the clock out of range, or backwards, just as
            // effectively (R2-K2, R2-K3).
            if !chrono_core::multiplier_in_range(multiplier) {
                emit(&error_event(Some(id), 1, "time.bad_multiplier", "core"));
                return false;
            }
            let noted = clock.set_multiplier(multiplier);
            emit(&Event::Ack { v: PROTOCOL_VERSION, id });
            emit(&clock.state_event());
            noted
        }
        Command::Jump { id, to, .. } => match read_jump(&to).and_then(|target| clock.jump(&target)) {
            Ok(noted) => {
                emit(&Event::Ack { v: PROTOCOL_VERSION, id });
                emit(&clock.state_event());
                noted
            }
            Err(key) => {
                emit(&error_event(Some(id), 1, key, "core"));
                false
            }
        },
        other => {
            emit(&unsupported_command(command_id(&other)));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: MomentKind, local: Option<&str>, bias: Option<i32>, delta: Option<&str>) -> MomentSpec {
        MomentSpec { kind, local: local.map(str::to_string), tz_bias_min: bias, delta: delta.map(str::to_string) }
    }

    /// The reading both mechanisms share: a step from `delta`, a moment from `local` with the zone it
    /// names, and a refusal key for each way a target can be wrong.
    #[test]
    fn a_jump_target_is_read_once_for_both_mechanisms() {
        let at = spec(MomentKind::Absolute, Some("2030-06-15T12:00:00"), Some(-120), None);
        assert_eq!(read_jump(&at), Ok(JumpTo::At { local: "2030-06-15T12:00:00", bias: Some(-120) }));
        let step = spec(MomentKind::Relative, None, None, Some("+3d"));
        assert_eq!(read_jump(&step), Ok(JumpTo::Step(parse_shift("+3d").expect("a step the grammar reads"))));

        assert_eq!(read_jump(&spec(MomentKind::Relative, None, None, None)), Err("moment.invalid"));
        assert_eq!(read_jump(&spec(MomentKind::Relative, None, None, Some("soon"))), Err("moment.invalid"));
        // Missing before its zone is looked at, on both paths now (R5-A3).
        assert_eq!(read_jump(&spec(MomentKind::Absolute, None, Some(900), None)), Err("moment.invalid"));
        assert_eq!(
            read_jump(&spec(MomentKind::Unknown, Some("2030-06-15T12:00:00"), None, None)),
            Err("moment.unsupported_kind")
        );
    }

    /// A clock that records what it was asked to do, and answers as a mechanism would.
    #[derive(Default)]
    struct Recorded {
        rates: Vec<i64>,
        jumps: usize,
        refuse_jump: Option<&'static str>,
    }

    impl SessionClock for Recorded {
        fn state_event(&self) -> Event {
            Event::Ack { v: PROTOCOL_VERSION, id: u64::MAX }
        }

        fn set_multiplier(&mut self, multiplier: i64) -> bool {
            self.rates.push(multiplier);
            true
        }

        fn jump(&mut self, _to: &JumpTo<'_>) -> Result<bool, &'static str> {
            match self.refuse_jump {
                Some(key) => Err(key),
                None => {
                    self.jumps += 1;
                    Ok(true)
                }
            }
        }
    }

    /// A rate out of range never reaches the clock, and what a move answers is what the loop gets -
    /// nothing for a refusal, a query or a command this state does not act on.
    #[test]
    fn a_refused_command_does_not_move_the_clock_and_a_move_hands_on_what_the_clock_answered() {
        let mut clock = Recorded::default();
        let speed = |multiplier| Command::SetMultiplier { v: PROTOCOL_VERSION, id: 1, multiplier };
        assert!(!apply_command(&mut clock, speed(-1)));
        assert!(!apply_command(&mut clock, speed(chrono_core::MULTIPLIER_MAX + 1)));
        assert!(apply_command(&mut clock, speed(60)));
        assert_eq!(clock.rates, [60]);

        let jump = |kind, local: &str| Command::Jump { v: PROTOCOL_VERSION, id: 2, to: spec(kind, Some(local), None, None) };
        assert!(apply_command(&mut clock, jump(MomentKind::Absolute, "2030-06-15T12:00:00")));
        assert!(!apply_command(&mut clock, jump(MomentKind::Unknown, "2030-06-15T12:00:00")));
        clock.refuse_jump = Some("moment.out_of_range");
        assert!(!apply_command(&mut clock, jump(MomentKind::Absolute, "9999-01-01T00:00:00")));
        assert_eq!(clock.jumps, 1);

        assert!(!apply_command(&mut clock, Command::Query { v: PROTOCOL_VERSION, id: 3, what: chrono_proto::QueryWhat::State }));
    }
}
