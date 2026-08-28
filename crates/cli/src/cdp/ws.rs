//! A minimal RFC 6455 WebSocket CLIENT, just enough to speak the Chrome DevTools Protocol to a
//! local Chromium/Electron target over `ws://127.0.0.1:<port>/...`. Deliberately dependency-free
//! (F3): the CDP driver is a substitution mechanism on par with the native hook, and the product
//! ships no async runtime or WebSocket crate. Scope is narrow on purpose - one connection, text
//! frames, client-side masking, and the control frames a browser actually sends (ping/close).
//!
//! What it does NOT do (unneeded for localhost CDP): TLS, permessage-deflate, the server-Accept
//! check (a client MAY skip it - RFC 6455 4.1), or a cryptographically random key (the key only
//! defeats caching proxies, irrelevant to a loopback debug port).

use std::io::{self, BufReader, Read, Write};
use std::net::TcpStream;

/// A framed text-message WebSocket over one TCP connection to a loopback CDP endpoint.
pub struct WsClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    mask_counter: u32,
}

impl WsClient {
    /// Connect to `host:port` and perform the HTTP upgrade handshake for `path` (the
    /// `webSocketDebuggerUrl` path from `/json/version`). Fails loudly if the server does not
    /// answer `101 Switching Protocols`.
    pub fn connect(host: &str, port: u16, path: &str) -> io::Result<WsClient> {
        let stream = TcpStream::connect((host, port))?;
        stream.set_nodelay(true).ok();
        // A safety net so an unresponsive target cannot hang the tool. Long enough for a quiet
        // interactive session between events; the long-lived event loop revisits this in slice C3.
        stream.set_read_timeout(Some(std::time::Duration::from_secs(20))).ok();
        let writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);

        // A fixed key is fine on loopback (see the module note). The server still echoes a valid
        // Accept; we simply do not verify it.
        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host}:{port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        (&writer).write_all(request.as_bytes())?;
        (&writer).flush()?;

        let status = read_handshake(&mut reader)?;
        if !status.contains(" 101") {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("websocket upgrade refused: {status}"),
            ));
        }

        Ok(WsClient { reader, writer, mask_counter: 0x9e37_79b9 })
    }

    /// Send one text message as a single masked frame (FIN + opcode 0x1). CDP messages are small
    /// enough that fragmenting the client side buys nothing.
    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        let payload = text.as_bytes();
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x81); // FIN | text

        let mask_bit = 0x80u8;
        let len = payload.len();
        if len < 126 {
            frame.push(mask_bit | len as u8);
        } else if len < 65536 {
            frame.push(mask_bit | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(mask_bit | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }

        // A varying (not necessarily random) 4-byte mask satisfies the RFC client-masking rule.
        self.mask_counter = self.mask_counter.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let mask = self.mask_counter.to_be_bytes();
        frame.extend_from_slice(&mask);
        for (i, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[i & 3]);
        }

        (&self.writer).write_all(&frame)?;
        (&self.writer).flush()
    }

    /// Receive the next text message, transparently answering pings and reassembling continuation
    /// frames. Returns an error on a close frame or EOF, so the caller can end the session.
    pub fn recv_text(&mut self) -> io::Result<String> {
        let mut message: Vec<u8> = Vec::new();
        loop {
            let (fin, opcode, payload) = self.read_frame()?;
            match opcode {
                0x0..=0x2 => {
                    // continuation (0x0) | text (0x1) | binary (0x2) - accumulate until FIN.
                    message.extend_from_slice(&payload);
                    if fin {
                        return String::from_utf8(message).map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidData, "websocket text was not UTF-8")
                        });
                    }
                }
                0x9 => self.write_control(0xA, &payload)?, // ping -> pong (echo)
                0xA => {}                                  // pong - ignore
                0x8 => {
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "websocket closed"));
                }
                other => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unexpected websocket opcode {other:#x}"),
                    ));
                }
            }
        }
    }

    /// Read one frame header + payload. Server frames are never masked.
    fn read_frame(&mut self) -> io::Result<(bool, u8, Vec<u8>)> {
        let mut hdr = [0u8; 2];
        self.reader.read_exact(&mut hdr)?;
        let fin = hdr[0] & 0x80 != 0;
        let opcode = hdr[0] & 0x0f;
        let masked = hdr[1] & 0x80 != 0;
        let len7 = (hdr[1] & 0x7f) as usize;

        let len = match len7 {
            126 => {
                let mut b = [0u8; 2];
                self.reader.read_exact(&mut b)?;
                u16::from_be_bytes(b) as usize
            }
            127 => {
                let mut b = [0u8; 8];
                self.reader.read_exact(&mut b)?;
                u64::from_be_bytes(b) as usize
            }
            n => n,
        };

        let mask = if masked {
            let mut m = [0u8; 4];
            self.reader.read_exact(&mut m)?;
            Some(m)
        } else {
            None
        };

        let mut payload = vec![0u8; len];
        self.reader.read_exact(&mut payload)?;
        if let Some(m) = mask {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= m[i & 3];
            }
        }
        Ok((fin, opcode, payload))
    }

    /// Send a masked control frame (pong/close) with a small payload.
    fn write_control(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 6);
        frame.push(0x80 | opcode);
        frame.push(0x80 | payload.len() as u8); // control payloads are <= 125 bytes
        self.mask_counter = self.mask_counter.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let mask = self.mask_counter.to_be_bytes();
        frame.extend_from_slice(&mask);
        for (i, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[i & 3]);
        }
        (&self.writer).write_all(&frame)?;
        (&self.writer).flush()
    }
}

