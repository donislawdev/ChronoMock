//! `chrono run`: the friendly driver, and a first-class interface rather than a wrapper.
//!
//! It spawns the core as a child process and speaks the machine protocol to it over stdio (ADR-6),
//! which is the same boundary the GUI uses - so the two interfaces cannot drift into different
//! behaviour, and neither of them can reach past the protocol into the mechanism.
//!
//! Everything the user types is parsed here and nowhere else. The core receives an absolute moment,
//! never `--at +30d`: one grammar resolves relative moments, the same one the calculator uses.


/// Reading the command line: `RunArgs` and every flag that fills it.
mod args;
/// Gathering what the session said, event by event.
mod collect;
/// `--report`: where the evidence file may go, and writing it whole.
mod evidence_file;
/// Deciding what time the session runs with, before anything is spawned.
mod moment;
/// `--dry-run`: describing the session instead of running it.
mod plan;

use std::io::{BufReader, IsTerminal, Write};
use std::process::{Command as PCommand, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono_proto::{Command, Event, MomentKind, MomentSpec, TimeSpec, PROTOCOL_VERSION};

use args::{elevation_problem, parse_run_args, target_spec_for, RunArgs};
use collect::Collector;
use moment::resolve_time_spec;
use crate::cdp;
use crate::cli::print_usage;
use crate::output::{diag, out, outln, Stream};
use crate::report::{arguments_line, mode_label, render_evidence, render_report, CutShort, EvidenceParams, RunFacts};
use crate::wire::{read_line_bytes, LineRead};
use crate::zone::{format_bias, now_filetime_utc, session_zone_default};
/// How long `chrono run` waits for ANY event from the core before calling it hung.
///
/// Deliberately the same 15 s the GUI uses (`SessionViewModel.IdleTimeout`), because it is the same
/// core with the same internal deadlines behind it - and those deadlines are checked against this
/// number by `RustTimeoutMirrorTests`. The core beats `state` about once a real second in every
/// mode, so 15 s is fifteen missed heartbeats.
pub(crate) const DRIVER_IDLE_TIMEOUT_SECS: u64 = 15;



/// Send an `end` command to the core over its stdin.
pub(crate) fn send_end(stdin: &mut std::process::ChildStdin) {
    send_command(stdin, &Command::End { v: PROTOCOL_VERSION, id: 2 });
}

/// Send a `set_multiplier` command in flight.
pub(crate) fn send_set_multiplier(stdin: &mut std::process::ChildStdin, m: i64) {
    send_command(stdin, &Command::SetMultiplier { v: PROTOCOL_VERSION, id: 3, multiplier: m });
}

/// Send a `jump` command in flight. A leading +/- marks a relative jump (current fake + one step),
/// carried in `delta` - anything else is an absolute moment in the session zone, carried in `local`.
pub(crate) fn send_jump(stdin: &mut std::process::ChildStdin, moment: &str, tz_bias_min: Option<i32>) {
    let first = moment.as_bytes().first().copied();
    let to = if first == Some(b'+') || first == Some(b'-') {
        MomentSpec { kind: MomentKind::Relative, local: None, tz_bias_min, delta: Some(moment.to_string()) }
    } else {
        MomentSpec { kind: MomentKind::Absolute, local: Some(moment.to_string()), tz_bias_min, delta: None }
    };
    send_command(stdin, &Command::Jump { v: PROTOCOL_VERSION, id: 4, to });
}

/// One command as one write: `writeln!` on the unbuffered child stdin wrote the line and its newline
/// as two, and a reader sharing the pipe took the first without the second (measured, R4-W1).
pub(crate) fn send_command(stdin: &mut std::process::ChildStdin, cmd: &Command) {
    if let Ok(mut line) = serde_json::to_string(cmd) {
        line.push('\n');
        let _ = stdin.write_all(line.as_bytes());
        let _ = stdin.flush();
    }
}

/// What the evidence file says beside the report: the time the session ran at, and the run itself -
/// which build, when, what, where and with which switches (8.8, R5 D4-5).
fn evidence_params(ra: &RunArgs, spec: &TimeSpec, now_bias: i32, started_utc: String) -> EvidenceParams {
    EvidenceParams {
        moment: spec.moment.local.clone().unwrap_or_else(|| "(default)".into()),
        // The zone the session ACTUALLY ran in, not the flag. This line used to read
        // "(host default)" for a session running on UTC - a false statement in the file a tester
        // is meant to cite as proof (untouchable rule 4).
        zone: match ra.zone_bias_min {
            Some(b) => format_bias(b),
            None => format!("{} (host default)", format_bias(now_bias)),
        },
        mode: mode_label(spec.mode, spec.multiplier),
        run: RunFacts {
            build: format!("{}, {}-bit core", env!("CARGO_PKG_VERSION"), usize::BITS),
            started_utc,
            target: ra.target.clone(),
            folder: match &ra.cwd {
                Some(dir) => dir.clone(),
                // The target inherits this command's directory, so that is where it ran.
                None => match std::env::current_dir() {
                    Ok(dir) => format!("{} (inherited from this command)", dir.display()),
                    Err(_) => "(inherited from this command, which could not name it)".to_string(),
                },
            },
            arguments: arguments_line(&ra.args, ra.report_args),
            flags: evidence_flags(ra),
        },
    }
}

/// The switches a run was given beyond its moment, zone and mode, which the evidence file names on a
/// line of their own, in one stable order, or "none" (R5, D4-5). Rebuilt from what was parsed rather
/// than copied from the command line, so the target's arguments never ride along with them.
fn evidence_flags(ra: &RunArgs) -> String {
    let mut out: Vec<String> = Vec::new();
    if let Some(id) = &ra.preset {
        out.push(format!("--preset {id}"));
    }
    let mut params: Vec<(&String, &String)> = ra.params.iter().collect();
    params.sort();
    out.extend(params.into_iter().map(|(id, value)| format!("--param {id}={value}")));
    let switches = [
        (ra.scale_duration, "--scale-duration"),
        (ra.scale_qpc, "--scale-qpc"),
        (ra.force, "--force"),
        (!ra.embedded, "--no-embedded"),
        (ra.elevated_embedded, "--elevated-embedded"),
        (ra.keep_background_timers, "--keep-background-timers"),
        (ra.json, "--json"),
        (ra.report_args, "--report-args"),
    ];
    out.extend(switches.into_iter().filter(|(on, _)| *on).map(|(_, flag)| flag.to_string()));
    if ra.ticks > 0 {
        out.push(format!("--ticks {}", ra.ticks));
    }
    if let Some(secs) = ra.timeout_secs {
        out.push(format!("--timeout {secs}"));
    }
    if let Some((tick, m)) = ra.set_after {
        out.push(format!("--set-after {tick}:{m}"));
    }
    if let Some((tick, moment)) = &ra.jump_after {
        out.push(format!("--jump-after {tick}:{moment}"));
    }
    if out.is_empty() { "none".to_string() } else { out.join(" ") }
}

pub(crate) fn driver_run(argv: &[String]) -> i32 {
    let ra = match parse_run_args(argv) {
        Ok(ra) => ra,
        Err(e) => {
            diag!("chrono: {e}");
            print_usage(Stream::Err);
            return 1;
        }
    };
    // A run whose evidence has nowhere to go is refused before it starts, not after the session has
    // run and cannot be run again (R5-B9, item 1).
    let report_problem = || ra.report.as_deref().and_then(|path| evidence_file::path_problem(path, &ra.target));
    if let Some(problem) = elevation_problem(&ra, chrono_mech::process_elevated(std::process::id()))
        .map(str::to_string)
        .or_else(report_problem)
    {
        diag!("chrono: {problem}");
        return 1;
    }

    // The session zone when the caller named none: the HOST's. Reading "now" as UTC instead hands the
    // target a local time off by the host's own offset - the failure untouchable rule 2 names. This used
    // to be computed only in the no-preset arm and only for the no-`--at` case, so a relative `--at` and
    // a preset both fell back to UTC and disagreed with the plain `chrono run app.exe` beside them
    // (R2-S7). One value, computed once, used by every path that derives a moment from "now".
    let now_bias = session_zone_default(ra.zone_bias_min);

    // The moment AND the time mode come either from a named preset (docs/04 4.3) or from the flags,
    // and both arms end in the one `TimeSpec` that goes on the wire. Reading the report's own
    // description off that same value is what stops it drifting from what was sent.
    let resolved = match resolve_time_spec(&ra, now_bias) {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };
    let spec = resolved.spec;

    // Everything above this line decided what the session WOULD be, and nothing has been started or
    // written yet. A dry run stops exactly here and says so (docs/08 section 9d).
    if ra.dry_run {
        return plan::dry_run(&ra, &spec, &resolved.origin, now_bias);
    }


    // A Chromium/Electron target is auto-detected by `__core` itself (ADR-9): the core takes the CDP
    // mechanism there and speaks this same protocol, so this driver streams and renders it exactly
    // like a native session. The only CDP-specific thing left here is labelling the report's coverage
    // unit as a JS "context" instead of a "pid" (the `cdp` flag on the report below).

    let exe = match crate::own_path::own_exe() {
        Ok(p) => p,
        Err(e) => {
            diag!("chrono: cannot locate own executable: {e}");
            return 3;
        }
    };

    let started_utc = format!("{}Z", chrono_core::filetime_utc_to_wall(now_filetime_utc(), 0));
    let mut child = match PCommand::new(exe)
        .arg("__core")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            diag!("chrono: cannot start core process: {e}");
            return 3;
        }
    };

    // Send `start` and keep stdin open so we can send `end` when we are done.
    let start = Command::Start {
        v: PROTOCOL_VERSION,
        id: 1,
        target: target_spec_for(&ra),
        time: spec.clone(),
        force: ra.force,
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    {
        let mut line = serde_json::to_string(&start).expect("serialize start");
        line.push('\n');
        if stdin.write_all(line.as_bytes()).is_err() {
            diag!("chrono: core closed its input before start");
            let _ = child.wait();
            return 3;
        }
        let _ = stdin.flush();
    }

    // Stream events. Send `end` after `--ticks` state heartbeats. With ticks 0 nothing is sent and the
    // session ends by itself (ADR-16). Either way, read through to `ended`.
    let Streamed { collected, timed_out, skipped, first_skipped } = stream_session(&mut child, &mut stdin, &ra, now_bias);
    drop(stdin);
    if let Some(first) = first_skipped {
        // Nothing but the core writes on that stream (R4-W1), so a line that is not an event is a
        // fault somewhere, and one skipped in silence would be the fault hidden (rule 6).
        for line in skipped_notice(skipped, &first) {
            diag!("{line}");
        }
    }

    let status = child.wait();
    // A core that stopped before `ended` left the session open, and nothing it sent before that is the
    // session's result (R4-W3). A run the driver cut short itself is said as that instead.
    let core_lost = timed_out.is_none() && collected.core_left_session_open();

    let report = collected.into_report(
        ra.target.clone(),
        // The core auto-detects a Chromium target and runs it over CDP - label the report's coverage
        // unit accordingly. Same pure function, same path string the core sees, so the two never drift.
        cdp::is_chromium_target(&ra.target),
        // Whether the run was cut short, and by what. It has to travel INTO the report rather than be
        // printed beside it, because the report is also written to the evidence file, and that file is
        // the one a tester cites (untouchable rule 4).
        timed_out.map(DriverLimit::cut_short).or(core_lost.then_some(CutShort::CoreLost)),
    );
    let evidence_written = match &ra.report {
        None => true,
        Some(path) => {
            let params = evidence_params(&ra, &spec, now_bias, started_utc);
            let written = evidence_file::write(path, &render_evidence(&report, &params));
            match &written {
                Ok(()) => diag!("chrono: evidence written to {path}"),
                Err(e) => diag!("chrono: {e}"),
            }
            written.is_ok()
        }
    };
    if !ra.json {
        out!("{}", render_report(&report));
    }

    // Whether a caller reading this command's stderr to its end will wait for the target instead of
    // for this command (R4/5 review round): the target was given a copy of that stream, and nothing
    // waits for the end of a terminal.
    let caller_waits_on_target = target_gets_our_stderr(
        crate::pe::is_windowed_program(std::path::Path::new(&ra.target)),
        cdp::is_chromium_target(&ra.target),
    ) && !std::io::stderr().is_terminal();

    // A session that timed out has no verdict to report, and must not borrow one: the core was
    // killed mid-flight, so its exit code says how it died, not what it found (untouchable rule 4).
    if let Some(which) = timed_out {
        return report_cut_short(which, ra.timeout_secs, caller_waits_on_target);
    }
    if core_lost {
        return report_core_lost(status.as_ref().ok().and_then(|s| s.code()), caller_waits_on_target);
    }
    if caller_waits_on_target && ended_with_the_target_running(report.target_exit, &report.warnings) {
        diag!("{STDERR_HELD_BY_TARGET}");
    }

    // The tool's exit code is the session verdict, carried by the core's exit code
    // (docs/08 section 8).
    let core_code = status.ok().and_then(|s| s.code());
    let code = driver_exit_code(core_code);
    if code == 3 {
        diag!("{}", no_verdict_notice(core_code));
    }
    let with_evidence = exit_code_with_evidence(code, evidence_written);
    if with_evidence != code {
        diag!("{}", evidence_lost_notice(code, ra.report.as_deref().unwrap_or_default()));
    }
    with_evidence
}

