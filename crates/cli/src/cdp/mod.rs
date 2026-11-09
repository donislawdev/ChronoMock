//! The Chromium/Electron substitution mechanism (F1-F4): instead of injecting a native hook, we
//! speak the Chrome DevTools Protocol to the target's own JS engine and override its time APIs. The
//! browser holds the clock, a JS shim is the "hook", and CDP over a WebSocket is the wire - the same
//! core<->interface shape as the native `__core` over NDJSON (ADR-6), one layer down.
//!
//! This module is the transport + JSON-RPC layer. Target detection, launch, the time shim, and the
//! session/report wiring live in sibling modules (built in later slices).

mod launch;
mod session;
#[cfg(test)]
mod shim_exec;
mod ws;

use serde_json::Value;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

pub use launch::{
    is_chromium_target, is_electron_target, launch_chromium, with_background_timers, BACKGROUND_TIMER_SWITCHES,
};
pub use session::{
    build_shim, inject_page, inject_worker, is_shimmable, is_worker, release_expr, script_identifier, set_expr,
    starts_workers, zone_id, zone_read_expr, Injected, ScheduledRate, COUNTED_APIS, COUNTS_EXPR,
};
pub use ws::WsClient;
/// The scripted browser the transport's own tests talk to, for the attacher's tests outside this module.
#[cfg(test)]
pub(crate) use ws::tests::{fake_browser_holding, fake_browser_paced};

/// One decoded CDP message: either a reply to a command we sent, or an event the browser pushed.
pub enum Msg {
    /// A reply to command `id`. `error` is set when the command failed.
    Response { id: u64, result: Value, error: Option<Value> },
    /// An unsolicited event, e.g. `Target.attachedToTarget`. `session_id` is set for a message from
    /// an attached (flattened) target session. Read by the event loop in slice C3.
    #[allow(dead_code)]
    Event { method: String, params: Value, session_id: Option<String> },
}

/// A live CDP session over one WebSocket. Commands are sent with a monotonic id - events that arrive
/// while waiting for a reply are queued so a later event loop can drain them (`next`).
/// How long to wait for a reply to one CDP command.
///
/// It must stay BELOW the client's idle watchdog, and that is the whole point of naming it. The
/// bound was added so a command whose reply never comes could not park the session loop for ever -
/// the comment at the deadline says, in as many words, that the risk being avoided is "the GUI's
/// 15 s watchdog calling a healthy core unresponsive". The value chosen was 20 s, which is longer
/// than that watchdog, so the guard did not close by five seconds (R3-3). A stuck target now costs
/// at most ten seconds of silence, and `RustTimeoutMirrorTests` fails the build if the watchdog is
/// ever moved below this.
pub const CALL_DEADLINE_SECS: u64 = 10;

/// How long reaching a DevTools endpoint may take as a whole: the connect, `/json/version` and the
/// WebSocket upgrade, under one deadline (R4-N26). Each read used to get these ten seconds of its own,
/// so a peer answering a byte at a time had no bound at all.
pub const CONNECT_DEADLINE: Duration = Duration::from_secs(10);

pub struct CdpClient {
    ws: WsClient,
    next_id: u64,
    queued: std::collections::VecDeque<Msg>,
    /// Set once the queue has had to drop a spent message, so the notice is printed a single time
    /// rather than on every subsequent drop.
    queue_overflow_warned: bool,
    /// Set once the queue has had to drop a `Target.*` event - its own notice, because that is the
    /// drop that can leave a context paused, and an earlier notice about spent messages must not
    /// stand for it.
    target_drop_warned: bool,
    /// Set once a message that was not JSON even after repair has been skipped, for the same reason.
    bad_json_warned: bool,
    /// How long `call` waits for its reply. [`CALL_DEADLINE_SECS`] unless the owner asked for less:
    /// a loop that has other work to do in the meantime - the native session's heartbeat and child
    /// poll - cannot afford ten seconds of standing on a renderer busy with its own JS.
    call_deadline: Duration,
    /// The target the caller is putting the shim into, and the session it does that in, while it does:
    /// another session of that target is let go as soon as it is announced ([`CdpClient::attaching`]).
    attaching: Option<(String, String)>,
}

/// Cap on events parked while waiting for a command reply. CDP events are small and the session
/// loop drains them, so this is far above any healthy rate - it exists so a runaway target cannot
/// grow the deque without limit (the same defence-in-depth as `MAX_WS_BYTES`, one layer up).
const MAX_QUEUED_EVENTS: usize = 10_000;

/// What [`push_bounded`] let go of to make room.
#[derive(Debug, PartialEq, Eq)]
enum Dropped {
    Nothing,
    /// A reply or an event the session does not read.
    Spent,
    /// A `Target.*` event, from a queue holding nothing else at twice the cap.
    Target,
}

/// Push onto a bounded queue, making room when it is already full, and say what went, so the caller
/// can report it - once for each kind. Split out from the client so the bound is testable without
/// a socket.
///
/// What goes is chosen, not the plain oldest (R4-S14): the oldest message that is not a `Target.*`
/// event. That is a reply nobody waits for any more (its call already timed out) or an event no part
/// of the session reads - only `Target.*` events are read. A `Target.*` event goes only when nothing
/// else is there: an `attachedToTarget` is how a target auto-attach paused on start gets its shim
/// and its release, and losing it left that target paused for good. A queue of nothing but target
/// events may grow to twice the cap before the oldest of them goes. The search stops at the first
/// message that is not a target event, which in a queue of ordinary traffic is the front.
fn push_bounded(queue: &mut std::collections::VecDeque<Msg>, msg: Msg) -> Dropped {
    let mut dropped = Dropped::Nothing;
    if queue.len() >= MAX_QUEUED_EVENTS {
        if let Some(i) = queue.iter().position(|m| !is_target_event(m)) {
            queue.remove(i);
            dropped = Dropped::Spent;
        } else if queue.len() >= MAX_QUEUED_EVENTS * 2 {
            queue.pop_front();
            dropped = Dropped::Target;
        }
    }
    queue.push_back(msg);
    dropped
}

/// A `Target.*` event: what attaches, detaches and destroys the contexts the session drives.
fn is_target_event(msg: &Msg) -> bool {
    matches!(msg, Msg::Event { method, .. } if method.starts_with("Target."))
}

