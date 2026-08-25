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

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command as PCommand, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Deserialize;

use chrono_core::calc::{Base, EvalContext, EvalError, MomentExpr, Sign, Step, Unit};
use chrono_core::{filetime_utc_to_wall, verdict_from_coverage, Moment, SessionSpec, TimeMode, Verdict};
use chrono_proto::{
    parse_command, Clock, Command, CoveredChannel, Event, MomentSpec, TargetSpec, TimeSpec,
    PROTOCOL_VERSION,
};

const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(String::as_str) {
        Some("__core") => core_mode(),
        Some("run") => driver_run(&args[2..]),
        Some("calc") => calc_run(&args[2..]),
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

fn print_usage() {
    eprintln!("usage: chrono run <target> [--at <local-moment>] [--zone <+HH:MM>] [--mode <flow|frozen|xN>] [--scale-duration] [--ticks N] [--set-after T:M] [--jump-after T:moment] [--args \"...\"] [--report <path>] [--json]");
    print_calc_usage();
}

fn print_calc_usage() {
    eprintln!("usage: chrono calc [--base <today|now|YYYY-MM-DDTHH:MM:SS>] [--shift <±N<unit>>]... [--set-time <HH:MM:SS>] [--snap <target>] [--nearest <target>] [--zone <+HH:MM>] [--calendar <us-banking|us-federal>]");
    eprintln!("       units: s m h d w mo q y bd (minute=m, month=mo). built now: shift (fixed + mo/q/y), set-time. snap/nearest/business-days: not built yet");
}

fn this_bitness() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "x64"
    } else {
        "x86"
    }
}

// ---------------------------------------------------------------------------
// Driver: `chrono run ...`
// ---------------------------------------------------------------------------

struct RunArgs {
    target: String,
    args: Vec<String>,
    at: Option<String>,
    zone_bias_min: Option<i32>,
    /// Wire mode token: "flow", "frozen", or "multiplier".
    mode: String,
    multiplier: Option<i64>,
    scale_duration: bool,
    /// How many `state` heartbeats to stream before ending. 0 = end right after the
    /// verdict (one-shot).
    ticks: u64,
    /// After the Nth state heartbeat, send set_multiplier M (in-flight speed change).
    set_after: Option<(u64, i64)>,
    /// After the Nth state heartbeat, jump the wall clock to the given moment.
    jump_after: Option<(u64, String)>,
    /// Optional path to write the human evidence report to, in addition to stdout.
    report: Option<String>,
    json: bool,
}

/// Parse `--mode` into a wire mode token and optional multiplier.
/// `flow` = real speed, `frozen` = held, `xN` = accelerated N times (N >= 1).
fn parse_mode(raw: &str) -> Result<(String, Option<i64>), String> {
    match raw {
        "flow" => Ok(("flow".into(), None)),
        "frozen" => Ok(("frozen".into(), None)),
        _ => {
            let n = raw
                .strip_prefix('x')
                .or_else(|| raw.strip_prefix('X'))
                .ok_or_else(|| format!("mode must be flow, frozen, or xN like x60, got '{raw}'"))?;
            let m: i64 = n
                .parse()
                .map_err(|_| format!("bad multiplier in mode '{raw}'"))?;
            if m < 1 {
                return Err(format!("multiplier must be >= 1, got '{raw}'"));
            }
            Ok(("multiplier".into(), Some(m)))
        }
    }
}

fn parse_run_args(argv: &[String]) -> Result<RunArgs, String> {
    let mut target: Option<String> = None;
    let mut args: Vec<String> = Vec::new();
    let mut at: Option<String> = None;
    let mut zone_bias_min: Option<i32> = None;
    let mut mode = String::from("flow");
    let mut multiplier: Option<i64> = None;
    let mut scale_duration = false;
    let mut ticks: u64 = 0;
    let mut set_after: Option<(u64, i64)> = None;
    let mut jump_after: Option<(u64, String)> = None;
    let mut json = false;
    let mut report: Option<String> = None;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--at" => {
                i += 1;
                at = Some(argv.get(i).ok_or("--at needs a value")?.clone());
            }
            "--zone" => {
                i += 1;
                let raw = argv.get(i).ok_or("--zone needs a value like +02:00")?;
                zone_bias_min = Some(parse_zone_to_bias(raw)?);
            }
            "--mode" => {
                i += 1;
                let raw = argv.get(i).ok_or("--mode needs a value like x60")?;
                let (m, mult) = parse_mode(raw)?;
                mode = m;
                multiplier = mult;
            }
            "--args" => {
                i += 1;
                let raw = argv.get(i).ok_or("--args needs a value")?;
                args = raw.split_whitespace().map(str::to_string).collect();
            }
            "--scale-duration" => scale_duration = true,
            "--ticks" => {
                i += 1;
                let raw = argv.get(i).ok_or("--ticks needs a value")?;
                ticks = raw.parse().map_err(|_| format!("bad --ticks value '{raw}'"))?;
            }
            "--set-after" => {
                i += 1;
                let raw = argv.get(i).ok_or("--set-after needs <tick>:<multiplier>")?;
                let (t, m) = raw
                    .split_once(':')
                    .ok_or("--set-after must be <tick>:<multiplier>")?;
                set_after = Some((
                    t.parse().map_err(|_| format!("bad tick in '{raw}'"))?,
                    m.parse().map_err(|_| format!("bad multiplier in '{raw}'"))?,
                ));
            }
            "--jump-after" => {
                i += 1;
                let raw = argv.get(i).ok_or("--jump-after needs <tick>:<moment>")?;
                let (t, mom) = raw
                    .split_once(':')
                    .ok_or("--jump-after must be <tick>:<moment>")?;
                jump_after = Some((t.parse().map_err(|_| format!("bad tick in '{raw}'"))?, mom.to_string()));
            }
            "--json" => json = true,
            "--report" => {
                i += 1;
                report = Some(argv.get(i).ok_or("--report needs a path")?.clone());
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag '{other}'"));
            }
            other => {
                if target.is_none() {
                    target = Some(other.to_string());
                } else {
                    return Err(format!("unexpected argument '{other}'"));
                }
            }
        }
        i += 1;
    }

    Ok(RunArgs {
        target: target.ok_or("missing <target>")?,
        args,
        at,
        zone_bias_min,
        mode,
        multiplier,
        scale_duration,
        ticks,
        set_after,
        jump_after,
        report,
        json,
    })
}

/// Parse a "+HH:MM" / "-HH:MM" offset into a session bias in minutes.
/// UTC = local + bias, so a local zone of UTC+2 gives bias -120.
fn parse_zone_to_bias(raw: &str) -> Result<i32, String> {
    let bytes = raw.as_bytes();
    let sign = match bytes.first() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return Err(format!("zone must start with + or -, got '{raw}'")),
    };
    let rest = &raw[1..];
    let (h, m) = rest
        .split_once(':')
        .ok_or_else(|| format!("zone must look like +HH:MM, got '{raw}'"))?;
    let hours: i32 = h.parse().map_err(|_| format!("bad zone hours in '{raw}'"))?;
    let mins: i32 = m.parse().map_err(|_| format!("bad zone minutes in '{raw}'"))?;
    let offset = sign * (hours * 60 + mins);
    Ok(-offset)
}

/// Real UTC now as FILETIME ticks (100 ns since 1601), via std - the driver is not
/// hooked, so this is genuine.
fn now_filetime_utc() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    const DAYS_1601_TO_1970: i64 = 134_774;
    (DAYS_1601_TO_1970 * 86_400 + d.as_secs() as i64) * 10_000_000 + (d.subsec_nanos() as i64 / 100)
}

