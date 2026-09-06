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
    filetime_utc_to_wall, verdict_from_coverage, Moment, SessionSpec, TimeMode, Verdict,
};
use chrono_proto::{
    parse_command, Command, Event, MomentSpec, TargetSpec, TimeSpec, PROTOCOL_VERSION,
};

use crate::cdp;
use crate::cdp_session::cdp_session;
use crate::cli::{this_bitness, CORE_VERSION};
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

    let m_target = chrono_mech::Target {
        path: &target.path,
        args: &target.args,
        cwd: target.cwd.as_deref(),
    };

    // Detect the target's runtime up front (static, no QPC hook, no process inspection) so the first
    // coverage can warn that its monotonic/elapsed clocks stand on QPC and do not scale - the failure a
    // Python/.NET/Java timer hits under a fast clock (B1, ADR-2). When --scale-qpc is on, this instead
    // cautions about render distortion (A2), since the QPC axis now scales.
    let runtime_warnings = detect_runtime_warnings(std::path::Path::new(&target.path), spec.scale_qpc);

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
            run_session(prepared.session, verdict, reader)
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

/// Drive a running session: emit a ~1 s `state` heartbeat, answer `query`, and stop
/// on `end`, on stdin EOF, or when the target exits. Returns the verdict's exit code.
pub(crate) fn run_session(
    mut session: chrono_mech::Session,
    verdict: Verdict,
    reader: BufReader<std::io::Stdin>,
) -> i32 {
    // A reader thread turns stdin lines into commands so the main thread can beat the
    // heartbeat and watch the target without blocking on read_line.
    let rx = spawn_command_reader(reader);

    // Report any child that already joined during the guard window, before the first
    // heartbeat, so a fast child does not wait a whole second to appear. Seed the family
    // roll-up with the parent verdict, then fold each child's verdict as it joins.
    let mut family = verdict;
    let mut family_pids: HashSet<u32> = HashSet::new();
    // Did the fake clock ever stand on the last instant this build can represent (R2-X2)?
    let mut clock_clamped = false;
    fold_children(&mut session, &mut family, &mut family_pids);

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
            Ok(cmd) => apply_command(&mut session, cmd),
            Err(mpsc::RecvTimeoutError::Timeout) => {} // the tick below handles it
            Err(mpsc::RecvTimeoutError::Disconnected) => break, // stdin closed
        }

        let now = Instant::now();
        if now >= child_deadline {
            fold_children(&mut session, &mut family, &mut family_pids);
            child_deadline = now + child_poll;
        }
        // The heartbeat keeps its own once-a-second cadence: `state` and the liveness check stay
        // exactly as often as the protocol says, whatever else the loop is doing.
        if now >= deadline {
            let st = session.state();
            // Sticky: that the clock STOOD on the edge stays true for this session even if a later
            // jump moves it back, because the readings taken while it stood there were clamped
            // (R2-X2).
            clock_clamped |= st.clock_at_range_end();
            emit(&state_event_from(&st));
            if !session.is_alive() {
                target_exit = session.exit_code();
                break;
            }

            deadline = now + heartbeat;
        }
    }

    close_session(session, family, family_pids, clock_clamped, target_exit)
}

/// Act on one command that arrived mid-session.
///
/// `end` never reaches here. Whether the loop keeps running is the loop's business rather than a
/// command's effect on the session, so that one arm stays where the decision is made. Everything
/// else is answered: a command this state cannot act on gets `unsupported` WITH its id, so a client
/// waiting on `ack` learns the outcome instead of waiting for ever.
pub(crate) fn apply_command(session: &mut chrono_mech::Session, cmd: Command) {
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
pub(crate) fn native_session_warnings(uncovered_processes: u32, clamped: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    if uncovered_processes > 0 {
        warnings.push("coverage.pid_registry_full".to_string());
    }
    if clamped {
        warnings.push("time.fake_clock_clamped".to_string());
    }
    warnings
}

/// End the session and state what it did: one last fold so a late child still counts, the coverage
/// every process ENDED with, the family verdict and `ended`. Returns the family's exit code.
pub(crate) fn close_session(
    mut session: chrono_mech::Session,
    mut family: Verdict,
    mut family_pids: HashSet<u32>,
    clock_clamped: bool,
    target_exit: Option<i32>,
) -> i32 {
    // Final fold so a child that joined since the last heartbeat still counts in the family.
    fold_children(&mut session, &mut family, &mut family_pids);
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
    session.end();
    // The sticky flag OR the final sample, so a session too short to have emitted a heartbeat still
    // reports a clamped clock.
    let session_warnings = native_session_warnings(
        uncovered_processes,
        clock_clamped || final_state.clock_at_range_end(),
    );
    emit(&Event::SessionVerdict {
        v: PROTOCOL_VERSION,
        verdict: family.wire().into(),
        reason_key: session_reason_key(family).into(),
        process_count: 1 + family_pids.len() as u32,
        warning_keys: session_warnings,
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
pub(crate) fn session_reason_key(v: Verdict) -> &'static str {
    match v {
        Verdict::Works => "session.family_covered",
        Verdict::Partial => "session.family_partial",
        Verdict::Fails => "session.family_uncovered",
        Verdict::Undetermined => "session.family_undetermined",
    }
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

    /// Each caveat is said only when it happened, and both together keep the order the report prints
    /// them in. A session that ran cleanly says nothing extra, which is what makes the two that do
    /// appear worth reading.
    #[test]
    fn the_native_session_warnings_say_only_what_happened() {
        assert!(native_session_warnings(0, false).is_empty());
        assert_eq!(native_session_warnings(2, false), vec!["coverage.pid_registry_full"]);
        assert_eq!(native_session_warnings(0, true), vec!["time.fake_clock_clamped"]);
        assert_eq!(
            native_session_warnings(1, true),
            vec!["coverage.pid_registry_full", "time.fake_clock_clamped"]
        );
    }
}