/// Replace every lone UTF-16 surrogate escape in JSON text (`\uD800`-`\uDFFF` without its pair) with
/// the escape of U+FFFD, the replacement character, the way a decoder does. `None` when there was
/// nothing to replace.
///
/// Chromium writes the JS strings it reports as JSON escapes, a lone surrogate included - a window
/// name cut in the middle of an emoji, a page title - and serde_json refuses the whole message for one
/// (R4-S15). Refusing it ended the CDP session: the core reported the app closed and ended it. The
/// scan follows JSON's own escapes, so an escaped backslash before a literal `u` is not mistaken for one.
fn repair_lone_surrogates(text: &str) -> Option<String> {
    fn unit_at(b: &[u8], i: usize) -> Option<u16> {
        if b.get(i) == Some(&b'\\') && b.get(i + 1) == Some(&b'u') {
            let hex = std::str::from_utf8(b.get(i + 2..i + 6)?).ok()?;
            return u16::from_str_radix(hex, 16).ok();
        }
        None
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        match unit_at(b, i) {
            Some(0xD800..=0xDBFF) if matches!(unit_at(b, i + 6), Some(0xDC00..=0xDFFF)) => {
                i += 12; // a proper pair stays as it is
            }
            Some(0xD800..=0xDFFF) => {
                out.push_str(&text[copied..i]);
                out.push_str("\\uFFFD");
                i += 6;
                copied = i;
                changed = true;
            }
            Some(_) => i += 6,
            None => i += 2, // any other escape, `\\` included, is two bytes
        }
    }
    if !changed {
        return None;
    }
    out.push_str(&text[copied..]);
    Some(out)
}

/// A host the way `TcpStream::connect` takes it: `[::1]` written in a URL is the address `::1`.
/// Connecting to the bracketed form asked the resolver for a name that does not exist (R4-N24).
fn bare_host(host: &str) -> &str {
    host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host)
}

/// A host the way an HTTP `Host` header writes it next to a port: an IPv6 address in brackets.
/// `::1:9222` is not a host and a port, and the DevTools server answers it with an error.
fn header_host(host: &str) -> String {
    let bare = bare_host(host);
    if bare.contains(':') { format!("[{bare}]") } else { bare.to_string() }
}

/// The addresses a loopback host names, without asking a resolver: an address written out is itself,
/// and `localhost` is both loopback addresses. Anything else is refused - every endpoint this client
/// speaks to is one the tool checked to be on this machine, and a name that needs a lookup is not.
fn loopback_addrs(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    let bare = bare_host(host);
    if bare.eq_ignore_ascii_case("localhost") {
        return Ok(vec![(Ipv4Addr::LOCALHOST, port).into(), (Ipv6Addr::LOCALHOST, port).into()]);
    }
    match bare.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => Ok(vec![SocketAddr::new(ip, port)]),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a loopback host: {}", sanitise_target_text(host)),
        )),
    }
}

/// The time left before `deadline`, or a timeout error when none is left - so a socket is never given
/// a zero timeout, which the standard library refuses.
fn remaining(deadline: Instant) -> io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "the browser's debugging port did not answer in time"));
    }
    Ok(left)
}