/// The exit code of a run whose session reached a verdict that the evidence file of `--report` does
/// not hold (R5-D28). The driver's own, never the core's.
const EVIDENCE_NOT_WRITTEN_EXIT: i32 = 8;

/// The exit code once the evidence file is accounted for (R5-D28, review round on PR #104).
///
/// A verdict whose evidence did not reach the path `--report` named becomes 8. The verdict alone said
/// 0 over a folder that held no evidence, or an older file: when the old file is held open the rename
/// fails and the old file stays, so a pipeline keeping that file as the proof of this run kept the proof
/// of another. The verdict is still in the report on standard output, and in the scratch file the line
/// before names when the write got that far. A code that is no verdict (1, 2, 3, 5, 6, 7) stays - it
/// already says the run proves nothing, and says why.
fn exit_code_with_evidence(code: i32, evidence_written: bool) -> i32 {
    match code {
        0 | 4 | 10 | 11 | 12 if !evidence_written => EVIDENCE_NOT_WRITTEN_EXIT,
        _ => code,
    }
}

/// What the driver says when a verdict gives way to [`EVIDENCE_NOT_WRITTEN_EXIT`].
fn evidence_lost_notice(verdict_code: i32, path: &str) -> String {
    format!(
        "chrono: exit code {EVIDENCE_NOT_WRITTEN_EXIT}, not the verdict's {verdict_code} - {path} does not hold this session's evidence, the line above says why"
    )
}

