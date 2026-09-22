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
    /// Reach the web pages inside the application: an embedded Chromium engine (WebView2, Qt
    /// WebEngine) is asked to open a local debugging port for the session, and its pages are put on
    /// the session clock through it (docs/09). On by default, and a client from before this field
    /// existed gets the default - the channel is the product's coverage promise (rule 27), and the
    /// opt-out is for the tester who does not want a debugging port open in their application.
    /// Ignored for a target that IS Chromium, which the CDP session drives anyway.
    #[serde(default = "embedded_default")]
    pub embedded: bool,
}

/// The default for [`TargetSpec::embedded`]: reach the pages.
fn embedded_default() -> bool {
    true
}

/// The coverage unit a `coverage` event speaks for: an operating-system process the hook is inside
/// (`pid` is its pid), or a JS context reached over the DevTools protocol (`pid` is the context's
/// index in this session). Two namespaces that a reader must not confuse: pid 8 and context 8 are
/// different units, and the family of one session can hold both (docs/09 section 12.4).
pub const UNIT_PROCESS: &str = "process";
pub const UNIT_CONTEXT: &str = "context";

/// The default for `coverage.kind`: a message from before the field existed came from a process.
fn unit_process() -> String {
    UNIT_PROCESS.to_string()
}

/// A DevTools endpoint the session reached inside the application: the pid that holds it, the
/// loopback port, and what the engine calls itself in `/json/version` (cleaned). The port is here
/// rather than in a warning's text because keys are static - a tester who wants to attach their
/// own DevTools reads it off this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachedEngine {
    pub pid: u32,
    pub port: u16,
    pub browser: String,
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
    /// Also scale QueryPerformanceCounter (ADR-2 reversal, opt-in). Additive: an older client that omits
    /// it defaults to false (QPC left real). Separate from scale_duration - it carries a render risk.
    #[serde(default)]
    pub scale_qpc: bool,
}

/// One covered channel and how many times the target has called it so far.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoveredChannel {
    pub channel: String,
    pub calls: u64,
}

