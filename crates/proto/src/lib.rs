//! Chrono Mock machine protocol (ADR-6, docs/08) - NDJSON over a byte stream.
//!
//! One JSON object per line. The core writes events on its stdout, reads commands
//! on its stdin. `stdout` is protocol only, `stderr` is human diagnostics.
//!
//! The commands are [`Command`] and the events [`Event`], one variant per message type. docs/08
//! names what each one means.
//!
//! `v` is repeated per message on purpose - a flat, unambiguous wire shape beats a
//! clever envelope that trips serde's flatten + internal-tag edge cases.

use serde::{Deserialize, Serialize};

/// What the random properties share - edges, risky text, the weekly canary.
#[cfg(test)]
mod search;

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
    /// Reach the web pages of an application that runs as administrator (docs/09 section 12.19): an
    /// elevated WebView2 host ignores the variable `embedded` relies on, so the session writes one value
    /// under the machine registry's WebView2 policy for its duration and removes it at the end. Off by
    /// default, because it changes the machine and opens a debugging port in an application that has
    /// administrator rights - a client from before this field existed gets that default. Needs a core
    /// that is itself elevated and a target that is an `.exe`, and says in the session's report when it
    /// could not do what was asked. Ignored for a Chromium target and under `embedded: false`.
    #[serde(default)]
    pub elevated_embedded: bool,
    /// Start the web engine with its slowdown of timers in hidden windows switched off (R4-N28): a
    /// Chromium engine runs the timers of a minimized or hidden window about once a second, which takes
    /// the speed-up away from them. Applies to the browser of a Chromium target and to the engine inside
    /// an application. Off by default, because the application then also works at full pace while
    /// hidden - a client from before this field existed gets that default. Without it, a page hidden
    /// while time ran faster is reported (`chromium.background_timers_slowed`).
    #[serde(default)]
    pub keep_background_timers: bool,
    /// Leave the sandbox of an application on Mozilla's Gecko engine as it is
    /// (ADR-28). Gecko runs its pages in processes it starts under a restricted token, which the hook does
    /// not enter, so they stay on the real clock. `false` starts the target with the variables that switch
    /// that sandbox off, which the processes it starts inherit, and the hook enters them like any child -
    /// at the cost of the protection, for as long as those processes run. On by default, and a client from
    /// before this field existed gets that default: a sandbox is switched off only when asked.
    #[serde(default = "gecko_sandbox_default")]
    pub gecko_sandbox: bool,
    /// Where a console program's console is (docs/08, ADR-17): `shared` gives it the core's console
    /// and stderr, `new` a console window of its own. Neither hands it the core's stdin or stdout,
    /// which carry this protocol (R4-W1). A client from before this field existed gets `shared`, and
    /// a value this build does not know makes the whole `start` unreadable, so it is refused rather
    /// than guessed at. Ignored for a Chromium target, whose browser gets no standard handles.
    #[serde(default)]
    pub console: TargetConsole,
}

/// The two places a console program's console can be (see [`TargetSpec::console`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetConsole {
    /// The core's own console and stderr: a terminal's, when the client runs in one.
    #[default]
    Shared,
    /// A console window of its own, for a client with no console to share (the GUI).
    New,
}

/// The default for [`TargetSpec::embedded`]: reach the pages.
fn embedded_default() -> bool {
    true
}

fn gecko_sandbox_default() -> bool {
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

/// Leaves a zero count off the wire, where an absent field already reads as zero.
fn is_zero(n: &u64) -> bool {
    *n == 0
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
    /// How many pages and workers the session put on its clock through this endpoint - the share of
    /// `session_verdict.context_count` that is this engine's. Zero for an endpoint the session attached
    /// to and shimmed nothing in, so a client claims an engine's pages only for the engine that had
    /// some: with two engines reached, the total alone says nothing about either (R5, P15). Absent in
    /// messages from an older core, which reads as zero - the side that claims nothing.
    #[serde(default)]
    pub context_count: u32,
}

/// What a [`MomentSpec`] names: a wall-clock moment, or a step from the session's own fake now.
///
/// A word this build does not know is read as [`MomentKind::Unknown`] rather than refused at parsing,
/// so the command still gets its own answer - `moment.unsupported_kind` with its id - instead of the
/// anonymous `protocol.bad_command_ignored` a malformed line gets (R5-P18). The wire spelling is the
/// lowercase word, as it was while the field was a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MomentKind {
    /// `local` read in the zone `tz_bias_min` names, or in the session's when it names none.
    Absolute,
    /// `delta`, one step in the calculator's grammar, from the session's fake now. Jumps only.
    Relative,
    /// Any other word. Sent by no client of this build.
    #[serde(other)]
    Unknown,
}

