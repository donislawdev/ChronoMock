//! `chrono __core`: the hidden core mode, and the only place the mechanism layer is touched.
//!
//! Reads commands off stdin, drives a native session, emits protocol events. It is deliberately the
//! narrowest module in the crate that can crash someone else's process - everything above it
//! (the driver, the calculator, the report) is pure and testable without an injection.
//!
//! The verdict it emits comes from the logical core, not from here: the mechanism collects the
//! evidence and `chrono_core` judges it, which is what keeps untouchable rule 4 checkable.


use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono_core::{
    filetime_utc_to_wall, verdict_from_coverage, Coverage, Moment, SessionSpec, TimeMode, Verdict,
};
use chrono_mech::UncoveredChild;
use chrono_proto::{
    parse_command, Command, Event, MomentSpec, TargetSpec, TimeSpec, PROTOCOL_VERSION,
    UNCOVERED_CHILDREN_WIRE_MAX,
};

use crate::cdp;
use crate::cdp_attach::ShimOrigin;
use crate::cdp_audit::{coverage_events, covered_channels, embedded_verdict};
use crate::cdp_clock::shim_origin_from_state;
use crate::cdp_session::cdp_session;
use crate::cli::{this_bitness, CORE_VERSION};
use crate::embedded::{is_renderer_role, is_web_engine_subprocess, role_from_command_line};
use crate::embedded_bridge::{reconcile_engine_warnings, EmbeddedBridge, Launch, KEY_REGISTRY_ARGUMENTS_HIDDEN};
use crate::events::{
    command_id, emit, emit_coverage, ended_clean, jump_error_key, state_event,
    state_event_from, unsupported_command,
};
use crate::grammar::parse_shift;
use crate::report::detect_runtime_warnings;
use crate::wire::{read_protocol_line, spawn_command_reader};
pub(crate) fn core_mode() -> i32 {
    // Handshake first - before reading any command - so a client can verify `protocol` and
    // `bitness` before it sends `start` (docs/08 section 3). Emitting `ready` ahead of the read also
    // makes the protocol deadlock-proof: a client may gate on `ready` or send `start` first, both work.
    emit(&Event::Ready {
        v: PROTOCOL_VERSION,
        protocol: PROTOCOL_VERSION,
        core_version: CORE_VERSION.into(),
        bitness: this_bitness().into(),
        capabilities: vec![],
    });

    // Read the first command (`start`) from a reader we hand to the session loop
    // afterwards, so it can keep reading subsequent commands (query, end).
    let mut reader = BufReader::new(std::io::stdin());
    let (target, time, force) = match read_start(&mut reader) {
        Ok(start) => start,
        Err(refusal) => {
            emit(&Event::Error {
                v: PROTOCOL_VERSION,
                id: refusal.id,
                code: 1,
                key: refusal.key.into(),
                origin: "core".into(),
            });
            if refusal.needs_ended {
                emit(&ended_clean());
            }
            return 1;
        }
    };

    // A Chromium/Electron target takes the CDP mechanism (ADR-8/ADR-9): its time-dependent logic runs
    // in a sandboxed renderer the native hook cannot reach, and Chromium timers are QPC-based which
    // ADR-2 leaves real. We drive its own JS engine over the DevTools protocol instead, speaking this
    // same machine protocol, so the GUI and CLI consume it identically. `ready` was already emitted
    // (its bitness is this core's own - the driver skips the bitness gate for a Chromium target).
    if cdp::is_chromium_target(&target.path) {
        return cdp_session(target, time, reader);
    }

    // Prepare and start the session (Stage 2: real injection of one wall channel).
    let hook = match hook_dll_path() {
        Some(p) => p,
        None => {
            emit(&Event::Error {
                v: PROTOCOL_VERSION,
                id: Some(1),
                code: 3,
                key: "core.hook_dll_missing".into(),
                origin: "core".into(),
            });
            // Human-side detail on stderr (never on the protocol stdout), naming the folder we looked
            // in. For a portable tool unpacked anywhere, WHERE we looked is the actionable half.
            let looked_in = std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(|d| d.display().to_string()))
                .unwrap_or_else(|| "the folder holding chrono.exe".into());
            eprintln!("chrono core: chrono_hook.dll not found in {looked_in} - this installation is incomplete");
            emit(&ended_clean());
            return 3;
        }
    };

    let spec = match build_spec(&time) {
        Ok(s) => s,
        Err((code, key)) => {
            emit(&Event::Error {
                v: PROTOCOL_VERSION,
                id: Some(1),
                code,
                key: key.into(),
                origin: "core".into(),
            });
            emit(&ended_clean());
            return code;
        }
    };

    // The embedded-engine channel (docs/09): the two variables that make an engine inside the
    // application open a local debugging port, and the discovery that uses it - never one without
    // the other. Off under `--no-embedded`, and then the environment is inherited untouched.
    let launch = Launch::for_target(&target);
    let m_target = chrono_mech::Target {
        path: &target.path,
        args: &target.args,
        cwd: target.cwd.as_deref(),
        env: &launch.env,
    };

    // Detect the target's runtime up front (static, no QPC hook, no process inspection) so the first
    // coverage can warn that its monotonic/elapsed clocks stand on QPC and do not scale - the failure a
    // Python/.NET/Java timer hits under a fast clock (B1, ADR-2). When --scale-qpc is on, this instead
    // cautions about render distortion (A2), since the QPC axis now scales.
    let mut runtime_warnings = detect_runtime_warnings(std::path::Path::new(&target.path), spec.scale_qpc);
    if launch.registry_hidden {
        // Known before the launch, like the runtime: a registry policy the tester set for this
        // application is not read while the session's variable is in its environment.
        runtime_warnings.push(KEY_REGISTRY_ARGUMENTS_HIDDEN.to_string());
    }

    match chrono_mech::prepare(&spec, &m_target, &hook) {
        Ok(prepared) => {
            // Surface an orphan reclaim so it is not silent (a prior core had died and left its
            // control block behind). Human diagnostic on stderr, never on the protocol stdout.
            if prepared.orphan_reclaimed {
                eprintln!("chrono core: reclaimed an orphaned session (a previous core had died)");
            }
            let verdict = verdict_from_coverage(&prepared.coverage);
            // The parent's own coverage (its pid). Children that join later report
            // separately from run_session, each with its own pid and counts.
            emit_coverage(prepared.session.pid, &prepared.coverage, &runtime_warnings);

            // Single-instance vanish (ADR-4): the target exited within the guard
            // window right after injection. Report it honestly with exit 12 rather
            // than trusting the install bits into a false verdict.
            if let Some(lived_ms) = prepared.vanished_lived_ms {
                emit(&Event::Vanished {
                    v: PROTOCOL_VERSION,
                    pid: prepared.session.pid,
                    reason_key: "target.single_instance_suspected".into(),
                    lived_ms,
                });
                prepared.session.end();
                emit(&ended_clean());
                return 12;
            }

            let reason_key = match verdict {
                Verdict::Works => "coverage.time_channels_covered",
                Verdict::Partial => "coverage.time_channels_partial",
                Verdict::Fails => "coverage.time_channels_uncovered",
                Verdict::Undetermined => "coverage.undetermined",
            };
            // The refusal the README promises: a verdict of `fails` means the target read time and saw
            // the REAL clock, so leaving it running produces evidence about a session that never
            // happened - worse than not launching at all. The verdict cannot precede the launch (the
            // only source of truth is a reading from inside the running process), so what is guaranteed
            // is that the tester learns the truth before working in that application, not that a doomed
            // launch never happens. `force` overrides and keeps the session, which every surface then
            // marks unreliable through the verdict it carries.
            let refuse = verdict == Verdict::Fails && !force;
            emit(&Event::Verdict {
                v: PROTOCOL_VERSION,
                id: Some(1),
                verdict: verdict.wire().into(),
                refuse_start: refuse,
                reason_key: reason_key.into(),
            });
            if refuse {
                prepared.session.terminate_target();
                prepared.session.end();
                emit(&ended_clean());
                return verdict.exit_code();
            }
            // Enter the running session: heartbeat, answer queries, end on command,
            // EOF, or target exit.
            run_session(prepared.session, verdict, reader, &launch, spec.scale_duration)
        }
        Err(e) => {
            let (code, key, origin, detail) = map_prepare_error(e);
            emit(&Event::Error {
                v: PROTOCOL_VERSION,
                id: Some(1),
                code,
                key: key.into(),
                origin: origin.into(),
            });
            // Human-side detail on stderr (never on the protocol stdout).
            eprintln!("chrono core: {detail}");
            emit(&ended_clean());
            code
        }
    }
}

