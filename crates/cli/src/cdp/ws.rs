//! A minimal RFC 6455 WebSocket CLIENT, just enough to speak the Chrome DevTools Protocol to a
//! local Chromium/Electron target over `ws://127.0.0.1:<port>/...`. Deliberately dependency-free
//! (F3): the CDP driver is a substitution mechanism on par with the native hook, and the product
//! ships no async runtime or WebSocket crate. Scope is narrow on purpose - one connection, text
//! frames, client-side masking, and the control frames a browser actually sends (ping/close).
//!
//! The receive side is a thread of its own (R4-N27). It reads with no timeout at all and hands each
//! complete text message to the owner over a bounded channel, and the owner waits on that channel -
//! never on the socket. A read that times out under `SO_RCVTIMEO` leaves the connection "in an
//! indeterminate state" that "should be closed" (Microsoft Learn, SOL_SOCKET socket options), and
//! this client used to time one out on purpose as its tick: every 500 ms in a Chromium session,
//! every 10 ms beside a native one. Measured on loopback with a 1 ms tick: 39 timed-out reads in ten
//! minutes ended in `os error 997` on the next read and lost the bytes the cancelled read had taken.
//!
//! The thread ends when the connection does. Nothing ends it sooner: measured, neither
//! `shutdown(Both)` nor closing our own handle wakes a read waiting on a cloned handle while the
//! peer stays silent - so dropping the client sends a WebSocket close frame and a FIN, and the
//! browser closes its side. A browser that never does keeps the thread until the core exits, which
//! is when the session ends anyway.
//!
//! What it does NOT do (unneeded for localhost CDP): TLS, permessage-deflate, the server-Accept
//! check (a client MAY skip it - RFC 6455 4.1), or a cryptographically random key (the key only
//! defeats caching proxies, irrelevant to a loopback debug port).

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a poll waits for a message when the owner has not asked for another interval.
const POLL_TIMEOUT: Duration = Duration::from_millis(500);

/// How long one write may wait for the browser to take it. A write that times out leaves the
/// connection as indeterminate as a read does, so it ends the connection - the browser stopped
/// reading it, which only a hung browser does.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Messages the reader thread holds for an owner that is busy. Past this it stops reading, and the
/// browser holds what it has not sent - TCP flow control, rather than memory growing on our side.
const INBOX: usize = 1024;

/// What the reader thread hands over: a message, or the end of the connection and why.
enum Incoming {
    Text(String),
    Closed(io::Error),
}

/// The write half: the owner sends commands, the reader thread answers pings, and the lock keeps
/// their frames from interleaving.
struct Writer {
    stream: TcpStream,
    mask_counter: u32,
}

impl Writer {
    /// One masked frame (FIN set). The mask varies frame by frame, which is all RFC 6455 asks of a
    /// client.
    fn frame(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len < 65536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        self.mask_counter = self.mask_counter.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let mask = self.mask_counter.to_be_bytes();
        frame.extend_from_slice(&mask);
        for (i, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[i & 3]);
        }
        self.stream.write_all(&frame)?;
        self.stream.flush()
    }
}

/// A framed text-message WebSocket over one TCP connection to a loopback CDP endpoint.
pub struct WsClient {
    writer: Arc<Mutex<Writer>>,
    inbox: Receiver<Incoming>,
    poll: Duration,
    /// Set once the connection is known to be over - the reader said so, or a write failed - so every
    /// later poll and send says so too, instead of waiting on a connection nobody reads.
    over: bool,
}

