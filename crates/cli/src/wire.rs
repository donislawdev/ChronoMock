//! Reading one NDJSON line off the machine protocol, with a bound on its length.
//!
//! The protocol itself (message shapes, versions) lives in `chrono-proto`. This is only the
//! transport detail both ends share: how much of a line either side is willing to read before
//! calling the stream broken.

use std::io::BufRead;
use std::sync::mpsc;

use chrono_proto::{parse_command, Command, Event};

use crate::events::{emit, error_event};

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
///
/// The length is judged before the text. The cap cuts a line at a byte, and a line of text cut inside
/// a character is no longer text: read as one string, such a line was refused as "not UTF-8" and sent
/// the client looking at its encoding, when what it did wrong was the length (measured by the random
/// property below, 2026-10-19).
pub(crate) fn read_protocol_line<R: BufRead>(reader: &mut R, line: &mut String) -> std::io::Result<usize> {
    let mut bytes = std::mem::take(line).into_bytes();
    bytes.clear();
    // UFCS on purpose: method syntax auto-derefs to `R`, and `Read::take` consumes its receiver, so
    // `reader.take(..)` tries to move the reader out of the borrow. Naming `&mut R` as the receiver
    // borrows it for the length of the read instead.
    let n = std::io::Read::take(&mut *reader, MAX_PROTOCOL_LINE as u64).read_until(b'\n', &mut bytes)?;
    if n == MAX_PROTOCOL_LINE && bytes.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("protocol line exceeds {MAX_PROTOCOL_LINE} bytes with no newline"),
        ));
    }
    *line = String::from_utf8(bytes)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "stream did not contain valid UTF-8"))?;
    Ok(n)
}


/// What one bounded read of a line found (see [`read_line_bytes`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LineRead {
    /// The stream ended.
    Eof,
    /// A line, in the buffer, with its newline when it had one.
    Line,
    /// A line past [`MAX_PROTOCOL_LINE`], read to its end and dropped.
    TooLong,
}

