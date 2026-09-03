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
use std::io::{BufRead, BufReader, Write};
use std::process::{Command as PCommand, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Deserialize;

use chrono_core::calc::{
    Base, EvalContext, EvalError, MomentExpr, NearestTarget, Sign, SnapTarget, Step, Unit,
};
use chrono_core::{filetime_utc_to_wall, verdict_from_coverage, Moment, SessionSpec, TimeMode, Verdict};
use chrono_proto::{
    parse_command, Clock, Command, CoveredChannel, Event, MomentSpec, TargetSpec, TimeSpec,
    PROTOCOL_VERSION,
};

/// The Chromium/Electron substitution mechanism (CDP faketime), a parallel path to the native core.
mod cdp;

const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

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

fn print_usage() {
    eprintln!("usage: chrono run <target> [--at <local-moment>] [--preset <id>] [--param id=value]... [--zone <+HH:MM>] [--mode <flow|frozen|xN>] [--scale-duration] [--ticks N] [--set-after T:M] [--jump-after T:moment] [--args \"...\"] [--report <path>] [--force] [--json]");
    eprintln!("       without --at (or --preset) the session clock starts at the real current time, so `--mode xN` alone just runs the target faster");
    eprintln!("       --force runs on even when the opening verdict says the substitution did not take effect (the target is stopped otherwise)");
    eprintln!("       (--preset supplies the moment and mode from presets/<id>.json, exclusive of --at/--mode/--scale-duration; --param fills its parameters, a trial start_date defaults to the target's file date)");
    print_calc_usage();
}

fn print_calc_usage() {
    eprintln!("usage: chrono calc [--base <today|now|YYYY-MM-DDTHH:MM:SS>] [--shift <±N<unit>>]... [--set-time <HH:MM:SS>] [--snap <target>] [--nearest <target>] [--to-zone <+HH:MM>] [--zone <+HH:MM>] [--calendar <us-banking|us-federal|pl>] [--format <mask>] [--json]");
    eprintln!("       or: chrono calc --preset <id> [--param id=value]...   (named moment, e.g. month-end, trial-first-day-after)");
    eprintln!("       or: chrono calc --analyze <pasted-date>   (interpret a date, e.g. 04/08/2008; shows both readings when ambiguous)");
    eprintln!("       --json emits machine output (chronomock.calc/1) for any of the above");
    eprintln!("       units: s m h d w mo q y bd (minute=m, month=mo)");
}

fn this_bitness() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "x64"
    } else {
        "x86"
    }
}

/// Hidden probe (CDP slice C1 verification): connect to a running Chromium/Electron debug port and
/// print what CDP sees. Not a user command - it proves the WebSocket + JSON-RPC transport against a
/// real target before the launch, shim, and report wiring are built on top.
fn cdp_probe(argv: &[String]) -> i32 {
    let port: u16 = match argv.first().and_then(|s| s.parse().ok()) {
        Some(p) => p,
        None => {
            eprintln!("usage: chrono __cdp-probe <port>");
            return 1;
        }
    };
    let mut client = match cdp::CdpClient::connect_to_port("127.0.0.1", port) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("chrono: cannot connect to CDP on port {port}: {e}");
            return 3;
        }
    };
    match client.call("Browser.getVersion", serde_json::json!({}), None) {
        Ok(v) => println!(
            "browser: {}",
            v.get("product").and_then(serde_json::Value::as_str).unwrap_or("?")
        ),
        Err(e) => {
            eprintln!("chrono: Browser.getVersion: {e}");
            return 3;
        }
    }
    match client.call("Target.getTargets", serde_json::json!({}), None) {
        Ok(v) => {
            let empty = Vec::new();
            let infos = v.get("targetInfos").and_then(serde_json::Value::as_array).unwrap_or(&empty);
            println!("targets: {}", infos.len());
            for t in infos {
                let ty = t.get("type").and_then(serde_json::Value::as_str).unwrap_or("?");
                let url = t.get("url").and_then(serde_json::Value::as_str).unwrap_or("");
                let shown = if url.len() > 70 { &url[..70] } else { url };
                println!("  - {ty} :: {shown}");
            }
        }
        Err(e) => {
            eprintln!("chrono: Target.getTargets: {e}");
            return 3;
        }
    }
    0
}

/// Hidden probe (CDP slice C2 verification): detect a Chromium/Electron target, launch it under our
/// own isolated profile and debug port, connect, and clean up. Proves detection + launch end to end
/// (the tool starts the app itself, unlike the C1 probe which attached to an already-running port).
fn cdp_launch_probe(argv: &[String]) -> i32 {
    let Some(target) = argv.first() else {
        eprintln!("usage: chrono __cdp-launch <target-exe>");
        return 1;
    };
    if !cdp::is_chromium_target(target) {
        println!("not a chromium target: {target}");
        return 1;
    }
    println!("chromium target detected: {target}");

    let launched = match cdp::launch_chromium(target, &[]) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("chrono: {e}");
            return 3;
        }
    };
    println!("debug port: {}", launched.port);

    let code = match cdp::CdpClient::connect_to_port("127.0.0.1", launched.port) {
        Ok(mut c) => match c.call("Browser.getVersion", serde_json::json!({}), None) {
            Ok(v) => {
                println!(
                    "browser: {}",
                    v.get("product").and_then(serde_json::Value::as_str).unwrap_or("?")
                );
                0
            }
            Err(e) => {
                eprintln!("chrono: Browser.getVersion: {e}");
                3
            }
        },
        Err(e) => {
            eprintln!("chrono: connect: {e}");
            3
        }
    };

    launched.shutdown();
    println!("shut down and cleaned up");
    code
}

fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Hidden probe (CDP slice C3 verification): launch a Chromium target, auto-attach to every context,
/// inject the time shim through the production path, then (Pomotroid-specific) drive its worker timer
/// and measure whether the app's own countdown accelerates by the multiplier. Proves the shim reaches
/// the sandboxed worker and speeds it up - the whole point of Chromium mode.
fn cdp_shim_probe(argv: &[String]) -> i32 {
    let Some(target) = argv.first() else {
        eprintln!("usage: chrono __cdp-shim <target-exe> [multiplier]");
        return 1;
    };
    let mult: i64 = argv.get(1).and_then(|s| s.parse().ok()).unwrap_or(60);
    if !cdp::is_chromium_target(target) {
        println!("not a chromium target: {target}");
        return 1;
    }

    let launched = match cdp::launch_chromium(target, &[]) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("chrono: {e}");
            return 3;
        }
    };
    let mut client = match cdp::CdpClient::connect_to_port("127.0.0.1", launched.port) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("chrono: connect: {e}");
            launched.shutdown();
            return 3;
        }
    };

    // Pure acceleration for the proof: fake start = real start (the absolute wall moment is C5).
    let now = now_epoch_ms();
    let shim = cdp::build_shim(now, now, mult);
    println!("multiplier: x{mult}, injecting shim into all contexts...");

    if let Err(e) = client.call(
        "Target.setAutoAttach",
        serde_json::json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
        None,
    ) {
        eprintln!("chrono: setAutoAttach: {e}");
        launched.shutdown();
        return 3;
    }

    // Drain attach events, shimming each context. Stop once the Pomodoro worker is covered.
    let mut worker_sid: Option<String> = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while worker_sid.is_none() && std::time::Instant::now() < deadline {
        match client.poll() {
            Ok(Some(cdp::Msg::Event { method, params, .. })) if method == "Target.attachedToTarget" => {
                let sid = params["sessionId"].as_str().unwrap_or("").to_string();
                let ty = params["targetInfo"]["type"].as_str().unwrap_or("").to_string();
                let url = params["targetInfo"]["url"].as_str().unwrap_or("").to_string();
                if !cdp::is_shimmable(&ty) {
                    continue;
                }
                let r = if cdp::is_worker(&ty) {
                    cdp::inject_worker(&mut client, &sid, &shim)
                } else {
                    cdp::inject_page(&mut client, &sid, &shim)
                };
                match r {
                    Ok(()) => println!("  shimmed {ty} :: {}", short_url(&url)),
                    Err(e) => println!("  FAILED  {ty}: {e}"),
                }
                if cdp::is_worker(&ty) && url.contains(".worker.js") {
                    worker_sid = Some(sid);
                }
            }
            Ok(_) => {} // another message, or a poll timeout - keep waiting
            Err(e) => {
                eprintln!("chrono: event loop: {e}");
                break;
            }
        }
    }

    let Some(sid) = worker_sid else {
        println!("no timer worker found (this proof needs Pomotroid); shim still installed on contexts above");
        launched.shutdown();
        return 1;
    };

    // Measurement stimulus (Pomotroid-specific): capture the worker's own elapsed via its postMessage,
    // then drive create+start. The shim already scaled setInterval, so elapsed should climb by x{mult}.
    let cap = "if(!globalThis.__cap){var _pm=self.postMessage;self.postMessage=function(m){try{if(m&&m.event==='tick'){globalThis.__last=m.elapsed;}}catch(e){}return _pm.call(self,m);};globalThis.__cap=true;globalThis.__last=0;} 'cap'";
    let trigger = "self.onmessage({data:{event:'create',min:25}});self.onmessage({data:{event:'start'}});'go'";
    let _ = client.call("Runtime.evaluate", serde_json::json!({ "expression": cap, "returnByValue": true }), Some(&sid));
    let start = std::time::Instant::now();
    let _ = client.call("Runtime.evaluate", serde_json::json!({ "expression": trigger, "returnByValue": true }), Some(&sid));

    println!("measuring the app's own countdown:");
    let mut last_elapsed = 0i64;
    for _ in 0..6 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let read = client.call("Runtime.evaluate", serde_json::json!({ "expression": "globalThis.__last||0", "returnByValue": true }), Some(&sid));
        if let Ok(v) = read {
            last_elapsed = v.get("result").and_then(|r| r.get("value")).and_then(serde_json::Value::as_i64).unwrap_or(0);
            let real = start.elapsed().as_secs_f64();
            println!("  real {real:.2}s -> app countdown advanced {last_elapsed}s");
        }
    }
    let real = start.elapsed().as_secs_f64();
    let rate = if real > 0.0 { last_elapsed as f64 / real } else { 0.0 };
    println!("=== app-time/real-time = ~{rate:.1}x (expected ~{mult}x) ===");

    // Validate the audit read path (used by `chrono run`): the worker called setInterval, so its
    // count must be > 0.
    if let Ok(v) = client.call("Runtime.evaluate", serde_json::json!({ "expression": cdp::COUNTS_EXPR, "returnByValue": true }), Some(&sid)) {
        let counts = v.get("result").and_then(|r| r.get("value")).map(|x| x.to_string()).unwrap_or_else(|| "null".into());
        println!("worker call counts (audit read): {counts}");
    }

    launched.shutdown();
    if rate > (mult as f64) * 0.5 {
        println!("PROVEN: the shim accelerated the sandboxed worker countdown.");
        0
    } else {
        println!("NOT PROVEN: no meaningful speed-up.");
        1
    }
}

/// Hidden probe (CDP slice C5 verification): shim a target's page at a given fake moment and read its
/// Date back, confirming new Date()/Date.now() see the session clock (not just setInterval scaling).
fn cdp_date_probe(argv: &[String]) -> i32 {
    let (Some(target), Some(iso)) = (argv.first(), argv.get(1)) else {
        eprintln!("usage: chrono __cdp-date <target-exe> <YYYY-MM-DDTHH:MM:SS>");
        return 1;
    };
    if !cdp::is_chromium_target(target) {
        println!("not a chromium target: {target}");
        return 1;
    }
    let real = now_epoch_ms();
    let fake = moment_epoch_ms(iso, Some(0)).unwrap_or(real); // the probe treats the moment as UTC
    let shim = cdp::build_shim(fake, real, 1); // flow: a wall offset, no acceleration

    let launched = match cdp::launch_chromium(target, &[]) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("chrono: {e}");
            return 3;
        }
    };
    let mut client = match cdp::CdpClient::connect_to_port("127.0.0.1", launched.port) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("chrono: connect: {e}");
            launched.shutdown();
            return 3;
        }
    };
    if client
        .call("Target.setAutoAttach", serde_json::json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }), None)
        .is_err()
    {
        launched.shutdown();
        return 3;
    }

    let mut page_sid: Option<String> = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while page_sid.is_none() && std::time::Instant::now() < deadline {
        match client.poll() {
            Ok(Some(cdp::Msg::Event { method, params, .. })) if method == "Target.attachedToTarget" => {
                let sid = params["sessionId"].as_str().unwrap_or("").to_string();
                let ty = params["targetInfo"]["type"].as_str().unwrap_or("").to_string();
                if ty == "page" && !sid.is_empty() && cdp::inject_page(&mut client, &sid, &shim).is_ok() {
                    page_sid = Some(sid);
                }
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("chrono: {e}");
                break;
            }
        }
    }

    let code = if let Some(sid) = page_sid {
        let expr = "JSON.stringify([new Date().toISOString(), new Date().getUTCFullYear(), Date.now()])";
        match client.call("Runtime.evaluate", serde_json::json!({ "expression": expr, "returnByValue": true }), Some(&sid)) {
            Ok(v) => {
                let reads = v.get("result").and_then(|r| r.get("value")).and_then(serde_json::Value::as_str).unwrap_or("?");
                println!("requested moment: {iso}");
                println!("page reads [new Date().toISOString(), getUTCFullYear(), Date.now()]: {reads}");
                0
            }
            Err(e) => {
                eprintln!("chrono: eval: {e}");
                3
            }
        }
    } else {
        println!("no page attached");
        1
    };

    launched.shutdown();
    code
}