/// Why an opening `start` was refused.
///
/// Every one of these is exit code 1 and an error event. What differs is the key, the id it answers
/// (a refusal that arrives before a valid `start` has no id to answer), and whether a session had
/// begun far enough to owe an `ended` before the process leaves.
#[derive(Debug)]
pub(crate) struct StartRefusal {
    key: &'static str,
    id: Option<u64>,
    needs_ended: bool,
}

/// Read and validate the one command that opens a session.
///
/// This is the protocol's front door: the first line any client sends is judged here, and while it
/// lived inside `core_mode` not one of its five refusals had a test, because reaching them meant
/// running the process. The reader is handed back afterwards, so the session loop keeps reading
/// `query`, `set_multiplier`, `jump` and `end` off the same stream.
///
/// The refusal travels as a value rather than an event, which keeps this a pure function of its
/// input and leaves what to emit, and in what order, with the caller.
pub(crate) fn read_start<R: BufRead>(
    reader: &mut R,
) -> Result<(TargetSpec, TimeSpec, bool), StartRefusal> {
    let mut line = String::new();
    // A line past the cap reads as "no command": the stream has stopped making sense, and the honest
    // answer is the refusal below rather than a guess at what the first megabyte meant.
    let n = read_protocol_line(reader, &mut line).unwrap_or(0);
    if n == 0 {
        return Err(StartRefusal { key: "protocol.no_command", id: None, needs_ended: false });
    }
    let Ok(cmd) = parse_command(line.trim_end()) else {
        return Err(StartRefusal { key: "protocol.bad_command", id: None, needs_ended: false });
    };
    match cmd {
        Command::Start { v, target, time, force, .. } => {
            // The version field is in every message BECAUSE the receiver is meant to check it. Nobody
            // did on this side, so it was a field the contract promised and the code ignored (R2-K3) -
            // a client speaking a version this core does not would have been served silently, with
            // whatever its fields happened to mean here. Checked once, at the only command that opens
            // a session: refuse before anything is launched, and say so.
            if v != PROTOCOL_VERSION {
                return Err(StartRefusal {
                    key: "protocol.version_mismatch",
                    id: None,
                    needs_ended: false,
                });
            }
            // A working directory that does not exist. Checked here, before either mechanism starts,
            // because both of them would otherwise report it as a failure of the TARGET: the native
            // path surfaces a bare CreateProcessW error as `target.launch_failed` ("the target
            // application could not be started"), which is true and useless - the target is fine, the
            // folder is not. Checked in the core rather than in the argument parser so it covers the
            // panel as well, which sends the same field over the wire (rule 6).
            if let Some(dir) = target.cwd.as_deref()
                && !std::path::Path::new(dir).is_dir()
            {
                return Err(StartRefusal {
                    key: "target.cwd_missing",
                    id: Some(1),
                    needs_ended: true,
                });
            }
            Ok((target, time, force))
        }
        _ => Err(StartRefusal { key: "protocol.expected_start", id: None, needs_ended: false }),
    }
}

/// What the session loop accumulates between the start verdict and the closing account: the family
/// verdict as processes join, the pids it has seen, the children the hook never followed into, and
/// whether either clock ever stood at the end of its range.
///
/// One value rather than five variables, because `close_session` took every one of them as its own
/// argument, and the embedded-engine channel (docs/09) adds its own share to the same account.
pub(crate) struct SessionLedger {
    family: Verdict,
    family_pids: HashSet<u32>,
    /// Children the hook did NOT follow into, named as they are spawned (SLOWNIK `uncoveredChild`).
    /// Polled at the child cadence for the same reason children are: a short-lived one has to be
    /// asked its name while it is still there.
    uncovered_children: Vec<UncoveredChild>,
    /// Did the fake clock ever stand on the last instant this build can represent (R2-X2)? Sticky:
    /// that the clock STOOD on the edge stays true for this session even if a later jump moves it
    /// back, because the readings taken while it stood there were clamped.
    clock_clamped: bool,
    /// Sticky for the same reason as the wall flag: readings taken while the duration axis stood
    /// there were held, and a later re-anchor does not make them untrue.
    duration_clamped: bool,
}

impl SessionLedger {
    /// Seed the family roll-up with the parent's verdict. Each child's verdict folds in as it joins.
    pub(crate) fn new(parent: Verdict) -> Self {
        SessionLedger {
            family: parent,
            family_pids: HashSet::new(),
            uncovered_children: Vec::new(),
            clock_clamped: false,
            duration_clamped: false,
        }
    }

    /// Poll for children that joined and for children the hook could not follow into.
    pub(crate) fn poll(&mut self, session: &mut chrono_mech::Session) {
        fold_children(session, &mut self.family, &mut self.family_pids);
        self.uncovered_children.extend(session.poll_uncovered_children());
    }

    /// Note whether either clock stands at the end of its range in this sample.
    pub(crate) fn sample(&mut self, st: &chrono_mech::SessionState) {
        self.clock_clamped |= st.clock_at_range_end();
        self.duration_clamped |= st.duration_at_range_end();
    }