/// The time flow a `start` asks for. A word this build does not know is [`TimeModeKind::Unknown`],
/// which the core refuses as `time.bad_mode` (R5-P18), as it did while the field was a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeModeKind {
    /// Real speed from the moment.
    Flow,
    /// The clock held at the moment.
    Frozen,
    /// `multiplier` times real speed, zero being frozen.
    Multiplier,
    /// Any other word. Sent by no client of this build.
    #[serde(other)]
    Unknown,
}

/// What a `query` asks for. Every query is answered with a `state` snapshot, whatever it names - an
/// unknown word included, which is [`QueryWhat::Unknown`] and answered all the same (R5-P18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryWhat {
    /// The session's clock as it stands.
    State,
    /// Any other word.
    #[serde(other)]
    Unknown,
}

/// The target moment (session-zone semantics, docs/01 section 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentSpec {
    pub kind: MomentKind,
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
    pub mode: TimeModeKind,
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
    /// The count stopped at the hook's ceiling, so the target called the channel AT LEAST `calls` times
    /// (R5-WY11). Absent when false, and an older message without it reads as an exact count. Additive.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub capped: bool,
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

/// A process on the session clock that was still running when the target closed, so the session
/// went on for it (ADR-16). `image` is the executable's file name when the process list had it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowedProcess {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
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
    /// Ask for an immediate `state` snapshot.
    Query {
        v: u32,
        id: u64,
        what: QueryWhat,
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
        /// With `refuse_start`, the processes of the application the core could not end, so they are
        /// still running on the real clock (R4-S5): ones it had no right to end, ones that did not end in
        /// time, and children the hook named that could not be confirmed as the application's. Empty on
        /// every other verdict and then absent from the message. Additive: an older message without it
        /// reads as empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        left_running: Vec<FollowedProcess>,
        /// With `refuse_start`, set when the core could not look for all of the application's processes:
        /// the process list would not be read, the launched process would not say when it was created, or
        /// the application was still starting processes after the last round. Processes it started may
        /// then run on the real clock without being named in `left_running`. False on every other verdict
        /// and then absent from the message. Additive: an older message without it reads as false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        family_search_incomplete: bool,
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
        /// The processes the session went on for after the target closed (ADR-16) - a launcher's
        /// application, or a helper that kept the session open. Empty when nothing outlived the
        /// target, and for a session that ended while the target ran. Additive.
        #[serde(default)]
        followed: Vec<FollowedProcess>,
        /// Processes outside the family running the same executable file as a windowed member, found
        /// when the family ended on its own without ever showing a window: a single-instance application
        /// most likely handed its work to one of them after the guard window, and it runs on the real
        /// clock (ADR-4). Each counts against the verdict like an uncovered process. Empty for every other
        /// session, and for a Chromium session. Additive, left out when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        copies_outside: Vec<FollowedProcess>,
        /// How long the machine slept during the session, in milliseconds, when it slept at least a
        /// second (R5-P1, R5-D34) - the time the session clock stood still (ADR-5), said beside the
        /// `time.host_slept` warning. Absent when the machine did not sleep. Additive.
        #[serde(default, skip_serializing_if = "is_zero")]
        host_slept_ms: u64,
        /// What the browser a Chromium session drove calls itself (`Browser.getVersion`, e.g.
        /// `Chrome/152.0.7977.149`), cleaned - which engine the evidence is about (R5-D6, R5-D44).
        /// Absent for a native session, and when the browser did not say. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        browser: Option<String>,
    },
    /// The machine slept while the session ran, noticed at the first heartbeat after it woke (R5-P1,
    /// R5-D34). The session clock stood still for that time - sleep is never multiplied by the speed
    /// (ADR-5, docs/01 section 3.0a) - and this is the record of it in the session's log. `slept_ms` is
    /// this sleep, `total_slept_ms` every sleep of the session so far. Spontaneous, no id. Additive: a
    /// client that does not know the type ignores the line.
    HostSlept {
        v: u32,
        slept_ms: u64,
        total_slept_ms: u64,
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
        serde_json::to_string(self).unwrap_or_else(|_| serialize_failed_line(PROTOCOL_VERSION))
    }
}