/// Resolve the `--at` value to an absolute wall string (the core only ever sees an
/// absolute moment). A leading `+`/`-` marks a relative moment - now plus one shift
/// step - resolved through the SHARED calc evaluator, so `--at` accepts exactly the
/// units the calculator does, including months, quarters, and years, which fold onto
/// the civil date (a fixed-tick delta cannot express them). Anything else passes
/// through as an absolute moment.
///
/// One grammar, not two: `--at`, `jump`, and the calculator all resolve through the same
/// step evaluator. The old `parse_relative_delta` (fixed-tick only) is gone entirely.
fn resolve_at(raw: &str, tz_bias_min: Option<i32>) -> Result<String, String> {
    if raw.starts_with(['+', '-']) {
        let now = resolve_now_civil(tz_bias_min)?;
        resolve_relative_at(raw, now)
    } else {
        Ok(raw.to_string())
    }
}

/// The pure core of a relative `--at`, taking "now" as data so it is deterministic to
/// test. The caller guarantees `raw` starts with a sign.
fn resolve_relative_at(raw: &str, now: chrono_core::calc::CivilDateTime) -> Result<String, String> {
    let step = parse_shift(raw)?;
    let expr = MomentExpr { base: Base::Now, steps: vec![step] };
    let outcome =
        chrono_core::calc::eval(&expr, &EvalContext { now, calendar: None }).map_err(describe_at_error)?;
    Ok(outcome.result().to_iso())
}

/// Message for an eval error while resolving a relative `--at`. A pre-spawn resolution
/// failure is a usage error (the caller exits 1), never a substitution verdict - business
/// days need a calendar, an extreme delta overflows. `--at` only ever builds one shift
/// step, so the step-level variants cannot occur, but the match stays total.
fn describe_at_error(e: EvalError) -> String {
    match e {
        EvalError::UnitUnsupported { unit, .. } => {
            format!("relative --at unit '{unit}' is not built yet (needs a calendar)")
        }
        EvalError::Overflow { .. } => "relative --at is too large".to_string(),
        EvalError::StepUnsupported { kind, .. } => format!("relative --at step '{kind}' is not supported"),
        EvalError::BadSetTime { .. } => "relative --at has an invalid time".to_string(),
    }
}

/// Send an `end` command to the core over its stdin.
fn send_end(stdin: &mut std::process::ChildStdin) {
    send_command(stdin, &Command::End { v: PROTOCOL_VERSION, id: 2 });
}

/// Send a `set_multiplier` command in flight.
fn send_set_multiplier(stdin: &mut std::process::ChildStdin, m: i64) {
    send_command(stdin, &Command::SetMultiplier { v: PROTOCOL_VERSION, id: 3, multiplier: m });
}

/// Translation key for a relative-jump eval error (docs/08 section 10). Business days need a
/// calendar (not built yet); anything else is an invalid moment. Honest, never silent (rule 6).
fn jump_error_key(e: EvalError) -> &'static str {
    match e {
        EvalError::UnitUnsupported { .. } | EvalError::StepUnsupported { .. } => "moment.unit_not_built",
        EvalError::Overflow { .. } | EvalError::BadSetTime { .. } => "moment.invalid",
    }
}

/// Send a `jump` command in flight. A leading +/- marks a relative jump (current fake + one step),
/// carried in `delta`; anything else is an absolute moment in the session zone, carried in `local`.
fn send_jump(stdin: &mut std::process::ChildStdin, moment: &str, tz_bias_min: Option<i32>) {
    let first = moment.as_bytes().first().copied();
    let to = if first == Some(b'+') || first == Some(b'-') {
        MomentSpec { kind: "relative".into(), local: None, tz_bias_min, delta: Some(moment.to_string()) }
    } else {
        MomentSpec { kind: "absolute".into(), local: Some(moment.to_string()), tz_bias_min, delta: None }
    };
    send_command(stdin, &Command::Jump { v: PROTOCOL_VERSION, id: 4, to });
}

fn send_command(stdin: &mut std::process::ChildStdin, cmd: &Command) {
    if let Ok(line) = serde_json::to_string(cmd) {
        let _ = writeln!(stdin, "{line}");
        let _ = stdin.flush();
    }
}

fn driver_run(argv: &[String]) -> i32 {
    let ra = match parse_run_args(argv) {
        Ok(ra) => ra,
        Err(e) => {
            eprintln!("chrono: {e}");
            print_usage();
            return 1;
        }
    };

    // Resolve a relative --at (now + delta) to an absolute moment before we spawn.
    let resolved_at = match &ra.at {
        Some(raw) => match resolve_at(raw, ra.zone_bias_min) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("chrono: {e}");
                print_usage();
                return 1;
            }
        },
        None => None,
    };

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
        target: TargetSpec {
            path: ra.target.clone(),
            args: ra.args.clone(),
            cwd: None,
        },
        time: TimeSpec {
            moment: MomentSpec {
                kind: "absolute".into(),
                local: resolved_at.clone(),
                tz_bias_min: ra.zone_bias_min,
                delta: None,
            },
            mode: ra.mode.clone(),
            multiplier: ra.multiplier,
            scale_duration: ra.scale_duration,
        },
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
    let mut warnings: Vec<String> = Vec::new();
    let mut uncovered: Vec<(u32, String)> = Vec::new(); // (pid, channel) - the honest gaps
    let mut covered: Vec<(u32, String, u64)> = Vec::new(); // (pid, channel, calls) - what took effect
    let mut observed: Vec<(u32, String, u64)> = Vec::new(); // (pid, channel, calls) - hooked, left real
    let mut timing: Option<(String, i64, i64)> = None; // (fake wall reached, real ms, fake ms) from `ended`
    let mut states_seen: u64 = 0;
    let mut end_sent = false;
    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
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
                    if let Some((t, m)) = ra.set_after {
                        if states_seen == t {
                            send_set_multiplier(&mut stdin, m);
                        }
                    }
                    if let Some((t, ref mom)) = ra.jump_after {
                        if states_seen == t {
                            send_jump(&mut stdin, mom, ra.zone_bias_min);
                        }
                    }
                    if ra.ticks > 0 && states_seen >= ra.ticks && !end_sent {
                        send_end(&mut stdin);
                        end_sent = true;
                    }
                }
                Ok(Event::SessionVerdict { verdict, reason_key, process_count, .. }) => {
                    session_line = Some((verdict, reason_key, process_count));
                }
                Ok(Event::Vanished { reason_key, lived_ms, .. }) => {
                    vanished = Some((reason_key, lived_ms));
                }
                Ok(Event::Coverage { pid, covered: cov, observed: obs, uncovered: unc, warning_keys, .. }) => {
                    for k in warning_keys {
                        if !warnings.contains(&k) {
                            warnings.push(k);
                        }
                    }
                    for ch in cov {
                        covered.push((pid, ch.channel, ch.calls));
                    }
                    for ch in obs {
                        observed.push((pid, ch.channel, ch.calls));
                    }
                    for ch in unc {
                        uncovered.push((pid, ch));
                    }
                }
                Ok(Event::Ended { elapsed_real_ms, elapsed_fake_ms, fake_end_wall, .. }) => {
                    if let Some(wall) = fake_end_wall {
                        timing = Some((wall, elapsed_real_ms, elapsed_fake_ms));
                    }
                    break;
                }
                _ => {}
            }
        }
    }
    drop(stdin);

    let status = child.wait();

    let report = SessionReport {
        target: ra.target.clone(),
        session_verdict: session_line,
        parent_verdict: verdict_line,
        vanished,
        warnings,
        uncovered,
        covered,
        observed,
        timing,
    };
    if let Some(path) = &ra.report {
        let params = EvidenceParams {
            moment: resolved_at.clone().unwrap_or_else(|| "(default)".into()),
            zone: ra.zone_bias_min.map(format_bias).unwrap_or_else(|| "(host default)".into()),
            mode: mode_label(&ra.mode, ra.multiplier),
        };
        match std::fs::write(path, render_evidence(&report, &params)) {
            Ok(()) => eprintln!("chrono: evidence written to {path}"),
            Err(e) => eprintln!("chrono: cannot write evidence to {path}: {e}"),
        }
    }
    if !ra.json {
        print!("{}", render_report(&report));
    }

    // The tool's exit code is the session verdict, carried by the core's exit code
    // (docs/08 section 8).
    match status {
        Ok(s) => s.code().unwrap_or(3),
        Err(_) => 3,
    }
}

