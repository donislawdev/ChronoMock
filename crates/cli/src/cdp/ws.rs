//! A minimal RFC 6455 WebSocket CLIENT, just enough to speak the Chrome DevTools Protocol to a
//! local Chromium/Electron target over `ws://127.0.0.1:<port>/...`. Deliberately dependency-free
//! (F3): the CDP driver is a substitution mechanism on par with the native hook, and the product
//! ships no async runtime or WebSocket crate. Scope is narrow on purpose - one connection, text
//! frames, client-side masking, and the control frames a browser actually sends (ping/close).
//!
//! The receive side is buffer-based and pollable: bytes are accumulated and a frame is parsed only
//! once it is complete, so a read timeout between frames never corrupts a partial frame. That lets a
//! long-lived event loop wake periodically (to check whether the target is still alive) without
//! ending the session on an idle gap.
//!
//! What it does NOT do (unneeded for localhost CDP): TLS, permessage-deflate, the server-Accept
//! check (a client MAY skip it - RFC 6455 4.1), or a cryptographically random key (the key only
//! defeats caching proxies, irrelevant to a loopback debug port).

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Poll granularity for the receive socket: a blocked read wakes this often so an event loop can
/// check target liveness. Frames still assemble across as many wakeups as they need.
const POLL_TIMEOUT: Duration = Duration::from_millis(500);

/// One read attempt's outcome.
enum Fill {
    Data,
    Timeout,
    Closed,
}

/// A framed text-message WebSocket over one TCP connection to a loopback CDP endpoint.
pub struct WsClient {
    stream: TcpStream,
    writer: TcpStream,
    mask_counter: u32,
    /// Unparsed bytes read from the socket.
    rbuf: Vec<u8>,
    /// A message being reassembled across continuation frames.
    msg: Vec<u8>,
}

impl WsClient {
    /// Connect to `host:port` and perform the HTTP upgrade handshake for `path` (the
    /// `webSocketDebuggerUrl` path from `/json/version`). Fails loudly if the server does not answer
    /// `101 Switching Protocols`.
    pub fn connect(host: &str, port: u16, path: &str) -> io::Result<WsClient> {
        let stream = TcpStream::connect((host, port))?;
        stream.set_nodelay(true).ok();
        // Generous timeout for the handshake, then the fine-grained poll timeout for operation.
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let writer = stream.try_clone()?;
        let mut client = WsClient { stream, writer, mask_counter: 0x9e37_79b9, rbuf: Vec::new(), msg: Vec::new() };

        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host}:{port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        (&client.writer).write_all(request.as_bytes())?;
        (&client.writer).flush()?;

        // Read until the header terminator, then verify the 101 status and drop the headers.
        let end = loop {
            if let Some(pos) = find_subslice(&client.rbuf, b"\r\n\r\n") {
                break pos;
            }
            match client.fill()? {
                Fill::Data => {}
                Fill::Timeout => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "websocket handshake timed out"))
                }
                Fill::Closed => {
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "closed during handshake"))
                }
            }
        };
        let status_end = find_subslice(&client.rbuf, b"\r\n").unwrap_or(end);
        let status = String::from_utf8_lossy(&client.rbuf[..status_end]).into_owned();
        if !status.contains(" 101") {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("websocket upgrade refused: {status}"),
            ));
        }
        client.rbuf.drain(..end + 4);

        client.stream.set_read_timeout(Some(POLL_TIMEOUT))?;
        Ok(client)
    }

    /// Send one text message as a single masked frame (FIN + opcode 0x1). CDP messages are small
    /// enough that fragmenting the client side buys nothing.
    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        let payload = text.as_bytes();
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x81); // FIN | text

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

        let mask = self.next_mask();
        frame.extend_from_slice(&mask);
        for (i, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[i & 3]);
        }

        (&self.writer).write_all(&frame)?;
        (&self.writer).flush()
    }

    /// Return the next complete text message, or `None` if the poll interval elapsed with no message
    /// ready (the caller can then do other work, e.g. check whether the target exited). Pings are
    /// answered transparently; a close frame or EOF is an error so the caller can end the session.
    pub fn poll_text(&mut self) -> io::Result<Option<String>> {
        loop {
            if let Some(text) = self.take_message()? {
                return Ok(Some(text));
            }
            match self.fill()? {
                Fill::Data => {}
                Fill::Timeout => return Ok(None),
                Fill::Closed => {
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "websocket closed"))
                }
            }
        }
    }

    /// Read whatever is available into the buffer, distinguishing data / idle timeout / close.
    fn fill(&mut self) -> io::Result<Fill> {
        let mut tmp = [0u8; 8192];
        match self.stream.read(&mut tmp) {
            Ok(0) => Ok(Fill::Closed),
            Ok(n) => {
                self.rbuf.extend_from_slice(&tmp[..n]);
                if self.rbuf.len() > MAX_WS_BYTES {
                    // A frame claiming a huge payload would otherwise grow this buffer toward that size
                    // (P3, pre-release audit): bound it so a hostile or runaway peer cannot exhaust memory.
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "websocket frame exceeds the size cap"));
                }

                Ok(Fill::Data)
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Ok(Fill::Timeout),
            Err(e) => Err(e),
        }
    }

    /// Assemble a complete text message from buffered frames, answering pings along the way. Returns
    /// `None` when the buffer does not yet hold a full frame (the caller reads more).
    fn take_message(&mut self) -> io::Result<Option<String>> {
        while let Some((fin, opcode, payload)) = take_frame(&mut self.rbuf) {
            match opcode {
                0x0..=0x2 => {
                    // continuation (0x0) | text (0x1) | binary (0x2) - accumulate until FIN.
                    self.msg.extend_from_slice(&payload);
                    if self.msg.len() > MAX_WS_BYTES {
                        // Many fragmented frames must not accumulate without bound either (P3).
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "websocket message exceeds the size cap"));
                    }
                    if fin {
                        let bytes = std::mem::take(&mut self.msg);
                        return String::from_utf8(bytes)
                            .map(Some)
                            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "websocket text was not UTF-8"));
                    }
                }
                0x9 => self.write_control(0xA, &payload)?, // ping -> pong (echo)
                0xA => {}                                  // pong - ignore
                0x8 => return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "websocket closed")),
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

    /// Send a masked control frame (pong) with a small payload.
    fn write_control(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 6);
        frame.push(0x80 | opcode);
        frame.push(0x80 | payload.len() as u8); // control payloads are <= 125 bytes
        let mask = self.next_mask();
        frame.extend_from_slice(&mask);
        for (i, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[i & 3]);
        }
        (&self.writer).write_all(&frame)?;
        (&self.writer).flush()
    }

    /// A varying (not necessarily random) 4-byte mask satisfies the RFC client-masking rule.
    fn next_mask(&mut self) -> [u8; 4] {
        self.mask_counter = self.mask_counter.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.mask_counter.to_be_bytes()
    }
}

