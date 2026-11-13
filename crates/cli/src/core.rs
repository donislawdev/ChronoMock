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
    filetime_utc_to_wall, verdict_from_coverage, Coverage, Moment, SessionSpec, Verdict,
};
use chrono_mech::{FamilyMember, RunningCopy, UncoveredChild};
use chrono_proto::{
    parse_command, Command, Event, FollowedProcess, ReachedEngine, TargetConsole, TargetSpec, TimeSpec,
    PROTOCOL_VERSION, UNCOVERED_CHILDREN_WIRE_MAX,
};

use crate::cdp;
use crate::cdp_attach::{ShimOrigin, END_WAIT};
use crate::cdp_audit::{coverage_events, covered_channels, embedded_verdict};
use crate::cdp_clock::shim_origin_from_state;
use crate::cdp_session::cdp_session;
use crate::cli::{this_bitness, CORE_VERSION};
use crate::embedded::{is_renderer_role, is_web_engine_subprocess, role_from_command_line};
use crate::embedded_bridge::{
    reconcile_engine_warnings, EmbeddedBridge, Launch, KEY_REGISTRY_ARGUMENTS_HIDDEN, KEY_WEB_ENGINE_UNCOVERED,
};
use crate::events::{
    emit, emit_coverage, ended_clean, error_event, jump_error_key, moment_error_key, reported_sleep_ms,
    sleep_to_announce, start_time_mode, state_event, state_event_from, KEY_HOST_SLEPT, SLEPT_REPORTED_MS,
};
use crate::output::diag;
use crate::pe::is_windowed_program;
use crate::policy_session::{PolicySession, KEY_LEFT};
use crate::report::detect_runtime_warnings;
use crate::session_commands::{apply_command, JumpTo, SessionClock};
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
    let Start { id: start_id, target, time, force } = match read_start(&mut reader) {
        Ok(start) => start,
        Err(refusal) => {
            emit(&error_event(refusal.id, 1, refusal.key, "core"));
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
        return cdp_session(start_id, target, time, reader);
    }

    // Prepare and start the session (Stage 2: real injection of one wall channel).
    let hook = match hook_dll_path() {
        Some(p) => p,
        None => {
            emit(&error_event(Some(start_id), 3, "core.hook_dll_missing", "core"));
            // Human-side detail on stderr (never on the protocol stdout), naming the folder we looked
            // in. For a portable tool unpacked anywhere, WHERE we looked is the actionable half.
            let looked_in = crate::own_path::own_exe()
                .ok()
                .and_then(|e| e.parent().map(|d| d.display().to_string()))
                .unwrap_or_else(|| "the folder holding chrono.exe".into());
            diag!("chrono core: chrono_hook.dll not found in {looked_in} - this installation is incomplete");
            emit(&ended_clean());
            return 3;
        }
    };

    let spec = match build_spec(&time) {
        Ok(s) => s,
        Err((code, key)) => {
            emit(&error_event(Some(start_id), code, key, "core"));
            emit(&ended_clean());
            return code;
        }
    };

    // The embedded-engine channel (docs/09): the two variables that make an engine inside the
    // application open a local debugging port, and the discovery that uses it - never one without
    // the other. Off under `--no-embedded`, and then the environment is inherited untouched.
    // The core's own token decides two things: whether the application it starts is elevated too (it
    // inherits the token), and whether the tester's request to reach its pages through the machine
    // registry can be met (docs/09 section 12.19). The value is written HERE, before the launch, because
    // an engine reads it the moment it is created - and `policy` takes it away again on every way out.
    let elevated = chrono_mech::process_elevated(std::process::id()) == Some(true);
    let mut policy = PolicySession::start(&target, elevated);
    let mut launch = Launch::for_target(&target, elevated);
    launch.note_policy(&policy);

    // Detect the target's runtime up front (static, no QPC hook, no process inspection) so the first
    // coverage can warn that its monotonic/elapsed clocks stand on QPC and do not scale - the failure a
    // Python/.NET/Java timer hits under a fast clock (B1, ADR-2). When --scale-qpc is on, this instead
    // cautions about render distortion (A2), since the QPC axis now scales.
    let mut runtime_warnings = detect_runtime_warnings(std::path::Path::new(&target.path), spec.scale_qpc);
    // The Gecko sandbox (ADR-28), said from the same function the dry run asks.
    let gecko = crate::gecko::warning(std::path::Path::new(&target.path), target.gecko_sandbox, crate::gecko::pages_unsandboxed_by_tester());
    runtime_warnings.extend(gecko.map(str::to_string));
    let env = target_environment(&launch, &target.path, spec.moment.tz_bias_min, target.gecko_sandbox, &mut runtime_warnings);
    let m_target = chrono_mech::Target {
        path: &target.path,
        args: &target.args,
        cwd: target.cwd.as_deref(),
        env: &env,
        stdio: chrono_mech::TargetStdio::choose(
            target.console == TargetConsole::New,
            is_windowed_program(std::path::Path::new(&target.path)),
        ),
    };
    if launch.registry_hidden {
        // Known before the launch, like the runtime: a registry policy the tester set for this
        // application is not read while the session's variable is in its environment.
        runtime_warnings.push(KEY_REGISTRY_ARGUMENTS_HIDDEN.to_string());
    }
    // What the registry step already knows, and the tester is owed before the application runs.
    runtime_warnings.extend(policy.start_keys().iter().cloned());

    match chrono_mech::prepare(&spec, &m_target, &hook) {
        Ok(mut prepared) => {
            // Surface a takeover of a previous session's control block so it is not silent, saying how
            // that session had ended (R4-D19). Human diagnostic on stderr, never on the protocol stdout.
            if let Some(notice) = reclaim_notice(prepared.reclaimed) {
                diag!("{notice}");
            }
            // The job the target was started in, when something about it went wrong (R4-S1).
            if let Some(note) = &prepared.start_job_note {
                diag!("chrono core: {note}");
            }
            let verdict = verdict_from_coverage(&prepared.coverage);
            // The parent's own coverage (its pid). Children that join later report
            // separately from run_session, each with its own pid and counts.
            emit_coverage(prepared.session.pid, &prepared.coverage, &with_zone_warning(runtime_warnings, prepared.session.zone_unnamed_for_icu()));
            let mut ledger = SessionLedger::new(verdict);

            // Single-instance vanish (ADR-4): the target exited within the guard
            // window right after injection. Report it honestly with exit 12 rather
            // than trusting the install bits into a false verdict. A target that exited leaving a
            // process on the session clock running is a launcher, not a vanish: the session goes on
            // for that process (ADR-16). A family of console programs that ended there did its work
            // quickly, and its session ends the way any other does (2026-10-27).
            if let Some(lived_ms) = prepared.vanished_lived_ms
                && !prepared.session.family_alive()
                && let Some(reason_key) = vanish_reason_key(&mut prepared.session, &mut ledger)
            {
                emit(&Event::Vanished {
                    v: PROTOCOL_VERSION,
                    pid: prepared.session.pid,
                    reason_key: reason_key.into(),
                    lived_ms,
                });
                // The target ended on its own, so `ended` carries its code, as it does for a session that
                // ran: a crash said without the code it ended with cannot be looked up.
                let target_exit = prepared.session.exit_code();
                prepared.session.end();
                emit(&with_target_exit(policy.ended_before_running(), target_exit));
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
            // The family is ended BEFORE the verdict goes out, so the verdict can name what is still
            // running, and say when it could not look for all of it (R4-S5).
            let (left_running, family_search_incomplete) =
                if refuse { end_refused_family(&prepared.session) } else { (Vec::new(), false) };
            if refuse {
                // The session is over, so the value goes now. How that went is kept, and `ended` below says
                // a value that could not be taken away: it is still on the machine.
                let _ = policy.finish();
            }
            emit(&Event::Verdict {
                v: PROTOCOL_VERSION,
                id: Some(start_id),
                verdict: verdict.wire().into(),
                refuse_start: refuse,
                reason_key: reason_key.into(),
                left_running,
                family_search_incomplete,
            });
            if refuse {
                prepared.session.end();
                emit(&policy.ended_before_running());
                return verdict.exit_code();
            }
            // Enter the running session: heartbeat, answer queries, end on command,
            // EOF, or target exit.
            run_session(prepared.session, ledger, reader, &launch, spec.scale_duration, policy)
        }
        Err(e) => {
            let (code, key, origin, detail) = map_prepare_error(e);
            emit(&error_event(Some(start_id), code, key, origin));
            // Human-side detail on stderr (never on the protocol stdout).
            diag!("chrono core: {detail}");
            emit(&policy.ended_before_running());
            code
        }
    }
}
/// The exit code of a start refused because another session is running (`session.already_active`,
/// docs/08 section 8, R5-D15). The driver passes it through like every other code in its table.
pub(crate) const SESSION_BUSY_EXIT: i32 = 7;

/// The keys of the refusals that owe no `ended`, because no session was begun - the only answers a core
/// gives without an `ended` after them. The driver reads a missing `ended` after any other error as a
/// session the core left open (`run::collect`), so this list is what tells the two apart, and
/// `the_refusals_that_owe_no_ended_are_exactly_the_listed_ones` holds it to `read_start` both ways.
pub(crate) const REFUSALS_WITHOUT_ENDED: [&str; 4] =
    ["protocol.no_command", "protocol.bad_command", "protocol.version_mismatch", "protocol.expected_start"];

/// The opening `start` as the session takes it. Its id is what every answer to it carries - the verdict
/// and an error before the session runs - where the core used to answer `1` whatever the client sent
/// (R5-P18). Both built-in clients send 1, so neither saw it.
#[derive(Debug)]
pub(crate) struct Start {
    id: u64,
    target: TargetSpec,
    time: TimeSpec,
    force: bool,
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
pub(crate) fn read_start<R: BufRead>(reader: &mut R) -> Result<Start, StartRefusal> {
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
        Command::Start { v, id, target, time, force } => {
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
                    id: Some(id),
                    needs_ended: true,
                });
            }
            Ok(Start { id, target, time, force })
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
    /// The processes the session went on for once the target had closed (ADR-16), noted the first
    /// time the target is seen gone. `None` while it runs, empty when nothing outlived it.
    followed: Option<Vec<FamilyMember>>,
    /// Processes both the hook's ring and the process tree under an elevated host reported. Each is in
    /// the list once and in both counts, so the total has to give one back.
    duplicated: u32,
    /// The pids the process tree named and still shows, so a ring entry for the same process is
    /// recognised. A process that has left the tree is forgotten: the system hands its pid on.
    unhooked_pids: HashSet<u32>,
    /// How much of the machine's sleep during the session the log has already been told about (R5-P1).
    slept_announced_ms: i64,
    /// Whether an absolute deadline had been converted when the session jumped or changed speed (R5-D2,
    /// R5-D35) - the one case in which such a deadline can fire at a moment the application did not mean.
    absolute_due_before_move: bool,
    /// Whether a process of the family has shown a visible window (ADR-4, 2026-11-11). Looked for at every
    /// heartbeat until the first one, then no more: one is enough to say the tester had the application.
    window_seen: bool,
    /// The copies of a windowed program of the family running outside it, looked for when the family
    /// ended on its own without ever showing a window - where its work most likely went.
    copies_outside: Vec<RunningCopy>,
    /// Whether those copies were to be looked for and the process list could not be read, so the report
    /// cannot say whether the work went to a copy - which it says, rather than nothing.
    copy_search_failed: bool,
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
            followed: None,
            duplicated: 0,
            unhooked_pids: HashSet::new(),
            slept_announced_ms: 0,
            absolute_due_before_move: false,
            window_seen: false,
            copies_outside: Vec::new(),
            copy_search_failed: false,
        }
    }

    /// Look for a visible window of the family, until one is seen (ADR-4, 2026-11-11). The children the
    /// hook never entered count too: a window the tester saw is one, whichever process drew it. Whose a
    /// process is, the session decides by identity, not by a pid it noted once (`family_shows_window`).
    pub(crate) fn look_for_window(&mut self, session: &chrono_mech::Session) {
        if !self.window_seen {
            self.window_seen = session.family_shows_window(&self.uncovered_children);
        }
    }

    /// The family ended on its own. When it never showed a window, a copy of one of its windowed programs
    /// running outside it is where its work most likely went (ADR-4, 2026-11-11). A session the tester
    /// stopped is not asked: an application cut off has handed nothing on, and `run_session` calls this
    /// only on the way out that the family took by itself.
    fn note_family_ended(&mut self, session: &chrono_mech::Session) {
        self.family_ended(|unhooked| session.running_copies(unhooked));
    }

    /// `note_family_ended` with the search passed in, so what the ledger does with each answer is tested
    /// without a session: a window seen asks nothing, copies found are kept, and a process list that
    /// could not be read is remembered for the report instead of told to the log alone.
    fn family_ended(&mut self, search: impl FnOnce(&[UncoveredChild]) -> Result<Vec<RunningCopy>, String>) {
        if self.window_seen {
            return;
        }
        match search(&self.uncovered_children) {
            Ok(copies) => self.copies_outside = copies,
            Err(why) => {
                diag!(
                    "chrono core: the process list could not be read, so a copy of the application running outside the session was not looked for: {why}"
                );
                self.copy_search_failed = true;
            }
        }
    }

    /// The `host_slept` event the log owes for this sample, when the machine slept at least
    /// [`SLEPT_REPORTED_MS`] since the last one it was told about (R5-P1, R5-D34). The heartbeat stands on
    /// a clock that counts sleep, so the first beat after a wake-up comes at once and carries it.
    pub(crate) fn host_slept_news(&mut self, st: &chrono_mech::SessionState) -> Option<Event> {
        let slept = sleep_to_announce(self.slept_announced_ms, st.host_slept_ms)?;
        self.slept_announced_ms = st.host_slept_ms;
        Some(Event::HostSlept {
            v: PROTOCOL_VERSION,
            slept_ms: slept as u64,
            total_slept_ms: st.host_slept_ms as u64,
        })
    }

    /// The session jumped or changed speed, with `absolute_dues` deadlines converted in the family so far.
    pub(crate) fn note_clock_moved(&mut self, absolute_dues: u64) {
        self.absolute_due_before_move |= absolute_dues > 0;
    }

    /// Poll for children that joined and for children the hook could not follow into, and keep
    /// the watch on the processes the session lasts for current.
    ///
    /// The child poll reads the registry first thing (it has to know which parents have ended), so the
    /// family it leaves is the one the session went on for, and the launched process is asked before it.
    pub(crate) fn poll(&mut self, session: &mut chrono_mech::Session) {
        fold_children(session, &mut self.family, &mut self.family_pids);
        let launched_ended = ended_before(session, chrono_mech::Session::is_alive, |session| {
            self.take_ring_children(session.poll_uncovered_children());
        });
        if self.followed.is_none() && launched_ended {
            self.followed = Some(session.living_family());
        }
    }

    /// Take in the children the hook's ring named. The process tree under an elevated host may have named
    /// one first. Named twice would be counted twice, so the second naming is dropped and the count gives
    /// the duplicate back. Only a process the TREE named, and still shows, is compared: two children of the
    /// ring may share a pid, because the system hands a pid on once its process is gone, and so may a ring
    /// child and a process the tree named earlier.
    fn take_ring_children(&mut self, children: Vec<UncoveredChild>) {
        for child in children {
            if self.unhooked_pids.contains(&child.pid) {
                self.duplicated += 1;
            } else {
                self.uncovered_children.push(child);
            }
        }
    }

    /// Take in the processes the process tree under an elevated host named: ones the hook never saw
    /// start, listed with the children it saw and could not follow into. `present` is every pid the tree
    /// shows now, so a process that has left it stops being compared against the ring.
    pub(crate) fn adopt_unhooked(&mut self, named: Vec<UncoveredChild>, present: &HashSet<u32>) {
        self.unhooked_pids.retain(|pid| present.contains(pid));
        self.unhooked_pids.extend(named.iter().map(|c| c.pid));
        self.uncovered_children.extend(named);
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

    /// The processes the hook is in: the application itself, and what it started that the hook followed
    /// into. These are the ones that can hold the WebView2 client library - the engine's own processes
    /// (the uncovered children) carry the engine, not the client.
    pub(crate) fn hosts(&self, parent: u32) -> Vec<u32> {
        std::iter::once(parent).chain(self.family_pids.iter().copied()).collect()
    }
}