/// What the driver says when the session closed but the core's exit code is no verdict. The core's own
/// code is named whenever it is not the 3 it maps to (R5-D4, item 3): a panic (101), an access violation
/// or a kill came out as a bare 3, and that one number was all a pipeline kept of why. `core_lost_notice`
/// has named it since R4/12a, this path did not.
fn no_verdict_notice(core_code: Option<i32>) -> String {
    let base = "chrono: the core ended without a verdict";
    let tail = "this run proves nothing about the target";
    match core_code {
        Some(3) => format!("{base} - {tail}"),
        Some(c) => format!("{base} (exit code {}) - {tail}", crate::report::exit_code_label(c)),
        None => format!("{base} (exit code unknown) - {tail}"),
    }
}

/// What reading the session produced: the events gathered, which limit cut the read short when one
/// did, how many lines on the core's output were not events, and what the first of those looked like.
struct Streamed {
    collected: Collector,
    timed_out: Option<DriverLimit>,
    skipped: u64,
    first_skipped: Option<String>,
}

impl Streamed {
    /// Count one skipped line, keeping what the first one looked like. The first is the one closest to
    /// whatever started writing there, and a count alone gives a tester nothing to report.
    fn skip(&mut self, what: String) {
        self.skipped += 1;
        if self.first_skipped.is_none() {
            self.first_skipped = Some(what);
        }
    }
}

/// One line of the core's output as the read loop takes it: the line without its ending, and what it
/// is - `None` for an empty line, the event, or a line that is not one, described for the notice.
fn read_event(raw: &str) -> (&str, Option<Result<Event, String>>) {
    let line = raw.strip_suffix('\n').unwrap_or(raw);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.is_empty() {
        return (line, None);
    }
    (line, Some(chrono_proto::parse_event(line).map_err(|_| skipped_sample(line))))
}

/// How many characters of a skipped line the driver shows. The window keeps the same start of such a
/// line in its diagnostics block (`CoreClient.NoiseSampleChars`), because the start is what says what
/// wrote it, and a line can run to `MAX_PROTOCOL_LINE`.
const SKIPPED_SAMPLE_CHARS: usize = 200;

/// The start of a skipped line as the driver prints it: without its line ending, cut at
/// `SKIPPED_SAMPLE_CHARS` characters, and escaped, so a control character in it reaches the terminal as
/// text rather than as an instruction to the terminal.
fn skipped_sample(text: &str) -> String {
    let text = text.trim_end_matches(['\r', '\n']);
    let cut: String = text.chars().take(SKIPPED_SAMPLE_CHARS).collect();
    let more = if cut.len() < text.len() { "..." } else { "" };
    format!("{cut:?}{more}")
}

/// What the driver says about the lines it skipped, in two lines: what they cost the report, and what
/// to report. The count alone said neither (R4/5 review round).
fn skipped_notice(skipped: u64, first: &str) -> [String; 2] {
    let (what, pronoun, which) = if skipped == 1 {
        ("a line on the core's output that was not a protocol event".to_string(), "it", "The line")
    } else {
        (format!("{skipped} lines on the core's output that were not protocol events"), "they", "The first of them")
    };
    [
        format!("chrono: skipped {what}, so the report may be missing what {pronoun} said"),
        format!(
            "chrono: only the core writes there, so this is a fault in Chrono Mock rather than in the application. {which}: {first}"
        ),
    ]
}

