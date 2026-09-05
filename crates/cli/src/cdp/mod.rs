//! The Chromium/Electron substitution mechanism (F1-F4): instead of injecting a native hook, we
//! speak the Chrome DevTools Protocol to the target's own JS engine and override its time APIs. The
//! browser holds the clock, a JS shim is the "hook", and CDP over a WebSocket is the wire - the same
//! rdzeni<->interfejs shape as the native `__core` over NDJSON (ADR-6), one layer down.
//!
//! This module is the transport + JSON-RPC layer. Target detection, launch, the time shim, and the
//! session/report wiring live in sibling modules (built in later slices).

mod launch;
mod session;
mod ws;

use serde_json::Value;
use std::io::{self, BufRead, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

pub use launch::{is_chromium_target, launch_chromium};
pub use session::{build_shim, inject_page, inject_worker, is_shimmable, is_worker, COUNTS_EXPR};
pub use ws::WsClient;

/// One decoded CDP message: either a reply to a command we sent, or an event the browser pushed.
pub enum Msg {
    /// A reply to command `id`. `error` is set when the command failed.
    Response { id: u64, result: Value, error: Option<Value> },
    /// An unsolicited event, e.g. `Target.attachedToTarget`. `session_id` is set for a message from
    /// an attached (flattened) target session. Read by the event loop in slice C3.
    #[allow(dead_code)]
    Event { method: String, params: Value, session_id: Option<String> },
}

/// A live CDP session over one WebSocket. Commands are sent with a monotonic id; events that arrive
/// while waiting for a reply are queued so a later event loop can drain them (`next`).
pub struct CdpClient {
    ws: WsClient,
    next_id: u64,
    queued: std::collections::VecDeque<Msg>,
    /// Set once the queue has had to drop an event, so the notice is printed a single time rather
    /// than on every subsequent drop.
    queue_overflow_warned: bool,
}

/// Cap on events parked while waiting for a command reply. CDP events are small and the session
/// loop drains them, so this is far above any healthy rate - it exists so a runaway target cannot
/// grow the deque without limit (the same defence-in-depth as `MAX_WS_BYTES`, one layer up).
const MAX_QUEUED_EVENTS: usize = 10_000;

/// Push onto a bounded queue, dropping the oldest entry when it is already full. Returns whether a
/// drop happened, so the caller can say so once. Split out from the client so the bound is testable
/// without a socket.
fn push_bounded(queue: &mut std::collections::VecDeque<Msg>, msg: Msg) -> bool {
    let dropped = queue.len() >= MAX_QUEUED_EVENTS;
    if dropped {
        queue.pop_front();
    }
    queue.push_back(msg);
    dropped
}

/// Fold text the TARGET supplied into something that cannot forge output.
///
/// Everything this tool prints about a CDP session carries words the target chose: an error it
/// reported, the URL it advertised for its own debugger, the type it gave a context. That text
/// reaches stderr and the session report - and the report is EVIDENCE (untouchable rule 4), so a
/// newline inside it would add a line no part of this tool wrote, which is the one thing a report
/// must never contain. The target is untrusted by construction: it is an arbitrary executable the
/// user pointed us at, and everything it says arrives over a socket.
///
/// Control characters become a visible escape rather than being dropped, so nothing disappears
/// silently and the text stays readable, and the result is capped for the reason MAX_WS_BYTES exists
/// one layer up - a line of evidence has no business being unbounded. Backslashes are left alone
/// deliberately: escaping them would turn every Windows path in a target's message into noise, and
/// the property needed here is "cannot add a line", not "round-trips exactly".
pub(crate) fn sanitise_target_text(text: &str) -> String {
    const MAX_CHARS: usize = 200;
    let mut out = String::with_capacity(text.len().min(MAX_CHARS * 2));
    for (seen, c) in text.chars().enumerate() {
        if seen == MAX_CHARS {
            out.push_str(" (truncated)");
            break;
        }
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // U+2028 and U+2029 are not control characters, but plenty of readers break a line on
            // them - including the JS engine at the other end of this very protocol.
            c if c.is_control() || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{{{:04x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// The error a target reported for a call, as an `io::Error`. Split out of `send` so the one place
/// that quotes a target's own words has a name and a test - those words come off the wire and end up
/// on stderr, so they go through `sanitise_target_text` on the way.
fn target_error(method: &str, error: &Value) -> io::Error {
    io::Error::other(format!(
        "CDP {method} failed: {}",
        sanitise_target_text(error.get("message").and_then(Value::as_str).unwrap_or("unknown"))
    ))
}

impl CdpClient {
    /// Discover the browser-level WebSocket endpoint from `http://host:port/json/version` and connect
    /// to it. This is the endpoint that carries the `Target` domain, so it can reach every page and
    /// worker in the target.
    pub fn connect_to_port(host: &str, port: u16) -> io::Result<CdpClient> {
        let version = http_get_json(host, port, "/json/version")?;
        let url = version
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no webSocketDebuggerUrl in /json/version"))?;
        let (ws_host, ws_port, ws_path) = parse_ws_url(url)?;
        if !ws_endpoint_is_ours(&ws_host, ws_port, port) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ws url is not the endpoint we opened: {}", sanitise_target_text(url)),
            ));
        }
        let ws = WsClient::connect(&ws_host, ws_port, &ws_path)?;
        Ok(CdpClient {
            ws,
            next_id: 1,
            queued: std::collections::VecDeque::new(),
            queue_overflow_warned: false,
        })
    }

    /// Send a command and block until its reply arrives, queuing any events seen in between. Returns
    /// the `result` object (or an error carrying the CDP `error.message`).
    pub fn call(&mut self, method: &str, params: Value, session_id: Option<&str>) -> io::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let mut req = serde_json::Map::new();
        req.insert("id".into(), Value::from(id));
        req.insert("method".into(), Value::from(method));
        req.insert("params".into(), params);
        if let Some(sid) = session_id {
            req.insert("sessionId".into(), Value::from(sid));
        }
        self.ws.send_text(&Value::Object(req).to_string())?;

        // A reply should come promptly; poll until it does, bounded so a hung target cannot block us.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            // Checked every pass, not only when the socket goes quiet. A target that keeps pushing
            // events - a page logging in a loop, a worker chattering - would otherwise never let the
            // deadline branch run, and a command whose reply never comes (a dead sessionId, say)
            // would block the whole session loop indefinitely: no heartbeat, no `end`, no liveness
            // check, and the GUI's 15 s watchdog calling a healthy core unresponsive.
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("CDP {method} timed out waiting for a reply"),
                ));
            }
            match self.poll_msg()? {
                Some(Msg::Response { id: rid, result, error }) if rid == id => {
                    return match error {
                        Some(e) => Err(target_error(method, &e)),
                        None => Ok(result),
                    };
                }
                Some(other) => self.queue_event(other),
                None => {}
            }
        }
    }

    /// Park an event seen while waiting for a reply, bounded. The queue is drained by the session
    /// loop, but nothing guarantees it drains as fast as a chatty target fills it, so an unbounded
    /// deque would grow with the target's event rate. Past the cap the OLDEST event is dropped:
    /// events are diagnostics here (coverage comes from polled counters), so losing the stalest one
    /// costs less than growing without limit - and the drop says so once, rather than silently.
    fn queue_event(&mut self, msg: Msg) {
        if push_bounded(&mut self.queued, msg) && !self.queue_overflow_warned {
            self.queue_overflow_warned = true;
            eprintln!(
                "chrono core: CDP event queue hit {MAX_QUEUED_EVENTS} - dropping the oldest events"
            );
        }
    }

    /// Poll for the next message (a queued one first), returning `None` when the poll interval elapsed
    /// with nothing ready - so an event loop can check target liveness between events. Blocks at most
    /// one poll interval on the socket.
    pub fn poll(&mut self) -> io::Result<Option<Msg>> {
        if let Some(m) = self.queued.pop_front() {
            return Ok(Some(m));
        }
        self.poll_msg()
    }

    fn poll_msg(&mut self) -> io::Result<Option<Msg>> {
        let Some(text) = self.ws.poll_text()? else {
            return Ok(None);
        };
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad CDP JSON: {e}")))?;
        let msg = if let Some(id) = v.get("id").and_then(Value::as_u64) {
            Msg::Response {
                id,
                result: v.get("result").cloned().unwrap_or(Value::Null),
                error: v.get("error").cloned(),
            }
        } else {
            Msg::Event {
                method: v.get("method").and_then(Value::as_str).unwrap_or("").to_string(),
                params: v.get("params").cloned().unwrap_or(Value::Null),
                session_id: v.get("sessionId").and_then(Value::as_str).map(str::to_string),
            }
        };
        Ok(Some(msg))
    }
}