impl WsClient {
    /// Connect to `host:port` and perform the HTTP upgrade handshake for `path` (the
    /// `webSocketDebuggerUrl` path from `/json/version`), all of it by `deadline` (R4-N26). Fails
    /// loudly if the server does not answer `101 Switching Protocols`.
    ///
    /// The handshake reads with a timeout - the time left to the deadline - which is allowed because a
    /// timeout ends this socket: it is dropped and never read again. Once the upgrade is through, the
    /// socket has no read timeout for the rest of its life.
    pub fn connect(host: &str, port: u16, path: &str, deadline: Instant) -> io::Result<WsClient> {
        let mut stream = super::connect_by(host, port, deadline)?;
        stream.set_nodelay(true).ok();
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;

        let host = super::header_host(host);
        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host}:{port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(request.as_bytes())?;
        stream.flush()?;

        // Read until the header terminator, then verify the 101 status and drop the headers.
        let mut rbuf = Vec::new();
        let end = loop {
            if let Some(pos) = find_subslice(&rbuf, b"\r\n\r\n") {
                break pos;
            }
            if rbuf.len() > MAX_HANDSHAKE_BYTES {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "websocket handshake exceeds the size cap"));
            }
            if super::read_some_by(&mut stream, &mut rbuf, deadline)? == 0 {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "closed during handshake"));
            }
        };
        let status_end = find_subslice(&rbuf, b"\r\n").unwrap_or(end);
        let status = String::from_utf8_lossy(&rbuf[..status_end]).into_owned();
        if !status.contains(" 101") {
            // The peer's own words, up to the whole header cap, so they are quoted the way every other
            // text from the target is: escaped and cut short (R5-B9, item 2).
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("websocket upgrade refused: {}", super::sanitise_target_text(&status)),
            ));
        }
        // What came with the 101 is the start of the first frame, and the reader begins from it.
        let leftover = rbuf.split_off(end + 4);

        stream.set_read_timeout(None)?;
        let reader = stream.try_clone()?;
        let writer = Arc::new(Mutex::new(Writer { stream, mask_counter: 0x9e37_79b9 }));
        let (tx, inbox) = mpsc::sync_channel(INBOX);
        let pongs = Arc::clone(&writer);
        std::thread::Builder::new()
            .name("chrono-cdp-read".into())
            .spawn(move || read_loop(reader, leftover, &tx, &pongs))?;
        Ok(WsClient { writer, inbox, poll: POLL_TIMEOUT, over: false })
    }

    /// Change how long a poll waits for a message. The default is [`POLL_TIMEOUT`], sized for a loop
    /// that does nothing else. A loop with its own cadence - the native session, polling children
    /// every 100 ms - asks for a shorter one, so a quiet connection costs it a tenth of that rather
    /// than five times it.
    pub fn set_poll_interval(&mut self, interval: Duration) {
        self.poll = interval;
    }

    /// Send one text message as a single masked frame. CDP messages are small enough that
    /// fragmenting the client side buys nothing. A write that fails or times out ends the connection.
    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        if self.over {
            return Err(closed());
        }
        let sent = match self.writer.lock() {
            Ok(mut writer) => writer.frame(0x1, text.as_bytes()),
            // The reader thread panicked while it held the lock, answering a ping: the connection
            // cannot be trusted to be in step any more.
            Err(_) => Err(io::Error::other("websocket writer was left mid-frame")),
        };
        if sent.is_err() {
            self.over = true;
        }
        sent
    }

    /// The next complete text message, waiting at most the poll interval for it. `None` when the
    /// interval passed with nothing - the caller can then do other work. An error once the
    /// connection is over, so the caller can end the session.
    pub fn poll_text(&mut self) -> io::Result<Option<String>> {
        self.poll_text_for(self.poll)
    }

    /// The same, waiting at most `wait` - for a caller with a deadline nearer than one interval.
    pub fn poll_text_for(&mut self, wait: Duration) -> io::Result<Option<String>> {
        if self.over {
            return Err(closed());
        }
        let got = self.inbox.recv_timeout(wait);
        match got {
            Ok(incoming) => self.take(incoming),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => self.take(Incoming::Closed(closed())),
        }
    }

    /// The next complete message that is already here, without waiting. `None` at once when there is
    /// none. For a loop that has just handled one message and wants the rest of a burst in the same
    /// turn (R4-S14) without adding a poll interval of waiting to every turn that has traffic.
    pub fn poll_text_ready(&mut self) -> io::Result<Option<String>> {
        if self.over {
            return Err(closed());
        }
        let got = self.inbox.try_recv();
        match got {
            Ok(incoming) => self.take(incoming),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => self.take(Incoming::Closed(closed())),
        }
    }

    fn take(&mut self, incoming: Incoming) -> io::Result<Option<String>> {
        match incoming {
            Incoming::Text(text) => Ok(Some(text)),
            Incoming::Closed(why) => {
                self.over = true;
                Err(why)
            }
        }
    }

    /// The read timeout the socket has, for the test that pins it at none once connected.
    #[cfg(test)]
    fn read_timeout(&self) -> io::Result<Option<Duration>> {
        self.writer.lock().map_err(|_| io::Error::other("poisoned"))?.stream.read_timeout()
    }
}

impl Drop for WsClient {
    /// Close the connection the way RFC 6455 does - a close frame - and send a FIN, so the browser
    /// closes its side and the reader thread, which nothing else can wake, sees the end. Best effort:
    /// a browser that has stopped reading costs the write timeout, once.
    fn drop(&mut self) {
        if let Ok(mut writer) = self.writer.lock() {
            if !self.over {
                let _ = writer.frame(0x8, &[]);
            }
            let _ = writer.stream.shutdown(Shutdown::Write);
        }
    }
}

/// The error a poll or a send gets once the connection is over.
fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "websocket closed")
}

/// Cap on the handshake response: the DevTools server answers in a few hundred bytes.
const MAX_HANDSHAKE_BYTES: usize = 64 * 1024;

