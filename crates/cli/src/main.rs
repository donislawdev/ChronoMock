//! Chrono Mock command-line interface - a first-class interface from v0.1
//! (chrono-mock.md 11.1 item 15), not an add-on to the GUI.
//!
//! One binary, two roles:
//!   * `chrono run <target> ...` - the friendly driver. Spawns the core process
//!     and speaks the machine protocol (ADR-6) to it over stdio.
//!   * `chrono __core` - the hidden core mode. Reads one `start`, drives the
//!     mechanism layer, emits protocol events on stdout.
//!
//! Stage 1 (walking skeleton): the mechanism only LAUNCHES the target (no
//! injection), so the verdict is the honest `undetermined` with reason key
//! `mechanism.not_implemented`. The full input->output path exists and never
//! fakes success.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono_core::calc::EvalError;
use chrono_core::{filetime_utc_to_wall, verdict_from_coverage, Moment, SessionSpec, TimeMode, Verdict};
use chrono_proto::{
    parse_command, Clock, Command, CoveredChannel, Event, MomentSpec, TimeSpec, PROTOCOL_VERSION,
};

/// `chrono calc` - the date calculator.
mod calc;
/// Reading a shipped calendar catalogue and validating it.
mod calendar;
/// The Chromium/Electron substitution mechanism (CDP faketime), a parallel path to the native core.
mod cdp;
/// Hidden diagnostic probes for the Chromium path.
mod cdp_probe;
/// The Chromium/Electron session - the second substitution mechanism.
mod cdp_session;
/// The command surface: version, bitness, usage texts.
mod cli;
/// The step grammar shared by the calculator flags and the preset reader.
mod grammar;
/// Presets: a named moment with parameters (docs/04 section 4).
mod preset;
/// The terminal report and the evidence export for a finished session.
mod report;
/// `chrono run` - the friendly driver.
mod run;
/// Test-only helpers shared by more than one module.
#[cfg(test)]
mod testutil;
/// One NDJSON line off the machine protocol, bounded.
mod wire;
/// Session zone and instant conversions (untouchable rule 2).
mod zone;

use calc::calc_run;
use grammar::parse_shift;
use cdp_probe::{cdp_date_probe, cdp_launch_probe, cdp_probe, cdp_shim_probe};
use cdp_session::cdp_session;
use cli::{print_usage, this_bitness, CORE_VERSION};
use report::detect_runtime_warnings;
use run::driver_run;
use wire::read_protocol_line;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(String::as_str) {
        Some("__core") => core_mode(),
        Some("run") => driver_run(&args[2..]),
        Some("calc") => calc_run(&args[2..]),
        Some("__cdp-probe") => cdp_probe(&args[2..]),
        Some("__cdp-launch") => cdp_launch_probe(&args[2..]),
        Some("__cdp-shim") => cdp_shim_probe(&args[2..]),
        Some("__cdp-date") => cdp_date_probe(&args[2..]),
        Some("--help") | Some("-h") | None => {
            print_usage();
            0
        }
        Some(other) => {
            eprintln!("chrono: unknown command '{other}'");
            print_usage();
            1
        }
    };
    std::process::exit(code);
}



















// ---------------------------------------------------------------------------
// Driver: `chrono run ...`
// ---------------------------------------------------------------------------













/// Translation key for a relative-jump eval error (docs/08 section 10). Business days need a
/// calendar (not built yet); anything else is an invalid moment. Honest, never silent (rule 6).
fn jump_error_key(e: EvalError) -> &'static str {
    match e {
        EvalError::NeedsCalendar { .. } => "moment.needs_calendar",
        _ => "moment.invalid",
    }
}






















// ---------------------------------------------------------------------------
// Date calculator: `chrono calc ...`
// ---------------------------------------------------------------------------
//
// The calculator surface of the shared step grammar (docs/04 section 4.3). The
// driver parses typed flags into a canonical `MomentExpr`, resolves "now" for a
// today/now base (the core stays pure and takes now as data), evaluates, and
// renders. No natural language (6.2): each flag is one step, order is step order.



// --- Machine JSON output for the calculator (chronomock.calc/1) -----------------------
//
// The calc engine lives in chrono-core (serde-free), so the CLI owns serialization: these DTOs are
// populated from the core's typed results, the mirror of the calendar/preset loaders. The GUI is a
// thin client of this contract (ADR-6), consuming the same engine output the human render shows.
// Contract keys are public names (rule 17); a breaking change needs a schema bump (private repo now).
// An error still goes to stderr with a non-zero exit - stdout carries a result only on success.































// ---------------------------------------------------------------------------
// Calendar loading (the data catalogue, docs/04 section 5)
// ---------------------------------------------------------------------------
//
// The consumer owns the I/O and serde; the core engine works over already-parsed rules.
// The JSON schema is the contract (docs/04 section 5); this is one reader of it. Unknown
// fields are ignored (additive evolution is safe); an unknown major schema version is refused.
















