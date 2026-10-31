//! The process's two streams (RFC-0012): results on stdout, everything else on stderr.

use std::fmt::Arguments;
use std::io::{ErrorKind, Write};

use serde_json::Value;

use crate::error::Failure;

/// Writes to the streams `run` was given. Once stdout has failed, later output is dropped and the
/// work goes on, so `exec … --save | head -1` keeps the edit. A reader that stopped early (a
/// closed pipe) is not an error, as in ripgrep; any other write error is reported and makes the
/// exit status 1.
pub struct Io<'a> {
    out: &'a mut dyn Write,
    err: &'a mut dyn Write,
    stdout_gone: bool,
    stdout_error: Option<std::io::Error>,
}

impl<'a> Io<'a> {
    pub fn new(out: &'a mut dyn Write, err: &'a mut dyn Write) -> Self {
        Io { out, err, stdout_gone: false, stdout_error: None }
    }

    fn stdout_result(&mut self, r: std::io::Result<()>) {
        if let Err(e) = r
            && !std::mem::replace(&mut self.stdout_gone, true)
            && e.kind() != ErrorKind::BrokenPipe
        {
            self.stdout_error = Some(e);
        }
    }

    /// A line of the result.
    pub fn line(&mut self, args: Arguments) {
        if !self.stdout_gone {
            let r = writeln!(self.out, "{args}");
            self.stdout_result(r);
        }
    }

    /// Text of the result, as is.
    pub fn text(&mut self, s: &str) {
        if !self.stdout_gone {
            let r = self.out.write_all(s.as_bytes());
            self.stdout_result(r);
        }
    }

    /// JSON result, pretty-printed unless `compact`.
    pub fn json(&mut self, compact: bool, v: &Value) {
        let s = if compact { serde_json::to_string(v) } else { serde_json::to_string_pretty(v) };
        self.line(format_args!("{}", s.unwrap_or_default()));
    }

    /// A line on stderr; a stderr that is gone must not turn a finished export into a crash.
    pub fn diag(&mut self, args: Arguments) {
        let _ = writeln!(self.err, "{args}");
    }

    /// Text on stderr, as is.
    pub fn diag_text(&mut self, s: &str) {
        let _ = self.err.write_all(s.as_bytes());
    }

    pub fn warning(&mut self, args: Arguments) {
        self.diag(format_args!("warning: {args}"));
    }

    /// Why stdout failed, if it did for a reason other than a closed reader.
    pub fn take_stdout_error(&mut self) -> Option<Failure> {
        self.stdout_error.take().map(Failure::Stdout)
    }
}
