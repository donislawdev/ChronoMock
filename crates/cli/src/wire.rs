//! Reading one NDJSON line off the machine protocol, with a bound on its length.
//!
//! The protocol itself (message shapes, versions) lives in `chrono-proto`. This is only the
//! transport detail both ends share: how much of a line either side is willing to read before
//! calling the stream broken.

use std::io::BufRead;
use std::sync::mpsc;

use chrono_proto::{parse_command, Command, Event, PROTOCOL_VERSION};

use crate::events::emit;

/// The largest NDJSON line either side of the protocol will read, in bytes.
///
/// Every other input channel in this tool is bounded - `MAX_WS_BYTES`, `MAX_HTTP_BODY`,
/// `MAX_HEADERS`, `MAX_HEADER_LINE`, `MAX_QUEUED_EVENTS`, the seqlock read budget, the
/// business-day walk - and the protocol line was the one that was not: a writer that never sent a
/// newline grew the reader's buffer for as long as it liked. Both ends are processes this tool
/// started, so this is depth rather than a hole - the failure it actually guards is our own, a core
/// stuck mid-line taking the driver's memory with it.
///
/// A megabyte is a deliberate 680x over the largest line MEASURED on 2026-09-05: a native session at
/// full coverage (36 channels, `--scale-duration`) emits a 1 533-byte `coverage` event, and a CDP
/// session 291. The cap is meant to catch a stream that has stopped making sense, not to be a size
/// the protocol ever approaches.
pub(crate) const MAX_PROTOCOL_LINE: usize = 1024 * 1024;

/// Read one NDJSON protocol line, refusing one that never ends. `Ok(0)` is EOF, as with `read_line`.
///
/// A line at the cap with no newline in it is an error and NOT a truncated line handed onwards: the
/// reader is then parked mid-line, so the remainder would arrive as the next "line" and parse as
/// junk - or worse, as a different event than the writer sent.
pub(crate) fn read_protocol_line<R: BufRead>(reader: &mut R, line: &mut String) -> std::io::Result<usize> {
    line.clear();
    // UFCS on purpose: method syntax auto-derefs to `R`, and `Read::take` consumes its receiver, so
    // `reader.take(..)` tries to move the reader out of the borrow. Naming `&mut R` as the receiver
    // borrows it for the length of the read instead.
    let n = std::io::Read::take(&mut *reader, MAX_PROTOCOL_LINE as u64).read_line(line)?;
    if n == MAX_PROTOCOL_LINE && !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("protocol line exceeds {MAX_PROTOCOL_LINE} bytes with no newline"),
        ));
    }
    Ok(n)
}


/// Turn a reader of protocol lines into a stream of commands, on its own thread.
///
/// Both sessions need exactly this, because the main thread has a heartbeat to beat and a target to
/// watch and cannot sit inside a read. They had a copy each, and the copies drifted: the native one
/// moved onto the bounded read when `MAX_PROTOCOL_LINE` came in and the Chromium one was missed, so
/// one stdin was bounded and the other was not, from the same loop with the same comment. There is
/// one now, and the bound cannot go missing from half of it.
///
/// A line past the cap ends the stream exactly as EOF does, rather than resyncing onto the tail of a
/// line nobody can vouch for. A line that does not PARSE is answered and the stream continues: the
/// first command (`start`) answered bad input properly and every one after it did not, so a client
/// waiting on `ack` waited for ever. No id on that answer, because the id lived in the line we could
/// not read.
pub(crate) fn spawn_command_reader<R>(reader: R) -> mpsc::Receiver<Command>
where
    R: BufRead + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Command>();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            match read_protocol_line(&mut reader, &mut line) {
                // EOF, or a line past the cap: dropping tx signals Disconnected to the session loop.
                Ok(0) | Err(_) => break,
                Ok(_) => match parse_command(line.trim_end()) {
                    Ok(cmd) => {
                        if tx.send(cmd).is_err() {
                            break;
                        }
                    }
                    Err(_) => emit(&Event::Error {
                        v: PROTOCOL_VERSION,
                        id: None,
                        code: 1,
                        key: "protocol.bad_command_ignored".into(),
                        origin: "core".into(),
                    }),
                },
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The protocol line was the only input channel in this tool without a bound, so a writer that
    /// never sent a newline grew the reader's buffer as long as it liked.
    #[test]
    fn a_protocol_line_that_never_ends_is_refused() {
        let endless = vec![b'x'; MAX_PROTOCOL_LINE + 10];
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(endless));
        let mut line = String::new();
        assert!(read_protocol_line(&mut reader, &mut line).is_err());
    }

    /// The bound must not change what an ordinary session reads, including the shapes that sit near
    /// its edges: an empty stream, and a final line with no terminator (which is NOT over-long, and
    /// reading it as an error would truncate the last event of a session).
    #[test]
    fn ordinary_protocol_lines_are_unaffected_by_the_bound() {
        let stream = "{\"type\":\"ready\"}\n{\"type\":\"state\"}\nlast line without a newline";
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(stream.as_bytes().to_vec()));
        let mut line = String::new();

        assert_eq!(read_protocol_line(&mut reader, &mut line).unwrap(), 17);
        assert_eq!(line, "{\"type\":\"ready\"}\n");
        // The buffer is cleared by the reader, so a caller cannot accidentally append events.
        assert_eq!(read_protocol_line(&mut reader, &mut line).unwrap(), 17);
        assert_eq!(line, "{\"type\":\"state\"}\n");

        assert_eq!(read_protocol_line(&mut reader, &mut line).unwrap(), 27);
        assert_eq!(line, "last line without a newline");
        assert_eq!(read_protocol_line(&mut reader, &mut line).unwrap(), 0, "EOF");
    }

    /// The bound reaches the command STREAM, not only a single read. The Chromium session kept its
    /// own copy of this loop on an unbounded `read_line`, so of the two stdin readers in the tool one
    /// refused an endless line and the other grew with it - same code, same comment, one missing cap.
    #[test]
    fn a_command_past_the_cap_ends_the_stream_instead_of_growing() {
        let mut stream = String::from("{\"type\":\"query\",\"v\":1,\"id\":7,\"what\":\"state\"}\n");
        stream.push_str(&"x".repeat(MAX_PROTOCOL_LINE + 10));
        stream.push('\n');
        stream.push_str("{\"type\":\"end\",\"v\":1,\"id\":8}\n");
        let rx = spawn_command_reader(std::io::BufReader::new(std::io::Cursor::new(stream.into_bytes())));

        assert!(
            matches!(rx.recv(), Ok(Command::Query { id: 7, .. })),
            "the command before the over-long line still arrives"
        );
        assert!(
            rx.recv().is_err(),
            "the over-long line ends the stream, so the command after it never arrives"
        );
    }

    /// A line that does not parse is answered and the stream CONTINUES, so one bad line does not end
    /// a session. The answer itself travels as `protocol.bad_command_ignored` on stdout.
    #[test]
    fn an_unparseable_line_does_not_end_the_command_stream() {
        let stream = "not json at all\n{\"type\":\"end\",\"v\":1,\"id\":9}\n";
        let rx = spawn_command_reader(std::io::BufReader::new(std::io::Cursor::new(stream.as_bytes().to_vec())));

        assert!(
            matches!(rx.recv(), Ok(Command::End { id: 9, .. })),
            "the command after an unreadable line still arrives"
        );
    }
}
