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

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::process::{Command as PCommand, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono_core::calc::{Base, EvalContext, EvalError, MomentExpr};
use chrono_proto::{Command, Event, MomentSpec, TimeSpec, PROTOCOL_VERSION};

use args::{parse_run_args, target_spec_for};
use crate::calc::{calc_error_exit_code, describe_calc_error, resolve_now_civil};
use crate::cdp;
use crate::cli::print_usage;
use crate::grammar::parse_shift;
use crate::preset::{
    load_preset, preset_targets_substitution, read_target_creation_date, resolve_moment,
    resolve_parameters,
};
use crate::report::{
    mode_label, render_evidence, render_report, EvidenceParams, ProcessCoverage, SessionReport,
};
use crate::wire::read_protocol_line;
use crate::zone::{format_bias, session_zone_default};
/// How long `chrono run` waits for ANY event from the core before calling it hung.
///
/// Deliberately the same 15 s the GUI uses (`SessionViewModel.IdleTimeout`), because it is the same
/// core with the same internal deadlines behind it - and those deadlines are checked against this
/// number by `RustTimeoutMirrorTests`. The core beats `state` about once a real second in every
/// mode, so 15 s is fifteen missed heartbeats.
pub(crate) const DRIVER_IDLE_TIMEOUT_SECS: u64 = 15;


/// Resolve the `--at` value to an absolute wall string (the core only ever sees an
/// absolute moment). A leading `+`/`-` marks a relative moment - now plus one shift
/// step - resolved through the SHARED calc evaluator, so `--at` accepts exactly the
/// units the calculator does, including months, quarters, and years, which fold onto
/// the civil date (a fixed-tick delta cannot express them). Anything else passes
/// through as an absolute moment.
///
/// One grammar, not two: `--at`, `jump`, and the calculator all resolve through the same
/// step evaluator. The old `parse_relative_delta` (fixed-tick only) is gone entirely.
pub(crate) fn resolve_at(raw: &str, tz_bias_min: Option<i32>) -> Result<String, String> {
    if raw.starts_with(['+', '-']) {
        let now = resolve_now_civil(tz_bias_min)?;
        resolve_relative_at(raw, now)
    } else {
        Ok(raw.to_string())
    }
}

/// The pure core of a relative `--at`, taking "now" as data so it is deterministic to
/// test. The caller guarantees `raw` starts with a sign.
pub(crate) fn resolve_relative_at(raw: &str, now: chrono_core::calc::CivilDateTime) -> Result<String, String> {
    let step = parse_shift(raw)?;
    let expr = MomentExpr { base: Base::Now, steps: vec![step] };
    // `--at` builds a single shift step (no `zone` step) and reads back the civil result, so the
    // session-zone bias here only sets the unused result zone - 0 is fine.
    let outcome = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None })
        .map_err(describe_at_error)?;
    Ok(outcome.result().to_iso())
}

/// Message for an eval error while resolving a relative `--at`. A pre-spawn resolution
/// failure is a usage error (the caller exits 1), never a substitution verdict - business
/// days need a calendar, an extreme delta overflows. `--at` only ever builds one shift
/// step, so the step-level variants cannot occur, but the match stays total.
pub(crate) fn describe_at_error(e: EvalError) -> String {
    match e {
        EvalError::NeedsCalendar { .. } => {
            "relative --at uses business days, which need a calendar (not available here)".to_string()
        }
        EvalError::Overflow { .. } => "relative --at is too large".to_string(),
        EvalError::StepUnsupported { kind, .. } => format!("relative --at step '{kind}' is not supported"),
        EvalError::DegenerateCalendar { .. } => "relative --at found no matching date".to_string(),
        EvalError::BadSetTime { .. } => "relative --at has an invalid time".to_string(),
        EvalError::BaseYearOutOfRange | EvalError::YearOutOfRange { .. } => format!(
            "relative --at lands outside the year range this build computes on ({}..={})",
            chrono_core::CIVIL_YEAR_MIN,
            chrono_core::CIVIL_YEAR_MAX
        ),
    }
}

/// Send an `end` command to the core over its stdin.
pub(crate) fn send_end(stdin: &mut std::process::ChildStdin) {
    send_command(stdin, &Command::End { v: PROTOCOL_VERSION, id: 2 });
}

/// Send a `set_multiplier` command in flight.
pub(crate) fn send_set_multiplier(stdin: &mut std::process::ChildStdin, m: i64) {
    send_command(stdin, &Command::SetMultiplier { v: PROTOCOL_VERSION, id: 3, multiplier: m });
}

