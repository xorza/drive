use std::collections::{BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::Deserialize;

use crate::ci::Ci;
use crate::console::Console;
use crate::step::Kind;

/// A step prints its first lines and its last: the summaries come last, nextest's list of the
/// failed tests among them.
const HEAD_LINES: usize = 150;
const TAIL_LINES: usize = 50;
/// The lines of its own a failed step shows again at the end of the run: the first ones, as a
/// tool prints a failure's name and its first error first.
pub(crate) const FIRST_LINES: usize = 10;

/// Cargo's progress, which a step does not print.
const PROGRESS: [&str; 15] = [
    "Compiling",
    "Checking",
    "Documenting",
    "Finished",
    "Fresh",
    "Blocking",
    "Downloading",
    "Downloaded",
    "Updating",
    "Locking",
    "Adding",
    "Building",
    "Generated",
    "Running",
    "Scraping",
];

/// What a step printed and counted, and how its process ended.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    /// The exit code, `None` when a signal ended the process.
    pub(crate) code: Option<i32>,
    /// The first lines of its own it printed, or would print outside CI: diagnostics, failures and
    /// results, without blank lines and without the errors an earlier step printed.
    pub(crate) first: Vec<String>,
    /// How many such lines it had in all.
    pub(crate) lines: usize,
    /// The crates cargo compiled rather than found fresh, once for each unit.
    pub(crate) compiled: Vec<String>,
    pub(crate) errors: usize,
    /// Errors that are no lint: the code does not compile.
    pub(crate) hard: usize,
    /// Errors an earlier step of the command printed.
    pub(crate) repeated: usize,
    /// Nextest's count of the tests, or the doc tests' summed results.
    pub(crate) summary: String,
}

impl Report {
    /// The process ended well and reported no error.
    pub(crate) fn passed(&self) -> bool {
        self.code == Some(0) && self.errors == 0
    }

    /// What the step's line adds after `label`, its flags: the tests' count, the errors, and for
    /// a failure that counts neither, how the process ended.
    pub(crate) fn note(&self, label: &str) -> String {
        let mut notes: Vec<String> = [label, self.summary.as_str()]
            .into_iter()
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .collect();
        if self.errors > 0 {
            let plural = if self.errors == 1 { "" } else { "s" };
            let repeated = if self.repeated > 0 {
                format!(", {} shown above", self.repeated)
            } else {
                String::new()
            };
            notes.push(format!("{} error{plural}{repeated}", self.errors));
        } else if !self.passed() && self.summary.is_empty() {
            notes.push(self.code.map_or_else(
                || "ended by a signal".to_owned(),
                |code| format!("exit code {code}"),
            ));
        }
        notes.join("; ")
    }
}

/// What a step prints: diagnostics, failures and results, not cargo's progress, and each error
/// diagnostic once in a command. Its log holds every line but libtest's passes.
#[derive(Debug)]
struct Filter<'s, W: Write> {
    kind: Kind,
    log: W,
    console: &'s mut Console,
    /// A crate's library and its tests report one diagnostic twice.
    logged: BTreeSet<String>,
    last_blank: bool,
    printed: usize,
    hidden: usize,
    tail: VecDeque<String>,
    /// The summed counts of libtest's `test result:` lines.
    results: [u64; 4],
    report: Report,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