/// Connect to a loopback endpoint by `deadline` (R4-N26). A plain connect has no bound of its own.
fn connect_by(host: &str, port: u16, deadline: Instant) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address to connect to");
    for addr in loopback_addrs(host, port)? {
        match TcpStream::connect_timeout(&addr, remaining(deadline)?) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// One read into `buf`, waiting at most until `deadline`, for a socket used once - the HTTP request,
/// the WebSocket handshake. The timeout is the time left, so however many reads it takes, the whole
/// exchange ends by the deadline (R4-N26). A read that times out leaves the socket indeterminate
/// (Microsoft Learn), and that is why only a socket that is then dropped may use this. Returns the
/// number of bytes read, 0 at the end of the stream.
fn read_some_by(stream: &mut TcpStream, buf: &mut Vec<u8>, deadline: Instant) -> io::Result<usize> {
    stream.set_read_timeout(Some(remaining(deadline)?))?;
    let mut tmp = [0u8; 8192];
    match stream.read(&mut tmp) {
        Ok(n) => {
            buf.extend_from_slice(&tmp[..n]);
            Ok(n)
        }
        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
            Err(io::Error::new(io::ErrorKind::TimedOut, "the browser's debugging port did not answer in time"))
        }
        Err(e) => Err(e),
    }
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
            // Control characters, and what shows as something else: the line and paragraph separators,
            // which plenty of readers break a line on (the JS engine at the other end of this very
            // protocol among them), and the format characters, which reorder or hide the rest - a
            // right-to-left override in a process name reverses what follows it (R5-B1).
            c if chrono_core::text::is_hidden_char(c) => {
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
    /// to it, all of it by `deadline` (R4-N26). This is the endpoint that carries the `Target` domain,
    /// so it can reach every page and worker in the target.
    pub fn connect_to_port(host: &str, port: u16, deadline: Instant) -> io::Result<CdpClient> {
        let version = http_get_json(host, port, "/json/version", deadline)?;
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
        let ws = WsClient::connect(&ws_host, ws_port, &ws_path, deadline)?;
        Ok(CdpClient::over_ws(ws))
    }

    /// Connect to the browser endpoint by the path the browser wrote beside its port in
    /// `DevToolsActivePort`, without asking `/json/version` first (R5-WY9) - a browser this session
    /// launched writes both lines. The path is text the target wrote and goes into the request line, so
    /// only the shape a browser writes is taken ([`is_browser_ws_path`]), and anything else asks over
    /// HTTP as before.
    pub fn connect_to_browser(host: &str, port: u16, path: Option<&str>, deadline: Instant) -> io::Result<CdpClient> {
        match path.filter(|p| is_browser_ws_path(p)) {
            Some(path) => Ok(CdpClient::over_ws(WsClient::connect(host, port, path, deadline)?)),
            None => CdpClient::connect_to_port(host, port, deadline),
        }
    }

    fn over_ws(ws: WsClient) -> CdpClient {
        CdpClient {
            ws,
            next_id: 1,
            queued: std::collections::VecDeque::new(),
            queue_overflow_warned: false,
            target_drop_warned: false,
            bad_json_warned: false,
            call_deadline: Duration::from_secs(CALL_DEADLINE_SECS),
            attaching: None,
        }
    }

    /// A client over a socket a test opened itself, without the HTTP discovery.
    #[cfg(test)]
    pub(crate) fn from_ws(ws: WsClient) -> CdpClient {
        CdpClient::over_ws(ws)
    }

    /// Shorten how long a quiet socket blocks a poll and how long a call waits for its reply. The
    /// defaults suit a loop that does nothing but drive this client. The native session loop drives
    /// the target, its children and a heartbeat beside it (docs/09 section 12.17), so it asks for
    /// budgets that keep those cadences - a renderer busy with its own JS does not answer
    /// `Runtime.evaluate`, and ten seconds of standing on it once a second would be the heartbeat gone.
    pub fn set_budgets(&mut self, poll: Duration, call: Duration) {
        self.ws.set_poll_interval(poll);
        self.call_deadline = call;
    }

    /// How long a call may wait for its reply, from now - for a caller that runs several calls under
    /// one deadline.
    pub fn call_budget(&self) -> Duration {
        self.call_deadline
    }

    /// Say which target the caller is putting the shim into, and in which session, until it calls this
    /// again with `None`. Meanwhile another session of that target is let go ([`CdpClient::let_go`]) as
    /// soon as it is announced: one already queued at once, and one that comes while a reply is awaited
    /// as it comes, rather than queued behind the attach.
    ///
    /// A target that two sessions hold paused on start runs nothing until both release it, a
    /// `Runtime.evaluate` included, and a service worker a page registers is announced twice: by the
    /// browser's auto-attach and by the page's own. Measured on two Chromium browsers (2026-11-09): the
    /// shim in the first session waited for the second, which sat unread in this queue, until the
    /// attach's deadline - ten seconds, then ten more for the second session, and the worker ran on the
    /// real clock. With the second session released, the shim answered in 8 ms, still before the
    /// worker's own script ran. The session the caller attaches in is never let go: a target attached by
    /// name is announced again in that same session.
    pub fn attaching(&mut self, attach: Option<(&str, &str)>) {
        self.attaching = attach.filter(|(target, _)| !target.is_empty()).map(|(t, s)| (t.to_string(), s.to_string()));
        if self.attaching.is_none() {
            return;
        }
        let (second, rest): (std::collections::VecDeque<Msg>, std::collections::VecDeque<Msg>) =
            std::mem::take(&mut self.queued).into_iter().partition(|m| self.is_second_session(m));
        self.queued = rest;
        for msg in second {
            self.let_go_announced(msg);
        }
    }

    /// Release a target the caller will not drive, and detach from it through the session that
    /// announced it - `parent`, `None` for the browser's own. Chromium knows a session by the session
    /// that made it: a detach sent to the browser for a session a page's auto-attach announced answers
    /// "No session with given id" and leaves it attached (measured 2026-11-09). Not waited for: a target
    /// that is not paused answers the same, and one already gone errors harmlessly.
    pub fn let_go(&mut self, session_id: &str, parent: Option<&str>) {
        let _ = self.send("Runtime.runIfWaitingForDebugger", serde_json::json!({}), Some(session_id));
        let _ = self.send("Target.detachFromTarget", serde_json::json!({ "sessionId": session_id }), parent);
    }

    /// Whether `msg` announces the target being attached ([`CdpClient::attaching`]) in a session other
    /// than the one it is attached in.
    fn is_second_session(&self, msg: &Msg) -> bool {
        let (Some((target, session)), Msg::Event { method, params, .. }) = (&self.attaching, msg) else {
            return false;
        };
        method == "Target.attachedToTarget"
            && params["targetInfo"]["targetId"].as_str() == Some(target.as_str())
            && params["sessionId"].as_str().is_some_and(|s| s != session.as_str())
    }

    /// Let go the session an `attachedToTarget` announced, through the session that announced it.
    fn let_go_announced(&mut self, msg: Msg) {
        if let Msg::Event { params, session_id, .. } = msg
            && let Some(sid) = params["sessionId"].as_str()
        {
            self.let_go(sid, session_id.as_deref());
        }
    }

    /// Send a command and return its id without waiting for the reply, which comes back from
    /// [`CdpClient::poll`] as a [`Msg::Response`] with that id (R4-S10). For requests to many contexts
    /// at once, whose replies are handled as they come.
    pub fn send(&mut self, method: &str, params: Value, session_id: Option<&str>) -> io::Result<u64> {
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
        Ok(id)
    }

    /// Send a command and block until its reply arrives, at most the call budget, queuing anything
    /// else seen in between. Returns the `result` object (or an error carrying the CDP
    /// `error.message`).
    pub fn call(&mut self, method: &str, params: Value, session_id: Option<&str>) -> io::Result<Value> {
        let deadline = Instant::now() + self.call_deadline;
        self.call_until(method, params, session_id, deadline)
    }

    /// The same, by a deadline the caller sets - one for a whole sequence of calls, so a sequence of
    /// five is bounded like one (R4-S10).
    pub fn call_until(&mut self, method: &str, params: Value, session_id: Option<&str>, deadline: Instant) -> io::Result<Value> {
        let id = self.send(method, params, session_id)?;
        self.reply_until(id, method, deadline)
    }

    /// Wait by `deadline` for the reply to a command already sent with [`CdpClient::send`], queuing
    /// anything else seen in between. For a few commands sent together to one session and then
    /// answered in turn: a reply that came while an earlier one was awaited is taken from the queue.
    pub fn reply_until(&mut self, id: u64, method: &str, deadline: Instant) -> io::Result<Value> {
        let queued = self.queued.iter().position(|m| matches!(m, Msg::Response { id: rid, .. } if *rid == id));
        if let Some(Msg::Response { result, error, .. }) = queued.and_then(|at| self.queued.remove(at)) {
            return match error {
                Some(e) => Err(target_error(method, &e)),
                None => Ok(result),
            };
        }
        loop {
            // Checked every pass, not only when the connection goes quiet. A target that keeps pushing
            // events - a page logging in a loop, a worker chattering - would otherwise never let the
            // deadline branch run, and a command whose reply never comes (a dead sessionId, say)
            // would block the whole session loop indefinitely: no heartbeat, no `end`, no liveness
            // check, and the GUI's 15 s watchdog calling a healthy core unresponsive.
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("CDP {method} timed out waiting for a reply"),
                ));
            }
            match self.poll_msg_for(left)? {
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
    /// deque would grow with the target's event rate. Past the cap the oldest message that is not a
    /// `Target.*` event is dropped, and a `Target.*` one only from a queue of nothing else at twice
    /// the cap (see [`push_bounded`]). Each kind of drop says so once, rather than silently. Another
    /// session of the target being attached is let go instead of parked ([`CdpClient::attaching`]).
    fn queue_event(&mut self, msg: Msg) {
        if self.is_second_session(&msg) {
            self.let_go_announced(msg);
            return;
        }
        // Written by hand rather than with `eprintln!`, which panics on a closed standard error
        // (R4-S11). This module names nothing in the crate, so it does not reach for `diag!`.
        match push_bounded(&mut self.queued, msg) {
            Dropped::Spent if !self.queue_overflow_warned => {
                self.queue_overflow_warned = true;
                let _ = writeln!(
                    io::stderr(),
                    "chrono core: CDP event queue hit {MAX_QUEUED_EVENTS} - dropping the oldest replies and events the session does not read"
                );
            }
            Dropped::Target if !self.target_drop_warned => {
                self.target_drop_warned = true;
                let _ = writeln!(
                    io::stderr(),
                    "chrono core: CDP event queue hit {} with target events alone - dropping the oldest, so a context it announced may stay paused",
                    MAX_QUEUED_EVENTS * 2
                );
            }
            _ => {}
        }
    }

    /// Poll for the next message (a queued one first), returning `None` when the poll interval elapsed
    /// with nothing ready - so an event loop can check target liveness between events. Blocks at most
    /// one poll interval on the socket.
    pub fn poll(&mut self) -> io::Result<Option<Msg>> {
        if let Some(m) = self.queued.pop_front() {
            return Ok(Some(m));
        }
        let Some(text) = self.ws.poll_text()? else {
            return Ok(None);
        };
        Ok(self.decode(&text))
    }

    /// The same, waiting at most `wait` - for a caller whose deadline is nearer than one interval.
    pub fn poll_for(&mut self, wait: Duration) -> io::Result<Option<Msg>> {
        if let Some(m) = self.queued.pop_front() {
            return Ok(Some(m));
        }
        self.poll_msg_for(wait)
    }

    /// The next message that is already here, without waiting: a queued one, or one the socket holds
    /// right now. `None` at once when there is none. For draining a burst within one turn (R4-S14).
    pub fn poll_ready(&mut self) -> io::Result<Option<Msg>> {
        if let Some(m) = self.queued.pop_front() {
            return Ok(Some(m));
        }
        match self.ws.poll_text_ready()? {
            Some(text) => Ok(self.decode(&text)),
            None => Ok(None),
        }
    }

    fn poll_msg_for(&mut self, wait: Duration) -> io::Result<Option<Msg>> {
        let Some(text) = self.ws.poll_text_for(wait)? else {
            return Ok(None);
        };
        Ok(self.decode(&text))
    }

    /// One CDP message from its text. A message that is not JSON - a lone surrogate the browser
    /// escaped, repaired first, or anything worse - is skipped with one notice for the session rather
    /// than ending the connection (R4-S15): the WebSocket framing around it is intact, so the next
    /// message reads as well as ever, and the one skipped is the only one lost.
    fn decode(&mut self, text: &str) -> Option<Msg> {
        let msg = message_from_text(text);
        if msg.is_none() && !self.bad_json_warned {
            self.bad_json_warned = true;
            // By hand rather than `eprintln!`, which panics on a closed standard error (R4-S11).
            let _ = writeln!(io::stderr(), "chrono core: skipped a CDP message that was not JSON");
        }
        msg
    }
}

/// One CDP message from its text, `None` for text that is not JSON even once its lone surrogates are
/// repaired. The reading half of [`CdpClient::decode`], apart from the client so it can be searched
/// without a socket.
fn message_from_text(text: &str) -> Option<Msg> {
    let v = serde_json::from_str::<Value>(text)
        .ok()
        .or_else(|| repair_lone_surrogates(text).and_then(|t| serde_json::from_str::<Value>(&t).ok()))?;
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
    Some(msg)
}

// A scanner rule for insecure WebSocket endpoints matches the unencrypted scheme wherever it is
// written, including in prose about it, which is why this paragraph does not spell it out. There is
// no `wss://` to move to: the DevTools Protocol speaks the plain scheme on the loopback port we
// asked it to open and offers no transport security there at all. The risk the rule gestures at is
// real and is answered a few lines down instead, by `ws_endpoint_is_ours`, which pins the endpoint
// to the exact port we opened and to a loopback name.
// nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket
/// Split a `ws://host:port/path` URL into its parts. CDP only ever hands us plain `ws://` loopback
/// URLs, so `wss://` and userinfo are out of scope.
fn parse_ws_url(url: &str) -> io::Result<(String, u16, String)> {
    let rest = url
        // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket
        .strip_prefix("ws://")
        // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket
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

/// Whether a path read from `DevToolsActivePort` has the shape a browser writes there,
/// `/devtools/browser/<id>` with an id of letters, digits and hyphens (a UUID on every engine measured,
/// Electron 44 among them). The path goes into the request line of the upgrade, so a space, a line
/// break or anything else the shape does not have is refused before it gets there.
fn is_browser_ws_path(path: &str) -> bool {
    path.strip_prefix("/devtools/browser/")
        .is_some_and(|id| !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

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
/// not worth a regression: measured 2026-09-05 on two Chromium engines six years apart, major 83 and
/// major 152 - both answer with the exact host and port we asked on (`127.0.0.1`), but a build that
/// said `localhost` would be equally legitimate and equally harmless.
fn ws_endpoint_is_ours(ws_host: &str, ws_port: u16, our_port: u16) -> bool {
    ws_port == our_port && LOOPBACK_HOSTS.iter().any(|h| h.eq_ignore_ascii_case(ws_host))
}

/// A loopback port nobody is listening on right now, for an engine that has to be TOLD its debug
/// port (Qt WebEngine reads one from its environment and opens nothing for a zero). Bound to
/// 127.0.0.1 on port zero and released at once, so the number is the system's pick, not a guess. A
/// process that binds it in the gap before the engine does is possible and detectable: the
/// listener that then appears on the port belongs to a pid outside the family, and the discovery
/// says so instead of speaking to it.
pub fn free_loopback_port() -> io::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/// Cap on the HTTP header block. A peer that never sends the blank line must not be read without
/// bound (R2-N9) - the peer is our own Chromium, so this is defence in depth, not a hole, but "the
/// peer is trustworthy" is exactly the assumption the body cap declines to make too.
const MAX_HTTP_HEADERS: usize = 64 * 1024;

/// Cap on the body (P3, pre-release audit): a hostile or broken peer's Content-Length must not drive a
/// huge allocation, and a bodiless response must not read without bound. The DevTools JSON we fetch
/// (the target list, the WS URL) is tiny, so this ceiling is generous.
const MAX_HTTP_BODY: usize = 16 * 1024 * 1024;

/// A tiny blocking HTTP/1.1 GET that returns the JSON body, all of it by `deadline` (R4-N26). Only
/// for the loopback CDP HTTP endpoints (`/json/version`, `/json`). Reads the body by
/// `Content-Length` - the DevTools HTTP server keeps the connection alive despite `Connection:
/// close`, so reading to EOF would block until the deadline. The deadline bounds the whole exchange,
/// not each read: a peer sending a byte at a time used to keep it going ten seconds per byte.
pub fn http_get_json(host: &str, port: u16, path: &str, deadline: Instant) -> io::Result<Value> {
    let mut stream = connect_by(host, port, deadline)?;
    let host = header_host(host);
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.set_write_timeout(Some(remaining(deadline)?))?;
    stream.write_all(request.as_bytes())?;
    stream.flush()?;

    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > MAX_HTTP_HEADERS {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "HTTP headers exceed the cap"));
        }
        if read_some_by(&mut stream, &mut buf, deadline)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "HTTP response ended before its headers did"));
        }
    };
    let content_length = content_length(&buf[..head_end]);
    let body_end = match content_length {
        Some(len) if len > MAX_HTTP_BODY => {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "HTTP Content-Length exceeds the cap"))
        }
        Some(len) => head_end + len,
        None => head_end + MAX_HTTP_BODY,
    };
    while buf.len() < body_end {
        if read_some_by(&mut stream, &mut buf, deadline)? == 0 {
            if content_length.is_some() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "HTTP body ended before its length"));
            }
            break;
        }
    }
    let body = &buf[head_end..buf.len().min(body_end)];
    serde_json::from_slice(body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("HTTP body was not JSON: {e}")))
}