/// Send a `jump` command in flight. A leading +/- marks a relative jump (current fake + one step),
/// carried in `delta`; anything else is an absolute moment in the session zone, carried in `local`.
pub(crate) fn send_jump(stdin: &mut std::process::ChildStdin, moment: &str, tz_bias_min: Option<i32>) {
    let first = moment.as_bytes().first().copied();
    let to = if first == Some(b'+') || first == Some(b'-') {
        MomentSpec { kind: "relative".into(), local: None, tz_bias_min, delta: Some(moment.to_string()) }
    } else {
        MomentSpec { kind: "absolute".into(), local: Some(moment.to_string()), tz_bias_min, delta: None }
    };
    send_command(stdin, &Command::Jump { v: PROTOCOL_VERSION, id: 4, to });
}

pub(crate) fn send_command(stdin: &mut std::process::ChildStdin, cmd: &Command) {
    if let Ok(line) = serde_json::to_string(cmd) {
        let _ = writeln!(stdin, "{line}");
        let _ = stdin.flush();
    }
}

pub(crate) fn driver_run(argv: &[String]) -> i32 {
    let ra = match parse_run_args(argv) {
        Ok(ra) => ra,
        Err(e) => {
            eprintln!("chrono: {e}");
            print_usage();
            return 1;
        }
    };

    // The session zone when the caller named none: the HOST's. Reading "now" as UTC instead hands the
    // target a local time off by the host's own offset - the failure untouchable rule 2 names. This used
    // to be computed only in the no-preset arm and only for the no-`--at` case, so a relative `--at` and
    // a preset both fell back to UTC and disagreed with the plain `chrono run app.exe` beside them
    // (R2-S7). One value, computed once, used by every path that derives a moment from "now".
    let now_bias = session_zone_default(ra.zone_bias_min);

    // The moment AND the time mode come either from a named preset (docs/04 4.3) or from the flags.
    // A preset is resolved driver-side here - the same way a relative --at is - so the core still
    // receives an absolute moment and a plain mode, and never learns that a preset existed.
    let (resolved_at, mode, multiplier, scale_duration, session_bias) = if let Some(pid) = &ra.preset {
        match load_preset(pid) {
            Ok(p) => {
                // The substitution surface honours applies_to: a calculator-only preset is not a
                // substitution question. Refuse it rather than run a moment nobody asked to run.
                if !preset_targets_substitution(&p.applies_to) {
                    eprintln!(
                        "chrono: preset '{}' targets {}, not substitution (preset.not_for_substitution)",
                        p.id, p.applies_to
                    );
                    return 1;
                }
                // Resolve parameters (--param, then the target's file date for a
                // target_file_creation hint), then substitute them into the moment. A non-parametric
                // preset resolves to an empty map and an unchanged moment.
                // The target file's date, read in the SESSION's zone: a creation time near midnight
                // resolves to a different calendar day in UTC than on the host, and that day is what a
                // trial preset counts from.
                let target_date = read_target_creation_date(&ra.target, Some(now_bias));
                let values = match resolve_parameters(&p.parameters, &ra.params, target_date) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("chrono: {}", e.message());
                        return e.exit_code();
                    }
                };
                let moment = match resolve_moment(p.moment, &values) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("chrono: {}", e.message());
                        return e.exit_code();
                    }
                };
                // Evaluate the preset moment against real "now" in the session zone, exactly like a
                // relative --at, to an absolute wall moment. No calendar here - a preset that needs
                // one is an honest error (the run surface has no --calendar yet).
                let now = match resolve_now_civil(Some(now_bias)) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("chrono: cannot resolve current time: {e}");
                        return 3;
                    }
                };
                match chrono_core::calc::eval(
                    &moment,
                    &EvalContext { now, zone_bias_min: now_bias, calendar: None },
                ) {
                    Ok(outcome) => (
                        Some(outcome.result().to_iso()),
                        p.time_mode.mode.clone(),
                        p.time_mode.multiplier,
                        p.time_mode.scale_duration,
                        // The zone the moment was computed in travels with it: a preset moment paired
                        // with a bias of 0 would land an offset away from the instant it names.
                        Some(now_bias),
                    ),
                    Err(e) => {
                        eprintln!("chrono: preset '{}' moment: {}", p.id, describe_calc_error(&e));
                        return calc_error_exit_code(&e);
                    }
                }
            }
            Err(e) => {
                eprintln!("chrono: {}", e.message());
                return e.exit_code();
            }
        }
    } else {
        // Resolve a relative --at (now + delta) to an absolute moment before we spawn. With no --at at
        // all, the session clock starts at the real current time - the same thing the Chromium path
        // already does with the same command line. Until now the two mechanisms disagreed: on a native
        // target the empty moment travelled all the way into the core and came back as "moment must be
        // YYYY-MM-DDTHH:MM:SS, got ''" - an error about a flag the usage line calls optional, raised as
        // far from the user's mistake as it could be. It also makes `chrono run app.exe --mode x60`
        // mean what it reads as: run this application faster without moving its date.
        let resolved = match &ra.at {
            Some(raw) => match resolve_at(raw, Some(now_bias)) {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("chrono: {e}");
                    print_usage();
                    return 1;
                }
            },
            None => match resolve_now_civil(Some(now_bias)) {
                Ok(now) => Some(now.to_iso()),
                Err(e) => {
                    eprintln!("chrono: {e}");
                    return 1;
                }
            },
        };
        // The zone the moment above was read in travels with it: the core turns local + bias into the
        // UTC anchor, so a moment paired with the wrong bias lands an offset away from the instant it
        // names. ONE zone for the session, whatever shape the moment came in (R2-X5). An absolute
        // `--at` used to be the exception - a typed wall-clock string was read as UTC - which made the
        // same string mean two different instants depending on which half of the tool read it, and
        // made the exported evidence say "(host default)" about a session that ran on UTC.
        let session_bias = Some(now_bias);
        (resolved, ra.mode.clone(), ra.multiplier, ra.scale_duration, session_bias)
    };

    // A Chromium/Electron target is auto-detected by `__core` itself (ADR-9): the core takes the CDP
    // mechanism there and speaks this same protocol, so this driver streams and renders it exactly
    // like a native session. The only CDP-specific thing left here is labelling the report's coverage
    // unit as a JS "context" instead of a "pid" (the `cdp` flag on the report below).

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("chrono: cannot locate own executable: {e}");
            return 3;
        }
    };

    let mut child = match PCommand::new(exe)
        .arg("__core")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("chrono: cannot start core process: {e}");
            return 3;
        }
    };

    // Send `start` and keep stdin open so we can send `end` when we are done.
    let start = Command::Start {
        v: PROTOCOL_VERSION,
        id: 1,
        target: target_spec_for(&ra),
        time: TimeSpec {
            moment: MomentSpec {
                kind: "absolute".into(),
                local: resolved_at.clone(),
                tz_bias_min: session_bias,
                delta: None,
            },
            mode: mode.clone(),
            multiplier,
            scale_duration,
            scale_qpc: ra.scale_qpc,
        },
        force: ra.force,
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    {
        let line = serde_json::to_string(&start).expect("serialize start");
        if writeln!(stdin, "{line}").is_err() {
            eprintln!("chrono: core closed its input before start");
            let _ = child.wait();
            return 3;
        }
        let _ = stdin.flush();
    }

    // Stream events. Send `end` after `--ticks` state heartbeats, or right after the
    // verdict when ticks is 0 (one-shot), then read through to `ended`.
    let mut verdict_line: Option<(String, String)> = None; // parent (start) verdict, fallback
    let mut session_line: Option<(String, String, u32)> = None; // family: (verdict, reason_key, count)
    let mut vanished: Option<(String, u64)> = None; // (reason_key, lived_ms)
    let mut errors: Vec<(String, String)> = Vec::new(); // (key, origin) - why a session did not start
    let mut warnings: Vec<String> = Vec::new();
    // Coverage per process, LATEST snapshot wins. The core emits one event per process as it is
    // discovered and a final one for every process at the end, because the first is sampled inside
    // the guard window and its call counts never move again (R2-X8). Appending every event instead
    // would print each channel twice, once with a number from the session's first blink.
    let mut cov_by_pid: HashMap<u32, ProcessCoverage> = HashMap::new();
    let mut pid_order: Vec<u32> = Vec::new(); // first-seen order, so the parent still leads the report
    let mut timing: Option<(String, i64, i64)> = None; // (fake wall reached, real ms, fake ms) from `ended`
    // The target's own exit code and whatever teardown could not remove, both from `ended`. The wire
    // has carried them since the session report grew a duration, and the GUI panel has shown them
    // since 7446a59 - the human CLI report dropped them on the floor, so a plain `chrono run` could
    // not tell "the app closed itself with code 3" from "we ended the session" (rule 6).
    let mut target_exit: Option<i32> = None;
    let mut residue: Vec<String> = Vec::new();
    let mut states_seen: u64 = 0;
    let mut end_sent = false;
    // Why the read runs on its own thread rather than in this loop: the driver has to be able to
    // give up. Reading straight from the pipe here had no time limit and no liveness check, so a
    // core that stopped answering hung `chrono run` with nothing in the log to say why - on the
    // surface the README points at CI, where the only thing that eventually notices is the runner's
    // own job timeout (R3-5). The GUI has had an idle watchdog since M-10; this is the same idea on
    // the other client, and the two now use the same 15 s.
    //
    // The second reason is measured, not assumed: EOF on the core's stdout does NOT arrive when the
    // core dies, because the TARGET inherits the write end of that pipe and holds it open. "Read
    // until EOF" is really "read until the tested application exits", which is no liveness signal
    // for the core at all.
    let mut timed_out: Option<&'static str> = None;
    if let Some(stdout) = child.stdout.take() {
        let (line_tx, line_rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            // Not `reader.lines()`: that grows one line without limit, and this is the driver reading
            // a core it launched. A line past the cap ends the stream like a read error would.
            let mut raw = String::new();
            loop {
                match read_protocol_line(&mut reader, &mut raw) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if line_tx.send(std::mem::take(&mut raw)).is_err() {
                    break;
                }
            }
        });

        let started = Instant::now();
        loop {
            // Whether the CORE is still alive, asked before every wait - because a line arriving on
            // this pipe does NOT mean it is. The target inherits the write end of the core's stdout
            // and writes its own output there: measured with `ping` as the target, whose once-a-
            // second reply line is 49 bytes, the idle timer was reset for as long as the target ran,
            // so a core killed 45 s earlier still looked alive. EOF is no signal either, for the
            // same reason - the pipe stays open while the target holds it (R3-5).
            //
            // Once the core is gone the only thing left to do is drain what it already said, so the
            // wait shrinks to a moment: anything queued still arrives (a queued line returns
            // immediately), and the loop then ends as a normal end-of-session, not a timeout.
            let core_gone = matches!(child.try_wait(), Ok(Some(_)));
            // The idle limit, or whatever is left of `--timeout` when that is the nearer of the two,
            // so a ceiling is honoured to the second rather than to the end of the next idle window.
            let idle_budget = Duration::from_secs(DRIVER_IDLE_TIMEOUT_SECS);
            let budget = if core_gone {
                Duration::from_millis(200)
            } else {
                match ra.timeout_secs {
                    Some(secs) => Duration::from_secs(secs)
                        .checked_sub(started.elapsed())
                        .map_or(Duration::ZERO, |left| left.min(idle_budget)),
                    None => idle_budget,
                }
            };
            let raw = match line_rx.recv_timeout(budget) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) if core_gone => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Which limit ran out decides only what we SAY. Both end the same way - the core
                    // is killed and the exit code is 6 - because a driver that printed a verdict it
                    // never received would be the tool inventing evidence (untouchable rule 4).
                    timed_out = Some(match ra.timeout_secs {
                        Some(secs) if started.elapsed() >= Duration::from_secs(secs) => "timeout",
                        _ => "idle",
                    });
                    let _ = child.kill();
                    break;
                }
            };
            // `lines()` strips the terminator and this stands in its place, so every reader below
            // sees exactly what it saw before.
            let line = raw.strip_suffix('\n').unwrap_or(&raw);
            let line = line.strip_suffix('\r').unwrap_or(line).to_string();
            if line.is_empty() {
                continue;
            }
            if ra.json {
                println!("{line}");
            }
            match chrono_proto::parse_event(&line) {
                Ok(Event::Verdict { verdict, reason_key, .. }) => {
                    verdict_line = Some((verdict, reason_key));
                    // ticks == 0: stay attached until the target exits. Detaching
                    // early would revert it to real time (self-detach), so a run with
                    // no tick budget keeps the substitution for the target's whole
                    // life. ticks > 0 ends after that many state heartbeats.
                }
                Ok(Event::State { .. }) => {
                    states_seen += 1;
                    if let Some((t, m)) = ra.set_after
                        && states_seen == t {
                            send_set_multiplier(&mut stdin, m);
                        }
                    if let Some((t, ref mom)) = ra.jump_after
                        && states_seen == t {
                            // The SESSION's zone, not the raw flag: a jump names a wall-clock moment on
                            // the clock the target is already showing. Reading it as UTC while the
                            // session ran on the host's zone landed the jump an offset away - measured
                            // at exactly that: `--jump-after 2:2038-01-19T03:14:07` reached 05:14 on a
                            // UTC+2 host. That predates R2-S7 (it came in with "no --at follows the
                            // host"), and it is the same rule-2 failure in a second place.
                            send_jump(&mut stdin, mom, Some(now_bias));
                        }
                    if ra.ticks > 0 && states_seen >= ra.ticks && !end_sent {
                        send_end(&mut stdin);
                        end_sent = true;
                    }
                }
                Ok(Event::SessionVerdict { verdict, reason_key, process_count, warning_keys, .. }) => {
                    session_line = Some((verdict, reason_key, process_count));
                    // Session-level warnings join the same de-duplicated list as the per-process ones:
                    // the report has one `warnings:` block, and where a warning came from is a wire
                    // detail, not something the reader should have to know.
                    for k in warning_keys {
                        if !warnings.contains(&k) {
                            warnings.push(k);
                        }
                    }
                }
                Ok(Event::Vanished { reason_key, lived_ms, .. }) => {
                    vanished = Some((reason_key, lived_ms));
                }
                Ok(Event::Error { key, origin, .. }) => {
                    errors.push((key, origin));
                }
                Ok(Event::Coverage { pid, covered: cov, observed: obs, uncovered: unc, warning_keys, .. }) => {
                    // Warnings are a union across every event - a later snapshot for the same process
                    // carries no driver-side warning, and dropping the earlier one would lose it.
                    for k in warning_keys {
                        if !warnings.contains(&k) {
                            warnings.push(k);
                        }
                    }
                    if !cov_by_pid.contains_key(&pid) {
                        pid_order.push(pid);
                    }
                    cov_by_pid.insert(pid, ProcessCoverage { covered: cov, observed: obs, uncovered: unc });
                }
                Ok(Event::Ended {
                    elapsed_real_ms,
                    elapsed_fake_ms,
                    fake_end_wall,
                    target_exit_code,
                    residue_keys,
                    ..
                }) => {
                    if let Some(wall) = fake_end_wall {
                        timing = Some((wall, elapsed_real_ms, elapsed_fake_ms));
                    }
                    target_exit = target_exit_code;
                    residue = residue_keys;
                    break;
                }
                _ => {}
            }
        }
    }
    drop(stdin);

    let status = child.wait();

    // Flatten the per-process snapshots into report rows, parent first (first-seen order).
    let mut uncovered: Vec<(u32, String)> = Vec::new(); // (pid, channel) - the honest gaps
    let mut covered: Vec<(u32, String, u64)> = Vec::new(); // (pid, channel, calls) - what took effect
    let mut observed: Vec<(u32, String, u64)> = Vec::new(); // (pid, channel, calls) - hooked, left real
    for pid in &pid_order {
        if let Some(pc) = cov_by_pid.get(pid) {
            for ch in &pc.covered {
                covered.push((*pid, ch.channel.clone(), ch.calls));
            }
            for ch in &pc.observed {
                observed.push((*pid, ch.channel.clone(), ch.calls));
            }
            for ch in &pc.uncovered {
                uncovered.push((*pid, ch.clone()));
            }
        }
    }

    let report = SessionReport {
        target: ra.target.clone(),
        session_verdict: session_line,
        parent_verdict: verdict_line,
        vanished,
        errors,
        warnings,
        uncovered,
        covered,
        observed,
        timing,
        target_exit,
        residue,
        // The core auto-detects a Chromium target and runs it over CDP; label the report's coverage
        // unit accordingly. Same pure function, same path string the core sees, so the two never drift.
        cdp: cdp::is_chromium_target(&ra.target),
    };
    if let Some(path) = &ra.report {
        let params = EvidenceParams {
            moment: resolved_at.clone().unwrap_or_else(|| "(default)".into()),
            // The zone the session ACTUALLY ran in, not the flag. This line used to read
            // "(host default)" for a session running on UTC - a false statement in the file a tester
            // is meant to cite as proof (untouchable rule 4).
            zone: match ra.zone_bias_min {
                Some(b) => format_bias(b),
                None => format!("{} (host default)", format_bias(now_bias)),
            },
            mode: mode_label(&mode, multiplier),
        };
        match std::fs::write(path, render_evidence(&report, &params)) {
            Ok(()) => eprintln!("chrono: evidence written to {path}"),
            Err(e) => eprintln!("chrono: cannot write evidence to {path}: {e}"),
        }
    }
    if !ra.json {
        print!("{}", render_report(&report));
    }

    // A session that timed out has no verdict to report, and must not borrow one: the core was
    // killed mid-flight, so its exit code says how it died, not what it found (untouchable rule 4).
    if let Some(which) = timed_out {
        match which {
            "timeout" => eprintln!(
                "chrono: gave up after the --timeout of {}s - the core was stopped, so this run has no verdict",
                ra.timeout_secs.unwrap_or(0)
            ),
            _ => eprintln!(
                "chrono: the core sent nothing for {DRIVER_IDLE_TIMEOUT_SECS}s and was stopped - this run has no verdict"
            ),
        }
        return 6;
    }

    // The tool's exit code is the session verdict, carried by the core's exit code
    // (docs/08 section 8).
    let code = driver_exit_code(status.ok().and_then(|s| s.code()));
    if code == 3 {
        eprintln!("chrono: the core ended without a verdict - this run proves nothing about the target");
    }
    code
}

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
        Some(c) if matches!(c, 0 | 1 | 2 | 3 | 4 | 5 | 6 | 10 | 11 | 12) => c,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A core killed from outside carries no verdict, and the number it does carry is in no table
    /// the contract publishes - measured at -1. A pipeline branches on this, so an unknown code
    /// becomes the internal-error code instead of being passed through as if it meant something
    /// (R3-5).
    #[test]
    fn an_exit_code_outside_the_contract_becomes_the_internal_error_code() {
        for verdict in [0, 1, 2, 3, 4, 5, 6, 10, 11, 12] {
            assert_eq!(driver_exit_code(Some(verdict)), verdict);
        }
        assert_eq!(driver_exit_code(Some(-1)), 3, "a killed core must not look like a verdict");
        assert_eq!(driver_exit_code(Some(7)), 3);
        assert_eq!(driver_exit_code(Some(0xC000_0005u32 as i32)), 3, "an access violation is not a verdict");
        assert_eq!(driver_exit_code(None), 3, "no code at all is not a verdict either");
    }

    #[test]
    fn at_absolute_passes_through() {
        assert_eq!(
            resolve_at("2038-01-19T03:14:07", Some(0)).unwrap(),
            "2038-01-19T03:14:07"
        );
    }

    #[test]
    fn at_relative_resolves_to_absolute_wall() {
        // Value is now-dependent, but a valid delta must produce a wall string.
        let s = resolve_at("+1d", Some(0)).unwrap();
        assert!(s.contains('T') && s.len() == 19, "unexpected wall string: {s}");
    }

    #[test]
    fn at_relative_rejects_bad_unit_and_number() {
        assert!(resolve_at("+1x", None).is_err());
        assert!(resolve_at("+abcd", None).is_err());
        assert!(resolve_at("-y", None).is_err());
    }

    #[test]
    fn at_relative_fixed_unit_is_deterministic_with_now() {
        // Fixed-length units resolve exactly as before - now + a plain offset. Deterministic
        // because now is passed as data, not read from the clock.
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 25, hour: 14, minute: 30, second: 45 };
        assert_eq!(resolve_relative_at("+1d", now).unwrap(), "2026-08-26T14:30:45");
        assert_eq!(resolve_relative_at("-2h", now).unwrap(), "2026-08-25T12:30:45");
        assert_eq!(resolve_relative_at("+1w", now).unwrap(), "2026-09-01T14:30:45");
    }

    #[test]
    fn at_relative_now_accepts_calendar_units() {
        // The new capability: `--at` gains months/quarters/years through the shared model,
        // with the same clamp - the substitution side could not express these before.
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 25, hour: 0, minute: 0, second: 0 };
        assert_eq!(resolve_relative_at("+1mo", now).unwrap(), "2026-09-25T00:00:00");
        assert_eq!(resolve_relative_at("-18years", now).unwrap(), "2008-08-25T00:00:00");
        // End-of-month clamp reaches `--at` too: Jan 31 + 1 month = Feb 28 (2027, non-leap).
        let jan31 = chrono_core::calc::CivilDateTime { year: 2027, month: 1, day: 31, hour: 12, minute: 0, second: 0 };
        assert_eq!(resolve_relative_at("+1mo", jan31).unwrap(), "2027-02-28T12:00:00");
    }

    #[test]
    fn at_relative_business_days_need_a_calendar() {
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 25, hour: 0, minute: 0, second: 0 };
        let err = resolve_relative_at("+5bd", now).unwrap_err();
        assert!(err.contains("calendar"), "honest needs-a-calendar message, got: {err}");
    }
}
