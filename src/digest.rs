use std::path::Path;

use crate::outcome::{Outcome, Status};

/// The failed steps the verdict names before it counts the rest.
const NAMED: usize = 3;

/// The end of a run, for an agent that reads only its last lines: each failed step with the first
/// lines it printed, then the verdict, which names the file that holds the whole run.
#[derive(Debug)]
pub(crate) struct Digest<'r> {
    /// What ran: `check`, `gate`, `run lan`.
    pub(crate) command: &'r str,
    pub(crate) outcomes: &'r [Outcome],
    /// A driver error that ended the run.
    pub(crate) error: Option<&'r str>,
    /// The file that holds every line of the run.
    pub(crate) file: Option<&'r Path>,
    /// The folder the driver runs in, which paths are named from.
    pub(crate) base: &'r Path,
}

impl Digest<'_> {
    /// Nothing but the verdict for a run that failed nowhere.
    pub(crate) fn lines(&self) -> Vec<String> {
        let failed: Vec<&Outcome> = self
            .outcomes
            .iter()
            .filter(|o| o.status == Status::Fail)
            .collect();
        let mut lines = Vec::new();
        if !failed.is_empty() {
            let count = |status| self.outcomes.iter().filter(|o| o.status == status).count();
            let names: Vec<&str> = failed.iter().map(|o| o.name.as_str()).collect();
            let mut counts = vec![format!("failed: {}", names.join(", "))];
            for (n, word) in [
                (count(Status::Skip), "not run"),
                (count(Status::Ok), "passed"),
            ] {
                if n > 0 {
                    counts.push(format!("{n} {word}"));
                }
            }
            lines.push(String::new());
            lines.push(counts.join(" · "));
            let width = failed.iter().map(|o| o.name.len()).max().unwrap_or(0);
            for outcome in &failed {
                lines.push(outcome.line(width, self.base));
                lines.extend(
                    outcome
                        .first
                        .iter()
                        .map(|l| format!("  {}", l.trim_start())),
                );
                let more = outcome.lines - outcome.first.len();
                if more > 0 {
                    lines.push(format!("  … {more} more"));
                }
            }
        }
        lines.push(self.verdict(&failed));
        lines
    }

    /// `drive <command>: ok|FAIL|error — …`, with the first cause of a failure.
    fn verdict(&self, failed: &[&Outcome]) -> String {
        let file = self.file.map_or_else(String::new, |f| {
            format!(" ({})", f.strip_prefix(self.base).unwrap_or(f).display())
        });
        let head = format!("drive {}:", self.command);
        if let Some(error) = self.error {
            // A tool's error comes on the lines after the one that names the tool.
            let words: Vec<&str> = (error.lines().map(str::trim))
                .filter(|l| !l.is_empty())
                .collect();
            return format!("{head} error — {}{file}", words.join(" "));
        }
        let Some(first) = failed.first() else {
            let steps = self.outcomes.len();
            let plural = if steps == 1 { "" } else { "s" };
            let reused = self.outcomes.iter().filter(|o| o.reused).count();
            let reused = if reused > 0 {
                format!(", {reused} reused")
            } else {
                String::new()
            };
            return format!("{head} ok — {steps} step{plural}{reused}{file}");
        };
        let shown: Vec<&str> = failed.iter().take(NAMED).map(|o| o.name.as_str()).collect();
        let names = if failed.len() > NAMED {
            format!("{} +{} more", shown.join(", "), failed.len() - NAMED)
        } else {
            shown.join(", ")
        };
        let cause = first
            .first
            .first()
            .map_or(first.note.as_str(), |l| l.trim_start());
        let cause = if cause.is_empty() {
            "it printed nothing"
        } else {
            cause
        };
        format!("{head} FAIL {names} — {cause}{file}")
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    fn step(name: &str, status: Status) -> Outcome {
        Outcome {
            status,
            ..Outcome::skipped(name, String::new())
        }
    }

    fn failed(name: &str, first: &[&str], lines: usize, note: &str) -> Outcome {
        Outcome {
            elapsed: Duration::from_millis(300),
            log: Some(PathBuf::from(format!("/w/target/drive/log/{name}.log"))),
            first: first.iter().map(|&l| l.to_owned()).collect(),
            lines,
            note: note.to_owned(),
            ..step(name, Status::Fail)
        }
    }

    fn digest(outcomes: &[Outcome], error: Option<&str>) -> Vec<String> {
        Digest {
            command: "gate",
            outcomes,
            error,
            file: Some(Path::new("/w/target/drive/last-gate.txt")),
            base: Path::new("/w"),
        }
        .lines()
    }

    #[test]
    fn a_passing_run_ends_on_its_verdict_alone() {
        let reused = Outcome::reused("lint", String::new());
        assert_eq!(
            digest(&[step("fmt", Status::Ok), reused], None),
            ["drive gate: ok — 2 steps, 1 reused (target/drive/last-gate.txt)"]
        );
        assert_eq!(
            digest(&[step("fmt", Status::Ok)], None),
            ["drive gate: ok — 1 step (target/drive/last-gate.txt)"]
        );
    }

    #[test]
    fn a_failed_run_shows_each_failed_steps_first_lines_and_its_first_cause_last() {
        let lint = failed(
            "lint",
            &["src/lib.rs:2:9: error: unused variable: `unused`"],
            1,
            "--all-features; 1 error",
        );
        let tests = failed(
            "tests",
            &[
                "        FAIL [   0.003s] demo tests::adds",
                "        FAIL [   0.003s] demo tests::plain",
            ],
            14,
            "2 failed",
        );
        let outcomes = [
            step("fmt", Status::Ok),
            lint,
            step("ship", Status::Skip),
            tests,
        ];
        assert_eq!(
            digest(&outcomes, None),
            [
                "",
                "failed: lint, tests · 1 not run · 1 passed",
                "lint   FAIL     0.3 s  target/drive/log/lint.log  --all-features; 1 error",
                "  src/lib.rs:2:9: error: unused variable: `unused`",
                "tests  FAIL     0.3 s  target/drive/log/tests.log  2 failed",
                "  FAIL [   0.003s] demo tests::adds",
                "  FAIL [   0.003s] demo tests::plain",
                "  … 12 more",
                "drive gate: FAIL lint, tests — src/lib.rs:2:9: error: unused variable: `unused` \
                 (target/drive/last-gate.txt)",
            ]
        );
    }

    #[test]
    fn the_verdict_counts_past_three_failures_and_falls_back_to_the_note() {
        let outcomes: Vec<Outcome> = ["a", "b", "c", "d", "e"]
            .map(|n| failed(n, &[], 0, ""))
            .into_iter()
            .collect();
        let lines = digest(&outcomes, None);
        assert_eq!(
            lines.last().unwrap(),
            "drive gate: FAIL a, b, c +2 more — it printed nothing (target/drive/last-gate.txt)"
        );
        let quiet = failed("package", &[], 0, "copy: No such file or directory");
        assert_eq!(
            digest(&[quiet], None).last().unwrap(),
            "drive gate: FAIL package — copy: No such file or directory \
             (target/drive/last-gate.txt)"
        );
    }

    #[test]
    fn a_driver_error_is_the_verdict() {
        let lines = digest(
            &[step("fmt", Status::Ok)],
            Some("cargo metadata in /w:\nerror: failed to parse manifest"),
        );
        assert_eq!(
            lines,
            [
                "drive gate: error — cargo metadata in /w: error: failed to parse manifest \
                 (target/drive/last-gate.txt)"
            ]
        );
    }
}