/// Read the core's events until `ended`, the end of the stream, or a limit, acting on each heartbeat
/// as it comes.
///
/// Lifted out of `driver_run` whole, so the pinned length and complexity ceilings (clippy.toml) stay
/// with the code that set them rather than with the function that happened to hold it.
fn stream_session(
    child: &mut std::process::Child,
    stdin: &mut std::process::ChildStdin,
    ra: &RunArgs,
    now_bias: i32,
) -> Streamed {
    let mut streamed = Streamed { collected: Collector::default(), timed_out: None, skipped: 0, first_skipped: None };
    let mut beats = Heartbeats::default();
    // Why the read runs on its own thread rather than in this loop: the driver has to be able to
    // give up. Reading straight from the pipe here had no time limit and no liveness check, so a
    // core that stopped answering hung `chrono run` with nothing in the log to say why - on the
    // surface the README points at CI, where the only thing that eventually notices is the runner's
    // own job timeout (R3-5). The GUI has had an idle watchdog since M-10 - this is the same idea on
    // the other client, and the two now use the same 15 s.
    //
    // The second reason is measured, not assumed: EOF on the core's stdout did NOT arrive when the
    // core died, because the TARGET held the write end of that pipe open. The target no longer gets
    // it (R4-W1), but "read until EOF" would still be "read until whoever holds the pipe lets go",
    // which is no liveness signal for the core at all.
    let Some(stdout) = child.stdout.take() else {
        return streamed;
    };
    let line_rx = spawn_line_reader(stdout);
    let mut clock = ReadClock::new(ra.timeout_secs);
    loop {
        // Whether the CORE is still alive, asked before every wait - because a line arriving on
        // this pipe does not mean it is. Before R4-W1 the target wrote its own output here, and
        // measured with `ping` as the target the idle timer was reset for as long as it ran, so a
        // core killed 45 s earlier still looked alive (R3-5).
        //
        // Once the core is gone the only thing left to do is drain what it already said, so the
        // wait shrinks to one short window that a line does not renew: anything queued arrives at
        // once, and the loop then ends as a normal end-of-session, not a timeout.
        let core_gone = clock.note_core(matches!(child.try_wait(), Ok(Some(_))));
        let raw = match line_rx.recv_timeout(clock.budget()) {
            Ok(Incoming::Line(line)) => line,
            Ok(Incoming::Unreadable(what)) => {
                streamed.skip(what);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) if core_gone => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Which limit ran out decides only what we SAY. Both end the same way - the core
                // is killed and the exit code is 6 - because a driver that printed a verdict it
                // never received would be the tool inventing evidence (untouchable rule 4).
                streamed.timed_out = Some(clock.which_limit());
                let _ = child.kill();
                break;
            }
        };
        // Only an event goes on: to `--json`, which is a stream of events and nothing else, and to
        // the idle clock, which a line that says nothing must not keep alive.
        let (line, read) = read_event(&raw);
        let event = match read {
            None => continue,
            Some(Ok(event)) => event,
            Some(Err(what)) => {
                streamed.skip(what);
                continue;
            }
        };
        clock.event_seen();
        if ra.json {
            outln!("{line}");
        }
        match event {
            Event::State { .. } => beats.on_state(ra, stdin, now_bias),
            // Everything else is evidence rather than a cue to act, so the collector owns
            // it. Nothing happens on a verdict on purpose: with ticks == 0 the run stays
            // attached until the session ends by itself, when the target and everything it
            // started on the session clock have exited (ADR-16), because detaching early would
            // revert them to real time (self-detach). With ticks > 0 the state arm above ends the session
            // after that many heartbeats. `ended` is the one event that stops the read.
            event => {
                if streamed.collected.record(event) {
                    break;
                }
            }
        }
    }
    streamed
}

/// What the reading thread hands on: a line of text, or word that a line was skipped because it was
/// not text or ran past the cap, with what it looked like.
enum Incoming {
    Line(String),
    Unreadable(String),
}

/// How many lines the reading thread holds for the loop before it waits for the loop to take one.
///
/// Every other input channel in this tool is bounded, the window's copy of this one included
/// (`CoreClient.MaxQueuedEvents`), and this one was not: a loop held up behind a slow reader of `--json`
/// let the queue grow for as long as the session ran (R4/5 review round). The core writes about one
/// event a second, so the bound is over an hour of heartbeats. Waiting is the right full-behaviour for
/// the same reason as in the window: dropping a line could lose a verdict or an `ended`, while a core
/// kept waiting on its own output only pauses until the loop catches up.
const MAX_QUEUED_LINES: usize = 4096;

/// The thread that reads the core's stdout line by line and hands each line on, until the stream
/// ends or cannot be read. A line that is not UTF-8, or runs past the cap, is reported and skipped
/// rather than ending the read (R4-W1): one bad line used to cost the whole session its verdict.
///
/// Nobody joins this thread, on purpose. Its read ends at EOF, which comes only when every holder of
/// the pipe's write end has let go, and that is not the driver's to decide. Today it comes as the core
/// exits, because the Chromium mode's browser dies with the core's job and a native target never gets
/// the handle (R4-W1), but a join would put the driver's own exit behind whatever holds it next. The
/// loop gives up by dropping the receiver instead, and the thread then ends at the next line it tries
/// to hand on, or with the process.
fn spawn_line_reader<R: std::io::Read + Send + 'static>(stdout: R) -> mpsc::Receiver<Incoming> {
    let (line_tx, line_rx) = mpsc::sync_channel::<Incoming>(MAX_QUEUED_LINES);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut raw = Vec::new();
        loop {
            let incoming = match read_line_bytes(&mut reader, &mut raw) {
                Ok(LineRead::Eof) | Err(_) => break,
                Ok(LineRead::TooLong) => {
                    Incoming::Unreadable(format!("a line of {} bytes or more", crate::wire::MAX_PROTOCOL_LINE))
                }
                Ok(LineRead::Line) => match String::from_utf8(std::mem::take(&mut raw)) {
                    Ok(line) => Incoming::Line(line),
                    Err(e) => Incoming::Unreadable(format!(
                        "{} (not UTF-8)",
                        skipped_sample(&String::from_utf8_lossy(e.as_bytes()))
                    )),
                },
            };
            if line_tx.send(incoming).is_err() {
                break;
            }
        }
    });
    line_rx
}

/// How long `stream_session` has left to wait, from three limits: the idle limit counted from the
/// last EVENT, whatever is left of `--timeout`, and once the core is gone a short window to drain what
/// it already wrote. A ceiling is honoured to the second rather than to the end of the next idle
/// window, and neither a line that is not an event nor a line after the core's death buys more time.
struct ReadClock {
    started: Instant,
    last_event: Instant,
    core_gone_at: Option<Instant>,
    timeout: Option<Duration>,
}

/// The two limits the driver itself enforces on a read (R5-A9). Which one ran out decides only what is
/// said - both stop the core, and the run exits 6 with no verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriverLimit {
    /// `--timeout` passed.
    Timeout,
    /// No event for `DRIVER_IDLE_TIMEOUT_SECS`.
    Idle,
}

