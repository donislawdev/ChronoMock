//! Chrono Mock machine protocol (ADR-6, docs/08) - NDJSON over a byte stream.
//!
//! One JSON object per line. The core writes events on its stdout, reads commands
//! on its stdin. `stdout` is protocol only, `stderr` is human diagnostics.
//!
//! Stage 1 defines the messages the walking skeleton needs: `start`/`end` commands
//! and `ready`/`verdict`/`ended`/`error` events. Later slices add `set_multiplier`,
//! `jump`, `query`, `ack`, `coverage`, `state`, `warning`, `vanished`.
//!
//! `v` is repeated per message on purpose - a flat, unambiguous wire shape beats a
//! clever envelope that trips serde's flatten + internal-tag edge cases.

use serde::{Deserialize, Serialize};

/// Wire protocol version carried in every message envelope.
pub const PROTOCOL_VERSION: u32 = 1;

/// What to run, as it appears on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetSpec {
    pub path: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

/// The target moment (session-zone semantics, docs/01 section 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentSpec {
    /// "absolute" or "relative".
    pub kind: String,
    #[serde(default)]
    pub local: Option<String>,
    #[serde(default)]
    pub tz_bias_min: Option<i32>,
    #[serde(default)]
    pub delta: Option<String>,
}

/// Time flow selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSpec {
    pub moment: MomentSpec,
    /// "flow" | "frozen" | "multiplier".
    pub mode: String,
    #[serde(default)]
    pub multiplier: Option<i64>,
    #[serde(default)]
    pub scale_duration: bool,
}

/// One covered channel and how many times the target has called it so far.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoveredChannel {
    pub channel: String,
    pub calls: u64,
}

/// One clock reading: the wall-clock text plus the session zone it is expressed in.
/// Both the fake and the real clock in a `state` event carry their zone (two legal
/// views of the same fact).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clock {
    pub wall: String,
    pub zone_bias_min: i32,
}

/// Commands: interface -> core (on the core's stdin).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Start {
        v: u32,
        id: u64,
        target: TargetSpec,
        time: TimeSpec,
    },
    /// Ask for an immediate `state` snapshot (`what` is a stable key, e.g. "state").
    Query {
        v: u32,
        id: u64,
        what: String,
    },
    /// Change the speed in flight. The core re-anchors from its own clock, so the
    /// payload carries only the new multiplier, never a timestamp.
    SetMultiplier {
        v: u32,
        id: u64,
        multiplier: i64,
    },
    /// Jump the wall clock to a new moment. The duration axis is left untouched.
    Jump {
        v: u32,
        id: u64,
        to: MomentSpec,
    },
    End {
        v: u32,
        id: u64,
    },
}

/// Events: core -> interface (on the core's stdout).
///
/// The core emits translation KEYS and structured data, never translated prose
/// (untouchable rules 15 and 16). `reason_key`, `key` are stable keys the consumer
/// renders in the user's language.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Ready {
        v: u32,
        protocol: u32,
        core_version: String,
        bitness: String,
        capabilities: Vec<String>,
    },
    Coverage {
        v: u32,
        pid: u32,
        covered: Vec<CoveredChannel>,
        /// Hooked and counted but deliberately left real (ADR-7 class B object waits). Its own
        /// bucket so the consumer never confuses it with substituted channels. `#[serde(default)]`
        /// keeps coverage messages from before this field existed parseable (additive evolution).
        #[serde(default)]
        observed: Vec<CoveredChannel>,
        uncovered: Vec<String>,
        warning_keys: Vec<String>,
    },
    Verdict {
        v: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        verdict: String,
        refuse_start: bool,
        reason_key: String,
    },
    /// Solicited reply that a command was applied (reflects the command's `id`).
    Ack {
        v: u32,
        id: u64,
    },
    /// The two clocks side by side, emitted as a ~1 s heartbeat and on `query`.
    /// Spontaneous heartbeats carry no `id`; coalesceable (newest wins).
    State {
        v: u32,
        fake: Clock,
        real: Clock,
        multiplier: i64,
        elapsed_fake_ms: i64,
        elapsed_real_ms: i64,
    },
    /// The target vanished right after injection - a suspected single-instance app
    /// (ADR-4). Spontaneous, no id; the tool exits with code 12.
    Vanished {
        v: u32,
        pid: u32,
        reason_key: String,
        lived_ms: u64,
    },
    /// The family-wide session verdict: the honest roll-up of the parent and every child
    /// process, emitted once at session end just before `ended`. The per-process `verdict`
    /// (parent, at start) gates refuse_start; this aggregates the whole family, so a launcher
    /// whose child does the timekeeping is judged by the family, not the parent alone
    /// (untouchable rule 4 at the session level). `process_count` is the family size (parent
    /// plus distinct children). Additive event (docs/08).
    SessionVerdict {
        v: u32,
        verdict: String,
        reason_key: String,
        process_count: u32,
    },
    Ended {
        v: u32,
        clean: bool,
        residue_keys: Vec<String>,
        target_exit_code: Option<i32>,
    },
    Error {
        v: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        code: i32,
        key: String,
        origin: String,
    },
}

