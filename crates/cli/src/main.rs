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

use std::io::{BufRead, BufReader, Write};
use std::process::{Command as PCommand, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
    eprintln!("usage: chrono run <target> [--at <local-moment>] [--zone <+HH:MM>] [--mode <flow|frozen|xN>] [--scale-duration] [--ticks N] [--set-after T:M] [--jump-after T:moment] [--args \"...\"] [--json]");
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

/// Resolve the `--at` value. A leading `+`/`-` marks a relative moment (now + delta)
/// with a fixed-length unit s/m/h/d/w; the driver resolves it to an absolute wall
/// string so the core only ever sees an absolute moment. Months, years, and business
/// days are the calculator's job (later) and are rejected here. Anything else passes
/// through as an absolute moment.
fn resolve_at(raw: &str, tz_bias_min: Option<i32>) -> Result<String, String> {
    let first = raw.as_bytes().first().copied();
    if first == Some(b'+') || first == Some(b'-') {
        let split = raw.len().saturating_sub(1);
        let (num, unit) = raw.split_at(split);
        let unit_secs: i64 = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86_400,
            "w" => 604_800,
            _ => return Err(format!("relative --at must end in s/m/h/d/w, got '{raw}'")),
        };
        let n: i64 = num
            .parse()
            .map_err(|_| format!("bad number in relative --at '{raw}'"))?;
        let target = now_filetime_utc() + n * unit_secs * 10_000_000;
        return Ok(filetime_utc_to_wall(target, tz_bias_min.unwrap_or(0)));
    }
    Ok(raw.to_string())
}

/// Send an `end` command to the core over its stdin.
fn send_end(stdin: &mut std::process::ChildStdin) {
    send_command(stdin, &Command::End { v: PROTOCOL_VERSION, id: 2 });
}

/// Send a `set_multiplier` command in flight.
fn send_set_multiplier(stdin: &mut std::process::ChildStdin, m: i64) {
    send_command(stdin, &Command::SetMultiplier { v: PROTOCOL_VERSION, id: 3, multiplier: m });
}

/// Send a `jump` command in flight (absolute moment in the session zone).
fn send_jump(stdin: &mut std::process::ChildStdin, moment: &str, tz_bias_min: Option<i32>) {
    send_command(
        stdin,
        &Command::Jump {
            v: PROTOCOL_VERSION,
            id: 4,
            to: MomentSpec {
                kind: "absolute".into(),
                local: Some(moment.to_string()),
                tz_bias_min,
                delta: None,
            },
        },
    );
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
    let mut verdict_line: Option<(String, String)> = None; // (verdict, reason_key)
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
                Ok(Event::Ended { .. }) => break,
                _ => {}
            }
        }
    }
    drop(stdin);

    let status = child.wait();

    if !ra.json {
        println!("target:  {}", ra.target);
        match &verdict_line {
            Some((v, key)) => println!("verdict: {v} ({key})"),
            None => println!("verdict: <no verdict emitted>"),
        }
    }

    // The tool's exit code is the session verdict, carried by the core's exit code
    // (docs/08 section 8).
    match status {
        Ok(s) => s.code().unwrap_or(3),
        Err(_) => 3,
    }
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

fn core_mode() -> i32 {
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

    // Handshake first, before doing any work.
    emit(&Event::Ready {
        v: PROTOCOL_VERSION,
        protocol: PROTOCOL_VERSION,
        core_version: CORE_VERSION.into(),
        bitness: this_bitness().into(),
        capabilities: vec![],
    });

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
            let verdict = verdict_from_coverage(&prepared.coverage);
            let covered: Vec<CoveredChannel> = prepared
                .coverage
                .covered
                .iter()
                .map(|c| CoveredChannel { channel: c.channel.clone(), calls: c.calls })
                .collect();
            emit(&Event::Coverage {
                v: PROTOCOL_VERSION,
                pid: prepared.session.pid,
                covered,
                uncovered: prepared.coverage.uncovered.clone(),
                warning_keys: vec![],
            });

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
    }
}

/// Drive a running session: emit a ~1 s `state` heartbeat, answer `query`, and stop
/// on `end`, on stdin EOF, or when the target exits. Returns the verdict's exit code.
fn run_session(
    session: chrono_mech::Session,
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
            Ok(Command::Jump { id, to, .. }) => match moment_from_spec(&to) {
                Ok(ft) => {
                    session.jump(ft);
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
            },
            Ok(_) => {} // Start or a not-yet-supported command: ignore
            Err(mpsc::RecvTimeoutError::Timeout) => {
                emit(&state_event(&session));
                if !session.is_alive() {
                    target_exit = session.exit_code();
                    break;
                }
                deadline = Instant::now() + heartbeat;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break, // stdin closed
        }
    }

    session.end();
    emit(&Event::Ended {
        v: PROTOCOL_VERSION,
        clean: true,
        residue_keys: vec![],
        target_exit_code: target_exit,
    });
    verdict.exit_code()
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
}