/// A process the family spawned without the hook inside it - it ran on the real clock. `image` is
/// the executable's file name when the child was still alive to be asked, absent otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncoveredChild {
    pub pid: u32,
    pub parent_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The `--type=` role a Chromium-based engine gives each subprocess (`renderer`, `gpu-process`,
    /// `utility`), read off the child's command line while it was alive. Absent for a child without
    /// one, or one that was gone before it could be asked. A renderer here is the process the
    /// application's pages run in - the one that decides what those pages read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// The most uncovered children one `session_verdict` names. A parent's ring holds 32 and the
/// registry has 256 slots, so the unbounded list could be thousands of entries in one NDJSON line
/// for a family that fans out - the total still travels, the names past this point do not.
pub const UNCOVERED_CHILDREN_WIRE_MAX: usize = 64;

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
        /// Run the session even when the opening verdict says the substitution did not take effect.
        /// Without it the core stops the target and refuses (`refuse_start`), because a target that
        /// looks time-shifted but is not is worse than one that never launched. Additive: an older
        /// client that omits it gets the refusal, which is the safe direction.
        #[serde(default)]
        force: bool,
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
        /// [`UNIT_PROCESS`] or [`UNIT_CONTEXT`] - what `pid` names. Additive: absent in older
        /// messages, which came from processes.
        #[serde(default = "unit_process")]
        kind: String,
        covered: Vec<CoveredChannel>,
        /// Hooked and counted but deliberately left real (ADR-7 class B object waits). Its own
        /// bucket so the consumer never confuses it with substituted channels. `#[serde(default)]`
        /// keeps coverage messages from before this field existed parseable (additive evolution).
        #[serde(default)]
        observed: Vec<CoveredChannel>,
        uncovered: Vec<String>,
        /// Channels the session meant to watch and could not hook - see `chrono_core::Coverage`.
        /// Additive like `observed`: a message from before this field existed still parses.
        #[serde(default)]
        unobserved: Vec<String>,
        /// Channels hooked only once their module loaded, by name - see `chrono_core::Coverage`.
        /// Additive like the two above: a message from before this field existed still parses.
        #[serde(default)]
        installed_late: Vec<String>,
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
    /// Spontaneous heartbeats carry no `id` - coalesceable (newest wins).
    State {
        v: u32,
        fake: Clock,
        real: Clock,
        multiplier: i64,
        elapsed_fake_ms: i64,
        elapsed_real_ms: i64,
    },
    /// The target vanished right after injection - a suspected single-instance app
    /// (ADR-4). Spontaneous, no id - the tool exits with code 12.
    Vanished {
        v: u32,
        pid: u32,
        reason_key: String,
        lived_ms: u64,
    },
    /// The family-wide session verdict: the honest roll-up of the parent and every child
    /// process, emitted once at session end just before `ended`. The per-process `verdict`
    /// (parent, at start) gates refuse_start - this aggregates the whole family, so a launcher
    /// whose child does the timekeeping is judged by the family, not the parent alone
    /// (untouchable rule 4 at the session level). `process_count` is the family size (parent
    /// plus distinct children). Additive event (docs/08).
    SessionVerdict {
        v: u32,
        verdict: String,
        reason_key: String,
        process_count: u32,
        /// Warnings about the SESSION as a whole, rather than about one process. A per-process
        /// `coverage` event cannot carry these: the first case to need one (a full PID registry,
        /// R2-S9) is precisely about processes that never got a slot, so there is no pid to attach it
        /// to, and attaching it to some other process's coverage would be a lie about that process.
        /// `#[serde(default)]` keeps session_verdict messages from before this field parseable
        /// (additive evolution, zasady/15).
        #[serde(default)]
        warning_keys: Vec<String>,
        /// Processes this family spawned and the hook did not follow into (docs/zasady/SLOWNIK
        /// `uncoveredChild`). Named here, on the one event that speaks for the whole family, because
        /// none of them has a `coverage` event of its own - that is what being uncovered means. The
        /// list is capped at [`UNCOVERED_CHILDREN_WIRE_MAX`] entries so one line stays a line a client
        /// can read, and `uncovered_children_total` is the true number regardless. Additive like
        /// `warning_keys`: absent in older messages, which deserialize to empty and zero.
        #[serde(default)]
        uncovered_children: Vec<UncoveredChild>,
        #[serde(default)]
        uncovered_children_total: u32,
        /// How many JS contexts the session covered beside its processes - the pages and workers of
        /// an embedded web engine (docs/09). `process_count` stays the number of PROCESSES. Additive.
        #[serde(default)]
        context_count: u32,
        /// The DevTools endpoints the session reached inside the application, one per engine.
        /// Empty for a session that found none, and for a Chromium session, which opened its own.
        #[serde(default)]
        engines: Vec<ReachedEngine>,
    },
    Ended {
        v: u32,
        clean: bool,
        residue_keys: Vec<String>,
        target_exit_code: Option<i32>,
        /// Session duration - real and fake milliseconds elapsed - and the fake wall clock reached
        /// at end. Additive (serde default), so a report can state how long the session ran and how
        /// far the fake clock advanced reliably, even for a session too short to emit a heartbeat.
        #[serde(default)]
        elapsed_real_ms: i64,
        #[serde(default)]
        elapsed_fake_ms: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fake_end_wall: Option<String>,
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

    /// S-12. `force` is additive: a client built before it existed sends no such field, and must parse
    /// into the REFUSING default - the safe direction. A missing flag must never mean "run anyway".
    #[test]
    fn a_start_without_force_defaults_to_refusing() {
        let line = r#"{"type":"start","v":1,"id":1,"target":{"path":"C:/app.exe","args":[],"cwd":null},
            "time":{"moment":{"kind":"absolute","local":"2038-01-19T03:14:07","tz_bias_min":0,"delta":null},
            "mode":"flow","multiplier":null,"scale_duration":false,"scale_qpc":false}}"#;
        match parse_command(line).expect("an older client's start still parses") {
            Command::Start { force, target, .. } => {
                assert!(!force, "a missing force must not run anyway");
                // The opposite default for the embedded-engine channel (docs/09): reaching the pages
                // inside the application is the coverage promise, and the opt-out is the flag a
                // client has to send. An older client that never heard of it gets the promise.
                assert!(target.embedded, "a missing embedded must mean reach the pages");
            }
            _ => panic!("expected a start command"),
        }
    }

    #[test]
    fn command_start_round_trips() {
        let cmd = Command::Start {
            v: PROTOCOL_VERSION,
            id: 1,
            target: TargetSpec { path: "C:/app.exe".into(), args: vec!["--x".into()], cwd: None, embedded: false },
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
                scale_qpc: false,
            },
            force: false,
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
            kind: UNIT_PROCESS.into(),
            covered: vec![CoveredChannel { channel: "GetSystemTime".into(), calls: 3 }],
            observed: vec![CoveredChannel { channel: "WaitForSingleObject".into(), calls: 5 }],
            uncovered: vec![],
            unobserved: vec!["WaitOnAddress".into()],
            installed_late: vec!["timeGetTime".into()],
            warning_keys: vec!["wait.object_waits_not_scaled".into()],
        };
        let line = ev.to_ndjson();
        assert!(line.contains(r#""observed""#), "observed must serialize, got {line}");
        assert!(line.contains(r#""unobserved""#), "unobserved must serialize, got {line}");
        assert!(line.contains(r#""installed_late""#), "installed_late must serialize, got {line}");
        match parse_event(&line).unwrap() {
            Event::Coverage { observed, unobserved, installed_late, warning_keys, .. } => {
                assert_eq!(observed.len(), 1);
                assert_eq!(observed[0].channel, "WaitForSingleObject");
                assert_eq!(observed[0].calls, 5);
                assert_eq!(unobserved, vec!["WaitOnAddress".to_string()]);
                assert_eq!(installed_late, vec!["timeGetTime".to_string()]);
                assert_eq!(warning_keys, vec!["wait.object_waits_not_scaled".to_string()]);
            }
            _ => panic!("wrong event variant"),
        }
        // A coverage message from before `observed`, `unobserved` and `installed_late` existed still
        // parses (serde default on all three), which is what makes each of them an additive change
        // rather than a break.
        let old = r#"{"type":"coverage","v":1,"pid":42,"covered":[],"uncovered":[],"warning_keys":[]}"#;
        match parse_event(old).unwrap() {
            Event::Coverage { observed, unobserved, installed_late, .. } => {
                assert!(observed.is_empty());
                assert!(unobserved.is_empty());
                assert!(installed_late.is_empty());
            }
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
            warning_keys: vec!["coverage.pid_registry_full".into()],
            uncovered_children: vec![
                UncoveredChild {
                    pid: 4242,
                    parent_pid: 100,
                    image: Some("helper.exe".into()),
                    role: Some("renderer".into()),
                },
                UncoveredChild { pid: 4243, parent_pid: 100, image: None, role: None },
            ],
            uncovered_children_total: 3,
            context_count: 2,
            engines: vec![ReachedEngine { pid: 8072, port: 51234, browser: "Engine/1.0".into() }],
        };
        let line = ev.to_ndjson();
        assert!(line.starts_with(r#"{"type":"session_verdict""#), "got {line}");
        assert!(line.contains(r#""context_count":2"#), "got {line}");
        assert!(line.contains(r#""engines":[{"pid":8072,"port":51234,"browser":"Engine/1.0"}]"#), "got {line}");
        // An unnamed child carries no `image` key at all, rather than a null the panel would render.
        assert!(line.contains(r#"{"pid":4243,"parent_pid":100}"#), "got {line}");
        match parse_event(&line).unwrap() {
            Event::SessionVerdict {
                verdict,
                process_count,
                warning_keys,
                uncovered_children,
                uncovered_children_total,
                ..
            } => {
                assert_eq!(verdict, "works");
                assert_eq!(process_count, 2);
                assert_eq!(warning_keys, vec!["coverage.pid_registry_full".to_string()]);
                assert_eq!(uncovered_children.len(), 2);
                assert_eq!(uncovered_children[0].image.as_deref(), Some("helper.exe"));
                assert_eq!(uncovered_children[0].role.as_deref(), Some("renderer"));
                assert_eq!(uncovered_children[1].image, None);
                assert_eq!(uncovered_children[1].role, None);
                assert_eq!(uncovered_children_total, 3);
            }
            _ => panic!("wrong event variant"),
        }
        match parse_event(&line).unwrap() {
            Event::SessionVerdict { context_count, engines, .. } => {
                assert_eq!(context_count, 2);
                assert_eq!(engines, vec![ReachedEngine { pid: 8072, port: 51234, browser: "Engine/1.0".into() }]);
            }
            _ => panic!("wrong event variant"),
        }
    }

    /// R2-S9. `warning_keys` was added to session_verdict after the field set was already in use, so a
    /// message without it has to keep parsing - additive evolution, not a new schema (zasady/15).
    #[test]
    fn a_session_verdict_without_warning_keys_still_parses() {
        let line = r#"{"type":"session_verdict","v":1,"verdict":"works","reason_key":"session.family_covered","process_count":1}"#;
        match parse_event(line).expect("an older core's message must still parse") {
            Event::SessionVerdict {
                warning_keys, process_count, uncovered_children, uncovered_children_total, ..
            } => {
                assert!(warning_keys.is_empty(), "absent means no warnings, never a parse failure");
                assert_eq!(process_count, 1);
                // The same rule for the two fields added after it: absent is empty, not a failure.
                assert!(uncovered_children.is_empty());
                assert_eq!(uncovered_children_total, 0);
            }
            _ => panic!("wrong event variant"),
        }
        // And for the two the embedded-engine channel added (docs/09 section 12.4).
        match parse_event(line).unwrap() {
            Event::SessionVerdict { context_count, engines, .. } => {
                assert_eq!(context_count, 0);
                assert!(engines.is_empty());
            }
            _ => panic!("wrong event variant"),
        }
    }

    /// `coverage.kind` names the namespace of `pid`: a process the hook is inside, or a JS context
    /// reached over the DevTools protocol. A message from before the field existed came from a
    /// process, so absent reads as that - never as a parse failure, never as a context.
    #[test]
    fn a_coverage_without_kind_is_a_process_and_kind_round_trips() {
        let old = r#"{"type":"coverage","v":1,"pid":42,"covered":[],"uncovered":[],"warning_keys":[]}"#;
        match parse_event(old).unwrap() {
            Event::Coverage { kind, pid, .. } => {
                assert_eq!(kind, UNIT_PROCESS);
                assert_eq!(pid, 42);
            }
            _ => panic!("wrong event variant"),
        }
        let ev = Event::Coverage {
            v: PROTOCOL_VERSION,
            pid: 3,
            kind: UNIT_CONTEXT.into(),
            covered: vec![CoveredChannel { channel: "page Date.now".into(), calls: 12 }],
            observed: vec![],
            uncovered: vec![],
            unobserved: vec![],
            installed_late: vec![],
            warning_keys: vec![],
        };
        let line = ev.to_ndjson();
        assert!(line.contains(r#""pid":3,"kind":"context""#), "kind rides beside the number it qualifies: {line}");
        match parse_event(&line).unwrap() {
            Event::Coverage { kind, .. } => assert_eq!(kind, UNIT_CONTEXT),
            _ => panic!("wrong event variant"),
        }
    }

    #[test]
    fn ended_carries_timing_and_defaults_when_absent() {
        let ev = Event::Ended {
            v: PROTOCOL_VERSION,
            clean: true,
            residue_keys: vec![],
            target_exit_code: Some(0),
            elapsed_real_ms: 1500,
            elapsed_fake_ms: 90000,
            fake_end_wall: Some("2038-01-19 03:15:07".into()),
        };
        let line = ev.to_ndjson();
        assert!(line.contains(r#""elapsed_real_ms":1500"#), "got {line}");
        assert!(line.contains(r#""fake_end_wall":"2038-01-19 03:15:07""#), "got {line}");
        // An older ended line without the timing fields still parses (serde default).
        let old = r#"{"type":"ended","v":1,"clean":true,"residue_keys":[],"target_exit_code":null}"#;
        match parse_event(old).unwrap() {
            Event::Ended { elapsed_real_ms, fake_end_wall, .. } => {
                assert_eq!(elapsed_real_ms, 0);
                assert!(fake_end_wall.is_none());
            }
            _ => panic!("wrong event variant"),
        }
    }
}