// ---------------------------------------------------------------------------
// Preset loading (the shared catalogue, docs/04 section 4)
// ---------------------------------------------------------------------------
//
// A preset is a NAMED moment expression (docs/04 4.3): the same canonical step model the
// calculator evaluates, plus its human framing (`name`, `explains`). This reader maps the JSON
// contract `chronomock.preset/1` to the core `MomentExpr`; as with the calendar reader the
// consumer owns the I/O and serde, the core engine stays pure. Steps map through the SAME parsers
// the CLI flags use (`parse_snap`/`parse_set_time`/...), so there is one grammar, not two.
//
// SECURITY (docs/04 4.1): a preset describes TIME, never a TARGET. The schema has no path field,
// so a shared preset cannot smuggle an executable path - enforced structurally (there is no field
// to put it in), which `preset_ignores_a_path_field` pins. Unknown fields are ignored (additive
// evolution, docs/04 section 3); an unknown major schema version is refused (section 3.1).
//
// This slice builds the calculator surface (`chrono calc --preset`), so it computes a moment and
// never starts a session; the substitution side (preset -> `run`) arrives with the proto step wire
// (docs/08 section 11 item 1), and the full session-level path guard lands with it.






































// ---------------------------------------------------------------------------
// Core mode: `chrono __core`
// ---------------------------------------------------------------------------

/// Emit one event line and flush immediately - a piped stdout is block-buffered,
/// so without the flush the driver would hang waiting for `ready`.
fn emit(ev: &Event) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{}", ev.to_ndjson());
    let _ = lock.flush();
}