enum Event {
    CompilerMessage {
        message: Diagnostic,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct Diagnostic {
    message: String,
    level: String,
    rendered: Option<String>,
    code: Option<Code>,
}

#[derive(Debug, Deserialize)]
struct Code {
    code: String,
}

impl<'s, W: Write> Filter<'s, W> {
    fn new(kind: Kind, log: W, console: &'s mut Console) -> Self {
        Self {
            kind,
            log,
            console,
            logged: BTreeSet::new(),
            last_blank: true,
            printed: 0,
            hidden: 0,
            tail: VecDeque::new(),
            results: [0; 4],
            report: Report::default(),
        }
    }

    fn stdout(&mut self, raw: &str) {
        if self.kind.diagnoses() && raw.starts_with('{') {
            match serde_json::from_str::<Event>(raw) {
                Ok(Event::CompilerMessage { message }) => return self.diagnostic(message),
                Ok(Event::Other) => return,
                Err(_) => {}
            }
        }
        self.line(raw);
    }

    fn line(&mut self, raw: &str) {
        let line = strip_ansi(raw.trim_end_matches(['\r', '\n']));
        // Thousands of them would bury a failure in the log, and the counts keep them.
        if self.kind.libtest() && is_pass(&line) {
            return;
        }
        writeln!(self.log, "{line}").ok();
        let trimmed = line.trim_start();
        let first = trimmed.split_whitespace().next().unwrap_or("");
        if matches!(first, "Compiling" | "Checking" | "Documenting")
            && let Some(name) = trimmed.split_whitespace().nth(1)
        {
            self.report.compiled.push(name.to_owned());
        }
        if self.kind.diagnoses()
            && trimmed.starts_with("= note: `cargo::")
            && trimmed.contains("is set to `warn`")
        {
            // One of cargo's own lints, which `-D warnings` does not reach.
            self.report.errors += 1;
        }
        let summary = trimmed
            .strip_prefix("Summary [")
            .and_then(|r| r.split_once("] "))
            .map(|(_, s)| s.to_owned());
        if self.kind.libtest() {
            add_test_result(&mut self.results, &line);
        }
        let own = summary.is_none() && !is_hidden(&line, self.kind);
        if own {
            self.keep(&line);
        }
        if self.console.ci() == Ci::GitHub {
            self.console.line(raw.trim_end_matches(['\r', '\n']));
        } else if own {
            self.show(&line);
        }
        if let Some(summary) = summary {
            self.report.summary = summary;
        }
    }

    fn diagnostic(&mut self, diagnostic: Diagnostic) {
        let rendered = diagnostic.rendered.unwrap_or(diagnostic.message);
        let rendered = rendered.trim_end_matches('\n').to_owned();
        if !self.logged.insert(rendered.clone()) {
            return;
        }
        writeln!(self.log, "{rendered}").ok();
        if self.console.ci() == Ci::GitHub {
            self.console.line(&rendered);
        }
        // The lints deny every warning, which comes back as an error.
        if !diagnostic.level.starts_with("error") {
            return;
        }
        self.report.errors += 1;
        let lint = diagnostic.code.is_some_and(|c| !is_error_code(&c.code));
        if !lint {
            self.report.hard += 1;
        }
        if self.console.has_shown(&rendered) {
            self.report.repeated += 1;
            return;
        }
        for line in rendered.lines() {
            self.keep(line);
        }
        if self.console.ci() == Ci::No {
            self.show(&rendered);
        }
        self.console.mark_shown(rendered);
    }

    fn keep(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.report.first.len() < FIRST_LINES {
            self.report.first.push(line.to_owned());
        }
        self.report.lines += 1;
    }

    fn show(&mut self, line: &str) {
        let blank = line.trim().is_empty();
        if blank && self.last_blank {
            return;
        }
        self.last_blank = blank;
        if self.printed < HEAD_LINES {
            self.console.line(line);
            self.printed += 1;
        } else {
            if self.tail.len() == TAIL_LINES {
                self.tail.pop_front();
            }
            self.tail.push_back(line.to_owned());
            self.hidden += 1;
        }
    }

    fn finish(mut self, log_name: &str) -> Report {
        let omitted = self.hidden - self.tail.len();
        if omitted > 0 {
            self.console
                .line(&format!("... {omitted} more lines in {log_name}"));
        }
        for line in self.tail.drain(..) {
            self.console.line(&line);
        }
        if self.kind.libtest() {
            self.report.summary = test_summary(self.results);
        }
        self.log.flush().ok();
        self.report
    }
}

/// What a step's output is, and the log it goes to.
#[derive(Debug)]
pub(crate) struct Capture<'n> {
    pub(crate) kind: Kind,
    pub(crate) log: File,
    /// The log as a line names it.
    pub(crate) log_name: &'n str,
}

impl Capture<'_> {
    /// Runs `command`, its output through the filter into the log.
    pub(crate) fn run(
        self,
        command: &mut Command,
        console: &mut Console,
    ) -> std::io::Result<Report> {
        let Self {
            kind,
            log,
            log_name,
        } = self;
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let filter = Mutex::new(Filter::new(kind, BufWriter::new(log), console));
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for raw in BufReader::new(stdout).lines().map_while(Result::ok) {
                    filter.lock().expect("no reader panics").stdout(&raw);
                }
            });
            for raw in BufReader::new(stderr).lines().map_while(Result::ok) {
                filter.lock().expect("no reader panics").line(&raw);
            }
        });
        let status = child.wait()?;
        let mut report = filter
            .into_inner()
            .expect("no reader panics")
            .finish(log_name);
        report.code = status.code();
        Ok(report)
    }
}

/// Whether a line is noise a step leaves to its log: cargo's progress and tallies, nextest's
/// headers, and the doc tests' passes.
fn is_hidden(line: &str, kind: Kind) -> bool {
    let trimmed = line.trim_start();
    let first = trimmed.split_whitespace().next().unwrap_or("");
    let progress = PROGRESS.contains(&first) && trimmed.len() > first.len();
    let nextest = trimmed.starts_with("Nextest run ID ")
        || (trimmed.starts_with("Starting ") && trimmed.contains(" test"))
        || (trimmed.starts_with("[>") && trimmed.contains("s] "))
        || (!trimmed.is_empty() && trimmed.chars().all(|c| c == '─'));
    let tally = (line.starts_with("warning: `")
        && line.contains(" generated ")
        && line.contains(" warning"))
        || (line.starts_with("error: could not compile `") && line.contains(" due to "))
        || line == "warning: build failed, waiting for other jobs to finish..."
        || line == "error: test run failed";
    let libtest = kind.libtest()
        && (trimmed.starts_with("Doc-tests ")
            || trimmed.starts_with("running ")
            || is_pass(line)
            || trimmed.starts_with("test result: ok.")
            || trimmed.starts_with("all doctests ran in "));
    progress || nextest || tally || libtest
}

