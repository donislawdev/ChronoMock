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
use std::process::{Command as PCommand, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono_core::calc::{Base, EvalContext, EvalError, MomentExpr, Sign, Step};
use chrono_core::{filetime_utc_to_wall, verdict_from_coverage, Moment, SessionSpec, TimeMode, Verdict};
use chrono_proto::{
    parse_command, Clock, Command, CoveredChannel, Event, MomentSpec, TargetSpec, TimeSpec,
    PROTOCOL_VERSION,
};

/// The Chromium/Electron substitution mechanism (CDP faketime), a parallel path to the native core.
mod cdp;
/// Reading a shipped calendar catalogue and validating it.
mod calendar;
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
/// Test-only helpers shared by more than one module.
#[cfg(test)]
mod testutil;
/// One NDJSON line off the machine protocol, bounded.
mod wire;
/// Session zone and instant conversions (untouchable rule 2).
mod zone;

use calendar::load_calendar;
use cdp_probe::{cdp_date_probe, cdp_launch_probe, cdp_probe, cdp_shim_probe};
use cdp_session::cdp_session;
use cli::{print_calc_usage, print_usage, this_bitness, CORE_VERSION};
use grammar::{parse_base, parse_nearest, parse_set_time, parse_shift, parse_snap};
use preset::{
    load_preset, preset_targets_calculator, preset_targets_substitution,
    read_target_creation_date, resolve_moment, resolve_parameters,
};
use wire::read_protocol_line;
use zone::{format_bias, now_filetime_utc, parse_zone_to_bias, session_zone_default};

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

/// How long `chrono run` waits for ANY event from the core before calling it hung.
///
/// Deliberately the same 15 s the GUI uses (`SessionViewModel.IdleTimeout`), because it is the same
/// core with the same internal deadlines behind it - and those deadlines are checked against this
/// number by `RustTimeoutMirrorTests`. The core beats `state` about once a real second in every
/// mode, so 15 s is fifteen missed heartbeats.
const DRIVER_IDLE_TIMEOUT_SECS: u64 = 15;


















// ---------------------------------------------------------------------------
// Driver: `chrono run ...`
// ---------------------------------------------------------------------------

struct RunArgs {
    target: String,
    args: Vec<String>,
    /// Working directory for the target (`--cwd`). `None` means "do not ask for one", and the target
    /// then inherits ours - the behaviour every run had before this flag existed.
    cwd: Option<String>,
    at: Option<String>,
    zone_bias_min: Option<i32>,
    /// Wire mode token: "flow", "frozen", or "multiplier".
    mode: String,
    multiplier: Option<i64>,
    scale_duration: bool,
    /// Also scale QueryPerformanceCounter (ADR-2 reversal, opt-in `--scale-qpc`).
    scale_qpc: bool,
    /// Run even when the opening verdict says the substitution did not take effect (`--force`).
    force: bool,
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
    /// A named preset id (docs/04 4.3): the moment AND the time mode come from `presets/<id>.json`
    /// instead of --at/--mode. None = build them from the flags. Exclusive of --at/--mode/--scale-duration.
    preset: Option<String>,
    /// Preset parameter values from `--param id=value` (docs/04 4.2). Only meaningful with --preset.
    /// In run, a `target_file_creation` hint also resolves from the target's file date.
    params: HashMap<String, String>,
    /// Give up after this many seconds of wall time, whatever the session is doing (`--timeout`).
    /// None = no ceiling, which stays the default because the normal way to bound a run is
    /// `--ticks`, and a session driving a real app has no business being cut off by surprise.
    timeout_secs: Option<u64>,
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
            // The friendly surface keeps its own floor of 1: `x0` here would be a confusing way to
            // spell `--mode frozen`, which already exists. The ceiling is the shared one, so the CLI
            // and the protocol agree on what a session may run at.
            if m < 1 {
                return Err(format!("multiplier must be >= 1, got '{raw}'"));
            }
            if m > chrono_core::MULTIPLIER_MAX {
                return Err(format!(
                    "multiplier must be <= {}, got '{raw}' - past that the fake clock leaves the \
                     representable date range mid-session and the time channels start disagreeing",
                    chrono_core::MULTIPLIER_MAX
                ));
            }
            Ok(("multiplier".into(), Some(m)))
        }
    }
}