/// The reader thread: read with no timeout, hand over every complete text message, answer pings,
/// and say once, at the end, why the connection ended. Stops early when the owner is gone.
fn read_loop(mut stream: TcpStream, mut rbuf: Vec<u8>, inbox: &SyncSender<Incoming>, writer: &Mutex<Writer>) {
    let mut msg = Vec::new();
    let end = loop {
        match take_message(&mut rbuf, &mut msg, writer) {
            Ok(Some(text)) => {
                if inbox.send(Incoming::Text(text)).is_err() {
                    return;
                }
                continue;
            }
            Ok(None) => {}
            Err(why) => break why,
        }
        let mut tmp = [0u8; 8192];
        match stream.read(&mut tmp) {
            Ok(0) => break closed(),
            Ok(n) => {
                rbuf.extend_from_slice(&tmp[..n]);
                if rbuf.len() > MAX_WS_BYTES {
                    // A frame claiming a huge payload would otherwise grow this buffer toward that size
                    // (P3, pre-release audit): bound it so a hostile or runaway peer cannot exhaust memory.
                    break io::Error::new(io::ErrorKind::InvalidData, "websocket frame exceeds the size cap");
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => break e,
        }
    };
    let _ = inbox.send(Incoming::Closed(end));
}

/// Assemble a complete text message from buffered frames, answering pings along the way. Returns
/// `None` when the buffer does not yet hold a full frame (the caller reads more).
fn take_message(rbuf: &mut Vec<u8>, msg: &mut Vec<u8>, writer: &Mutex<Writer>) -> io::Result<Option<String>> {
    while let Some((fin, opcode, payload)) = take_frame(rbuf)? {
        match opcode {
            0x0..=0x2 => {
                // continuation (0x0) | text (0x1) | binary (0x2) - accumulate until FIN.
                msg.extend_from_slice(&payload);
                if msg.len() > MAX_WS_BYTES {
                    // Many fragmented frames must not accumulate without bound either (P3).
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "websocket message exceeds the size cap"));
                }
                if fin {
                    let bytes = std::mem::take(msg);
                    return String::from_utf8(bytes)
                        .map(Some)
                        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "websocket text was not UTF-8"));
                }
            }
            0x9 => {
                // ping -> pong (echo). RFC 6455 caps a control payload at 125 bytes, but a peer can
                // break that, and a pong announcing a length it does not carry would put the byte
                // stream out of step from there on. Echo only what fits.
                let echo = &payload[..payload.len().min(125)];
                writer.lock().map_err(|_| io::Error::other("websocket writer was left mid-frame"))?.frame(0xA, echo)?;
            }
            0xA => {} // pong - ignore
            0x8 => return Err(closed()),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unexpected websocket opcode {other:#x}"),
                ))
            }
        }
    }
    Ok(None)
}

/// Cap on the read buffer and on an assembled message (P3, pre-release audit). CDP messages are small, so
/// anything past this is a runaway or hostile peer - bounded so a huge or endless frame cannot grow memory
/// without limit. The peer here is a Chromium instance we launched, so this is defence in depth.
const MAX_WS_BYTES: usize = 16 * 1024 * 1024;