/// The host's clock now, as the origin a page is shimmed from.
fn host_origin(session: &chrono_mech::Session, scale_duration: bool) -> ShimOrigin {
    shim_origin_from_state(&session.state(), scale_duration)
}

/// Drive a running session: emit a ~1 s `state` heartbeat, answer `query`, and stop
/// on `end`, on stdin EOF, or when the target exits. Returns the verdict's exit code.
///
/// `ledger` starts from the launched process's verdict, holding the children the guard window already
/// named: the hook's ring names each child once.
pub(crate) fn run_session(
    mut session: chrono_mech::Session,
    mut ledger: SessionLedger,
    reader: BufReader<std::io::Stdin>,
    launch: &Launch,
    scale_duration: bool,
    policy: PolicySession,
) -> i32 {
    // A reader thread turns stdin lines into commands so the main thread can beat the
    // heartbeat and watch the target without blocking on read_line.
    let rx = spawn_command_reader(reader);

    // Report any child that already joined during the guard window, before the first
    // heartbeat, so a fast child does not wait a whole second to appear.
    ledger.poll(&mut session);
    // The bridge to the pages inside the application (docs/09 section 12): looks for an engine's
    // debugging port in the family from here on, inert when the channel is off. Its pages go on the
    // session's zone, the one the hook hands the rest of the application.
    let mut bridge = EmbeddedBridge::start(launch, ledger.family(session.pid), scale_duration, session.state().tz_bias);

    let heartbeat = Duration::from_secs(1);
    // Children are polled faster than the heartbeat. A child publishes its evidence in a section
    // that only its own handle keeps alive, so a child shorter-lived than the poll interval takes
    // that evidence with it - and installers, the case ADR-3 exists for, spawn exactly such short
    // helpers. A tenth of a second narrows the window without touching the protocol's once-a-second
    // heartbeat (the section walk is a few memory reads, not I/O).
    let child_poll = Duration::from_millis(100);
    let mut deadline = Instant::now() + heartbeat;
    let mut child_deadline = Instant::now() + child_poll;

    loop {
        // Both deadlines are also checked after handling a command, not only when the wait times out.
        // With commands arriving back to back, `wait` is zero and recv_timeout keeps returning a command
        // rather than a timeout - so `state` stopped being emitted for as long as the client kept
        // talking, and the GUI's idle watchdog would call a perfectly healthy core unresponsive.
        let wait = deadline.min(child_deadline).saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(Command::End { .. }) => break,
            Ok(cmd) => {
                if apply_command(&mut NativeClock { session: &mut session, bridge: &mut bridge }, cmd) {
                    ledger.note_clock_moved(session.absolute_dues_total());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {} // the tick below handles it
            Err(mpsc::RecvTimeoutError::Disconnected) => break, // stdin closed
        }

        let now = Instant::now();
        if now >= child_deadline {
            ledger.poll(&mut session);
            let family = ledger.family(session.pid);
            bridge.family(family.iter().copied());
            bridge.look_for_webview2(&ledger.hosts(session.pid));
            // The engine of an elevated host is started by a service, so the hook never sees it: the
            // process tree is where it shows (docs/09 section 12.19).
            let named = bridge.follow_host_tree(&family);
            ledger.adopt_unhooked(named, bridge.tree_pids());
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
            if let Some(slept) = ledger.host_slept_news(&st) {
                emit(&slept);
            }
            emit(&state_event_from(&st));
            // The session lasts for the family on its clock, not for the process it launched: a
            // launcher ends on purpose and leaves the application running (ADR-16). Asked before the
            // window, because asking refreshes the watch the window question reads its members from.
            let family_alive = session.family_alive();
            ledger.look_for_window(&session);
            if !family_alive {
                ledger.note_family_ended(&session);
                break;
            }
            bridge.request_counts();
            bridge.resync(shim_origin_from_state(&st, scale_duration));

            deadline = now + heartbeat;
        }
    }

    // The exit code is the launched process's, the one the tester named, read whatever ended the session:
    // the family's end, a Stop or a closed stdin. A launched process that ended while a process it started
    // kept the session going ended on its own all the same - a launcher, or an application that crashed
    // and left a helper running - and a crash is what the tester has to hear. `None` while it still runs.
    let target_exit = session.exit_code();
    close_session(session, ledger, bridge, policy, target_exit)
}

/// The native session's clock, as the commands see it (R5-A3): the hooked family's clock, with the
/// pages of an engine inside the application put on it after every move (docs/09 section 12). Every
/// applied move is noted, for the timers that were pending when the clock moved (R5-D2).
struct NativeClock<'a> {
    session: &'a mut chrono_mech::Session,
    bridge: &'a mut EmbeddedBridge,
}