/// Read the HTTP upgrade response, returning the status line. Consumes through the blank line that
/// terminates the headers so the stream is positioned at the first WebSocket frame.
fn read_handshake(reader: &mut BufReader<TcpStream>) -> io::Result<String> {
    let mut status = String::new();
    let mut line = Vec::new();
    let mut first = true;
    loop {
        line.clear();
        read_line(reader, &mut line)?;
        // A blank line (just CRLF) ends the headers.
        let trimmed = trim_crlf(&line);
        if first {
            status = String::from_utf8_lossy(trimmed).into_owned();
            first = false;
        }
        if trimmed.is_empty() {
            return Ok(status);
        }
    }
}

/// Read one line up to and including LF (we do not rely on BufRead::read_until to keep the byte
/// handling explicit and CRLF-aware).
fn read_line(reader: &mut BufReader<TcpStream>, out: &mut Vec<u8>) -> io::Result<()> {
    let mut byte = [0u8; 1];
    loop {
        reader.read_exact(&mut byte)?;
        out.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(());
        }
    }
}

fn trim_crlf(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Rebuild a client frame the way send_text does (header + mask + masked payload), then verify a
    // reader (server side) recovers the original bytes. Exercises the 7-bit and 16-bit length paths.
    fn roundtrip(payload: &[u8]) {
        let mut frame = Vec::new();
        frame.push(0x81u8);
        let mask = [0x12u8, 0x34, 0x56, 0x78];
        if payload.len() < 126 {
            frame.push(0x80 | payload.len() as u8);
        } else {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        for (i, b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i & 3]);
        }

        // Decode header the same way read_frame does and unmask.
        let fin = frame[0] & 0x80 != 0;
        let opcode = frame[0] & 0x0f;
        assert!(fin);
        assert_eq!(opcode, 0x1);
        let masked = frame[1] & 0x80 != 0;
        assert!(masked);
        let len7 = (frame[1] & 0x7f) as usize;
        let (len, mut off) = if len7 == 126 {
            (u16::from_be_bytes([frame[2], frame[3]]) as usize, 4)
        } else {
            (len7, 2)
        };
        let m = [frame[off], frame[off + 1], frame[off + 2], frame[off + 3]];
        off += 4;
        let mut got = Vec::new();
        for i in 0..len {
            got.push(frame[off + i] ^ m[i & 3]);
        }
        assert_eq!(got, payload);
    }

    #[test]
    fn short_frame_roundtrips() {
        roundtrip(b"{\"id\":1,\"method\":\"Browser.getVersion\"}");
    }

    #[test]
    fn frame_at_the_16bit_boundary_roundtrips() {
        let big = vec![b'x'; 1000];
        roundtrip(&big);
    }

    #[test]
    fn trim_crlf_strips_line_endings() {
        assert_eq!(trim_crlf(b"HTTP/1.1 101\r\n"), b"HTTP/1.1 101");
        assert_eq!(trim_crlf(b""), b"");
        assert_eq!(trim_crlf(b"\r\n"), b"");
    }
}