impl Event {
    /// Serialize to a single NDJSON line (no trailing newline). Defensive fallback
    /// keeps the stream valid even if serialization somehow fails.
    pub fn to_ndjson(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            String::from(
                r#"{"type":"error","v":1,"code":3,"key":"proto.serialize_failed","origin":"proto"}"#,
            )
        })
    }
}

/// Parse one NDJSON line into an event (used by the interface side).
pub fn parse_event(line: &str) -> Result<Event, serde_json::Error> {
    serde_json::from_str(line)
}

/// Parse one NDJSON line into a command (used by the core side).
pub fn parse_command(line: &str) -> Result<Command, serde_json::Error> {
    serde_json::from_str(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_start_round_trips() {
        let cmd = Command::Start {
            v: PROTOCOL_VERSION,
            id: 1,
            target: TargetSpec { path: "C:/app.exe".into(), args: vec!["--x".into()], cwd: None },
            time: TimeSpec {
                moment: MomentSpec {
                    kind: "absolute".into(),
                    local: Some("2038-01-19T03:14:07".into()),
                    tz_bias_min: Some(-120),
                    delta: None,
                },
                mode: "multiplier".into(),
                multiplier: Some(60),
                scale_duration: false,
            },
        };
        let line = serde_json::to_string(&cmd).unwrap();
        assert!(line.contains(r#""type":"start""#));
        let back = parse_command(&line).unwrap();
        match back {
            Command::Start { id, time, .. } => {
                assert_eq!(id, 1);
                assert_eq!(time.multiplier, Some(60));
            }
            _ => panic!("wrong command variant"),
        }
    }

    #[test]
    fn event_ready_has_type_tag() {
        let ev = Event::Ready {
            v: PROTOCOL_VERSION,
            protocol: PROTOCOL_VERSION,
            core_version: "0.1.0".into(),
            bitness: "x64".into(),
            capabilities: vec![],
        };
        let line = ev.to_ndjson();
        assert!(line.starts_with(r#"{"type":"ready""#));
        assert!(parse_event(&line).is_ok());
    }

    #[test]
    fn verdict_omits_null_id() {
        let ev = Event::Verdict {
            v: PROTOCOL_VERSION,
            id: None,
            verdict: "undetermined".into(),
            refuse_start: false,
            reason_key: "mechanism.not_implemented".into(),
        };
        let line = ev.to_ndjson();
        assert!(!line.contains("\"id\""), "null id must be omitted, got {line}");
    }

    #[test]
    fn coverage_observed_round_trips_and_defaults() {
        let ev = Event::Coverage {
            v: PROTOCOL_VERSION,
            pid: 42,
            covered: vec![CoveredChannel { channel: "GetSystemTime".into(), calls: 3 }],
            observed: vec![CoveredChannel { channel: "WaitForSingleObject".into(), calls: 5 }],
            uncovered: vec![],
            warning_keys: vec!["wait.object_waits_not_scaled".into()],
        };
        let line = ev.to_ndjson();
        assert!(line.contains(r#""observed""#), "observed must serialize, got {line}");
        match parse_event(&line).unwrap() {
            Event::Coverage { observed, warning_keys, .. } => {
                assert_eq!(observed.len(), 1);
                assert_eq!(observed[0].channel, "WaitForSingleObject");
                assert_eq!(observed[0].calls, 5);
                assert_eq!(warning_keys, vec!["wait.object_waits_not_scaled".to_string()]);
            }
            _ => panic!("wrong event variant"),
        }
        // A coverage message from before `observed` existed still parses (serde default).
        let old = r#"{"type":"coverage","v":1,"pid":42,"covered":[],"uncovered":[],"warning_keys":[]}"#;
        match parse_event(old).unwrap() {
            Event::Coverage { observed, .. } => assert!(observed.is_empty()),
            _ => panic!("wrong event variant"),
        }
    }

    #[test]
    fn set_multiplier_and_jump_round_trip() {
        let sm = Command::SetMultiplier { v: 1, id: 7, multiplier: 120 };
        let line = serde_json::to_string(&sm).unwrap();
        assert!(line.contains(r#""type":"set_multiplier""#));
        assert!(matches!(
            parse_command(&line).unwrap(),
            Command::SetMultiplier { multiplier: 120, .. }
        ));

        let jump = Command::Jump {
            v: 1,
            id: 8,
            to: MomentSpec {
                kind: "absolute".into(),
                local: Some("2050-01-01T00:00:00".into()),
                tz_bias_min: Some(0),
                delta: None,
            },
        };
        let line = serde_json::to_string(&jump).unwrap();
        assert!(line.contains(r#""type":"jump""#));
        assert!(matches!(parse_command(&line).unwrap(), Command::Jump { .. }));
    }

    #[test]
    fn session_verdict_round_trips() {
        let ev = Event::SessionVerdict {
            v: PROTOCOL_VERSION,
            verdict: "works".into(),
            reason_key: "session.family_covered".into(),
            process_count: 2,
        };
        let line = ev.to_ndjson();
        assert!(line.starts_with(r#"{"type":"session_verdict""#), "got {line}");
        match parse_event(&line).unwrap() {
            Event::SessionVerdict { verdict, process_count, .. } => {
                assert_eq!(verdict, "works");
                assert_eq!(process_count, 2);
            }
            _ => panic!("wrong event variant"),
        }
    }
}
