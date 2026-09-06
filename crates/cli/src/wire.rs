//! Reading one NDJSON line off the machine protocol, with a bound on its length.
//!
//! The protocol itself (message shapes, versions) lives in `chrono-proto`. This is only the
//! transport detail both ends share: how much of a line either side is willing to read before
//! calling the stream broken.

use std::io::BufRead;

/// The largest NDJSON line either side of the protocol will read, in bytes.
///
/// Every other input channel in this tool is bounded - `MAX_WS_BYTES`, `MAX_HTTP_BODY`,
/// `MAX_HEADERS`, `MAX_HEADER_LINE`, `MAX_QUEUED_EVENTS`, the seqlock read budget, the
/// business-day walk - and the protocol line was the one that was not: a writer that never sent a
/// newline grew the reader's buffer for as long as it liked. Both ends are processes this tool
/// started, so this is depth rather than a hole; the failure it actually guards is our own, a core
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
}
