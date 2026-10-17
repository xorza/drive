use std::path::PathBuf;
use std::time::Duration;

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
    /// The lines it printed above its own.
    pub(crate) printed: usize,
    /// The units cargo built rather than found fresh.
    pub(crate) built: usize,
    pub(crate) note: String,
    /// It found code that does not compile, so no later step of its root builds.
    pub(crate) broken: bool,
    /// It passed on these inputs before, so it did not run.
    pub(crate) reused: bool,
}

impl Outcome {
    pub(crate) fn skipped(name: &str, status: Status, note: String) -> Self {
        Self {
            name: name.to_owned(),
            status,
            elapsed: Duration::ZERO,
            log: None,
            printed: 0,
            built: 0,
            note,
            broken: false,
            reused: status == Status::Ok,
        }
    }

    /// `name  status  seconds  [log]  note`, with the log only for a failure.
    pub(crate) fn line(&self, name_width: usize) -> String {
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
            line += &log.display().to_string();
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
        let mut outcome = Outcome::skipped(
            "lint",
            Status::Ok,
            "no input changed since it passed".to_owned(),
        );
        assert!(outcome.reused);
        assert_eq!(
            outcome.line(6),
            "lint    ok       0.0 s  no input changed since it passed"
        );
        outcome.status = Status::Fail;
        outcome.elapsed = Duration::from_millis(12_345);
        outcome.log = Some(PathBuf::from("target/drive/log/lint.log"));
        outcome.note = "2 errors".to_owned();
        assert_eq!(
            outcome.line(4),
            "lint  FAIL    12.3 s  target/drive/log/lint.log  2 errors"
        );
        assert!(!Outcome::skipped("tests", Status::Skip, String::new()).reused);
    }
}