/// Read one line as bytes, bounded like [`read_protocol_line`], for a reader that skips what it
/// cannot use rather than stopping.
///
/// The driver reading the core is that reader (R4-W1). It used to read text and stop at the first
/// byte that was not UTF-8 or the first line past the cap, and a target that shared the core's stdout
/// wrote exactly such bytes: a program writing in a Polish console's code page was enough (measured
/// with a probe writing CP852, `tools/probes/r4-5`). The target is kept off the protocol
/// now, but a reader that ends a session over one bad line hands the next such writer the same power.
/// So the bytes come back as they are, the caller decides whether they are text, and a line over the
/// cap is consumed to its newline so the next read starts on the next line.
pub(crate) fn read_line_bytes<R: BufRead>(reader: &mut R, line: &mut Vec<u8>) -> std::io::Result<LineRead> {
    line.clear();
    // UFCS for the same reason as in `read_protocol_line`.
    let n = std::io::Read::take(&mut *reader, MAX_PROTOCOL_LINE as u64).read_until(b'\n', line)?;
    if n == 0 {
        return Ok(LineRead::Eof);
    }
    if n < MAX_PROTOCOL_LINE || line.last() == Some(&b'\n') {
        return Ok(LineRead::Line);
    }
    loop {
        let buffered = reader.fill_buf()?;
        if buffered.is_empty() {
            break;
        }
        if let Some(at) = buffered.iter().position(|&b| b == b'\n') {
            reader.consume(at + 1);
            break;
        }
        let len = buffered.len();
        reader.consume(len);
    }
    line.clear();
    Ok(LineRead::TooLong)
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
                    sink(&error_event(None, 1, stream_error_key(&e), "core"));
                    break;
                }
                Ok(_) => match parse_command(line.trim_end()) {
                    Ok(cmd) => {
                        if tx.send(cmd).is_err() {
                            break;
                        }
                    }
                    Err(_) => sink(&error_event(None, 1, "protocol.bad_command_ignored", "core")),
                },
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_proto::PROTOCOL_VERSION;

    /// The protocol line was the only input channel in this tool without a bound, so a writer that
    /// never sent a newline grew the reader's buffer as long as it liked.
    /// The skipping reader: a line with bytes that are not UTF-8 comes back as bytes, a line past the
    /// cap is dropped whole, and in both cases the NEXT line is read intact - the property the driver
    /// needs to keep reading the core past a bad line (R4-W1).
    #[test]
    fn a_bad_or_endless_line_is_skipped_and_the_next_one_is_read_whole() {
        let mut input = b"Odpowied\xAB z 127.0.0.1\r\n".to_vec();
        input.extend(vec![b'x'; MAX_PROTOCOL_LINE + 10]);
        input.extend_from_slice(b"\n{\"type\":\"ended\"}\nlast line without a newline");
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
        let mut line = Vec::new();

        assert_eq!(read_line_bytes(&mut reader, &mut line).unwrap(), LineRead::Line);
        assert!(String::from_utf8(line.clone()).is_err(), "the bytes come back as they are");
        assert_eq!(read_line_bytes(&mut reader, &mut line).unwrap(), LineRead::TooLong);
        assert!(line.is_empty());
        assert_eq!(read_line_bytes(&mut reader, &mut line).unwrap(), LineRead::Line);
        assert_eq!(line, b"{\"type\":\"ended\"}\n");
        assert_eq!(read_line_bytes(&mut reader, &mut line).unwrap(), LineRead::Line);
        assert_eq!(line, b"last line without a newline");
        assert_eq!(read_line_bytes(&mut reader, &mut line).unwrap(), LineRead::Eof);
    }

    /// A line of exactly the cap that ends in its newline is a line, not an over-long one.
    #[test]
    fn a_line_exactly_at_the_cap_is_still_a_line() {
        let mut input = vec![b'x'; MAX_PROTOCOL_LINE - 1];
        input.push(b'\n');
        input.extend_from_slice(b"next\n");
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
        let mut line = Vec::new();
        assert_eq!(read_line_bytes(&mut reader, &mut line).unwrap(), LineRead::Line);
        assert_eq!(line.len(), MAX_PROTOCOL_LINE);
        assert_eq!(read_line_bytes(&mut reader, &mut line).unwrap(), LineRead::Line);
        assert_eq!(line, b"next\n");
    }

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

    mod properties {
        use super::*;
        use crate::search::edge_u64;
        use chrono_proto::{MomentKind, MomentSpec, QueryWhat, TargetConsole, TargetSpec, TimeModeKind, TimeSpec};
        use proptest::collection::vec;
        use proptest::option;
        use proptest::sample::select;
        use proptest::prelude::*;
        use std::io::{BufReader, Cursor};

        /// The bytes of one line, printed by their length and their two ends: a line drawn at the cap
        /// is a megabyte, and a failing case is printed in full.
        #[derive(Clone)]
        struct Body(Vec<u8>);

        impl std::fmt::Debug for Body {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let b = &self.0;
                if b.len() <= 48 {
                    return write!(f, "{:?}", String::from_utf8_lossy(b));
                }
                let (head, tail) = (&b[..24], &b[b.len() - 24..]);
                write!(f, "{} bytes {:?}..{:?}", b.len(), String::from_utf8_lossy(head), String::from_utf8_lossy(tail))
            }
        }

        /// A stream of lines, the last with or without its newline, read through a buffer of a size
        /// that puts the reader's chunk edges anywhere - the skip of an over-long line works chunk by
        /// chunk.
        #[derive(Debug, Clone)]
        struct Stream {
            lines: Vec<Body>,
            last_ends: bool,
            buffer: usize,
        }

        impl Stream {
            fn bytes(&self) -> Vec<u8> {
                let mut out = Vec::new();
                for (i, line) in self.lines.iter().enumerate() {
                    out.extend_from_slice(&line.0);
                    if i + 1 < self.lines.len() || self.last_ends {
                        out.push(b'\n');
                    }
                }
                out
            }

            fn reader(&self) -> BufReader<Cursor<Vec<u8>>> {
                BufReader::with_capacity(self.buffer, Cursor::new(self.bytes()))
            }

            /// Each line as a reader meets it: its body and whether a newline ended it. An empty last
            /// line without a newline is no line at all - the stream simply ended.
            fn met(&self) -> impl Iterator<Item = (&[u8], bool)> {
                let count = self.lines.len();
                self.lines.iter().enumerate().filter_map(move |(i, line)| {
                    let newline = i + 1 < count || self.last_ends;
                    (newline || !line.0.is_empty()).then_some((line.0.as_slice(), newline))
                })
            }
        }

        /// Text across the cap: a character of two, three or four bytes placed so the cap falls
        /// inside it, or just before or after it.
        fn across_the_cap() -> impl Strategy<Value = Vec<u8>> {
            (prop_oneof![Just('ł'), Just('€'), Just('\u{1f600}')], 0usize..=4).prop_map(|(c, back)| {
                let mut body = vec![b'x'; MAX_PROTOCOL_LINE - back];
                body.extend_from_slice(c.to_string().as_bytes());
                body.extend_from_slice(b"tail");
                body
            })
        }

        /// A line's bytes, a newline never among them: short, of any bytes or of text, at the cap and
        /// either side of it - by name, since a uniform length is never there - and well past it.
        fn body() -> impl Strategy<Value = Body> {
            let byte = any::<u8>().prop_map(|b| if b == b'\n' { b'\r' } else { b });
            let at_the_cap = ((MAX_PROTOCOL_LINE - 2)..=(MAX_PROTOCOL_LINE + 2), prop_oneof![Just(b'x'), Just(0xC3u8)])
                .prop_map(|(len, fill)| vec![fill; len]);
            prop_oneof![
                6 => vec(byte, 0..40),
                3 => "[^\n]{0,30}".prop_map(String::into_bytes),
                1 => at_the_cap,
                1 => across_the_cap(),
                1 => (MAX_PROTOCOL_LINE + 3..MAX_PROTOCOL_LINE + 5000).prop_map(|len| vec![b'y'; len]),
            ]
            .prop_map(Body)
        }

        fn stream() -> impl Strategy<Value = Stream> {
            (vec(body(), 0..5), any::<bool>(), prop_oneof![1usize..=64, Just(8192usize)])
                .prop_map(|(lines, last_ends, buffer)| Stream { lines, last_ends, buffer })
        }

        /// Each read as kind and length, the form a failure is printed in.
        fn shape(reads: &[(LineRead, Vec<u8>)]) -> Vec<(&LineRead, usize)> {
            reads.iter().map(|(kind, bytes)| (kind, bytes.len())).collect()
        }

        /// A command as the GUI or the driver writes one, with the text and numbers a session may put
        /// in it.
        fn command() -> impl Strategy<Value = Command> {
            let text = || "[^\u{0}]{0,12}";
            // Flat selections rather than nested `prop_oneof!`: the nesting deepened the strategy enough
            // to overflow the test thread's stack in a debug build.
            let kind = select(vec![MomentKind::Absolute, MomentKind::Relative, MomentKind::Unknown]);
            let mode = select(vec![TimeModeKind::Flow, TimeModeKind::Frozen, TimeModeKind::Multiplier, TimeModeKind::Unknown]);
            let what = select(vec![QueryWhat::State, QueryWhat::Unknown]);
            let moment = (kind, option::of(text()), option::of(any::<i32>()), option::of(text()))
                .prop_map(|(kind, local, tz_bias_min, delta)| MomentSpec { kind, local, tz_bias_min, delta });
            let target = (text(), vec(text(), 0..3), option::of(text()), any::<[bool; 4]>(), any::<bool>()).prop_map(
                |(path, args, cwd, [embedded, elevated_embedded, keep_background_timers, gecko_sandbox], new)| TargetSpec {
                    path,
                    args,
                    cwd,
                    embedded,
                    elevated_embedded,
                    keep_background_timers,
                    gecko_sandbox,
                    console: if new { TargetConsole::New } else { TargetConsole::Shared },
                },
            );
            let time = (moment.clone(), mode, option::of(any::<i64>()), any::<[bool; 2]>()).prop_map(
                |(moment, mode, multiplier, [scale_duration, scale_qpc])| TimeSpec { moment, mode, multiplier, scale_duration, scale_qpc },
            );
            let id = edge_u64;
            prop_oneof![
                (id(), target, time, any::<bool>())
                    .prop_map(|(id, target, time, force)| Command::Start { v: PROTOCOL_VERSION, id, target, time, force }),
                (id(), what).prop_map(|(id, what)| Command::Query { v: PROTOCOL_VERSION, id, what }),
                (id(), any::<i64>()).prop_map(|(id, multiplier)| Command::SetMultiplier { v: PROTOCOL_VERSION, id, multiplier }),
                (id(), moment).prop_map(|(id, to)| Command::Jump { v: PROTOCOL_VERSION, id, to }),
                id().prop_map(|id| Command::End { v: PROTOCOL_VERSION, id }),
            ]
        }

        /// A line no reading of the protocol can take for a command, from the families a client gets
        /// wrong: nothing, blanks, a command cut short, a command of a type that does not exist, a
        /// command without its id, and words that are not an object at all.
        fn junk() -> impl Strategy<Value = String> {
            let cut = command().prop_flat_map(|c| {
                let line = serde_json::to_string(&c).expect("a command is JSON");
                let ends: Vec<usize> = line.char_indices().map(|(i, _)| i).collect();
                (Just(line), proptest::sample::select(ends))
            });
            let unknown = ("[a-z_]{1,12}", edge_u64()).prop_map(|(kind, id)| {
                let kind = if ["start", "query", "set_multiplier", "jump", "end"].contains(&kind.as_str()) { kind + "_" } else { kind };
                format!(r#"{{"type":"{kind}","v":1,"id":{id}}}"#)
            });
            let without_id = command().prop_map(|c| {
                let mut value = serde_json::to_value(&c).expect("a command is JSON");
                value.as_object_mut().expect("a command is an object").remove("id");
                value.to_string()
            });
            prop_oneof![
                1 => Just(String::new()),
                1 => "[ \t]{1,4}",
                3 => cut.prop_map(|(line, end)| line[..end].to_string()),
                2 => unknown,
                2 => without_id,
                2 => "[^{ \t\r\n][^\r\n]{0,30}",
            ]
        }

        /// One line a client sends, and what the reader must make of it: a command - as JSON, the
        /// form it is compared in, beside the line it was written as - or a line that is none.
        #[derive(Debug, Clone)]
        enum Sent {
            Command(serde_json::Value, String),
            Junk(String),
        }

        fn sent() -> impl Strategy<Value = Sent> {
            let written = (command(), "[ \t]{0,2}", prop_oneof![Just(""), Just("\r"), Just(" ")])
                .prop_map(|(c, before, after)| {
                    let line = format!("{before}{}{after}", serde_json::to_string(&c).expect("a command is JSON"));
                    Sent::Command(serde_json::to_value(&c).expect("a command is JSON"), line)
                });
            prop_oneof![3 => written, 2 => junk().prop_map(Sent::Junk)]
        }

        /// What ends a stream before its end: a line past the cap, or bytes that are not text.
        #[derive(Debug, Clone)]
        enum Broken {
            TooLong(Body),
            NotText(Body),
        }

        fn broken() -> impl Strategy<Value = Broken> {
            let too_long = prop_oneof![
                (MAX_PROTOCOL_LINE..=MAX_PROTOCOL_LINE + 2).prop_map(|len| vec![b'x'; len]),
                across_the_cap(),
            ];
            let not_text = prop_oneof![
                Just(vec![0xFFu8]),
                Just(b"{\"type\":\"end\",\"v\":1,\"id\":\xC3}".to_vec()),
                Just("ł".as_bytes()[..1].to_vec()),
            ];
            prop_oneof![too_long.prop_map(|b| Broken::TooLong(Body(b))), not_text.prop_map(|b| Broken::NotText(Body(b)))]
        }

        proptest! {
            /// R4-W1. The driver reads the core past whatever arrives: any bytes come back as they are,
            /// a line past the cap is dropped whole and only that line, and the next one is read intact -
            /// wherever the reader's buffer happens to cut the stream. A reader that stopped at the first
            /// line that was not text ended a session over one program writing in a console code page.
            #[test]
            fn any_stream_reads_back_line_by_line_and_a_line_past_the_cap_goes_whole(stream in stream()) {
                let want: Vec<(LineRead, Vec<u8>)> = stream
                    .met()
                    .map(|(body, newline)| {
                        if body.len() >= MAX_PROTOCOL_LINE {
                            (LineRead::TooLong, Vec::new())
                        } else {
                            let mut bytes = body.to_vec();
                            if newline {
                                bytes.push(b'\n');
                            }
                            (LineRead::Line, bytes)
                        }
                    })
                    .chain([(LineRead::Eof, Vec::new())])
                    .collect();
                let mut reader = stream.reader();
                let mut got = Vec::new();
                let mut line = Vec::new();
                while got.len() <= want.len() {
                    let read = read_line_bytes(&mut reader, &mut line).expect("an in-memory stream reads");
                    let end = read == LineRead::Eof;
                    got.push((read, line.clone()));
                    if end {
                        break;
                    }
                }
                prop_assert!(got == want, "read {:?}, wanted {:?}", shape(&got), shape(&want));
            }

            /// A line of the protocol is text under the cap, or a refusal that names which of the two it
            /// is not. A line past the cap is called too long whatever its bytes, because the length is
            /// judged first - text cut by the cap inside a character is still text that was too long, not
            /// text that was never text - so the client is sent to look at what it actually did wrong
            /// (rule 6).
            #[test]
            fn a_protocol_line_is_text_under_the_cap_or_names_what_it_is_not(stream in stream()) {
                let mut reader = stream.reader();
                let mut line = String::new();
                for (body, newline) in stream.met() {
                    let read = read_protocol_line(&mut reader, &mut line);
                    if body.len() >= MAX_PROTOCOL_LINE {
                        let key = read.as_ref().err().map(stream_error_key);
                        prop_assert_eq!(key, Some("protocol.line_too_long"), "for {:?}", Body(body.to_vec()));
                        return Ok(());
                    }
                    let Ok(text) = std::str::from_utf8(body) else {
                        let key = read.as_ref().err().map(stream_error_key);
                        prop_assert_eq!(key, Some("protocol.stream_unreadable"), "for {:?}", Body(body.to_vec()));
                        return Ok(());
                    };
                    prop_assert!(read.is_ok(), "{:?} was refused: {:?}", Body(body.to_vec()), read.err());
                    let want = if newline { format!("{text}\n") } else { text.to_string() };
                    prop_assert_eq!(read.ok(), Some(want.len()));
                    prop_assert!(line == want, "read {:?}, wanted {:?}", Body(line.clone().into_bytes()), Body(want.into_bytes()));
                }
                prop_assert_eq!(read_protocol_line(&mut reader, &mut line).ok(), Some(0), "the stream did not end");
            }

            /// The command stream: every command a client writes arrives, in order and as written, and
            /// every line that is not one is answered once and passed over. A stream that cannot be read
            /// on ends with one answer that says why, and nothing after it arrives. Each part of this was
            /// once missing: the answers after the first command, the answer to an over-long line, and
            /// the cap itself on the stdin of a Chromium session.
            #[test]
            fn every_command_arrives_in_order_and_every_other_line_is_answered_once(
                lines in vec(sent(), 0..8),
                broken in option::of(broken()),
                after in vec(command(), 0..2),
            ) {
                let mut bytes = Vec::new();
                let mut want_commands = Vec::new();
                let mut want_keys = Vec::new();
                for line in &lines {
                    match line {
                        Sent::Command(value, written) => {
                            bytes.extend_from_slice(written.as_bytes());
                            want_commands.push(value.clone());
                        }
                        Sent::Junk(text) => {
                            bytes.extend_from_slice(text.as_bytes());
                            want_keys.push("protocol.bad_command_ignored");
                        }
                    }
                    bytes.push(b'\n');
                }
                if let Some(broken) = &broken {
                    let (body, key) = match broken {
                        Broken::TooLong(b) => (b, "protocol.line_too_long"),
                        Broken::NotText(b) => (b, "protocol.stream_unreadable"),
                    };
                    bytes.extend_from_slice(&body.0);
                    bytes.push(b'\n');
                    want_keys.push(key);
                    for c in &after {
                        bytes.extend_from_slice(serde_json::to_string(c).expect("a command is JSON").as_bytes());
                        bytes.push(b'\n');
                    }
                }
                let (keys_tx, keys_rx) = mpsc::channel::<String>();
                let commands = spawn_command_reader_with(BufReader::new(Cursor::new(bytes)), move |e: &Event| {
                    let key = match e {
                        Event::Error { key, .. } => key.clone(),
                        other => format!("not an error: {other:?}"),
                    };
                    let _ = keys_tx.send(key);
                });
                let got_commands: Vec<serde_json::Value> =
                    commands.into_iter().map(|c| serde_json::to_value(&c).expect("a command is JSON")).collect();
                let got_keys: Vec<String> = keys_rx.into_iter().collect();
                prop_assert_eq!(got_commands, want_commands);
                prop_assert_eq!(got_keys, want_keys);
            }
        }
    }
}