impl SessionClock for NativeClock<'_> {
    fn state_event(&self) -> Event {
        state_event(self.session)
    }

    fn set_multiplier(&mut self, multiplier: i64) -> bool {
        self.session.set_multiplier(multiplier);
        // The host first (it is the source of truth), then its new clock to the pages.
        self.bridge.set_multiplier(host_origin(self.session, self.bridge.scale_duration()));
        true
    }

    /// A step resolves in the mechanism, which re-anchors under one clock read. A moment goes through
    /// the gate a start goes through.
    fn jump(&mut self, to: &JumpTo<'_>) -> Result<bool, &'static str> {
        match to {
            JumpTo::Step(step) => self.session.jump_step(step).map_err(jump_error_key)?,
            JumpTo::At { local, bias } => {
                let at = moment_at(local, *bias, self.session.tz_bias())?;
                self.session.jump(at);
            }
        }
        self.bridge.jump(host_origin(self.session, self.bridge.scale_duration()));
        Ok(true)
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

/// An absolute deadline was converted to a delay before the session jumped or changed speed (R5-D2, R5-D35).
const KEY_ABSOLUTE_DUE: &str = "timer.absolute_due_converted";

/// The warnings about what the session's own clock meant for the application, each said only when it
/// happened: the machine slept while the session clock stood still (R5-P1, R5-D34), an absolute
/// deadline converted on the speed of its moment was pending when the clock moved (R5-D2, R5-D35), and a
/// jump took the clock to before the period of the key name the session zone borrowed (ADR-26).
fn session_clock_warnings(host_slept_ms: i64, absolute_due_before_move: bool, jumped_before_zone_name: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    if host_slept_ms >= SLEPT_REPORTED_MS {
        warnings.push(KEY_HOST_SLEPT.to_string());
    }
    if absolute_due_before_move {
        warnings.push(KEY_ABSOLUTE_DUE.to_string());
    }
    if jumped_before_zone_name {
        warnings.push(KEY_JUMP_BEFORE_ZONE_NAME.to_string());
    }
    warnings
}

/// The application was still running when the session ended: it is back on the real date, and its
/// timers and elapsed-time counters carry on at normal speed from where the session left them.
const KEY_LEFT_RUNNING: &str = "session.left_running";

/// The target closed while processes it had started were still running on the session clock, and
/// the session went on for them (ADR-16). Said because a session that outlives the program the
/// tester named is a surprise unless it is explained, and a helper that never ends keeps it open.
/// It says the session went on for them, never that it lasted until they closed: a Stop or `--ticks`
/// can end it while one still runs, and `session.left_running` then says that beside it.
const KEY_FOLLOWED_FAMILY: &str = "session.followed_family";

/// A process that signed in to the session would not let the core wait on it, so the session could
/// not see it close and may have ended while it still ran (R5-D3). It used to count as ended, silently.
const KEY_MEMBER_UNWATCHABLE: &str = "session.member_unwatchable";

/// The family ended on its own without ever showing a window, while a copy of one of its windowed
/// programs ran outside the session: a single-instance application most likely handed its work to that
/// copy after the guard window, and the copy runs on the real clock (ADR-4, 2026-11-11). "Most likely",
/// because no process says what it handed on - the situation is what is read.
const KEY_HANDED_TO_COPY: &str = "session.handed_to_copy";

/// The reason of a family verdict whose application most likely handed its work to a copy outside the
/// session - before every other reason, because it is why the session may have tested nothing.
const KEY_FAMILY_HANDED_TO_COPY: &str = "session.family_handed_to_copy";

/// The family ended on its own without ever showing a window, and the process list could not be read,
/// so whether its work went to a copy outside the session is not known (ADR-4, 2026-11-11). Said apart
/// from `session.handed_to_copy`: nothing was found, and the verdict stays what the family earned.
const KEY_COPY_SEARCH_FAILED: &str = "session.copy_search_failed";

/// The target vanished inside the guard window after starting a process the hook could not enter,
/// usually one of the other bitness, which runs on the real clock (ADR-16).
const KEY_HANDED_OFF_UNCOVERED: &str = "target.handed_off_uncovered";

/// The target vanished inside the guard window with the exit code of a crash (`report::ended_by_crash`).
/// It used to be reported as a suspected single-instance application, whose remedy - close the copy that
/// is running - does nothing for a program that crashes.
const KEY_TARGET_CRASHED: &str = "target.crashed";

/// The target ended with the exit code of a crash after the guard window. The verdict is about the clock
/// it read until then and stays what the family earned, but under a verdict of works the crash read as a
/// session that went well, and the session may be its cause.
const KEY_SESSION_TARGET_CRASHED: &str = "session.target_crashed";

/// The tester's environment set `TZ`, and the session did not hand that value on to the target, because
/// the runtimes that read it would take their zone from it instead of the session's (`session_zone_change`).
/// Said once, in the first coverage: the CLI driver and the window union warnings over every event.
const KEY_TZ_VARIABLE_REMOVED: &str = "session.tz_variable_removed";

/// The runtimes built on ICU find no zone for this session (`Session::zone_unnamed_for_icu`, ADR-26), so
/// they show it without a name or show UTC. Known when the session starts, and said in the first coverage
/// beside the warnings known before the launch.
const KEY_ZONE_UNNAMED: &str = "session.zone_unnamed";

/// A jump took the clock to before the period of the key name the session zone borrowed (ADR-26), where
/// the runtimes built on ICU can show the offset the real zone of that name had at the date. Said with the
/// verdict, because any jump of the session can be the one.
const KEY_JUMP_BEFORE_ZONE_NAME: &str = "session.jump_before_zone_name";

/// The warnings known before the launch, and then whether ICU can name the session zone, which is known
/// once the session has chosen the zone's name.
fn with_zone_warning(mut warnings: Vec<String>, zone_unnamed_for_icu: bool) -> Vec<String> {
    if zone_unnamed_for_icu {
        warnings.push(KEY_ZONE_UNNAMED.to_string());
    }
    warnings
}

/// End the application a refusal will not let run, with everything it started (R4-S5), and name what
/// is still running afterwards for the verdict, with whether the family could be looked for in full. Why
/// it could not goes to stderr, where the other human-side details of a start go - the verdict carries
/// the fact, so the report and the window cannot claim the whole application was ended.
fn end_refused_family(session: &chrono_mech::Session) -> (Vec<FollowedProcess>, bool) {
    let end = session.terminate_family();
    if let Some(why) = &end.incomplete {
        diag!("chrono core: {why}");
    }
    let left = end
        .left_running
        .into_iter()
        .map(|m| FollowedProcess { pid: m.pid, image: m.image.as_deref().map(crate::cdp::sanitise_target_text) })
        .collect();
    (left, end.incomplete.is_some())
}

/// Whether the launched process had ended, asked BEFORE `then` reads the registry - the order the family
/// is asked in everywhere (R4-N11). A child the hook follows signs in while its parent is still inside
/// the call that creates it, so a launched process seen ended here left every child it started in the
/// registry `then` reads. Asked after it, a launcher that started the application and ended in between
/// was seen ended next to a registry read from before the child, and the session went on for the
/// application without the report saying so.
fn ended_before<S>(session: &mut S, alive: fn(&S) -> bool, then: impl FnOnce(&mut S)) -> bool {
    let ended = !alive(session);
    then(session);
    ended
}

/// What a native session changes in the environment its target inherits: the variables of the
/// embedded-engine channel, the ones that switch the Gecko sandbox off when the tester asked for that
/// (ADR-28, said by `gecko::warning`), then `TZ` for the session zone (`session_zone_change`). A `TZ` of
/// the tester's own that this takes away is said twice - its value on stderr, the fact in the report,
/// among the warnings that are known before the launch.
fn target_environment(
    launch: &Launch,
    target_path: &str,
    bias: Option<i32>,
    gecko_sandbox: bool,
    warnings: &mut Vec<String>,
) -> Vec<chrono_mech::EnvChange> {
    let mut env = chrono_mech::EnvChange::set_each(&launch.env);
    if !gecko_sandbox {
        env.extend(crate::gecko::sandbox_off());
    }
    let Some(bias) = bias else {
        return env;
    };
    let jiff = crate::pe::links_jiff(std::path::Path::new(target_path));
    let (change, taken) = session_zone_change(jiff, bias, std::env::var_os("TZ"));
    if let Some(own) = taken {
        match &change {
            Some(chrono_mech::EnvChange::Set(_, value)) => {
                diag!("chrono core: TZ={} replaced by TZ={value} for this program, which reads its zone from TZ", own.to_string_lossy());
            }
            _ => diag!(
                "chrono core: TZ={} left out of this program's environment, so the session zone reaches the runtimes that read TZ first",
                own.to_string_lossy()
            ),
        }
        warnings.push(KEY_TZ_VARIABLE_REMOVED.to_string());
    }
    env.extend(change);
    env
}

/// What a session does to `TZ` in its target's environment, and the tester's own value it takes away.
///
/// A `TZ` of the tester's own never reaches the target, because the session zone is the point of the
/// session and the runtimes that read `TZ` take it before the zone the system answers with - the C
/// runtime, and so Python, MSYS2 and Node (measured 2026-10-27 at +05:30 under `TZ` set to UTC+2: Python
/// showed +02:00, Node +00:00). A program built on the Rust `jiff` crate gets the session zone there
/// instead (`pe::links_jiff`): jiff finds the local zone by a name, the session zone's name is in no table
/// it carries, and `TZ` is the one thing it reads first. Only such a program, because Node reads a POSIX
/// value as an unknown zone at +00:00. A `TZ` the same as the one the session gives is not taken away.
fn session_zone_change(
    jiff: bool,
    bias: i32,
    own: Option<std::ffi::OsString>,
) -> (Option<chrono_mech::EnvChange>, Option<std::ffi::OsString>) {
    if jiff {
        let value = crate::zone::posix_tz(bias);
        let taken = own.filter(|own| *own != *value);
        return (Some(chrono_mech::EnvChange::Set("TZ".to_string(), value)), taken);
    }
    let change = own.is_some().then(|| chrono_mech::EnvChange::Remove("TZ".to_string()));
    (change, own)
}

/// `ended` with the exit code of a target that ended on its own. The other fields stay what the start
/// that did not become a running session gives them.
fn with_target_exit(mut ended: Event, code: Option<i32>) -> Event {
    if let Event::Ended { target_exit_code, .. } = &mut ended {
        *target_exit_code = code;
    }
    ended
}

/// Why a target vanished inside the guard window with nothing on the session clock left running, or
/// `None` when it did not vanish but ended. Asked only after the family was found gone, so a process it
/// started is either one the hook never entered or none at all.
///
/// The children this asks the hook's ring about go into `ledger` either way. The ring names each child
/// once, and a family that ended without vanishing goes on to its report, which has to name them.
fn vanish_reason_key(session: &mut chrono_mech::Session, ledger: &mut SessionLedger) -> Option<&'static str> {
    let children = session.poll_uncovered_children();
    let crashed = session.exit_code().is_some_and(crate::report::ended_by_crash);
    let reason = vanish_reason(session.pid, &children, chrono_mech::process_is_alive, session.family_only_console(), crashed);
    ledger.take_ring_children(children);
    reason
}

/// The reason for a vanish, from the children the hook could not enter, from how the target ended and
/// from what the family was: a hand-off when one the target started is still running, whatever the family
/// was, because that child runs on the real clock. Then a crash, which a single-instance application is
/// not. Otherwise the single-instance suspicion ADR-4 was written for, which needs a windowed program in
/// the family (owner decision 2026-10-27, `Session::family_only_console`): a console tool that did its
/// work in a few milliseconds used to be reported as a session that never took effect. A console family
/// that crashed there ends its session the way any other does, and the session says the crash.
fn vanish_reason(
    root: u32,
    children: &[UncoveredChild],
    alive: impl Fn(u32) -> bool,
    only_console: bool,
    crashed: bool,
) -> Option<&'static str> {
    if children.iter().any(|c| c.parent_pid == root && alive(c.pid)) {
        Some(KEY_HANDED_OFF_UNCOVERED)
    } else if only_console {
        None
    } else if crashed {
        Some(KEY_TARGET_CRASHED)
    } else {
        Some("target.single_instance_suspected")
    }
}