/// Split a `ws://host:port/path` URL into its parts. CDP only ever hands us plain `ws://` loopback
/// URLs, so `wss://` and userinfo are out of scope.
fn parse_ws_url(url: &str) -> io::Result<(String, u16, String)> {
    let rest = url
        .strip_prefix("ws://")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("not a ws:// url: {}", sanitise_target_text(url))))?;
    let slash = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..slash];
    let path = if slash < rest.len() { &rest[slash..] } else { "/" };
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("no port in ws url: {}", sanitise_target_text(url))))?;
    let port: u16 = port
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("bad port in ws url: {}", sanitise_target_text(url))))?;
    Ok((host.to_string(), port, path.to_string()))
}

/// Loopback names a DevTools endpoint may legitimately give for itself. An allow-list rather than a
/// parse: every form here is this machine, and anything else is somewhere we did not choose to go.
const LOOPBACK_HOSTS: [&str; 4] = ["127.0.0.1", "localhost", "::1", "[::1]"];

/// Whether the ws endpoint the target advertised is the one we already chose to talk to.
///
/// `webSocketDebuggerUrl` is text the TARGET wrote, and the code that reads it then connects there -
/// so without this the target names any `host:port` it likes and the driver goes, carrying the
/// content of our CDP commands with it. That is a server-side request forgery whose trigger is the
/// application under test, which is untrusted by construction (it is an arbitrary executable the
/// user pointed us at).
///
/// The port must match EXACTLY the one we discovered, because "go somewhere else" is the whole of
/// the attack and a different port is already somewhere else. The host is checked against the
/// loopback list rather than compared, because the name a browser uses for itself is cosmetic and
/// not worth a regression: measured 2026-09-05 on two engines six years apart - Chrome 83 (in
/// Pomotroid) and Edge 152 - both answer with the exact host and port we asked on (`127.0.0.1`),
/// but a build that said `localhost` would be equally legitimate and equally harmless.
fn ws_endpoint_is_ours(ws_host: &str, ws_port: u16, our_port: u16) -> bool {
    ws_port == our_port && LOOPBACK_HOSTS.iter().any(|h| h.eq_ignore_ascii_case(ws_host))
}