/// The captured outcome of a `chrono run` session, rendered as a human report in the
/// non-json path. The core emits stable KEYS; this consumer renders English prose (rule 15:
/// the CLI is English-only, so the key->text table lives here, never in the core - rule 16).
/// The Stage 2 verifier is exactly this: a view over what the core already emits, one that
/// makes a NON-effect as legible as an effect.
struct SessionReport {
    target: String,
    /// Family verdict (verdict token, reason key, process count) - the headline.
    session_verdict: Option<(String, String, u32)>,
    /// Parent/start verdict, used only if no session_verdict arrived (e.g. an older core).
    parent_verdict: Option<(String, String)>,
    /// The target vanished right after injection - an honest non-effect (ADR-4).
    vanished: Option<(String, u64)>,
    warnings: Vec<String>,
    /// Channels queried but not covered, tagged with the pid that queried them (never summed
    /// across processes - untouchable rule 4).
    uncovered: Vec<(u32, String)>,
    /// Channels covered (substituted), tagged with the pid and the call count. Per-pid, never
    /// summed across processes (untouchable rule 4).
    covered: Vec<(u32, String, u64)>,
    /// Channels hooked but deliberately left real (waits, network, multimedia timers) - their own
    /// bucket so a reader never reads them as substituted.
    observed: Vec<(u32, String, u64)>,
    /// Last state heartbeat seen: (fake wall reached, real ms elapsed, fake ms elapsed), or None
    /// when the session was too short for a heartbeat. View-only, sampled from the state stream.
    timing: Option<(String, i64, i64)>,
}

/// One-line English headline for a verdict wire token. Upper-case so success and failure are
/// scannable at a glance.
fn verdict_headline(verdict: &str) -> &'static str {
    match verdict {
        "works" => "WORKS - time substitution took effect",
        "partial" => "PARTIAL - time substitution took effect only in part",
        "fails" => "DID NOT TAKE EFFECT - time was queried but no channel was covered",
        "undetermined" => "UNDETERMINED - coverage could not be established",
        _ => "UNKNOWN",
    }
}

/// English gloss for a reason key. An unknown key yields "" and the caller falls back to the
/// raw key - we never invent an explanation the core did not send.
fn describe_reason(key: &str) -> &'static str {
    match key {
        "session.family_covered" | "coverage.time_channels_covered" => {
            "every process that read time saw the session clock"
        }
        "session.family_partial" | "coverage.time_channels_partial" => {
            "some time channels were covered, some were queried but not covered"
        }
        "session.family_uncovered" | "coverage.time_channels_uncovered" => {
            "time was queried but no channel was covered"
        }
        "session.family_undetermined" | "coverage.undetermined" => {
            "no process read a covered time channel"
        }
        _ => "",
    }
}

/// English gloss for a warning key, with the key appended for traceability. An unknown key is
/// shown verbatim (honest fallback).
fn describe_warning(key: &str) -> String {
    let text = match key {
        "wait.object_waits_not_scaled" => {
            "object waits are hooked but left real - an I/O or hardware timeout is not shortened"
        }
        "timer.multimedia_not_scaled" => "the multimedia timer (timeSetEvent) is observed but not scaled",
        "inheritance.ntcreateuserprocess_child_maybe_uncovered" => {
            "a child spawned directly via NtCreateUserProcess may not be covered"
        }
        "source.network_at_start" => {
            "the target opened a network connection - it may read time from a server, which no local hook can cover"
        }
        _ => "",
    };
    if text.is_empty() {
        key.to_string()
    } else {
        format!("{text} ({key})")
    }
}

/// "1 call" vs "N calls" - a bare plural reads wrong in a report a user pastes into a bug ticket.
fn calls_label(n: u64) -> String {
    if n == 1 {
        "1 call".to_string()
    } else {
        format!("{n} calls")
    }
}

/// Render the session outcome as a human report (English). Failure and non-effect are the
/// point: the Stage 2 gate is recognising when substitution did NOT take effect.
fn render_report(r: &SessionReport) -> String {
    let mut out = String::from("Chrono Mock - session report\n");
    out.push_str(&format!("  target:   {}\n", r.target));

    // Headline priority: a vanish is an honest non-effect, then the family verdict, then the
    // parent verdict as a fallback for an older core, then nothing.
    if let Some((reason_key, lived_ms)) = &r.vanished {
        out.push_str("  verdict:  DID NOT TAKE EFFECT - the target vanished right after injection\n");
        out.push_str(&format!(
            "            (suspected single-instance app: {reason_key}; lived {lived_ms} ms)\n"
        ));
    } else if let Some((verdict, reason_key, count)) = &r.session_verdict {
        out.push_str(&format!(
            "  verdict:  {}  (processes: {count})\n",
            verdict_headline(verdict)
        ));
        let why = describe_reason(reason_key);
        if why.is_empty() {
            out.push_str(&format!("            reason: {reason_key}\n"));
        } else {
            out.push_str(&format!("            {why}\n"));
        }
    } else if let Some((verdict, reason_key)) = &r.parent_verdict {
        out.push_str(&format!("  verdict:  {}\n", verdict_headline(verdict)));
        out.push_str(&format!("            reason: {reason_key}\n"));
    } else {
        out.push_str("  verdict:  <no verdict emitted>\n");
    }

    if let Some((fake_wall, real_ms, fake_ms)) = &r.timing {
        out.push_str(&format!("  session:  fake clock reached {fake_wall}\n"));
        out.push_str(&format!(
            "            real elapsed {:.1}s, fake elapsed {:.1}s\n",
            *real_ms as f64 / 1000.0,
            *fake_ms as f64 / 1000.0
        ));
    }

    if !r.covered.is_empty() {
        out.push_str("  covered channels (substituted, with call counts):\n");
        for (pid, ch, calls) in &r.covered {
            out.push_str(&format!("            - pid {pid}: {ch} ({})\n", calls_label(*calls)));
        }
    }

    if !r.observed.is_empty() {
        out.push_str("  observed channels (hooked but left real):\n");
        for (pid, ch, calls) in &r.observed {
            out.push_str(&format!("            - pid {pid}: {ch} ({})\n", calls_label(*calls)));
        }
    }

    if !r.uncovered.is_empty() {
        out.push_str("  uncovered channels (queried but not covered):\n");
        for (pid, ch) in &r.uncovered {
            out.push_str(&format!("            - pid {pid}: {ch}\n"));
        }
    }

    if !r.warnings.is_empty() {
        out.push_str("  warnings:\n");
        for w in &r.warnings {
            out.push_str(&format!("            - {}\n", describe_warning(w)));
        }
    }

    out
}

