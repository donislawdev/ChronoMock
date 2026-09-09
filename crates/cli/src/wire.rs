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


/// Which of the two transport failures ended the stream.
///
/// They are separate keys because they are separate mistakes on the client's side and have separate
/// fixes: one line grew past [`MAX_PROTOCOL_LINE`] with no newline in it, or the bytes were not UTF-8
/// at all. Folding them into one key would hand the reader a message that fits half the cases.
///
/// `InvalidData` is what `read_line` returns for bytes that are not text, and the over-long line is
/// raised by [`read_protocol_line`] with the same kind - so the two are told apart by the message
/// this crate itself wrote, not by guessing at the io error.
fn stream_error_key(e: &std::io::Error) -> &'static str {
    if e.to_string().contains("protocol line exceeds") {
        "protocol.line_too_long"
    } else {
        "protocol.stream_unreadable"
    }
}

/// Turn a reader of protocol lines into a stream of commands, on its own thread.
///
/// Both sessions need exactly this, because the main thread has a heartbeat to beat and a target to
/// watch and cannot sit inside a read. They had a copy each, and the copies drifted: the native one
/// moved onto the bounded read when `MAX_PROTOCOL_LINE` came in and the Chromium one was missed, so
/// one stdin was bounded and the other was not, from the same loop with the same comment. There is
/// one now, and the bound cannot go missing from half of it.
///
/// A line past the cap ends the stream rather than resyncing onto the tail of a line nobody can
/// vouch for - but it says so on the way out, which it did not use to. A line that does not PARSE is
/// answered and the stream continues: the first command (`start`) answered bad input properly and
/// every one after it did not, so a client waiting on `ack` waited for ever. No id on either answer,
/// because the id lived in the line we could not read.
pub(crate) fn spawn_command_reader<R>(reader: R) -> mpsc::Receiver<Command>
where
    R: BufRead + Send + 'static,
{
    spawn_command_reader_with(reader, emit)
}

/// [`spawn_command_reader`] with the event sink handed in.
///
/// The sink exists for one reason: `emit` writes to the process's stdout, and a test cannot read that
/// back without capturing a global. What has to be tested here is not the FORMATTING of the event -
/// `chrono-proto` owns that - but that one is emitted AT ALL on the path that used to be silent. A
/// test asserting only the key mapping would have passed over the version of this loop that mapped
/// the key correctly and then never sent it.
pub(crate) fn spawn_command_reader_with<R, E>(reader: R, sink: E) -> mpsc::Receiver<Command>
where
    R: BufRead + Send + 'static,
    E: Fn(&Event) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Command>();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            match read_protocol_line(&mut reader, &mut line) {
                // EOF: the client closed its end. That is the ordinary way a session ends, and it
                // needs no announcement - dropping tx signals Disconnected to the session loop.
                Ok(0) => break,
                // A TRANSPORT failure ends the stream too, and used to end it in exactly the same
                // silence, so a client that sent an over-long line or a byte that is not UTF-8 saw
                // what looked like an ordinary EOF and was told the core had stopped. Silence is
                // forbidden (rule 6): say which of the two happened before going, so the reader
                // looks at what they sent rather than at the core.
                Err(e) => {
                    sink(&Event::Error {
                        v: PROTOCOL_VERSION,
                        id: None,
                        code: 1,
                        key: stream_error_key(&e).into(),
                        origin: "core".into(),
                    });
                    break;
                }
                Ok(_) => match parse_command(line.trim_end()) {
                    Ok(cmd) => {
                        if tx.send(cmd).is_err() {
                            break;
                        }
                    }
                    Err(_) => sink(&Event::Error {
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

    /// Collect the events a reader emits, so a test can assert that one was SENT rather than only
    /// that its key would have been right.
    fn events_of(stream: &str) -> (Vec<Command>, Vec<String>) {
        let (etx, erx) = mpsc::channel::<String>();
        let rx = spawn_command_reader_with(
            std::io::BufReader::new(std::io::Cursor::new(stream.as_bytes().to_vec())),
            move |e: &Event| {
                if let Event::Error { key, .. } = e {
                    let _ = etx.send(key.clone());
                }
            },
        );
        let commands: Vec<Command> = rx.into_iter().collect();
        let keys: Vec<String> = erx.into_iter().collect();
        (commands, keys)
    }

    /// 🔴 A transport failure used to end the stream in exactly the silence of an ordinary EOF, so a
    /// client that sent an over-long line was told the CORE had stopped - a diagnosis pointing away
    /// from the mistake. Ending the stream is still right, because a half-read line cannot be resynced
    /// onto - going without a word was not.
    #[test]
    fn an_over_long_line_says_why_the_stream_ended() {
        let mut stream = String::from("{\"type\":\"query\",\"v\":1,\"id\":7,\"what\":\"state\"}\n");
        stream.push_str(&"x".repeat(MAX_PROTOCOL_LINE + 10));
        stream.push('\n');

        let (commands, keys) = events_of(&stream);

        assert_eq!(commands.len(), 1, "the command before the over-long line still arrives");
        assert_eq!(keys, ["protocol.line_too_long"]);
    }

    /// EOF is the ordinary end of a session and needs no announcement - a key here would cry wolf on
    /// every clean shutdown, which is the other half of rule 6.
    #[test]
    fn a_clean_end_of_stream_says_nothing() {
        let (commands, keys) = events_of("{\"type\":\"end\",\"v\":1,\"id\":9}\n");

        assert_eq!(commands.len(), 1);
        assert!(keys.is_empty(), "a clean EOF is not a failure to report, got {keys:?}");
    }

    /// The two transport failures are told apart, so the message names what the client actually sent.
    /// One key for both would fit half the cases and send the reader to look at the wrong thing.
    #[test]
    fn the_two_transport_failures_are_named_separately() {
        let too_long = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("protocol line exceeds {MAX_PROTOCOL_LINE} bytes with no newline"),
        );
        assert_eq!(stream_error_key(&too_long), "protocol.line_too_long");

        // What `read_line` returns for bytes that are not text - the same error KIND, so the two
        // cannot be told apart by kind alone, which is why the message is what decides.
        let not_utf8 = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        );
        assert_eq!(stream_error_key(&not_utf8), "protocol.stream_unreadable");
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