impl DriverLimit {
    /// The same limit as the report says it.
    fn cut_short(self) -> CutShort {
        match self {
            DriverLimit::Timeout => CutShort::Timeout,
            DriverLimit::Idle => CutShort::Idle,
        }
    }
}

/// How long the driver drains the core's output once the core is gone.
const DRAIN_AFTER_CORE: Duration = Duration::from_millis(200);

impl ReadClock {
    fn new(timeout_secs: Option<u64>) -> ReadClock {
        let now = Instant::now();
        ReadClock { started: now, last_event: now, core_gone_at: None, timeout: timeout_secs.map(Duration::from_secs) }
    }

    /// Record whether the core is gone - the first time it is seen gone starts the drain window -
    /// and say whether it is.
    fn note_core(&mut self, gone_now: bool) -> bool {
        if gone_now && self.core_gone_at.is_none() {
            self.core_gone_at = Some(Instant::now());
        }
        self.core_gone_at.is_some()
    }

    fn event_seen(&mut self) {
        self.last_event = Instant::now();
    }

    /// The nearest of the three limits, from now.
    fn budget(&self) -> Duration {
        let idle = Duration::from_secs(DRIVER_IDLE_TIMEOUT_SECS).saturating_sub(self.last_event.elapsed());
        let timeout = self.timeout.map_or(Duration::MAX, |t| t.saturating_sub(self.started.elapsed()));
        let drain = self.core_gone_at.map_or(Duration::MAX, |at| DRAIN_AFTER_CORE.saturating_sub(at.elapsed()));
        idle.min(timeout).min(drain)
    }

    /// Which limit a wait that ran out hit: `--timeout` when it has passed, the idle limit otherwise.
    fn which_limit(&self) -> DriverLimit {
        match self.timeout {
            Some(t) if self.started.elapsed() >= t => DriverLimit::Timeout,
            _ => DriverLimit::Idle,
        }
    }
}

/// The heartbeats seen so far, and what the driver sends on them: `--set-after`, `--jump-after` and
/// the `end` that `--ticks` asks for.
#[derive(Default)]
struct Heartbeats {
    seen: u64,
    end_sent: bool,
}

impl Heartbeats {
    fn on_state(&mut self, ra: &RunArgs, stdin: &mut std::process::ChildStdin, now_bias: i32) {
        self.seen += 1;
        if let Some((t, m)) = ra.set_after
            && self.seen == t {
                send_set_multiplier(stdin, m);
            }
        if let Some((t, ref mom)) = ra.jump_after
            && self.seen == t {
                // The SESSION's zone, not the raw flag: a jump names a wall-clock moment on
                // the clock the target is already showing. Reading it as UTC while the
                // session ran on the host's zone landed the jump an offset away - measured
                // at exactly that: `--jump-after 2:2038-01-19T03:14:07` reached 05:14 on a
                // UTC+2 host. That predates R2-S7 (it came in with "no --at follows the
                // host"), and it is the same rule-2 failure in a second place.
                send_jump(stdin, mom, Some(now_bias));
            }
        if ra.ticks > 0 && self.seen >= ra.ticks && !self.end_sent {
            send_end(stdin);
            self.end_sent = true;
        }
    }
}

/// What the driver says when it cut the run short, and the code it exits with.
///
/// Lifted out of `driver_run` so the pinned complexity ceiling stays where it is (clippy.toml), which
/// is the ceiling working rather than a nuisance.
fn report_cut_short(which: DriverLimit, timeout_secs: Option<u64>, caller_waits_on_target: bool) -> i32 {
    for line in cut_short_notice(which, timeout_secs, caller_waits_on_target) {
        diag!("{line}");
    }
    6
}

/// The lines `report_cut_short` prints. Which limit ran out decides only the first - both mean the
/// same thing, that there is no verdict to report.
fn cut_short_notice(which: DriverLimit, timeout_secs: Option<u64>, caller_waits_on_target: bool) -> Vec<String> {
    let mut lines = vec![match which {
        DriverLimit::Timeout => format!(
            "chrono: gave up after the --timeout of {}s - the core was stopped, so this run has no verdict",
            timeout_secs.unwrap_or(0)
        ),
        DriverLimit::Idle => format!(
            "chrono: the core sent no event for {DRIVER_IDLE_TIMEOUT_SECS}s and was stopped - this run has no verdict"
        ),
    }];
    lines.extend(target_outlives_core_notice(caller_waits_on_target));
    lines
}

/// What the driver says when the core stopped before it closed the session, and the code it exits
/// with: 3, whatever code the core itself died with (R4-W3).
///
/// The core's own code is the verdict only once the session is closed. Passed through here, it said
/// whatever the killer chose: `taskkill /F` ends a process with 1, so a killed core made `chrono run`
/// exit with the usage-error code and print nothing at all, measured - and a kill with 0 would have
/// read as WORKS to any pipeline. docs/08 section 8 has said "killed from outside is 3" since R3-5.
fn report_core_lost(core_code: Option<i32>, caller_waits_on_target: bool) -> i32 {
    for line in core_lost_notice(core_code, caller_waits_on_target) {
        diag!("{line}");
    }
    3
}

/// The lines `report_core_lost` prints. The core's own code is named, because it is what tells a
/// crash from a kill, and it is the one fact about the core's end this side has.
fn core_lost_notice(core_code: Option<i32>, caller_waits_on_target: bool) -> Vec<String> {
    let code = core_code.map_or_else(|| "unknown".to_string(), crate::report::exit_code_label);
    let mut lines = vec![format!(
        "chrono: the core stopped before it closed the session (exit code {code}), so this run proves nothing about the target"
    )];
    lines.extend(target_outlives_core_notice(caller_waits_on_target));
    lines
}

