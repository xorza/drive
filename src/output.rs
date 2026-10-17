use std::collections::{BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::Deserialize;

use crate::step::Kind;

/// A step prints its first lines and its last: the summaries come last, nextest's list of the
/// failed tests among them.
const HEAD_LINES: usize = 150;
const TAIL_LINES: usize = 50;

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

/// Whether the driver runs in GitHub Actions, which prints each step's whole output in a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ci {
    No,
    GitHub,
}

/// What a step printed and counted, and how its process ended.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    /// The exit code, `None` when a signal ended the process.
    pub(crate) code: Option<i32>,
    pub(crate) printed: usize,
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

/// What a step prints: diagnostics, failures and results, not cargo's progress, and each error
/// diagnostic once in a command. Its log holds every line.
#[derive(Debug)]
struct Filter<'s, W: Write> {
    kind: Kind,
    ci: Ci,
    log: W,
    /// The error diagnostics the command printed, shared by its steps.
    shown: &'s mut BTreeSet<String>,
    /// A crate's library and its tests report one diagnostic twice.
    logged: BTreeSet<String>,
    last_blank: bool,
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
    fn new(kind: Kind, ci: Ci, log: W, shown: &'s mut BTreeSet<String>) -> Self {
        Self {
            kind,
            ci,
            log,
            shown,
            logged: BTreeSet::new(),
            last_blank: true,
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
        if self.ci == Ci::GitHub {
            println!("{}", raw.trim_end_matches(['\r', '\n']));
        } else if summary.is_none() && !is_hidden(&line, self.kind) {
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
        if self.ci == Ci::GitHub {
            println!("{rendered}");
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
        if self.shown.contains(&rendered) {
            self.report.repeated += 1;
        } else if self.ci == Ci::No {
            self.show(&rendered);
            self.shown.insert(rendered);
        }
    }

    fn show(&mut self, line: &str) {
        let blank = line.trim().is_empty();
        if blank && self.last_blank {
            return;
        }
        self.last_blank = blank;
        if self.report.printed < HEAD_LINES {
            println!("{line}");
            self.report.printed += 1;
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
            println!("... {omitted} more lines in {log_name}");
        }
        self.report.printed += self.tail.len();
        for line in self.tail.drain(..) {
            println!("{line}");
        }
        if self.kind.libtest() {
            self.report.summary = test_summary(self.results);
        }
        self.log.flush().ok();
        self.report
    }
}

/// Runs `command`, its output through the filter into `log`.
pub(crate) fn run(
    command: &mut Command,
    kind: Kind,
    ci: Ci,
    log: File,
    log_name: &str,
    shown: &mut BTreeSet<String>,
) -> std::io::Result<Report> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let filter = Mutex::new(Filter::new(kind, ci, BufWriter::new(log), shown));
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
            || (trimmed.starts_with("test ")
                && (trimmed.ends_with(" ... ok") || trimmed.contains(" ... ignored")))
            || trimmed.starts_with("test result: ok.")
            || trimmed.starts_with("all doctests ran in "));
    progress || nextest || tally || libtest
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

    fn feed(
        kind: Kind,
        shown: &mut BTreeSet<String>,
        stdout: &[&str],
        stderr: &[&str],
    ) -> (Report, String) {
        let mut log = Vec::new();
        let report = {
            let mut filter = Filter::new(kind, Ci::No, &mut log, shown);
            for l in stdout {
                filter.stdout(l);
            }
            for l in stderr {
                filter.line(l);
            }
            filter.finish("x.log")
        };
        (report, String::from_utf8(log).unwrap())
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
        let mut shown = BTreeSet::new();
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
        let (report, log) = feed(Kind::Check, &mut shown, &stdout, &stderr);
        assert_eq!(
            report,
            Report {
                code: None,
                printed: 2,
                compiled: vec!["a".to_owned(), "b".to_owned()],
                errors: 2,
                hard: 1,
                repeated: 0,
                summary: String::new()
            }
        );
        assert!(log.contains("warning: unused") && log.contains("Compiling a"));

        // A later step of the command counts the same errors but does not print them again.
        let (again, _) = feed(Kind::Check, &mut shown, &[lint.as_str()], &[]);
        assert_eq!((again.errors, again.repeated, again.printed), (1, 1, 0));
    }

    #[test]
    fn keeps_nextest_summary_and_failures() {
        let mut shown = BTreeSet::new();
        let stderr = [
            "    Starting 3 tests across 2 binaries",
            "        FAIL [   0.010s] core tests::fails",
            "──────────",
            "     Summary [   0.020s] 3 tests run: 2 passed, 1 failed",
            "error: test run failed",
        ];
        let (report, _) = feed(Kind::Nextest, &mut shown, &[], &stderr);
        assert_eq!(report.summary, "3 tests run: 2 passed, 1 failed");
        assert_eq!(report.printed, 1);
    }

    #[test]
    fn sums_doc_tests_and_hides_their_passes() {
        let mut shown = BTreeSet::new();
        let stdout = [
            "   Doc-tests core",
            "running 2 tests",
            "test src/lib.rs - f (line 3) ... ok",
            "test src/lib.rs - g (line 9) ... FAILED",
            "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s",
        ];
        let (report, _) = feed(Kind::Doctests, &mut shown, &stdout, &[]);
        assert_eq!(report.summary, "1 passed, 1 failed");
        assert_eq!(report.printed, 2);
    }

    #[test]
    fn prints_the_head_and_the_tail() {
        let mut shown = BTreeSet::new();
        let lines: Vec<String> = (0..300).map(|i| format!("line {i}")).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (report, log) = feed(Kind::Program, &mut shown, &lines, &[]);
        assert_eq!(report.printed, HEAD_LINES + TAIL_LINES);
        assert_eq!(log.lines().count(), 300);
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
