//! Hidden diagnostic probes for the Chromium path: `__cdp-probe`, `__cdp-launch`, `__cdp-shim`,
//! `__cdp-date`.
//!
//! Not user commands. They exercise the transport, the launch and the shim one layer at a time, so a
//! Chromium failure can be located without a whole session around it.
//!
//! Kept out of `cdp/`, which is the transport client and names `crate::` nowhere - a property worth
//! more than the tidier directory. These probes are the product USING that client.


use crate::cdp;
use crate::zone::{moment_epoch_ms, now_epoch_ms};
/// Hidden probe (CDP slice C1 verification): connect to a running Chromium/Electron debug port and
/// print what CDP sees. Not a user command - it proves the WebSocket + JSON-RPC transport against a
/// real target before the launch, shim, and report wiring are built on top.
pub(crate) fn cdp_probe(argv: &[String]) -> i32 {
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
                println!("{}", probe_target_line("-", ty, url));
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
pub(crate) fn cdp_launch_probe(argv: &[String]) -> i32 {
    let Some(target) = argv.first() else {
        eprintln!("usage: chrono __cdp-launch <target-exe>");
        return 1;
    };
    if !cdp::is_chromium_target(target) {
        println!("not a chromium target: {target}");
        return 1;
    }
    println!("chromium target detected: {target}");

    let launched = match cdp::launch_chromium(target, &[], None, || {}) {
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

/// Hidden probe (CDP slice C3 verification): launch a Chromium target, auto-attach to every context,
/// inject the time shim through the production path, then (Pomotroid-specific) drive its worker timer
/// and measure whether the app's own countdown accelerates by the multiplier. Proves the shim reaches
/// the sandboxed worker and speeds it up - the whole point of Chromium mode.
pub(crate) fn cdp_shim_probe(argv: &[String]) -> i32 {
    let Some(target) = argv.first() else {
        eprintln!("usage: chrono __cdp-shim <target-exe> [multiplier]");
        return 1;
    };
    let mult: i64 = argv.get(1).and_then(|s| s.parse().ok()).unwrap_or(60);
    if !cdp::is_chromium_target(target) {
        println!("not a chromium target: {target}");
        return 1;
    }

    let launched = match cdp::launch_chromium(target, &[], None, || {}) {
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
                    Ok(()) => println!("{}", probe_target_line("shimmed", &ty, &url)),
                    // `e` is already folded at its source (`evaluate_shim`); `ty` is not.
                    Err(e) => println!("  FAILED  {}: {e}", cdp::sanitise_target_text(&ty)),
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
pub(crate) fn cdp_date_probe(argv: &[String]) -> i32 {
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

    let launched = match cdp::launch_chromium(target, &[], None, || {}) {
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
                // `reads` is a string the PAGE built and returned, so it is the target's words like
                // any other - a newline in it would add a line to this probe's output.
                println!(
                    "page reads [new Date().toISOString(), getUTCFullYear(), Date.now()]: {}",
                    cdp::sanitise_target_text(reads)
                );
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
pub(crate) fn short_url(url: &str) -> String {
    url.chars().take(66).collect()
}

/// One `<label> <type> :: <url>` line about a target, as the hidden probes print it. Split out for
/// the reason `target_error` was (`cdp/mod.rs`): both halves are words the TARGET chose - the type it
/// gave its own context and the URL it advertised - so the one place that quotes them has a name and
/// a test, and a second caller cannot quietly reintroduce the raw form.
///
/// Trimmed by CHARACTERS before sanitising, in that order: `short_url` cannot split a character, and
/// sanitising first would let an escape (`\n` becoming two characters) be cut in half by the trim.
pub(crate) fn probe_target_line(label: &str, ty: &str, url: &str) -> String {
    format!(
        "  {label} {} :: {}",
        cdp::sanitise_target_text(ty),
        cdp::sanitise_target_text(&short_url(url))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The probe's target list quotes two strings the target chose, and this one call site kept the
    /// byte slice that S-20 removed everywhere else - so a target whose URL carried a non-ASCII
    /// profile path crashed the probe, and one whose context type carried a newline could add a line
    /// of output nothing in this tool wrote.
    #[test]
    fn a_probe_target_line_cannot_panic_or_forge_a_line() {
        // The removed slice was `&url[..70]`, so byte 70 has to land INSIDE a character or this
        // proves nothing. Asserted rather than reasoned about: the first version of this test used a
        // 34-byte prefix, which put byte 70 exactly on a boundary - it went green against the very
        // code it was meant to catch, and only the forgery half of it was doing any work.
        let url = format!("ws://127.0.0.1:9333/devtools/page/x{}", "ó".repeat(60));
        assert!(!url.is_char_boundary(70), "test URL stopped splitting a character at byte 70");
        let line = probe_target_line("-", "page", &url);
        assert!(line.starts_with("  - page :: ws://127.0.0.1:9333/"));
        // Trimmed by characters, so the URL half is exactly what `short_url` yields.
        assert!(line.ends_with(&short_url(&url)));

        let forged = probe_target_line("-", "page\nchrono core: verdict: works", "ws://127.0.0.1:9333/x");
        assert!(!forged.contains('\n'), "target words reached the line raw: {forged}");
    }
}