/// A URL trimmed for a diagnostic line. By CHARACTERS, not bytes: `&url[..66]` panics when byte 66
/// lands inside a multi-byte character, and a debug URL can carry a profile path with a non-ASCII user
/// name. Only the hidden probes print this, so it was never on a user path - but a panic to shorten a
/// log line is a poor trade either way.
fn short_url(url: &str) -> String {
    url.chars().take(66).collect()
}

/// FILETIME ticks (100 ns since 1601) at the Unix epoch (1970-01-01T00:00:00Z).
const FT_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

/// Unix-epoch ms for a session moment (local-in-zone, internally UTC - rule 2), reusing the core's
/// anchor math so the CDP and native paths agree on the instant. `None` if the moment is out of the
/// representable range.
fn moment_epoch_ms(local: &str, bias: Option<i32>) -> Option<i64> {
    let m = Moment { local: local.to_string(), tz_bias_min: bias };
    let ft = chrono_core::moment_to_filetime_utc(&m).ok()?;
    Some((ft - FT_UNIX_EPOCH) / 10_000)
}

/// Session-zone wall-clock text for a Unix-epoch ms instant, via the core formatter (one source of
/// truth for the civil half).
fn epoch_ms_to_wall(epoch_ms: i64, bias: i32) -> String {
    let ft = epoch_ms.saturating_mul(10_000).saturating_add(FT_UNIX_EPOCH);
    filetime_utc_to_wall(ft, bias)
}

/// One shimmed JS context of a Chromium target: the coverage unit of a CDP session (rule 4 - never
/// summed across contexts).
struct CdpContext {
    index: u32,
    session_id: String,
    ty: String,
    /// The CDP targetId, kept because `Target.targetDestroyed` names a target, not a session.
    target_id: String,
}