    /// Every pid the session knows of: the parent, the children the hook entered, and the children
    /// it did not - the family an engine's debugging port is looked for in (docs/09 section 12.9).
    pub(crate) fn family(&self, parent: u32) -> Vec<u32> {
        std::iter::once(parent)
            .chain(self.family_pids.iter().copied())
            .chain(self.uncovered_children.iter().map(|c| c.pid))
            .collect()
    }
}

/// The host's clock now, as the origin a page is shimmed from.
fn host_origin(session: &chrono_mech::Session, scale_duration: bool) -> ShimOrigin {
    shim_origin_from_state(&session.state(), scale_duration)
}

/// Drive a running session: emit a ~1 s `state` heartbeat, answer `query`, and stop
/// on `end`, on stdin EOF, or when the target exits. Returns the verdict's exit code.
pub(crate) fn run_session(
    mut session: chrono_mech::Session,
    verdict: Verdict,
    reader: BufReader<std::io::Stdin>,
    launch: &Launch,
    scale_duration: bool,
) -> i32 {
    // A reader thread turns stdin lines into commands so the main thread can beat the
    // heartbeat and watch the target without blocking on read_line.
    let rx = spawn_command_reader(reader);

    // Report any child that already joined during the guard window, before the first
    // heartbeat, so a fast child does not wait a whole second to appear.
    let mut ledger = SessionLedger::new(verdict);
    ledger.poll(&mut session);
    // The bridge to the pages inside the application (docs/09 section 12): looks for an engine's
    // debugging port in the family from here on, inert when the channel is off.
    let mut bridge = EmbeddedBridge::start(launch, ledger.family(session.pid), scale_duration);

    let heartbeat = Duration::from_secs(1);
    // Children are polled faster than the heartbeat. A child publishes its evidence in a section
    // that only its own handle keeps alive, so a child shorter-lived than the poll interval takes
    // that evidence with it - and installers, the case ADR-3 exists for, spawn exactly such short
    // helpers. A tenth of a second narrows the window without touching the protocol's once-a-second
    // heartbeat (the section walk is a few memory reads, not I/O).
    let child_poll = Duration::from_millis(100);
    let mut deadline = Instant::now() + heartbeat;
    let mut child_deadline = Instant::now() + child_poll;
    let mut target_exit: Option<i32> = None;

    loop {
        // Both deadlines are also checked after handling a command, not only when the wait times out.
        // With commands arriving back to back, `wait` is zero and recv_timeout keeps returning a command
        // rather than a timeout - so `state` stopped being emitted for as long as the client kept
        // talking, and the GUI's idle watchdog would call a perfectly healthy core unresponsive.
        let wait = deadline.min(child_deadline).saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(Command::End { .. }) => break,
            Ok(cmd) => apply_command(&mut session, &mut bridge, cmd),
            Err(mpsc::RecvTimeoutError::Timeout) => {} // the tick below handles it
            Err(mpsc::RecvTimeoutError::Disconnected) => break, // stdin closed
        }

        let now = Instant::now();
        if now >= child_deadline {
            ledger.poll(&mut session);
            bridge.family(ledger.family(session.pid));
            child_deadline = now + child_poll;
        }
        // Every turn, from the host's clock as it stands now - a page shimmed this turn starts on
        // the same clock as the host. Bounded by the bridge's own budgets (10 ms per engine socket).
        if bridge.is_active() {
            bridge.pump(host_origin(&session, scale_duration));
        }
        // The heartbeat keeps its own once-a-second cadence: `state` and the liveness check stay
        // exactly as often as the protocol says, whatever else the loop is doing.
        if now >= deadline {
            let st = session.state();
            ledger.sample(&st);
            emit(&state_event_from(&st));
            if !session.is_alive() {
                target_exit = session.exit_code();
                break;
            }
            bridge.poll_counts();
            bridge.resync(shim_origin_from_state(&st, scale_duration));

            deadline = now + heartbeat;
        }
    }

    close_session(session, ledger, bridge, target_exit)
}