/// Session parameters echoed into an evidence export so the file stands alone as proof (8.8).
struct EvidenceParams {
    moment: String,
    zone: String,
    mode: String,
}

/// A clean WORKS session (no vanish, no partial/fails). Anything else must carry the unreliable
/// banner in an evidence export - evidence that hides doubt is worse than none (8.8).
fn session_is_reliable(r: &SessionReport) -> bool {
    if r.vanished.is_some() {
        return false;
    }
    if let Some((v, _, _)) = &r.session_verdict {
        return v == "works";
    }
    if let Some((v, _)) = &r.parent_verdict {
        return v == "works";
    }
    false
}

/// Format a session bias (minutes, UTC = local + bias) back to a "+HH:MM" zone label.
fn format_bias(bias: i32) -> String {
    let offset = -bias;
    let sign = if offset < 0 { '-' } else { '+' };
    let abs = offset.abs();
    format!("{sign}{:02}:{:02}", abs / 60, abs % 60)
}

/// Human label for the requested time mode.
fn mode_label(mode: &str, multiplier: Option<i64>) -> String {
    match mode {
        "multiplier" => format!("x{}", multiplier.unwrap_or(1)),
        other => other.to_string(),
    }
}

/// Compose the evidence file: an unreliable banner for any non-WORKS session (8.8), then the human
/// report, then the requested parameters so the file is self-contained proof.
fn render_evidence(r: &SessionReport, p: &EvidenceParams) -> String {
    let mut out = String::new();
    if !session_is_reliable(r) {
        out.push_str(
            "!! UNRELIABLE EVIDENCE - the time substitution did not fully take effect. \
             Do not cite this as proof of behavior.\n\n",
        );
    }
    out.push_str(&render_report(r));
    out.push_str(&format!(
        "  requested:  {} (zone {}, mode {})\n",
        p.moment, p.zone, p.mode
    ));
    out
}

// ---------------------------------------------------------------------------
// Date calculator: `chrono calc ...`
// ---------------------------------------------------------------------------
//
// The calculator surface of the shared step grammar (docs/04 section 4.3). The
// driver parses typed flags into a canonical `MomentExpr`, resolves "now" for a
// today/now base (the core stays pure and takes now as data), evaluates, and
// renders. No natural language (6.2): each flag is one step, order is step order.

/// Parsed `chrono calc` arguments: a base, an ordered step list, and the session
/// zone used to resolve a today/now base.
struct CalcArgs {
    base: Base,
    steps: Vec<Step>,
    zone_bias_min: Option<i32>,
    /// Calendar id (e.g. "us-banking") for business-day and holiday metadata. None = omit them.
    calendar: Option<String>,
}

fn calc_run(argv: &[String]) -> i32 {
    let ca = match parse_calc_args(argv) {
        Ok(ca) => ca,
        Err(e) => {
            eprintln!("chrono: {e}");
            print_calc_usage();
            return 1;
        }
    };

    // Resolve the real current time in the session zone, as data for the pure core.
    // Same pattern as `resolve_at`: UTC now, shifted by the session bias (UTC when
    // no zone is given). Zone-aware "today" without an explicit zone is a later slice.
    let now = match resolve_now_civil(ca.zone_bias_min) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("chrono: cannot resolve current time: {e}");
            return 3;
        }
    };

    // Load the calendar (if requested) before evaluating: a missing or malformed calendar is a
    // usage error surfaced before any result, never a silently dropped metadata field.
    let calendar = match &ca.calendar {
        Some(id) => match load_calendar(id) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("chrono: {e}");
                return 1;
            }
        },
        None => None,
    };

    let expr = MomentExpr { base: ca.base, steps: ca.steps };
    match chrono_core::calc::eval(&expr, &EvalContext { now, calendar: calendar.as_ref() }) {
        Ok(outcome) => {
            print!("{}", render_calc(&expr, &outcome, ca.zone_bias_min, &now, calendar.as_ref()));
            0
        }
        Err(e) => {
            eprintln!("{}", describe_calc_error(&e));
            calc_error_exit_code(&e)
        }
    }
}

fn parse_calc_args(argv: &[String]) -> Result<CalcArgs, String> {
    let mut base = Base::Today;
    let mut steps: Vec<Step> = Vec::new();
    let mut zone_bias_min: Option<i32> = None;
    let mut calendar: Option<String> = None;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--base" => {
                i += 1;
                base = parse_base(argv.get(i).ok_or("--base needs a value")?)?;
            }
            "--calendar" => {
                i += 1;
                calendar = Some(argv.get(i).ok_or("--calendar needs an id like us-banking")?.clone());
            }
            "--zone" => {
                i += 1;
                let raw = argv.get(i).ok_or("--zone needs a value like +02:00")?;
                zone_bias_min = Some(parse_zone_to_bias(raw)?);
            }
            "--shift" => {
                i += 1;
                steps.push(parse_shift(argv.get(i).ok_or("--shift needs a value like +18years")?)?);
            }
            "--set-time" => {
                i += 1;
                steps.push(parse_set_time(argv.get(i).ok_or("--set-time needs a value like 23:59:59")?)?);
            }
            "--snap" => {
                i += 1;
                steps.push(Step::Snap(argv.get(i).ok_or("--snap needs a target")?.clone()));
            }
            "--nearest" => {
                i += 1;
                steps.push(Step::Nearest(argv.get(i).ok_or("--nearest needs a target")?.clone()));
            }
            other if other.starts_with("--") => return Err(format!("unknown flag '{other}'")),
            other => return Err(format!("unexpected argument '{other}'")),
        }
        i += 1;
    }

    Ok(CalcArgs { base, steps, zone_bias_min, calendar })
}

/// Parse a `--base` value: the keywords `today`/`now`, or an absolute civil date-time.
fn parse_base(raw: &str) -> Result<Base, String> {
    match raw {
        "today" => Ok(Base::Today),
        "now" => Ok(Base::Now),
        _ => Ok(Base::Absolute(chrono_core::calc::parse_civil_datetime(raw)?)),
    }
}

/// Parse a `--shift` value `±N<unit>` into a shift step. The sign is mandatory; the
/// unit accepts short codes and full names. Minute stays `m`; month is `mo`, never `m`.
fn parse_shift(raw: &str) -> Result<Step, String> {
    let sign = match raw.as_bytes().first() {
        Some(b'+') => Sign::Plus,
        Some(b'-') => Sign::Minus,
        _ => return Err(format!("shift must start with + or -, got '{raw}'")),
    };
    let rest = &raw[1..];
    let split = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let (num, unit_str) = rest.split_at(split);
    if num.is_empty() {
        return Err(format!("shift needs a number, got '{raw}'"));
    }
    let amount: i64 = num.parse().map_err(|_| format!("bad number in shift '{raw}'"))?;
    let unit = parse_unit(unit_str).ok_or_else(|| format!("unknown unit '{unit_str}' in shift '{raw}'"))?;
    Ok(Step::Shift { sign, amount, unit })
}

/// Map a unit token (short code or full name) to a canonical unit. 🔴 `m` is minutes,
/// `mo` is months - never conflate them (the substitution `--at` delta already uses `m`
/// for minutes, so calc keeps the same convention).
fn parse_unit(s: &str) -> Option<Unit> {
    Some(match s {
        "s" | "sec" | "secs" | "seconds" => Unit::Seconds,
        "m" | "min" | "mins" | "minutes" => Unit::Minutes,
        "h" | "hr" | "hrs" | "hours" => Unit::Hours,
        "d" | "day" | "days" => Unit::Days,
        "w" | "week" | "weeks" => Unit::Weeks,
        "mo" | "month" | "months" => Unit::Months,
        "q" | "quarter" | "quarters" => Unit::Quarters,
        "y" | "yr" | "yrs" | "year" | "years" => Unit::Years,
        "bd" | "business_days" | "businessdays" => Unit::BusinessDays,
        _ => return None,
    })
}

