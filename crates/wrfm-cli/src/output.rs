//! Broken-pipe-tolerant stdout / stderr.
//!
//! Rust's `print!` family panics when a write fails, and std sets `SIGPIPE`
//! to `SIG_IGN` on Unix — so a consumer that closes the pipe early (the
//! documented `wrfm render m.wrfm | head` recipe, `jq` exiting after the
//! first match, …) turned into
//!
//! ```text
//! thread 'main' panicked: failed printing to stdout: Broken pipe (os error 32)
//! ```
//!
//! with exit code 101 — outside the documented 0/1/2/3 contract.
//!
//! Every write goes through [`stdout`] / [`stderr`] instead: the first
//! `BrokenPipe` marks the stream closed and later writes become no-ops, so
//! the command still finishes and its exit code still carries the verdict.
//! Any OTHER write error keeps the old loud behaviour (panic) — a full disk
//! is a real failure, not a consumer walking away.

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);
static STDERR_CLOSED: AtomicBool = AtomicBool::new(false);

/// Write to stdout, ignoring a consumer that closed the pipe.
pub fn stdout(args: fmt::Arguments<'_>) {
    write_or_ignore(&mut io::stdout().lock(), args, &STDOUT_CLOSED, "stdout");
}

/// Write to stderr, ignoring a consumer that closed the pipe.
pub fn stderr(args: fmt::Arguments<'_>) {
    write_or_ignore(&mut io::stderr().lock(), args, &STDERR_CLOSED, "stderr");
}

/// Flush stdout unless the consumer already went away (the commands flush
/// before `std::process::exit`, which skips destructors).
pub fn flush() {
    if !STDOUT_CLOSED.load(Ordering::Relaxed) {
        let _ = io::stdout().flush();
    }
}

/// The whole policy, in one place: `BrokenPipe` closes the stream silently,
/// every other error is fatal (the caller is a CLI, there is no recovery).
fn write_or_ignore<W: Write>(
    w: &mut W,
    args: fmt::Arguments<'_>,
    closed: &AtomicBool,
    stream: &str,
) {
    if closed.load(Ordering::Relaxed) {
        return;
    }
    if let Err(e) = w.write_fmt(args) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            // The consumer is gone: drop the rest of the output, keep going
            // so the verdict still reaches the exit code.
            closed.store(true, Ordering::Relaxed);
            return;
        }
        panic!("failed printing to {stream}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    /// A writer that fails with `kind` on every write.
    struct Failing(io::ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(self.0, "injected"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn broken_pipe_closes_the_stream_without_panicking() {
        let closed = AtomicBool::new(false);
        let mut w = Failing(io::ErrorKind::BrokenPipe);
        write_or_ignore(&mut w, format_args!("first"), &closed, "stdout");
        assert!(
            closed.load(Ordering::Relaxed),
            "the stream is marked closed"
        );
        // A second write is a no-op (it must not reach the failing writer at
        // all — a panicking one would still fail here).
        struct Explodes;
        impl Write for Explodes {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                panic!("closed stream must not be written to");
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        write_or_ignore(&mut Explodes, format_args!("second"), &closed, "stdout");
    }

    #[test]
    #[should_panic(expected = "failed printing to stdout")]
    fn other_write_errors_stay_fatal() {
        let closed = AtomicBool::new(false);
        let mut w = Failing(io::ErrorKind::StorageFull);
        write_or_ignore(&mut w, format_args!("boom"), &closed, "stdout");
    }

    #[test]
    fn a_healthy_writer_is_untouched() {
        let closed = AtomicBool::new(false);
        let mut buf: Vec<u8> = Vec::new();
        write_or_ignore(&mut buf, format_args!("hello"), &closed, "stdout");
        assert_eq!(buf, b"hello");
        assert!(!closed.load(Ordering::Relaxed));
    }
}