/// Act on one command that arrived mid-session.
///
/// `end` never reaches here. Whether the loop keeps running is the loop's business rather than a
/// command's effect on the session, so that one arm stays where the decision is made. Everything
/// else is answered: a command this state cannot act on gets `unsupported` WITH its id, so a client
/// waiting on `ack` learns the outcome instead of waiting for ever.
pub(crate) fn apply_command(session: &mut chrono_mech::Session, bridge: &mut EmbeddedBridge, cmd: Command) {
    match cmd {
        Command::Query { id, .. } => {
            emit(&state_event(session));
            emit(&Event::Ack { v: PROTOCOL_VERSION, id });
        }
        Command::SetMultiplier { id, multiplier, .. } => {
            // Bounded here as well as at start: a rate change in flight reaches the same anchor
            // arithmetic, so an unchecked one walks the clock out of range (or backwards) just as
            // effectively (R2-K2, R2-K3). Rejecting ONE command never ends the session - the
            // session keeps running at the rate it had, and the client is told why (rule 6).
            if chrono_core::multiplier_in_range(multiplier) {
                session.set_multiplier(multiplier);
                // The host first (it is the source of truth), then its new clock to the pages.
                bridge.set_multiplier(host_origin(session, bridge.scale_duration()));
                emit(&Event::Ack { v: PROTOCOL_VERSION, id });
                emit(&state_event(session));
            } else {
                emit(&Event::Error {
                    v: PROTOCOL_VERSION,
                    id: Some(id),
                    code: 1,
                    key: "time.bad_multiplier".into(),
                    origin: "core".into(),
                });
            }
        }
        Command::Jump { id, to, .. } => {
            // Relative jump (current fake + one step) resolves in the core through the SHARED
            // evaluator, so `jump` accepts the same calendar units as calc and `--at`. The core
            // alone knows the live fake clock. Absolute resolves through moment_from_spec. Both
            // re-anchor under one clock read.
            let resolved: Result<(), &str> = if to.kind == "relative" {
                match to.delta.as_deref() {
                    Some(d) => match parse_shift(d) {
                        Ok(step) => session.jump_step(&step).map_err(jump_error_key),
                        Err(_) => Err("moment.invalid"),
                    },
                    None => Err("moment.invalid"),
                }
            } else {
                moment_from_spec(&to).map(|ft| session.jump(ft))
            };
            match resolved {
                Ok(()) => {
                    bridge.jump(host_origin(session, bridge.scale_duration()));
                    emit(&Event::Ack { v: PROTOCOL_VERSION, id });
                    emit(&state_event(session));
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
        // Same as the CDP loop: a command this state does not act on is answered, not dropped.
        other => emit(&unsupported_command(command_id(&other))),
    }
}

/// How many times this whole family read a clock the session actually substituted.
///
/// `None` when the question cannot be asked at all: no process reported coverage, or not one
/// channel was substituted. Those are separate facts carrying their own verdicts - a session whose
/// hooks never installed is `uncovered`, not a session the application ignored - and folding them
/// in here would answer a question nobody asked.
///
/// Only `covered` counts. The observed buckets (object waits, the multimedia timer, a direct spawn,
/// a network connect) are hooked and deliberately left REAL, so a target that touched only those
/// read no substituted clock at all - counting them would let a plain `Sleep` silence the one line
/// saying the fake date was never looked at.
///
/// Summed across the family rather than per process, because a parent that reads nothing while its
/// child reads the fake date is a session that WORKED. Per-process counts are already on each
/// `coverage` event for whoever wants them.
///
/// 🔴 The pages inside the application count too, and leaving them out was a real bug. A hybrid
/// session substitutes the clock twice over - natively in the host, and in the host's embedded web
/// engine through its debugging port - and those two halves are tallied in different places
/// (`read_all_coverage` against `pages.counts`). An application whose time logic lives in its web UI
/// can have a host that never reads a substituted export while its pages read `Date.now` all
/// session: native-only, that is `Some(0)`, and the report would have carried
/// `embedded.web_engine_reached` and "no process ever read a clock the session substitutes" side by
/// side. `page_reads` arrives already filtered to counts above zero (CDP coverage means CALLED, not
/// merely hooked), so an empty slice really does mean the pages read nothing.
///
/// The native counters are read with a volatile 64-bit load, which on x86 is two 32-bit loads and
/// can tear while the target runs. That cannot produce a false zero here: a torn read of a non-zero
/// count still carries its low word, and the low word of anything from one upward is not zero.
pub(crate) fn substituted_reads(
    coverage: &[(u32, Coverage)],
    page_reads: &[(u32, String, u64)],
) -> Option<u64> {
    let mut any_channel = !page_reads.is_empty();
    let mut total: u64 = page_reads.iter().fold(0u64, |sum, (_, _, calls)| sum.saturating_add(*calls));
    for (_, cov) in coverage {
        for channel in &cov.covered {
            any_channel = true;
            total = total.saturating_add(channel.calls);
        }
    }
    any_channel.then_some(total)
}

/// The warnings a finished native session carries beside its verdict, each said only when it
/// happened.
///
/// A full PID registry means processes of this family ran with nowhere to report coverage into, so
/// the process count and the channel lists are both short. No per-process event can carry that,
/// because those processes have no pid here (R2-S9), so it rides the session verdict instead - the
/// one event that speaks for the whole family.
///
/// The clock reaching the end of the representable range and standing there means the session did
/// less than it promised: the wall stopped while fake time kept being counted, and a still picture
/// is not an explanation (R2-X2, rule 6).
pub(crate) fn native_session_warnings(
    uncovered_processes: u32,
    clamped: bool,
    duration_clamped: bool,
    substituted_reads: Option<u64>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    // First, because it is the one line that changes what the verdict above it MEANS to a reader.
    //
    // Measured on the shared-page probe: it reads the clock straight out of KUSER_SHARED_DATA and
    // never touches a substituted export, so all 41 channels install, nothing lands in `uncovered`,
    // and the session reports `works` over an application that saw the REAL date from start to
    // finish. The verdict is not wrong about the MECHANISM - the hooks are in and they substitute -
    // but "time substitution took effect" is read as "my application ran on the fake clock", and
    // there it did not. This says so without moving the verdict or the exit code, because what the
    // mechanism achieved and what the application consumed are two different answers and the tester
    // needs both (untouchable rule 4).
    if substituted_reads == Some(0) {
        warnings.push("coverage.session_clock_never_read".to_string());
    }
    if uncovered_processes > 0 {
        warnings.push("coverage.pid_registry_full".to_string());
    }
    if clamped {
        warnings.push("time.fake_clock_clamped".to_string());
    }
    // The monotonic axes standing is a SEPARATE fact from the wall clock standing, and the two do not
    // arrive together: the wall stops at the end of the FILETIME range, the duration axes when their
    // elapsed term fills an i64. A session can meet either without the other, and a reader who is
    // told about one and not the other is told the session did more than it did (rule 6).
    if duration_clamped {
        warnings.push("time.duration_axis_clamped".to_string());
    }
    warnings
}

/// End the session and state what it did: one last fold so a late child still counts, the coverage
/// every process ENDED with, the family verdict and `ended`. Returns the family's exit code.
pub(crate) fn close_session(
    mut session: chrono_mech::Session,
    mut ledger: SessionLedger,
    bridge: EmbeddedBridge,
    target_exit: Option<i32>,
) -> i32 {
    // Final fold so a child that joined since the last heartbeat still counts in the family.
    ledger.poll(&mut session);
    let SessionLedger { mut family, family_pids, uncovered_children, clock_clamped, duration_clamped } =
        ledger;
    // What the pages inside the application did, folded into the family like any process: a page
    // shimmed and reading time is covered, one refused or failed is not, and none at all judges
    // nothing (docs/09 section 12.7).
    let pages = bridge.finish();
    let page_rows = covered_channels(pages.counts.clone());
    family = family.combine(embedded_verdict(pages.reached, pages.seen.len(), !page_rows.is_empty(), pages.failed + pages.overflow));
    let uncovered_children_total = session.uncovered_children_total();
    // A process nobody reached ran on the real clock: that is "something uncovered" for the family,
    // so the family cannot be `works` (untouchable rule 4 at the session level - the verdict model
    // already says so, this is the fact it was never fed). Folded as a verdict rather than a flag so
    // the same `combine` rule that rolls up channels rolls up processes.
    let any_uncovered_children = uncovered_children_total > 0 || !uncovered_children.is_empty();
    if any_uncovered_children {
        family = family.combine(Verdict::Fails);
    }
    // Capture the session clocks before ending so `ended` can state the duration and the fake wall
    // clock reached - reliably, even for a session too short to have emitted a heartbeat.
    let final_state = session.state();
    // Read BEFORE `end` unmaps the control block. A full PID registry means some processes of this
    // family ran with nowhere to report coverage into, so both the count below and the channel lists
    // are short - and no per-process event can carry that, because those processes have no pid here
    // (R2-S9). It rides the session verdict, which is the one event that speaks for the whole family.
    let uncovered_processes = session.uncovered_process_count();

    // The counts every process ENDED with. The first event for a process is emitted inside the ADR-4
    // guard window, so its call counts are the first 300 ms of the session and never move again -
    // measured, a probe that read the clock 25 times was reported as "2 calls" (R2-X8). The audit
    // promises "which channels, and how many times", so the last word on a pid has to be the true
    // one: a later `coverage` for the same pid supersedes the earlier one (docs/08).
    let final_coverage = session.read_all_coverage();
    for (pid, cov) in &final_coverage {
        emit_coverage(*pid, cov, &[]);
    }
    // One `coverage` per page context reached, under `kind: context` - never when there was none,
    // because the bare event a Chromium session emits for an empty audit would claim a context here.
    if !pages.seen.is_empty() {
        for event in coverage_events(&pages.seen, &page_rows, Vec::new()) {
            emit(&event);
        }
    }
    let zone_differs = session.state().tz_bias != chrono_mech::host_tz_bias_min();
    session.end();
    // The sticky flag OR the final sample, so a session too short to have emitted a heartbeat still
    // reports a clamped clock.
    let mut session_warnings = native_session_warnings(
        uncovered_processes,
        clock_clamped || final_state.clock_at_range_end(),
        duration_clamped || final_state.duration_at_range_end(),
        substituted_reads(&final_coverage, &page_rows),
    );
    let mut children_warnings = uncovered_children_warnings(&uncovered_children, uncovered_children_total);
    reconcile_engine_warnings(&mut children_warnings, pages.pages_reached());
    session_warnings.extend(children_warnings);
    session_warnings.extend(pages.session_warnings(zone_differs));
    // The wire names the first UNCOVERED_CHILDREN_WIRE_MAX and carries the true total beside them.
    // The image name is text from the target's world, so it passes the same sieve as everything
    // else the target writes before it reaches a terminal or the panel.
    let named: Vec<chrono_proto::UncoveredChild> = uncovered_children
        .iter()
        .take(UNCOVERED_CHILDREN_WIRE_MAX)
        .map(|c| chrono_proto::UncoveredChild {
            pid: c.pid,
            parent_pid: c.parent_pid,
            image: c.image.as_deref().map(crate::cdp::sanitise_target_text),
            role: c.command_line.as_deref().and_then(role_from_command_line),
        })
        .collect();
    emit(&Event::SessionVerdict {
        v: PROTOCOL_VERSION,
        verdict: family.wire().into(),
        reason_key: session_reason_key(family, any_uncovered_children).into(),
        process_count: 1 + family_pids.len() as u32,
        warning_keys: session_warnings,
        uncovered_children: named,
        uncovered_children_total,
        context_count: pages.seen.len() as u32,
        engines: pages.engines,
    });
    emit(&Event::Ended {
        v: PROTOCOL_VERSION,
        clean: true,
        residue_keys: vec![],
        target_exit_code: target_exit,
        elapsed_real_ms: final_state.elapsed_real_ms,
        elapsed_fake_ms: final_state.elapsed_fake_ms,
        fake_end_wall: Some(filetime_utc_to_wall(final_state.fake_ft, final_state.tz_bias)),
    });
    family.exit_code()
}

/// Poll for children that joined, emit each one's coverage, and fold their verdicts into the
/// running family verdict (tracking distinct pids for the family size). Called at every poll
/// point so the family roll-up sees every process - parent and children alike.
pub(crate) fn fold_children(
    session: &mut chrono_mech::Session,
    family: &mut Verdict,
    pids: &mut HashSet<u32>,
) {
    for (pid, cov) in session.poll_new_coverage() {
        // Children carry no driver-side runtime warning - the parent's coverage already reported the
        // runtime for the family, and a runtime warning is not summed across processes (rule 4).
        emit_coverage(pid, &cov, &[]);
        *family = family.combine(verdict_from_coverage(&cov));
        pids.insert(pid);
    }
}

/// Stable reason key for the family (session) verdict, scoped to the whole family.
///
/// With uncovered children the family is `partial` or `fails` by construction (`close_session`
/// folds a `Fails` in), and the reason has to say WHY in words a tester can act on: not "some
/// channels were queried but not covered" - which is about what a hooked process read - but that a
/// process of this application ran with no hook in it at all.
pub(crate) fn session_reason_key(v: Verdict, uncovered_children: bool) -> &'static str {
    match (v, uncovered_children) {
        (Verdict::Works | Verdict::Partial, true) => "session.family_partial_children",
        (Verdict::Fails | Verdict::Undetermined, true) => "session.family_uncovered_children",
        (Verdict::Works, false) => "session.family_covered",
        (Verdict::Partial, false) => "session.family_partial",
        (Verdict::Fails, false) => "session.family_uncovered",
        (Verdict::Undetermined, false) => "session.family_undetermined",
    }
}

/// The session-level warnings that uncovered children raise, each said only when it happened and
/// each claiming exactly what the evidence supports:
/// - that some were spawned at all (`total` rather than the list's length, because a parent's ring
///   can overflow and the count is the number the audit owes),
/// - that one of them is a Chromium RENDERER, by the role on its command line - the process the
///   application's pages run in, so every page read the real clock (the strong claim),
/// - or, failing that, that an embedded web engine's processes are among them by image name - an
///   engine is in this application and part of it ran real, but what its pages read is not
///   established, because the same image name belongs to the GPU and utility processes and a
///   renderer that exited before it could be asked leaves no role behind (the weak claim).
///
/// The strong claim subsumes the weak one. Both are floors: a child gone before the poll has no name
/// and no role.
pub(crate) fn uncovered_children_warnings(children: &[UncoveredChild], total: u32) -> Vec<String> {
    let mut warnings = Vec::new();
    if total > 0 || !children.is_empty() {
        warnings.push("inheritance.children_uncovered".to_string());
    }
    let renderer = children.iter().any(|c| {
        c.command_line.as_deref().and_then(role_from_command_line).is_some_and(|r| is_renderer_role(&r))
    });
    let engine = children.iter().any(|c| c.image.as_deref().is_some_and(is_web_engine_subprocess));
    if renderer {
        warnings.push("embedded.web_engine_uncovered".to_string());
    } else if engine {
        warnings.push("embedded.web_engine_processes_uncovered".to_string());
    }
    warnings
}

/// Resolve a `MomentSpec` to a UTC FILETIME for a jump. Absolute moments only here
/// (relative delta is a later slice).
pub(crate) fn moment_from_spec(spec: &MomentSpec) -> Result<i64, &'static str> {
    match spec.kind.as_str() {
        "absolute" => {
            let m = Moment {
                local: spec.local.clone().unwrap_or_default(),
                tz_bias_min: spec.tz_bias_min,
            };
            chrono_core::moment_to_filetime_utc(&m).map_err(|_| "moment.invalid")
        }
        _ => Err("moment.unsupported_kind"),
    }
}

/// The hook DLL sits next to the executable (same target dir, matching bitness).
pub(crate) fn hook_dll_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    hook_dll_in(exe.parent()?)
}