/// Parse a `--set-time` value `HH:MM:SS`. Field ranges are validated in the core
/// evaluator (BadSetTime), so parsing only checks the shape and numeric form here.
fn parse_set_time(raw: &str) -> Result<Step, String> {
    let p: Vec<&str> = raw.split(':').collect();
    if p.len() != 3 {
        return Err(format!("set-time must be HH:MM:SS, got '{raw}'"));
    }
    let hour = p[0].parse().map_err(|_| format!("bad hour in set-time '{raw}'"))?;
    let minute = p[1].parse().map_err(|_| format!("bad minute in set-time '{raw}'"))?;
    let second = p[2].parse().map_err(|_| format!("bad second in set-time '{raw}'"))?;
    Ok(Step::SetTime { hour, minute, second })
}

/// Real current time in the session zone, as a civil date-time for the pure core.
/// Reuses the tested UTC-now and wall-clock conversion, then parses back to civil.
fn resolve_now_civil(zone_bias_min: Option<i32>) -> Result<chrono_core::calc::CivilDateTime, String> {
    let wall = filetime_utc_to_wall(now_filetime_utc(), zone_bias_min.unwrap_or(0));
    chrono_core::calc::parse_civil_datetime(&wall)
}

/// Exit code for a calc error (a small table separate from the substitution verdict
/// codes in docs/08 section 8): bad input is a usage error (1), an operation not built
/// in this release is its own honest code (5) so a script can tell the two apart.
fn calc_error_exit_code(e: &EvalError) -> i32 {
    match e {
        EvalError::StepUnsupported { .. } | EvalError::UnitUnsupported { .. } => 5,
        EvalError::Overflow { .. } | EvalError::BadSetTime { .. } => 1,
    }
}

/// Human message for a calc error, carrying a stable key (docs/08 section 10) and a
/// 1-based step number. "Not built yet" is the product's honest vocabulary, never a
/// silent skip or a faked result (zasady/01 section 2).
fn describe_calc_error(e: &EvalError) -> String {
    match e {
        EvalError::StepUnsupported { kind, index } => {
            format!("chrono calc: step {} ({kind}) is not built yet (calc.step_unsupported)", index + 1)
        }
        EvalError::UnitUnsupported { unit, index } => {
            format!(
                "chrono calc: step {} (shift {unit}) needs a calendar, not built yet (calc.unit_unsupported)",
                index + 1
            )
        }
        EvalError::Overflow { index } => {
            format!("chrono calc: step {} overflows the representable range (calc.overflow)", index + 1)
        }
        EvalError::BadSetTime { index } => {
            format!("chrono calc: step {} has an out-of-range time (calc.bad_set_time)", index + 1)
        }
    }
}

/// Render a calc result: the base, the intermediate value after each step, and the
/// final moment (7.3 - the user sees where they went wrong, not just the final number).
fn render_calc(
    expr: &MomentExpr,
    outcome: &chrono_core::calc::EvalOutcome,
    zone_bias_min: Option<i32>,
    now: &chrono_core::calc::CivilDateTime,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    let zone = zone_bias_min.map(format_bias).unwrap_or_else(|| "UTC".into());
    let mut out = String::from("Chrono Mock - date calculator\n");
    match &expr.base {
        Base::Today => out.push_str(&format!("  base:    {}  (today, session zone {zone})\n", outcome.base.to_iso())),
        Base::Now => out.push_str(&format!("  base:    {}  (now, session zone {zone})\n", outcome.base.to_iso())),
        Base::Absolute(_) => out.push_str(&format!("  base:    {}\n", outcome.base.to_iso())),
    }
    for (i, step) in expr.steps.iter().enumerate() {
        out.push_str(&format!("  step {}:  {}  -> {}\n", i + 1, describe_step(step), outcome.after_each[i].to_iso()));
    }
    out.push_str(&format!("  result:  {}\n", outcome.result().to_iso()));
    out.push_str(&render_formats(&outcome.result(), zone_bias_min.unwrap_or(0)));
    out.push_str(&render_metadata(&outcome.result(), now, calendar));
    out
}

/// Render the metadata for the result (7.3): weekday, ISO and US week numbers side by side
/// (they are different numbers), day of year, quarter, leap year, and the signed day distance
/// from today. With a calendar, also the business-day and holiday fields (which need one) -
/// omitted entirely without a calendar, never guessed.
fn render_metadata(
    civil: &chrono_core::calc::CivilDateTime,
    now: &chrono_core::calc::CivilDateTime,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    let m = chrono_core::calc::metadata(civil, now);
    let days = match m.days_from_today {
        0 => "today".to_string(),
        n if n > 0 => format!("+{n} days"),
        n => format!("{n} days"),
    };
    let mut out = String::from("  metadata:\n");
    out.push_str(&format!("    weekday       {}\n", m.weekday));
    out.push_str(&format!("    ISO week      {:04}-W{:02}\n", m.iso_week_year, m.iso_week));
    out.push_str(&format!("    US week       {}\n", m.us_week));
    out.push_str(&format!("    day of year   {}\n", m.day_of_year));
    out.push_str(&format!("    quarter       Q{}\n", m.quarter));
    out.push_str(&format!("    leap year     {}\n", if m.is_leap_year { "yes" } else { "no" }));
    out.push_str(&format!("    days from now {days}\n"));

    if let Some(cal) = calendar {
        let business = if chrono_core::calendar::is_business_day(civil, cal) { "yes" } else { "no" };
        out.push_str(&format!("    business day  {business}  ({})\n", cal.id));
        let holiday = match chrono_core::calendar::holiday_on(civil, cal) {
            Some(h) => h.name_en.as_str(),
            None => "no",
        };
        out.push_str(&format!("    holiday       {holiday}\n"));
    }
    out
}

/// Render the result in every output format, each on its own labelled line ready to copy
/// (docs/02 section 8, in that order). An instant-based format outside FILETIME range shows
/// "(out of range)" rather than a wrong number or nothing.
fn render_formats(civil: &chrono_core::calc::CivilDateTime, tz_bias_min: i32) -> String {
    let f = chrono_core::calc::formats(civil, tz_bias_min);
    let num = |n: Option<i64>| n.map(|v| v.to_string()).unwrap_or_else(|| "(out of range)".into());
    let mut out = String::from("  formats:\n");
    out.push_str(&format!("    ISO date      {}\n", f.iso_date));
    out.push_str(&format!("    ISO datetime  {}\n", f.iso_datetime));
    out.push_str(&format!("    US            {}\n", f.us));
    out.push_str(&format!("    PL            {}\n", f.pl));
    out.push_str(&format!("    epoch (s)     {}\n", num(f.epoch_seconds)));
    out.push_str(&format!("    epoch (ms)    {}\n", num(f.epoch_millis)));
    out.push_str(&format!("    FILETIME      {}\n", num(f.filetime)));
    out.push_str(&format!("    RFC 1123      {}\n", f.rfc1123.unwrap_or_else(|| "(out of range)".into())));
    out
}