/// What the driver says about the application whenever the core ends before the session does, by the
/// driver's hand or not.
fn target_outlives_core_notice(caller_waits_on_target: bool) -> Vec<String> {
    // The target is NOT killed with the core, and that is the normal arrangement rather than an
    // oversight: a session ordinarily stays attached until the application exits, because detaching
    // early would hand it back the real clock. Stopping the core does not change that, and nothing
    // else tells the tester the app is still up - measured on a real run, where the application had to
    // be closed by hand afterwards. Killing someone else's application over a diagnostic ceiling is a
    // bigger decision than this line, so this says it instead of doing it (rule 6).
    //
    // Which clock it runs on is not said, because it is not one answer. This line used to say "on the
    // session clock", and since ADR-14 and ADR-18 that is false for the native half: with the core gone
    // the hook lets go and the app reads the real date again (measured in R4/6, a core killed two
    // seconds in). Only pages inside an embedded web engine stay on the session clock after a core that
    // died, and the driver cannot tell here whether there were any.
    let mut lines = vec![
        "chrono: the target was started by the core and does not exit with it - it may still be running, so close it yourself"
            .to_string(),
    ];
    // Whether it runs is not known here, the core that knew is gone - hence "if it is".
    if caller_waits_on_target {
        lines.push(
            "chrono: if it is, it still writes to this command's standard error, so whatever reads that stream to its end waits until the application exits"
                .to_string(),
        );
    }
    lines
}

/// Whether a target started the way this one was gets a copy of this command's standard error. A
/// console program on the shared console does (ADR-17), a program with a window and the Chromium
/// mode's browser get none.
fn target_gets_our_stderr(windowed: Option<bool>, chromium: bool) -> bool {
    windowed != Some(true) && !chromium
}

/// Whether the session ended with the target itself still running: processes were left running, and
/// the target is not one of those that exited, because `ended` names the target's exit code when it has
/// one. A process the target started may hold the stream too, but whether it was given one is not
/// known here, so only the target is spoken for.
fn ended_with_the_target_running(target_exit: Option<i32>, warnings: &[String]) -> bool {
    target_exit.is_none() && warnings.iter().any(|w| w == "session.left_running")
}

/// Said when the session ended with the target still running on a copy of this command's standard
/// error, which is not a terminal. The command exits with the session, and a script or a CI step
/// reading that stream to its end then waits for the application instead - measured, the command out
/// at 4 s and the end of the stream at 13.5 s (R4/5 review round). Unexplained, that wait looks like
/// the tool hanging. It was the same before R4/5, when the application's lines went into the protocol
/// and were lost.
const STDERR_HELD_BY_TARGET: &str = "chrono: the application is still running and writes to this command's standard error, so whatever reads that stream to its end waits until the application exits";