/// The hook DLL inside `dir`, or None when it is not there. The existence check is the whole point
/// (R2-W3): without it a broken install - a half-unpacked release, an antivirus quarantine, someone
/// who copied `chrono.exe` alone - walked past this and failed later inside the target, where
/// `LoadLibraryW` returns NULL and the only word we have for that is `target.inject_failed`. That
/// blames the application under test for a file missing from OUR folder, and the diagnosis that
/// names the real cause (`core.hook_dll_missing`, translated in both languages) was unreachable.
///
/// A file deleted between this check and the injection still lands on the old path, and a directory
/// we cannot stat reads as missing - which is the honest answer either way, since a DLL we cannot
/// see is a DLL we cannot inject.
pub(crate) fn hook_dll_in(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let dll = dir.join("chrono_hook.dll");
    dll.is_file().then_some(dll)
}

pub(crate) fn build_spec(time: &TimeSpec) -> Result<SessionSpec, (i32, &'static str)> {
    let mode = match time.mode.as_str() {
        "flow" => TimeMode::Flow,
        "frozen" => TimeMode::Frozen,
        "multiplier" => {
            // The core reads NDJSON from whatever client is on the other end, so it validates for
            // itself rather than trusting the friendly CLI to have done it. It did not, and the two
            // surfaces had drifted: `--mode` required >= 1 while the protocol took any i64, so a
            // negative rate ran the target's clock CONTINUOUSLY BACKWARD and an enormous one walked
            // it out of the representable range - both reported as `works` (R2-K2, R2-K3).
            let m = time.multiplier.unwrap_or(1);
            if !chrono_core::multiplier_in_range(m) {
                return Err((1, "time.bad_multiplier"));
            }
            TimeMode::Multiplier(m)
        }
        _ => return Err((1, "time.bad_mode")),
    };
    Ok(SessionSpec {
        moment: Moment {
            local: time.moment.local.clone().unwrap_or_default(),
            tz_bias_min: time.moment.tz_bias_min,
        },
        mode,
        scale_duration: time.scale_duration,
        scale_qpc: time.scale_qpc,
    })
}