/// Whether any process of the family the hook reached is still running as the session ends - the
/// application the tester is left with. A recycled pid reads as alive, which errs toward saying so
/// once too often, never toward keeping quiet about a process left behind.
fn family_left_running(session: &chrono_mech::Session, family_pids: &HashSet<u32>) -> bool {
    session.is_alive() || family_pids.iter().any(|&pid| chrono_mech::process_is_alive(pid))
}

/// How many processes ran uncovered that nobody reported through the hook's own ring or could name: the
/// ring's own count, plus the processes the process tree under an elevated host turned up that the ring
/// never held, less the ones both named. The list can never be longer than the number it is a list of.
fn uncovered_total(ring_total: u32, tree_found: u32, duplicated: u32, listed: usize) -> u32 {
    ring_total.saturating_add(tree_found).saturating_sub(duplicated).max(listed as u32)
}

/// Put what the registry step has to say about the end of the session among the session warnings. A
/// value that could not be taken away goes FIRST: the panel shows the list through a window shorter than
/// the list, and that is the one line that says something is still set on this machine. The account of a
/// value that was removed goes last, with the other plain facts.
fn place_policy_keys(warnings: &mut Vec<String>, keys: Vec<String>) {
    if keys.iter().any(|k| k == KEY_LEFT) {
        warnings.splice(0..0, keys);
    } else {
        warnings.extend(keys);
    }
}

/// End the session and state what it did: one last fold so a late child still counts, the coverage
/// every process ENDED with, the family verdict and `ended`. Returns the family's exit code.
pub(crate) fn close_session(
    mut session: chrono_mech::Session,
    mut ledger: SessionLedger,
    mut bridge: EmbeddedBridge,
    mut policy: PolicySession,
    target_exit: Option<i32>,
) -> i32 {
    // Final fold so a child that joined since the last heartbeat still counts in the family.
    ledger.poll(&mut session);
    let SessionLedger {
        mut family,
        family_pids,
        mut uncovered_children,
        clock_clamped,
        duration_clamped,
        followed,
        duplicated,
        unhooked_pids: _,
        slept_announced_ms,
        absolute_due_before_move,
        window_seen: _,
        copies_outside,
        copy_search_failed,
    } = ledger;
    let followed = followed.unwrap_or_default();
    // The application may outlive the session - a `--ticks` cutoff, a Stop in the panel - and the
    // session does not stop it (docs/01 section 8.4). It lets it go instead, and the pages have to be
    // let go while their connections are still open, so this comes before `finish`.
    let left_running = family_left_running(&session, &family_pids);
    // One deadline for everything the pages are still asked at the end - the release and the last
    // counts - because a GUI gives the core two seconds after `end` (ADR-20).
    let end_by = Instant::now() + END_WAIT;
    let origin = host_origin(&session, bridge.scale_duration());
    if left_running {
        bridge.release_pages(origin, end_by);
    }
    // What the pages inside the application did, folded into the family like any process: a page
    // shimmed and reading time is covered, one refused or failed is not, and none at all judges
    // nothing (docs/09 section 12.7).
    let hosts: Vec<u32> = std::iter::once(session.pid).chain(family_pids.iter().copied()).collect();
    bridge.look_for_webview2_last(&hosts);
    // One last look at the tree under an elevated host, for what appeared in the final second.
    let known: Vec<u32> = hosts.iter().copied().chain(uncovered_children.iter().map(|c| c.pid)).collect();
    uncovered_children.extend(bridge.follow_host_tree_last(&known));
    let pages = bridge.finish(origin, end_by);
    // The engine is let go of, so the registry value that let the session reach it goes too - before
    // the verdict, so the verdict can say how that went.
    let policy_keys = policy.finish();
    let page_rows = covered_channels(pages.counts.clone());
    // Pooled pages ran on the session clock (R5-P20), so only the ones the shim did not reach count
    // against the verdict.
    family = family.combine(embedded_verdict(pages.reached, pages.contexts, !page_rows.is_empty(), pages.failed));
    // An application that loaded WebView2 while no engine of it was reached: its pages may have run on
    // the real clock, and nothing the hook saw says otherwise - for an elevated host the engine is
    // started by a system service, so the hook never sees it at all. The family cannot be `works`.
    if pages.engine_missed() {
        family = family.combine(Verdict::Partial);
    }
    let uncovered_children_total = uncovered_total(
        session.uncovered_children_total(),
        pages.tree_found,
        duplicated,
        uncovered_children.len(),
    );
    // A process nobody reached ran on the real clock: that is "something uncovered" for the family,
    // so the family cannot be `works` (untouchable rule 4 at the session level - the verdict model
    // already says so, this is the fact it was never fed). Folded as a verdict rather than a flag so
    // the same `combine` rule that rolls up channels rolls up processes.
    let any_uncovered_children = uncovered_children_total > 0 || !uncovered_children.is_empty();
    if any_uncovered_children {
        family = family.combine(Verdict::Fails);
    }
    // The copy a single-instance application most likely handed its work to runs on the real clock, so
    // it counts like a process nobody reached (ADR-4, 2026-11-11).
    let (copies, hand_off_key) = hand_off_findings(copies_outside, copy_search_failed);
    let handed_to_copy = !copies.is_empty();
    if handed_to_copy {
        family = family.combine(Verdict::Fails);
    }
    // Capture the session clocks before ending so `ended` can state the duration and the fake wall
    // clock reached - reliably, even for a session too short to have emitted a heartbeat.
    let final_state = session.state();
    // A wake-up after the last heartbeat is announced here, before the verdict that sums it up.
    if let Some(slept) = sleep_to_announce(slept_announced_ms, final_state.host_slept_ms) {
        emit(&Event::HostSlept {
            v: PROTOCOL_VERSION,
            slept_ms: slept as u64,
            total_slept_ms: final_state.host_slept_ms as u64,
        });
    }
    // Read BEFORE `end` unmaps the control block. A full PID registry means some processes of this
    // family ran with nowhere to report coverage into, so both the count below and the channel lists
    // are short - and no per-process event can carry that, because those processes have no pid here
    // (R2-S9). It rides the session verdict, which is the one event that speaks for the whole family.
    let uncovered_processes = session.uncovered_process_count();
    let unwatchable_members = session.unwatchable_member_count();
    let jumped_before_zone_name = session.jumped_before_zone_name();

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
    // Rate 1 before the core leaves, after everything `ended` reports has been read. Each process of
    // the family freezes its duration axes when its hook sees the core gone (`release_duration_axes`),
    // and at rate 1 every one of them freezes on the same line whenever its watcher happens to wake -
    // at the session rate they would disagree by the rate times the gap between two wake-ups.
    if left_running {
        session.set_multiplier(1);
    }
    session.end();
    // The sticky flag OR the final sample, so a session too short to have emitted a heartbeat still
    // reports a clamped clock.
    let mut session_warnings = native_session_warnings(
        uncovered_processes,
        clock_clamped || final_state.clock_at_range_end(),
        duration_clamped || final_state.duration_at_range_end(),
        substituted_reads(&final_coverage, &page_rows),
    );
    session_warnings.extend(session_clock_warnings(final_state.host_slept_ms, absolute_due_before_move, jumped_before_zone_name));
    let mut children_warnings = uncovered_children_warnings(&uncovered_children, uncovered_children_total);
    reconcile_engine_warnings(&mut children_warnings, every_renderer_reached(&uncovered_children, &pages.engines));
    session_warnings.extend(children_warnings);
    session_warnings.extend(pages.session_warnings(left_running));
    // First, as the policy value left behind is: the panel shows the list through a window shorter
    // than the list, and a crash is what the tester has to read before anything about the clock.
    if target_exit.is_some_and(crate::report::ended_by_crash) {
        session_warnings.insert(0, KEY_SESSION_TARGET_CRASHED.to_string());
    }
    place_policy_keys(&mut session_warnings, policy_keys);
    session_warnings.extend(hand_off_key.map(str::to_string));
    if !followed.is_empty() {
        session_warnings.push(KEY_FOLLOWED_FAMILY.to_string());
    }
    if unwatchable_members > 0 {
        session_warnings.push(KEY_MEMBER_UNWATCHABLE.to_string());
    }
    if left_running {
        session_warnings.push(KEY_LEFT_RUNNING.to_string());
    }
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
        reason_key: session_reason_key(family, any_uncovered_children, handed_to_copy).into(),
        process_count: 1 + family_pids.len() as u32,
        warning_keys: session_warnings,
        uncovered_children: named,
        uncovered_children_total,
        context_count: u32::try_from(pages.contexts).unwrap_or(u32::MAX),
        engines: pages.engines,
        // The image name comes from the process list, text from the target's world like the
        // uncovered children's, and passes the same sieve.
        followed: followed
            .iter()
            .map(|m| FollowedProcess { pid: m.pid, image: m.image.as_deref().map(crate::cdp::sanitise_target_text) })
            .collect(),
        copies_outside: copies,
        host_slept_ms: reported_sleep_ms(final_state.host_slept_ms),
        // A native session drives no browser of its own - the engines it reached say what they are.
        browser: None,
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

/// What the family's end says about a hand-off to a copy outside the session (ADR-4, 2026-11-11): the
/// copies the verdict names, as the wire carries them, with the image name through the sieve every text
/// from the target's world goes through, and the warning that goes with them. A search that could not
/// read the process list names nothing and has a warning of its own. The copies are already the ones
/// outside the session: the mechanism left out every process it could tell was the session's own,
/// a child the hook never entered included.
fn hand_off_findings(copies: Vec<RunningCopy>, search_failed: bool) -> (Vec<FollowedProcess>, Option<&'static str>) {
    let named: Vec<FollowedProcess> = copies
        .into_iter()
        .map(|copy| FollowedProcess { pid: copy.pid, image: Some(crate::cdp::sanitise_target_text(&copy.image)) })
        .collect();
    let key = if named.is_empty() { search_failed.then_some(KEY_COPY_SEARCH_FAILED) } else { Some(KEY_HANDED_TO_COPY) };
    (named, key)
}

/// Stable reason key for the family (session) verdict, scoped to the whole family.
///
/// With uncovered children the family is `partial` or `fails` by construction (`close_session`
/// folds a `Fails` in), and the reason has to say WHY in words a tester can act on: not "some
/// channels were queried but not covered" - which is about what a hooked process read - but that a
/// process of this application ran with no hook in it at all.
///
/// A hand-off to a copy outside the session comes first (ADR-4, 2026-11-11): it is why the session may
/// have tested nothing at all, which no other reason says.
pub(crate) fn session_reason_key(v: Verdict, uncovered_children: bool, handed_to_copy: bool) -> &'static str {
    if handed_to_copy {
        return KEY_FAMILY_HANDED_TO_COPY;
    }
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
    let renderer = children.iter().any(is_renderer_child);
    let engine = children.iter().any(|c| c.image.as_deref().is_some_and(is_web_engine_subprocess));
    if renderer {
        warnings.push(KEY_WEB_ENGINE_UNCOVERED.to_string());
    } else if engine {
        warnings.push("embedded.web_engine_processes_uncovered".to_string());
    }
    warnings
}