/// One step described in English for the report.
fn describe_step(step: &Step) -> String {
    match step {
        Step::Shift { sign, amount, unit } => {
            let s = match sign {
                Sign::Plus => "+",
                Sign::Minus => "-",
            };
            format!("shift {s}{amount} {}", unit.name())
        }
        Step::SetTime { hour, minute, second } => format!("set time {hour:02}:{minute:02}:{second:02}"),
        Step::Snap(t) => format!("snap {t}"),
        Step::Nearest(t) => format!("nearest {t}"),
        Step::Zone(t) => format!("zone {t}"),
    }
}

// ---------------------------------------------------------------------------
// Calendar loading (the data catalogue, docs/04 section 5)
// ---------------------------------------------------------------------------
//
// The consumer owns the I/O and serde; the core engine works over already-parsed rules.
// The JSON schema is the contract (docs/04 section 5); this is one reader of it. Unknown
// fields are ignored (additive evolution is safe); an unknown major schema version is refused.

#[derive(Deserialize)]
struct CalendarDto {
    schema: String,
    id: String,
    country: String,
    weekend: Vec<String>,
    observed: String,
    holidays: Vec<HolidayDto>,
}

#[derive(Deserialize)]
struct HolidayDto {
    id: String,
    name: NameDto,
    rule: RuleDto,
    #[serde(default)]
    valid_from: Option<i64>,
    #[serde(default)]
    valid_to: Option<i64>,
    source: String,
}

#[derive(Deserialize)]
struct NameDto {
    en: String,
    local: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RuleDto {
    Fixed { month: u32, day: u32 },
    NthWeekday { month: u32, weekday: String, order: i32 },
    EasterOffset { offset: i32 },
}

/// Map a weekday name to a Sunday-based index 0..=6.
fn weekday_index(name: &str) -> Result<u32, String> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "sunday" => 0,
        "monday" => 1,
        "tuesday" => 2,
        "wednesday" => 3,
        "thursday" => 4,
        "friday" => 5,
        "saturday" => 6,
        other => return Err(format!("unknown weekday '{other}'")),
    })
}

fn observed_from(s: &str) -> Result<chrono_core::calendar::Observed, String> {
    use chrono_core::calendar::Observed;
    Ok(match s {
        "none" => Observed::None,
        "sat_to_fri_sun_to_mon" => Observed::SatToFriSunToMon,
        "sun_to_mon" => Observed::SunToMon,
        "weekend_to_mon" => Observed::WeekendToMon,
        other => return Err(format!("unknown observed rule '{other}'")),
    })
}

fn rule_from(dto: RuleDto) -> Result<chrono_core::calendar::HolidayRule, String> {
    use chrono_core::calendar::HolidayRule;
    Ok(match dto {
        RuleDto::Fixed { month, day } => HolidayRule::Fixed { month, day },
        RuleDto::NthWeekday { month, weekday, order } => {
            HolidayRule::NthWeekday { month, weekday: weekday_index(&weekday)?, order }
        }
        RuleDto::EasterOffset { offset } => HolidayRule::EasterOffset { offset },
    })
}

/// Locate a calendar file: next to the executable (portable layout), else in ./calendars.
fn find_calendar_file(id: &str) -> Result<std::path::PathBuf, String> {
    let name = format!("{id}.json");
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("calendars").join(&name));
        }
    }
    candidates.push(std::path::Path::new("calendars").join(&name));
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| format!("calendar '{id}' not found (looked in <exe>/calendars and ./calendars)"))
}

