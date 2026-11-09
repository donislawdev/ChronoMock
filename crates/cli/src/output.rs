//! What the tool writes for its caller: the answer on standard output, everything else on standard
//! error - and neither of them may crash it (R4-S11).
//!
//! `println!` and `eprintln!` panic when a write fails, and a pipe whose reader has gone fails every
//! write. Measured before this module existed: `chrono version`, `chrono calc` and a `chrono run`
//! with nobody reading ended in a panic and exit code 101, which no table documents, and the core
//! wrote its diagnostics through the same macros - so a panel that died took the core down in the
//! middle of `close_session`, before it put the family back on rate 1 (ADR-14).
//!
//! The rule is the one docs/08 already had for `--report`: an output that cannot be written is said
//! on standard error where that is still possible, and the exit code stays what the command found.
//! `tests/hygiene.rs` (H10) keeps the panicking macros out of the shipped code.

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the first write to standard output that failed. Nothing is written there after it, so a
/// session whose reader left keeps collecting its events without trying every one of them again.
static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);

/// Text on standard output, or nothing once standard output has failed. The first failure is said
/// once on standard error. Use it through `out!` and `outln!`.
pub(crate) fn to_stdout(args: fmt::Arguments<'_>, newline: bool) {
    if STDOUT_CLOSED.load(Ordering::Relaxed) {
        return;
    }
    let written = write_through(&mut io::stdout().lock(), args, newline);
    if let Err(e) = written
        && !STDOUT_CLOSED.swap(true, Ordering::Relaxed)
    {
        to_stderr(format_args!(
            "chrono: could not write to standard output ({e}) - the rest of the output was dropped, and the exit code still reports the result"
        ));
    }
}

/// One line on standard error, or nothing when that fails: a diagnostic nobody can read is no reason
/// to stop the work it describes. Use it through `diag!`.
pub(crate) fn to_stderr(args: fmt::Arguments<'_>) {
    let _ = write_through(&mut io::stderr().lock(), args, true);
}

/// The stream a text that can be either an answer or a diagnostic is written to. A usage is the answer
/// when it was asked for, so `chrono --help | more` shows it, and beside a refusal it explains the
/// refusal, on standard error with standard output left empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stream {
    Out,
    Err,
}

/// One line on `stream`, through the same writers as `outln!` and `diag!`. Use it through `say!`.
pub(crate) fn to_stream(stream: Stream, args: fmt::Arguments<'_>) {
    match stream {
        Stream::Out => to_stdout(args, true),
        Stream::Err => to_stderr(args),
    }
}

/// Flushed on every call, because a line buffer holding the last of the answer would otherwise fail
/// at exit, where nobody is left to say so.
fn write_through(sink: &mut impl Write, args: fmt::Arguments<'_>, newline: bool) -> io::Result<()> {
    sink.write_fmt(args)?;
    if newline {
        sink.write_all(b"\n")?;
    }
    sink.flush()
}

/// `eprintln!` that cannot panic.
macro_rules! diag {
    ($($arg:tt)*) => {
        $crate::output::to_stderr(format_args!($($arg)*))
    };
}

/// `println!` that cannot panic.
macro_rules! outln {
    ($($arg:tt)*) => {
        $crate::output::to_stdout(format_args!($($arg)*), true)
    };
}

/// `print!` that cannot panic.
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::output::to_stdout(format_args!($($arg)*), false)
    };
}

/// `outln!` or `diag!`, chosen by the `Stream` given first.
macro_rules! say {
    ($stream:expr, $($arg:tt)*) => {
        $crate::output::to_stream($stream, format_args!($($arg)*))
    };
}

pub(crate) use {diag, out, outln, say};

#[cfg(test)]
mod tests {
    use super::*;

    /// A sink that refuses every write, as a pipe does once its reader has gone.
    struct Gone;

    impl Write for Gone {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn a_failed_write_is_returned_rather_than_raised() {
        let failed = write_through(&mut Gone, format_args!("verdict: {}", "works"), true);
        assert_eq!(failed.map_err(|e| e.kind()), Err(io::ErrorKind::BrokenPipe));
    }

    #[test]
    fn a_line_is_the_text_and_one_newline() {
        let mut sink = Vec::new();
        write_through(&mut sink, format_args!("{} {}", "a", 1), true).expect("a vector takes every write");
        write_through(&mut sink, format_args!("b"), false).expect("a vector takes every write");
        assert_eq!(sink, b"a 1\nb");
    }
}
