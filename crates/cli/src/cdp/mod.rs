//! The Chromium/Electron substitution mechanism (F1-F4): instead of injecting a native hook, we
//! speak the Chrome DevTools Protocol to the target's own JS engine and override its time APIs. The
//! browser holds the clock, a JS shim is the "hook", and CDP over a WebSocket is the wire - the same
//! rdzeni<->interfejs shape as the native `__core` over NDJSON (ADR-6), one layer down.
//!
//! This module is the transport + JSON-RPC layer. Target detection, launch, the time shim, and the
//! session/report wiring live in sibling modules (built in later slices).

mod launch;
mod ws;

use serde_json::Value;
use std::io::{self, BufRead, Read, Write};
use std::net::TcpStream;

pub use launch::{is_chromium_target, launch_chromium};
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
        Ok(CdpClient { ws, next_id: 1, queued: std::collections::VecDeque::new() })
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

        loop {
            match self.read_msg()? {
                Msg::Response { id: rid, result, error } if rid == id => {
                    return match error {
                        Some(e) => Err(io::Error::other(format!(
                            "CDP {method} failed: {}",
                            e.get("message").and_then(Value::as_str).unwrap_or("unknown")
                        ))),
                        None => Ok(result),
                    };
                }
                other => self.queued.push_back(other),
            }
        }
    }

    /// Return the next message (a queued one first), for an event-driven loop. Blocks on the socket.
    /// Used by the auto-attach event loop in slice C3.
    #[allow(dead_code)]
    pub fn next(&mut self) -> io::Result<Msg> {
        if let Some(m) = self.queued.pop_front() {
            return Ok(m);
        }
        self.read_msg()
    }

    fn read_msg(&mut self) -> io::Result<Msg> {
        let text = self.ws.recv_text()?;
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad CDP JSON: {e}")))?;
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            Ok(Msg::Response {
                id,
                result: v.get("result").cloned().unwrap_or(Value::Null),
                error: v.get("error").cloned(),
            })
        } else {
            Ok(Msg::Event {
                method: v.get("method").and_then(Value::as_str).unwrap_or("").to_string(),
                params: v.get("params").cloned().unwrap_or(Value::Null),
                session_id: v.get("sessionId").and_then(Value::as_str).map(str::to_string),
            })
        }
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
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
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

    let body = match content_length {
        Some(len) => {
            let mut b = vec![0u8; len];
            reader.read_exact(&mut b)?;
            b
        }
        None => {
            let mut b = Vec::new();
            reader.read_to_end(&mut b)?;
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
}