/// Parse and remove one complete WebSocket frame from the front of `buf`, if the buffer holds one.
/// Server frames are usually unmasked, but the mask bit is honoured either way. Returns
/// `Ok(Some((fin, opcode, unmasked_payload)))`, `Ok(None)` when more bytes are needed, or `Err`
/// when the header itself is unusable - a claimed length past the cap, which cannot be answered by
/// waiting for more bytes because those bytes will never come.
fn take_frame(buf: &mut Vec<u8>) -> io::Result<Option<(bool, u8, Vec<u8>)>> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let b0 = buf[0];
    let b1 = buf[1];
    let fin = b0 & 0x80 != 0;
    let opcode = b0 & 0x0f;
    let masked = b1 & 0x80 != 0;
    let len7 = (b1 & 0x7f) as usize;

    let (payload_len, header_len): (u64, usize) = match len7 {
        126 => {
            if buf.len() < 4 {
                return Ok(None);
            }
            (u16::from_be_bytes([buf[2], buf[3]]) as u64, 4)
        }
        127 => {
            if buf.len() < 10 {
                return Ok(None);
            }
            let mut b = [0u8; 8];
            b.copy_from_slice(&buf[2..10]);
            // Kept as u64 deliberately: `as usize` would silently truncate a 64-bit claim on x86,
            // which the support matrix ships, and the check below would then pass on a bogus length.
            (u64::from_be_bytes(b), 10)
        }
        n => (n as u64, 2),
    };

    // A header may claim any length up to u64::MAX, and the addition below is not checked - release
    // builds run with overflow-checks on, so such a claim PANICKED the core mid-session, leaving the
    // launched Chromium holding a debug port and a temp profile with nobody left to clean them up.
    // The cap is the same one `fill` and `take_message` enforce, applied here where the claim first
    // appears. This cannot be answered with `Ok(None)` ("read more"): the bytes will never arrive.
    if payload_len > MAX_WS_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "websocket frame header claims a payload beyond the size cap",
        ));
    }
    let payload_len = payload_len as usize;

    let mask_len = if masked { 4 } else { 0 };
    let total = header_len + mask_len + payload_len;
    if buf.len() < total {
        return Ok(None);
    }

    let mask = if masked {
        [buf[header_len], buf[header_len + 1], buf[header_len + 2], buf[header_len + 3]]
    } else {
        [0u8; 4]
    };
    let mut payload = buf[header_len + mask_len..total].to_vec();
    if masked {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i & 3];
        }
    }
    buf.drain(..total);
    Ok(Some((fin, opcode, payload)))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build a server-style (unmasked) text frame the way a browser sends one.
    fn server_frame(payload: &[u8], fin: bool) -> Vec<u8> {
        let mut f = Vec::new();
        f.push(if fin { 0x81 } else { 0x01 });
        if payload.len() < 126 {
            f.push(payload.len() as u8);
        } else {
            f.push(126);
            f.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        f.extend_from_slice(payload);
        f
    }

    /// S-4 regression. A 127 length marker is followed by a full u64, and the frame total was added
    /// unchecked - a claim near u64::MAX overflowed usize and PANICKED the core (release builds keep
    /// overflow-checks on), which killed a live Chromium session and left its debug port and temp
    /// profile behind. Verified before the fix: "attempt to add with overflow".
    #[test]
    fn a_frame_claiming_an_impossible_length_is_refused_not_panicked() {
        // 0x82 = FIN + binary, 0xFF = mask bit + len7 127 (a u64 length follows).
        let mut buf = vec![0x82u8, 0xFF];
        buf.extend_from_slice(&u64::MAX.to_be_bytes());
        let err = take_frame(&mut buf).expect_err("an impossible length must be refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        // Just past the cap is refused too - the boundary, not only the extreme.
        let mut over = vec![0x82u8, 0x7F];
        over.extend_from_slice(&(MAX_WS_BYTES as u64 + 1).to_be_bytes());
        assert!(take_frame(&mut over).is_err());

        // Exactly at the cap is a legal claim: no error, just "not enough bytes yet".
        let mut at_cap = vec![0x82u8, 0x7F];
        at_cap.extend_from_slice(&(MAX_WS_BYTES as u64).to_be_bytes());
        assert!(take_frame(&mut at_cap).expect("a legal claim is not an error").is_none());
    }

    #[test]
    fn takes_a_short_frame() {
        let mut buf = server_frame(b"{\"id\":1}", true);
        let (fin, op, payload) = take_frame(&mut buf).unwrap().unwrap();
        assert!(fin);
        assert_eq!(op, 0x1);
        assert_eq!(payload, b"{\"id\":1}");
        assert!(buf.is_empty());
    }

    #[test]
    fn takes_a_16bit_length_frame() {
        let big = vec![b'x'; 1000];
        let mut buf = server_frame(&big, true);
        let (_, _, payload) = take_frame(&mut buf).unwrap().unwrap();
        assert_eq!(payload.len(), 1000);
    }

    #[test]
    fn returns_none_until_the_frame_is_complete() {
        let full = server_frame(b"hello", true);
        let mut partial = full[..4].to_vec(); // header + part of payload
        assert!(take_frame(&mut partial).unwrap().is_none());
        assert_eq!(partial.len(), 4); // nothing consumed
    }

    #[test]
    fn leaves_trailing_bytes_of_the_next_frame() {
        let mut buf = server_frame(b"AB", true);
        buf.extend_from_slice(&server_frame(b"CD", true));
        let (_, _, first) = take_frame(&mut buf).unwrap().unwrap();
        assert_eq!(first, b"AB");
        let (_, _, second) = take_frame(&mut buf).unwrap().unwrap();
        assert_eq!(second, b"CD");
        assert!(take_frame(&mut buf).unwrap().is_none());
    }

    /// A loopback server that reads the upgrade request and hands the connection to `then`.
    fn server(then: impl FnOnce(TcpStream) + Send + 'static) -> (u16, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut seen = Vec::new();
            let mut b = [0u8; 1024];
            while find_subslice(&seen, b"\r\n\r\n").is_none() {
                let n = s.read(&mut b).unwrap();
                if n == 0 {
                    return;
                }
                seen.extend_from_slice(&b[..n]);
            }
            then(s);
        });
        (port, handle)
    }

    const UPGRADED: &[u8] = b"HTTP/1.1 101 Switching Protocols\r\n\r\n";

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    /// A loopback server that answers the upgrade and then sends `frames` in one write, the way a burst
    /// of CDP events arrives. Kept open until `hold` passes so the client sees a live, quiet socket.
    pub(crate) fn burst_server(frames: Vec<Vec<u8>>, hold: Duration) -> (u16, std::thread::JoinHandle<()>) {
        server(move |mut s| {
            s.write_all(UPGRADED).unwrap();
            let all: Vec<u8> = frames.concat();
            s.write_all(&all).unwrap();
            std::thread::sleep(hold);
        })
    }

    /// A loopback browser: answers each CDP request `answer` has an answer for (and leaves the rest
    /// unanswered, like a busy context), and hands back every request it read, in order, once the
    /// client closes the connection.
    pub(crate) fn fake_browser(
        answer: impl Fn(&serde_json::Value) -> Option<serde_json::Value> + Send + 'static,
    ) -> (u16, std::thread::JoinHandle<Vec<serde_json::Value>>) {
        fake_browser_holding(move |request| answer(request).map(|result| vec![(request["id"].clone(), result)]).unwrap_or_default())
    }

    /// One message of the scripted browser: a reply to `id`, or - with a `null` id - an event, whose
    /// whole text is `result`.
    fn scripted_message(id: serde_json::Value, result: serde_json::Value) -> String {
        if id.is_null() {
            result.to_string()
        } else {
            serde_json::json!({ "id": id, "result": result }).to_string()
        }
    }

    /// The same, but `answer` may hold a reply back and give it with a later request: it returns the
    /// replies to write now, each an id and a result - like a renderer busy until a later command. An
    /// entry whose id is `null` is an event: its "result" is written as the whole message.
    pub(crate) fn fake_browser_holding(
        mut answer: impl FnMut(&serde_json::Value) -> Vec<(serde_json::Value, serde_json::Value)> + Send + 'static,
    ) -> (u16, std::thread::JoinHandle<Vec<serde_json::Value>>) {
        fake_browser_paced(None, move |request| request.map(&mut answer).unwrap_or_default())
    }

    /// The same, and once the client has sent nothing for `quiet`, `answer` is asked again with no
    /// request: a renderer busy with its own work comes free when it does, not when the next command
    /// arrives, so a reply held back can come after the client went on without it.
    ///
    /// The socket is read by a thread of its own with plain blocking reads, and the quiet is the wait on
    /// what that thread hands over: a receive that timed out leaves a Windows socket in an indeterminate
    /// state that should not be read again (Winsock, `SO_RCVTIMEO`), so no read here ever times out.
    pub(crate) fn fake_browser_paced(
        quiet: Option<Duration>,
        mut answer: impl FnMut(Option<&serde_json::Value>) -> Vec<(serde_json::Value, serde_json::Value)> + Send + 'static,
    ) -> (u16, std::thread::JoinHandle<Vec<serde_json::Value>>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (port, server) = server(move |mut s| {
            s.write_all(UPGRADED).unwrap();
            let (bytes_tx, bytes_rx) = std::sync::mpsc::channel::<Vec<u8>>();
            let mut reader = s.try_clone().unwrap();
            let reading = std::thread::spawn(move || {
                let mut b = [0u8; 8192];
                while let Ok(n) = reader.read(&mut b) {
                    if n == 0 || bytes_tx.send(b[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
            let mut log = Vec::new();
            let mut buf = Vec::new();
            'read: loop {
                while let Ok(Some((_, opcode, payload))) = take_frame(&mut buf) {
                    if opcode == 0x8 {
                        break 'read;
                    }
                    let Ok(request) = serde_json::from_slice::<serde_json::Value>(&payload) else {
                        continue;
                    };
                    for (id, result) in answer(Some(&request)) {
                        s.write_all(&server_frame(scripted_message(id, result).as_bytes(), true)).unwrap();
                    }
                    log.push(request);
                }
                let next = match quiet {
                    Some(quiet) => bytes_rx.recv_timeout(quiet),
                    None => bytes_rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                };
                match next {
                    Ok(bytes) => buf.extend_from_slice(&bytes),
                    Err(RecvTimeoutError::Timeout) => {
                        for (id, result) in answer(None) {
                            s.write_all(&server_frame(scripted_message(id, result).as_bytes(), true)).unwrap();
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            // A client that sent its close frame and kept the connection open would hold the reading
            // thread in its read for good.
            let _ = s.shutdown(Shutdown::Both);
            let _ = reading.join();
            let _ = tx.send(log);
        });
        let log = std::thread::spawn(move || {
            server.join().unwrap();
            rx.recv().unwrap_or_default()
        });
        (port, log)
    }

    /// A refused upgrade quotes the peer's status line escaped and cut short (R5-B9, item 2): a control
    /// sequence in it cannot reach the terminal, and a status line up to the whole header cap does not
    /// become one line of the diagnostic.
    #[test]
    fn a_refused_upgrade_quotes_the_status_line_escaped_and_short() {
        let (port, server) = server(move |mut s| {
            let mut status = b"HTTP/1.1 400 \x1b[2Kfake\x1b[8m ".to_vec();
            status.extend(std::iter::repeat_n(b'x', 4000));
            status.extend_from_slice(b"\r\n\r\n");
            s.write_all(&status).unwrap();
        });
        let err = WsClient::connect("127.0.0.1", port, "/", soon()).err().expect("refused");
        let text = err.to_string();
        assert!(!text.contains('\x1b'), "no escape character reaches the message: {text:?}");
        assert!(text.contains("\\u{001b}") && text.contains("(truncated)"), "{text}");
        server.join().unwrap();
    }

    /// R4-N27: once the upgrade is through, the socket has no read timeout. A read that times out
    /// leaves a connection indeterminate (Microsoft Learn), and a timeout used as the tick lost bytes
    /// on this machine (os error 997, `tools/probes/r4-15b`). The waiting is on the reader's channel.
    #[test]
    fn a_connected_socket_has_no_read_timeout() {
        let (port, server) = burst_server(vec![server_frame(b"{}", true)], Duration::from_millis(300));
        let ws = WsClient::connect("127.0.0.1", port, "/", soon()).unwrap();
        assert_eq!(ws.read_timeout().unwrap(), None);
        drop(ws);
        server.join().unwrap();
    }

    /// R4-N26: a message whose bytes trickle in no longer holds a poll until it is complete - the
    /// poll returns at its interval, and the message arrives whole when its last byte does.
    #[test]
    fn a_message_that_trickles_in_does_not_hold_a_poll_past_its_interval() {
        let frame = server_frame(b"{\"slow\":true}", true);
        let (port, server) = server(move |mut s| {
            s.write_all(UPGRADED).unwrap();
            for byte in frame {
                s.write_all(&[byte]).unwrap();
                std::thread::sleep(Duration::from_millis(60));
            }
            std::thread::sleep(Duration::from_millis(300));
        });
        let mut ws = WsClient::connect("127.0.0.1", port, "/", soon()).unwrap();
        ws.set_poll_interval(Duration::from_millis(100));
        let started = Instant::now();
        assert_eq!(ws.poll_text().unwrap(), None, "nothing whole yet");
        assert!(started.elapsed() < Duration::from_millis(400), "the poll waited for the message: {:?}", started.elapsed());
        let mut got = None;
        for _ in 0..40 {
            if let Some(text) = ws.poll_text().unwrap() {
                got = Some(text);
                break;
            }
        }
        assert_eq!(got.as_deref(), Some("{\"slow\":true}"));
        server.join().unwrap();
    }

    /// R4-N26: a server that takes the connection and never answers the upgrade costs the deadline,
    /// once - not ten seconds per read.
    #[test]
    fn a_handshake_that_never_answers_ends_at_its_deadline() {
        let (port, server) = server(|_s| std::thread::sleep(Duration::from_millis(1200)));
        let started = Instant::now();
        let err = WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_millis(300)).err();
        assert_eq!(err.map(|e| e.kind()), Some(io::ErrorKind::TimedOut));
        assert!(started.elapsed() < Duration::from_millis(900), "{:?}", started.elapsed());
        server.join().unwrap();
    }

    /// The bytes that came in the same write as the 101 are the start of the first message, not lost.
    #[test]
    fn the_frame_that_came_with_the_upgrade_is_the_first_message() {
        let (port, server) = server(|mut s| {
            let mut both = UPGRADED.to_vec();
            both.extend_from_slice(&server_frame(b"{\"first\":1}", true));
            s.write_all(&both).unwrap();
            std::thread::sleep(Duration::from_millis(500));
        });
        let mut ws = WsClient::connect("127.0.0.1", port, "/", soon()).unwrap();
        assert_eq!(ws.poll_text_for(Duration::from_secs(2)).unwrap().as_deref(), Some("{\"first\":1}"));
        server.join().unwrap();
    }

    /// Dropping the client closes the connection the way RFC 6455 does - a close frame - and ends our
    /// side with a FIN, which is what lets the reader thread see the end: nothing on this side can
    /// wake a read the peer keeps waiting (measured, `tools/probes/r4-15b`).
    #[test]
    fn dropping_the_client_sends_a_close_frame_and_ends_its_side() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (port, server) = server(move |mut s| {
            s.write_all(UPGRADED).unwrap();
            let mut got = Vec::new();
            let _ = s.read_to_end(&mut got);
            let _ = tx.send(got);
        });
        let ws = WsClient::connect("127.0.0.1", port, "/", soon()).unwrap();
        drop(ws);
        let got = rx.recv_timeout(Duration::from_secs(3)).expect("the server saw the end of the stream");
        assert_eq!(got.first(), Some(&0x88), "a close frame, FIN set: {got:?}");
        server.join().unwrap();
    }

    /// A browser that stops reading the socket ends the connection at the write timeout, and every
    /// later poll says it is over - a write that times out leaves the connection as indeterminate as
    /// a read does (Microsoft Learn), so it is not used again.
    #[test]
    fn a_browser_that_stops_reading_ends_the_connection() {
        let (port, server) = server(|mut s| {
            s.write_all(UPGRADED).unwrap();
            std::thread::sleep(Duration::from_secs(6));
        });
        let mut ws = WsClient::connect("127.0.0.1", port, "/", soon()).unwrap();
        let big = "x".repeat(1024 * 1024);
        let started = Instant::now();
        let mut failed = false;
        for _ in 0..256 {
            if ws.send_text(&big).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed, "256 MB went into a socket nobody reads");
        assert!(started.elapsed() < WRITE_TIMEOUT + Duration::from_secs(2), "{:?}", started.elapsed());
        assert!(ws.poll_text_for(Duration::from_millis(10)).is_err(), "the connection is over");
        assert!(ws.send_text("{}").is_err());
        drop(ws);
        server.join().unwrap();
    }

    /// R4-S14: after one message, the rest of a burst is taken in the same turn and nothing is waited
    /// for once it is gone - the drain must not add a poll interval to every busy turn.
    #[test]
    fn a_burst_is_taken_without_waiting_and_the_end_of_it_is_seen_at_once() {
        let frames: Vec<Vec<u8>> = (0..5).map(|n| server_frame(format!("{{\"n\":{n}}}").as_bytes(), true)).collect();
        let (port, server) = burst_server(frames, Duration::from_millis(1500));
        let mut ws = WsClient::connect("127.0.0.1", port, "/", soon()).unwrap();
        ws.set_poll_interval(Duration::from_millis(800));
        // The burst is in once the first message is: the reader hands them over as they come.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(ws.poll_text().unwrap().as_deref(), Some("{\"n\":0}"));
        for n in 1..5 {
            assert_eq!(ws.poll_text_ready().unwrap(), Some(format!("{{\"n\":{n}}}")), "message {n} of the burst");
        }
        let started = std::time::Instant::now();
        assert_eq!(ws.poll_text_ready().unwrap(), None, "nothing more is here");
        assert!(started.elapsed() < Duration::from_millis(200), "the end of the burst was waited for: {:?}", started.elapsed());
        // And the socket still waits its interval when asked to.
        let started = std::time::Instant::now();
        assert_eq!(ws.poll_text().unwrap(), None);
        assert!(started.elapsed() >= Duration::from_millis(500), "a plain poll stopped waiting: {:?}", started.elapsed());
        server.join().unwrap();
    }

    #[test]
    fn unmasks_a_client_style_frame() {
        // Sanity that the mask XOR is symmetric: mask a payload, then take_frame unmasks it.
        let mask = [0x12u8, 0x34, 0x56, 0x78];
        let payload = b"masked!";
        let mut buf = vec![0x81, 0x80 | payload.len() as u8];
        buf.extend_from_slice(&mask);
        for (i, b) in payload.iter().enumerate() {
            buf.push(b ^ mask[i & 3]);
        }
        let (_, _, got) = take_frame(&mut buf).unwrap().unwrap();
        assert_eq!(got, payload);
    }

    /// Random properties of the frame parser. Its bytes come from a port that any application on
    /// the machine may own, so nothing about them can be assumed.
    mod properties {
        use super::*;
        use crate::search::{edge_u64, near};
        use proptest::collection::vec;
        use proptest::prelude::*;

        /// What `take_frame` must answer for some bytes, worked out apart from it - in u128, so no
        /// claimed length can overflow the arithmetic that judges the parser.
        #[derive(Debug)]
        enum Answer {
            More,
            Refused,
            Frame { fin: bool, opcode: u8, payload: Vec<u8>, taken: usize },
        }

        fn answer(bytes: &[u8]) -> Answer {
            if bytes.len() < 2 {
                return Answer::More;
            }
            let (claim, header): (u128, usize) = match bytes[1] & 0x7f {
                126 if bytes.len() < 4 => return Answer::More,
                126 => (u128::from(u16::from_be_bytes([bytes[2], bytes[3]])), 4),
                127 if bytes.len() < 10 => return Answer::More,
                127 => {
                    let mut wide = [0u8; 8];
                    wide.copy_from_slice(&bytes[2..10]);
                    (u128::from(u64::from_be_bytes(wide)), 10)
                }
                short => (u128::from(short), 2),
            };
            if claim > MAX_WS_BYTES as u128 {
                return Answer::Refused;
            }
            let masked = bytes[1] & 0x80 != 0;
            let body = header + if masked { 4 } else { 0 };
            if (bytes.len() as u128) < body as u128 + claim {
                return Answer::More;
            }
            let taken = body + claim as usize;
            let payload = bytes[body..taken]
                .iter()
                .enumerate()
                .map(|(i, byte)| if masked { byte ^ bytes[header + (i & 3)] } else { *byte })
                .collect();
            Answer::Frame { fin: bytes[0] & 0x80 != 0, opcode: bytes[0] & 0x0f, payload, taken }
        }

        /// A claimed length: any, the edges of the 64-bit range, the cap and either side of it.
        fn claim() -> impl Strategy<Value = u64> {
            prop_oneof![edge_u64(), near(MAX_WS_BYTES as u64, 2), 0u64..=200]
        }

        /// The first bytes of a frame in each length form - seven bits, sixteen, sixty-four - with
        /// the mask bit either way.
        fn header() -> impl Strategy<Value = Vec<u8>> {
            let length = prop_oneof![
                (0u8..=125).prop_map(|n| vec![n]),
                prop_oneof![any::<u16>(), Just(u16::MAX), 0u16..=300]
                    .prop_map(|n| [vec![126u8], n.to_be_bytes().to_vec()].concat()),
                claim().prop_map(|n| [vec![127u8], n.to_be_bytes().to_vec()].concat()),
            ];
            (any::<u8>(), any::<bool>(), length).prop_map(|(first, masked, mut length)| {
                if masked {
                    length[0] |= 0x80;
                }
                [vec![first], length].concat()
            })
        }

        /// A header and whatever chance puts behind it.
        fn shaped() -> impl Strategy<Value = Vec<u8>> {
            (header(), vec(any::<u8>(), 0..300)).prop_map(|(h, tail)| [h, tail].concat())
        }

        /// What the reader may hold: a shaped frame, the same cut anywhere, or bytes with no shape
        /// at all.
        fn held() -> impl Strategy<Value = Vec<u8>> {
            prop_oneof![
                1 => vec(any::<u8>(), 0..64),
                3 => shaped(),
                1 => shaped().prop_flat_map(|bytes| {
                    let len = bytes.len();
                    (Just(bytes), 0..=len)
                })
                .prop_map(|(bytes, cut)| bytes[..cut].to_vec()),
            ]
        }

        /// One frame to send: any opcode, the mask or none, a payload that crosses each length
        /// form's edge now and then, and a length written in that form or any longer one.
        #[derive(Debug, Clone)]
        struct Sent {
            fin: bool,
            opcode: u8,
            mask: Option<[u8; 4]>,
            payload: Vec<u8>,
            form: u8,
        }

        fn sent() -> impl Strategy<Value = Sent> {
            let payload = prop_oneof![
                6 => vec(any::<u8>(), 0..=130),
                1 => (65_534usize..=65_537).prop_flat_map(|len| vec(any::<u8>(), len)),
            ];
            (any::<bool>(), 0u8..16, proptest::option::of(any::<[u8; 4]>()), payload, 0u8..3).prop_map(
                |(fin, opcode, mask, payload, form)| Sent { fin, opcode, mask, payload, form },
            )
        }

        fn encode(frame: &Sent) -> Vec<u8> {
            let len = frame.payload.len();
            let shortest = if len < 126 { 0 } else if len < 65_536 { 1 } else { 2 };
            let mask_bit = if frame.mask.is_some() { 0x80 } else { 0 };
            let mut out = vec![if frame.fin { 0x80 } else { 0 } | frame.opcode];
            match frame.form.max(shortest) {
                0 => out.push(mask_bit | len as u8),
                1 => {
                    out.push(mask_bit | 126);
                    out.extend_from_slice(&(len as u16).to_be_bytes());
                }
                _ => {
                    out.push(mask_bit | 127);
                    out.extend_from_slice(&(len as u64).to_be_bytes());
                }
            }
            match frame.mask {
                Some(mask) => {
                    out.extend_from_slice(&mask);
                    out.extend(frame.payload.iter().enumerate().map(|(i, byte)| byte ^ mask[i & 3]));
                }
                None => out.extend_from_slice(&frame.payload),
            }
            out
        }

        proptest! {
            /// S-4's class. Whatever the reader holds, the parser answers what the header says: wait
            /// for more and keep every byte, refuse a claim past the cap (the bytes will never come),
            /// or hand over exactly one frame and leave the rest. Never a panic - a claim near
            /// u64::MAX used to overflow the sum of the frame's parts and take the core down.
            #[test]
            fn any_bytes_are_one_frame_a_wait_or_a_refusal(bytes in held()) {
                let mut buf = bytes.clone();
                let got = take_frame(&mut buf);
                match (answer(&bytes), got) {
                    (Answer::More, Ok(None)) => prop_assert_eq!(&buf, &bytes),
                    (Answer::Refused, Err(e)) => {
                        prop_assert_eq!(e.kind(), io::ErrorKind::InvalidData);
                        prop_assert_eq!(&buf, &bytes);
                    }
                    (Answer::Frame { fin, opcode, payload, taken }, Ok(Some(frame))) => {
                        prop_assert_eq!(frame, (fin, opcode, payload));
                        prop_assert_eq!(&buf[..], &bytes[taken..]);
                    }
                    (want, got) => prop_assert!(false, "wanted {:?}, the parser said {:?}", want, got),
                }
            }

            /// Frames sent one after another and read in pieces of any size come back whole, in
            /// order, once each - the way the reader thread meets them, a read at a time.
            #[test]
            fn frames_read_in_any_pieces_come_back_whole_and_in_order(
                frames in vec(sent(), 1..4),
                pieces in vec(1usize..5_000, 1..12),
            ) {
                let wire: Vec<u8> = frames.iter().flat_map(encode).collect();
                let mut buf = Vec::new();
                let mut got = Vec::new();
                let mut at = 0;
                for piece in pieces.iter().cycle() {
                    if at == wire.len() {
                        break;
                    }
                    let end = (at + piece).min(wire.len());
                    buf.extend_from_slice(&wire[at..end]);
                    at = end;
                    while let Some(frame) = take_frame(&mut buf).map_err(|e| TestCaseError::fail(e.to_string()))? {
                        got.push(frame);
                    }
                }
                prop_assert!(buf.is_empty(), "{} bytes left over", buf.len());
                let want: Vec<_> = frames.into_iter().map(|f| (f.fin, f.opcode, f.payload)).collect();
                prop_assert_eq!(got, want);
            }
        }
    }
}