/// Emit a `coverage` event for one process (parent or a child), tagged with its pid.
/// Coverage is reliable (never coalesced) - one event per process, never summed. `extra_warnings` are
/// driver-side warning keys (e.g. the detected runtime, B1) appended to the core's own, de-duplicated.
fn emit_coverage(pid: u32, cov: &chrono_core::Coverage, extra_warnings: &[String]) {
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









fn core_mode() -> i32 {
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
    let mut line = String::new();
    // A line past the cap reads as "no command": the stream has stopped making sense, and the honest
    // answer is the refusal below rather than a guess at what the first megabyte meant.
    let n = read_protocol_line(&mut reader, &mut line).unwrap_or(0);
    if n == 0 {
        emit(&Event::Error {
            v: PROTOCOL_VERSION,
            id: None,
            code: 1,
            key: "protocol.no_command".into(),
            origin: "core".into(),
        });
        return 1;
    }

    let cmd = match parse_command(line.trim_end()) {
        Ok(c) => c,
        Err(_) => {
            emit(&Event::Error {
                v: PROTOCOL_VERSION,
                id: None,
                code: 1,
                key: "protocol.bad_command".into(),
                origin: "core".into(),
            });
            return 1;
        }
    };

    let (target, time, force) = match cmd {
        Command::Start { v, target, time, force, .. } => {
            // The version field is in every message BECAUSE the receiver is meant to check it. Nobody
            // did on this side, so it was a field the contract promised and the code ignored (R2-K3) -
            // a client speaking a version this core does not would have been served silently, with
            // whatever its fields happened to mean here. Checked once, at the only command that opens
            // a session: refuse before anything is launched, and say so.
            if v != PROTOCOL_VERSION {
                emit(&Event::Error {
                    v: PROTOCOL_VERSION,
                    id: None,
                    code: 1,
                    key: "protocol.version_mismatch".into(),
                    origin: "core".into(),
                });
                return 1;
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
                emit(&Event::Error {
                    v: PROTOCOL_VERSION,
                    id: Some(1),
                    code: 1,
                    key: "target.cwd_missing".into(),
                    origin: "core".into(),
                });
                emit(&ended_clean());
                return 1;
            }
            (target, time, force)
        }
        _ => {
            emit(&Event::Error {
                v: PROTOCOL_VERSION,
                id: None,
                code: 1,
                key: "protocol.expected_start".into(),
                origin: "core".into(),
            });
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
fn context_index_for(
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

/// The id carried by any command, so a refusal can name the command it refuses.
fn command_id(cmd: &Command) -> u64 {
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
fn unsupported_command(id: u64) -> Event {
    Event::Error {
        v: PROTOCOL_VERSION,
        id: Some(id),
        code: 1,
        key: "protocol.unsupported_command".into(),
        origin: "core".into(),
    }
}

fn ended_clean() -> Event {
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
fn ended_after_launch(residue: Vec<String>) -> Event {
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

/// Drive a running session: emit a ~1 s `state` heartbeat, answer `query`, and stop
/// on `end`, on stdin EOF, or when the target exits. Returns the verdict's exit code.
fn run_session(
    mut session: chrono_mech::Session,
    verdict: Verdict,
    reader: BufReader<std::io::Stdin>,
) -> i32 {
    // A reader thread turns stdin lines into commands so the main thread can beat the
    // heartbeat and watch the target without blocking on read_line.
    let (tx, rx) = mpsc::channel::<Command>();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            // `read_protocol_line` clears the buffer itself, and a line past the cap comes back as
            // an error - which lands on the same branch as EOF, ending the command stream rather
            // than resyncing onto the tail of a line nobody can vouch for.
            match read_protocol_line(&mut reader, &mut line) {
                Ok(0) | Err(_) => break, // EOF or error: dropping tx signals Disconnected
                Ok(_) => match parse_command(line.trim_end()) {
                    Ok(cmd) => {
                        if tx.send(cmd).is_err() {
                            break;
                        }
                    }
                    // Say so, and keep going. An unreadable line used to vanish without a word:
                    // the FIRST command (`start`) answers bad input properly, every one after it
                    // did not, and the protocol has an `ack` - so a client that waits for one waits
                    // for ever. `--json` is advertised for CI, which is exactly where such a client
                    // gets written. No id, because the id lives in the line we could not read.
                    Err(_) => emit(&Event::Error {
                        v: PROTOCOL_VERSION,
                        id: None,
                        code: 1,
                        key: "protocol.bad_command_ignored".into(),
                        origin: "core".into(),
                    }),
                },
            }
        }
    });

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
            Ok(Command::Query { id, .. }) => {
                emit(&state_event(&session));
                emit(&Event::Ack { v: PROTOCOL_VERSION, id });
            }
            Ok(Command::SetMultiplier { id, multiplier, .. }) => {
                // Bounded here as well as at start: a rate change in flight reaches the same anchor
                // arithmetic, so an unchecked one walks the clock out of range (or backwards) just as
                // effectively (R2-K2, R2-K3). Rejecting ONE command never ends the session - the
                // session keeps running at the rate it had, and the client is told why (rule 6).
                if chrono_core::multiplier_in_range(multiplier) {
                    session.set_multiplier(multiplier);
                    emit(&Event::Ack { v: PROTOCOL_VERSION, id });
                    emit(&state_event(&session));
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
            Ok(Command::Jump { id, to, .. }) => {
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
                        emit(&state_event(&session));
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
            Ok(other) => emit(&unsupported_command(command_id(&other))),
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
    let mut session_warnings = Vec::new();
    if uncovered_processes > 0 {
        session_warnings.push("coverage.pid_registry_full".to_string());
    }

    // The clock reached the end of the representable range and stood there. The session then did less
    // than it promised - the wall stopped while fake time kept being counted - and a still picture is
    // not an explanation (R2-X2, rule 6). Checked on the final sample too, so a session shorter than
    // one heartbeat still reports it.
    if clock_clamped || final_state.clock_at_range_end() {
        session_warnings.push("time.fake_clock_clamped".to_string());
    }
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
fn fold_children(
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
fn session_reason_key(v: Verdict) -> &'static str {
    match v {
        Verdict::Works => "session.family_covered",
        Verdict::Partial => "session.family_partial",
        Verdict::Fails => "session.family_uncovered",
        Verdict::Undetermined => "session.family_undetermined",
    }
}

/// Resolve a `MomentSpec` to a UTC FILETIME for a jump. Absolute moments only here
/// (relative delta is a later slice).
fn moment_from_spec(spec: &MomentSpec) -> Result<i64, &'static str> {
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

/// Build a `state` event from the session's current clocks.
fn state_event(session: &chrono_mech::Session) -> Event {
    state_event_from(&session.state())
}

/// The wire `state` for a state already sampled - so a caller that needs to LOOK at the sample
/// (the clamp check, R2-X2) reads the same one it reports, not a second sample taken next door.
fn state_event_from(s: &chrono_mech::SessionState) -> Event {
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

/// The hook DLL sits next to the executable (same target dir, matching bitness).
fn hook_dll_path() -> Option<std::path::PathBuf> {
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
fn hook_dll_in(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let dll = dir.join("chrono_hook.dll");
    dll.is_file().then_some(dll)
}

fn build_spec(time: &TimeSpec) -> Result<SessionSpec, (i32, &'static str)> {
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

fn map_prepare_error(e: chrono_mech::PrepareError) -> (i32, &'static str, &'static str, String) {
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










































    // --- calc surface ---------------------------------------------------------






























    // --- Presets (Stage 4 slice 16-18): a named moment expression, docs/04 4.3 ----------------












    // --- run --preset (Stage 4 slice 17): the substitution bridge, docs/06.3 pkt 3 -----------





    // --- Parametric presets (Stage 4 slice 18): --param, docs/04 4.2 --------------------------










    // --- run --param + default_hint (Stage 4 slice 19): trial in substitution ------------------





    // ---- Runtime detection (B1): a Python/.NET/Java target whose monotonic/elapsed clock is on QPC ----







    // ---- Hook DLL presence (R2-W3): a broken install must not be reported as the target's fault ----


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
}