/// Map the core's exit code to the tool's.
///
/// Normally it passes straight through: the core's exit code IS the session verdict (docs/08
/// section 8). What this adds is the case where it is not a verdict at all - a core killed from
/// outside comes back as the operating system's status, which reached the caller unchanged and
/// unexplained (measured: killing the core mid-session made `chrono run` exit -1). A number no
/// table describes is worse than an error, because a pipeline branches on it, so it is reported as
/// the internal-error code with a line saying what happened (rules 4 and 6).
pub(crate) fn driver_exit_code(core_code: Option<i32>) -> i32 {
    match core_code {
        Some(c) if matches!(c, 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 10 | 11 | 12) => c,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flags line of the evidence file names every switch the run was given, in one order, and
    /// never the target's arguments, which have a line of their own (R5, D4-5).
    #[test]
    fn the_evidence_flags_name_the_switches_and_never_the_target_arguments() {
        let parsed = |s: &[&str]| parse_run_args(&s.iter().map(|a| a.to_string()).collect::<Vec<_>>()).unwrap();
        assert_eq!(evidence_flags(&parsed(&["app.exe"])), "none");
        let busy = parsed(&[
            "app.exe", "--args", "--token s3cret", "--scale-duration", "--ticks", "10", "--no-embedded", "--report",
            "e.txt", "--report-args", "--timeout", "30", "--set-after", "3:60", "--json",
        ]);
        assert_eq!(
            evidence_flags(&busy),
            "--scale-duration --no-embedded --json --report-args --ticks 10 --timeout 30 --set-after 3:60"
        );
        let preset = parsed(&["app.exe", "--preset", "month-end", "--param", "b=2", "--param", "a=1"]);
        assert_eq!(evidence_flags(&preset), "--preset month-end --param a=1 --param b=2", "parameters in a stable order");
    }

    /// The driver's reader goes on past a line it cannot use (R4-W1). It used to stop at the first
    /// byte that was not UTF-8 - the whole rest of the session, verdict included, was then lost.
    #[test]
    fn the_reader_skips_a_bad_line_and_hands_on_the_next() {
        let mut input = b"Odpowied\xAB z 127.0.0.1\r\n{\"type\":\"state\"}\n".to_vec();
        input.extend(vec![b'x'; crate::wire::MAX_PROTOCOL_LINE + 1]);
        input.extend_from_slice(b"\n{\"type\":\"ended\"}\n");
        let rx = spawn_line_reader(std::io::Cursor::new(input));
        let got: Vec<Result<String, String>> = rx
            .iter()
            .map(|incoming| match incoming {
                Incoming::Line(line) => Ok(line),
                Incoming::Unreadable(what) => Err(what),
            })
            .collect();
        assert_eq!(got.len(), 4, "{got:?}");
        assert_eq!(got[1], Ok("{\"type\":\"state\"}\n".to_string()));
        assert_eq!(got[3], Ok("{\"type\":\"ended\"}\n".to_string()));

        // What a skipped line looked like travels with it, because the driver shows the first one: the
        // text read leniently when it was not UTF-8, and only its length when it ran past the cap.
        let not_text = got[0].as_ref().expect_err("the CP852 line is skipped");
        assert!(not_text.contains(char::REPLACEMENT_CHARACTER) && not_text.contains(" z 127.0.0.1"), "{not_text}");
        assert!(not_text.ends_with("\" (not UTF-8)"), "the line ending is not part of the sample: {not_text}");
        let too_long = got[2].as_ref().expect_err("the long line is skipped");
        assert_eq!(too_long, &format!("a line of {} bytes or more", crate::wire::MAX_PROTOCOL_LINE));
    }

    /// A skipped line is printed as its start, escaped (R4/5 review round). The line came from a stream
    /// that has stopped making sense, so a control character in it must reach the terminal as text.
    #[test]
    fn a_skipped_line_is_shown_cut_and_escaped() {
        let escape = char::from(27u8);
        let shown = skipped_sample(&format!("{escape}[2J wiped\r\n"));
        assert!(!shown.contains(escape), "a control character reached the output: {shown}");
        assert_eq!(shown, format!("{:?}", format!("{escape}[2J wiped")));

        let long = "\u{17C}".repeat(SKIPPED_SAMPLE_CHARS + 50);
        let cut = skipped_sample(&long);
        assert_eq!(cut, format!("{:?}...", "\u{17C}".repeat(SKIPPED_SAMPLE_CHARS)), "cut at characters, not bytes");
        assert_eq!(skipped_sample("short"), "\"short\"", "a line under the limit is not marked as cut");
    }

    /// A line that does not parse is skipped with its start kept, the FIRST one is what the notice
    /// shows, and every one is counted (R4/5 review round).
    #[test]
    fn a_line_that_is_not_an_event_is_counted_and_the_first_one_kept() {
        let event = r#"{"type":"session_verdict","v":1,"verdict":"works","reason_key":"session.family_covered","process_count":1}"#;
        let raw = format!("{event}\r\n");
        let (line, read) = read_event(&raw);
        assert_eq!(line, event, "the line ending is not part of the line --json passes on");
        assert!(matches!(read, Some(Ok(Event::SessionVerdict { .. }))));
        assert!(read_event("\r\n").1.is_none(), "an empty line is neither an event nor skipped");

        let mut streamed = Streamed { collected: Collector::default(), timed_out: None, skipped: 0, first_skipped: None };
        for raw in ["Reply from 127.0.0.1\r\n", "Progress 40%\n"] {
            match read_event(raw).1 {
                Some(Err(what)) => streamed.skip(what),
                other => panic!("{raw:?} read as {other:?}"),
            }
        }
        assert_eq!(streamed.skipped, 2);
        assert_eq!(streamed.first_skipped.as_deref(), Some("\"Reply from 127.0.0.1\""));
    }

    /// The notice says what a skipped line means for the report and gives the tester something to
    /// report. It used to be a count and nothing else (R4/5 review round).
    #[test]
    fn the_notice_about_skipped_lines_says_what_they_cost_and_shows_the_first() {
        let [one_cost, one_what] = skipped_notice(1, "\"x\"");
        assert!(one_cost.contains("skipped a line on the core's output that was not a protocol event"), "{one_cost}");
        assert!(one_cost.ends_with("the report may be missing what it said"), "{one_cost}");
        assert!(one_what.contains("a fault in Chrono Mock rather than in the application"), "{one_what}");
        assert!(one_what.ends_with("The line: \"x\""), "{one_what}");

        let [many_cost, many_what] = skipped_notice(3, "\"x\"");
        assert!(many_cost.contains("skipped 3 lines on the core's output that were not protocol events"), "{many_cost}");
        assert!(many_cost.ends_with("what they said"), "{many_cost}");
        assert!(many_what.ends_with("The first of them: \"x\""), "{many_what}");
    }

    /// The reading thread stops taking lines from the core once the loop has `MAX_QUEUED_LINES`
    /// waiting (R4/5 review round). Unbounded, a loop held up behind a slow reader of `--json` let the
    /// queue grow for as long as the session ran.
    #[test]
    fn the_reader_waits_when_the_loop_falls_behind() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        /// A core that has far more to say than the bound, counting what the reader took.
        struct Talkative {
            left: usize,
            taken: Arc<AtomicUsize>,
        }
        impl std::io::Read for Talkative {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let lines = (buf.len() / 3).min(self.left);
                for slot in buf[..lines * 3].chunks_mut(3) {
                    slot.copy_from_slice(b"{}\n");
                }
                self.left -= lines;
                self.taken.fetch_add(lines, Ordering::SeqCst);
                Ok(lines * 3)
            }
        }

        let total = MAX_QUEUED_LINES * 100;
        let taken = Arc::new(AtomicUsize::new(0));
        let rx = spawn_line_reader(Talkative { left: total, taken: Arc::clone(&taken) });
        // Nothing is received here, so a bounded reader settles at its bound and an unbounded one at
        // everything. Settled means three looks in a row with no change, after the reader has started.
        let (mut last, mut same) = (0, 0);
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(50));
            let now = taken.load(Ordering::SeqCst);
            same = if now == last && now > 0 { same + 1 } else { 0 };
            last = now;
            if same == 3 {
                break;
            }
        }
        assert_eq!(same, 3, "the reader never settled, at {last} lines taken");
        assert!(last >= MAX_QUEUED_LINES, "the reader stopped before the queue was full: {last}");
        assert!(last < total / 2, "the reader took {last} of {total} lines with nobody receiving them");
        drop(rx);
    }

    /// The idle limit counts from the last EVENT and the drain window from the core's death, so
    /// neither a stream of lines that are not events nor a line after the core died buys more time -
    /// before R4-W1 each did, and `--timeout 5` waited 20 s for a target writing ten lines a second.
    #[test]
    fn the_read_budget_is_not_renewed_by_lines_that_say_nothing() {
        let now = Instant::now();
        let idle = Duration::from_secs(DRIVER_IDLE_TIMEOUT_SECS);
        let fresh = ReadClock::new(None);
        assert!(fresh.budget() <= idle && fresh.budget() > idle - Duration::from_secs(1));
        assert!(ReadClock::new(Some(5)).budget() <= Duration::from_secs(5));

        let quiet = ReadClock { started: now, last_event: now - (idle - Duration::from_millis(100)), core_gone_at: None, timeout: None };
        assert!(quiet.budget() <= Duration::from_millis(100), "the idle limit counts from the last event");

        let mut draining = ReadClock::new(Some(60));
        assert!(!draining.note_core(false));
        assert!(draining.note_core(true));
        let first = draining.core_gone_at;
        std::thread::sleep(Duration::from_millis(5));
        assert!(draining.note_core(true));
        assert_eq!(draining.core_gone_at, first, "the drain window starts once, at the first sight of the death");
        assert!(draining.budget() <= DRAIN_AFTER_CORE);
        draining.core_gone_at = Some(now - Duration::from_secs(1));
        draining.event_seen();
        assert_eq!(draining.budget(), Duration::ZERO, "an event after the window does not reopen it");

        let late = ReadClock { started: now - Duration::from_secs(6), last_event: now, core_gone_at: None, timeout: Some(Duration::from_secs(5)) };
        assert_eq!(late.budget(), Duration::ZERO);
        assert_eq!(late.which_limit(), DriverLimit::Timeout);
        assert_eq!(quiet.which_limit(), DriverLimit::Idle);
    }

    /// The run says the target holds its stderr only where it does (R4/5 review round): a target given
    /// a copy of it, and a session that ended with that target itself still running.
    #[test]
    fn only_a_target_given_our_stderr_and_left_running_holds_it() {
        assert!(target_gets_our_stderr(Some(false), false), "a console program on the shared console");
        assert!(target_gets_our_stderr(None, false), "a program of unknown kind goes on the shared console too");
        assert!(!target_gets_our_stderr(Some(true), false), "a program with a window gets no standard handles");
        assert!(!target_gets_our_stderr(Some(false), true), "the Chromium mode's browser gets none either");

        let left = vec!["session.left_running".to_string()];
        assert!(ended_with_the_target_running(None, &left));
        assert!(!ended_with_the_target_running(Some(0), &left), "the target exited, only its children run");
        assert!(!ended_with_the_target_running(None, &[]), "nothing was left running");
        assert!(!ended_with_the_target_running(None, &["session.followed_family".to_string()]));
    }

    /// A run cut short says the target may still run, and where it was given this command's stderr,
    /// that a reader of that stream may wait for it (R4/5 review round).
    #[test]
    fn a_run_cut_short_says_a_reader_of_stderr_may_wait_for_the_target() {
        let held = cut_short_notice(DriverLimit::Timeout, Some(5), true);
        assert!(held[0].contains("--timeout of 5s"), "{held:?}");
        assert!(held[1].contains("may still be running, so close it yourself"), "{held:?}");
        assert!(held[2].starts_with("chrono: if it is, it still writes to this command's standard error"), "{held:?}");
        assert_eq!(held.len(), 3);

        let not_held = cut_short_notice(DriverLimit::Idle, None, false);
        assert!(not_held[0].contains("sent no event for"), "{not_held:?}");
        assert_eq!(not_held.len(), 2, "no word about stderr when the target was not given it: {not_held:?}");
    }

    /// With the core gone the native half of the app reads the real date again (ADR-14, ADR-18, measured
    /// in R4/6 with the core killed), so the line about the app outliving the core must not claim the
    /// session clock for it - it did, and it was false.
    #[test]
    fn the_app_that_outlives_the_core_is_not_said_to_run_on_the_session_clock() {
        for line in cut_short_notice(DriverLimit::Idle, None, true).iter().chain(core_lost_notice(Some(1), true).iter()) {
            assert!(!line.contains("session clock"), "{line}");
        }
    }

    /// R4-W3: a core that stopped before it closed the session is named as that, with the code it died
    /// with, because that code is what tells a crash from a kill. The exit code of the run is held by
    /// `tests/core_lost.rs` on a real session, where the core is ended with the code that was measured.
    #[test]
    fn a_core_that_stopped_before_it_closed_the_session_is_said_with_the_code_it_died_with() {
        let killed = core_lost_notice(Some(1), false);
        assert_eq!(
            killed[0],
            "chrono: the core stopped before it closed the session (exit code 1), so this run proves nothing about the target"
        );
        assert!(killed[1].contains("may still be running, so close it yourself"), "{killed:?}");
        assert_eq!(killed.len(), 2, "no word about stderr when the target was not given it: {killed:?}");

        let crashed = core_lost_notice(Some(-1073741819), true);
        assert!(crashed[0].contains("(exit code -1073741819 (0xC0000005))"), "{crashed:?}");
        assert_eq!(crashed.len(), 3, "{crashed:?}");

        assert!(core_lost_notice(None, false)[0].contains("(exit code unknown)"));
    }

    /// The code that becomes 3 is still said, so a panic, a crash and a kill stay apart (R5-D4, item 3).
    #[test]
    fn the_no_verdict_line_names_the_core_code_it_replaced() {
        let panic = no_verdict_notice(Some(101));
        assert!(panic.contains("(exit code 101)") && panic.contains("without a verdict"), "{panic}");
        let crashed = no_verdict_notice(Some(0xC000_0005u32 as i32));
        assert!(crashed.contains("0xC0000005"), "an access violation is named as one: {crashed}");
        assert!(no_verdict_notice(None).contains("(exit code unknown)"));
        // The core's own internal-error code is the 3 it maps to, so nothing is lost and nothing is added.
        assert!(!no_verdict_notice(Some(3)).contains("exit code"), "{}", no_verdict_notice(Some(3)));
    }

    /// A core killed from outside carries no verdict, and the number it does carry is in no table
    /// the contract publishes - measured at -1. A pipeline branches on this, so an unknown code
    /// becomes the internal-error code instead of being passed through as if it meant something
    /// (R3-5).
    #[test]
    fn an_exit_code_outside_the_contract_becomes_the_internal_error_code() {
        for verdict in [0, 1, 2, 3, 4, 5, 6, crate::core::SESSION_BUSY_EXIT, 10, 11, 12] {
            assert_eq!(driver_exit_code(Some(verdict)), verdict);
        }
        assert_eq!(driver_exit_code(Some(-1)), 3, "a killed core must not look like a verdict");
        assert_eq!(driver_exit_code(Some(EVIDENCE_NOT_WRITTEN_EXIT)), 3, "8 is the driver's own, never the core's");
        assert_eq!(driver_exit_code(Some(0xC000_0005u32 as i32)), 3, "an access violation is not a verdict");
        assert_eq!(driver_exit_code(None), 3, "no code at all is not a verdict either");
    }

    /// A verdict whose evidence file was not written gives way to 8, and every code that is no verdict
    /// keeps its own (R5-D28).
    #[test]
    fn a_verdict_without_its_evidence_file_becomes_exit_code_8() {
        for verdict in [0, 4, 10, 11, 12] {
            assert_eq!(exit_code_with_evidence(verdict, true), verdict);
            assert_eq!(exit_code_with_evidence(verdict, false), EVIDENCE_NOT_WRITTEN_EXIT, "verdict {verdict}");
        }
        for no_verdict in [1, 2, 3, 5, 6, crate::core::SESSION_BUSY_EXIT] {
            assert_eq!(exit_code_with_evidence(no_verdict, false), no_verdict);
        }
        let said = evidence_lost_notice(10, "e.txt");
        assert!(said.contains("exit code 8") && said.contains("verdict's 10") && said.contains("e.txt"), "{said}");
    }
}