/// Split an `--args` value into arguments, honouring double-quotes so one argument may contain
/// spaces (`--args '"a b" c'` -> ["a b", "c"]). Whitespace outside quotes separates arguments; a
/// quote toggles quoting and is dropped. Plain space-separated values behave exactly as before, so
/// existing usage is unchanged; `mech`'s quoting re-quotes each token for the target's CRT (P9).
fn split_args(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    for c in raw.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

/// What the driver puts in `start.target`. Separated from the command so a test can see it: the
/// three fields used to be written inline with `cwd` hard-coded to `None`, and putting that back
/// changed nothing any test could notice - measured, by reverting it.
fn target_spec_for(ra: &RunArgs) -> TargetSpec {
    TargetSpec {
        path: ra.target.clone(),
        args: ra.args.clone(),
        cwd: ra.cwd.clone(),
    }
}

fn parse_run_args(argv: &[String]) -> Result<RunArgs, String> {
    let mut target: Option<String> = None;
    let mut args: Vec<String> = Vec::new();
    let mut cwd: Option<String> = None;
    let mut at: Option<String> = None;
    let mut zone_bias_min: Option<i32> = None;
    let mut mode = String::from("flow");
    let mut multiplier: Option<i64> = None;
    let mut scale_duration = false;
    let mut scale_qpc = false;
    let mut force = false;
    let mut ticks: u64 = 0;
    let mut timeout_secs: Option<u64> = None;
    let mut set_after: Option<(u64, i64)> = None;
    let mut jump_after: Option<(u64, String)> = None;
    let mut json = false;
    let mut report: Option<String> = None;
    let mut preset: Option<String> = None;
    let mut params: HashMap<String, String> = HashMap::new();
    // Whether any moment/mode flag appeared, so `--preset` (which supplies both) can reject being
    // combined with them instead of silently ignoring one source.
    let mut saw_time_flag = false;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--at" => {
                i += 1;
                at = Some(argv.get(i).ok_or("--at needs a value")?.clone());
                saw_time_flag = true;
            }
            "--preset" => {
                i += 1;
                preset = Some(argv.get(i).ok_or("--preset needs an id like month-end")?.clone());
            }
            "--param" => {
                i += 1;
                let raw = argv.get(i).ok_or("--param needs id=value like start_date=2026-01-01")?;
                let (id, value) = raw
                    .split_once('=')
                    .ok_or_else(|| format!("--param must be id=value, got '{raw}'"))?;
                if id.is_empty() {
                    return Err(format!("--param needs a non-empty id, got '{raw}'"));
                }
                params.insert(id.to_string(), value.to_string());
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
                saw_time_flag = true;
            }
            "--args" => {
                i += 1;
                let raw = argv.get(i).ok_or("--args needs a value")?;
                args = split_args(raw);
            }
            "--cwd" => {
                i += 1;
                let raw = argv.get(i).ok_or("--cwd needs a value")?;
                // An explicitly empty value is a usage error, not "no directory". The two mean
                // different things on the wire (absent vs present-and-empty) and only one of them is
                // something CreateProcessW can be given.
                if raw.trim().is_empty() {
                    return Err("--cwd needs a directory - omit the flag to start where the tool does".into());
                }
                cwd = Some(raw.clone());
            }
            "--scale-duration" => {
                scale_duration = true;
                saw_time_flag = true;
            }
            "--force" => {
                force = true;
            }
            "--scale-qpc" => {
                // ADR-2 reversal, opt-in. NOT a preset-exclusive time flag: a preset carries its own
                // scale_duration but never scale_qpc, so --scale-qpc is the only source and composes with
                // --preset (unlike --scale-duration, which would double a preset's own setting).
                scale_qpc = true;
            }
            "--ticks" => {
                i += 1;
                let raw = argv.get(i).ok_or("--ticks needs a value")?;
                ticks = raw.parse().map_err(|_| format!("bad --ticks value '{raw}'"))?;
            }
            "--timeout" => {
                i += 1;
                let raw = argv.get(i).ok_or("--timeout needs a value in seconds")?;
                let secs: u64 = raw.parse().map_err(|_| format!("bad --timeout value '{raw}'"))?;
                if secs == 0 {
                    return Err("--timeout must be at least 1 second".into());
                }
                timeout_secs = Some(secs);
            }
            "--set-after" => {
                i += 1;
                let raw = argv.get(i).ok_or("--set-after needs <tick>:<multiplier>")?;
                let (t, m) = raw
                    .split_once(':')
                    .ok_or("--set-after must be <tick>:<multiplier>")?;
                let tick: u64 = t.parse().map_err(|_| format!("bad tick in '{raw}'"))?;
                let mult: i64 = m.parse().map_err(|_| format!("bad multiplier in '{raw}'"))?;
                // Same invariant as --mode xN (parse_mode): an in-flight multiplier is >= 1. A zero or
                // negative value would freeze or run the wall clock backward as a silent side effect
                // of an unvalidated surface (rule 4); freezing in flight is not a feature here.
                if mult < 1 {
                    return Err(format!("--set-after multiplier must be >= 1, got '{raw}'"));
                }
                if mult > chrono_core::MULTIPLIER_MAX {
                    return Err(format!(
                        "--set-after multiplier must be <= {}, got '{raw}'",
                        chrono_core::MULTIPLIER_MAX
                    ));
                }
                set_after = Some((tick, mult));
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

    // A preset supplies both the moment and the time mode, so combining it with --at/--mode/
    // --scale-duration would mean two sources. Reject it rather than pick one silently.
    if preset.is_some() && saw_time_flag {
        return Err("--preset supplies the moment and mode; it cannot be combined with \
                    --at/--mode/--scale-duration"
            .into());
    }
    // --param only makes sense with --preset (it fills a preset's declared parameters).
    if !params.is_empty() && preset.is_none() {
        return Err("--param needs --preset (parameters belong to a preset)".into());
    }

    Ok(RunArgs {
        target: target.ok_or("missing <target>")?,
        args,
        cwd,
        at,
        zone_bias_min,
        mode,
        multiplier,
        scale_duration,
        scale_qpc,
        force,
        ticks,
        timeout_secs,
        set_after,
        jump_after,
        report,
        json,
        preset,
        params,
    })
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
fn describe_at_error(e: EvalError) -> String {
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
        EvalError::NeedsCalendar { .. } => "moment.needs_calendar",
        _ => "moment.invalid",
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
fn driver_exit_code(core_code: Option<i32>) -> i32 {
    match core_code {
        Some(c) if matches!(c, 0 | 1 | 2 | 3 | 4 | 5 | 6 | 10 | 11 | 12) => c,
        _ => 3,
    }
}

/// One process's channel coverage as of the latest `coverage` event for its pid. Kept per process
/// and replaced rather than appended, because the core reports a process more than once: a first
/// snapshot when it is discovered, and a final one when the session ends (R2-X8).
struct ProcessCoverage {
    covered: Vec<chrono_proto::CoveredChannel>,
    observed: Vec<chrono_proto::CoveredChannel>,
    uncovered: Vec<String>,
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
    /// Error events the core emitted, as (key, origin). The `--json` surface always carried these;
    /// the human report used to drop them on the floor and print `<no verdict emitted>`, so a session
    /// that never started said nothing about WHY (untouchable rule 6). A start failure becomes the
    /// headline; anything the core rejected mid-session (an out-of-range `set_multiplier`, which does
    /// not end the session) is listed below the verdict instead of vanishing.
    errors: Vec<(String, String)>,
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
    /// Session duration as the core states it in `ended`: (fake wall reached, real ms elapsed,
    /// fake ms elapsed), or None when `ended` carried no end wall (a session that never started).
    /// Authoritative, not sampled from the heartbeats - one source of truth (3d35a79).
    timing: Option<(String, i64, i64)>,
    /// The target's own exit code from `ended.target_exit_code` - present only when the app exited
    /// on its own. None for a `--ticks` cutoff (the target is still running), for a CDP session (we
    /// close our own instance), and for a session that never started. Informational: it is NOT this
    /// tool's exit code, which is the session verdict (docs/08 section 8).
    target_exit: Option<i32>,
    /// What teardown could not remove, from `ended.residue_keys`. Empty on a native session (its
    /// hooks unhook themselves); today the only key is a Chromium temp profile that stayed locked.
    residue: Vec<String>,
    /// Whether this was a Chromium (CDP) session: its coverage unit is a JS context, not an OS
    /// process, so the report says "context" instead of "pid".
    cdp: bool,
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
        "chromium.contexts_covered" => "every JS context ran on the session clock",
        "chromium.contexts_partial" => "some JS contexts ran on the session clock, some could not be reached",
        "chromium.no_time_calls" => "the shim was installed, but the app called no JS time API",
        "chromium.no_contexts" => "no JS context could be shimmed",
        _ => "",
    }
}

/// English gloss for an error key the core emitted. Same contract as `describe_reason`: an unknown
/// key yields "" and the caller falls back to the raw key, so a core newer than this driver still
/// says something true rather than something invented.
///
/// Whose fault it is matters more here than anywhere else in the report. `core.hook_dll_missing` and
/// `session.control_failed` are OURS (a broken install, a broken session), `target.launch_failed` is
/// the target's, and `target.inject_failed` is genuinely ambiguous - which is exactly why the missing
/// DLL had to stop being reported as that one (R2-W3).
fn describe_error(key: &str) -> &'static str {
    match key {
        "core.hook_dll_missing" => {
            "chrono_hook.dll is missing next to chrono.exe - this Chrono Mock installation is incomplete"
        }
        "session.control_failed" => "the session's control memory could not be set up",
        "session.already_active" => "another Chrono Mock session is already running - one at a time",
        "target.launch_failed" => "the target application could not be started",
        "target.cwd_missing" => "the working directory asked for does not exist",
        "target.inject_failed" => "the hook could not be injected into the target",
        "target.bitness_mismatch" => {
            "the target and this chrono.exe are different bitness - run the chrono.exe that matches the target"
        }
        "target.attach_failed" => "the target could not be attached to (Chromium/Electron, CDP)",
        "moment.invalid" => "the requested moment is not a valid date and time",
        "time.bad_mode" => "the requested time mode is not one this core knows",
        "time.bad_multiplier" => "the requested speed is outside the range this core accepts",
        "protocol.version_mismatch" => "the client and the core speak different protocol versions",
        "protocol.no_command" | "protocol.bad_command" | "protocol.expected_start" => {
            "the core did not receive a usable start command"
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
        "coverage.pid_registry_full" => {
            "this session ran more processes than the audit can track (256), so some ran uncovered and are missing from the process count and the channel lists below"
        }
        "time.fake_clock_clamped" => {
            "the fake clock reached the last date this build can represent (year 30828) and stood there for the rest of the session, so late readings are not the moments the rate would have produced"
        }
        "inheritance.child_not_injected" => {
            "a child process could not be covered and ran on the REAL clock - usually a child of the other bitness; the process count below is short by that many"
        }
        "source.network_at_start" => {
            "the target opened a network connection - it may read time from a server, which no local hook can cover"
        }
        // Says what the port MEANS, not just that there is one. Chromium's debugging port listens on
        // loopback with no authentication, so for as long as the session runs, any other process on
        // this machine can attach to it and drive the app - read its pages, run JavaScript in it,
        // navigate it. That is a fact about the session the tester is entitled to before they point
        // this at something that matters, and the previous wording read as a note about tidiness.
        "chromium.launched_with_debug_port" => {
            "an Electron/Chromium app: launched with a remote-debugging port and a clean isolated profile, not your real one - while the session runs, any other local process can use that port to control the app"
        }
        "chromium.app_closed_before_audit" => {
            "the app closed before the audit could read final call counts - the coverage below may be incomplete"
        }
        "chromium.rate_change_affects_running_timers" => {
            "the speed changed in flight: new timers and the clock reflect it at once, but a setInterval already running keeps its old cadence"
        }
        "runtime.python_monotonic_qpc" => {
            "this Python app measures time with perf_counter and monotonic - both use QueryPerformanceCounter on Python 3.13+, which is left real, so a timer built on them does not scale (time.time and the wall clock do)"
        }
        "runtime.python_perfcounter_qpc" => {
            "this Python app's perf_counter uses QueryPerformanceCounter, which is left real, so a perf_counter timer does not scale - on Python 3.13+ monotonic uses QPC too (time.time and the wall clock do scale)"
        }
        "runtime.dotnet_stopwatch_qpc" => {
            "this .NET app's Stopwatch uses QueryPerformanceCounter, which is left real, so a Stopwatch timer does not scale (DateTime and Environment.TickCount do)"
        }
        "runtime.java_nanotime_qpc" => {
            "this Java app's System.nanoTime uses QueryPerformanceCounter, which is left real, so a nanoTime timer does not scale (System.currentTimeMillis does)"
        }
        "qpc.scaled_render_may_distort" => {
            "QueryPerformanceCounter is being scaled (--scale-qpc), so a QPC-timed monotonic/elapsed clock accelerates - but a target that times its rendering or animation off QPC may look distorted"
        }
        _ => "",
    };
    if text.is_empty() {
        key.to_string()
    } else {
        format!("{text} ({key})")
    }
}

/// English text for a cleanup residue key from `ended.residue_keys`. Same shape as
/// `describe_warning`: a known key gets prose plus the key, an unknown one is printed literally
/// rather than swallowed, because a key we cannot name is still a mess we left (rule 6).
fn describe_residue(key: &str) -> String {
    let text = match key {
        "cleanup.chromium_profile_left" => {
            "the temporary Chromium profile could not be removed - delete it by hand if it lingers in your temp folder"
        }
        _ => "",
    };
    if text.is_empty() {
        key.to_string()
    } else {
        format!("{text} ({key})")
    }
}

/// Human label for the target's own exit code. `GetExitCodeProcess` yields a u32 that the wire
/// carries as an i32, so a crash arrives as a large negative number - an access violation reads as
/// -1073741819, which tells a tester nothing. A negative code therefore also gets the hexadecimal
/// form (0xC0000005 there), which is the one that can be looked up.
fn exit_code_label(code: i32) -> String {
    if code < 0 {
        format!("{code} (0x{:08X})", code as u32)
    } else {
        code.to_string()
    }
}

/// Statically detect the target's language runtime and warn that its monotonic/elapsed clocks stand on
/// QueryPerformanceCounter, which is left real (ADR-2) and so does not scale - the failure the user hit
/// when a Python/.NET/Java timer stayed still under a fast clock. This reads only file NAMES beside the
/// target (and its PyInstaller `_internal/` folder): no QPC hook (ADR-2 holds), no process inspection.
/// Best-effort: a runtime unpacked at runtime (PyInstaller onefile) or launched as `java -jar` is not
/// caught here, and a false positive only adds a "may not scale" note, never a false verdict (rules 4, 6).
fn detect_runtime_warnings(target_path: &std::path::Path, scale_qpc: bool) -> Vec<String> {
    // Under --scale-qpc the QPC axis IS scaled (A1), so a "monotonic/elapsed does not scale" warning would
    // be WRONG (B1 suppressed - it would contradict the feature). Instead warn once that scaling QPC can
    // distort a target that times its rendering off QPC (games, animation) - the render risk ADR-2 guarded
    // against. This holds for any target, so it does not depend on the runtime fingerprint below.
    if scale_qpc {
        return vec!["qpc.scaled_render_may_distort".to_string()];
    }

    fn add(keys: &mut Vec<String>, key: &str) {
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }

    let mut keys: Vec<String> = Vec::new();

    // The target executable's own name is a strong signal - a plain interpreter launcher.
    if let Some(name) = target_path.file_name().and_then(|n| n.to_str()) {
        match name.to_ascii_lowercase().as_str() {
            "python.exe" | "pythonw.exe" => add(&mut keys, "runtime.python_perfcounter_qpc"),
            "java.exe" | "javaw.exe" => add(&mut keys, "runtime.java_nanotime_qpc"),
            _ => {}
        }
    }

    // Runtime DLLs and manifests shipped beside the exe, and in PyInstaller's `_internal/` folder.
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(parent) = target_path.parent() {
        dirs.push(parent.to_path_buf());
        dirs.push(parent.join("_internal"));
    }
    for dir in dirs {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue, // a missing _internal/ is normal, not an error
        };
        for entry in entries.flatten() {
            let name = match entry.file_name().to_str() {
                Some(n) => n.to_ascii_lowercase(),
                None => continue,
            };
            if let Some(minor) = python_dll_minor(&name) {
                // Python 3.13+ moved monotonic onto QPC too (CPython PR 116781); earlier, only
                // perf_counter is on QPC and monotonic (GetTickCount64) still scales.
                add(
                    &mut keys,
                    if minor >= 13 {
                        "runtime.python_monotonic_qpc"
                    } else {
                        "runtime.python_perfcounter_qpc"
                    },
                );
            } else if name == "python3.dll" || name == "python.dll" {
                add(&mut keys, "runtime.python_perfcounter_qpc"); // stable-ABI dll, version unknown
            } else if name == "coreclr.dll" || name == "clr.dll" || name.ends_with(".deps.json") {
                add(&mut keys, "runtime.dotnet_stopwatch_qpc");
            } else if name == "jvm.dll" {
                add(&mut keys, "runtime.java_nanotime_qpc");
            }
        }
    }

    // A PyInstaller bundle ships BOTH python3.dll (stable ABI, version unknown) and python3XX.dll, so
    // both python keys can fire. The monotonic warning already names perf_counter, so drop the narrower
    // perf_counter-only key when the monotonic one is present - one clear warning, not two overlapping.
    if keys.iter().any(|k| k == "runtime.python_monotonic_qpc") {
        keys.retain(|k| k != "runtime.python_perfcounter_qpc");
    }

    keys
}

/// Parse the CPython minor version from a versioned dll name: "python314.dll" -> Some(14), "python39.dll"
/// -> Some(9). Returns None for "python3.dll" (stable ABI, no minor) or any non-matching name.
fn python_dll_minor(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("python3").and_then(|s| s.strip_suffix(".dll"))?;
    if rest.is_empty() {
        return None; // "python3.dll" - stable ABI, minor unknown
    }
    rest.parse::<u32>().ok()
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
    // A Chromium session's coverage unit is a JS context, not an OS process (rule 4 - never sum
    // across units either way).
    let unit = if r.cdp { "context" } else { "pid" };
    let units = if r.cdp { "contexts" } else { "processes" };

    // Headline priority: a vanish is an honest non-effect, then the family verdict, then the
    // parent verdict as a fallback for an older core, then nothing.
    if let Some((reason_key, lived_ms)) = &r.vanished {
        out.push_str("  verdict:  DID NOT TAKE EFFECT - the target vanished right after injection\n");
        out.push_str(&format!(
            "            (suspected single-instance app: {reason_key}; lived {lived_ms} ms)\n"
        ));
    } else if let Some((verdict, reason_key, count)) = &r.session_verdict {
        out.push_str(&format!(
            "  verdict:  {}  ({units}: {count})\n",
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
    } else if let Some((key, origin)) = r.errors.first() {
        // No verdict at all AND an error: the session never started. Say why, and say whose side it
        // came from - `<no verdict emitted>` alone left the tester to guess between a broken install,
        // a target that would not launch, and a tool that did nothing (untouchable rule 6).
        out.push_str("  verdict:  DID NOT START - the session never began\n");
        let why = describe_error(key);
        if why.is_empty() {
            out.push_str(&format!("            reason: {key} (from {origin})\n"));
        } else {
            out.push_str(&format!("            {why} [{key}]\n"));
        }
    } else {
        out.push_str("  verdict:  <no verdict emitted>\n");
    }

    // Errors the headline did not consume: a session that DID start and then had a command rejected
    // (an out-of-range set_multiplier, which by design does not end the session). Those used to be
    // visible only under --json.
    let trailing = if r.session_verdict.is_some() || r.parent_verdict.is_some() || r.vanished.is_some()
    {
        &r.errors[..]
    } else {
        r.errors.get(1..).unwrap_or(&[])
    };
    if !trailing.is_empty() {
        out.push_str("  errors:\n");
        for (key, origin) in trailing {
            let why = describe_error(key);
            if why.is_empty() {
                out.push_str(&format!("            - {key} (from {origin})\n"));
            } else {
                out.push_str(&format!("            - {why} [{key}]\n"));
            }
        }
    }

    if let Some((fake_wall, real_ms, fake_ms)) = &r.timing {
        out.push_str(&format!("  session:  fake clock reached {fake_wall}\n"));
        out.push_str(&format!(
            "            real elapsed {:.1}s, fake elapsed {:.1}s\n",
            *real_ms as f64 / 1000.0,
            *fake_ms as f64 / 1000.0
        ));
    }

    // The target's own exit code, when the app closed itself. Spelled out rather than printed bare,
    // because the report already carries a second number that means something else entirely: this
    // tool's exit code is the verdict (docs/08 section 8), never the target's.
    if let Some(code) = r.target_exit {
        out.push_str(&format!(
            "  exited:   the target closed itself with code {}\n",
            exit_code_label(code)
        ));
    }

    if !r.covered.is_empty() {
        out.push_str("  covered channels (substituted, with call counts):\n");
        for (pid, ch, calls) in &r.covered {
            out.push_str(&format!("            - {unit} {pid}: {ch} ({})\n", calls_label(*calls)));
        }
    }

    if !r.observed.is_empty() {
        out.push_str("  observed channels (hooked but left real):\n");
        for (pid, ch, calls) in &r.observed {
            out.push_str(&format!("            - {unit} {pid}: {ch} ({})\n", calls_label(*calls)));
        }
    }

    if !r.uncovered.is_empty() {
        out.push_str("  uncovered channels (queried but not covered):\n");
        for (pid, ch) in &r.uncovered {
            out.push_str(&format!("            - {unit} {pid}: {ch}\n"));
        }
    }

    if !r.warnings.is_empty() {
        out.push_str("  warnings:\n");
        for w in &r.warnings {
            out.push_str(&format!("            - {}\n", describe_warning(w)));
        }
    }

    // Mess the session left behind. A run that could not clean up after itself says so rather than
    // ending on a tidy-looking report (rule 6) - the same list the GUI panel shows.
    if !r.residue.is_empty() {
        out.push_str("  not fully cleaned up:\n");
        for key in &r.residue {
            out.push_str(&format!("            - {}\n", describe_residue(key)));
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
    /// A pasted date to analyze in reverse (7.3) instead of building a moment. None = build mode.
    analyze: Option<String>,
    /// A custom .NET/Java-style format mask (7.3, docs/02 8.9) for the result. None = fixed formats only.
    format: Option<String>,
    /// A named preset id (docs/04 4.3): the moment comes from `presets/<id>.json`, not from step
    /// flags. None = build the moment from the flags. Cannot be combined with the step flags.
    preset: Option<String>,
    /// Preset parameter values from `--param id=value` (docs/04 4.2). Only meaningful with --preset.
    params: HashMap<String, String>,
    /// Emit the result as machine JSON (chronomock.calc/1) instead of human text, so the GUI can consume
    /// the same engine output as a thin client (ADR-6). Composes with build, --analyze, and --preset.
    json: bool,
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

    // The session zone for this whole invocation: the one the caller named, else the HOST's. It used
    // to be UTC here while `chrono run` beside it already followed the host, so the two halves of one
    // tool disagreed about what day it is - and in a zone ahead of UTC, `--base today` between midnight
    // and the offset returned YESTERDAY (R2-S7). A calculator that is a day out around midnight is
    // worse than no calculator. `--zone` still wins, and the zone is printed either way (rule 2).
    //
    // ONE zone per invocation, not one per base kind: the civil fields and the instant formats
    // (epoch, FILETIME, RFC 1123) have to be read in the same zone, or a single result would carry
    // two answers - the very shape of R2-S11 next door.
    let zone_bias = session_zone_default(ca.zone_bias_min);
    let zone_from_host = ca.zone_bias_min.is_none();

    // Resolve the real current time in that zone, as data for the pure core.
    let now = match resolve_now_civil(Some(zone_bias)) {
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

    // Reverse analysis (7.3): with --analyze, interpret a pasted date instead of building a moment.
    // The build flags (--base/--shift/...) do not apply; --calendar and --zone still do.
    if let Some(input) = &ca.analyze {
        return match chrono_core::calc::analyze_date(input) {
            Ok(analysis) => {
                if ca.json {
                    println!("{}", calc_analysis_json(&analysis, input, &now, Some(zone_bias), calendar.as_ref()));
                } else {
                    print!("{}", render_analysis(&analysis, input, &now, Some(zone_bias), calendar.as_ref()));
                }
                0
            }
            Err(e) => {
                eprintln!("chrono calc: {e} (calc.analyze_unrecognized)");
                1
            }
        };
    }

    // A named preset (docs/04 4.3) supplies the moment in place of the step flags; otherwise the
    // moment is built from the flags. The preset also carries a human header (name + "explains").
    let preset_id = ca.preset.clone();
    let (expr, preset_header, preset_meta) = match preset_id {
        Some(pid) => match load_preset(&pid) {
            Ok(p) => {
                // The calculator surface honours `applies_to`: a substitution-only preset
                // (e.g. year-rollover) is not a calculator question (docs/05 3.1). Refuse it
                // instead of computing a moment nobody asked the calculator for.
                if !preset_targets_calculator(&p.applies_to) {
                    eprintln!(
                        "chrono calc: preset '{}' targets {}, not the calculator (calc.preset_not_for_calculator)",
                        p.id, p.applies_to
                    );
                    return 1;
                }
                // Resolve the preset's parameters (--param / default) then substitute them into its
                // moment. A non-parametric preset resolves to an empty map and an unchanged moment.
                // The calculator has no target, so no target_file_creation hint (None).
                let values = match resolve_parameters(&p.parameters, &ca.params, None) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("chrono calc: {}", e.message());
                        return e.exit_code();
                    }
                };
                let moment = match resolve_moment(p.moment, &values) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("chrono calc: {}", e.message());
                        return e.exit_code();
                    }
                };
                let pj = PresetJson { id: p.id, name: p.name_en, explains: p.explains_en };
                let header =
                    format!("  preset:   {} - {}\n  explains: {}\n", pj.id, pj.name, pj.explains);
                (moment, Some(header), Some(pj))
            }
            Err(e) => {
                eprintln!("chrono calc: {}", e.message());
                return e.exit_code();
            }
        },
        None => (MomentExpr { base: ca.base, steps: ca.steps }, None, None),
    };
    match chrono_core::calc::eval(
        &expr,
        &EvalContext { now, zone_bias_min: zone_bias, calendar: calendar.as_ref() },
    ) {
        Ok(outcome) => {
            if ca.json {
                println!(
                    "{}",
                    calc_moment_json(&outcome, &now, calendar.as_ref(), ca.format.as_deref(), preset_meta)
                );
                return 0;
            }
            let mut text =
                render_calc(&expr, &outcome, Some(zone_bias), zone_from_host, &now, calendar.as_ref(), preset_header.as_deref());
            // A custom mask (7.3) adds one more line in the target app's exact format.
            if let Some(mask) = &ca.format {
                text.push_str(&format!(
                    "  custom format:  {}\n",
                    chrono_core::calc::format_with_mask(&outcome.result(), mask)
                ));
            }
            print!("{text}");
            0
        }
        Err(e) => {
            eprintln!("{}", describe_calc_error(&e));
            calc_error_exit_code(&e)
        }
    }
}

// --- Machine JSON output for the calculator (chronomock.calc/1) -----------------------
//
// The calc engine lives in chrono-core (serde-free), so the CLI owns serialization: these DTOs are
// populated from the core's typed results, the mirror of the calendar/preset loaders. The GUI is a
// thin client of this contract (ADR-6), consuming the same engine output the human render shows.
// Contract keys are public names (rule 17); a breaking change needs a schema bump (private repo now).
// An error still goes to stderr with a non-zero exit - stdout carries a result only on success.

const CALC_SCHEMA: &str = "chronomock.calc/1";

#[derive(serde::Serialize)]
struct CalcJson {
    schema: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    moment: Option<MomentJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    analysis: Option<AnalysisJson>,
}

#[derive(serde::Serialize)]
struct MomentJson {
    /// The result moment as ISO wall-clock in `zone_bias_min`.
    iso: String,
    /// Result-zone bias in minutes (UTC = local + bias); a `zone` step may move it off the session zone.
    zone_bias_min: i32,
    base: String,
    /// The intermediate result after each step, in order.
    steps: Vec<String>,
    formats: FormatsJson,
    metadata: MetadataJson,
    /// Stable significance tokens (Significance::key); empty when the date hits no landmark.
    significance: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<PresetJson>,
}

#[derive(serde::Serialize)]
struct FormatsJson {
    iso_date: String,
    iso_datetime: String,
    us: String,
    pl: String,
    /// Instant-based formats are null outside the representable FILETIME range (never a wrong number).
    epoch_seconds: Option<i64>,
    epoch_millis: Option<i64>,
    filetime: Option<i64>,
    rfc1123: Option<String>,
}

#[derive(serde::Serialize)]
struct MetadataJson {
    weekday: &'static str,
    iso_week_year: i64,
    iso_week: u32,
    us_week: u32,
    day_of_year: u32,
    quarter: u32,
    is_leap_year: bool,
    days_from_today: i64,
    /// null when no calendar was supplied; otherwise whether the date is a business day.
    business_day: Option<bool>,
    /// The holiday's English name, or null (no calendar, or not a holiday - business_day disambiguates).
    holiday: Option<String>,
}

#[derive(serde::Serialize)]
struct PresetJson {
    id: String,
    name: String,
    explains: String,
}

#[derive(serde::Serialize)]
struct AnalysisJson {
    input: String,
    ambiguous: bool,
    readings: Vec<ReadingJson>,
}

#[derive(serde::Serialize)]
struct ReadingJson {
    /// Stable reading token (DateReading::key): iso / us_month_day / pl_day_month.
    reading: &'static str,
    iso: String,
    significance: Vec<&'static str>,
    metadata: MetadataJson,
}

fn formats_json(civil: &chrono_core::calc::CivilDateTime, bias: i32) -> FormatsJson {
    let f = chrono_core::calc::formats(civil, bias);
    FormatsJson {
        iso_date: f.iso_date,
        iso_datetime: f.iso_datetime,
        us: f.us,
        pl: f.pl,
        epoch_seconds: f.epoch_seconds,
        epoch_millis: f.epoch_millis,
        filetime: f.filetime,
        rfc1123: f.rfc1123,
    }
}

fn metadata_json(
    civil: &chrono_core::calc::CivilDateTime,
    now: &chrono_core::calc::CivilDateTime,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> MetadataJson {
    let m = chrono_core::calc::metadata(civil, now);
    let (business_day, holiday) = match calendar {
        Some(cal) => (
            Some(chrono_core::calendar::is_business_day(civil, cal)),
            chrono_core::calendar::holiday_on(civil, cal).map(|h| h.name_en.clone()),
        ),
        None => (None, None),
    };
    MetadataJson {
        weekday: m.weekday,
        iso_week_year: m.iso_week_year,
        iso_week: m.iso_week,
        us_week: m.us_week,
        day_of_year: m.day_of_year,
        quarter: m.quarter,
        is_leap_year: m.is_leap_year,
        days_from_today: m.days_from_today,
        business_day,
        holiday,
    }
}

fn significance_keys(
    civil: &chrono_core::calc::CivilDateTime,
    bias: i32,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> Vec<&'static str> {
    chrono_core::calc::significance(civil, bias, calendar).iter().map(|s| s.key()).collect()
}

fn calc_moment_json(
    outcome: &chrono_core::calc::EvalOutcome,
    now: &chrono_core::calc::CivilDateTime,
    calendar: Option<&chrono_core::calendar::Calendar>,
    format_mask: Option<&str>,
    preset: Option<PresetJson>,
) -> String {
    let result = outcome.result();
    let bias = outcome.result_bias;
    let moment = MomentJson {
        iso: result.to_iso(),
        zone_bias_min: bias,
        base: outcome.base.to_iso(),
        steps: outcome.after_each.iter().map(|c| c.to_iso()).collect(),
        formats: formats_json(&result, bias),
        metadata: metadata_json(&result, now, calendar),
        significance: significance_keys(&result, bias, calendar),
        custom_format: format_mask.map(|m| chrono_core::calc::format_with_mask(&result, m)),
        preset,
    };
    let doc = CalcJson { schema: CALC_SCHEMA, moment: Some(moment), analysis: None };
    serde_json::to_string(&doc).unwrap_or_else(|e| format!(r#"{{"error":"serialize: {e}"}}"#))
}

fn calc_analysis_json(
    analysis: &chrono_core::calc::DateAnalysis,
    input: &str,
    now: &chrono_core::calc::CivilDateTime,
    zone_bias_min: Option<i32>,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    let bias = zone_bias_min.unwrap_or(0);
    let readings = analysis
        .readings
        .iter()
        .map(|(reading, civil)| ReadingJson {
            reading: reading.key(),
            iso: civil.to_iso(),
            significance: significance_keys(civil, bias, calendar),
            metadata: metadata_json(civil, now, calendar),
        })
        .collect();
    let doc = CalcJson {
        schema: CALC_SCHEMA,
        moment: None,
        analysis: Some(AnalysisJson { input: input.to_string(), ambiguous: analysis.is_ambiguous(), readings }),
    };
    serde_json::to_string(&doc).unwrap_or_else(|e| format!(r#"{{"error":"serialize: {e}"}}"#))
}

fn parse_calc_args(argv: &[String]) -> Result<CalcArgs, String> {
    let mut base = Base::Today;
    let mut steps: Vec<Step> = Vec::new();
    let mut zone_bias_min: Option<i32> = None;
    let mut calendar: Option<String> = None;
    let mut analyze: Option<String> = None;
    let mut format: Option<String> = None;
    let mut preset: Option<String> = None;
    let mut params: HashMap<String, String> = HashMap::new();
    let mut json = false;
    // Whether any moment-building flag appeared, so `--preset` (which supplies its own moment)
    // can reject being combined with them instead of silently ignoring one source.
    let mut saw_step_flag = false;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--base" => {
                i += 1;
                base = parse_base(argv.get(i).ok_or("--base needs a value")?)?;
                saw_step_flag = true;
            }
            "--param" => {
                i += 1;
                let raw = argv.get(i).ok_or("--param needs id=value like start_date=2026-01-01")?;
                let (id, value) = raw
                    .split_once('=')
                    .ok_or_else(|| format!("--param must be id=value, got '{raw}'"))?;
                if id.is_empty() {
                    return Err(format!("--param needs a non-empty id, got '{raw}'"));
                }
                params.insert(id.to_string(), value.to_string());
            }
            "--preset" => {
                i += 1;
                preset = Some(argv.get(i).ok_or("--preset needs an id like month-end")?.clone());
            }
            "--calendar" => {
                i += 1;
                calendar = Some(argv.get(i).ok_or("--calendar needs an id like us-banking")?.clone());
            }
            "--analyze" => {
                i += 1;
                analyze = Some(argv.get(i).ok_or("--analyze needs a date like 04/08/2008")?.clone());
            }
            "--format" => {
                i += 1;
                let m = argv.get(i).ok_or("--format needs a mask like yyyy-MM-dd")?;
                if m.is_empty() {
                    return Err("--format mask is empty".into());
                }
                format = Some(m.clone());
            }
            "--zone" => {
                i += 1;
                let raw = argv.get(i).ok_or("--zone needs a value like +02:00")?;
                zone_bias_min = Some(parse_zone_to_bias(raw)?);
            }
            "--to-zone" => {
                i += 1;
                let raw = argv.get(i).ok_or("--to-zone needs a value like +05:45")?;
                steps.push(Step::Zone(parse_zone_to_bias(raw)?));
                saw_step_flag = true;
            }
            "--shift" => {
                i += 1;
                steps.push(parse_shift(argv.get(i).ok_or("--shift needs a value like +18years")?)?);
                saw_step_flag = true;
            }
            "--set-time" => {
                i += 1;
                steps.push(parse_set_time(argv.get(i).ok_or("--set-time needs a value like 23:59:59")?)?);
                saw_step_flag = true;
            }
            "--snap" => {
                i += 1;
                steps.push(Step::Snap(parse_snap(argv.get(i).ok_or("--snap needs a target")?)?));
                saw_step_flag = true;
            }
            "--nearest" => {
                i += 1;
                steps.push(Step::Nearest(parse_nearest(argv.get(i).ok_or("--nearest needs a target")?)?));
                saw_step_flag = true;
            }
            "--json" => json = true,
            other if other.starts_with("--") => return Err(format!("unknown flag '{other}'")),
            other => return Err(format!("unexpected argument '{other}'")),
        }
        i += 1;
    }

    // A preset supplies its own moment (base + steps); combining it with step flags would mean two
    // sources for one moment. Reject it rather than pick one silently. `--analyze` is a different
    // mode entirely (it reads a date, builds nothing), so it cannot ride with `--preset` either.
    if preset.is_some() && saw_step_flag {
        return Err("--preset builds the moment on its own; it cannot be combined with \
                    --base/--shift/--set-time/--snap/--nearest/--to-zone"
            .into());
    }
    if preset.is_some() && analyze.is_some() {
        return Err("--preset and --analyze are different modes; use one at a time".into());
    }
    // --param only makes sense with --preset (it fills a preset's declared parameters).
    if !params.is_empty() && preset.is_none() {
        return Err("--param needs --preset (parameters belong to a preset)".into());
    }

    Ok(CalcArgs { base, steps, zone_bias_min, calendar, analyze, format, preset, params, json })
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
        EvalError::StepUnsupported { .. } | EvalError::NeedsCalendar { .. } => 5,
        EvalError::Overflow { .. }
        | EvalError::BadSetTime { .. }
        | EvalError::DegenerateCalendar { .. }
        // Out of the year band is bad INPUT, not an unbuilt operation: the step is built, the year
        // is one this build will not compute a calendar on. Exit 1, next to the other usage errors.
        | EvalError::BaseYearOutOfRange
        | EvalError::YearOutOfRange { .. } => 1,
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
        EvalError::NeedsCalendar { index } => {
            format!("chrono calc: step {} needs a calendar - pass --calendar (calc.needs_calendar)", index + 1)
        }
        EvalError::DegenerateCalendar { index } => format!(
            "chrono calc: step {} found no business day - this calendar marks (almost) every day as one off; check its weekend and holidays (calc.degenerate_calendar)",
            index + 1
        ),
        EvalError::Overflow { index } => {
            format!("chrono calc: step {} overflows the representable range (calc.overflow)", index + 1)
        }
        EvalError::BadSetTime { index } => {
            format!("chrono calc: step {} has an out-of-range time (calc.bad_set_time)", index + 1)
        }
        EvalError::BaseYearOutOfRange => format!(
            "chrono calc: the base year is outside the range this build computes on ({}..={}) (calc.year_out_of_range)",
            chrono_core::CIVIL_YEAR_MIN,
            chrono_core::CIVIL_YEAR_MAX
        ),
        // Deliberately not phrased as an overflow: nothing overflowed. The step produced an exact
        // year that this build does not compute calendars on, and naming it that way is the whole
        // reason it is a separate variant.
        EvalError::YearOutOfRange { index } => format!(
            "chrono calc: step {} lands on a year outside the range this build computes on ({}..={}) (calc.year_out_of_range)",
            index + 1,
            chrono_core::CIVIL_YEAR_MIN,
            chrono_core::CIVIL_YEAR_MAX
        ),
    }
}

/// Render a calc result: the base, the intermediate value after each step, and the
/// final moment (7.3 - the user sees where they went wrong, not just the final number).
fn render_calc(
    expr: &MomentExpr,
    outcome: &chrono_core::calc::EvalOutcome,
    zone_bias_min: Option<i32>,
    // True when the caller named no `--zone` and this is the host's offset. Printed, so a reader
    // never has to wonder whether a zone they did not type is one they can rely on (rule 2).
    zone_from_host: bool,
    now: &chrono_core::calc::CivilDateTime,
    calendar: Option<&chrono_core::calendar::Calendar>,
    preset_header: Option<&str>,
) -> String {
    let zone = match (zone_bias_min.map(format_bias), zone_from_host) {
        (Some(z), true) => format!("{z}, from the host"),
        (Some(z), false) => z,
        (None, _) => "UTC".into(),
    };
    let mut out = String::from("Chrono Mock - date calculator\n");
    // When the moment came from a named preset, name it and show its "explains" line (the
    // preset's authored framing, docs/04 4.2 - distinct from the computed significance block).
    if let Some(h) = preset_header {
        out.push_str(h);
    }
    match &expr.base {
        Base::Today => out.push_str(&format!("  base:    {}  (today, session zone {zone})\n", outcome.base.to_iso())),
        Base::Now => out.push_str(&format!("  base:    {}  (now, session zone {zone})\n", outcome.base.to_iso())),
        Base::Absolute(_) => out.push_str(&format!("  base:    {}\n", outcome.base.to_iso())),
    }
    for (i, step) in expr.steps.iter().enumerate() {
        out.push_str(&format!("  step {}:  {}  -> {}\n", i + 1, describe_step(step), outcome.after_each[i].to_iso()));
    }
    out.push_str(&format!("  result:  {}\n", outcome.result().to_iso()));
    // Formats and significance follow the RESULT's zone, which a `zone` step may have moved away
    // from the session zone. Without a `zone` step this equals the session zone, so the output is
    // unchanged from before.
    let result_bias = outcome.result_bias;
    out.push_str(&render_formats(&outcome.result(), result_bias));
    out.push_str(&render_metadata(&outcome.result(), now, calendar));
    out.push_str(&render_significance(&outcome.result(), result_bias, calendar));
    out
}

/// Render the "what this date tests" block (7.3): the test-relevant landmarks the result lands
/// on, one per line, ready to read at a glance. This is the calculator's differentiator over an
/// online date calculator (6.2). With a calendar it also names the weekend / holiday / observed-
/// holiday landmarks. Omitted entirely when the date hits nothing notable - the block is a
/// positive signal, never a "no landmark" line to scan past.
fn render_significance(
    civil: &chrono_core::calc::CivilDateTime,
    tz_bias_min: i32,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    let marks = chrono_core::calc::significance(civil, tz_bias_min, calendar);
    if marks.is_empty() {
        return String::new();
    }
    let mut out = String::from("  what this date tests:\n");
    for m in marks {
        out.push_str(&format!("    {}\n", m.label()));
    }
    out
}

/// Render a reverse-analysis result (7.3): the input, then each reading with its resolved date,
/// weekday, and the "what this date tests" markers. Two readings mean an ambiguous numeric order
/// (04/08 is April 8 in the US, August 4 in Poland) - both are shown rather than one chosen silently.
fn render_analysis(
    analysis: &chrono_core::calc::DateAnalysis,
    input: &str,
    now: &chrono_core::calc::CivilDateTime,
    zone_bias_min: Option<i32>,
    calendar: Option<&chrono_core::calendar::Calendar>,
) -> String {
    let mut out = String::from("Chrono Mock - date analysis\n");
    if analysis.is_ambiguous() {
        out.push_str(&format!("  input:   {input}  (ambiguous - month/day order differs by locale)\n"));
    } else {
        out.push_str(&format!("  input:   {input}\n"));
    }
    let bias = zone_bias_min.unwrap_or(0);
    for (reading, civil) in &analysis.readings {
        let weekday = chrono_core::calc::metadata(civil, now).weekday;
        out.push_str(&format!(
            "  {}:  {:04}-{:02}-{:02}  ({weekday})\n",
            reading.label(),
            civil.year,
            civil.month,
            civil.day
        ));
        for m in chrono_core::calc::significance(civil, bias, calendar) {
            out.push_str(&format!("      {}\n", m.label()));
        }
    }
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
        Step::Snap(t) => format!("snap to {}", t.label()),
        Step::Nearest(t) => format!("nearest {}", t.label()),
        Step::Zone(bias) => format!("zone {}", format_bias(*bias)),
    }
}

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
        // Upper bound (R2-K2): the largest accepted rate passes, one past it does not. Above this
        // the fake clock leaves the representable range mid-session and the channels disagree.
        assert!(parse_mode(&format!("x{}", chrono_core::MULTIPLIER_MAX)).is_ok());
        assert!(parse_mode(&format!("x{}", chrono_core::MULTIPLIER_MAX + 1)).is_err());
    }

    #[test]
    fn set_after_multiplier_must_be_at_least_one() {
        // The in-flight multiplier gets the same >= 1 rule as --mode xN, instead of silently
        // accepting a zero/negative that would freeze or reverse the wall clock.
        let ok = parse_run_args(&["t".into(), "--set-after".into(), "5:60".into()]).unwrap();
        assert_eq!(ok.set_after, Some((5, 60)));
        assert!(parse_run_args(&["t".into(), "--set-after".into(), "5:0".into()]).is_err());
        assert!(parse_run_args(&["t".into(), "--set-after".into(), "5:-3".into()]).is_err());
    }

    /// `--cwd` closes an asymmetry that lasted from the day the protocol was written: `TargetSpec.cwd`
    /// has always been on the wire and `chrono-mech` has always handed it to CreateProcessW, but no
    /// surface offered it - the CLI had `--args` and nothing else, and the driver hard-coded `cwd: None`.
    #[test]
    fn cwd_is_parsed_from_the_command_line() {
        let ok = parse_run_args(&["app.exe".into(), "--cwd".into(), r"C:\work".into()]).unwrap();
        assert_eq!(ok.cwd.as_deref(), Some(r"C:\work"));

        // Absent stays absent: "do not ask for a directory" is not the same as asking for one.
        let none = parse_run_args(&["app.exe".into()]).unwrap();
        assert_eq!(none.cwd, None);
    }

    /// The half the parser test does NOT cover. The driver used to write `cwd: None` inline, so a
    /// parser that read the flag perfectly still sent nothing - and reverting that line failed no
    /// test at all until this one existed.
    #[test]
    fn the_driver_puts_the_parsed_cwd_on_the_wire() {
        let ra = parse_run_args(&["app.exe".into(), "--cwd".into(), "C:/work".into()]).unwrap();
        assert_eq!(target_spec_for(&ra).cwd.as_deref(), Some("C:/work"));

        let bare = parse_run_args(&["app.exe".into()]).unwrap();
        assert_eq!(target_spec_for(&bare).cwd, None);
    }

    /// An explicitly empty value is a usage error rather than a quiet "no directory". The two are
    /// different on the wire (absent vs present-and-empty), and only absent is something the
    /// mechanism can act on - CreateProcessW cannot be given an empty directory.
    #[test]
    fn an_empty_cwd_is_refused_rather_than_read_as_no_directory() {
        assert!(parse_run_args(&["app.exe".into(), "--cwd".into(), String::new()]).is_err());
        assert!(parse_run_args(&["app.exe".into(), "--cwd".into(), "   ".into()]).is_err());
        assert!(parse_run_args(&["app.exe".into(), "--cwd".into()]).is_err());
    }



    #[test]
    fn args_split_plain_and_quoted() {
        let s = |x: &str| x.to_string();
        // Plain space-separated values behave exactly as before (backward compatible).
        assert_eq!(split_args("a b c"), vec![s("a"), s("b"), s("c")]);
        assert_eq!(split_args("  x   y "), vec![s("x"), s("y")]);
        assert_eq!(split_args("out 100 10"), vec![s("out"), s("100"), s("10")]);
        // Double-quotes group a single argument that contains spaces (the P9 fix).
        assert_eq!(split_args("\"a b\" c"), vec![s("a b"), s("c")]);
        assert!(split_args("").is_empty());
        assert_eq!(split_args("\"\""), vec![s("")]); // an explicit empty argument
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

    fn empty_report() -> SessionReport {
        SessionReport {
            target: "app.exe".into(),
            session_verdict: None,
            parent_verdict: None,
            vanished: None,
            errors: vec![],
            warnings: vec![],
            uncovered: vec![],
            covered: vec![],
            observed: vec![],
            timing: None,
            target_exit: None,
            residue: vec![],
            cdp: false,
        }
    }

    #[test]
    fn cdp_report_labels_the_unit_as_context() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "chromium.contexts_covered".into(), 2)),
            covered: vec![(1, "page setInterval".into(), 5)],
            warnings: vec!["chromium.launched_with_debug_port".into()],
            cdp: true,
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("contexts: 2"), "got:\n{out}");
        assert!(out.contains("- context 1: page setInterval"), "got:\n{out}");
        assert!(out.contains("JS context ran on the session clock"), "got:\n{out}");
        assert!(out.contains("remote-debugging port"), "got:\n{out}");
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
    fn a_bitness_mismatch_is_a_usage_error_and_names_both_sides() {
        // R2-S1: exit 1, not 2. Nothing is wrong with the application - the wrong build of the tool
        // was pointed at it, and the message has to say which one to run instead.
        let (code, key, origin, detail) =
            map_prepare_error(chrono_mech::PrepareError::BitnessMismatch("x86", "x64"));
        assert_eq!(code, 1);
        assert_eq!(key, "target.bitness_mismatch");
        assert_eq!(origin, "mechanism");
        assert!(detail.contains("runs as x86"), "got: {detail}");
        assert!(detail.contains("this core is x64"), "got: {detail}");
        assert!(detail.contains("run the x86 chrono.exe"), "got: {detail}");
        // And it reads as its own thing in the report, not as an injection failure.
        assert_ne!(describe_error(key), describe_error("target.inject_failed"));
        assert!(!describe_error(key).is_empty());
    }

    #[test]
    fn a_session_that_never_started_says_why_not_no_verdict_emitted() {
        // R2-W3, second half. A broken install now reaches `core.hook_dll_missing` instead of being
        // called `target.inject_failed` - but the human report used to drop every error event, so the
        // reachable diagnosis would still have arrived only under --json.
        let r = SessionReport {
            errors: vec![("core.hook_dll_missing".into(), "core".into())],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("DID NOT START"), "got:\n{out}");
        assert!(out.contains("installation is incomplete"), "got:\n{out}");
        assert!(out.contains("[core.hook_dll_missing]"), "got:\n{out}");
        assert!(!out.contains("<no verdict emitted>"), "got:\n{out}");
        // The headline consumed the only error, so no duplicate list underneath it.
        assert!(!out.contains("  errors:"), "got:\n{out}");
    }

    #[test]
    fn an_unknown_error_key_is_shown_verbatim_with_its_origin() {
        // Same contract as the reason and warning tables: a core newer than this driver says something
        // true rather than something invented.
        let r = SessionReport {
            errors: vec![("some.future_key".into(), "mechanism".into())],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("some.future_key (from mechanism)"), "got:\n{out}");
    }

    #[test]
    fn an_error_after_the_session_started_is_listed_not_swallowed() {
        // A rejected in-flight command (an out-of-range set_multiplier) deliberately does not end the
        // session, so it must not become the headline - but it must not disappear either.
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            errors: vec![("time.bad_multiplier".into(), "core".into())],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("WORKS"), "got:\n{out}");
        assert!(out.contains("  errors:"), "got:\n{out}");
        assert!(out.contains("outside the range"), "got:\n{out}");
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
    fn target_exit_code_is_shown_as_the_apps_own() {
        // The wire has carried `ended.target_exit_code` all along and the GUI panel has shown it
        // since 7446a59; the CLI report dropped it, so a plain run could not tell "the app failed
        // with code 3" from "we ended the session" (rule 6). Spelled out, because this report also
        // carries a verdict whose code means something else entirely (docs/08 section 8).
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            target_exit: Some(3),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("the target closed itself with code 3"), "got:\n{out}");
    }

    #[test]
    fn a_target_that_did_not_close_itself_gets_no_exit_line() {
        // No exit code means the app did not close itself: a --ticks cutoff (it is still running),
        // a CDP session (we close our own instance), or a session that never started. Printing 0
        // there would state a result the session never observed (untouchable rule 4).
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            target_exit: None,
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(!out.contains("closed itself"), "got:\n{out}");
        assert!(!out.contains("exited:"), "got:\n{out}");
    }

    #[test]
    fn a_crash_exit_code_carries_the_hex_form() {
        // GetExitCodeProcess yields a u32 the wire carries as an i32, so an access violation arrives
        // as -1073741819 - a number nobody recognises until it is written 0xC0000005.
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            target_exit: Some(-1073741819),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("-1073741819 (0xC0000005)"), "got:\n{out}");
    }

    #[test]
    fn cleanup_residue_is_reported_unknown_key_verbatim() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "chromium.contexts_covered".into(), 1)),
            residue: vec!["cleanup.chromium_profile_left".into(), "cleanup.something_new".into()],
            cdp: true,
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("not fully cleaned up"), "got:\n{out}");
        assert!(out.contains("temporary Chromium profile"), "got:\n{out}");
        assert!(out.contains("cleanup.chromium_profile_left"), "got:\n{out}");
        // Same rule as warnings: a key we cannot name is still shown, never swallowed.
        assert!(out.contains("cleanup.something_new"), "got:\n{out}");
    }

    #[test]
    fn evidence_export_carries_the_target_exit_code() {
        // docs/08 section 9: --report writes "the same readable report", so the file a tester cites
        // as proof must not be missing what the console showed.
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            target_exit: Some(2),
            ..empty_report()
        };
        let p = EvidenceParams {
            moment: "2038-01-19T03:14:07".into(),
            zone: "+00:00".into(),
            mode: "x60".into(),
        };
        let out = render_evidence(&r, &p);
        assert!(out.contains("the target closed itself with code 2"), "got:\n{out}");
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
    fn calc_exit_codes_split_bad_input_from_needs_data() {
        // Not built / needs data -> code 5; bad input -> usage 1.
        assert_eq!(calc_error_exit_code(&EvalError::StepUnsupported { kind: "zone", index: 0 }), 5);
        assert_eq!(calc_error_exit_code(&EvalError::NeedsCalendar { index: 0 }), 5);
        // R2-S10: a degenerate calendar is BAD INPUT (exit 1), not an unbuilt operation (exit 5).
        // It used to travel as `NotFound` -> 5 from `--nearest` and as `Overflow` -> 1 from `+Nbd`:
        // two answers, one cause, and the 5 told a script the feature does not exist yet.
        assert_eq!(calc_error_exit_code(&EvalError::DegenerateCalendar { index: 0 }), 1);
        assert_eq!(calc_error_exit_code(&EvalError::Overflow { index: 0 }), 1);
        assert_eq!(calc_error_exit_code(&EvalError::BadSetTime { index: 0 }), 1);
    }

    #[test]
    fn calc_error_message_carries_key_and_one_based_step() {
        let msg = describe_calc_error(&EvalError::NeedsCalendar { index: 0 });
        assert!(msg.contains("step 1"), "1-based step number, got: {msg}");
        assert!(msg.contains("calc.needs_calendar"), "stable key, got: {msg}");
        let msg = describe_calc_error(&EvalError::StepUnsupported { kind: "zone", index: 2 });
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
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
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
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, None, false, &now, None, None);
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
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, Some(0), false, &now, None, None);
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
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, Some(0), false, &now, None, None);
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
    fn calc_snap_end_of_quarter_end_to_end() {
        let ca = parse_calc_args(&[
            "--base".into(),
            "2026-05-15T09:30:00".into(),
            "--snap".into(),
            "end-of-quarter".into(),
        ])
        .unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        assert_eq!(out.result().to_iso(), "2026-06-30T23:59:59");
    }

    #[test]
    fn render_calc_shows_the_what_this_tests_block_when_a_landmark_is_hit() {
        // A snap to the end of the year lands on Dec 31: the block names the year-end landmark.
        let ca = parse_calc_args(&["--base".into(), "2026-05-15T00:00:00".into(), "--snap".into(), "eoy".into()])
            .unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, Some(0), false, &now, None, None);
        assert!(text.contains("what this date tests:"), "got:\n{text}");
        assert!(text.contains("last day of the year (year-end rollover)"), "got:\n{text}");
    }

    #[test]
    fn render_calc_omits_the_block_for_a_plain_date() {
        // A mid-month weekday hits no landmark, so the block is absent (positive signal only).
        let ca = parse_calc_args(&["--base".into(), "2026-08-12T09:00:00".into()]).unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let text = render_calc(&expr, &out, Some(0), false, &now, None, None);
        assert!(!text.contains("what this date tests:"), "got:\n{text}");
    }

    #[test]
    fn render_calc_names_calendar_landmarks_only_with_a_calendar() {
        // 2026-07-04 is a Saturday and Independence Day: with a calendar the block names both.
        let ca = parse_calc_args(&["--base".into(), "2026-07-04T00:00:00".into()]).unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let cal = test_calendar();
        let text = render_calc(&expr, &out, Some(0), false, &now, Some(&cal), None);
        assert!(text.contains("what this date tests:"), "got:\n{text}");
        assert!(text.contains("weekend - not a business day"), "got:\n{text}");
        assert!(text.contains("public holiday"), "got:\n{text}");
        // Without a calendar the same date names no calendar landmark (and here nothing at all).
        assert!(!render_calc(&expr, &out, Some(0), false, &now, None, None).contains("what this date tests:"));
    }

    #[test]
    fn calc_to_zone_converts_preserving_the_instant_end_to_end() {
        // 12:00 in UTC+2 re-expressed in UTC+5:45 (Kathmandu's offset): 15:45 wall-clock, and the
        // instant (epoch) is unchanged - the whole point of a zone conversion.
        let ca = parse_calc_args(&[
            "--base".into(),
            "2026-01-15T12:00:00".into(),
            "--zone".into(),
            "+02:00".into(),
            "--to-zone".into(),
            "+05:45".into(),
        ])
        .unwrap();
        let expr = MomentExpr { base: ca.base, steps: ca.steps };
        let now = chrono_core::calc::CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let bias = ca.zone_bias_min.unwrap_or(0);
        let out = chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: bias, calendar: None }).unwrap();
        assert_eq!(out.result().to_iso(), "2026-01-15T15:45:00");
        assert_eq!(out.result_bias, -345);
        let text = render_calc(&expr, &out, ca.zone_bias_min, false, &now, None, None);
        assert!(text.contains("step 1:  zone +05:45"), "got:\n{text}");
        assert!(text.contains("ISO datetime  2026-01-15T15:45:00+05:45"), "got:\n{text}");
        // Same instant as the +02:00 base (12:00+02:00 = 10:00 UTC).
        let base = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 15, hour: 12, minute: 0, second: 0 };
        assert_eq!(
            chrono_core::calc::formats(&out.result(), out.result_bias).epoch_seconds,
            chrono_core::calc::formats(&base, -120).epoch_seconds
        );
    }

    #[test]
    fn calc_to_zone_rejects_a_malformed_offset() {
        assert!(parse_calc_args(&["--to-zone".into(), "midnight".into()]).is_err());
        assert!(parse_calc_args(&["--to-zone".into(), "+5".into()]).is_err());
    }

    #[test]
    fn render_analysis_shows_both_readings_for_an_ambiguous_date() {
        let analysis = chrono_core::calc::analyze_date("04/08/2008").unwrap();
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 25, hour: 0, minute: 0, second: 0 };
        let text = render_analysis(&analysis, "04/08/2008", &now, Some(0), None);
        assert!(text.contains("ambiguous"), "got:\n{text}");
        assert!(text.contains("US MM/DD/YYYY:  2008-04-08  (Tuesday)"), "got:\n{text}");
        assert!(text.contains("PL DD/MM/YYYY:  2008-08-04  (Monday)"), "got:\n{text}");
    }

    #[test]
    fn calc_json_flag_parses() {
        let ca = parse_calc_args(&["--base".into(), "2026-09-30T23:59:59".into(), "--json".into()]).unwrap();
        assert!(ca.json);
        assert!(!parse_calc_args(&["--base".into(), "2026-09-30T00:00:00".into()]).unwrap().json);
    }

    #[test]
    fn calc_moment_json_carries_the_contract_fields() {
        let base = chrono_core::calc::CivilDateTime { year: 2026, month: 9, day: 30, hour: 23, minute: 59, second: 59 };
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 26, hour: 0, minute: 0, second: 0 };
        let expr = MomentExpr { base: Base::Absolute(base), steps: vec![] };
        let outcome =
            chrono_core::calc::eval(&expr, &EvalContext { now, zone_bias_min: 0, calendar: None }).unwrap();
        let json = calc_moment_json(&outcome, &now, None, None, None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema"], "chronomock.calc/1");
        assert_eq!(v["moment"]["iso"], "2026-09-30T23:59:59");
        assert_eq!(v["moment"]["formats"]["iso_date"], "2026-09-30");
        // No calendar supplied, so the calendar-dependent metadata is null (never guessed).
        assert!(v["moment"]["metadata"]["business_day"].is_null());
        let sig = v["moment"]["significance"].as_array().unwrap();
        assert!(sig.iter().any(|s| s == "end_of_quarter"), "got: {sig:?}");
    }

    #[test]
    fn calc_analysis_json_shows_both_readings_with_stable_keys() {
        let analysis = chrono_core::calc::analyze_date("04/08/2008").unwrap();
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 8, day: 26, hour: 0, minute: 0, second: 0 };
        let json = calc_analysis_json(&analysis, "04/08/2008", &now, Some(0), None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema"], "chronomock.calc/1");
        assert_eq!(v["analysis"]["ambiguous"], true);
        let readings = v["analysis"]["readings"].as_array().unwrap();
        assert_eq!(readings.len(), 2);
        // `iso` is the full ISO datetime (midnight for a date reading), consistent with moment.iso.
        assert_eq!(readings[0]["reading"], "us_month_day");
        assert_eq!(readings[0]["iso"], "2008-04-08T00:00:00");
        assert_eq!(readings[1]["reading"], "pl_day_month");
        assert_eq!(readings[1]["iso"], "2008-08-04T00:00:00");
    }

    #[test]
    fn render_analysis_names_a_holiday_with_a_calendar() {
        // 07/04/2026: the US reading is Independence Day (a Saturday), named only with a calendar.
        let analysis = chrono_core::calc::analyze_date("07/04/2026").unwrap();
        let now = chrono_core::calc::CivilDateTime { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 };
        let cal = test_calendar();
        let text = render_analysis(&analysis, "07/04/2026", &now, Some(0), Some(&cal));
        assert!(text.contains("public holiday"), "got:\n{text}");
    }

    #[test]
    fn parse_calc_reads_the_format_mask_and_rejects_empty() {
        let ca = parse_calc_args(&["--format".into(), "dd.MM.yyyy".into()]).unwrap();
        assert_eq!(ca.format.as_deref(), Some("dd.MM.yyyy"));
        assert!(parse_calc_args(&["--format".into(), String::new()]).is_err());
    }

    // --- Presets (Stage 4 slice 16-18): a named moment expression, docs/04 4.3 ----------------










    /// `--preset` supplies its own moment, so combining it with a step flag (or --analyze) is a
    /// usage error; alone it parses.
    #[test]
    fn preset_flag_is_exclusive_of_step_flags() {
        assert!(parse_calc_args(&["--preset".into(), "month-end".into()]).is_ok());
        assert_eq!(
            parse_calc_args(&["--preset".into(), "month-end".into()]).unwrap().preset.as_deref(),
            Some("month-end")
        );
        assert!(parse_calc_args(&["--preset".into(), "month-end".into(), "--shift".into(), "+1d".into()]).is_err());
        assert!(parse_calc_args(&["--shift".into(), "+1d".into(), "--preset".into(), "month-end".into()]).is_err());
        assert!(parse_calc_args(&["--preset".into(), "x".into(), "--analyze".into(), "2020-01-01".into()]).is_err());
    }


    // --- run --preset (Stage 4 slice 17): the substitution bridge, docs/06.3 pkt 3 -----------




    /// `run --preset` supplies the moment and mode, so combining it with a time flag is a usage
    /// error; alone (with a target) it parses and carries the id.
    #[test]
    fn run_preset_flag_is_exclusive_of_time_flags() {
        let ok = parse_run_args(&["--preset".into(), "month-end".into(), "app.exe".into()]).unwrap();
        assert_eq!(ok.preset.as_deref(), Some("month-end"));
        assert_eq!(ok.target, "app.exe");
        assert!(parse_run_args(&["--preset".into(), "m".into(), "--at".into(), "2020-01-01T00:00:00".into(), "app.exe".into()]).is_err());
        assert!(parse_run_args(&["--preset".into(), "m".into(), "--mode".into(), "x60".into(), "app.exe".into()]).is_err());
        assert!(parse_run_args(&["--preset".into(), "m".into(), "--scale-duration".into(), "app.exe".into()]).is_err());
    }

    // --- Parametric presets (Stage 4 slice 18): --param, docs/04 4.2 --------------------------









    /// --param only makes sense with --preset.
    #[test]
    fn calc_param_needs_preset() {
        assert!(parse_calc_args(&["--param".into(), "start_date=2026-01-01".into()]).is_err());
        let ok = parse_calc_args(&["--preset".into(), "trial-first-day-after".into(), "--param".into(), "start_date=2026-01-01".into()]).unwrap();
        assert_eq!(ok.params.get("start_date").map(String::as_str), Some("2026-01-01"));
    }

    // --- run --param + default_hint (Stage 4 slice 19): trial in substitution ------------------




    /// --param in run needs --preset too.
    #[test]
    fn run_param_needs_preset() {
        assert!(parse_run_args(&["--param".into(), "start_date=2026-01-01".into(), "app.exe".into()]).is_err());
        let ok = parse_run_args(&[
            "--preset".into(),
            "trial-first-day-after".into(),
            "--param".into(),
            "start_date=2026-01-01".into(),
            "app.exe".into(),
        ])
        .unwrap();
        assert_eq!(ok.params.get("start_date").map(String::as_str), Some("2026-01-01"));
    }

    // ---- Runtime detection (B1): a Python/.NET/Java target whose monotonic/elapsed clock is on QPC ----

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!("{prefix}-{nanos}-{:?}", std::thread::current().id()));
        p
    }

    #[test]
    fn python_dll_minor_parses_versioned_names() {
        assert_eq!(python_dll_minor("python314.dll"), Some(14));
        assert_eq!(python_dll_minor("python313.dll"), Some(13));
        assert_eq!(python_dll_minor("python39.dll"), Some(9));
        assert_eq!(python_dll_minor("python3.dll"), None); // stable ABI, no minor
        assert_eq!(python_dll_minor("python.dll"), None);
        assert_eq!(python_dll_minor("kernel32.dll"), None);
    }

    #[test]
    fn detect_runtime_flags_python_313_plus_monotonic() {
        // PyInstaller onedir ships BOTH python3.dll (stable ABI) and python3XX.dll, exactly like the real
        // BeanNetworkTester bundle. Python 3.14 -> monotonic warning, and the narrower perf_counter-only
        // key (from python3.dll) is dropped so there is one clear warning, not two overlapping ones.
        let dir = unique_temp_dir("chrono-rt-py313");
        std::fs::create_dir_all(dir.join("_internal")).unwrap();
        std::fs::write(dir.join("_internal").join("python3.dll"), b"").unwrap();
        std::fs::write(dir.join("_internal").join("python314.dll"), b"").unwrap();
        let target = dir.join("App.exe");
        std::fs::write(&target, b"").unwrap();

        let keys = detect_runtime_warnings(&target, false);
        assert!(keys.iter().any(|k| k == "runtime.python_monotonic_qpc"));
        assert!(!keys.iter().any(|k| k == "runtime.python_perfcounter_qpc"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_runtime_flags_python_312_perfcounter_only() {
        // Python 3.12: only perf_counter is on QPC; monotonic (GetTickCount64) still scales.
        let dir = unique_temp_dir("chrono-rt-py312");
        std::fs::create_dir_all(dir.join("_internal")).unwrap();
        std::fs::write(dir.join("_internal").join("python312.dll"), b"").unwrap();
        let target = dir.join("App.exe");
        std::fs::write(&target, b"").unwrap();

        let keys = detect_runtime_warnings(&target, false);
        assert!(keys.iter().any(|k| k == "runtime.python_perfcounter_qpc"));
        assert!(!keys.iter().any(|k| k == "runtime.python_monotonic_qpc"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_runtime_flags_dotnet_java_and_stays_silent_on_native() {
        // .NET via a sibling manifest.
        let net = unique_temp_dir("chrono-rt-net");
        std::fs::create_dir_all(&net).unwrap();
        std::fs::write(net.join("App.deps.json"), b"{}").unwrap();
        let net_target = net.join("App.exe");
        std::fs::write(&net_target, b"").unwrap();
        assert!(detect_runtime_warnings(&net_target, false).iter().any(|k| k == "runtime.dotnet_stopwatch_qpc"));
        std::fs::remove_dir_all(&net).ok();

        // Java via the target exe name (a plain launcher).
        let java = unique_temp_dir("chrono-rt-java");
        std::fs::create_dir_all(&java).unwrap();
        let java_target = java.join("java.exe");
        std::fs::write(&java_target, b"").unwrap();
        assert!(detect_runtime_warnings(&java_target, false).iter().any(|k| k == "runtime.java_nanotime_qpc"));
        std::fs::remove_dir_all(&java).ok();

        // A native target with no runtime markers gets no warning (honest silence, rule 4).
        let native = unique_temp_dir("chrono-rt-native");
        std::fs::create_dir_all(&native).unwrap();
        let native_target = native.join("Native.exe");
        std::fs::write(&native_target, b"").unwrap();
        assert!(detect_runtime_warnings(&native_target, false).is_empty());
        std::fs::remove_dir_all(&native).ok();
    }

    #[test]
    fn scale_qpc_replaces_runtime_warning_with_a_render_caution() {
        // A2: under --scale-qpc the QPC axis scales, so the "does not scale" warning would be wrong. Even a
        // Python target that would otherwise get runtime.python_monotonic_qpc gets ONLY the render caution.
        let dir = unique_temp_dir("chrono-rt-qpc");
        std::fs::create_dir_all(dir.join("_internal")).unwrap();
        std::fs::write(dir.join("_internal").join("python314.dll"), b"").unwrap();
        let target = dir.join("App.exe");
        std::fs::write(&target, b"").unwrap();

        let keys = detect_runtime_warnings(&target, true);
        assert_eq!(keys, vec!["qpc.scaled_render_may_distort".to_string()]);
        assert!(!keys.iter().any(|k| k == "runtime.python_monotonic_qpc"));

        // And a native target under --scale-qpc still gets the render caution (QPC scales for any target).
        let native = unique_temp_dir("chrono-rt-qpc-native");
        std::fs::create_dir_all(&native).unwrap();
        let native_target = native.join("Native.exe");
        std::fs::write(&native_target, b"").unwrap();
        assert_eq!(
            detect_runtime_warnings(&native_target, true),
            vec!["qpc.scaled_render_may_distort".to_string()]
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&native).ok();
    }

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