/// A child whose command line names the role web pages run in.
fn is_renderer_child(child: &UncoveredChild) -> bool {
    child.command_line.as_deref().and_then(role_from_command_line).is_some_and(|r| is_renderer_role(&r))
}

/// Whether every renderer among the uncovered children had pages put on the session clock: its parent is
/// an engine the session reached and shimmed at least one page or worker through (R5, P15). A renderer is
/// spawned by the browser process that holds the debugging port, so its parent is that engine's pid - the
/// relation the window's process table reads too. The session's total used to decide this for every
/// renderer at once, so with two engines reached and pages in one, the renderer of the other lost the
/// warning that its pages read the real clock. A renderer whose parent is no engine keeps the warning.
pub(crate) fn every_renderer_reached(children: &[UncoveredChild], engines: &[ReachedEngine]) -> bool {
    children
        .iter()
        .filter(|c| is_renderer_child(c))
        .all(|c| engines.iter().any(|e| e.pid == c.parent_pid && e.context_count > 0))
}

/// Resolve a jump's moment to a UTC FILETIME, through the same gate a start goes through.
///
/// A target that names no zone is read in the SESSION zone (`session_bias`), like every other moment
/// of the session. It was read as UTC, against the documentation of `Moment`, so a client that left
/// the field out landed the clock an offset away from the time it asked for (R4-S9) - the error the
/// driver's own `--jump-after` had been fixed for. One that names a zone keeps it: the moment is read
/// there and the session zone does not change.
fn moment_at(local: &str, bias: Option<i32>, session_bias: i32) -> Result<i64, &'static str> {
    let m = Moment { local: local.to_string(), tz_bias_min: Some(bias.unwrap_or(session_bias)) };
    chrono_core::session_instant(&m).map(|(ft, _)| ft).map_err(|e| moment_error_key(&e))
}

/// The hook DLL sits next to the executable (same target dir, matching bitness), the real one when chrono was
/// started through a symbolic link (`own_path`).
pub(crate) fn hook_dll_path() -> Option<std::path::PathBuf> {
    let exe = crate::own_path::own_exe().ok()?;
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
    let mode = start_time_mode(time).map_err(|key| (1, key))?;
    let moment = Moment {
        local: time.moment.local.clone().unwrap_or_default(),
        tz_bias_min: time.moment.tz_bias_min,
    };
    // Checked here, before anything is launched, with the key that names the fault. `prepare` checks
    // again for itself, but by then the target's command line has been built.
    chrono_core::session_instant(&moment).map_err(|e| (1, moment_error_key(&e)))?;
    Ok(SessionSpec { moment, mode, scale_duration: time.scale_duration, scale_qpc: time.scale_qpc })
}

/// What the core says when it took over a previous session's control block, or `None` when there was
/// none to take over. The wording follows how that session had ended (R4-D19): after an ordered end the
/// block survives only because the application kept running, and it is on the real clock since that end
/// - "a previous core had died", said there before R4-W2, was false.
pub(crate) fn reclaim_notice(reclaimed: chrono_mech::Reclaimed) -> Option<&'static str> {
    use chrono_mech::Reclaimed as R;
    match reclaimed {
        R::Nothing => None,
        R::EndedSession => Some(
            "chrono core: took over the control block of a session that ended while its application \
             kept running - that application is on the real clock",
        ),
        R::DeadCore => Some("chrono core: reclaimed an orphaned session (a previous core had died)"),
    }
}

/// The key for a target that ended while Windows was loading it (R4-S6). The four statuses the loader
/// ends a process with when a static import fails are each named for what the tester can do about it,
/// and every other code - a library's start-up code that ended the process itself, say - gets one key
/// that says only what is known: the application ended before it ran.
pub(crate) fn loader_failure_key(code: u32) -> &'static str {
    match code {
        0xC000_0135 => "target.loader_dll_not_found",
        0xC000_0139 => "target.loader_entry_missing",
        0xC000_007B => "target.loader_bad_image",
        0xC000_0142 => "target.loader_init_failed",
        _ => "target.died_while_loading",
    }
}