/// The line [`Event::to_ndjson`] falls back to, at protocol version `v`. The version is a parameter
/// rather than a digit in the text: the fallback used to say `"v":1` whatever the protocol was, and a
/// test of the text alone cannot tell the two apart while the protocol is 1 (R5-P18).
fn serialize_failed_line(v: u32) -> String {
    format!(r#"{{"type":"error","v":{v},"code":3,"key":"proto.serialize_failed","origin":"proto"}}"#)
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
                // The opposite again for the option that writes the machine registry: a client that
                // never heard of it must not get a write it did not ask for.
                assert!(!target.elevated_embedded, "a missing elevated_embedded must mean no registry write");
                // And for the engine switches that change how the application behaves while hidden
                // (R4-N28): asked for, never assumed.
                assert!(!target.keep_background_timers, "a missing keep_background_timers must leave the engine as it is");
                // And for the sandbox of a Gecko application (ADR-28): switched off only when asked.
                assert!(target.gecko_sandbox, "a missing gecko_sandbox must leave the sandbox on");
                // A client from before the field keeps the console it always had, and now without the
                // protocol in it (R4-W1).
                assert_eq!(target.console, TargetConsole::Shared, "a missing console must mean shared");
            }
            _ => panic!("expected a start command"),
        }
    }

    #[test]
    fn command_start_round_trips() {
        let cmd = Command::Start {
            v: PROTOCOL_VERSION,
            id: 1,
            target: TargetSpec {
                path: "C:/app.exe".into(),
                args: vec!["--x".into()],
                cwd: None,
                embedded: false,
                elevated_embedded: true,
                keep_background_timers: true,
                gecko_sandbox: false,
                console: TargetConsole::New,
            },
            time: TimeSpec {
                moment: MomentSpec {
                    kind: MomentKind::Absolute,
                    local: Some("2038-01-19T03:14:07".into()),
                    tz_bias_min: Some(-120),
                    delta: None,
                },
                mode: TimeModeKind::Multiplier,
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
            Command::Start { id, time, target, .. } => {
                assert_eq!(id, 1);
                assert_eq!(time.multiplier, Some(60));
                assert_eq!(target.console, TargetConsole::New);
                assert!(target.elevated_embedded, "the option has to survive the wire");
            }
            _ => panic!("wrong command variant"),
        }
        assert!(line.contains(r#""console":"new""#), "{line}");
        assert!(line.contains(r#""elevated_embedded":true"#), "{line}");
    }

    /// A console value this build does not know makes the `start` unreadable, so the core refuses it
    /// the way it refuses any malformed command - never guessing which console was meant.
    #[test]
    fn a_start_with_an_unknown_console_is_not_read() {
        let line = r#"{"type":"start","v":1,"id":1,"target":{"path":"C:/app.exe","console":"window"},
            "time":{"moment":{"kind":"absolute","local":"2038-01-19T03:14:07","tz_bias_min":0,"delta":null},
            "mode":"flow","multiplier":null,"scale_duration":false,"scale_qpc":false}}"#;
        assert!(parse_command(line).is_err());
        assert!(parse_command(&line.replace("window", "shared")).is_ok());
    }

    /// The opposite of the console: a moment kind, a mode or a query word this build does not know
    /// still reads, as `Unknown`, so the core answers that one command with its id and its own key
    /// (`moment.unsupported_kind`, `time.bad_mode`) rather than dropping the line as malformed (R5-P18).
    /// And the words it knows go out as the lowercase strings the field carried while it was one.
    #[test]
    fn an_unknown_kind_mode_or_query_word_is_read_as_unknown_and_known_ones_keep_their_spelling() {
        let start = r#"{"type":"start","v":1,"id":4,"target":{"path":"C:/app.exe"},
            "time":{"moment":{"kind":"sideways","local":"2038-01-19T03:14:07"},"mode":"backwards"}}"#;
        match parse_command(start).expect("an unknown kind and mode still read") {
            Command::Start { id, time, .. } => {
                assert_eq!((id, time.moment.kind, time.mode), (4, MomentKind::Unknown, TimeModeKind::Unknown));
            }
            other => panic!("expected a start, got {other:?}"),
        }
        let query = parse_command(r#"{"type":"query","v":1,"id":5,"what":"everything"}"#).expect("an unknown query word reads");
        assert!(matches!(query, Command::Query { id: 5, what: QueryWhat::Unknown, .. }), "{query:?}");

        let words = [
            (serde_json::to_string(&MomentKind::Absolute), "\"absolute\""),
            (serde_json::to_string(&MomentKind::Relative), "\"relative\""),
            (serde_json::to_string(&TimeModeKind::Flow), "\"flow\""),
            (serde_json::to_string(&TimeModeKind::Frozen), "\"frozen\""),
            (serde_json::to_string(&TimeModeKind::Multiplier), "\"multiplier\""),
            (serde_json::to_string(&QueryWhat::State), "\"state\""),
        ];
        for (written, expected) in words {
            assert_eq!(written.expect("a unit variant serialises"), expected);
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
            left_running: vec![],
            family_search_incomplete: false,
        };
        let line = ev.to_ndjson();
        assert!(!line.contains("\"id\""), "null id must be omitted, got {line}");
    }

    /// R4-S5: a refusal names what it could not end, and every other verdict reads exactly as before -
    /// no empty list on the wire, and a message from before the field reads as empty.
    #[test]
    fn a_refusal_names_what_it_left_running_and_other_verdicts_do_not_change() {
        let refused = Event::Verdict {
            v: PROTOCOL_VERSION,
            id: Some(1),
            verdict: "fails".into(),
            refuse_start: true,
            reason_key: "coverage.time_channels_uncovered".into(),
            left_running: vec![
                FollowedProcess { pid: 5150, image: Some("helper.exe".into()) },
                FollowedProcess { pid: 5151, image: None },
            ],
            family_search_incomplete: true,
        };
        match parse_event(&refused.to_ndjson()).unwrap() {
            Event::Verdict { left_running, refuse_start, family_search_incomplete, .. } => {
                assert!(refuse_start);
                assert!(family_search_incomplete, "the incomplete search was lost on the wire");
                assert_eq!(left_running.len(), 2);
                assert_eq!(left_running[0], FollowedProcess { pid: 5150, image: Some("helper.exe".into()) });
                assert_eq!(left_running[1], FollowedProcess { pid: 5151, image: None });
            }
            other => panic!("expected a verdict, got {other:?}"),
        }
        let works = Event::Verdict {
            v: PROTOCOL_VERSION,
            id: Some(1),
            verdict: "works".into(),
            refuse_start: false,
            reason_key: "coverage.time_channels_covered".into(),
            left_running: vec![],
            family_search_incomplete: false,
        };
        assert!(!works.to_ndjson().contains("left_running"), "an empty list went on the wire");
        assert!(!works.to_ndjson().contains("family_search_incomplete"), "a false flag went on the wire");
        let older = r#"{"type":"verdict","v":1,"id":1,"verdict":"works","refuse_start":false,"reason_key":"x"}"#;
        match parse_event(older).unwrap() {
            Event::Verdict { left_running, family_search_incomplete, .. } => {
                assert!(left_running.is_empty());
                assert!(!family_search_incomplete);
            }
            other => panic!("expected a verdict, got {other:?}"),
        }
    }

    #[test]
    fn coverage_observed_round_trips_and_defaults() {
        let ev = Event::Coverage {
            v: PROTOCOL_VERSION,
            pid: 42,
            kind: UNIT_PROCESS.into(),
            covered: vec![CoveredChannel { channel: "GetSystemTime".into(), calls: 3, capped: false }],
            observed: vec![CoveredChannel { channel: "WaitForSingleObject".into(), calls: 5, capped: false }],
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
                kind: MomentKind::Absolute,
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
            engines: vec![ReachedEngine { pid: 8072, port: 51234, browser: "Engine/1.0".into(), context_count: 2 }],
            followed: vec![
                FollowedProcess { pid: 5150, image: Some("app.exe".into()) },
                FollowedProcess { pid: 5151, image: None },
            ],
            copies_outside: vec![],
            host_slept_ms: 0,
            browser: None,
        };
        let line = ev.to_ndjson();
        assert!(line.starts_with(r#"{"type":"session_verdict""#), "got {line}");
        assert!(line.contains(r#""context_count":2"#), "got {line}");
        assert!(
            line.contains(r#""engines":[{"pid":8072,"port":51234,"browser":"Engine/1.0","context_count":2}]"#),
            "got {line}"
        );
        // A process the list did not name carries no `image` key, as an unnamed child does.
        assert!(line.contains(r#""followed":[{"pid":5150,"image":"app.exe"},{"pid":5151}]"#), "got {line}");
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
            Event::SessionVerdict { context_count, engines, followed, .. } => {
                assert_eq!(context_count, 2);
                assert_eq!(
                    engines,
                    vec![ReachedEngine { pid: 8072, port: 51234, browser: "Engine/1.0".into(), context_count: 2 }]
                );
                assert_eq!(followed.len(), 2);
                assert_eq!(followed[0], FollowedProcess { pid: 5150, image: Some("app.exe".into()) });
                assert_eq!(followed[1].image, None);
            }
            _ => panic!("wrong event variant"),
        }
    }

    /// ADR-4, 2026-11-11. The copies a single-instance application most likely handed its work to travel
    /// like the processes a session went on for, a copy without a name carries no `image` key, and an
    /// empty list stays off the wire, so the line of every other session is the one it was.
    #[test]
    fn the_copies_outside_travel_and_an_empty_list_stays_off_the_wire() {
        let copies = vec![FollowedProcess { pid: 4600, image: Some("app.exe".into()) }, FollowedProcess { pid: 4601, image: None }];
        let ev = Event::SessionVerdict {
            v: PROTOCOL_VERSION,
            verdict: "partial".into(),
            reason_key: "session.family_handed_to_copy".into(),
            process_count: 1,
            warning_keys: vec!["session.handed_to_copy".into()],
            uncovered_children: vec![],
            uncovered_children_total: 0,
            context_count: 0,
            engines: vec![],
            followed: vec![],
            copies_outside: copies.clone(),
            host_slept_ms: 0,
            browser: None,
        };
        let line = ev.to_ndjson();
        assert!(line.contains(r#""copies_outside":[{"pid":4600,"image":"app.exe"},{"pid":4601}]"#), "got {line}");
        let Event::SessionVerdict { copies_outside, .. } = parse_event(&line).unwrap() else { panic!("wrong event variant") };
        assert_eq!(copies_outside, copies);
        let older = r#"{"type":"session_verdict","v":1,"verdict":"works","reason_key":"session.family_covered","process_count":1}"#;
        let parsed = parse_event(older).unwrap();
        let Event::SessionVerdict { copies_outside, .. } = &parsed else { panic!("wrong event variant") };
        assert!(copies_outside.is_empty(), "absent means no copies, never a parse failure");
        assert!(!parsed.to_ndjson().contains("copies_outside"), "an empty list went on the wire");
    }

    /// R5-P1, R5-D34, R5-WY11. The sleep travels as its own event and as a total on the verdict, which an
    /// older message lacks and reads as no sleep, and a count at the ceiling says so beside the number,
    /// left off the wire when it is exact.
    #[test]
    fn the_sleep_and_the_ceiling_travel_and_older_messages_read_as_none() {
        let slept = Event::HostSlept { v: 1, slept_ms: 28_800_000, total_slept_ms: 28_860_000 };
        let line = slept.to_ndjson();
        assert_eq!(line, r#"{"type":"host_slept","v":1,"slept_ms":28800000,"total_slept_ms":28860000}"#);
        match parse_event(&line).expect("the event parses back") {
            Event::HostSlept { slept_ms, total_slept_ms, .. } => assert_eq!((slept_ms, total_slept_ms), (28_800_000, 28_860_000)),
            _ => panic!("wrong event variant"),
        }
        let older = r#"{"type":"session_verdict","v":1,"verdict":"works","reason_key":"session.family_covered","process_count":1}"#;
        let parsed = parse_event(older).unwrap();
        match &parsed {
            Event::SessionVerdict { host_slept_ms, .. } => assert_eq!(*host_slept_ms, 0),
            _ => panic!("wrong event variant"),
        }
        assert!(!parsed.to_ndjson().contains("host_slept_ms"), "no sleep leaves the field off the wire");
        let exact = CoveredChannel { channel: "GetSystemTime".into(), calls: 3, capped: false };
        assert!(!serde_json::to_string(&exact).unwrap().contains("capped"), "an exact count carries no flag");
        let floor = CoveredChannel { channel: "GetSystemTime".into(), calls: 1_048_576, capped: true };
        assert!(serde_json::to_string(&floor).unwrap().contains(r#""capped":true"#));
        let old: CoveredChannel = serde_json::from_str(r#"{"channel":"GetSystemTime","calls":7}"#).unwrap();
        assert!(!old.capped, "an older message reads as an exact count");
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
        // And for the two the embedded-engine channel added (docs/09 section 12.4), and the list of
        // processes a session went on for (ADR-16).
        match parse_event(line).unwrap() {
            Event::SessionVerdict { context_count, engines, followed, .. } => {
                assert_eq!(context_count, 0);
                assert!(engines.is_empty());
                assert!(followed.is_empty());
            }
            _ => panic!("wrong event variant"),
        }
        // An engine named by a core older than its per-engine count: the count reads as zero, the side
        // on which a client claims none of its pages (R5, P15).
        let older = r#"{"type":"session_verdict","v":1,"verdict":"partial","reason_key":"session.family_partial","process_count":1,"engines":[{"pid":8072,"port":51234,"browser":"Engine/1.0"}]}"#;
        match parse_event(older).expect("an engine without its count must still parse") {
            Event::SessionVerdict { engines, .. } => assert_eq!(engines[0].context_count, 0),
            _ => panic!("wrong event variant"),
        }
    }

    /// R5-D44: the browser a Chromium session drove travels on its verdict, and a verdict without it -
    /// a native session's, or an older core's - parses as none and writes no key.
    #[test]
    fn the_browser_of_a_chromium_session_round_trips_and_is_absent_otherwise() {
        let older = r#"{"type":"session_verdict","v":1,"verdict":"works","reason_key":"session.family_covered","process_count":1}"#;
        let parsed = parse_event(older).unwrap();
        assert!(matches!(&parsed, Event::SessionVerdict { browser: None, .. }));
        assert!(!parsed.to_ndjson().contains("browser"), "no browser, no key");
        let with = older.replace(r#""process_count":1"#, r#""process_count":1,"browser":"Chrome/152.0.7977.149""#);
        let parsed = parse_event(&with).unwrap();
        assert!(matches!(&parsed, Event::SessionVerdict { browser: Some(b), .. } if b == "Chrome/152.0.7977.149"));
        assert!(parsed.to_ndjson().contains(r#""browser":"Chrome/152.0.7977.149""#));
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
            covered: vec![CoveredChannel { channel: "page Date.now".into(), calls: 12, capped: false }],
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

    /// R5-P18: the fallback line is an error event a client reads like any other, at the version it is
    /// given - checked at a version that is not 1, so a digit written into the text cannot pass for it.
    #[test]
    fn the_fallback_line_is_an_error_at_the_version_it_is_given() {
        for v in [PROTOCOL_VERSION, 7] {
            match parse_event(&serialize_failed_line(v)).expect("the fallback parses as an event") {
                Event::Error { v: got, id, code, key, origin } => {
                    assert_eq!(got, v);
                    assert_eq!(id, None);
                    assert_eq!(code, 3);
                    assert_eq!(key, "proto.serialize_failed");
                    assert_eq!(origin, "proto");
                }
                other => panic!("wrong event variant: {other:?}"),
            }
        }
    }

    mod properties {
        use super::*;
        use crate::search::{edge_i64, edge_u64, text};
        use proptest::collection::vec;
        use proptest::option;
        use proptest::prelude::*;

        fn edge_u32() -> impl Strategy<Value = u32> {
            prop_oneof![3 => any::<u32>(), 1 => 0u32..=2, 1 => (u32::MAX - 2)..=u32::MAX]
        }

        fn edge_i32() -> impl Strategy<Value = i32> {
            prop_oneof![3 => any::<i32>(), 1 => -2i32..=2, 1 => (i32::MAX - 2)..=i32::MAX, 1 => i32::MIN..=(i32::MIN + 2)]
        }

        /// A list as short as a field that is left off the wire when empty needs, and no longer.
        fn few<S: Strategy>(item: S) -> impl Strategy<Value = Vec<S::Value>> {
            vec(item, 0..3)
        }

        fn channel() -> impl Strategy<Value = CoveredChannel> {
            (text(), edge_u64(), any::<bool>()).prop_map(|(channel, calls, capped)| CoveredChannel { channel, calls, capped })
        }

        fn followed() -> impl Strategy<Value = FollowedProcess> {
            (edge_u32(), option::of(text())).prop_map(|(pid, image)| FollowedProcess { pid, image })
        }

        fn uncovered_child() -> impl Strategy<Value = UncoveredChild> {
            (edge_u32(), edge_u32(), option::of(text()), option::of(text()))
                .prop_map(|(pid, parent_pid, image, role)| UncoveredChild { pid, parent_pid, image, role })
        }

        fn engine() -> impl Strategy<Value = ReachedEngine> {
            (edge_u32(), any::<u16>(), text(), edge_u32())
                .prop_map(|(pid, port, browser, context_count)| ReachedEngine { pid, port, browser, context_count })
        }

        fn clock() -> impl Strategy<Value = Clock> {
            (text(), edge_i32()).prop_map(|(wall, zone_bias_min)| Clock { wall, zone_bias_min })
        }

        fn coverage() -> impl Strategy<Value = Event> {
            let kind = prop_oneof![Just(UNIT_PROCESS.to_string()), Just(UNIT_CONTEXT.to_string()), text()];
            let lists = (few(channel()), few(channel()), few(text()), few(text()), few(text()), few(text()));
            (edge_u32(), edge_u32(), kind, lists).prop_map(
                |(v, pid, kind, (covered, observed, uncovered, unobserved, installed_late, warning_keys))| Event::Coverage {
                    v,
                    pid,
                    kind,
                    covered,
                    observed,
                    uncovered,
                    unobserved,
                    installed_late,
                    warning_keys,
                },
            )
        }

        fn verdict() -> impl Strategy<Value = Event> {
            (edge_u32(), option::of(edge_u64()), text(), any::<bool>(), text(), few(followed()), any::<bool>()).prop_map(
                |(v, id, verdict, refuse_start, reason_key, left_running, family_search_incomplete)| Event::Verdict {
                    v,
                    id,
                    verdict,
                    refuse_start,
                    reason_key,
                    left_running,
                    family_search_incomplete,
                },
            )
        }

        fn session_verdict() -> impl Strategy<Value = Event> {
            let first = (edge_u32(), text(), text(), edge_u32(), few(text()), few(uncovered_child()));
            let rest =
                (edge_u32(), edge_u32(), few(engine()), few(followed()), few(followed()), edge_u64(), option::of(text()));
            (first, rest).prop_map(
                |(
                    (v, verdict, reason_key, process_count, warning_keys, uncovered_children),
                    (uncovered_children_total, context_count, engines, followed, copies_outside, host_slept_ms, browser),
                )| Event::SessionVerdict {
                    v,
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
                },
            )
        }

        fn ended() -> impl Strategy<Value = Event> {
            (edge_u32(), any::<bool>(), few(text()), option::of(edge_i32()), edge_i64(), edge_i64(), option::of(text())).prop_map(
                |(v, clean, residue_keys, target_exit_code, elapsed_real_ms, elapsed_fake_ms, fake_end_wall)| Event::Ended {
                    v,
                    clean,
                    residue_keys,
                    target_exit_code,
                    elapsed_real_ms,
                    elapsed_fake_ms,
                    fake_end_wall,
                },
            )
        }

        /// Every event the core writes, each of its optional and defaulted fields both present and
        /// absent, its numbers at their edges and its text with what a one-line framing must survive.
        fn event() -> impl Strategy<Value = Event> {
            let ready = (edge_u32(), edge_u32(), text(), text(), few(text())).prop_map(
                |(v, protocol, core_version, bitness, capabilities)| Event::Ready { v, protocol, core_version, bitness, capabilities },
            );
            let ack = (edge_u32(), edge_u64()).prop_map(|(v, id)| Event::Ack { v, id });
            let state = (edge_u32(), clock(), clock(), edge_i64(), edge_i64(), edge_i64()).prop_map(
                |(v, fake, real, multiplier, elapsed_fake_ms, elapsed_real_ms)| Event::State {
                    v,
                    fake,
                    real,
                    multiplier,
                    elapsed_fake_ms,
                    elapsed_real_ms,
                },
            );
            let vanished = (edge_u32(), edge_u32(), text(), edge_u64())
                .prop_map(|(v, pid, reason_key, lived_ms)| Event::Vanished { v, pid, reason_key, lived_ms });
            let host_slept = (edge_u32(), edge_u64(), edge_u64())
                .prop_map(|(v, slept_ms, total_slept_ms)| Event::HostSlept { v, slept_ms, total_slept_ms });
            let error = (edge_u32(), option::of(edge_u64()), edge_i32(), text(), text())
                .prop_map(|(v, id, code, key, origin)| Event::Error { v, id, code, key, origin });
            prop_oneof![
                ready.boxed(),
                coverage().boxed(),
                verdict().boxed(),
                ack.boxed(),
                state.boxed(),
                vanished.boxed(),
                session_verdict().boxed(),
                host_slept.boxed(),
                ended().boxed(),
                error.boxed(),
            ]
        }

        proptest! {
            /// What the core writes is what a client reads, from exactly one line. A field left off the
            /// wire when it is empty, false or zero (`skip_serializing_if`) is additive only while its
            /// reader defaults it - without the default, the core's own most ordinary message (a verdict
            /// that left nothing running) stops parsing. And a line break written into a message ends the
            /// line under every reader of the stream. Both are judged on the value read back, compared as
            /// JSON with the value sent.
            #[test]
            fn every_event_reads_back_as_itself_from_one_line(event in event()) {
                let line = event.to_ndjson();
                prop_assert!(!line.contains(['\n', '\r']), "the event takes more than one line: {:?}", line);
                let back = parse_event(&line);
                prop_assert!(back.is_ok(), "the line does not read back: {} ({:?})", line, back.err());
                let sent = serde_json::to_value(&event).expect("an event is JSON");
                let read = serde_json::to_value(back.expect("checked above")).expect("an event is JSON");
                prop_assert_eq!(read, sent, "for the line {}", line);
            }
        }
    }
}