/// Libtest's line for a test that passed or did not run.
fn is_pass(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("test ")
        && (trimmed.ends_with(" ... ok") || trimmed.contains(" ... ignored"))
}

/// Rustc's code of an error that is no lint, `E0308`; a lint's code is its name.
fn is_error_code(code: &str) -> bool {
    code.len() == 5 && code.starts_with('E') && code[1..].bytes().all(|b| b.is_ascii_digit())
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.clone().next() == Some('[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Adds a `test result:` line's passed, failed, ignored and filtered counts to `totals`.
fn add_test_result(totals: &mut [u64; 4], line: &str) {
    let Some((_, result)) = line.split_once("test result: ") else {
        return;
    };
    // `ok. 3 passed; 0 failed; …`: the status, then one `<count> <label>` per part.
    let Some((_, counts)) = result.split_once(". ") else {
        return;
    };
    for part in counts.split("; ") {
        let Some((count, label)) = part.split_once(' ') else {
            continue;
        };
        let Ok(count) = count.parse::<u64>() else {
            continue;
        };
        let slot = match label {
            "passed" => 0,
            "failed" => 1,
            "ignored" => 2,
            "filtered out" => 3,
            _ => continue,
        };
        totals[slot] += count;
    }
}

fn test_summary([passed, failed, ignored, filtered]: [u64; 4]) -> String {
    let mut parts = vec![format!("{passed} passed")];
    for (count, word) in [
        (failed, "failed"),
        (ignored, "ignored"),
        (filtered, "filtered out"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {word}"));
        }
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The report, the log, and what the console printed of this step.
    fn feed(
        kind: Kind,
        console: &mut Console,
        stdout: &[&str],
        stderr: &[&str],
    ) -> (Report, String, String) {
        let before = console.text().len();
        let mut log = Vec::new();
        let report = {
            let mut filter = Filter::new(kind, &mut log, console);
            for l in stdout {
                filter.stdout(l);
            }
            for l in stderr {
                filter.line(l);
            }
            filter.finish("x.log")
        };
        (
            report,
            String::from_utf8(log).unwrap(),
            console.text()[before..].to_owned(),
        )
    }

    fn message(level: &str, code: Option<&str>, rendered: &str) -> String {
        let code = code.map_or_else(|| "null".to_owned(), |c| format!(r#"{{"code":"{c}"}}"#));
        format!(
            r#"{{"reason":"compiler-message","message":{{"message":"m","level":"{level}","rendered":"{rendered}\n","code":{code}}}}}"#
        )
    }

    #[test]
    fn counts_errors_once_and_tells_lints_from_hard_errors() {
        let lint = message(
            "error",
            Some("clippy::needless_return"),
            "src/a.rs:1:1: error: needless return",
        );
        let hard = message(
            "error",
            Some("E0308"),
            "src/b.rs:2:2: error[E0308]: mismatched types",
        );
        let warning = message("warning", Some("unused"), "src/c.rs:3:3: warning: unused");
        let artifact = r#"{"reason":"compiler-artifact","fresh":true}"#;
        let mut console = Console::new(Ci::No);
        let stdout = [
            lint.as_str(),
            lint.as_str(),
            hard.as_str(),
            warning.as_str(),
            artifact,
        ];
        let stderr = [
            "   Compiling a v0.1.0",
            "    Checking b v0.1.0",
            "warning: `a` (lib) generated 1 warning",
            "error: could not compile `a` (lib) due to 2 previous errors",
        ];
        let (report, log, printed) = feed(Kind::Check, &mut console, &stdout, &stderr);
        let first = [
            "src/a.rs:1:1: error: needless return",
            "src/b.rs:2:2: error[E0308]: mismatched types",
        ];
        assert_eq!(
            report,
            Report {
                code: None,
                first: first.map(str::to_owned).to_vec(),
                lines: 2,
                compiled: vec!["a".to_owned(), "b".to_owned()],
                errors: 2,
                hard: 1,
                repeated: 0,
                summary: String::new()
            }
        );
        assert_eq!(printed, format!("{}\n{}\n", first[0], first[1]));
        assert!(log.contains("warning: unused") && log.contains("Compiling a"));

        // A later step of the command counts the same errors but neither prints nor keeps them.
        let (again, _, printed) = feed(Kind::Check, &mut console, &[lint.as_str()], &[]);
        assert_eq!((again.errors, again.repeated, again.lines), (1, 1, 0));
        assert_eq!((again.first, printed), (Vec::new(), String::new()));
    }

    #[test]
    fn a_note_counts_the_errors_and_tells_how_a_silent_failure_ended() {
        let report = |code, errors, repeated, summary: &str| Report {
            code,
            errors,
            repeated,
            summary: summary.to_owned(),
            ..Report::default()
        };
        assert_eq!(
            report(Some(0), 0, 0, "").note("--all-features"),
            "--all-features"
        );
        assert!(report(Some(0), 0, 0, "").passed());
        assert_eq!(
            report(Some(101), 2, 1, "").note("--all-features"),
            "--all-features; 2 errors, 1 shown above"
        );
        assert_eq!(report(Some(1), 1, 0, "").note(""), "1 error");
        // A test run's count says what failed; a silent program says how it ended.
        assert_eq!(
            report(Some(101), 0, 0, "1 passed, 1 failed").note(""),
            "1 passed, 1 failed"
        );
        assert_eq!(report(Some(3), 0, 0, "").note(""), "exit code 3");
        assert_eq!(report(None, 0, 0, "").note("x"), "x; ended by a signal");
        assert!(!report(Some(0), 1, 0, "").passed());
    }

    #[test]
    fn keeps_nextest_summary_and_failures() {
        let mut console = Console::new(Ci::No);
        let stderr = [
            "    Starting 3 tests across 2 binaries",
            "        FAIL [   0.010s] core tests::fails",
            "──────────",
            "     Summary [   0.020s] 3 tests run: 2 passed, 1 failed",
            "error: test run failed",
        ];
        let (report, _, printed) = feed(Kind::Nextest, &mut console, &[], &stderr);
        assert_eq!(report.summary, "3 tests run: 2 passed, 1 failed");
        assert_eq!(report.first, ["        FAIL [   0.010s] core tests::fails"]);
        assert_eq!(printed, "        FAIL [   0.010s] core tests::fails\n");
    }

    #[test]
    fn sums_doc_tests_and_hides_their_passes() {
        let mut console = Console::new(Ci::No);
        let stdout = [
            "   Doc-tests core",
            "running 2 tests",
            "test src/lib.rs - f (line 3) ... ok",
            "test src/lib.rs - g (line 9) ... FAILED",
            "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s",
        ];
        let (report, log, _) = feed(Kind::Doctests, &mut console, &stdout, &[]);
        assert_eq!(report.summary, "1 passed, 1 failed");
        assert_eq!(report.lines, 2);
        // The log keeps the failure and leaves the pass out.
        assert!(log.contains("(line 9) ... FAILED"));
        assert!(!log.contains("(line 3) ... ok"));
    }

    #[test]
    fn prints_the_head_and_the_tail() {
        let mut console = Console::new(Ci::No);
        let lines: Vec<String> = (0..300).map(|i| format!("line {i}")).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (report, log, printed) = feed(Kind::Plain, &mut console, &lines, &[]);
        let printed: Vec<&str> = printed.lines().collect();
        // The head, the count of what it leaves out, the tail.
        assert_eq!(printed.len(), HEAD_LINES + 1 + TAIL_LINES);
        assert_eq!(printed[HEAD_LINES], "... 100 more lines in x.log");
        assert_eq!(printed[HEAD_LINES + 1], "line 250");
        assert_eq!(log.lines().count(), 300);
        assert_eq!((report.first.len(), report.lines), (FIRST_LINES, 300));
        assert_eq!(report.first[FIRST_LINES - 1], "line 9");
    }

    #[test]
    fn small_helpers() {
        assert_eq!(strip_ansi("\x1b[1;31merror\x1b[0m: x"), "error: x");
        assert!(is_error_code("E0308"));
        assert!(!is_error_code("clippy::x"));
        assert!(!is_error_code("E03"));
        assert!(is_hidden("   Compiling a v0.1.0", Kind::Check));
        assert!(!is_hidden("Compiling", Kind::Check));
        assert!(!is_hidden("test a ... ok", Kind::Nextest));
        assert!(is_hidden("test a ... ok", Kind::Doctests));
        assert!(is_hidden("test a ... ok", Kind::CargoTest));
        assert!(is_hidden("test a ... ignored, print-only", Kind::CargoTest));
        assert!(!is_hidden("test a ... FAILED", Kind::CargoTest));
        assert_eq!(test_summary([0, 0, 0, 0]), "0 passed");
        assert_eq!(
            test_summary([3, 0, 1, 4]),
            "3 passed, 1 ignored, 4 filtered out"
        );
    }
}