pub(crate) fn map_prepare_error(e: chrono_mech::PrepareError) -> (i32, &'static str, &'static str, String) {
    use chrono_mech::PrepareError as P;
    match e {
        P::Moment(m) => (1, "moment.invalid", "core", m),
        P::Control(m) => (3, "session.control_failed", "mechanism", m),
        P::Launch(m) => (2, "target.launch_failed", "mechanism", m),
        P::Inject(m) => (2, "target.inject_failed", "mechanism", m),
        // A usage error, not a failure of the target (docs/08 section 8, exit 1): nothing is wrong
        // with the application - the wrong build of the tool was pointed at it, and the fix is a
        // different command. Injection cannot cross bitness, so this is a known impossibility
        // declared before the attempt rather than reported as `target.inject_failed` afterwards,
        // where it was indistinguishable from an antivirus block (R2-S1).
        P::BitnessMismatch(target_bits, core_bits) => (
            1,
            "target.bitness_mismatch",
            "mechanism",
            format!(
                "the target runs as {target_bits} and this core is {core_bits} - a hook cannot cross bitness; run the {target_bits} chrono.exe instead"
            ),
        ),
        // pid 0 = the other core holds the session lock but has not published its pid yet (it is
        // still starting). Naming "pid 0" would be a lie, so say what is actually known.
        P::SessionActive(0) => (
            3,
            "session.already_active",
            "mechanism",
            "another session's core is starting or running - one session at a time".to_string(),
        ),
        P::SessionActive(pid) => (
            3,
            "session.already_active",
            "mechanism",
            format!("another session's core (pid {pid}) is running - one session at a time"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::unique_temp_dir;

    #[test]
    fn build_spec_rejects_a_multiplier_the_clock_cannot_survive() {
        // R2-K2 / R2-K3: the core reads NDJSON from any client, so it validates for itself. The CLI
        // used to be the only gate and only checked the floor, so the protocol accepted a negative
        // rate (measured: the target read 31, 29, 28, 26 December while the session said `works`)
        // and an unbounded one (measured at x1e15: one channel wrapped, another silently fell back
        // to the REAL clock, and successive reads jumped between centuries).
        let spec = |m: i64| TimeSpec {
            moment: MomentSpec {
                kind: "absolute".into(),
                local: Some("2038-01-01T00:00:00".into()),
                tz_bias_min: Some(0),
                delta: None,
            },
            mode: "multiplier".into(),
            multiplier: Some(m),
            scale_duration: false,
            scale_qpc: false,
        };

        assert!(build_spec(&spec(60)).is_ok());
        assert!(build_spec(&spec(0)).is_ok(), "0 is the wire spelling of freeze, a value not an error");
        assert!(build_spec(&spec(chrono_core::MULTIPLIER_MAX)).is_ok());

        for bad in [-1, -1_000_000, chrono_core::MULTIPLIER_MAX + 1, i64::MAX] {
            let err = build_spec(&spec(bad)).expect_err("out of range must be refused");
            assert_eq!(err, (1, "time.bad_multiplier"), "multiplier {bad}");
        }
    }

    #[test]
    fn hook_dll_is_only_found_when_it_is_actually_there() {
        // The whole point of the check. Without it this returns Some for an empty folder, `prepare`
        // walks on, LoadLibraryW fails inside the target, and the tester is told `target.inject_failed`
        // - the application under test blamed for a file missing from ours (untouchable rule 6).
        let dir = unique_temp_dir("chrono-hookdll");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(hook_dll_in(&dir), None, "an empty folder must not claim to hold the hook DLL");

        let dll = dir.join("chrono_hook.dll");
        std::fs::write(&dll, b"").unwrap();
        assert_eq!(hook_dll_in(&dir), Some(dll));

        // A DIRECTORY named chrono_hook.dll is not a DLL either - is_file, not exists.
        let other = unique_temp_dir("chrono-hookdll-dir");
        std::fs::create_dir_all(other.join("chrono_hook.dll")).unwrap();
        assert_eq!(hook_dll_in(&other), None);

        // A folder that does not exist at all reads as missing rather than panicking.
        let gone = unique_temp_dir("chrono-hookdll-gone");
        assert_eq!(hook_dll_in(&gone), None);

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&other).ok();
    }

    fn read(stream: &str) -> Result<(TargetSpec, TimeSpec, bool), StartRefusal> {
        read_start(&mut BufReader::new(std::io::Cursor::new(stream.as_bytes().to_vec())))
    }

    /// A `start` built through the real serializer rather than hand-written JSON, so the test cannot
    /// drift from the shape the wire actually carries.
    fn start_line(v: u32, cwd: Option<&str>) -> String {
        serde_json::to_string(&Command::Start {
            v,
            id: 1,
            target: TargetSpec {
                path: "app.exe".into(),
                args: Vec::new(),
                cwd: cwd.map(str::to_string),
                embedded: true,
            },
            time: TimeSpec {
                moment: MomentSpec {
                    kind: "absolute".into(),
                    local: Some("2038-01-19T03:14:07".into()),
                    tz_bias_min: Some(0),
                    delta: None,
                },
                mode: "flow".into(),
                multiplier: None,
                scale_duration: false,
                scale_qpc: false,
            },
            force: false,
        })
        .expect("a start command serializes")
            + "\n"
    }

    fn refusal(stream: &str) -> &'static str {
        read(stream).expect_err("expected a refusal").key
    }

    /// Nothing at all is "no command", which is a refusal rather than a guess at what the client
    /// meant to say.
    #[test]
    fn an_empty_stream_is_no_command() {
        assert_eq!(refusal(""), "protocol.no_command");
    }

    /// A line past the cap reads the same way. The stream has stopped making sense, and the honest
    /// answer is a refusal rather than a guess at what the first megabyte meant.
    #[test]
    fn a_line_past_the_cap_reads_as_no_command() {
        let mut stream = "x".repeat(crate::wire::MAX_PROTOCOL_LINE + 10);
        stream.push('\n');
        assert_eq!(refusal(&stream), "protocol.no_command");
    }

    /// A line that is not a command at all is answered, not ignored.
    #[test]
    fn a_line_that_is_not_json_is_a_bad_command() {
        assert_eq!(refusal("not a command\n"), "protocol.bad_command");
    }

    /// Only `start` opens a session. Anything else arriving first is told so rather than being
    /// silently waited past.
    #[test]
    fn a_command_other_than_start_cannot_open_a_session() {
        let end = serde_json::to_string(&Command::End { v: PROTOCOL_VERSION, id: 1 }).unwrap();
        assert_eq!(refusal(&(end + "\n")), "protocol.expected_start");
    }

    /// R2-K3: the version field is in every message because the receiver is meant to check it. A
    /// client speaking another version is refused BEFORE anything is launched.
    #[test]
    fn a_client_speaking_another_version_is_refused_before_anything_launches() {
        assert_eq!(
            refusal(&start_line(PROTOCOL_VERSION + 1, None)),
            "protocol.version_mismatch"
        );
    }

    /// A working directory that does not exist is the folder's fault, not the target's. Both
    /// mechanisms would otherwise report it as `target.launch_failed`, which is true and useless.
    /// This is also the one refusal that owes an `ended`, because a valid `start` had been accepted.
    #[test]
    fn a_working_directory_that_does_not_exist_is_named_as_such() {
        let refused = read(&start_line(PROTOCOL_VERSION, Some("C:/no-such-folder-chrono-mock-test")))
            .expect_err("expected a refusal");
        assert_eq!(refused.key, "target.cwd_missing");
        assert_eq!(refused.id, Some(1), "it answers the start it refused");
        assert!(refused.needs_ended, "a session that got this far owes an ended");
    }

    /// The happy path carries the target and the time through untouched, and nothing else opens a
    /// session on the way.
    #[test]
    fn a_well_formed_start_carries_its_target_and_time_through() {
        let (target, time, force) =
            read(&start_line(PROTOCOL_VERSION, None)).expect("a well-formed start is accepted");
        assert_eq!(target.path, "app.exe");
        assert_eq!(target.cwd, None);
        assert_eq!(time.mode, "flow");
        assert_eq!(time.moment.local.as_deref(), Some("2038-01-19T03:14:07"));
        assert!(!force);
    }

    /// Each caveat is said only when it happened, and all of them together keep the order the report
    /// prints them in. A session that ran cleanly says nothing extra, which is what makes the ones
    /// that do appear worth reading.
    #[test]
    fn the_native_session_warnings_say_only_what_happened() {
        assert!(native_session_warnings(0, false, false, None).is_empty());
        assert_eq!(native_session_warnings(2, false, false, None), vec!["coverage.pid_registry_full"]);
        assert_eq!(native_session_warnings(0, true, false, None), vec!["time.fake_clock_clamped"]);
        assert_eq!(
            native_session_warnings(1, true, false, None),
            vec!["coverage.pid_registry_full", "time.fake_clock_clamped"]
        );
    }

    /// One process as a fixture spells it: its pid, its substituted channels, its observed ones,
    /// each channel with the call count the session ended on.
    type ProcessFixture<'a> = (u32, &'a [(&'a str, u64)], &'a [(&'a str, u64)]);

    /// A family's coverage, spelled the way `read_all_coverage` hands it over: one entry per pid.
    fn family(processes: &[ProcessFixture]) -> Vec<(u32, Coverage)> {
        processes
            .iter()
            .map(|(pid, covered, observed)| {
                let channels = |cs: &[(&str, u64)]| {
                    cs.iter()
                        .map(|(channel, calls)| chrono_core::ChannelCoverage {
                            channel: (*channel).to_string(),
                            calls: *calls,
                        })
                        .collect()
                };
                (
                    *pid,
                    Coverage {
                        covered: channels(covered),
                        observed: channels(observed),
                        ..Coverage::default()
                    },
                )
            })
            .collect()
    }

    /// Zero reads is a FACT, and no reads to speak of is an ABSENCE of one. The warning rides the
    /// first and has to stay silent on the second, because a session whose hooks never installed is
    /// already `uncovered` and saying "the application never read the clock" over it would name the
    /// wrong cause.
    #[test]
    fn a_family_that_never_read_is_told_apart_from_one_we_know_nothing_about() {
        assert_eq!(substituted_reads(&[], &[]), None, "no process reported at all");
        assert_eq!(
            substituted_reads(&family(&[(10, &[], &[])]), &[]),
            None,
            "a process with not one substituted channel is a failed install, not a quiet target"
        );
        assert_eq!(
            substituted_reads(&family(&[(10, &[("GetSystemTime", 0), ("NtQuerySystemTime", 0)], &[])]), &[]),
            Some(0),
            "channels substituted and never called is the fact this exists for"
        );
    }

    /// Summed across the family, because the question is about the SESSION. A parent that reads
    /// nothing while its child reads the fake date is a session that worked, and warning there would
    /// tell the tester to distrust a result that is sound.
    #[test]
    fn one_child_reading_the_fake_clock_speaks_for_the_whole_family() {
        let quiet_parent_busy_child = family(&[
            (10, &[("GetSystemTime", 0)], &[]),
            (11, &[("GetSystemTime", 4)], &[]),
        ]);
        assert_eq!(substituted_reads(&quiet_parent_busy_child, &[]), Some(4));
        assert!(
            native_session_warnings(0, false, false, substituted_reads(&quiet_parent_busy_child, &[]))
                .is_empty(),
            "the family read the session clock, so there is nothing to caution about"
        );
    }

    /// The observed buckets are hooked and deliberately left REAL (ADR-7), so calls into them are not
    /// reads of a substituted clock. Counting them would let a plain `Sleep` or a `WaitForSingleObject`
    /// silence the one line saying the fake date was never looked at - the target would have blocked
    /// on the real clock and the report would call that engagement.
    #[test]
    fn a_wait_left_real_is_not_a_read_of_the_session_clock() {
        let only_waits = family(&[(
            10,
            &[("GetSystemTimeAsFileTime", 0)],
            &[("WaitForSingleObject", 37), ("Sleep", 12)],
        )]);
        assert_eq!(substituted_reads(&only_waits, &[]), Some(0));
        assert_eq!(
            native_session_warnings(0, false, false, substituted_reads(&only_waits, &[])),
            vec!["coverage.session_clock_never_read"],
            "thirty-seven object waits are not one look at the fake date"
        );
    }

    /// 🔴 The pages inside the application are half the session, and counting only the native half
    /// was a real bug (caught in review on PR #40, before it shipped).
    ///
    /// A hybrid session substitutes the clock twice over - natively in the host, and in the host's
    /// embedded web engine through its debugging port - and the two halves are tallied in different
    /// places. An application whose time logic lives in its web UI can have a host that never touches
    /// a substituted export while its pages read `Date.now` all session. Native-only, that is
    /// `Some(0)`, and the verdict would have carried `embedded.web_engine_reached` and "no process
    /// ever read a clock the session substitutes" on the same screen.
    #[test]
    fn pages_reading_the_fake_clock_count_as_much_as_the_host_does() {
        let silent_host = family(&[(10, &[("GetSystemTimeAsFileTime", 0)], &[])]);
        let pages_read = [(0u32, "page Date.now".to_string(), 9u64)];
        assert_eq!(
            substituted_reads(&silent_host, &pages_read),
            Some(9),
            "the host read nothing and the pages read nine - the session substituted for both"
        );
        assert!(
            native_session_warnings(0, false, false, substituted_reads(&silent_host, &pages_read))
                .is_empty(),
            "warning here would contradict embedded.web_engine_reached on the same verdict"
        );
        // The paired direction, so the line above cannot pass by never firing: the same host with
        // pages that were reached and stayed silent is still a session nobody read the clock in.
        assert_eq!(
            native_session_warnings(0, false, false, substituted_reads(&silent_host, &[])),
            vec!["coverage.session_clock_never_read"]
        );
    }

    /// Pages alone can carry the answer. A session whose native side established no substituted
    /// channel at all is `None` on its own - but if the engine inside it read the fake date, the
    /// question WAS answerable and the answer is not zero.
    #[test]
    fn pages_alone_make_the_question_answerable() {
        assert_eq!(substituted_reads(&[], &[(0, "page Date.now".to_string(), 3)]), Some(3));
        assert_eq!(
            substituted_reads(&[], &[]),
            None,
            "no native channel and no page read is still nothing to go on"
        );
    }

    /// The reversal probe for the warning itself, in both directions and against the ABSENCE case -
    /// a guard that only ever fires, or only ever stays quiet, proves nothing.
    #[test]
    fn the_never_read_caution_fires_on_zero_and_on_nothing_else() {
        assert_eq!(
            native_session_warnings(0, false, false, Some(0)),
            vec!["coverage.session_clock_never_read"]
        );
        assert!(native_session_warnings(0, false, false, Some(1)).is_empty(), "one read is engagement");
        assert!(
            native_session_warnings(0, false, false, None).is_empty(),
            "not knowing is not the same as knowing it was zero"
        );
        assert_eq!(
            native_session_warnings(1, true, false, Some(0)),
            vec![
                "coverage.session_clock_never_read",
                "coverage.pid_registry_full",
                "time.fake_clock_clamped"
            ],
            "it leads, because it changes what the verdict above it means to a reader"
        );
    }

    /// The monotonic axes standing is its OWN caveat, separate from the wall clock standing.
    ///
    /// The two do not arrive together and neither implies the other: the wall stops at the end of the
    /// FILETIME range, the duration axes when their elapsed term fills an i64. Reporting one for the
    /// other would tell the reader the session did something it did not (rule 6).
    #[test]
    fn a_standing_duration_axis_is_its_own_caveat() {
        assert_eq!(
            native_session_warnings(0, false, true, None),
            vec!["time.duration_axis_clamped"],
            "the duration axes can stand while the wall clock is nowhere near its end"
        );
        assert_eq!(
            native_session_warnings(0, true, true, None),
            vec!["time.fake_clock_clamped", "time.duration_axis_clamped"],
            "and both can be true at once"
        );
    }

    fn child(pid: u32, image: Option<&str>, command_line: Option<&str>) -> UncoveredChild {
        UncoveredChild {
            pid,
            parent_pid: 1,
            image: image.map(str::to_string),
            command_line: command_line.map(str::to_string),
        }
    }

    /// The strong claim - every page read the real clock - needs a renderer, and the renderer is
    /// known by its role, not by the image name the GPU and utility processes share with it. An
    /// engine image without a renderer among the named children gets the weak claim, and a plain
    /// helper gets neither. The count alone (a ring that overflowed with nothing named) still says
    /// that children were uncovered.
    #[test]
    fn a_web_engine_warning_claims_only_what_the_role_proves() {
        assert!(uncovered_children_warnings(&[], 0).is_empty());
        assert_eq!(uncovered_children_warnings(&[], 3), vec!["inheritance.children_uncovered"]);
        let helper = child(10, Some("helper.exe"), Some("helper.exe --quiet"));
        assert_eq!(uncovered_children_warnings(&[helper], 1), vec!["inheritance.children_uncovered"]);

        let gpu = child(11, Some("msedgewebview2.exe"), Some("x.exe --type=gpu-process"));
        assert_eq!(
            uncovered_children_warnings(std::slice::from_ref(&gpu), 1),
            vec!["inheritance.children_uncovered", "embedded.web_engine_processes_uncovered"],
            "an engine image without a renderer is the weak claim"
        );
        let nameless = child(12, Some("QtWebEngineProcess.exe"), None);
        assert_eq!(
            uncovered_children_warnings(&[nameless], 1),
            vec!["inheritance.children_uncovered", "embedded.web_engine_processes_uncovered"],
            "a command line that could not be read leaves the weak claim"
        );

        let renderer = child(13, Some("msedgewebview2.exe"), Some("x.exe --type=renderer --lang=en"));
        assert_eq!(
            uncovered_children_warnings(&[gpu, renderer], 2),
            vec!["inheritance.children_uncovered", "embedded.web_engine_uncovered"],
            "a renderer makes the strong claim, and it subsumes the weak one"
        );
        // The role decides, not the image: a CEF subprocess is the application's own executable.
        let cef = child(14, Some("someapp.exe"), Some("someapp.exe --type=renderer"));
        assert_eq!(
            uncovered_children_warnings(&[cef], 1),
            vec!["inheritance.children_uncovered", "embedded.web_engine_uncovered"]
        );
    }

    /// The reason key names processes when children were uncovered, in the verdict the fold makes
    /// unreachable otherwise: a family with such a child is partial or fails, never works.
    #[test]
    fn the_family_reason_names_the_uncovered_children() {
        assert_eq!(session_reason_key(Verdict::Works, false), "session.family_covered");
        assert_eq!(session_reason_key(Verdict::Partial, true), "session.family_partial_children");
        assert_eq!(session_reason_key(Verdict::Fails, true), "session.family_uncovered_children");
        assert_eq!(Verdict::Works.combine(Verdict::Fails), Verdict::Partial);
        assert_eq!(Verdict::Undetermined.combine(Verdict::Fails), Verdict::Fails);
    }
}
