use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::step::Step;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Ok,
    Fail,
    Skip,
}

impl Status {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Fail => "FAIL",
            Self::Skip => "skip",
        }
    }
}

/// What one step came to: its line, and what later steps read of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) name: String,
    pub(crate) status: Status,
    pub(crate) elapsed: Duration,
    pub(crate) log: Option<PathBuf>,
    /// The first lines of its own it printed: diagnostics, failures and results.
    pub(crate) first: Vec<String>,
    /// How many such lines it had in all.
    pub(crate) lines: usize,
    /// The time the driver spent around the step: git's trees, the step's identity, its pass.
    pub(crate) overhead: Duration,
    /// The crates cargo compiled rather than found fresh, once for each unit.
    pub(crate) compiled: Vec<String>,
    pub(crate) errors: usize,
    /// Nextest's or libtest's count of the tests.
    pub(crate) summary: String,
    pub(crate) note: String,
    /// It found code that does not compile, so no later step of its root builds.
    pub(crate) broken: bool,
    /// It passed on these inputs before, so it did not run.
    pub(crate) reused: bool,
}

impl Outcome {
    /// A step that did not run, as an earlier one failed.
    pub(crate) fn skipped(name: &str, note: String) -> Self {
        Self::not_run(name, Status::Skip, note)
    }

    /// A copy the driver made itself, which `copied` says came out, in `elapsed`, logged in `log`.
    pub(crate) fn copied(
        step: &Step,
        copied: std::io::Result<()>,
        elapsed: Duration,
        log: PathBuf,
    ) -> Self {
        let (status, note) = match copied {
            Ok(()) => (Status::Ok, String::new()),
            Err(error) => (Status::Fail, format!("{}: {error}", step.program_name())),
        };
        Self {
            elapsed,
            log: Some(log),
            ..Self::not_run(&step.name, status, note)
        }
    }

    /// A step that passed on these inputs before, so it did not run.
    pub(crate) fn reused(name: &str, note: String) -> Self {
        Self {
            reused: true,
            ..Self::not_run(name, Status::Ok, note)
        }
    }

    fn not_run(name: &str, status: Status, note: String) -> Self {
        Self {
            name: name.to_owned(),
            status,
            elapsed: Duration::ZERO,
            log: None,
            first: Vec::new(),
            lines: 0,
            overhead: Duration::ZERO,
            compiled: Vec::new(),
            errors: 0,
            summary: String::new(),
            note,
            broken: false,
            reused: false,
        }
    }

    /// `name  status  seconds  [log]  note`, with the log only for a failure, relative to `base`
    /// when it lies under it.
    pub(crate) fn line(&self, name_width: usize, base: &Path) -> String {
        let mut line = format!(
            "{:<name_width$}  {:<4}  {:>6.1} s",
            self.name,
            self.status.name(),
            self.elapsed.as_secs_f64()
        );
        if self.status == Status::Fail
            && let Some(log) = &self.log
        {
            line += "  ";
            line += &log.strip_prefix(base).unwrap_or(log).display().to_string();
        }
        if !self.note.is_empty() {
            line += "  ";
            line += &self.note;
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_names_the_log_of_a_failure_only() {
        let mut outcome = Outcome::reused("lint", "no input changed since it passed".to_owned());
        assert!(outcome.reused);
        let base = Path::new("/w");
        assert_eq!(
            outcome.line(6, base),
            "lint    ok       0.0 s  no input changed since it passed"
        );
        outcome.status = Status::Fail;
        outcome.elapsed = Duration::from_millis(12_345);
        outcome.log = Some(PathBuf::from("/w/target/drive/log/lint.log"));
        outcome.note = "2 errors".to_owned();
        assert_eq!(
            outcome.line(4, base),
            "lint  FAIL    12.3 s  target/drive/log/lint.log  2 errors"
        );
        assert_eq!(
            outcome.line(4, Path::new("/elsewhere")),
            "lint  FAIL    12.3 s  /w/target/drive/log/lint.log  2 errors"
        );
        let skipped = Outcome::skipped("tests", String::new());
        assert!(!skipped.reused);
        assert_eq!(skipped.status, Status::Skip);
    }
}
