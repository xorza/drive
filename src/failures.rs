//! The failed tests of one nextest run, each with where and why it panicked: what a reader looks
//! for first in a failed run, which nextest prints apart, the status as a test ends and its
//! output after the summary.

/// The lines of a panic a failure keeps: where and the message's first line on one, then the
/// next, as an assertion's `left` and `right`.
const PANIC_LINES: usize = 3;
/// The characters a panic's line keeps: one value's `Debug` can fill thousands, which the log
/// keeps whole.
const LINE_CHARS: usize = 300;

/// Nextest's lines of the tests that failed, read as the run prints them.
#[derive(Debug, Default)]
pub(crate) struct Failures {
    tests: Vec<Failure>,
    /// Nextest printed its summary: what follows is its final report, each failure's status line
    /// again with its output.
    reported: bool,
    /// The failure whose output the final report prints now.
    reading: Option<Reading>,
}

#[derive(Debug)]
struct Failure {
    /// Its status line as the test ended: `TIMEOUT [   5.009s] core tests::waits`.
    status: String,
    panic: Vec<String>,
}

#[derive(Debug)]
struct Reading {
    at: usize,
    panic: Panic,
}

/// How far a failure's output is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panic {
    /// No panic yet.
    Before,
    /// The line that says where it panicked, which the message's first line joins.
    Where,
    Message,
    Read,
}

impl Failures {
    /// Nextest printed its summary.
    pub(crate) const fn summary(&mut self) {
        self.reported = true;
        self.reading = None;
    }

    /// Takes one of the run's own lines; `true` for a line of the tests, which only the
    /// failures keep: a status line, a notice as `TERMINATING`, and a test's output.
    pub(crate) fn line(&mut self, line: &str) -> bool {
        let trimmed = line.trim();
        if let Some(word) = status(trimmed) {
            if self.reported {
                let at = self.tests.iter().position(|test| test.status == trimmed);
                self.reading = at.map(|at| Reading {
                    at,
                    panic: Panic::Before,
                });
            } else if ends_failed(word) {
                self.tests.push(Failure {
                    status: trimmed.to_owned(),
                    panic: Vec::new(),
                });
            }
            return true;
        }
        if !self.reported {
            return false;
        }
        if let Some(reading) = &mut self.reading {
            reading.panic = read(&mut self.tests[reading.at].panic, reading.panic, trimmed);
        }
        true
    }

    /// Each failure's status line, then where and why it panicked, in the order the tests ended.
    pub(crate) fn lines(&self) -> impl Iterator<Item = &str> {
        (self.tests.iter())
            .flat_map(|test| std::iter::once(&test.status).chain(&test.panic))
            .map(String::as_str)
    }
}

/// The state after `line` of a failure's output, whose panic so far is `panic`.
fn read(panic: &mut Vec<String>, state: Panic, line: &str) -> Panic {
    match state {
        Panic::Before => {
            let place = (line.strip_prefix("thread '"))
                .and_then(|rest| rest.split_once(" panicked at "))
                .map(|(_, place)| place);
            let Some(place) = place else {
                return Panic::Before;
            };
            panic.push(clipped(&format!("panicked at {place}")));
            Panic::Where
        }
        Panic::Message | Panic::Where
            if line.is_empty()
                || line.starts_with("note: run with `RUST_BACKTRACE")
                || line.starts_with("stack backtrace:") =>
        {
            Panic::Read
        }
        Panic::Where => {
            let place = panic.pop().expect("the line that says where");
            panic.push(clipped(&format!("{place} {line}")));
            Panic::Message
        }
        Panic::Message => {
            panic.push(clipped(line));
            if panic.len() < PANIC_LINES {
                Panic::Message
            } else {
                Panic::Read
            }
        }
        Panic::Read => Panic::Read,
    }
}

fn clipped(line: &str) -> String {
    match line.char_indices().nth(LINE_CHARS) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_owned(),
    }
}

/// The word of a nextest status line, `FAIL` in `FAIL [   0.003s] core tests::adds`.
fn status(line: &str) -> Option<&str> {
    let (word, rest) = line.split_once(" [")?;
    let word_like = !word.is_empty()
        && (word.bytes())
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b"- ".contains(&b));
    (word_like && rest.contains("] ")).then_some(word)
}