/// The `Content-Length` a header block declares, if it declares one that is a number.
fn content_length(head: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(head).lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        lower.strip_prefix("content-length:").and_then(|rest| rest.trim().parse().ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R5-WY9: the path a browser wrote beside its port goes into the request line of the upgrade, so
    /// only the shape a browser writes is taken - anything else asks `/json/version` as before.
    #[test]
    fn only_a_browser_endpoint_path_is_taken_from_the_port_file() {
        assert!(is_browser_ws_path("/devtools/browser/1d5720f5-2425-4888-afd2-8e372ab3e408"));
        for bad in [
            "/devtools/browser/",
            "/devtools/page/1d5720f5",
            "/devtools/browser/abc HTTP/1.1\r\nX-Evil: 1",
            "/devtools/browser/../json",
            "devtools/browser/abc",
            "",
        ] {
            assert!(!is_browser_ws_path(bad), "{bad:?}");
        }
        assert!(!is_browser_ws_path(&format!("/devtools/browser/{}", "a".repeat(65))), "too long");
    }

    #[test]
    fn target_text_cannot_add_a_line() {
        assert_eq!(sanitise_target_text("one\ntwo"), "one\\ntwo");
        assert_eq!(sanitise_target_text("carriage\rreturn"), "carriage\\rreturn");
        assert_eq!(sanitise_target_text("tab\there"), "tab\\there");
        // An escape sequence would otherwise let a target repaint the terminal it is reported in.
        assert_eq!(sanitise_target_text("esc\u{1b}[31m"), "esc\\u{001b}[31m");
        assert_eq!(sanitise_target_text("sep\u{2028}arator"), "sep\\u{2028}arator");
        // A format character shows too, so it cannot reverse or hide what follows it (R5-B1).
        let reversed: String = ['a', char::from_u32(0x202E).unwrap(), 'b'].iter().collect();
        assert_eq!(sanitise_target_text(&reversed), "a\\u{202e}b");
        let zero_width: String = ['a', char::from_u32(0x200B).unwrap(), 'b'].iter().collect();
        assert_eq!(sanitise_target_text(&zero_width), "a\\u{200b}b");
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

    /// The cap counts what the target said, not what the quote prints: a hidden character just under
    /// it is one of the two hundred however long its escape, and one just past it goes with the rest.
    #[test]
    fn a_hidden_character_at_the_cap_counts_once() {
        let mut said = "a".repeat(198);
        said.push(char::from_u32(0x202E).unwrap());
        said.push_str("bcdef");
        assert_eq!(sanitise_target_text(&said), format!("{}\\u{{202e}}b (truncated)", "a".repeat(198)));

        let mut past = "a".repeat(200);
        past.push('\n');
        assert_eq!(sanitise_target_text(&past), format!("{} (truncated)", "a".repeat(200)));
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

    /// What Chromium majors 83 and 152 actually answer (measured), plus the loopback spellings a
    /// different build could legitimately use. Refusing these would be a regression, not a fix.
    #[test]
    fn the_endpoint_we_opened_is_accepted_however_it_spells_loopback() {
        for host in ["127.0.0.1", "localhost", "LocalHost", "::1", "[::1]"] {
            assert!(ws_endpoint_is_ours(host, 9333, 9333), "host {host} was refused");
        }
    }

    /// The premise of R4-S15, measured rather than assumed: serde_json refuses a lone surrogate escape,
    /// which Chromium writes for a JS string cut inside an emoji.
    #[test]
    fn a_lone_surrogate_is_refused_by_the_parser_and_repaired_by_us() {
        let raw = r#"{"method":"Target.attachedToTarget","params":{"targetInfo":{"title":"cut \uD83D here"}}}"#;
        assert!(serde_json::from_str::<Value>(raw).is_err(), "serde_json took a lone surrogate");
        let repaired = repair_lone_surrogates(raw).expect("something to repair");
        let v: Value = serde_json::from_str(&repaired).expect("the repaired message parses");
        assert_eq!(v["params"]["targetInfo"]["title"], "cut \u{fffd} here");
        // A lone LOW surrogate is repaired the same way.
        assert!(repair_lone_surrogates(r#"{"t":"\uDE00"}"#).is_some());
    }

    /// What must stay exactly as it is: a proper pair, other escapes, and an escaped backslash before
    /// a literal `u` - that is the text `\uD800`, not an escape of it.
    #[test]
    fn a_proper_pair_and_an_escaped_backslash_are_left_alone() {
        // Built at run time: an editor that writes a literal escape of four hex digits may turn it into
        // the character itself, and a test of escapes would then test nothing.
        let esc = |hex: &str| format!("\\u{hex}");
        let json = |inner: String| format!("{{\"t\":\"{inner}\"}}");
        let pair = format!("{}{}", esc("D83D"), esc("DE00"));
        assert_eq!(repair_lone_surrogates(&json(pair.clone())), None);
        assert_eq!(repair_lone_surrogates(&json(format!("a\\\"b\\\\c\\n{}", esc("0041")))), None);
        assert_eq!(repair_lone_surrogates(&json(format!("\\\\{}", &esc("D800")[1..]))), None);
        assert_eq!(repair_lone_surrogates("plain"), None);
        // Mixed: a good pair kept, a lone one beside it replaced.
        let mixed = repair_lone_surrogates(&json(format!("{pair}{}", esc("D83D")))).unwrap();
        assert_eq!(mixed, json(format!("{pair}{}", esc("FFFD"))));
    }

    /// R4-N24: an IPv6 loopback is connected to bare and written into the `Host` header in brackets.
    #[test]
    fn an_ipv6_host_is_bare_to_connect_and_bracketed_in_the_header() {
        assert_eq!(bare_host("[::1]"), "::1");
        assert_eq!(bare_host("::1"), "::1");
        assert_eq!(bare_host("127.0.0.1"), "127.0.0.1");
        assert_eq!(header_host("::1"), "[::1]");
        assert_eq!(header_host("[::1]"), "[::1]");
        assert_eq!(header_host("localhost"), "localhost");
    }

    fn target_event(n: u64) -> Msg {
        Msg::Event { method: "Target.attachedToTarget".into(), params: Value::from(n), session_id: None }
    }

    /// R4-S14: a full queue never drops a `Target.*` event while anything else is there - the oldest
    /// message that is not one goes, a stale reply or an event - so a target paused on start keeps
    /// its way out.
    #[test]
    fn a_full_queue_keeps_its_target_events() {
        let mut q = std::collections::VecDeque::new();
        q.push_back(target_event(0));
        q.push_back(Msg::Response { id: 7, result: Value::Null, error: None });
        for n in 0..(MAX_QUEUED_EVENTS as u64 - 2) {
            q.push_back(event(n));
        }
        assert_eq!(push_bounded(&mut q, event(1_000_000)), Dropped::Spent);
        assert!(matches!(q.front(), Some(Msg::Event { method, .. }) if method == "Target.attachedToTarget"), "the target event went");
        assert!(!q.iter().any(|m| matches!(m, Msg::Response { .. })), "the stale reply is the one dropped");
        assert_eq!(push_bounded(&mut q, event(1_000_001)), Dropped::Spent);
        assert!(matches!(q.front(), Some(Msg::Event { method, .. }) if method == "Target.attachedToTarget"));
        assert_eq!(method_of(&q[1]), "E1", "then the oldest ordinary event");
    }

    /// A queue of nothing but target events keeps them up to twice the cap and reports nothing
    /// dropped while nothing is - it used to report a drop from the first push past the cap, which
    /// spent the one notice before the drop that matters. Past twice the cap the oldest goes, and that
    /// is reported as a target event dropped (CodeRabbit on #83).
    #[test]
    fn a_queue_of_target_events_reports_only_the_drop_that_happened() {
        let mut only = std::collections::VecDeque::new();
        for n in 0..(MAX_QUEUED_EVENTS as u64 * 2) {
            assert_eq!(push_bounded(&mut only, target_event(n)), Dropped::Nothing, "push {n}");
        }
        assert_eq!(only.len(), MAX_QUEUED_EVENTS * 2);
        assert_eq!(push_bounded(&mut only, target_event(u64::MAX)), Dropped::Target);
        assert_eq!(only.len(), MAX_QUEUED_EVENTS * 2, "the bound still holds");
        assert_eq!(only.front().map(|m| matches!(m, Msg::Event { params, .. } if params == &Value::from(1u64))), Some(true), "the oldest went");
    }

    /// R4-S15 end to end over a socket: a message that is not JSON is skipped, and the connection
    /// carries on with the next one - it used to end the session.
    #[test]
    fn a_message_that_is_not_json_is_skipped_and_the_next_one_still_arrives() {
        let frame = |text: &str| {
            let mut f = vec![0x81u8, text.len() as u8];
            f.extend_from_slice(text.as_bytes());
            f
        };
        let frames = vec![
            frame(r#"{"method":"Target.attachedTo"#),
            frame(r#"{"method":"Target.attachedToTarget","params":{"t":"\uD83D"}}"#),
        ];
        let (port, server) = ws::tests::burst_server(frames, Duration::from_millis(1500));
        let ws = WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut client = CdpClient::from_ws(ws);
        let mut got = Vec::new();
        for _ in 0..4 {
            match client.poll().expect("the connection holds") {
                Some(Msg::Event { method, params, .. }) => got.push((method, params)),
                Some(Msg::Response { .. }) => panic!("no reply was sent"),
                None => {}
            }
            if !got.is_empty() {
                break;
            }
        }
        assert_eq!(got.len(), 1, "the repaired message arrived after the broken one");
        assert_eq!(got[0].0, "Target.attachedToTarget");
        assert_eq!(got[0].1["t"], "\u{fffd}");
        server.join().unwrap();
    }

    /// R4-S10: a command sent without waiting comes back from `poll` as a reply with its id, for
    /// whoever asked - and a call made meanwhile still gets its own reply, with the other one kept.
    #[test]
    fn a_reply_to_a_command_nobody_waited_on_comes_back_by_its_id() {
        let frame = |text: &str| {
            let mut f = vec![0x81u8, text.len() as u8];
            f.extend_from_slice(text.as_bytes());
            f
        };
        // The reply nobody waits on comes first, while the call is waiting for its own - so the call
        // has to keep it for later rather than drop it.
        let frames = vec![frame(r#"{"id":1,"result":{"value":"sent"}}"#), frame(r#"{"id":2,"result":{"value":"call"}}"#)];
        let (port, server) = ws::tests::burst_server(frames, Duration::from_millis(1500));
        let ws = WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut client = CdpClient::from_ws(ws);
        assert_eq!(client.send("Runtime.evaluate", Value::Null, Some("S")).unwrap(), 1);
        let call = client.call("Runtime.evaluate", Value::Null, Some("S")).unwrap();
        assert_eq!(call["value"], "call");
        match client.poll().unwrap() {
            Some(Msg::Response { id: 1, result, error: None }) => assert_eq!(result["value"], "sent"),
            _ => panic!("the reply to the command sent first was not kept"),
        }
        server.join().unwrap();
    }

    /// R4-N26: a peer that answers a byte at a time cannot keep a request past its deadline - the
    /// deadline bounds the whole exchange. Each read used to get ten seconds of its own.
    #[test]
    fn a_peer_answering_a_byte_at_a_time_cannot_keep_a_request_past_its_deadline() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut b = [0u8; 1024];
            let _ = s.read(&mut b);
            for byte in b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}" {
                if s.write_all(&[*byte]).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let started = Instant::now();
        let err = http_get_json("127.0.0.1", port, "/json/version", Instant::now() + Duration::from_millis(500)).err();
        assert_eq!(err.map(|e| e.kind()), Some(io::ErrorKind::TimedOut));
        assert!(started.elapsed() < Duration::from_millis(900), "{:?}", started.elapsed());
        server.join().unwrap();
    }

    /// The same exchange at the speed a DevTools server answers is read whole, by its length.
    #[test]
    fn a_whole_answer_is_read_by_its_length() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut b = [0u8; 1024];
            let _ = s.read(&mut b);
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\ncontent-length: 13\r\n\r\n{\"Browser\":1}").unwrap();
            // Kept open, as the DevTools server keeps it: the length is what ends the read.
            std::thread::sleep(Duration::from_millis(800));
        });
        let v = http_get_json("127.0.0.1", port, "/json/version", Instant::now() + Duration::from_secs(5)).unwrap();
        assert_eq!(v["Browser"], 1);
        server.join().unwrap();
    }

    /// Every endpoint this client connects to is on this machine: an address written out must be a
    /// loopback one, and no name is looked up - `localhost` is both loopback addresses.
    #[test]
    fn only_a_loopback_host_is_connected_to_and_no_name_is_looked_up() {
        assert_eq!(loopback_addrs("127.0.0.1", 9).unwrap(), vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 9))]);
        assert_eq!(loopback_addrs("[::1]", 9).unwrap(), vec![SocketAddr::from((Ipv6Addr::LOCALHOST, 9))]);
        assert_eq!(loopback_addrs("LocalHost", 9).unwrap().len(), 2);
        for host in ["10.0.0.5", "169.254.169.254", "evil.example.com", "127.0.0.1.evil.com", ""] {
            assert!(loopback_addrs(host, 9).is_err(), "{host} was accepted");
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
            assert_eq!(push_bounded(&mut q, event(n)), Dropped::Nothing, "nothing is dropped below the cap");
        }
        assert_eq!(q.len(), MAX_QUEUED_EVENTS);
        assert_eq!(method_of(q.front().unwrap()), "E0");

        assert_eq!(push_bounded(&mut q, event(9_999_999)), Dropped::Spent, "past the cap a drop is reported");
        assert_eq!(q.len(), MAX_QUEUED_EVENTS, "the queue does not grow past the cap");
        assert_eq!(method_of(q.front().unwrap()), "E1", "the oldest went, not the newest");
        assert_eq!(method_of(q.back().unwrap()), "E9999999");
    }

    mod properties {
        use super::*;
        use chrono_core::text::is_hidden_char;
        use proptest::collection::vec;
        use proptest::prelude::*;

        /// One piece of a JSON string as a browser writes it.
        #[derive(Debug, Clone)]
        enum Piece {
            /// The escape of one UTF-16 unit, its hex digits in upper case or in lower.
            Unit(u16, bool),
            /// A character written as itself.
            Char(char),
            /// A short escape: a backslash, a quote, a slash, a line feed or a tab.
            Short(char),
            /// An escaped backslash, then the letter and four hex digits of a unit escape - text that
            /// looks like an escape and is not one.
            NotAnEscape(u16),
        }

        impl Piece {
            fn written(&self) -> String {
                match self {
                    Piece::Unit(u, true) => format!("\\u{u:04X}"),
                    Piece::Unit(u, false) => format!("\\u{u:04x}"),
                    Piece::Char(c) => c.to_string(),
                    Piece::Short('\n') => "\\n".to_string(),
                    Piece::Short('\t') => "\\t".to_string(),
                    Piece::Short(c) => format!("\\{c}"),
                    Piece::NotAnEscape(u) => format!("\\\\u{u:04X}"),
                }
            }

            /// The UTF-16 units the piece stands for.
            fn units(&self, out: &mut Vec<u16>) {
                match self {
                    Piece::Unit(u, _) => out.push(*u),
                    Piece::Char(c) | Piece::Short(c) => out.extend(c.encode_utf16(&mut [0; 2]).iter()),
                    Piece::NotAnEscape(u) => out.extend(format!("\\u{u:04X}").encode_utf16()),
                }
            }
        }

        /// The pieces of one string, with lone surrogates of both halves, proper pairs, and the
        /// escaped backslash before a `u` drawn often - a uniform unit is a surrogate one time in
        /// thirty-two, and the shapes a scan of escapes gets wrong sit side by side.
        fn pieces() -> impl Strategy<Value = Vec<Piece>> {
            let upper = any::<bool>;
            let plain = any::<char>().prop_map(|c| if c == '"' || c == '\\' || c < ' ' { 'x' } else { c });
            let piece = prop_oneof![
                3 => (0xD800u16..=0xDBFF, upper()).prop_map(|(u, up)| vec![Piece::Unit(u, up)]),
                3 => (0xDC00u16..=0xDFFF, upper()).prop_map(|(u, up)| vec![Piece::Unit(u, up)]),
                3 => (0xD800u16..=0xDBFF, 0xDC00u16..=0xDFFF, upper())
                    .prop_map(|(high, low, up)| vec![Piece::Unit(high, up), Piece::Unit(low, up)]),
                2 => (any::<u16>(), upper()).prop_map(|(u, up)| vec![Piece::Unit(u, up)]),
                4 => plain.prop_map(|c| vec![Piece::Char(c)]),
                2 => prop_oneof![Just('\\'), Just('"'), Just('/'), Just('\n'), Just('\t')].prop_map(|c| vec![Piece::Short(c)]),
                2 => prop_oneof![0xD800u16..=0xDFFF, any::<u16>()].prop_map(|u| vec![Piece::NotAnEscape(u)]),
            ];
            vec(piece, 0..10).prop_map(|groups| groups.into_iter().flatten().collect())
        }

        /// A message of either kind, carrying the drawn string where a browser puts a title.
        fn message() -> impl Strategy<Value = (bool, Vec<Piece>)> {
            (any::<bool>(), pieces())
        }

        // The scheme a target writes for the endpoint we opened. The scanner rule on insecure WebSocket
        // endpoints matches it wherever it is written, test input included, and the note above
        // `parse_ws_url` says why there is no encrypted scheme to move to. Named once, so the one
        // exemption covers every use.
        // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket
        const PLAIN_SCHEME: &str = "ws://";

        /// A URL as a target may advertise its debugger, and whether it is one a browser writes for
        /// the endpoint we opened: our port or another, loopback in each spelling or a host somewhere
        /// else, and the shapes a parser by hand gets wrong - userinfo, a second port, a scheme in
        /// the wrong case, a port with a sign or a leading zero.
        fn advertised(ours: u16) -> impl Strategy<Value = (String, bool)> {
            let scheme = prop_oneof![6 => Just(PLAIN_SCHEME), 1 => Just("wss://"), 1 => Just("WS://"), 1 => Just("http://"), 1 => Just("")];
            let host = prop_oneof![
                Just("127.0.0.1"),
                Just("localhost"),
                Just("LocalHost"),
                Just("::1"),
                Just("[::1]"),
                Just("127.0.0.2"),
                Just("0.0.0.0"),
                Just("10.0.0.1"),
                Just("example.com"),
                Just("127.0.0.1.example.com"),
                Just("localhost."),
                Just("[::ffff:127.0.0.1]"),
                Just(""),
                Just("127.0.0.1:1@example.com"),
                Just("user@127.0.0.1"),
            ];
            let port = prop_oneof![
                4 => Just(ours.to_string()),
                1 => Just(ours.wrapping_add(1).to_string()),
                1 => Just(format!("+{ours}")),
                1 => Just(format!("0{ours}")),
                1 => Just("65536".to_string()),
                1 => Just(String::new()),
                1 => any::<u16>().prop_map(|p| p.to_string()),
            ];
            let path = prop_oneof![Just("/devtools/browser/1d57-20f5"), Just("/"), Just(""), Just("?x"), Just("/a b"), Just("@example.com:80/")];
            let built = (scheme, host, port, path).prop_map(move |(scheme, host, port, path)| {
                // Written out here rather than read from LOOPBACK_HOSTS: a name gone from that list must
                // redden this, not leave with it.
                let ours_named = scheme == PLAIN_SCHEME
                    && ["127.0.0.1", "localhost", "LocalHost", "::1", "[::1]"].contains(&host)
                    && port == ours.to_string()
                    && ["/devtools/browser/1d57-20f5", "/", ""].contains(&path);
                (format!("{scheme}{host}:{port}{path}"), ours_named)
            });
            prop_oneof![8 => built, 1 => ".{0,40}".prop_map(|text| (text, false))]
        }

        /// What a target says, with what hides or breaks a line drawn by name - controls, the
        /// separators, the marks that reorder or hide the text after them - and long enough now and
        /// then to cross the cap on a quote, with every character showing or not.
        fn said() -> impl Strategy<Value = String> {
            let hidden = prop_oneof![
                Just('\n'),
                Just('\r'),
                Just('\t'),
                Just('\0'),
                Just('\u{1b}'),
                Just('\u{85}'),
                Just('\u{61c}'),
                Just('\u{200b}'),
                Just('\u{200e}'),
                Just('\u{202e}'),
                Just('\u{2028}'),
                Just('\u{2029}'),
                Just('\u{2066}'),
                Just('\u{feff}'),
                Just('\u{e0001}'),
            ];
            // Mapped, not filtered: proptest draws control characters often, and a filter that drops
            // them ran out of draws at 20 000 cases (measured 2026-10-19, "Too many local rejects").
            let shown = || prop_oneof![3 => proptest::char::range(' ', '~'), 1 => any::<char>().prop_map(|c| if is_hidden_char(c) { 'x' } else { c })];
            let any_piece = prop_oneof![5 => shown(), 1 => hidden];
            prop_oneof![
                3 => vec(any_piece.clone(), 0..24),
                1 => vec(shown(), 196..206),
                1 => vec(any_piece, 196..206),
            ]
            .prop_map(String::from_iter)
        }

        proptest! {
            /// R4-S15. Any string a browser escapes reads as a UTF-16 decoder reads it: a proper pair as
            /// its character, a lone surrogate of either half as the replacement character, and text
            /// that only looks like an escape as itself. Refusing the message ended the session, and
            /// the repair is done only when there is something to repair.
            #[test]
            fn any_string_the_browser_escapes_reads_as_a_utf16_decoder_reads_it((reply, pieces) in message()) {
                let written: String = pieces.iter().map(Piece::written).collect();
                let mut units = Vec::new();
                for piece in &pieces {
                    piece.units(&mut units);
                }
                let text = if reply {
                    format!(r#"{{"id":7,"result":{{"title":"{written}"}}}}"#)
                } else {
                    format!(r#"{{"method":"Target.targetInfoChanged","params":{{"title":"{written}"}},"sessionId":"S"}}"#)
                };
                let title = match message_from_text(&text) {
                    Some(Msg::Response { id: 7, result, error: None }) if reply => result["title"].clone(),
                    Some(Msg::Event { method, params, session_id: Some(s) }) if !reply && method == "Target.targetInfoChanged" && s == "S" => {
                        params["title"].clone()
                    }
                    other => {
                        let read = match other {
                            None => "nothing".to_string(),
                            Some(Msg::Response { id, result, error }) => format!("reply {id} {result} {error:?}"),
                            Some(Msg::Event { method, params, session_id }) => format!("event {method} {params} {session_id:?}"),
                        };
                        return Err(TestCaseError::fail(format!("{text:?} read as {read}")));
                    }
                };
                let decoded = String::from_utf16_lossy(&units);
                prop_assert_eq!(title.as_str(), Some(decoded.as_str()), "for {:?}", text);
                prop_assert_eq!(repair_lone_surrogates(&text).is_some(), String::from_utf16(&units).is_err(), "for {:?}", text);
            }

            /// `webSocketDebuggerUrl` is text the target wrote, and the client connects where it says.
            /// Whatever it says, the client goes only to this machine and only to the port it opened -
            /// the check once looked at the scheme and whether the port parsed, and an IPv6 loopback in
            /// brackets once went to a resolver (R4-N24). And the endpoint a browser does write for
            /// that port, in any spelling of loopback, is taken: refusing it would end every session.
            #[test]
            fn whatever_endpoint_the_target_names_the_client_goes_only_to_this_machine_at_its_own_port(
                (ours, (url, ours_named)) in (1u16..=u16::MAX).prop_flat_map(|ours| (Just(ours), advertised(ours)))
            ) {
                let taken = parse_ws_url(&url).ok().filter(|(host, port, _)| ws_endpoint_is_ours(host, *port, ours));
                prop_assert!(taken.is_some() || !ours_named, "{:?} was refused for port {}", url, ours);
                if let Some((host, port, _)) = taken {
                    let addrs = loopback_addrs(&host, port);
                    let here = addrs.as_ref().is_ok_and(|a| !a.is_empty() && a.iter().all(|a| a.ip().is_loopback() && a.port() == ours));
                    prop_assert!(here, "{:?} goes to {:?}, and the port opened was {}", url, addrs, ours);
                }
            }

            /// Text a target supplies reaches stderr and the session report, which is evidence (rule 4):
            /// quoted, it never holds a character that breaks the line, reorders what follows or hides -
            /// a right-to-left mark in a process name reversed the report (R5-B1). Text that shows is
            /// kept exactly as it is, cut at two hundred characters with a mark saying so.
            #[test]
            fn what_a_target_says_is_quoted_on_one_line_and_hides_nothing(said in said()) {
                let quoted = sanitise_target_text(&said);
                prop_assert!(!quoted.chars().any(is_hidden_char), "{:?} was quoted as {:?}", said, quoted);
                if said.chars().take(200).all(|c| !is_hidden_char(c)) {
                    let kept: String = said.chars().take(200).collect();
                    let want = if said.chars().count() > 200 { format!("{kept} (truncated)") } else { kept };
                    prop_assert_eq!(quoted, want);
                }
            }
        }
    }
}