/// Read each live context's per-API call counts and merge them (by max, so a peak survives a reload)
/// into `counts`, keyed by `(context index, "type api")`. Returns whether any context answered - a
/// dead context (the app closed) simply errors and is skipped, so the audit stays honest.
fn poll_counts(
    client: &mut cdp::CdpClient,
    contexts: &[CdpContext],
    counts: &mut std::collections::BTreeMap<(u32, String), u64>,
) -> bool {
    let mut any = false;
    for c in contexts {
        let read = client.call(
            "Runtime.evaluate",
            serde_json::json!({ "expression": cdp::COUNTS_EXPR, "returnByValue": true }),
            Some(&c.session_id),
        );
        if let Ok(v) = read {
            if let Some(obj) = v.get("result").and_then(|x| x.get("value")).and_then(serde_json::Value::as_object) {
                any = true;
                for (api, key) in [("setInterval", "si"), ("setTimeout", "st"), ("Date.now", "now"), ("performance.now", "perf")] {
                    if let Some(n) = obj.get(key).and_then(serde_json::Value::as_u64) {
                        let entry = counts.entry((c.index, format!("{} {}", c.ty, api))).or_insert(0);
                        *entry = (*entry).max(n);
                    }
                }
            }
        }
    }
    any
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

fn parse_run_args(argv: &[String]) -> Result<RunArgs, String> {
    let mut target: Option<String> = None;
    let mut args: Vec<String> = Vec::new();
    let mut at: Option<String> = None;
    let mut zone_bias_min: Option<i32> = None;
    let mut mode = String::from("flow");
    let mut multiplier: Option<i64> = None;
    let mut scale_duration = false;
    let mut scale_qpc = false;
    let mut force = false;
    let mut ticks: u64 = 0;
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
        at,
        zone_bias_min,
        mode,
        multiplier,
        scale_duration,
        scale_qpc,
        force,
        ticks,
        set_after,
        jump_after,
        report,
        json,
        preset,
        params,
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
    // Parse as u32 so an INNER sign (e.g. `+-5:00` or `+05:-30`) is rejected, not silently taken as a
    // negative component (M-4). Range-check hours 0..=14 (the max real offset is +14:00) and minutes
    // 0..=59, which also keeps `hours * 60 + mins` well inside i32: the old i32 parse overflowed on a
    // huge value and, in a release build (no overflow-checks), WRAPPED to a wrong bias - a silently
    // wrong session time, exactly the "off by N hours" error untouchable rule 2 guards against.
    let hours: u32 = h.parse().map_err(|_| format!("bad zone hours in '{raw}' (digits only)"))?;
    let mins: u32 = m.parse().map_err(|_| format!("bad zone minutes in '{raw}' (digits only)"))?;
    if hours > 14 {
        return Err(format!("zone hours out of range in '{raw}' (0..=14)"));
    }
    if mins > 59 {
        return Err(format!("zone minutes out of range in '{raw}' (0..=59)"));
    }
    let offset = sign * (hours as i32 * 60 + mins as i32);
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
        EvalError::NotFound { .. } => "relative --at found no matching date".to_string(),
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
                let target_date = read_target_creation_date(&ra.target, ra.zone_bias_min);
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
                let now = match resolve_now_civil(ra.zone_bias_min) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("chrono: cannot resolve current time: {e}");
                        return 3;
                    }
                };
                match chrono_core::calc::eval(
                    &moment,
                    &EvalContext { now, zone_bias_min: ra.zone_bias_min.unwrap_or(0), calendar: None },
                ) {
                    Ok(outcome) => (
                        Some(outcome.result().to_iso()),
                        p.time_mode.mode.clone(),
                        p.time_mode.multiplier,
                        p.time_mode.scale_duration,
                        ra.zone_bias_min,
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
        // With no moment asked for, the session zone follows the HOST when the caller named none. Reading
        // "now" as UTC there would hand the target a local time off by the host's own offset - a session
        // meant to change nothing would quietly move the clock by two hours here, which is exactly the
        // failure untouchable rule 2 names. An explicit --zone still wins, and an entered --at is
        // untouched: that moment is in the session zone the caller chose, as before.
        let now_bias = ra.zone_bias_min.unwrap_or_else(chrono_mech::host_tz_bias_min);

        // Resolve a relative --at (now + delta) to an absolute moment before we spawn. With no --at at
        // all, the session clock starts at the real current time - the same thing the Chromium path
        // already does with the same command line. Until now the two mechanisms disagreed: on a native
        // target the empty moment travelled all the way into the core and came back as "moment must be
        // YYYY-MM-DDTHH:MM:SS, got ''" - an error about a flag the usage line calls optional, raised as
        // far from the user's mistake as it could be. It also makes `chrono run app.exe --mode x60`
        // mean what it reads as: run this application faster without moving its date.
        let resolved = match &ra.at {
            Some(raw) => match resolve_at(raw, ra.zone_bias_min) {
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
        // The zone the moment above was read in has to travel with it: the core turns local + bias into
        // the UTC anchor, so a moment resolved in the host zone paired with a bias of 0 would land an
        // offset away from the instant it names.
        let session_bias = if ra.at.is_some() { ra.zone_bias_min } else { Some(now_bias) };
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
        target: TargetSpec {
            path: ra.target.clone(),
            args: ra.args.clone(),
            cwd: None,
        },
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
        // The core auto-detects a Chromium target and runs it over CDP; label the report's coverage
        // unit accordingly. Same pure function, same path string the core sees, so the two never drift.
        cdp: cdp::is_chromium_target(&ra.target),
    };
    if let Some(path) = &ra.report {
        let params = EvidenceParams {
            moment: resolved_at.clone().unwrap_or_else(|| "(default)".into()),
            zone: ra.zone_bias_min.map(format_bias).unwrap_or_else(|| "(host default)".into()),
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
        "chromium.launched_with_debug_port" => {
            "an Electron/Chromium app: launched with a remote-debugging port and a clean isolated profile, not your real one"
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

    // Reverse analysis (7.3): with --analyze, interpret a pasted date instead of building a moment.
    // The build flags (--base/--shift/...) do not apply; --calendar and --zone still do.
    if let Some(input) = &ca.analyze {
        return match chrono_core::calc::analyze_date(input) {
            Ok(analysis) => {
                if ca.json {
                    println!("{}", calc_analysis_json(&analysis, input, &now, ca.zone_bias_min, calendar.as_ref()));
                } else {
                    print!("{}", render_analysis(&analysis, input, &now, ca.zone_bias_min, calendar.as_ref()));
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
        &EvalContext { now, zone_bias_min: ca.zone_bias_min.unwrap_or(0), calendar: calendar.as_ref() },
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
                render_calc(&expr, &outcome, ca.zone_bias_min, &now, calendar.as_ref(), preset_header.as_deref());
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

/// Parse a `--snap` target token (full or short form) into a typed target.
fn parse_snap(raw: &str) -> Result<SnapTarget, String> {
    Ok(match raw {
        "start-of-month" | "som" => SnapTarget::StartOfMonth,
        "end-of-month" | "eom" => SnapTarget::EndOfMonth,
        "start-of-quarter" | "soq" => SnapTarget::StartOfQuarter,
        "end-of-quarter" | "eoq" => SnapTarget::EndOfQuarter,
        "start-of-year" | "soy" => SnapTarget::StartOfYear,
        "end-of-year" | "eoy" => SnapTarget::EndOfYear,
        other => {
            return Err(format!("unknown snap target '{other}' (use start-of/end-of month|quarter|year)"))
        }
    })
}

/// Parse a `--nearest` target token into a typed target.
fn parse_nearest(raw: &str) -> Result<NearestTarget, String> {
    Ok(match raw {
        "next-business-day" | "nbd" => NearestTarget::NextBusinessDay,
        "prev-business-day" | "previous-business-day" | "pbd" => NearestTarget::PrevBusinessDay,
        "next-leap-day" | "next-feb-29" | "nld" => NearestTarget::NextLeapDay,
        other => {
            return Err(format!(
                "unknown nearest target '{other}' (next-business-day, prev-business-day, next-leap-day)"
            ))
        }
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
        EvalError::StepUnsupported { .. } | EvalError::NeedsCalendar { .. } | EvalError::NotFound { .. } => 5,
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
        EvalError::NeedsCalendar { index } => {
            format!("chrono calc: step {} needs a calendar - pass --calendar (calc.needs_calendar)", index + 1)
        }
        EvalError::NotFound { index } => {
            format!("chrono calc: step {} found no matching date in range (calc.not_found)", index + 1)
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
    preset_header: Option<&str>,
) -> String {
    let zone = zone_bias_min.map(format_bias).unwrap_or_else(|| "UTC".into());
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

/// Validate and map one holiday rule. The engine trusts its inputs, and a calendar is a data file from
/// outside the build - the one documented extension point of this tool - so an out-of-range field must be
/// refused HERE, naming the field. Left unchecked it does not fail, which is worse: `days_from_civil`
/// happily rolls month 13 into the next January and day 40 into the following month, so the calendar
/// silently marks the wrong dates as holidays and every business-day answer built on it is quietly wrong.
fn rule_from(id: &str, dto: RuleDto) -> Result<chrono_core::calendar::HolidayRule, String> {
    use chrono_core::calendar::HolidayRule;
    Ok(match dto {
        RuleDto::Fixed { month, day } => {
            check_month(id, month)?;
            // 1..=31 for the day, not the month's real length: a rule may legitimately name Feb 29,
            // and the engine resolves an impossible date per year. Beyond 31 is a typo in any month.
            if !(1..=31).contains(&day) {
                return Err(format!("holiday '{id}': day {day} out of range (1..=31)"));
            }

            HolidayRule::Fixed { month, day }
        }
        RuleDto::NthWeekday { month, weekday, order } => {
            check_month(id, month)?;
            // -1 = "the last such weekday in the month"; 1..=5 counts from the start (5 exists only in
            // some months, which the engine already resolves). Anything else silently lands in another
            // month, e.g. order 9 walking past the end.
            if order != -1 && !(1..=5).contains(&order) {
                return Err(format!(
                    "holiday '{id}': order {order} out of range (-1 for last, or 1..=5)"
                ));
            }

            HolidayRule::NthWeekday { month, weekday: weekday_index(&weekday)?, order }
        }
        RuleDto::EasterOffset { offset } => {
            // A year either side of Easter covers every real observance (Corpus Christi is +60).
            if !(-366..=366).contains(&offset) {
                return Err(format!(
                    "holiday '{id}': easter offset {offset} out of range (-366..=366 days)"
                ));
            }

            HolidayRule::EasterOffset { offset }
        }
    })
}

fn check_month(id: &str, month: u32) -> Result<(), String> {
    if !(1..=12).contains(&month) {
        return Err(format!("holiday '{id}': month {month} out of range (1..=12)"));
    }

    Ok(())
}

/// Locate a calendar file: next to the executable (portable layout), else in ./calendars.
/// Whether a catalogue id (calendar / preset) is safe to turn into a file name: non-empty and made
/// only of letters, digits, '-' and '_'. This rejects any path separator, '..', drive letter, or
/// ADS colon BEFORE the id becomes a path, so `--preset ../../secret` cannot read a file outside the
/// catalogue directory (docs/04 4.1 - a shared catalogue entry can never smuggle a path).
fn is_valid_catalogue_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn find_calendar_file(id: &str) -> Result<std::path::PathBuf, String> {
    if !is_valid_catalogue_id(id) {
        return Err(format!("invalid calendar id '{id}' (use letters, digits, '-' or '_')"));
    }
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
    calendar_from_text(&text).map_err(|e| format!("{e} (in {})", path.display()))
}

/// Parse and validate a calendar from its JSON text, mapping the `chronomock.calendar/1` schema to the
/// engine's types. Separated from the on-disk lookup (symmetry with `parse_preset`) so the shipped
/// calendars can be golden-tested against the real engine without the file-resolution step.
fn calendar_from_text(text: &str) -> Result<chrono_core::calendar::Calendar, String> {
    let dto: CalendarDto = serde_json::from_str(text).map_err(|e| format!("bad calendar JSON: {e}"))?;
    // An unknown major schema version is refused, not half-understood (docs/04 section 3.1).
    if dto.schema != "chronomock.calendar/1" {
        return Err(format!(
            "unsupported calendar schema '{}' (this build reads chronomock.calendar/1)",
            dto.schema
        ));
    }
    let mut weekend = dto.weekend.iter().map(|w| weekday_index(w)).collect::<Result<Vec<_>, _>>()?;
    // Duplicates are harmless to the engine (membership is a contains) but they hide a typo, and
    // they make the count below meaningless - so fold them away before counting.
    weekend.sort_unstable();
    weekend.dedup();
    // A week with no working day leaves "+1 business day" with nothing to land on. The engine now
    // bounds its walk instead of hanging (S-1), but a file this broken should never reach it: say
    // which field is wrong, here, where the author can fix it.
    if weekend.len() >= 7 {
        return Err(
            "calendar 'weekend' lists all seven days - no business day would ever exist".to_string()
        );
    }
    let mut seen_ids: Vec<String> = Vec::new();
    let holidays = dto
        .holidays
        .into_iter()
        .map(|h| {
            // A duplicate id makes the audit ambiguous - `holiday_on` names one of them and the reader
            // cannot tell which. Cheap to catch, impossible to diagnose later.
            if seen_ids.contains(&h.id) {
                return Err(format!("duplicate holiday id '{}'", h.id));
            }

            // An inverted window silently means "never a holiday", which reads as a missing entry
            // rather than as the mistake it is.
            if let (Some(from), Some(to)) = (h.valid_from, h.valid_to) {
                if from > to {
                    return Err(format!(
                        "holiday '{}': valid_from {from} is after valid_to {to}",
                        h.id
                    ));
                }
            }

            seen_ids.push(h.id.clone());
            let rule = rule_from(&h.id, h.rule)?;
            Ok(chrono_core::calendar::Holiday {
                id: h.id,
                name_en: h.name.en,
                name_local: h.name.local,
                rule,
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

/// A parsed preset: its declared parameters and its RAW moment (docs/04 4.3), not yet resolved to a
/// `MomentExpr` - because a parametric base/shift needs values (`--param` / `default`) that the file
/// alone does not carry. `resolve_parameters` + `resolve_moment` turn it into a concrete moment.
/// A non-parametric preset (slices 16/17) has empty `parameters` and resolves trivially. Also carries
/// the human framing (calculator) and the time mode (substitution); the calculator ignores time_mode.
#[derive(Debug)]
struct Preset {
    id: String,
    name_en: String,
    explains_en: String,
    /// `calculator` / `substitution` / `both` (docs/04 4.2). Each surface honours it.
    applies_to: String,
    parameters: Vec<Parameter>,
    moment: MomentDto,
    time_mode: PresetTimeMode,
}

/// A preset parameter (docs/04 4.2): a typed slot filled by `--param`, a file `default`, or (in a
/// substitution session, a later slice) a `default_hint` such as the target's file date.
#[derive(Debug)]
struct Parameter {
    id: String,
    kind: ParamKind,
    default: Option<ParamValue>,
    /// Where to propose a value from when neither `--param` nor `default` is given (docs/04 4.2).
    /// `target_file_creation` needs a target, so it is honoured only in `run` (a later slice); the
    /// calculator, having no target, reports it as a value the user must supply.
    default_hint: Option<String>,
}

/// A parameter's type. `date` fills a base; `duration` and `variant` fill a shift (a `variant` by a
/// signed day offset - docs/05 3.6). (`int` is later.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParamKind {
    Date,
    Duration,
    Variant,
}

/// A resolved parameter value, ready to substitute into the moment.
#[derive(Debug, Clone)]
enum ParamValue {
    Date(chrono_core::calc::CivilDateTime),
    Duration { amount: i64, unit: Unit },
    /// A boundary variant (docs/05 3.6) resolved to a signed day offset: day_before -1, on_day 0,
    /// day_after +1. It fills a shift and carries its own direction, so that shift step needs no sign.
    Variant(i64),
}

/// A preset's time mode, resolved to the substitution surface's wire shape (the same `mode` /
/// `multiplier` / `scale_duration` a `run` session carries). The contract carries `multiplier` and
/// `scale_duration_clock` (docs/04 4.2); `multiplier == 1` is real-time `flow`, `> 1` is `xN`.
#[derive(Debug, Clone)]
struct PresetTimeMode {
    /// Wire mode token: "flow" or "multiplier". (Presets do not express "frozen".)
    mode: String,
    multiplier: Option<i64>,
    scale_duration: bool,
}

impl Default for PresetTimeMode {
    /// A preset with no `time_mode` (e.g. a calculator-only one) runs at real speed.
    fn default() -> Self {
        Self { mode: "flow".into(), multiplier: None, scale_duration: false }
    }
}

/// Why a preset could not be loaded for the calculator. Enumerated, not a shared string, so the
/// exit code follows the cause: input problems are usage errors (1), while "the model has this in
/// it but calc does not resolve it yet" is the honest not-built code (5), same split as calc's own
/// error table (docs/08 section 9a).
#[derive(Debug)]
enum PresetError {
    /// No such preset file.
    NotFound(String),
    /// Bad JSON, unknown schema, or a malformed field.
    BadFile(String),
    /// A shape the model allows but this build does not resolve yet (parameters).
    NotBuilt(String),
}

impl PresetError {
    fn exit_code(&self) -> i32 {
        match self {
            PresetError::NotBuilt(_) => 5,
            PresetError::NotFound(_) | PresetError::BadFile(_) => 1,
        }
    }
    fn message(&self) -> &str {
        match self {
            PresetError::NotFound(m) | PresetError::BadFile(m) | PresetError::NotBuilt(m) => m,
        }
    }
}

#[derive(Deserialize)]
struct PresetDto {
    schema: String,
    id: String,
    name: PresetTextDto,
    explains: PresetTextDto,
    applies_to: String,
    // Typed parameters (docs/04 4.2): each is filled by --param, a file default, or a default_hint.
    // The moment's parametric base/shift refer to these by id; resolve_parameters + resolve_moment
    // substitute them into a concrete MomentExpr (the core never learns a parameter existed).
    #[serde(default)]
    parameters: Vec<ParameterDto>,
    moment: MomentDto,
    // Only substitution/both presets carry a time mode; a calculator-only one may omit it (the
    // calculator ignores it either way). Absent = real-time flow (PresetTimeMode::default).
    #[serde(default)]
    time_mode: Option<TimeModeDto>,
}

/// A preset's `time_mode` object (docs/04 4.2): `{ "multiplier": N, "scale_duration_clock": bool }`.
#[derive(Deserialize)]
struct TimeModeDto {
    #[serde(default)]
    multiplier: Option<i64>,
    #[serde(default)]
    scale_duration_clock: bool,
}

/// A preset parameter as written in the file (docs/04 4.2): `{ "id", "type", "default"?, "default_hint"? }`.
/// `default` shape depends on `type` (a string for `date`, `{ amount, unit }` for `duration`), so it
/// stays a raw value here and is parsed against the type in `parse_parameter`.
#[derive(Deserialize)]
struct ParameterDto {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    default: Option<serde_json::Value>,
    #[serde(default)]
    default_hint: Option<String>,
}

/// A `duration` value/`default` object: `{ "amount": N, "unit": "days" }` (docs/04 4.2).
#[derive(Deserialize)]
struct DurationDto {
    amount: i64,
    unit: String,
}

/// The English text is what the CLI renders (rule 15 - CLI is English only); `pl` rides in the file
/// for the GUI but this reader does not need it, so it is not a field here (unknown fields ignored).
#[derive(Deserialize)]
struct PresetTextDto {
    en: String,
}

#[derive(Debug, Deserialize)]
struct MomentDto {
    base: BaseDto,
    #[serde(default)]
    steps: Vec<StepDto>,
}

/// A preset base: the keyword `today`/`now`, an `{ "absolute": "ISO" }` object, or a
/// `{ "parameter": "name" }` object (docs/04 4.2) resolved from a `date` parameter.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BaseDto {
    Keyword(String),
    Object {
        #[serde(default)]
        absolute: Option<String>,
        #[serde(default)]
        parameter: Option<String>,
    },
}

/// One preset step, externally tagged exactly as docs/04 4.2 writes it: `{ "shift": {...} }`,
/// `{ "set_time": "HH:MM:SS" }`, `{ "snap": "end-of-month" }`, `{ "nearest": "next-business-day" }`,
/// `{ "zone": "+05:45" }`. The string forms reuse the CLI parsers, keeping one grammar.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StepDto {
    Shift(ShiftDto),
    SetTime(String),
    Snap(String),
    Nearest(String),
    Zone(String),
}

/// A `shift` step in a preset: a literal `{ sign, amount, unit }`, a parametric `{ sign, parameter }`
/// resolved from a `duration`, or a parametric `{ parameter }` resolved from a `variant` (docs/04 4.2).
/// The sign is optional because a `variant` parameter carries its own direction (docs/05 3.6).
#[derive(Debug, Deserialize)]
struct ShiftDto {
    #[serde(default)]
    sign: Option<String>,
    #[serde(default)]
    amount: Option<i64>,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    parameter: Option<String>,
}

/// Whether a preset's `applies_to` makes it a calculator question (docs/04 4.2, docs/05 3.1).
fn preset_targets_calculator(applies_to: &str) -> bool {
    matches!(applies_to, "calculator" | "both")
}

/// Map a preset base to the core `Base`. A parametric base is refused (not built), never silently
/// treated as `today`.
fn base_from(dto: BaseDto, values: &HashMap<String, ParamValue>) -> Result<Base, PresetError> {
    match dto {
        BaseDto::Keyword(k) => match k.as_str() {
            "today" => Ok(Base::Today),
            "now" => Ok(Base::Now),
            other => Err(PresetError::BadFile(format!(
                "unknown preset base '{other}' (use today, now, or an absolute/parameter object)"
            ))),
        },
        // A parametric base takes its date from a `date` parameter (docs/04 4.2).
        BaseDto::Object { parameter: Some(id), .. } => match values.get(&id) {
            Some(ParamValue::Date(civil)) => Ok(Base::Absolute(*civil)),
            Some(ParamValue::Duration { .. }) => {
                Err(PresetError::BadFile(format!("base parameter '{id}' must be a date, not a duration")))
            }
            Some(ParamValue::Variant(_)) => {
                Err(PresetError::BadFile(format!("base parameter '{id}' must be a date, not a variant")))
            }
            None => Err(PresetError::BadFile(format!("base parameter '{id}' has no value"))),
        },
        BaseDto::Object { absolute: Some(s), parameter: None } => {
            let civil = chrono_core::calc::parse_civil_datetime(&s).map_err(PresetError::BadFile)?;
            Ok(Base::Absolute(civil))
        }
        BaseDto::Object { absolute: None, parameter: None } => Err(PresetError::BadFile(
            "preset base object needs 'absolute' or 'parameter'".into(),
        )),
    }
}

/// Map a preset step to a core `Step`, reusing the CLI parsers so a preset speaks the same step
/// grammar as the flags. A `parameter` shift is resolved from the values map.
fn step_from(dto: StepDto, values: &HashMap<String, ParamValue>) -> Result<Step, PresetError> {
    match dto {
        StepDto::Shift(s) => shift_from(s, values),
        StepDto::SetTime(raw) => parse_set_time(&raw).map_err(PresetError::BadFile),
        StepDto::Snap(raw) => parse_snap(&raw).map(Step::Snap).map_err(PresetError::BadFile),
        StepDto::Nearest(raw) => parse_nearest(&raw).map(Step::Nearest).map_err(PresetError::BadFile),
        StepDto::Zone(raw) => parse_zone_to_bias(&raw).map(Step::Zone).map_err(PresetError::BadFile),
    }
}

fn shift_from(s: ShiftDto, values: &HashMap<String, ParamValue>) -> Result<Step, PresetError> {
    // A parametric shift takes its shape from a parameter: a `duration` gives magnitude and unit (the
    // step's sign carries direction), a `variant` gives a signed day offset (carrying its own sign).
    if let Some(id) = &s.parameter {
        return match values.get(id) {
            Some(ParamValue::Duration { amount, unit }) => {
                Ok(Step::Shift { sign: parse_shift_sign(&s.sign)?, amount: *amount, unit: *unit })
            }
            Some(ParamValue::Variant(days)) => {
                let sign = if *days < 0 { Sign::Minus } else { Sign::Plus };
                Ok(Step::Shift { sign, amount: days.abs(), unit: Unit::Days })
            }
            Some(ParamValue::Date(_)) => {
                Err(PresetError::BadFile(format!("shift parameter '{id}' must be a duration or variant, not a date")))
            }
            None => Err(PresetError::BadFile(format!("shift parameter '{id}' has no value"))),
        };
    }
    let sign = parse_shift_sign(&s.sign)?;
    let amount = s.amount.ok_or_else(|| PresetError::BadFile("shift needs an amount or a parameter".into()))?;
    if amount < 0 {
        return Err(PresetError::BadFile("shift amount must be non-negative (the sign carries direction)".into()));
    }
    let unit_str = s.unit.ok_or_else(|| PresetError::BadFile("shift needs a unit".into()))?;
    let unit = parse_unit(&unit_str).ok_or_else(|| PresetError::BadFile(format!("unknown unit '{unit_str}' in shift")))?;
    Ok(Step::Shift { sign, amount, unit })
}

/// Parse a shift step's `sign` field (`+`/`-`). Required for a literal or `duration`-parametric shift;
/// a `variant`-parametric shift omits it (the variant carries its own direction), so this runs only
/// where a sign is actually needed.
fn parse_shift_sign(sign: &Option<String>) -> Result<Sign, PresetError> {
    match sign.as_deref() {
        Some("+") => Ok(Sign::Plus),
        Some("-") => Ok(Sign::Minus),
        Some(other) => Err(PresetError::BadFile(format!("shift sign must be + or -, got '{other}'"))),
        None => Err(PresetError::BadFile("shift needs a sign (+ or -)".into())),
    }
}

/// Parse a preset from JSON WITHOUT resolving its moment (pure - no I/O, no parameter values). The
/// moment stays raw because a parametric base/shift needs values `resolve_parameters` supplies later;
/// a non-parametric preset resolves trivially (empty values). Unknown major schema is refused.
fn parse_preset(text: &str) -> Result<Preset, PresetError> {
    let dto: PresetDto =
        serde_json::from_str(text).map_err(|e| PresetError::BadFile(format!("bad preset JSON: {e}")))?;
    if dto.schema != "chronomock.preset/1" {
        return Err(PresetError::BadFile(format!(
            "unsupported preset schema '{}' (this build reads chronomock.preset/1)",
            dto.schema
        )));
    }
    let parameters = dto.parameters.into_iter().map(parse_parameter).collect::<Result<Vec<_>, _>>()?;
    let time_mode = time_mode_from(dto.time_mode)?;
    Ok(Preset {
        id: dto.id,
        name_en: dto.name.en,
        explains_en: dto.explains.en,
        applies_to: dto.applies_to,
        parameters,
        moment: dto.moment,
        time_mode,
    })
}

/// Parse one file parameter declaration into a typed `Parameter`, checking the type and any default.
fn parse_parameter(dto: ParameterDto) -> Result<Parameter, PresetError> {
    let kind = match dto.kind.as_str() {
        "date" => ParamKind::Date,
        "duration" => ParamKind::Duration,
        "variant" => ParamKind::Variant,
        other => {
            return Err(PresetError::NotBuilt(format!(
                "parameter '{}' has type '{other}', which calc does not resolve yet (built: date, duration, variant)",
                dto.id
            )))
        }
    };
    let default = match dto.default {
        Some(v) => Some(param_value_from_json(&dto.id, kind, &v)?),
        None => None,
    };
    Ok(Parameter { id: dto.id, kind, default, default_hint: dto.default_hint })
}

/// Parse a parameter's file `default` (a JSON value) against its declared type.
fn param_value_from_json(id: &str, kind: ParamKind, v: &serde_json::Value) -> Result<ParamValue, PresetError> {
    match kind {
        ParamKind::Date => {
            let s = v
                .as_str()
                .ok_or_else(|| PresetError::BadFile(format!("parameter '{id}' default must be a date string")))?;
            Ok(ParamValue::Date(parse_param_date(s).map_err(PresetError::BadFile)?))
        }
        ParamKind::Duration => {
            let d: DurationDto = serde_json::from_value(v.clone())
                .map_err(|_| PresetError::BadFile(format!("parameter '{id}' default must be {{ amount, unit }}")))?;
            duration_value(id, d.amount, &d.unit)
        }
        ParamKind::Variant => {
            let s = v
                .as_str()
                .ok_or_else(|| PresetError::BadFile(format!("parameter '{id}' default must be a variant label")))?;
            Ok(ParamValue::Variant(variant_days(id, s)?))
        }
    }
}

/// Resolve every declared parameter to a value: `--param` first, then the file `default`, then a
/// `default_hint` (in `run`, where a target exists), else an error. `target_date` carries the
/// target's file creation date on the run path (`None` in calc, where a hint stays an honest request
/// to pass `--param`). A `--param` naming no declared parameter is rejected - a silently ignored typo
/// is a wrong result, not a warning.
fn resolve_parameters(
    params: &[Parameter],
    cli: &HashMap<String, String>,
    target_date: Option<chrono_core::calc::CivilDateTime>,
) -> Result<HashMap<String, ParamValue>, PresetError> {
    for id in cli.keys() {
        if !params.iter().any(|p| &p.id == id) {
            return Err(PresetError::BadFile(format!("unknown parameter '{id}' for this preset")));
        }
    }
    let mut out = HashMap::new();
    for p in params {
        let value = if let Some(raw) = cli.get(&p.id) {
            parse_param_value(&p.id, p.kind, raw)?
        } else if let Some(def) = &p.default {
            def.clone()
        } else if let Some(hint) = &p.default_hint {
            resolve_hint(&p.id, p.kind, hint, target_date)?
        } else {
            return Err(PresetError::BadFile(format!("parameter '{}' has no value - pass --param {}=<value>", p.id, p.id)));
        };
        out.insert(p.id.clone(), value);
    }
    Ok(out)
}

/// Resolve a parameter's `default_hint` to a value. Only `target_file_creation` is built (docs/04
/// 4.2): it fills a `date` parameter from the target's file date, available only in `run`. Without a
/// target it is an honest "not built" asking for `--param`, not a guess.
fn resolve_hint(
    id: &str,
    kind: ParamKind,
    hint: &str,
    target_date: Option<chrono_core::calc::CivilDateTime>,
) -> Result<ParamValue, PresetError> {
    match hint {
        "target_file_creation" => {
            if kind != ParamKind::Date {
                return Err(PresetError::BadFile(format!(
                    "parameter '{id}': hint target_file_creation fills a date, but the parameter is not a date"
                )));
            }
            match target_date {
                Some(date) => Ok(ParamValue::Date(date)),
                None => Err(PresetError::NotBuilt(format!(
                    "parameter '{id}' takes its value from the target file date (only available when running a target) - pass --param {id}=<value>"
                ))),
            }
        }
        other => Err(PresetError::NotBuilt(format!(
            "parameter '{id}' uses default_hint '{other}', which is not built yet (built: target_file_creation)"
        ))),
    }
}

/// Resolve a preset's raw moment to a concrete `MomentExpr`, substituting parameter values into a
/// parametric base/shift. This is where the parametric preset becomes an ordinary moment the core
/// evaluates - the core never sees a parameter.
fn resolve_moment(moment: MomentDto, values: &HashMap<String, ParamValue>) -> Result<MomentExpr, PresetError> {
    let base = base_from(moment.base, values)?;
    let steps = moment.steps.into_iter().map(|s| step_from(s, values)).collect::<Result<Vec<_>, _>>()?;
    Ok(MomentExpr { base, steps })
}

/// Parse a `--param` value string against the parameter's type. `date` accepts a bare date; a
/// `duration` is a magnitude and a unit with no sign (the shift carries the sign).
fn parse_param_value(id: &str, kind: ParamKind, raw: &str) -> Result<ParamValue, PresetError> {
    match kind {
        ParamKind::Date => Ok(ParamValue::Date(parse_param_date(raw).map_err(PresetError::BadFile)?)),
        ParamKind::Duration => {
            let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
            let (num, unit_str) = raw.split_at(split);
            let amount: i64 = num
                .parse()
                .map_err(|_| PresetError::BadFile(format!("parameter '{id}': bad duration '{raw}' (want e.g. 30days)")))?;
            duration_value(id, amount, unit_str)
        }
        ParamKind::Variant => Ok(ParamValue::Variant(variant_days(id, raw)?)),
    }
}

/// Map a boundary-variant label to its signed day offset (docs/05 3.6): the day before, on, or after
/// the boundary. The three labels are the contract - an unknown one is a usage error, never a guess.
fn variant_days(id: &str, label: &str) -> Result<i64, PresetError> {
    match label {
        "day_before" => Ok(-1),
        "on_day" => Ok(0),
        "day_after" => Ok(1),
        other => Err(PresetError::BadFile(format!(
            "parameter '{id}': unknown variant '{other}' (day_before, on_day, day_after)"
        ))),
    }
}

/// Build a `duration` value, mapping the unit token and rejecting a negative magnitude.
fn duration_value(id: &str, amount: i64, unit_str: &str) -> Result<ParamValue, PresetError> {
    if amount < 0 {
        return Err(PresetError::BadFile(format!("parameter '{id}' amount must be non-negative")));
    }
    let unit = parse_unit(unit_str)
        .ok_or_else(|| PresetError::BadFile(format!("parameter '{id}': unknown unit '{unit_str}'")))?;
    Ok(ParamValue::Duration { amount, unit })
}

/// Parse a date or date-time; a bare date gets midnight so `--param start_date=2026-01-01` works.
fn parse_param_date(s: &str) -> Result<chrono_core::calc::CivilDateTime, String> {
    let normalized =
        if s.contains('T') || s.contains(' ') { s.to_string() } else { format!("{s}T00:00:00") };
    chrono_core::calc::parse_civil_datetime(&normalized)
}

/// Map a preset's `time_mode` to the substitution wire shape. `multiplier == 1` (or absent) is
/// real-time `flow`; `> 1` is `xN`; `< 1` is rejected. Presets do not express `frozen`.
fn time_mode_from(dto: Option<TimeModeDto>) -> Result<PresetTimeMode, PresetError> {
    let Some(dto) = dto else { return Ok(PresetTimeMode::default()) };
    let multiplier = dto.multiplier.unwrap_or(1);
    let (mode, multiplier) = match multiplier {
        1 => ("flow".to_string(), None),
        m if m > 1 => ("multiplier".to_string(), Some(m)),
        _ => return Err(PresetError::BadFile(format!("time_mode multiplier must be >= 1, got {multiplier}"))),
    };
    Ok(PresetTimeMode { mode, multiplier, scale_duration: dto.scale_duration_clock })
}

/// Whether a preset's `applies_to` makes it a substitution question (docs/04 4.2).
fn preset_targets_substitution(applies_to: &str) -> bool {
    matches!(applies_to, "substitution" | "both")
}

/// Read the target executable's creation date, expressed in the session zone, for a
/// `target_file_creation` hint (docs/04 4.2). `None` if the file's metadata cannot be read (the
/// launch will then fail plainly on its own), so a hint falls back to the honest "pass --param".
fn read_target_creation_date(
    target: &str,
    tz_bias_min: Option<i32>,
) -> Option<chrono_core::calc::CivilDateTime> {
    use std::os::windows::fs::MetadataExt;
    let meta = std::fs::metadata(target).ok()?;
    // creation_time() is a Windows FILETIME (100ns since 1601-01-01 UTC) - the same shape the
    // wall-clock conversion speaks - so express it in the session zone as a civil date.
    let wall = filetime_utc_to_wall(meta.creation_time() as i64, tz_bias_min.unwrap_or(0));
    chrono_core::calc::parse_civil_datetime(&wall).ok()
}

/// Locate a preset file: next to the executable (portable layout), else in ./presets.
fn find_preset_file(id: &str) -> Result<std::path::PathBuf, PresetError> {
    if !is_valid_catalogue_id(id) {
        return Err(PresetError::NotFound(format!(
            "invalid preset id '{id}' (use letters, digits, '-' or '_')"
        )));
    }
    let name = format!("{id}.json");
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("presets").join(&name));
        }
    }
    candidates.push(std::path::Path::new("presets").join(&name));
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| PresetError::NotFound(format!("preset '{id}' not found (looked in <exe>/presets and ./presets)")))
}

/// Load and validate a preset by id.
fn load_preset(id: &str) -> Result<Preset, PresetError> {
    let path = find_preset_file(id)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| PresetError::BadFile(format!("cannot read {}: {e}", path.display())))?;
    parse_preset(&text)
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

/// A Chromium/Electron session driven over the Chrome DevTools Protocol, speaking the SAME machine
/// protocol as the native core (ADR-9). It is the inverse of the old human-report `driver_run_cdp`:
/// launch + shim + audit, but every outcome travels as `state`/`coverage`/`session_verdict`/`ended`,
/// so the GUI and CLI consume a CDP session exactly like a native one. Interactive commands (`query`
/// now, `set_multiplier`/`jump` in slice C7) arrive on the same stdin the native session reads.
///
/// `--ticks` is a DRIVER concern (the driver counts `state` heartbeats and sends `end`), so this loop
/// runs until `end`, stdin EOF, or the app closing - exactly like `run_session`.
fn cdp_session(target: TargetSpec, time: TimeSpec, reader: BufReader<std::io::Stdin>) -> i32 {
    // Resolve the fake-clock origin ONCE, in Unix-epoch ms, and share it with both the shim and every
    // `state` event, so the panel's fake clock matches what the app's own `Date.now()` reads (no skew
    // from the launch+attach duration). The moment is absolute here (the driver resolved a relative
    // --at before spawning); an out-of-range moment is an honest error, never a silent fall-back to
    // real time (untouchable rule 4).
    let real_start_ms = now_epoch_ms();
    let bias = time.moment.tz_bias_min.unwrap_or(0);
    let fake_start_ms = match time.moment.local.as_deref() {
        Some(local) => match moment_epoch_ms(local, time.moment.tz_bias_min) {
            Some(ms) => ms,
            None => {
                emit(&Event::Error {
                    v: PROTOCOL_VERSION,
                    id: Some(1),
                    code: 1,
                    key: "moment.invalid".into(),
                    origin: "core".into(),
                });
                emit(&ended_clean());
                return 1;
            }
        },
        None => real_start_ms, // no --at: the fake clock starts at real now (pure offset/acceleration)
    };
    // flow = x1 (a plain wall offset), xN accelerates, frozen = x0 (the wall is held; the shim keeps
    // timers real via TS = M || 1).
    let mult = match time.mode.as_str() {
        "multiplier" => time.multiplier.unwrap_or(1).max(1),
        "frozen" => 0,
        _ => 1,
    };
    // The live session clock, computed Rust-side and kept in step with the shim: set_multiplier and
    // jump re-anchor it here and push the new origin to every context. The shim is built PER ATTACH from
    // this clock's current origin (below), so a context that attaches after an in-flight change starts on
    // the same clock as the others. A flag records whether a rate change happened in flight, so the end
    // report can carry the honest running-timer caveat (rule 4).
    let mut clock = CdpClock::new(fake_start_ms, real_start_ms, mult, bias);
    let mut rate_changed_in_flight = false;

    // Launch under our own isolated profile + debug port, then attach. Any failure is an honest error
    // event plus exit 2 (could not launch/attach), never a faked verdict.
    let mut launched = match cdp::launch_chromium(&target.path, &target.args) {
        Ok(l) => l,
        Err(e) => {
            emit(&Event::Error {
                v: PROTOCOL_VERSION,
                id: Some(1),
                code: 2,
                key: "target.launch_failed".into(),
                origin: "mechanism".into(),
            });
            eprintln!("chrono core: {e}");
            emit(&ended_clean());
            return 2;
        }
    };
    let mut client = match cdp::CdpClient::connect_to_port("127.0.0.1", launched.port) {
        Ok(c) => c,
        Err(e) => {
            launched.shutdown();
            emit(&Event::Error {
                v: PROTOCOL_VERSION,
                id: Some(1),
                code: 2,
                key: "target.attach_failed".into(),
                origin: "mechanism".into(),
            });
            eprintln!("chrono core: cannot attach over CDP: {e}");
            emit(&ended_clean());
            return 2;
        }
    };
    if let Err(e) = client.call(
        "Target.setAutoAttach",
        serde_json::json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
        None,
    ) {
        launched.shutdown();
        emit(&Event::Error {
            v: PROTOCOL_VERSION,
            id: Some(1),
            code: 2,
            key: "target.attach_failed".into(),
            origin: "mechanism".into(),
        });
        eprintln!("chrono core: cannot set up auto-attach: {e}");
        emit(&ended_clean());
        return 2;
    }
    // No start verdict for CDP: unlike the native guard window, at start there is nothing to judge yet
    // (contexts attach asynchronously and have made no time calls). The authoritative verdict is the
    // family `session_verdict` at end - emitting "undetermined" now would read as "not working" when it
    // only means "not audited yet".

    // A stdin reader thread turns command lines into `Command`s (end/query/set_multiplier/jump) so the
    // main loop can interleave them with CDP polling and the heartbeat without blocking on read_line.
    let (tx, rx) = mpsc::channel::<Command>();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
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

    // Install the shim into every context as it attaches (page and its Web Workers), beat a ~1 s
    // `state` heartbeat, and sample per-context call counts, until `end`, stdin EOF, or the app closes.
    let mut contexts: Vec<CdpContext> = Vec::new();
    let mut counts: std::collections::BTreeMap<(u32, String), u64> = std::collections::BTreeMap::new();
    let mut failed = 0usize;
    let mut next_index = 0u32;
    let mut app_closed = false;
    let heartbeat = Duration::from_secs(1);
    let mut deadline = Instant::now() + heartbeat;
    let mut last_audit = Instant::now();

    'session: loop {
        if !launched.is_running() {
            app_closed = true;
            break;
        }
        // Drain protocol commands without blocking.
        loop {
            match rx.try_recv() {
                Ok(Command::End { .. }) => break 'session,
                Ok(Command::Query { id, .. }) => {
                    emit(&clock.state_event_at(now_epoch_ms()));
                    emit(&Event::Ack { v: PROTOCOL_VERSION, id });
                }
                Ok(Command::SetMultiplier { id, multiplier, .. }) => {
                    // Re-anchor the clock (wall and duration both continue from now at the new rate) and
                    // push the new origin + rate to every context. New timers pick up the rate at once;
                    // Date.now/new Date/performance.now reflect it immediately - only an already-queued
                    // setInterval keeps its old cadence, which the end report warns about (rule 4).
                    let now = now_epoch_ms();
                    let (fake0, real0, m) = clock.set_multiplier_at(multiplier, now);
                    cdp_broadcast(&mut client, &contexts, &cdp_set_multiplier_expr(fake0, real0, m));
                    rate_changed_in_flight = true;
                    emit(&Event::Ack { v: PROTOCOL_VERSION, id });
                    emit(&clock.state_event_at(now_epoch_ms()));
                }
                Ok(Command::Jump { id, to, .. }) => {
                    // Move the wall to a new fake instant (absolute, or a relative delta on the current
                    // fake time), leaving the duration axis untouched (rule 3). A bad moment is an honest
                    // error, never a silent no-op (rule 6).
                    let now = now_epoch_ms();
                    match cdp_resolve_jump(&clock, &to, now) {
                        Ok(new_fake) => {
                            let (fake0, real0) = clock.jump_to_at(new_fake, now);
                            cdp_broadcast(&mut client, &contexts, &cdp_jump_expr(fake0, real0));
                            emit(&Event::Ack { v: PROTOCOL_VERSION, id });
                            emit(&clock.state_event_at(now_epoch_ms()));
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
                Ok(_) => {} // Start or another command: ignore
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'session, // stdin closed (EOF)
            }
        }
        // Poll CDP (bounded by the WS read timeout) for a newly attached context, and shim it.
        match client.poll() {
            Ok(Some(cdp::Msg::Event { method, params, .. })) if method == "Target.attachedToTarget" => {
                let sid = params["sessionId"].as_str().unwrap_or("").to_string();
                let ty = params["targetInfo"]["type"].as_str().unwrap_or("").to_string();
                let tid = params["targetInfo"]["targetId"].as_str().unwrap_or("").to_string();
                if !sid.is_empty() && cdp::is_shimmable(&ty) {
                    next_index += 1;
                    // Build the shim from the clock's CURRENT origin, not the session's initial values, so
                    // a context attaching after an in-flight rate change or jump starts on the same clock
                    // as every other context (one absolute origin; rule 3). Before any change this is
                    // identical to the initial shim.
                    let shim = cdp::build_shim(clock.wall_fake0, clock.wall_real0, clock.mult);
                    let injected = if cdp::is_worker(&ty) {
                        cdp::inject_worker(&mut client, &sid, &shim)
                    } else {
                        cdp::inject_page(&mut client, &sid, &shim)
                    };
                    match injected {
                        Ok(()) => contexts.push(CdpContext {
                            index: next_index,
                            session_id: sid,
                            ty,
                            target_id: tid,
                        }),
                        Err(_) => failed += 1,
                    }
                }
            }
            // A context that went away - a reload, a closed window, a recycled worker. Drop it from the
            // poll list: nothing else did, so the list only ever grew, and every dead entry still got a
            // Runtime.evaluate every second. That inflated the reported context count, and a command to a
            // dead session that draws no reply at all costs the full 20 s deadline INSIDE the session
            // loop - no heartbeat, no `end`, no liveness check for that whole time. Its counts stay in
            // `counts` (keyed by context index, not by session), so the audit loses nothing.
            Ok(Some(cdp::Msg::Event { method, params, .. }))
                if method == "Target.detachedFromTarget" || method == "Target.targetDestroyed" =>
            {
                let sid = params["sessionId"].as_str().unwrap_or("");
                let tid = params["targetId"].as_str().unwrap_or("");
                contexts.retain(|c| {
                    let gone = (!sid.is_empty() && c.session_id == sid)
                        || (!tid.is_empty() && c.target_id == tid);
                    !gone
                });
            }
            Ok(_) => {}
            Err(_) => {
                app_closed = true; // the connection dropped, i.e. the app exited
                break;
            }
        }
        // ~1 s heartbeat (also when frozen) and ~1 s coverage sampling.
        if Instant::now() >= deadline {
            emit(&clock.state_event_at(now_epoch_ms()));
            deadline = Instant::now() + heartbeat;
        }
        if last_audit.elapsed() >= Duration::from_secs(1) {
            poll_counts(&mut client, &contexts, &mut counts);
            last_audit = Instant::now();
        }
    }
    poll_counts(&mut client, &contexts, &mut counts); // final best-effort read
    let audited = !counts.is_empty();

    // Coverage = APIs the app actually called (count > 0), per context - honest "covered", like native.
    let mut covered: Vec<(u32, String, u64)> = counts
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|((idx, ch), n)| (idx, ch, n))
        .collect();
    covered.sort();

    let verdict = if contexts.is_empty() {
        Verdict::Fails
    } else if !covered.is_empty() {
        if failed > 0 {
            Verdict::Partial
        } else {
            Verdict::Works
        }
    } else {
        Verdict::Undetermined
    };
    let (token, reason) = match &verdict {
        Verdict::Works => ("works", "chromium.contexts_covered"),
        Verdict::Partial => ("partial", "chromium.contexts_partial"),
        Verdict::Fails => ("fails", "chromium.no_contexts"),
        Verdict::Undetermined => ("undetermined", "chromium.no_time_calls"),
    };

    let mut warnings = vec!["chromium.launched_with_debug_port".to_string()];
    if app_closed && !audited {
        warnings.push("chromium.app_closed_before_audit".to_string());
    }
    if rate_changed_in_flight {
        // Honest caveat: a rate change reaches Date.now/new Date/performance.now and every NEW timer at
        // once, but a setInterval already scheduled at the old rate keeps its old cadence - the JS engine
        // had already queued it (rule 4). The native hook has no equivalent gap (it divides Ctl live).
        warnings.push("chromium.rate_change_affects_running_timers".to_string());
    }

    // Emit one `coverage` per attached context (pid = context index), never summed across contexts
    // (rule 4). The invasive-launch warning rides on the FIRST event, and if no context attached at
    // all we still emit one bare coverage - so the warning is never lost for an idle or zero-context app.
    if contexts.is_empty() {
        emit(&Event::Coverage {
            v: PROTOCOL_VERSION,
            pid: 0,
            covered: Vec::new(),
            observed: Vec::new(),
            uncovered: Vec::new(),
            warning_keys: std::mem::take(&mut warnings),
        });
    } else {
        for ctx in &contexts {
            let chans: Vec<CoveredChannel> = covered
                .iter()
                .filter(|(idx, _, _)| *idx == ctx.index)
                .map(|(_, ch, n)| CoveredChannel { channel: ch.clone(), calls: *n })
                .collect();
            emit(&Event::Coverage {
                v: PROTOCOL_VERSION,
                pid: ctx.index,
                covered: chans,
                observed: Vec::new(),
                uncovered: Vec::new(),
                warning_keys: std::mem::take(&mut warnings),
            });
        }
    }

    emit(&Event::SessionVerdict {
        v: PROTOCOL_VERSION,
        verdict: token.to_string(),
        reason_key: reason.to_string(),
        process_count: contexts.len() as u32,
    });

    // Session timing from the live clock (correct across any in-flight rate changes and jumps): real is
    // the whole session, fake is the elapsed duration, and the end wall is where the fake clock landed.
    // Then tear down our instance (kill + remove the temp profile), reporting any cleanup residue
    // honestly via `ended.residue_keys` (rules 4, 6).
    let end_now = now_epoch_ms();
    let real_ms = end_now - clock.session_real0;
    let fake_ms = clock.elapsed_fake_ms(end_now);
    let fake_end_wall = epoch_ms_to_wall(clock.fake_wall_ms(end_now), bias);
    let residue = launched.shutdown_with_residue();
    emit(&Event::Ended {
        v: PROTOCOL_VERSION,
        clean: residue.is_empty(),
        residue_keys: residue,
        target_exit_code: None,
        elapsed_real_ms: real_ms,
        elapsed_fake_ms: fake_ms,
        fake_end_wall: Some(fake_end_wall),
    });
    verdict.exit_code()
}

/// The live clock of a CDP session, computed entirely Rust-side so the panel matches the app's own
/// `Date.now()` with no browser round-trip. The wall origin (`wall_fake0` at `wall_real0`, rate `mult`)
/// is re-anchored on a rate change or a jump and pushed identically to every JS context, so all
/// contexts and the panel share one absolute origin. A separate duration accumulator keeps
/// `elapsed_fake` an honest integral of the rate over time - a jump moves the wall but adds no elapsed
/// duration - mirroring the native split between elapsed time and the fake wall reached.
struct CdpClock {
    wall_fake0: i64,
    wall_real0: i64,
    mult: i64,
    bias: i32,
    session_real0: i64,
    dur_fake_accum: i64,
    dur_real0: i64,
}

impl CdpClock {
    fn new(fake0_ms: i64, real0_ms: i64, mult: i64, bias: i32) -> Self {
        CdpClock {
            wall_fake0: fake0_ms,
            wall_real0: real0_ms,
            mult,
            bias,
            session_real0: real0_ms,
            dur_fake_accum: 0,
            dur_real0: real0_ms,
        }
    }

    /// The fake wall instant (epoch ms) at `now`: the current segment's origin plus scaled real time.
    /// Frozen (mult 0) holds it at the origin; xN accelerates.
    fn fake_wall_ms(&self, now: i64) -> i64 {
        self.wall_fake0 + (now - self.wall_real0).saturating_mul(self.mult)
    }

    /// Fake duration elapsed (the integral of the rate): the accumulator plus the current segment. A
    /// jump does not touch this, so a wall discontinuity is never counted as elapsed time.
    fn elapsed_fake_ms(&self, now: i64) -> i64 {
        self.dur_fake_accum + (now - self.dur_real0).saturating_mul(self.mult)
    }

    /// A `state` event at `now` (passed in so the mapping is pure and unit-testable).
    fn state_event_at(&self, now: i64) -> Event {
        Event::State {
            v: PROTOCOL_VERSION,
            fake: Clock {
                wall: epoch_ms_to_wall(self.fake_wall_ms(now), self.bias),
                zone_bias_min: self.bias,
            },
            real: Clock { wall: epoch_ms_to_wall(now, self.bias), zone_bias_min: self.bias },
            multiplier: self.mult,
            elapsed_fake_ms: self.elapsed_fake_ms(now),
            elapsed_real_ms: now - self.session_real0,
        }
    }

    /// Re-anchor for a new multiplier at `now`: the wall and the duration both continue from where they
    /// are, so neither jumps (rule 3); only the future rate changes. A negative rate would run the wall
    /// backward, which is never valid, so it clamps to 0 (freeze). Returns (fake0, real0, mult) to push
    /// to the shim.
    fn set_multiplier_at(&mut self, m: i64, now: i64) -> (i64, i64, i64) {
        self.dur_fake_accum += (now - self.dur_real0).saturating_mul(self.mult);
        self.dur_real0 = now;
        self.wall_fake0 = self.fake_wall_ms(now);
        self.wall_real0 = now;
        self.mult = m.max(0);
        (self.wall_fake0, self.wall_real0, self.mult)
    }

    /// Re-anchor for a jump to a new fake wall at `now`: the wall moves and continues at the same rate;
    /// the duration axis is untouched, so a backward jump never rewinds elapsed time (rule 3). Returns
    /// (fake0, real0) to push to the shim.
    fn jump_to_at(&mut self, new_fake_ms: i64, now: i64) -> (i64, i64) {
        self.wall_fake0 = new_fake_ms;
        self.wall_real0 = now;
        (self.wall_fake0, self.wall_real0)
    }
}

/// Resolve a CDP jump target to a fake epoch-ms instant: an absolute moment in the session zone, or a
/// relative delta applied to the CURRENT fake instant through the shared calc evaluator (the same
/// grammar as `calc` and the native jump). Returns a translation key on a bad moment (rule 6).
fn cdp_resolve_jump(clock: &CdpClock, to: &MomentSpec, now: i64) -> Result<i64, &'static str> {
    if to.kind == "relative" {
        let delta = to.delta.as_deref().ok_or("moment.invalid")?;
        let cur_wall = epoch_ms_to_wall(clock.fake_wall_ms(now), clock.bias);
        let cur_civil = chrono_core::calc::parse_civil_datetime(&cur_wall).map_err(|_| "moment.invalid")?;
        let step = parse_shift(delta).map_err(|_| "moment.invalid")?;
        let expr = MomentExpr { base: Base::Now, steps: vec![step] };
        let outcome = chrono_core::calc::eval(
            &expr,
            &EvalContext { now: cur_civil, zone_bias_min: 0, calendar: None },
        )
        .map_err(jump_error_key)?;
        moment_epoch_ms(&outcome.result().to_iso(), Some(clock.bias)).ok_or("moment.invalid")
    } else {
        let local = to.local.as_deref().ok_or("moment.invalid")?;
        moment_epoch_ms(local, Some(clock.bias)).ok_or("moment.invalid")
    }
}

/// Evaluate a JS expression in every attached context (best-effort: a context that just closed errors
/// and is skipped, so an in-flight update stays honest for the rest).
fn cdp_broadcast(client: &mut cdp::CdpClient, contexts: &[CdpContext], expr: &str) {
    for ctx in contexts {
        let _ = client.call(
            "Runtime.evaluate",
            serde_json::json!({ "expression": expr, "returnByValue": true }),
            Some(&ctx.session_id),
        );
    }
}

/// The JS to push a new wall origin AND rate into a context's `__chronomock`, re-anchoring its local
/// duration axis first so `performance.now` stays continuous across the rate change (rule 3). The wall
/// origin (fake0, real0, mult) is the driver's, identical for every context, so all contexts stay in
/// step.
fn cdp_set_multiplier_expr(fake0: i64, real0: i64, mult: i64) -> String {
    format!(
        "(function(){{var S=globalThis.__chronomock;if(!S)return 'no-shim';\
         var p=S._realPerf?S._realPerf():0;S.perfBase=(S.perfBase||0)+(p-S.perfAnchorReal)*(S.M||1);\
         S.perfAnchorReal=p;S.fakeStart={fake0};S.realStart={real0};S.M={mult};return 'ok';}})()"
    )
}

/// The JS to push a new wall origin into a context's `__chronomock` for a jump - wall only; the rate
/// and the duration axis are untouched, so a backward jump never rewinds elapsed time (rule 3).
fn cdp_jump_expr(fake0: i64, real0: i64) -> String {
    format!(
        "(function(){{var S=globalThis.__chronomock;if(!S)return 'no-shim';\
         S.fakeStart={fake0};S.realStart={real0};return 'ok';}})()"
    )
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

    let (target, time, force) = match cmd {
        Command::Start { target, time, force, .. } => (target, time, force),
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
            emit(&state_event(&session));
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

    // Read a shipped data file from the repo (CARGO_MANIFEST_DIR is crates/cli). The golden tests below
    // exercise the REAL calendars/ and presets/ that ship, through the real engine - so a typo in a
    // holiday date, a wrong valid_from, or a broken observation rule fails the suite (RELEASE-004).
    fn read_data(rel: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
    }

    /// S-1, first line. Calendars are the one catalogue outsiders are invited to write, so a file
    /// that makes "next business day" unanswerable has to be refused where its author can see it -
    /// not walked into by the engine. Duplicated weekend days are folded first, so the check counts
    /// distinct days and a repeated "saturday" is not mistaken for a full week.
    #[test]
    fn calendar_with_every_day_as_weekend_is_refused() {
        let all_week = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["monday","tuesday","wednesday","thursday","friday","saturday","sunday"],
            "observed":"none","holidays":[]}"#;
        let err = calendar_from_text(all_week).expect_err("a week with no working day is refused");
        assert!(err.contains("weekend"), "the message must name the field: {err}");

        // Duplicates alone are not an error - they fold, and the calendar still works.
        let dupes = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["saturday","saturday","sunday"],"observed":"none","holidays":[]}"#;
        let cal = calendar_from_text(dupes).expect("duplicate weekend days fold");
        assert_eq!(cal.weekend.len(), 2);
    }

    /// S-17. Every rule field is refused out of range, naming the holiday and the field. Unchecked,
    /// none of these fail loudly - the civil-date maths rolls month 13 into January and day 40 into the
    /// next month, so the calendar quietly marks the WRONG dates and every business-day answer built on
    /// it is wrong with it. Calendars are the documented third-party extension point, so this is the
    /// place where a broken file has to stop.
    #[test]
    fn calendar_rules_out_of_range_are_refused_with_the_field_named() {
        let cal = |rule: &str| {
            format!(
                r#"{{"schema":"chronomock.calendar/1","id":"x","country":"XX","weekend":["saturday","sunday"],
                "observed":"none","holidays":[{{"id":"bad","name":{{"en":"B","local":"B"}},
                "rule":{rule},"source":"test"}}]}}"#
            )
        };

        for (rule, needle) in [
            (r#"{"type":"fixed","month":13,"day":1}"#, "month 13"),
            (r#"{"type":"fixed","month":1,"day":40}"#, "day 40"),
            (r#"{"type":"nth_weekday","month":1,"weekday":"monday","order":9}"#, "order 9"),
            (r#"{"type":"nth_weekday","month":0,"weekday":"monday","order":1}"#, "month 0"),
            (r#"{"type":"easter_offset","offset":5000}"#, "5000"),
        ] {
            let err = calendar_from_text(&cal(rule)).expect_err("out of range must be refused");
            assert!(err.contains("bad"), "the message must name the holiday: {err}");
            assert!(err.contains(needle), "the message must name the value: {err}");
        }

        // The legitimate neighbours of those bounds still parse.
        for rule in [
            r#"{"type":"fixed","month":2,"day":29}"#,
            r#"{"type":"nth_weekday","month":12,"weekday":"monday","order":-1}"#,
            r#"{"type":"nth_weekday","month":5,"weekday":"monday","order":5}"#,
            r#"{"type":"easter_offset","offset":60}"#,
        ] {
            calendar_from_text(&cal(rule)).unwrap_or_else(|e| panic!("{rule} should parse: {e}"));
        }
    }

    /// S-17, the whole-file checks: a repeated id makes the audit ambiguous (holiday_on names one of
    /// them and the reader cannot tell which), and an inverted validity window means "never a holiday",
    /// which reads as a missing entry instead of the mistake it is.
    #[test]
    fn duplicate_holiday_ids_and_inverted_validity_windows_are_refused() {
        let dupes = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["saturday","sunday"],"observed":"none","holidays":[
            {"id":"same","name":{"en":"A","local":"A"},"rule":{"type":"fixed","month":1,"day":1},"source":"t"},
            {"id":"same","name":{"en":"B","local":"B"},"rule":{"type":"fixed","month":2,"day":2},"source":"t"}]}"#;
        assert!(calendar_from_text(dupes).expect_err("duplicate id").contains("same"));

        let inverted = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["saturday","sunday"],"observed":"none","holidays":[
            {"id":"w","name":{"en":"A","local":"A"},"rule":{"type":"fixed","month":1,"day":1},
             "valid_from":2030,"valid_to":2020,"source":"t"}]}"#;
        assert!(calendar_from_text(inverted).expect_err("inverted window").contains("valid_from"));
    }

    /// S-20. The trim was `&url[..66]`, which panics when byte 66 lands inside a multi-byte character -
    /// and a debug URL can carry a profile path with a non-ASCII user name. Only the hidden probes print
    /// this, so it never sat on a user path, but a panic to shorten a log line is a poor trade.
    #[test]
    fn a_non_ascii_url_is_trimmed_by_characters_not_bytes() {
        // Each of these is two bytes, so a byte slice at 66 would land mid-character.
        let url = format!("ws://127.0.0.1:9333/devtools/page/{}", "ó".repeat(60));
        let short = short_url(&url);
        assert_eq!(short.chars().count(), 66);
        assert!(url.starts_with(&short));

        // A short URL is returned whole, and ASCII behaves exactly as before.
        assert_eq!(short_url("ws://127.0.0.1:9333/x"), "ws://127.0.0.1:9333/x");
        assert_eq!(short_url(&"a".repeat(100)).len(), 66);
    }

    #[test]
    fn shipped_calendars_parse_and_hit_golden_dates() {
        use chrono_core::calc::CivilDateTime;
        use chrono_core::calendar::{holiday_on, is_business_day};
        let d = |y: i64, m: u32, day: u32| CivilDateTime {
            year: y,
            month: m,
            day,
            hour: 0,
            minute: 0,
            second: 0,
        };

        let pl = calendar_from_text(&read_data("calendars/pl.json")).expect("pl parses");
        // Easter Monday 2026 = 2026-04-06 (Easter Sunday 2026-04-05): the easter_offset computus.
        assert_eq!(holiday_on(&d(2026, 4, 6), &pl).map(|h| h.id.as_str()), Some("easter_monday"));
        // Corpus Christi 2026 = Easter + 60 days = 2026-06-04: a larger easter_offset.
        assert_eq!(holiday_on(&d(2026, 6, 4), &pl).map(|h| h.id.as_str()), Some("corpus_christi"));
        // Epiphany was restored as a Polish public holiday from 2011 (valid_from:2011; law of 24 Sept
        // 2010, abolished 1960): a holiday in 2026, NOT in 2010.
        assert_eq!(holiday_on(&d(2026, 1, 6), &pl).map(|h| h.id.as_str()), Some("epiphany"));
        assert!(holiday_on(&d(2010, 1, 6), &pl).is_none());
        // Christmas Eve became a non-working day from 2025 (valid_from:2025): a holiday in 2025, not 2024.
        assert_eq!(holiday_on(&d(2025, 12, 24), &pl).map(|h| h.id.as_str()), Some("christmas_eve"));
        assert!(holiday_on(&d(2024, 12, 24), &pl).is_none());

        let fed = calendar_from_text(&read_data("calendars/us-federal.json")).expect("us-federal parses");
        let bank = calendar_from_text(&read_data("calendars/us-banking.json")).expect("us-banking parses");
        // One country, two calendars: Independence Day 2026 falls on Saturday (Jul 4), so it is observed
        // on Friday Jul 3 federally (sat->fri) - not a business day - while banking (sun->mon only) does
        // not shift a Saturday holiday, so the same Friday IS a business day.
        assert!(!is_business_day(&d(2026, 7, 3), &fed));
        assert!(is_business_day(&d(2026, 7, 3), &bank));
        // Juneteenth is a federal holiday from 2021 (valid_from:2021): a holiday in 2021, not in 2020.
        assert_eq!(holiday_on(&d(2021, 6, 19), &fed).map(|h| h.id.as_str()), Some("juneteenth"));
        assert!(holiday_on(&d(2020, 6, 19), &fed).is_none());
        // MLK Day 2026 = the third Monday of January = 2026-01-19: an nth_weekday rule.
        assert_eq!(holiday_on(&d(2026, 1, 19), &fed).map(|h| h.id.as_str()), Some("mlk_day"));
        // Banking observes a Sunday holiday on the Monday: New Year 2023-01-01 (Sun) -> Mon 2023-01-02.
        assert!(!is_business_day(&d(2023, 1, 2), &bank));
    }

    #[test]
    fn shipped_presets_parse_and_hit_golden_dates() {
        use chrono_core::calc::{CivilDateTime, EvalContext};
        let now = CivilDateTime { year: 2026, month: 2, day: 15, hour: 12, minute: 0, second: 0 };

        // Every shipped preset parses (a malformed one, or a bad schema/field, fails here).
        let ids = [
            "month-end",
            "quarter-end",
            "epoch-zero",
            "year-2038",
            "trial-first-day-after",
            "trial-last-day",
            "year-rollover",
            "license-expired-year-ago",
            "date-before-install",
            "payment-due-business-days",
            "clock-skew-plus-90s",
            "feb-29",
            "age-of-majority",
            "fiscal-year-end",
        ];
        for id in ids {
            let text = read_data(&format!("presets/{id}.json"));
            parse_preset(&text).unwrap_or_else(|e| panic!("preset {id} parses: {}", e.message()));
        }

        // Evaluate a preset with its default parameters, through the real resolve + engine path.
        let eval_preset = |id: &str, ctx: &EvalContext| {
            let p = parse_preset(&read_data(&format!("presets/{id}.json"))).unwrap();
            let values = resolve_parameters(&p.parameters, &HashMap::new(), None).unwrap();
            let expr = resolve_moment(p.moment, &values).unwrap();
            chrono_core::calc::eval(&expr, ctx).unwrap().result()
        };

        let no_cal = EvalContext { now, zone_bias_min: 0, calendar: None };
        // Absolute-base presets are exact.
        assert_eq!(eval_preset("epoch-zero", &no_cal).to_iso(), "1970-01-01T00:00:00");
        assert_eq!(eval_preset("year-2038", &no_cal).to_iso(), "2038-01-19T03:14:07");
        // month-end snaps today (2026-02-15) to the last day of February - a common year, so the 28th.
        let month_end = eval_preset("month-end", &no_cal);
        assert_eq!((month_end.year, month_end.month, month_end.day), (2026, 2, 28));

        // clock-skew-plus-90s carries base "now" (not "today"): the +90 s shift is measured from the
        // current instant WITH its seconds, so 12:00:00 + 90 s crosses the minute to 12:01:30. A base of
        // "today" (midnight) would make the 2FA skew meaningless - this pins that the preset uses "now".
        assert_eq!(eval_preset("clock-skew-plus-90s", &no_cal).to_iso(), "2026-02-15T12:01:30");

        // feb-29 uses nearest next-leap-day, pure arithmetic with NO calendar: from 2026-02-15 the next
        // 29 February is 2028 (2026 and 2027 are common years). Proves the leap-day nearest target
        // resolves without a --calendar, unlike the business-day targets.
        assert_eq!(eval_preset("feb-29", &no_cal).to_iso(), "2028-02-29T00:00:00");

        // age-of-majority is parametric and calculator-only: a birth date + the default day_before
        // variant is the day before the 18th birthday. 2008-03-15 + 18y = 2026-03-15, day_before ->
        // 2026-03-14. Exercises the whole variant path (parse + resolve + sign-less shift).
        let aom = parse_preset(&read_data("presets/age-of-majority.json")).unwrap();
        let aom_vals = resolve_parameters(&aom.parameters, &param_map(&[("birth_date", "2008-03-15")]), None).unwrap();
        let aom_expr = resolve_moment(aom.moment, &aom_vals).unwrap();
        assert_eq!(chrono_core::calc::eval(&aom_expr, &no_cal).unwrap().result().to_iso(), "2026-03-14T00:00:00");

        // fiscal-year-end takes a fiscal-year start date and returns its last day (start + 1 year - 1 day),
        // never a hardcoded 31 December (docs/05 3.4). US federal 2025-10-01 -> 2026-09-30.
        let fye = parse_preset(&read_data("presets/fiscal-year-end.json")).unwrap();
        let fye_vals = resolve_parameters(&fye.parameters, &param_map(&[("fiscal_year_start", "2025-10-01")]), None).unwrap();
        let fye_expr = resolve_moment(fye.moment, &fye_vals).unwrap();
        assert_eq!(chrono_core::calc::eval(&fye_expr, &no_cal).unwrap().result().to_iso(), "2026-09-30T00:00:00");

        // payment-due-business-days is calendar-aware: +90 business days from today lands on a different
        // day per market, which is the whole point of a calendar-aware preset. Anchor at 2026-06-01 so the
        // window spans US Labor Day (first Monday of September, a US weekday holiday Poland does not have),
        // guaranteeing the two markets diverge; if the calendar were ignored they would be identical.
        let now_pay = CivilDateTime { year: 2026, month: 6, day: 1, hour: 12, minute: 0, second: 0 };
        let bank = calendar_from_text(&read_data("calendars/us-banking.json")).unwrap();
        let pl = calendar_from_text(&read_data("calendars/pl.json")).unwrap();
        let due_us = eval_preset("payment-due-business-days", &EvalContext { now: now_pay, zone_bias_min: 0, calendar: Some(&bank) });
        let due_pl = eval_preset("payment-due-business-days", &EvalContext { now: now_pay, zone_bias_min: 0, calendar: Some(&pl) });
        assert_ne!(due_us.to_iso(), due_pl.to_iso(), "US and PL must diverge over 90 business days");
    }

    #[test]
    fn cdp_clock_scales_and_holds_frozen() {
        // xN: at +1000 ms real the fake wall advanced 60000 ms, and elapsed_fake is 60000.
        let c = CdpClock::new(1_000_000, 500_000, 60, 0);
        assert_eq!(c.fake_wall_ms(501_000), 1_060_000);
        assert_eq!(c.elapsed_fake_ms(501_000), 60_000);
        // frozen (x0): the wall never moves, however much real time passes.
        let f = CdpClock::new(1_000_000, 500_000, 0, 0);
        assert_eq!(f.fake_wall_ms(505_000), 1_000_000);
        assert_eq!(f.elapsed_fake_ms(505_000), 0);
    }

    #[test]
    fn cdp_clock_rate_change_is_continuous_and_accumulates() {
        let mut c = CdpClock::new(1_000_000, 500_000, 60, 0);
        // Run 1000 ms at x60, then switch to x120 at now = 501_000.
        let (fake0, real0, m) = c.set_multiplier_at(120, 501_000);
        assert_eq!(m, 120);
        // The wall is continuous across the change: the fake instant at the switch is unchanged.
        assert_eq!(fake0, 1_060_000);
        assert_eq!(real0, 501_000);
        assert_eq!(c.fake_wall_ms(501_000), 1_060_000);
        // 500 ms more at x120: wall += 60000, elapsed_fake = 60000 (seg 1) + 60000 (seg 2).
        assert_eq!(c.fake_wall_ms(501_500), 1_120_000);
        assert_eq!(c.elapsed_fake_ms(501_500), 120_000);
    }

    #[test]
    fn cdp_clock_jump_moves_wall_not_duration() {
        let mut c = CdpClock::new(1_000_000, 500_000, 60, 0);
        let now = 501_000;
        let target = c.fake_wall_ms(now) + 86_400_000; // jump forward one day
        c.jump_to_at(target, now);
        assert_eq!(c.fake_wall_ms(now), target); // the wall jumped
        // But elapsed duration did not: still 60000 ms of fake time passed, not a day.
        assert_eq!(c.elapsed_fake_ms(now), 60_000);
        // A backward jump does not rewind elapsed duration either (rule 3).
        c.jump_to_at(1_000_000, now);
        assert_eq!(c.elapsed_fake_ms(now), 60_000);
    }

    #[test]
    fn cdp_clock_negative_rate_clamps_to_freeze() {
        let mut c = CdpClock::new(1_000_000, 500_000, 60, 0);
        let (_, _, m) = c.set_multiplier_at(-5, 501_000);
        assert_eq!(m, 0); // never runs the wall backward
        assert_eq!(c.fake_wall_ms(502_000), c.fake_wall_ms(509_000)); // frozen after
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

    #[test]
    fn zone_parses_valid_offsets_and_rejects_garbage() {
        // UTC = local + bias, so a local +HH:MM gives bias -(H*60 + M). Real offsets across the range parse.
        assert_eq!(parse_zone_to_bias("+00:00").unwrap(), 0);
        assert_eq!(parse_zone_to_bias("+02:00").unwrap(), -120);
        assert_eq!(parse_zone_to_bias("-05:00").unwrap(), 300);
        assert_eq!(parse_zone_to_bias("+05:45").unwrap(), -345); // Nepal
        assert_eq!(parse_zone_to_bias("+14:00").unwrap(), -840); // max real offset
        assert_eq!(parse_zone_to_bias("-12:00").unwrap(), 720);
        // An INNER sign is rejected (M-4), never silently taken as a negative component.
        assert!(parse_zone_to_bias("+-5:00").is_err());
        assert!(parse_zone_to_bias("+05:-30").is_err());
        // Out of range, non-digit, missing parts - all rejected, no silent overflow/wrap.
        assert!(parse_zone_to_bias("+99:00").is_err());
        assert!(parse_zone_to_bias("+05:99").is_err());
        assert!(parse_zone_to_bias("+99999999:00").is_err()); // used to overflow i32 in a release build
        assert!(parse_zone_to_bias("05:00").is_err()); // no leading sign
        assert!(parse_zone_to_bias("+ab:cd").is_err());
        assert!(parse_zone_to_bias("+05").is_err()); // no minutes
    }

    #[test]
    fn catalogue_id_rejects_path_traversal() {
        // A catalogue id must never become a path escape: separators, '..', a drive colon, or empty
        // are refused before the id is turned into a file name (docs/04 4.1).
        assert!(is_valid_catalogue_id("month-end"));
        assert!(is_valid_catalogue_id("us_banking"));
        assert!(is_valid_catalogue_id("year-2038"));
        assert!(!is_valid_catalogue_id(""));
        assert!(!is_valid_catalogue_id(".."));
        assert!(!is_valid_catalogue_id("../secret"));
        assert!(!is_valid_catalogue_id("a/b"));
        assert!(!is_valid_catalogue_id("a\\b"));
        assert!(!is_valid_catalogue_id("c:evil"));
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
            warnings: vec![],
            uncovered: vec![],
            covered: vec![],
            observed: vec![],
            timing: None,
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
    fn calc_exit_codes_split_bad_input_from_needs_data() {
        // Not built / needs data -> code 5; bad input -> usage 1.
        assert_eq!(calc_error_exit_code(&EvalError::StepUnsupported { kind: "zone", index: 0 }), 5);
        assert_eq!(calc_error_exit_code(&EvalError::NeedsCalendar { index: 0 }), 5);
        assert_eq!(calc_error_exit_code(&EvalError::NotFound { index: 0 }), 5);
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
        let text = render_calc(&expr, &out, None, &now, None, None);
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
        let text = render_calc(&expr, &out, Some(0), &now, None, None);
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
        let text = render_calc(&expr, &out, Some(0), &now, None, None);
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

    #[test]
    fn parse_snap_reads_targets_and_rejects_unknown() {
        assert_eq!(parse_snap("end-of-quarter").unwrap(), SnapTarget::EndOfQuarter);
        assert_eq!(parse_snap("eom").unwrap(), SnapTarget::EndOfMonth);
        assert_eq!(parse_snap("start-of-year").unwrap(), SnapTarget::StartOfYear);
        assert!(parse_snap("end-of-week").is_err());
    }

    #[test]
    fn parse_nearest_reads_targets_and_rejects_unknown() {
        assert_eq!(parse_nearest("next-business-day").unwrap(), NearestTarget::NextBusinessDay);
        assert_eq!(parse_nearest("pbd").unwrap(), NearestTarget::PrevBusinessDay);
        assert!(parse_nearest("next-full-moon").is_err());
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
        let text = render_calc(&expr, &out, Some(0), &now, None, None);
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
        let text = render_calc(&expr, &out, Some(0), &now, None, None);
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
        let text = render_calc(&expr, &out, Some(0), &now, Some(&cal), None);
        assert!(text.contains("what this date tests:"), "got:\n{text}");
        assert!(text.contains("weekend - not a business day"), "got:\n{text}");
        assert!(text.contains("public holiday"), "got:\n{text}");
        // Without a calendar the same date names no calendar landmark (and here nothing at all).
        assert!(!render_calc(&expr, &out, Some(0), &now, None, None).contains("what this date tests:"));
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
        let text = render_calc(&expr, &out, ca.zone_bias_min, &now, None, None);
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

    /// Resolve a non-parametric preset's moment (empty parameter values) - the slice 16/17 path,
    /// now that parse and resolve are separate.
    fn resolve_no_params(p: Preset) -> Result<MomentExpr, PresetError> {
        let values = resolve_parameters(&p.parameters, &HashMap::new(), None)?;
        resolve_moment(p.moment, &values)
    }

    /// Build a --param map for tests.
    fn param_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A `both` preset over `today` with a snap step maps to the canonical moment and carries its
    /// English framing. `snap` speaks the same token as the `--snap` flag (one grammar).
    #[test]
    fn preset_maps_to_canonical_moment_with_framing() {
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "month-end",
            "name": { "en": "Last day of month", "pl": "x" },
            "explains": { "en": "Month-end close?", "pl": "x" },
            "applies_to": "both",
            "moment": { "base": "today", "steps": [ { "snap": "end-of-month" } ] }
        }"#;
        let p = parse_preset(json).unwrap();
        assert_eq!(p.id, "month-end");
        assert_eq!(p.name_en, "Last day of month");
        assert_eq!(p.explains_en, "Month-end close?");
        assert_eq!(p.applies_to, "both");
        let m = resolve_no_params(p).unwrap();
        assert_eq!(m.base, Base::Today);
        assert_eq!(m.steps, vec![Step::Snap(SnapTarget::EndOfMonth)]);
    }

    /// An `{ "absolute": ... }` base resolves to a fixed civil moment; a malformed one is refused at
    /// resolve time (the base is not parsed until then), never normalized silently.
    #[test]
    fn preset_absolute_base_parses_and_rejects_bad_date() {
        let ok = r#"{
            "schema": "chronomock.preset/1", "id": "epoch-zero",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
            "moment": { "base": { "absolute": "1970-01-01T00:00:00" }, "steps": [] }
        }"#;
        let m = resolve_no_params(parse_preset(ok).unwrap()).unwrap();
        assert_eq!(
            m.base,
            Base::Absolute(chrono_core::calc::CivilDateTime {
                year: 1970, month: 1, day: 1, hour: 0, minute: 0, second: 0
            })
        );
        assert!(m.steps.is_empty());

        let bad = ok.replace("1970-01-01T00:00:00", "2025-02-31T00:00:00");
        assert!(matches!(resolve_no_params(parse_preset(&bad).unwrap()), Err(PresetError::BadFile(_))));
    }

    /// An unknown major schema version is refused (docs/04 3.1), as a usage error.
    #[test]
    fn preset_unknown_schema_is_refused() {
        let json = r#"{
            "schema": "chronomock.preset/2", "id": "x",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
            "moment": { "base": "today", "steps": [] }
        }"#;
        let e = parse_preset(json).unwrap_err();
        assert!(matches!(e, PresetError::BadFile(_)));
        assert_eq!(e.exit_code(), 1);
    }

    /// A parameter type not built yet (int) is the honest "not built" (exit 5) at parse time, never
    /// guessed. (variant is now built - slice for age-of-majority.)
    #[test]
    fn preset_unbuilt_param_type_is_not_built() {
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "age",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "calculator",
            "parameters": [ { "id": "count", "type": "int" } ],
            "moment": { "base": "today", "steps": [] }
        }"#;
        let e = parse_preset(json).unwrap_err();
        assert!(matches!(e, PresetError::NotBuilt(_)));
        assert_eq!(e.exit_code(), 5);
    }

    /// A variant parameter resolves its label to a signed day offset that fills a sign-less shift step
    /// (docs/05 3.6): day_before -1, on_day 0, day_after +1. The shift carries the variant's direction.
    #[test]
    fn variant_parameter_fills_a_signless_shift() {
        use chrono_core::calc::{eval, CivilDateTime, EvalContext};
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "b",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "calculator",
            "parameters": [ { "id": "boundary", "type": "variant", "default": "day_before" } ],
            "moment": { "base": { "absolute": "2026-03-15T00:00:00" },
                        "steps": [ { "shift": { "parameter": "boundary" } } ] }
        }"#;
        let ctx = EvalContext {
            now: CivilDateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 },
            zone_bias_min: 0,
            calendar: None,
        };
        // Re-parse per case because resolve_moment consumes the raw moment (MomentDto is not Clone).
        let resolve = |cli: &[(&str, &str)]| {
            let p = parse_preset(json).unwrap();
            let vals = resolve_parameters(&p.parameters, &param_map(cli), None).unwrap();
            let expr = resolve_moment(p.moment, &vals).unwrap();
            eval(&expr, &ctx).unwrap().result().to_iso()
        };
        assert_eq!(resolve(&[]), "2026-03-14T00:00:00"); // default day_before -> the day before
        assert_eq!(resolve(&[("boundary", "day_after")]), "2026-03-16T00:00:00");
        assert_eq!(resolve(&[("boundary", "on_day")]), "2026-03-15T00:00:00"); // on_day -> unchanged
    }

    /// docs/04 4.1: a preset describes TIME, never a TARGET. There is no path field in the model, so
    /// a smuggled `"path"` is simply ignored - it cannot reach the moment. Structural enforcement.
    #[test]
    fn preset_ignores_a_path_field() {
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "sneaky", "path": "C:/evil.exe",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
            "moment": { "base": "today", "steps": [] }
        }"#;
        // It loads (unknown fields ignored, docs/04 section 3) and the result has no way to carry a
        // path - `Preset` has no such field. The moment is exactly the declared one.
        let m = resolve_no_params(parse_preset(json).unwrap()).unwrap();
        assert_eq!(m.base, Base::Today);
        assert!(m.steps.is_empty());
    }

    /// The calculator honours `applies_to`: substitution-only presets are not calculator questions.
    #[test]
    fn preset_applies_to_gates_the_calculator() {
        assert!(preset_targets_calculator("calculator"));
        assert!(preset_targets_calculator("both"));
        assert!(!preset_targets_calculator("substitution"));
    }

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

    /// Bad JSON is a usage-level bad-file error, not a panic.
    #[test]
    fn preset_bad_json_is_reported() {
        assert!(matches!(parse_preset("{ not json"), Err(PresetError::BadFile(_))));
    }

    // --- run --preset (Stage 4 slice 17): the substitution bridge, docs/06.3 pkt 3 -----------

    /// A preset's time_mode maps to the substitution wire shape: multiplier 1 (or absent) is flow,
    /// >1 is xN, <1 is refused. scale_duration_clock rides through.
    #[test]
    fn preset_time_mode_maps_to_wire_shape() {
        let none = time_mode_from(None).unwrap();
        assert_eq!(none.mode, "flow");
        assert_eq!(none.multiplier, None);
        assert!(!none.scale_duration);

        let flow = time_mode_from(Some(TimeModeDto { multiplier: Some(1), scale_duration_clock: false })).unwrap();
        assert_eq!((flow.mode.as_str(), flow.multiplier), ("flow", None));

        let xn = time_mode_from(Some(TimeModeDto { multiplier: Some(60), scale_duration_clock: true })).unwrap();
        assert_eq!((xn.mode.as_str(), xn.multiplier, xn.scale_duration), ("multiplier", Some(60), true));

        assert!(matches!(
            time_mode_from(Some(TimeModeDto { multiplier: Some(0), scale_duration_clock: false })),
            Err(PresetError::BadFile(_))
        ));
    }

    /// A preset carrying a time_mode object surfaces it on the loaded preset.
    #[test]
    fn preset_from_json_reads_time_mode() {
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "fast",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
            "moment": { "base": "today", "steps": [] },
            "time_mode": { "multiplier": 1440, "scale_duration_clock": true }
        }"#;
        let p = parse_preset(json).unwrap();
        assert_eq!(p.time_mode.mode, "multiplier");
        assert_eq!(p.time_mode.multiplier, Some(1440));
        assert!(p.time_mode.scale_duration);
    }

    /// The substitution surface honours applies_to: calculator-only presets are not run questions.
    #[test]
    fn preset_applies_to_gates_substitution() {
        assert!(preset_targets_substitution("substitution"));
        assert!(preset_targets_substitution("both"));
        assert!(!preset_targets_substitution("calculator"));
    }

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

    /// The canonical trial preset (docs/04 4.2): a `date` parameter fills the base, a `duration`
    /// parameter fills a shift. With both values the moment substitutes to a concrete expression.
    const TRIAL_JSON: &str = r#"{
        "schema": "chronomock.preset/1", "id": "trial-first-day-after",
        "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
        "parameters": [
            { "id": "trial_length", "type": "duration", "default": { "amount": 30, "unit": "days" } },
            { "id": "start_date", "type": "date", "default_hint": "target_file_creation" }
        ],
        "moment": {
            "base": { "parameter": "start_date" },
            "steps": [
                { "shift": { "sign": "+", "parameter": "trial_length" } },
                { "shift": { "sign": "+", "amount": 1, "unit": "days" } },
                { "set_time": "00:00:01" }
            ]
        }
    }"#;

    fn civil(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono_core::calc::CivilDateTime {
        chrono_core::calc::CivilDateTime { year: y, month: mo, day: d, hour: h, minute: mi, second: s }
    }

    #[test]
    fn param_date_base_and_duration_shift_substitute() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let values =
            resolve_parameters(&p.parameters, &param_map(&[("start_date", "2026-01-01"), ("trial_length", "30days")]), None)
                .unwrap();
        let m = resolve_moment(p.moment, &values).unwrap();
        assert_eq!(m.base, Base::Absolute(civil(2026, 1, 1, 0, 0, 0)));
        assert_eq!(
            m.steps,
            vec![
                Step::Shift { sign: Sign::Plus, amount: 30, unit: Unit::Days },
                Step::Shift { sign: Sign::Plus, amount: 1, unit: Unit::Days },
                Step::SetTime { hour: 0, minute: 0, second: 1 },
            ]
        );
    }

    /// A parameter with a file `default` (trial_length) may be omitted; the default is used.
    #[test]
    fn param_default_used_when_flag_absent() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let values = resolve_parameters(&p.parameters, &param_map(&[("start_date", "2026-01-01")]), None).unwrap();
        let m = resolve_moment(p.moment, &values).unwrap();
        assert_eq!(m.steps[0], Step::Shift { sign: Sign::Plus, amount: 30, unit: Unit::Days });
    }

    /// A required parameter with only a default_hint (start_date) is the honest "not built" in the
    /// calculator (exit 5) - the hint's target date is not available here. Never guessed.
    #[test]
    fn param_hint_only_needs_a_value_in_calc() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let e = resolve_parameters(&p.parameters, &param_map(&[]), None).unwrap_err();
        assert!(matches!(e, PresetError::NotBuilt(_)));
        assert_eq!(e.exit_code(), 5);
    }

    /// A --param naming no declared parameter is rejected (a silently ignored typo is a wrong result).
    #[test]
    fn param_unknown_id_is_rejected() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let e = resolve_parameters(&p.parameters, &param_map(&[("start_date", "2026-01-01"), ("nope", "1")]), None).unwrap_err();
        assert!(matches!(e, PresetError::BadFile(_)));
    }

    /// A --param value that does not parse against its type is a usage error, not a panic.
    #[test]
    fn param_bad_value_is_rejected() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        assert!(matches!(
            resolve_parameters(&p.parameters, &param_map(&[("start_date", "not-a-date")]), None),
            Err(PresetError::BadFile(_))
        ));
        let p2 = parse_preset(TRIAL_JSON).unwrap();
        assert!(matches!(
            resolve_parameters(&p2.parameters, &param_map(&[("start_date", "2026-01-01"), ("trial_length", "30frobs")]), None),
            Err(PresetError::BadFile(_))
        ));
    }

    /// A duration value in a date slot (or vice versa) is refused at substitution, not misread.
    #[test]
    fn param_wrong_type_for_slot_is_refused() {
        // Feed trial_length (a duration) where the base expects a date by pointing base at it.
        let json = r#"{
            "schema": "chronomock.preset/1", "id": "mismatch",
            "name": { "en": "n" }, "explains": { "en": "e" }, "applies_to": "both",
            "parameters": [ { "id": "d", "type": "duration", "default": { "amount": 5, "unit": "days" } } ],
            "moment": { "base": { "parameter": "d" }, "steps": [] }
        }"#;
        let p = parse_preset(json).unwrap();
        let values = resolve_parameters(&p.parameters, &param_map(&[]), None).unwrap();
        assert!(matches!(resolve_moment(p.moment, &values), Err(PresetError::BadFile(_))));
    }

    /// --param only makes sense with --preset.
    #[test]
    fn calc_param_needs_preset() {
        assert!(parse_calc_args(&["--param".into(), "start_date=2026-01-01".into()]).is_err());
        let ok = parse_calc_args(&["--preset".into(), "trial-first-day-after".into(), "--param".into(), "start_date=2026-01-01".into()]).unwrap();
        assert_eq!(ok.params.get("start_date").map(String::as_str), Some("2026-01-01"));
    }

    // --- run --param + default_hint (Stage 4 slice 19): trial in substitution ------------------

    /// The target_file_creation hint fills a date parameter from the target's file date (run only);
    /// without a target it is the honest not-built; a duration slot or an unbuilt hint is refused.
    #[test]
    fn hint_target_file_creation_resolves_only_with_a_target() {
        let d = civil(2025, 6, 15, 9, 30, 0);
        assert!(matches!(
            resolve_hint("start_date", ParamKind::Date, "target_file_creation", Some(d)),
            Ok(ParamValue::Date(x)) if x == d
        ));
        let e = resolve_hint("start_date", ParamKind::Date, "target_file_creation", None).unwrap_err();
        assert!(matches!(e, PresetError::NotBuilt(_)));
        assert_eq!(e.exit_code(), 5);
        assert!(matches!(
            resolve_hint("d", ParamKind::Duration, "target_file_creation", Some(d)),
            Err(PresetError::BadFile(_))
        ));
        assert!(matches!(
            resolve_hint("x", ParamKind::Date, "somewhere_else", Some(d)),
            Err(PresetError::NotBuilt(_))
        ));
    }

    /// In run, the trial's start_date resolves from the target date and trial_length from its default,
    /// with no --param at all - the flagship "trial in substitution" flow.
    #[test]
    fn param_hint_resolves_from_target_date_in_run() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let target = civil(2025, 1, 10, 0, 0, 0);
        let values = resolve_parameters(&p.parameters, &param_map(&[]), Some(target)).unwrap();
        assert!(matches!(values.get("start_date"), Some(ParamValue::Date(x)) if *x == target));
        assert!(matches!(
            values.get("trial_length"),
            Some(ParamValue::Duration { amount: 30, unit: Unit::Days })
        ));
    }

    /// --param wins over the hint even when a target date is available.
    #[test]
    fn param_flag_overrides_hint_in_run() {
        let p = parse_preset(TRIAL_JSON).unwrap();
        let target = civil(2025, 1, 10, 0, 0, 0);
        let values =
            resolve_parameters(&p.parameters, &param_map(&[("start_date", "2030-12-31")]), Some(target)).unwrap();
        assert!(matches!(values.get("start_date"), Some(ParamValue::Date(x)) if *x == civil(2030, 12, 31, 0, 0, 0)));
    }

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
}