/// A status that ends a test failed, not a notice as `TERMINATING` or `SLOW`, nor a try that a
/// retry follows.
fn ends_failed(word: &str) -> bool {
    matches!(word, "FAIL" | "TIMEOUT" | "ABORT" | "LEAK-FAIL") || word.starts_with("SIG")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run of nextest 0.9.131 with `--status-level fail --final-status-level slow
    /// --failure-output final`: two failures, an abort and a timeout.
    const RUN: &str = "\
        FAIL [   0.003s] nx tests::fails
        FAIL [   0.004s] nx tests::panics
     SIGABRT [   0.103s] nx tests::aborts
 TERMINATING [>  2.000s] nx tests::hangs
     TIMEOUT [   2.002s] nx tests::hangs
     Summary [   2.002s] 5 tests run: 1 passed, 3 failed, 1 timed out, 0 skipped
        SLOW [   1.468s] nx tests::slow
     SIGABRT [   0.103s] nx tests::aborts
  stdout ───

    running 1 test

    (test aborted with signal 6: SIGABRT)

        FAIL [   0.003s] nx tests::fails
  stdout ───

    running 1 test
    test tests::fails ... FAILED

  stderr ───

    thread 'tests::fails' (2824) panicked at src/lib.rs:8:9:
    assertion `left == right` failed: the sum is wrong
      left: 2
     right: 3
    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

     TIMEOUT [   2.002s] nx tests::hangs
  stdout ───

    running 1 test

    (test timed out)

        FAIL [   0.004s] nx tests::panics
  stderr ───

    thread 'tests::panics' (2823) panicked at src/lib.rs:12:9:
    a plain panic
    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
";

    #[test]
    fn keeps_each_failure_as_it_ended_with_where_and_why_it_panicked() {
        let mut failures = Failures::default();
        let mut taken = 0;
        for line in RUN.lines() {
            if line.trim_start().starts_with("Summary [") {
                failures.summary();
            } else if failures.line(line) {
                taken += 1;
            }
        }
        // `TERMINATING` and `SLOW` are notices; a timeout keeps the line it ended on. An
        // assertion keeps its first three lines, `right` past them, and a test with no panic
        // keeps its status alone.
        assert_eq!(
            failures.lines().collect::<Vec<_>>(),
            [
                "FAIL [   0.003s] nx tests::fails",
                "panicked at src/lib.rs:8:9: assertion `left == right` failed: the sum is wrong",
                "left: 2",
                "right: 3",
                "FAIL [   0.004s] nx tests::panics",
                "panicked at src/lib.rs:12:9: a plain panic",
                "SIGABRT [   0.103s] nx tests::aborts",
                "TIMEOUT [   2.002s] nx tests::hangs",
            ]
        );
        // Every line but the summary is the tests'.
        assert_eq!(taken, RUN.lines().count() - 1);
    }

    #[test]
    fn a_line_before_the_summary_that_is_no_status_stays_the_run_s() {
        let mut failures = Failures::default();
        assert!(!failures.line("error: command `cargo test --no-run` exited with code 101"));
        assert!(!failures.line("     Summary is no status"));
        assert!(failures.line("        FAIL [   0.003s] a tests::b"));
        assert_eq!(status("TRY 2 FAIL [   0.1s] a b"), Some("TRY 2 FAIL"));
        assert!(!ends_failed("TRY 2 FAIL") && ends_failed("SIGSEGV"));
        assert_eq!(status("LEAK-FAIL [   0.1s] a b"), Some("LEAK-FAIL"));
        assert_eq!(status("[>  1.000s] a b"), None);
        failures.summary();
        assert!(failures.line("    stray output of no failure"));
        assert_eq!(
            failures.lines().collect::<Vec<_>>(),
            ["FAIL [   0.003s] a tests::b"]
        );
        let long = "x".repeat(LINE_CHARS + 1);
        assert_eq!(clipped(&long), format!("{}…", &long[..LINE_CHARS]));
        assert_eq!(clipped(&long[1..]), long[1..]);
    }
}