/// A tiny blocking HTTP/1.1 GET that returns the JSON body. Only for the loopback CDP HTTP endpoints
/// (`/json/version`, `/json`). Reads the body by `Content-Length` - the DevTools HTTP server keeps
/// the connection alive despite `Connection: close`, so reading to EOF would block forever. A read
/// timeout guards against an unresponsive endpoint hanging the tool.
pub fn http_get_json(host: &str, port: u16, path: &str) -> io::Result<Value> {
    let stream = TcpStream::connect((host, port))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let writer = stream.try_clone()?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    (&writer).write_all(request.as_bytes())?;
    (&writer).flush()?;

    let mut reader = std::io::BufReader::new(stream);
    let mut content_length: Option<usize> = None;
    // The body is capped below, and the header block gets the same treatment (R2-N9): a peer that
    // never sends the blank line would otherwise be read header by header without bound. The peer is
    // our own Chromium, so this is defence in depth, not a hole - but "the peer is trustworthy" is
    // exactly the assumption the body cap already declined to make.
    const MAX_HEADERS: usize = 200;
    const MAX_HEADER_LINE: usize = 8 * 1024;
    for _ in 0..MAX_HEADERS {
        let mut line = String::new();
        let n = reader.by_ref().take(MAX_HEADER_LINE as u64).read_line(&mut line)?;
        if n == 0 {
            break; // EOF before the blank line
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(rest) = trimmed.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = rest.trim().parse().ok();
        }
    }

    // Cap the body (P3, pre-release audit): a hostile or broken peer's Content-Length must not drive a huge
    // up-front allocation, and a bodiless response must not read without bound. The DevTools JSON we fetch
    // (the target list, the WS URL) is tiny, so this ceiling is generous.
    const MAX_HTTP_BODY: usize = 16 * 1024 * 1024;
    let body = match content_length {
        Some(len) => {
            if len > MAX_HTTP_BODY {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "HTTP Content-Length exceeds the cap"));
            }

            let mut b = vec![0u8; len];
            reader.read_exact(&mut b)?;
            b
        }
        None => {
            let mut b = Vec::new();
            reader.by_ref().take(MAX_HTTP_BODY as u64).read_to_end(&mut b)?;
            b
        }
    };
    serde_json::from_slice(&body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("HTTP body was not JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_text_cannot_add_a_line() {
        assert_eq!(sanitise_target_text("one\ntwo"), "one\\ntwo");
        assert_eq!(sanitise_target_text("carriage\rreturn"), "carriage\\rreturn");
        assert_eq!(sanitise_target_text("tab\there"), "tab\\there");
        // An escape sequence would otherwise let a target repaint the terminal it is reported in.
        assert_eq!(sanitise_target_text("esc\u{1b}[31m"), "esc\\u{001b}[31m");
        assert_eq!(sanitise_target_text("sep\u{2028}arator"), "sep\\u{2028}arator");
    }

    #[test]
    fn target_text_leaves_ordinary_words_alone() {
        let real = "Uncaught TypeError: x is not a function";
        assert_eq!(sanitise_target_text(real), real);
        // Backslashes stay: a Windows path in a target's message must still read as one.
        assert_eq!(sanitise_target_text(r"C:\Users\qa\app.exe"), r"C:\Users\qa\app.exe");
        assert_eq!(sanitise_target_text(""), "");
    }

    #[test]
    fn target_text_is_capped_on_a_character_not_a_byte() {
        // Multi-byte on purpose: a byte-wise cut would split the character and panic.
        let long = "\u{105}".repeat(500);
        let out = sanitise_target_text(&long);
        assert!(out.ends_with(" (truncated)"), "{out}");
        assert_eq!(out.chars().filter(|c| *c == '\u{105}').count(), 200);
    }

    #[test]
    fn a_target_error_cannot_forge_a_report_line() {
        let reported = serde_json::json!({ "message": "boom\nchrono core: verdict: works" });
        let err = target_error("Runtime.evaluate", &reported);
        let text = err.to_string();
        assert!(!text.contains('\n'), "target words reached the message raw: {text}");
        assert!(text.contains("boom\\nchrono core"), "{text}");
    }

    #[test]
    fn a_bad_ws_url_from_the_target_cannot_forge_a_line() {
        let err = parse_ws_url("nonsense\nchrono core: verdict: works").unwrap_err();
        let text = err.to_string();
        assert!(!text.contains('\n'), "target words reached the message raw: {text}");
    }

    #[test]
    fn parses_a_browser_ws_url() {
        let (h, p, path) = parse_ws_url("ws://127.0.0.1:9333/devtools/browser/abc-123").unwrap();
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 9333);
        assert_eq!(path, "/devtools/browser/abc-123");
    }

    #[test]
    fn rejects_non_ws_url() {
        assert!(parse_ws_url("http://127.0.0.1:9333/x").is_err());
        assert!(parse_ws_url("ws://127.0.0.1/x").is_err()); // no port
    }

    /// The target writes `webSocketDebuggerUrl`, so it gets to name where we connect next. A host it
    /// chose is a request forgery with our driver as the courier - the one thing the endpoint check
    /// exists to stop.
    #[test]
    fn a_ws_url_pointing_off_this_machine_is_refused() {
        for host in ["evil.example.com", "10.0.0.5", "169.254.169.254", "127.0.0.1.evil.com"] {
            assert!(!ws_endpoint_is_ours(host, 9333, 9333), "host {host} was accepted");
        }
    }

    /// A different port is already somewhere we did not choose to go, even on this machine - another
    /// service listening on loopback is exactly the interesting target for a forged request.
    #[test]
    fn a_ws_url_on_another_port_is_refused() {
        assert!(!ws_endpoint_is_ours("127.0.0.1", 9334, 9333));
        assert!(!ws_endpoint_is_ours("localhost", 80, 9333));
    }

    /// What Chrome 83 and Edge 152 actually answer (measured), plus the loopback spellings a
    /// different build could legitimately use. Refusing these would be a regression, not a fix.
    #[test]
    fn the_endpoint_we_opened_is_accepted_however_it_spells_loopback() {
        for host in ["127.0.0.1", "localhost", "LocalHost", "::1", "[::1]"] {
            assert!(ws_endpoint_is_ours(host, 9333, 9333), "host {host} was refused");
        }
    }

    fn event(n: u64) -> Msg {
        Msg::Event { method: format!("E{n}"), params: Value::from(n), session_id: None }
    }

    fn method_of(m: &Msg) -> String {
        match m {
            Msg::Event { method, .. } => method.clone(),
            Msg::Response { .. } => "response".to_string(),
        }
    }

    /// S-15 regression. Events parked while waiting for a command reply used to accumulate without
    /// any bound, so a chatty target grew the deque with its own event rate. Now the queue is capped
    /// and the OLDEST is dropped - the newest events are the ones still worth having.
    #[test]
    fn the_event_queue_is_bounded_and_drops_the_oldest() {
        let mut q = std::collections::VecDeque::new();
        for n in 0..MAX_QUEUED_EVENTS as u64 {
            assert!(!push_bounded(&mut q, event(n)), "nothing is dropped below the cap");
        }
        assert_eq!(q.len(), MAX_QUEUED_EVENTS);
        assert_eq!(method_of(q.front().unwrap()), "E0");

        assert!(push_bounded(&mut q, event(9_999_999)), "past the cap a drop is reported");
        assert_eq!(q.len(), MAX_QUEUED_EVENTS, "the queue does not grow past the cap");
        assert_eq!(method_of(q.front().unwrap()), "E1", "the oldest went, not the newest");
        assert_eq!(method_of(q.back().unwrap()), "E9999999");
    }
}