/// Cap on the read buffer and on an assembled message (P3, pre-release audit). CDP messages are small, so
/// anything past this is a runaway or hostile peer - bounded so a huge or endless frame cannot grow memory
/// without limit. The peer here is a Chromium instance we launched, so this is defence in depth.
const MAX_WS_BYTES: usize = 16 * 1024 * 1024;

/// Parse and remove one complete WebSocket frame from the front of `buf`, if the buffer holds one.
/// Server frames are usually unmasked, but the mask bit is honoured either way. Returns
/// `(fin, opcode, unmasked_payload)`, or `None` when more bytes are needed.
fn take_frame(buf: &mut Vec<u8>) -> Option<(bool, u8, Vec<u8>)> {
    if buf.len() < 2 {
        return None;
    }
    let b0 = buf[0];
    let b1 = buf[1];
    let fin = b0 & 0x80 != 0;
    let opcode = b0 & 0x0f;
    let masked = b1 & 0x80 != 0;
    let len7 = (b1 & 0x7f) as usize;

    let (payload_len, header_len) = match len7 {
        126 => {
            if buf.len() < 4 {
                return None;
            }
            (u16::from_be_bytes([buf[2], buf[3]]) as usize, 4)
        }
        127 => {
            if buf.len() < 10 {
                return None;
            }
            let mut b = [0u8; 8];
            b.copy_from_slice(&buf[2..10]);
            (u64::from_be_bytes(b) as usize, 10)
        }
        n => (n, 2),
    };

    let mask_len = if masked { 4 } else { 0 };
    let total = header_len + mask_len + payload_len;
    if buf.len() < total {
        return None;
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
    Some((fin, opcode, payload))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn takes_a_short_frame() {
        let mut buf = server_frame(b"{\"id\":1}", true);
        let (fin, op, payload) = take_frame(&mut buf).unwrap();
        assert!(fin);
        assert_eq!(op, 0x1);
        assert_eq!(payload, b"{\"id\":1}");
        assert!(buf.is_empty());
    }

    #[test]
    fn takes_a_16bit_length_frame() {
        let big = vec![b'x'; 1000];
        let mut buf = server_frame(&big, true);
        let (_, _, payload) = take_frame(&mut buf).unwrap();
        assert_eq!(payload.len(), 1000);
    }

    #[test]
    fn returns_none_until_the_frame_is_complete() {
        let full = server_frame(b"hello", true);
        let mut partial = full[..4].to_vec(); // header + part of payload
        assert!(take_frame(&mut partial).is_none());
        assert_eq!(partial.len(), 4); // nothing consumed
    }

    #[test]
    fn leaves_trailing_bytes_of_the_next_frame() {
        let mut buf = server_frame(b"AB", true);
        buf.extend_from_slice(&server_frame(b"CD", true));
        let (_, _, first) = take_frame(&mut buf).unwrap();
        assert_eq!(first, b"AB");
        let (_, _, second) = take_frame(&mut buf).unwrap();
        assert_eq!(second, b"CD");
        assert!(take_frame(&mut buf).is_none());
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
        let (_, _, got) = take_frame(&mut buf).unwrap();
        assert_eq!(got, payload);
    }
}