/// Load and validate a calendar by id, mapping the JSON schema to the core engine's types.
fn load_calendar(id: &str) -> Result<chrono_core::calendar::Calendar, String> {
    let path = find_calendar_file(id)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let dto: CalendarDto =
        serde_json::from_str(&text).map_err(|e| format!("bad calendar JSON in {}: {e}", path.display()))?;
    // An unknown major schema version is refused, not half-understood (docs/04 section 3.1).
    if dto.schema != "chronomock.calendar/1" {
        return Err(format!(
            "unsupported calendar schema '{}' (this build reads chronomock.calendar/1)",
            dto.schema
        ));
    }
    let weekend = dto.weekend.iter().map(|w| weekday_index(w)).collect::<Result<Vec<_>, _>>()?;
    let holidays = dto
        .holidays
        .into_iter()
        .map(|h| {
            Ok(chrono_core::calendar::Holiday {
                id: h.id,
                name_en: h.name.en,
                name_local: h.name.local,
                rule: rule_from(h.rule)?,
                valid_from: h.valid_from,
                valid_to: h.valid_to,
                source: h.source,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(chrono_core::calendar::Calendar {
        id: dto.id,
        country: dto.country,
        weekend,
        observed: observed_from(&dto.observed)?,
        holidays,
    })
}

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
/// Coverage is reliable (never coalesced) - one event per process, never summed.
fn emit_coverage(pid: u32, cov: &chrono_core::Coverage) {
    let to_wire = |cs: &[chrono_core::ChannelCoverage]| -> Vec<CoveredChannel> {
        cs.iter()
            .map(|c| CoveredChannel { channel: c.channel.clone(), calls: c.calls })
            .collect()
    };
    emit(&Event::Coverage {
        v: PROTOCOL_VERSION,
        pid,
        covered: to_wire(&cov.covered),
        observed: to_wire(&cov.observed),
        uncovered: cov.uncovered.clone(),
        warning_keys: cov.warning_keys.clone(),
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
    let n = reader.read_line(&mut line).unwrap_or(0);
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

    let (target, time) = match cmd {
        Command::Start { target, time, .. } => (target, time),
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
            emit_coverage(prepared.session.pid, &prepared.coverage);

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
            emit(&Event::Verdict {
                v: PROTOCOL_VERSION,
                id: Some(1),
                verdict: verdict.wire().into(),
                refuse_start: false,
                reason_key: reason_key.into(),
            });
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
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break, // EOF or error: dropping tx signals Disconnected
                Ok(_) => {
                    if let Ok(cmd) = parse_command(line.trim_end()) {
                        if tx.send(cmd).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    // Report any child that already joined during the guard window, before the first
    // heartbeat, so a fast child does not wait a whole second to appear. Seed the family
    // roll-up with the parent verdict, then fold each child's verdict as it joins.
    let mut family = verdict;
    let mut family_pids: HashSet<u32> = HashSet::new();
    fold_children(&mut session, &mut family, &mut family_pids);

    let heartbeat = Duration::from_secs(1);
    let mut deadline = Instant::now() + heartbeat;
    let mut target_exit: Option<i32> = None;

    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(Command::End { .. }) => break,
            Ok(Command::Query { id, .. }) => {
                emit(&state_event(&session));
                emit(&Event::Ack { v: PROTOCOL_VERSION, id });
            }
            Ok(Command::SetMultiplier { id, multiplier, .. }) => {
                session.set_multiplier(multiplier);
                emit(&Event::Ack { v: PROTOCOL_VERSION, id });
                emit(&state_event(&session));
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
            Ok(_) => {} // Start or a not-yet-supported command: ignore
            Err(mpsc::RecvTimeoutError::Timeout) => {
                emit(&state_event(&session));
                fold_children(&mut session, &mut family, &mut family_pids);
                if !session.is_alive() {
                    target_exit = session.exit_code();
                    break;
                }
                deadline = Instant::now() + heartbeat;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break, // stdin closed
        }
    }

    // Final fold so a child that joined since the last heartbeat still counts in the family.
    fold_children(&mut session, &mut family, &mut family_pids);
    // Capture the session clocks before ending so `ended` can state the duration and the fake wall
    // clock reached - reliably, even for a session too short to have emitted a heartbeat.
    let final_state = session.state();
    session.end();
    emit(&Event::SessionVerdict {
        v: PROTOCOL_VERSION,
        verdict: family.wire().into(),
        reason_key: session_reason_key(family).into(),
        process_count: 1 + family_pids.len() as u32,
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
        emit_coverage(pid, &cov);
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
    let s = session.state();
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
    Some(exe.parent()?.join("chrono_hook.dll"))
}

fn build_spec(time: &TimeSpec) -> Result<SessionSpec, (i32, &'static str)> {
    let mode = match time.mode.as_str() {
        "flow" => TimeMode::Flow,
        "frozen" => TimeMode::Frozen,
        "multiplier" => TimeMode::Multiplier(time.multiplier.unwrap_or(1)),
        _ => return Err((1, "time.bad_mode")),
    };
    Ok(SessionSpec {
        moment: Moment {
            local: time.moment.local.clone().unwrap_or_default(),
            tz_bias_min: time.moment.tz_bias_min,
        },
        mode,
        scale_duration: time.scale_duration,
    })
}

fn map_prepare_error(e: chrono_mech::PrepareError) -> (i32, &'static str, &'static str, String) {
    use chrono_mech::PrepareError as P;
    match e {
        P::Moment(m) => (1, "moment.invalid", "core", m),
        P::Control(m) => (3, "session.control_failed", "mechanism", m),
        P::Launch(m) => (2, "target.launch_failed", "mechanism", m),
        P::Inject(m) => (2, "target.inject_failed", "mechanism", m),
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

    #[test]
    fn mode_flow_and_frozen_carry_no_multiplier() {
        assert_eq!(parse_mode("flow").unwrap(), ("flow".to_string(), None));
        assert_eq!(parse_mode("frozen").unwrap(), ("frozen".to_string(), None));
    }

    #[test]
    fn mode_accepts_xn_either_case() {
        assert_eq!(parse_mode("x60").unwrap(), ("multiplier".to_string(), Some(60)));
        assert_eq!(parse_mode("X1440").unwrap(), ("multiplier".to_string(), Some(1440)));
    }

    #[test]
    fn mode_rejects_unknown_and_nonpositive() {
        assert!(parse_mode("fast").is_err());
        assert!(parse_mode("x0").is_err());
        assert!(parse_mode("x-5").is_err());
        assert!(parse_mode("xabc").is_err());
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
    fn at_relative_business_days_not_built_is_an_error() {
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 25, hour: 0, minute: 0, second: 0 };
        let err = resolve_relative_at("+5bd", now).unwrap_err();
        assert!(err.contains("not built yet"), "honest not-built message, got: {err}");
    }

    fn empty_report() -> SessionReport {
        SessionReport {
            target: "app.exe".into(),
            session_verdict: None,
            parent_verdict: None,
            vanished: None,
            warnings: vec![],
            uncovered: vec![],
            covered: vec![],
            observed: vec![],
            timing: None,
        }
    }

    #[test]
    fn works_headline_is_scannable() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 2)),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("WORKS"), "got:\n{out}");
        assert!(out.contains("processes: 2"), "got:\n{out}");
        assert!(out.contains("saw the session clock"), "got:\n{out}");
    }

    #[test]
    fn vanish_reads_as_not_taking_effect() {
        let r = SessionReport {
            vanished: Some(("target.single_instance_suspected".into(), 1500)),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("DID NOT TAKE EFFECT"), "got:\n{out}");
        assert!(out.contains("vanished"), "got:\n{out}");
    }

    #[test]
    fn uncovered_and_warnings_are_surfaced_unknown_key_verbatim() {
        let r = SessionReport {
            session_verdict: Some(("partial".into(), "session.family_partial".into(), 1)),
            uncovered: vec![(1234, "KUSER_SHARED_DATA".into())],
            warnings: vec!["wait.object_waits_not_scaled".into(), "some.unknown_key".into()],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("PARTIAL"), "got:\n{out}");
        assert!(out.contains("pid 1234: KUSER_SHARED_DATA"), "got:\n{out}");
        assert!(out.contains("object waits are hooked"), "got:\n{out}");
        // An unknown warning key is shown verbatim - we never invent an explanation.
        assert!(out.contains("some.unknown_key"), "got:\n{out}");
    }

    #[test]
    fn covered_and_observed_channels_are_shown_with_counts_per_pid() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            covered: vec![(1234, "GetSystemTime".into(), 7)],
            observed: vec![(1234, "WaitForSingleObject".into(), 1)],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("covered channels"), "got:\n{out}");
        assert!(out.contains("pid 1234: GetSystemTime (7 calls)"), "got:\n{out}");
        assert!(out.contains("observed channels (hooked but left real)"), "got:\n{out}");
        // Singular reads correctly (1 call, not "1 calls").
        assert!(out.contains("pid 1234: WaitForSingleObject (1 call)"), "got:\n{out}");
    }

    #[test]
    fn evidence_from_works_has_no_unreliable_banner_and_echoes_params() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            covered: vec![(1, "GetSystemTime".into(), 2)],
            ..empty_report()
        };
        let p = EvidenceParams {
            moment: "2038-01-19T03:14:07".into(),
            zone: "+00:00".into(),
            mode: "x60".into(),
        };
        let out = render_evidence(&r, &p);
        assert!(!out.contains("UNRELIABLE"), "a works session must not be flagged, got:\n{out}");
        assert!(out.contains("WORKS"), "got:\n{out}");
        assert!(
            out.contains("requested:  2038-01-19T03:14:07 (zone +00:00, mode x60)"),
            "got:\n{out}"
        );
    }

    #[test]
    fn evidence_from_vanish_leads_with_the_unreliable_banner() {
        let r = SessionReport {
            vanished: Some(("target.single_instance_suspected".into(), 1500)),
            ..empty_report()
        };
        let p = EvidenceParams {
            moment: "2038-01-19T03:14:07".into(),
            zone: "+00:00".into(),
            mode: "flow".into(),
        };
        let out = render_evidence(&r, &p);
        assert!(out.starts_with("!! UNRELIABLE EVIDENCE"), "non-works must lead with the banner, got:\n{out}");
        assert!(out.contains("DID NOT TAKE EFFECT"), "got:\n{out}");
    }

    #[test]
    fn format_bias_maps_common_zones() {
        assert_eq!(format_bias(0), "+00:00");
        assert_eq!(format_bias(-120), "+02:00"); // UTC+2
        assert_eq!(format_bias(300), "-05:00"); // UTC-5
    }

    #[test]
    fn session_timing_section_shows_reached_clock_and_elapsed() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            timing: Some(("2038-01-19 03:15:07".into(), 1500, 90000)),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("session:  fake clock reached 2038-01-19 03:15:07"), "got:\n{out}");
        // 1.5s real mapped to 90s fake - the x60 acceleration is visible in the report.
        assert!(out.contains("real elapsed 1.5s, fake elapsed 90.0s"), "got:\n{out}");
    }

    // --- calc surface ---------------------------------------------------------

    #[test]
    fn parse_shift_reads_sign_amount_and_unit() {
        assert_eq!(parse_shift("+18years").unwrap(), Step::Shift { sign: Sign::Plus, amount: 18, unit: Unit::Years });
        assert_eq!(parse_shift("-1d").unwrap(), Step::Shift { sign: Sign::Minus, amount: 1, unit: Unit::Days });
        assert_eq!(parse_shift("+2q").unwrap(), Step::Shift { sign: Sign::Plus, amount: 2, unit: Unit::Quarters });
    }

    #[test]
    fn shift_unit_m_is_minutes_mo_is_months() {
        // The collision that would silently corrupt every month calc if conflated.
        assert_eq!(parse_shift("+5m").unwrap(), Step::Shift { sign: Sign::Plus, amount: 5, unit: Unit::Minutes });
        assert_eq!(parse_shift("+5mo").unwrap(), Step::Shift { sign: Sign::Plus, amount: 5, unit: Unit::Months });
    }

    #[test]
    fn parse_shift_rejects_bad_shapes() {
        assert!(parse_shift("18years").is_err()); // no sign
        assert!(parse_shift("+years").is_err()); // no number
        assert!(parse_shift("+18zz").is_err()); // unknown unit
        assert!(parse_shift("+").is_err()); // sign only
    }

    #[test]
    fn parse_base_reads_keywords_and_absolute() {
        assert_eq!(parse_base("today").unwrap(), Base::Today);
        assert_eq!(parse_base("now").unwrap(), Base::Now);
        assert!(matches!(parse_base("2025-01-31T12:00:00").unwrap(), Base::Absolute(_)));
        assert!(parse_base("2025-02-31T00:00:00").is_err()); // impossible day rejected, not normalized
    }

    #[test]
    fn parse_set_time_reads_hms() {
        assert_eq!(parse_set_time("23:59:59").unwrap(), Step::SetTime { hour: 23, minute: 59, second: 59 });
        assert!(parse_set_time("23:59").is_err()); // wrong shape
        assert!(parse_set_time("aa:bb:cc").is_err()); // non-numeric
    }

    #[test]
    fn calc_exit_codes_split_bad_input_from_not_built() {
        // Not built in this release -> its own code 5; bad input -> usage 1.
        assert_eq!(calc_error_exit_code(&EvalError::StepUnsupported { kind: "snap", index: 0 }), 5);
        assert_eq!(calc_error_exit_code(&EvalError::UnitUnsupported { unit: "business_days", index: 0 }), 5);
        assert_eq!(calc_error_exit_code(&EvalError::Overflow { index: 0 }), 1);
        assert_eq!(calc_error_exit_code(&EvalError::BadSetTime { index: 0 }), 1);
    }

    #[test]
    fn calc_error_message_carries_key_and_one_based_step() {
        let msg = describe_calc_error(&EvalError::UnitUnsupported { unit: "business_days", index: 0 });
        assert!(msg.contains("step 1"), "1-based step number, got: {msg}");
        assert!(msg.contains("calc.unit_unsupported"), "stable key, got: {msg}");
        let msg = describe_calc_error(&EvalError::StepUnsupported { kind: "snap", index: 2 });
        assert!(msg.contains("step 3") && msg.contains("calc.step_unsupported"), "got: {msg}");
    }

    #[test]
    fn calc_parse_to_eval_end_to_end_absolute_base() {
        // The whole CLI path minus the system clock: parse flags, evaluate, check the
        // month clamp that proves the model beats a fixed-tick delta.
        let ca = parse_calc_args(&[
            "--base".into(),
            "2025-01-31T12:00:00".into(),
            "--shift".into(),
            "+1mo".into(),
        ])
        .unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, calendar: None }).unwrap();
        assert_eq!(out.result().to_iso(), "2025-02-28T12:00:00");
    }

    #[test]
    fn render_calc_shows_base_steps_and_result() {
        let ca = parse_calc_args(&[
            "--base".into(),
            "2008-08-04T00:00:00".into(),
            "--shift".into(),
            "-18years".into(),
            "--shift".into(),
            "-1d".into(),
            "--set-time".into(),
            "23:59:59".into(),
        ])
        .unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, None, &now, None);
        assert!(text.contains("base:    2008-08-04T00:00:00"), "got:\n{text}");
        assert!(text.contains("step 1:  shift -18 years  -> 1990-08-04T00:00:00"), "got:\n{text}");
        assert!(text.contains("step 3:  set time 23:59:59  -> 1990-08-03T23:59:59"), "got:\n{text}");
        assert!(text.contains("result:  1990-08-03T23:59:59"), "got:\n{text}");
    }

    #[test]
    fn calc_default_base_is_today_with_no_steps() {
        // The degenerate-but-legal input: `chrono calc` with no args -> today, no steps.
        let ca = parse_calc_args(&[]).unwrap();
        assert_eq!(ca.base, Base::Today);
        assert!(ca.steps.is_empty());
    }

    #[test]
    fn render_calc_includes_the_formats_block() {
        let ca = parse_calc_args(&["--base".into(), "1970-01-01T00:00:00".into()]).unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, Some(0), &now, None);
        assert!(text.contains("formats:"), "got:\n{text}");
        assert!(text.contains("ISO datetime  1970-01-01T00:00:00+00:00"), "got:\n{text}");
        assert!(text.contains("US            01/01/1970"), "got:\n{text}");
        assert!(text.contains("epoch (s)     0"), "got:\n{text}");
        assert!(text.contains("FILETIME      116444736000000000"), "got:\n{text}");
        assert!(text.contains("RFC 1123      Thu, 01 Jan 1970 00:00:00 GMT"), "got:\n{text}");
    }

    #[test]
    fn render_calc_includes_the_metadata_block() {
        let ca = parse_calc_args(&["--base".into(), "2026-01-01T00:00:00".into()]).unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        // A fixed "today" makes days-from-now deterministic: 2026-01-01 is 9 days before 2026-01-10.
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 10, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, Some(0), &now, None);
        assert!(text.contains("metadata:"), "got:\n{text}");
        assert!(text.contains("weekday       Thursday"), "got:\n{text}"); // 2026-01-01 is a Thursday
        assert!(text.contains("ISO week      2026-W01"), "got:\n{text}");
        assert!(text.contains("US week       1"), "got:\n{text}");
        assert!(text.contains("quarter       Q1"), "got:\n{text}");
        assert!(text.contains("days from now -9 days"), "got:\n{text}");
    }

    fn test_calendar() -> chrono_core::calendar::Calendar {
        use chrono_core::calendar::{Calendar, Holiday, HolidayRule, Observed};
        Calendar {
            id: "us-test".into(),
            country: "US".into(),
            weekend: vec![0, 6],
            observed: Observed::SunToMon,
            holidays: vec![Holiday {
                id: "independence_day".into(),
                name_en: "Independence Day".into(),
                name_local: "Independence Day".into(),
                rule: HolidayRule::Fixed { month: 7, day: 4 },
                valid_from: None,
                valid_to: None,
                source: "test".into(),
            }],
        }
    }

    #[test]
    fn render_metadata_with_calendar_shows_business_day_and_holiday() {
        let civil = chrono_core::calc::CivilDateTime { year: 2026, month: 7, day: 4, hour: 0, minute: 0, second: 0 };
        let cal = test_calendar();
        let text = render_metadata(&civil, &civil, Some(&cal));
        assert!(text.contains("holiday       Independence Day"), "got:\n{text}");
        // 2026-07-04 is a Saturday, so it is not a business day.
        assert!(text.contains("business day  no  (us-test)"), "got:\n{text}");
    }

    #[test]
    fn calendar_loader_maps_weekdays_and_observed() {
        assert_eq!(weekday_index("Monday").unwrap(), 1);
        assert_eq!(weekday_index("sunday").unwrap(), 0);
        assert!(weekday_index("funday").is_err());
        assert!(matches!(observed_from("sun_to_mon").unwrap(), chrono_core::calendar::Observed::SunToMon));
        assert!(observed_from("whenever").is_err());
    }
}
