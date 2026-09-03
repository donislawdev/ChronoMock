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
                        Some(e) => Err(io::Error::other(format!(
                            "CDP {method} failed: {}",
                            e.get("message").and_then(Value::as_str).unwrap_or("unknown")
                        ))),
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
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("not a ws:// url: {url}")))?;
    let slash = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..slash];
    let path = if slash < rest.len() { &rest[slash..] } else { "/" };
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("no port in ws url: {url}")))?;
    let port: u16 = port
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("bad port in ws url: {url}")))?;
    Ok((host.to_string(), port, path.to_string()))
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