pub(crate) fn map_prepare_error(e: chrono_mech::PrepareError) -> (i32, &'static str, &'static str, String) {
    use chrono_mech::PrepareError as P;
    match e {
        P::Moment(e) => (1, moment_error_key(&e), "core", e.to_string()),
        P::Control(m) => (3, "session.control_failed", "mechanism", m),
        P::Launch(m) => (2, "target.launch_failed", "mechanism", m),
        P::Inject(m) => (2, "target.inject_failed", "mechanism", m),
        // The application's own start failed before the hook got its turn (R4-S6): exit 2 like every
        // failure of the target, with the code on the detail line whichever key names it.
        P::EndedLoading(code) => (
            2,
            loader_failure_key(code),
            "mechanism",
            format!("the target ended while Windows was loading it, with code 0x{code:08X}"),
        ),
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
        //
        // Exit code 7 of its own (R5-D15): the one start failure that trying again later fixes, so a
        // pipeline can wait and retry instead of reading it as the internal error 3 used to say it was.
        P::SessionActive(0) => (
            SESSION_BUSY_EXIT,
            "session.already_active",
            "mechanism",
            "another session's core is starting or running - one session at a time".to_string(),
        ),
        P::SessionActive(pid) => (
            SESSION_BUSY_EXIT,
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
    use chrono_proto::{MomentKind, MomentSpec, TimeModeKind};

    /// A `TZ` of the tester's own never reaches the target and is named as taken, whatever the program: a
    /// program built on jiff gets the session zone in its place, any other gets none. A `TZ` the same as
    /// the one the session gives is not taken, and a tester without `TZ` loses nothing.
    #[test]
    fn a_tz_of_the_testers_own_is_taken_from_every_target_and_jiff_gets_the_session_zone() {
        use chrono_mech::EnvChange::{Remove, Set};
        let own = |value: &str| Some(std::ffi::OsString::from(value));
        let session = || Some(Set("TZ".to_string(), "CMS-5:30".to_string()));
        let cases = [
            ("not jiff, no TZ", false, None, None, None),
            ("not jiff, TZ of the tester's", false, own("EET-2"), Some(Remove("TZ".to_string())), own("EET-2")),
            ("not jiff, TZ like the session's", false, own("CMS-5:30"), Some(Remove("TZ".to_string())), own("CMS-5:30")),
            ("jiff, no TZ", true, None, session(), None),
            ("jiff, TZ of the tester's", true, own("EET-2"), session(), own("EET-2")),
            ("jiff, TZ the session gives", true, own("CMS-5:30"), session(), None),
        ];
        for (label, jiff, tester, change, taken) in cases {
            assert_eq!(session_zone_change(jiff, -330, tester), (change, taken), "{label}");
        }
    }

    /// R5-P1, R5-D34. A sleep of the machine reaches the log once, when it is at least a second long, and
    /// each later one on its own - never the same sleep twice, never a resume of a fraction of a second.
    #[test]
    fn a_sleep_of_the_machine_is_announced_once_and_only_from_a_second_up() {
        assert_eq!(sleep_to_announce(0, 0), None, "no sleep");
        assert_eq!(sleep_to_announce(0, 999), None, "under a second is a resume, not a sleep worth a line");
        assert_eq!(sleep_to_announce(0, 1_000), Some(1_000));
        assert_eq!(sleep_to_announce(0, 28_800_000), Some(28_800_000), "eight hours");
        // Already announced: nothing new until the next sleep, which is told on its own.
        assert_eq!(sleep_to_announce(28_800_000, 28_800_000), None);
        assert_eq!(sleep_to_announce(28_800_000, 28_800_500), None);
        assert_eq!(sleep_to_announce(28_800_000, 28_860_000), Some(60_000));
        // The verdict carries the total from the same threshold, and nothing below it.
        assert_eq!(reported_sleep_ms(999), 0);
        assert_eq!(reported_sleep_ms(1_000), 1_000);
    }

    /// R5-P1, R5-D35 and ADR-26: each warning only when it happened.
    #[test]
    fn the_session_clock_warnings_say_only_what_happened() {
        assert!(session_clock_warnings(0, false, false).is_empty());
        assert!(session_clock_warnings(999, false, false).is_empty());
        assert_eq!(session_clock_warnings(5_000, false, false), vec![KEY_HOST_SLEPT]);
        assert_eq!(session_clock_warnings(0, true, false), vec![KEY_ABSOLUTE_DUE]);
        assert_eq!(session_clock_warnings(0, false, true), vec![KEY_JUMP_BEFORE_ZONE_NAME]);
        assert_eq!(
            session_clock_warnings(5_000, true, true),
            vec![KEY_HOST_SLEPT, KEY_ABSOLUTE_DUE, KEY_JUMP_BEFORE_ZONE_NAME]
        );
    }

    /// ADR-26: the zone's warning joins those known before the launch only when ICU cannot name the zone,
    /// and after them, in the order they were found.
    #[test]
    fn the_zone_warning_follows_the_ones_known_before_the_launch_only_when_icu_cannot_name_the_zone() {
        let known = vec![KEY_TZ_VARIABLE_REMOVED.to_string()];
        assert_eq!(with_zone_warning(known.clone(), false), known);
        assert_eq!(with_zone_warning(known, true), vec![KEY_TZ_VARIABLE_REMOVED, KEY_ZONE_UNNAMED]);
        assert_eq!(with_zone_warning(Vec::new(), true), vec![KEY_ZONE_UNNAMED]);
    }

    /// R5-D35. The ledger remembers whether a deadline had been converted when the clock moved: a move with
    /// none converted yet says nothing, and a later move with none new does not take back an earlier one.
    #[test]
    fn a_converted_deadline_counts_only_when_the_clock_moved_after_it() {
        let mut ledger = SessionLedger::new(Verdict::Works);
        ledger.note_clock_moved(0);
        assert!(!ledger.absolute_due_before_move, "a move with nothing converted yet");
        ledger.note_clock_moved(3);
        assert!(ledger.absolute_due_before_move);
        ledger.note_clock_moved(0);
        assert!(ledger.absolute_due_before_move, "a later move does not take it back");
    }

    /// A target the loader ended is named by its status (R4-S6): four statuses each with the key that
    /// says what to do about it, every other code with the general one, all exit 2, and the code on the
    /// detail line whichever key names it.
    #[test]
    fn a_target_the_loader_ended_is_named_by_its_status() {
        for (code, key) in [
            (0xC000_0135, "target.loader_dll_not_found"),
            (0xC000_0139, "target.loader_entry_missing"),
            (0xC000_007B, "target.loader_bad_image"),
            (0xC000_0142, "target.loader_init_failed"),
            (0xC000_0017, "target.died_while_loading"),
            (1, "target.died_while_loading"),
        ] {
            let (exit, got, origin, detail) = map_prepare_error(chrono_mech::PrepareError::EndedLoading(code));
            assert_eq!((exit, got, origin), (2, key, "mechanism"), "code 0x{code:08X}");
            assert!(detail.contains(&format!("0x{code:08X}")), "the code is not on the detail line: {detail}");
        }
    }

    /// A start refused because another session runs ends with the busy code, not the internal-error
    /// code it shared with a crashed core (R5-D15), whether or not the other core has named itself yet.
    #[test]
    fn another_running_session_is_the_busy_code_not_an_internal_error() {
        for pid in [0, 4242] {
            let (exit, key, origin, detail) = map_prepare_error(chrono_mech::PrepareError::SessionActive(pid));
            assert_eq!((exit, key, origin), (SESSION_BUSY_EXIT, "session.already_active", "mechanism"), "pid {pid}");
            assert_eq!(exit, 7, "the number docs/08 publishes");
            assert!(detail.contains("one session at a time"), "got: {detail}");
        }
    }

    /// A launcher that signs a child in and ends between two looks: running and alone at the first look,
    /// ended with the child in the registry from the second on. Each look counts itself, and the registry
    /// read says whether it saw the child.
    struct LauncherWorld {
        looks: std::cell::Cell<u32>,
        registry_saw_child: Option<bool>,
    }

    fn launcher_runs(world: &LauncherWorld) -> bool {
        let look = world.looks.get();
        world.looks.set(look + 1);
        look == 0
    }

    fn read_registry(world: &mut LauncherWorld) {
        let look = world.looks.get();
        world.looks.set(look + 1);
        world.registry_saw_child = Some(look >= 1);
    }

    /// The family the session went on for is noted from a registry read taken after the launched process
    /// was seen ended (R4-N11), so it holds every child the launcher started. The same world asked registry
    /// first notes an ended launcher over a read from before its child, and the report stayed silent that
    /// the session went on for the application.
    #[test]
    fn a_launcher_seen_ended_is_noted_with_every_child_it_started() {
        let mut world = LauncherWorld { looks: std::cell::Cell::new(0), registry_saw_child: None };
        assert!(!ended_before(&mut world, launcher_runs, read_registry), "the launcher still ran at the first look");
        assert_eq!(world.registry_saw_child, Some(true), "the registry is read every time");
        assert!(ended_before(&mut world, launcher_runs, read_registry));
        assert_eq!(world.registry_saw_child, Some(true), "an ended launcher was noted over a registry without its child");

        let mut world = LauncherWorld { looks: std::cell::Cell::new(0), registry_saw_child: None };
        read_registry(&mut world);
        let ended = !launcher_runs(&world);
        assert!(ended && world.registry_saw_child == Some(false), "this world does not reproduce the race the order is there for");
    }

    /// A takeover is always said, and said the way the previous session ended (R4-D19). The ordered end
    /// used to be reported as a dead core, which it was not.
    #[test]
    fn a_takeover_says_how_the_previous_session_ended() {
        use chrono_mech::Reclaimed;
        assert_eq!(reclaim_notice(Reclaimed::Nothing), None);
        let ended = reclaim_notice(Reclaimed::EndedSession).expect("an ordered end's takeover is said");
        assert!(ended.contains("ended while its application kept running"), "{ended}");
        assert!(!ended.contains("died"), "an ordered end was reported as a dead core: {ended}");
        let died = reclaim_notice(Reclaimed::DeadCore).expect("a dead core's takeover is said");
        assert!(died.contains("a previous core had died"), "{died}");
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
                kind: MomentKind::Absolute,
                local: Some("2038-01-01T00:00:00".into()),
                tz_bias_min: Some(0),
                delta: None,
            },
            mode: TimeModeKind::Multiplier,
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

    fn moment_spec(local: &str, bias: Option<i32>) -> MomentSpec {
        MomentSpec { kind: MomentKind::Absolute, local: Some(local.into()), tz_bias_min: bias, delta: None }
    }

    /// R4-S7 and R4-S9 at the native start: the gate runs before anything launches and names the fault
    /// - a zone the hook's local channels cannot carry, or an instant the clock cannot hold.
    #[test]
    fn build_spec_holds_the_moment_and_its_zone_to_the_session_gate() {
        let spec = |local: &str, bias: Option<i32>| TimeSpec {
            moment: moment_spec(local, bias),
            mode: TimeModeKind::Flow,
            multiplier: None,
            scale_duration: false,
            scale_qpc: false,
        };
        assert!(build_spec(&spec("2038-01-01T00:00:00", Some(-899))).is_ok());
        assert_eq!(build_spec(&spec("2038-01-01T00:00:00", None)).err(), Some((1, "time.bad_zone")));
        assert_eq!(build_spec(&spec("2038-01-01T00:00:00", Some(i32::MAX))).err(), Some((1, "time.bad_zone")));
        // The report's two examples: past the clock's end, and before 1601 in the session zone.
        assert_eq!(build_spec(&spec("30828-09-14T02:48:05", Some(0))).err(), Some((1, "moment.out_of_range")));
        assert_eq!(build_spec(&spec("1600-12-31T20:00:00", Some(300))).err(), Some((1, "moment.out_of_range")));
        assert_eq!(build_spec(&spec("2038-02-30T00:00:00", Some(0))).err(), Some((1, "moment.invalid")));
    }

    /// R4-S9: an absolute jump that names no zone is read in the SESSION zone, not in UTC - and one
    /// that names a zone keeps it. Both go through the same gate as a start.
    #[test]
    fn an_absolute_jump_without_a_zone_is_read_in_the_session_zone() {
        let east2 = chrono_core::moment_to_filetime_utc(&Moment {
            local: "2030-06-15T12:00:00".into(),
            tz_bias_min: Some(-120),
        })
        .unwrap();
        assert_eq!(moment_at("2030-06-15T12:00:00", None, -120), Ok(east2));
        assert_eq!(
            moment_at("2030-06-15T12:00:00", Some(0), -120),
            Ok(east2 + 120 * 60 * 10_000_000),
            "a zone named on the jump is the one it is read in"
        );
        assert_eq!(moment_at("2030-06-15T12:00:00", Some(-900), 0), Err("time.bad_zone"));
        assert_eq!(moment_at("1000-01-01T00:00:00", None, 0), Err("moment.out_of_range"));
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

    fn read(stream: &str) -> Result<Start, StartRefusal> {
        read_start(&mut BufReader::new(std::io::Cursor::new(stream.as_bytes().to_vec())))
    }

    /// A `start` built through the real serializer rather than hand-written JSON, so the test cannot
    /// drift from the shape the wire actually carries.
    fn start_line(v: u32, cwd: Option<&str>) -> String {
        start_line_with_id(v, 1, cwd)
    }

    fn start_line_with_id(v: u32, id: u64, cwd: Option<&str>) -> String {
        serde_json::to_string(&Command::Start {
            v,
            id,
            target: TargetSpec {
                path: "app.exe".into(),
                args: Vec::new(),
                cwd: cwd.map(str::to_string),
                embedded: true,
                elevated_embedded: false,
                keep_background_timers: false,
                gecko_sandbox: true,
                console: Default::default(),
            },
            time: TimeSpec {
                moment: MomentSpec {
                    kind: MomentKind::Absolute,
                    local: Some("2038-01-19T03:14:07".into()),
                    tz_bias_min: Some(0),
                    delta: None,
                },
                mode: TimeModeKind::Flow,
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

    /// `REFUSALS_WITHOUT_ENDED` is exactly the refusals that owe no `ended`: every refusal `read_start` can
    /// give is in it if and only if it owes none, and every key in it is one of those. The driver treats a
    /// missing `ended` after any error outside the list as a session the core left open (R4-W3), so a new
    /// refusal that drifted from the list would be misread in one direction or the other.
    #[test]
    fn the_refusals_that_owe_no_ended_are_exactly_the_listed_ones() {
        let past_the_cap = "x".repeat(crate::wire::MAX_PROTOCOL_LINE + 10) + "\n";
        let end = serde_json::to_string(&Command::End { v: PROTOCOL_VERSION, id: 1 }).unwrap() + "\n";
        let inputs = [
            String::new(),
            past_the_cap,
            "not a command\n".to_string(),
            end,
            start_line(PROTOCOL_VERSION + 1, None),
            start_line(PROTOCOL_VERSION, Some("C:/no-such-folder-chrono-mock-test")),
        ];
        let mut without_ended = std::collections::BTreeSet::new();
        for input in &inputs {
            let refused = read(input).expect_err("every input here is refused");
            assert_eq!(
                REFUSALS_WITHOUT_ENDED.contains(&refused.key),
                !refused.needs_ended,
                "{} owes an ended: {}, and the list says the opposite",
                refused.key,
                refused.needs_ended
            );
            if !refused.needs_ended {
                without_ended.insert(refused.key);
            }
        }
        assert_eq!(
            without_ended,
            REFUSALS_WITHOUT_ENDED.into_iter().collect(),
            "a key in the list that no refusal gives, or the reverse"
        );
    }

    /// The happy path carries the target and the time through untouched, and nothing else opens a
    /// session on the way.
    #[test]
    fn a_well_formed_start_carries_its_target_and_time_through() {
        let Start { id, target, time, force } =
            read(&start_line(PROTOCOL_VERSION, None)).expect("a well-formed start is accepted");
        assert_eq!(id, 1);
        assert_eq!(target.path, "app.exe");
        assert_eq!(target.cwd, None);
        assert_eq!(time.mode, TimeModeKind::Flow);
        assert_eq!(time.moment.local.as_deref(), Some("2038-01-19T03:14:07"));
        assert!(!force);
    }

    /// R5-P18: an answer to `start` carries the id the client gave it. Read at an id that is not 1, the
    /// one both built-in clients send, so the old constant cannot pass for it - and every answer the
    /// core gives before its session runs takes the id from the start, not from a digit in the source.
    #[test]
    fn an_answer_to_start_carries_the_id_the_client_gave_it() {
        assert_eq!(read(&start_line_with_id(PROTOCOL_VERSION, 42, None)).expect("accepted").id, 42);
        let refused = read(&start_line_with_id(PROTOCOL_VERSION, 42, Some("C:/no-such-folder-chrono-mock-test")))
            .expect_err("expected a refusal");
        assert_eq!(refused.id, Some(42));
        for (file, source) in [("core.rs", include_str!("core.rs")), ("cdp_session.rs", include_str!("cdp_session.rs"))] {
            let production = &source[..source.find("#[cfg(test)]").expect("a test module")];
            assert!(!production.contains("id: Some(1)"), "{file} answers the start with a fixed id");
        }
    }

    /// A value left in the registry is said first, because the panel shows the warnings through a window
    /// shorter than the list and it is the one line that says something is still set. A value that was
    /// removed is a plain fact and goes last, and nothing to say changes nothing.
    #[test]
    fn a_registry_value_left_behind_is_the_first_warning_and_a_removed_one_the_last() {
        use crate::policy_session::KEY_REMOVED;
        let base = || vec!["a".to_string(), "b".to_string()];

        let mut warnings = base();
        place_policy_keys(&mut warnings, vec![KEY_LEFT.to_string()]);
        assert_eq!(warnings, [KEY_LEFT, "a", "b"]);

        let mut warnings = base();
        place_policy_keys(&mut warnings, vec![KEY_REMOVED.to_string()]);
        assert_eq!(warnings, ["a", "b", KEY_REMOVED]);

        let mut warnings = base();
        place_policy_keys(&mut warnings, Vec::new());
        assert_eq!(warnings, ["a", "b"]);
    }

    /// Every way a start can end once the registry step has run says what became of the value it wrote. The
    /// three ends that follow it - a vanished target, a refusal, a start that could not be prepared - are one
    /// line each, and a line that said `ended_clean()` would let a value that could not be removed go
    /// without a word. The unit tests cover the account itself and not that each end asks for it, and
    /// reaching an end with a value that cannot be removed takes a real elevated session, so this reads
    /// the source. The count is a canary: a body that lost its ends would pass the first assertion alone.
    #[test]
    fn no_start_ends_clean_once_the_registry_step_has_run() {
        let source = include_str!("core.rs");
        let from = source.find("PolicySession::start(&target").expect("the registry step is where the guard starts");
        let rest = &source[from..];
        let body = &rest[..rest.find("\n}\n").expect("the function ends at a closing brace in column 0")];
        assert!(
            !body.contains("ended_clean()"),
            "an end after the registry step said ended_clean(): use policy.ended_before_running(), which says a value that is still set"
        );
        assert_eq!(
            body.matches("policy.ended_before_running()").count(),
            3,
            "the vanished, refused and unprepared ends each ask the registry step for the account"
        );
    }

    /// The tree and the hook's ring may name the same process. It is listed once and the total gives the
    /// second naming back, and a ring child the tree never named is listed as always.
    #[test]
    fn a_process_named_by_both_the_tree_and_the_ring_is_listed_once() {
        let named = |pid| UncoveredChild { pid, parent_pid: 42, image: Some("engine.exe".into()), command_line: None };
        let mut ledger = SessionLedger::new(Verdict::Works);
        ledger.adopt_unhooked(vec![named(77)], &pids(&[77]));

        ledger.take_ring_children(vec![named(77), named(88)]);

        assert_eq!(ledger.uncovered_children.iter().map(|c| c.pid).collect::<Vec<_>>(), [77, 88]);
        assert_eq!(ledger.duplicated, 1);
        // Two ring children that share a pid are two processes: only the tree's names are compared.
        ledger.take_ring_children(vec![named(88), named(88)]);
        assert_eq!(ledger.uncovered_children.len(), 4);
        assert_eq!(ledger.duplicated, 1);
    }

    fn pids(of: &[u32]) -> HashSet<u32> {
        of.iter().copied().collect()
    }

    /// The system hands a pid on once its process is gone. The tree named 77 and 77 left the tree, so a ring
    /// child that has the same pid is another process: it is listed and nothing is given back, where
    /// comparing against every pid the tree ever named would drop it as a duplicate.
    #[test]
    fn a_ring_child_that_has_the_pid_of_a_process_gone_from_the_tree_is_a_new_process() {
        let named = |pid| UncoveredChild { pid, parent_pid: 42, image: Some("engine.exe".into()), command_line: None };
        let mut ledger = SessionLedger::new(Verdict::Works);
        ledger.adopt_unhooked(vec![named(77)], &pids(&[77]));
        ledger.adopt_unhooked(Vec::new(), &pids(&[]));

        ledger.take_ring_children(vec![named(77)]);

        assert_eq!(ledger.duplicated, 0, "a different process is not a duplicate");
        assert_eq!(ledger.uncovered_children.iter().map(|c| c.pid).collect::<Vec<_>>(), [77, 77]);
    }

    /// The total is the ring's count plus what the tree found, less what both named, and never less than
    /// the list it counts.
    #[test]
    fn the_uncovered_total_adds_the_tree_to_the_ring_and_gives_back_what_both_named() {
        assert_eq!(uncovered_total(3, 0, 0, 3), 3, "no tree: the ring's own count");
        assert_eq!(uncovered_total(0, 6, 0, 6), 6, "an elevated host: the tree alone");
        assert_eq!(uncovered_total(3, 6, 1, 8), 8, "one process named by both is counted once");
        assert_eq!(uncovered_total(0, 0, 0, 5), 5, "never fewer than the list");
        assert_eq!(uncovered_total(1, 0, 4, 0), 0, "a count cannot go below zero");
    }

    /// What the process tree names under an elevated host is listed with the children the hook could not
    /// follow into, and its pids are in the family a debugging port is looked for in.
    #[test]
    fn processes_named_by_the_tree_join_the_family_and_the_uncovered_list() {
        let mut ledger = SessionLedger::new(Verdict::Works);
        let named = |pid| UncoveredChild { pid, parent_pid: 42, image: Some("engine.exe".into()), command_line: None };
        ledger.adopt_unhooked(vec![named(77), named(78)], &pids(&[77, 78]));

        assert_eq!(ledger.uncovered_children.len(), 2);
        let family = ledger.family(42);
        assert!(family.contains(&42) && family.contains(&77) && family.contains(&78), "{family:?}");
        assert!(!ledger.hosts(42).contains(&77), "an engine's process is not a host: it carries no client library");
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
                            capped: false,
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

    /// The renderer warning gives way only for a renderer whose OWN engine had pages put on the session
    /// clock (R5, P15). With two engines reached and pages in one, the session's total used to soften the
    /// warning for both, and the renderer of the engine with none read the real clock all the same.
    #[test]
    fn a_renderer_counts_as_reached_only_through_an_engine_that_had_pages() {
        let renderer_of = |pid, parent_pid| UncoveredChild {
            pid,
            parent_pid,
            image: Some("engine.exe".into()),
            command_line: Some("engine.exe --type=renderer".into()),
        };
        let engine = |pid, context_count| ReachedEngine { pid, port: 9000, browser: "Engine/1.0".into(), context_count };
        let both = [engine(100, 3), engine(200, 0)];

        assert!(every_renderer_reached(&[renderer_of(1, 100)], &both), "its engine had pages");
        assert!(!every_renderer_reached(&[renderer_of(2, 200)], &both), "its engine had none, the other's do not count");
        assert!(!every_renderer_reached(&[renderer_of(1, 100), renderer_of(2, 200)], &both), "one without is enough");
        assert!(!every_renderer_reached(&[renderer_of(3, 300)], &both), "a renderer under no engine reached");
        // A helper of the engine without pages hosts no page, so it does not hold the warning.
        let gpu = UncoveredChild {
            pid: 4,
            parent_pid: 200,
            image: Some("engine.exe".into()),
            command_line: Some("engine.exe --type=gpu-process".into()),
        };
        assert!(every_renderer_reached(&[renderer_of(1, 100), gpu], &both));

        // Through the reconcile, as the session runs it: the strong warning stays while one renderer's
        // engine had no page, and gives way when every renderer's engine had some.
        let children = [renderer_of(1, 100), renderer_of(2, 200)];
        let mut kept = uncovered_children_warnings(&children, 2);
        reconcile_engine_warnings(&mut kept, every_renderer_reached(&children, &both));
        assert_eq!(kept, vec!["inheritance.children_uncovered", "embedded.web_engine_uncovered"]);
        let mut softened = uncovered_children_warnings(&children[..1], 1);
        reconcile_engine_warnings(&mut softened, every_renderer_reached(&children[..1], &both));
        assert_eq!(softened, vec!["inheritance.children_uncovered", "embedded.web_engine_reached"]);
    }

    /// The reason key names processes when children were uncovered, in the verdict the fold makes
    /// unreachable otherwise: a family with such a child is partial or fails, never works.
    #[test]
    fn the_family_reason_names_the_uncovered_children() {
        assert_eq!(session_reason_key(Verdict::Works, false, false), "session.family_covered");
        assert_eq!(session_reason_key(Verdict::Partial, true, false), "session.family_partial_children");
        assert_eq!(session_reason_key(Verdict::Fails, true, false), "session.family_uncovered_children");
        assert_eq!(Verdict::Works.combine(Verdict::Fails), Verdict::Partial);
        assert_eq!(Verdict::Undetermined.combine(Verdict::Fails), Verdict::Fails);
    }

    /// ADR-4, 2026-11-11. A hand-off to a copy outside the session is the reason whatever else the family
    /// did, because it is why the session may have tested nothing - uncovered children included.
    #[test]
    fn a_hand_off_to_a_copy_is_the_reason_before_every_other() {
        for verdict in [Verdict::Partial, Verdict::Fails] {
            for children in [false, true] {
                assert_eq!(session_reason_key(verdict, children, true), "session.family_handed_to_copy");
            }
        }
    }

    /// Copies found are named with the image through the sieve, under the warning that says what to do.
    /// A search that could not read the process list names nothing and says so under a key of its own, and
    /// a search that found nothing says nothing.
    #[test]
    fn the_hand_off_findings_name_the_copies_or_say_the_search_failed() {
        let copies = vec![
            RunningCopy { pid: 4600, image: "app.exe".into() },
            RunningCopy { pid: 4700, image: "app\u{1b}[2J.exe".into() },
        ];
        let (named, key) = hand_off_findings(copies, false);
        assert_eq!(named.iter().map(|c| c.pid).collect::<Vec<_>>(), vec![4600, 4700]);
        assert!(named.iter().all(|c| c.image.as_deref().is_some_and(|i| !i.contains('\u{1b}'))), "{named:?}");
        assert_eq!(key, Some("session.handed_to_copy"));
        assert_eq!(hand_off_findings(Vec::new(), true), (Vec::new(), Some("session.copy_search_failed")));
        assert_eq!(hand_off_findings(Vec::new(), false), (Vec::new(), None));
    }

    /// The ledger at the family's end: a window seen asks for no copies at all, copies found are kept, and
    /// a process list that could not be read is kept for the report rather than lost in the log.
    #[test]
    fn the_ledger_looks_for_copies_only_when_no_window_was_seen() {
        let copy = || vec![RunningCopy { pid: 4600, image: "app.exe".into() }];

        let mut seen = SessionLedger::new(Verdict::Works);
        seen.window_seen = true;
        seen.family_ended(|_| panic!("a family that showed a window was searched for copies"));
        assert!(seen.copies_outside.is_empty() && !seen.copy_search_failed);

        let mut found = SessionLedger::new(Verdict::Works);
        found.uncovered_children.push(child(4300, Some("helper.exe"), None));
        found.family_ended(|unhooked| {
            assert_eq!(unhooked.iter().map(|c| c.pid).collect::<Vec<_>>(), vec![4300], "the children were not passed on");
            Ok(copy())
        });
        assert_eq!(found.copies_outside, copy());
        assert!(!found.copy_search_failed);

        let mut unread = SessionLedger::new(Verdict::Works);
        unread.family_ended(|_| Err("snapshot refused".into()));
        assert!(unread.copies_outside.is_empty());
        assert!(unread.copy_search_failed, "an unread process list was told to the log alone");
    }

    /// A target that vanished leaving a program it started running, one the hook could not enter,
    /// handed off to it - measured with a 64-bit launcher starting a 32-bit program, which the report
    /// called a single-instance application (ADR-16). Anything else is still the ADR-4 suspicion.
    #[test]
    fn a_vanish_that_left_an_uncovered_program_running_is_a_hand_off() {
        let child = |pid, parent_pid| UncoveredChild { pid, parent_pid, image: None, command_line: None };
        assert_eq!(vanish_reason(10, &[child(11, 10)], |_| true, false, false), Some(KEY_HANDED_OFF_UNCOVERED));
        // Ended already, started by another process of the family, or nothing started at all.
        let suspected = Some("target.single_instance_suspected");
        assert_eq!(vanish_reason(10, &[child(11, 10)], |_| false, false, false), suspected);
        assert_eq!(vanish_reason(10, &[child(12, 11)], |_| true, false, false), suspected);
        assert_eq!(vanish_reason(10, &[], |_| true, false, false), suspected);
    }

    /// A windowed target that crashed inside the guard window was reported as a suspected single-instance
    /// application, whose remedy - close the copy already running - does nothing for a crash. A program it
    /// started and left running still makes it a hand-off, and a console family still did not vanish.
    #[test]
    fn a_target_that_crashed_inside_the_guard_window_is_said_to_have_crashed() {
        let child = |pid, parent_pid| UncoveredChild { pid, parent_pid, image: None, command_line: None };
        assert_eq!(vanish_reason(10, &[], |_| true, false, true), Some(KEY_TARGET_CRASHED));
        assert_eq!(vanish_reason(10, &[child(11, 10)], |_| false, false, true), Some(KEY_TARGET_CRASHED));
        assert_eq!(vanish_reason(10, &[child(11, 10)], |_| true, false, true), Some(KEY_HANDED_OFF_UNCOVERED));
        assert_eq!(vanish_reason(10, &[], |_| true, true, true), None);
    }

    /// `ended` after a vanish carries the code the target ended with, and nothing else of it changes.
    #[test]
    fn the_end_of_a_vanish_carries_the_targets_exit_code() {
        let ended = with_target_exit(crate::events::ended_after_launch(vec![]), Some(0xC000_0005u32 as i32));
        let Event::Ended { target_exit_code, clean, elapsed_real_ms, .. } = ended else { panic!("not ended: {ended:?}") };
        assert_eq!(target_exit_code, Some(0xC000_0005u32 as i32));
        assert!(clean);
        assert_eq!(elapsed_real_ms, 0);
    }

    /// ADR-4, owner decision 2026-10-27. A family of console programs that ended inside the guard window
    /// is not a vanish - measured on four console tools that wrote the session's date and were told the
    /// session never took effect. A hand-off to an uncovered program still is, whatever the family was,
    /// because that program runs on the real clock.
    #[test]
    fn a_family_of_console_programs_that_ended_quickly_did_not_vanish() {
        let child = |pid, parent_pid| UncoveredChild { pid, parent_pid, image: None, command_line: None };
        assert_eq!(vanish_reason(10, &[], |_| true, true, false), None);
        assert_eq!(vanish_reason(10, &[child(11, 10)], |_| false, true, false), None, "its uncovered child ended too");
        assert_eq!(vanish_reason(10, &[child(11, 10)], |_| true, true, false), Some(KEY_HANDED_OFF_UNCOVERED));
    }
}
