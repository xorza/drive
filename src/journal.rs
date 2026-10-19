use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::outcome::Outcome;
use crate::output::Ci;
use crate::tool;

/// The run log: one JSON line for each command, with its steps, so the time of the builds, the
/// tests and the driver itself is measured, not assumed. One file a month, `2026-10.jsonl`.
#[derive(Debug)]
pub(crate) struct Journal {
    started: Instant,
    at: SystemTime,
    command: String,
    args: Vec<String>,
    ci: Ci,
    /// The time to load the project: `cargo metadata` for each root.
    load: Duration,
}

/// How a command ended.
#[derive(Debug)]
pub(crate) enum End {
    Passed,
    Failed,
    /// A driver error, with the steps that ran before it.
    Error(String),
}

/// A calendar date and time of day in UTC.
#[derive(Debug, PartialEq, Eq)]
struct Utc {
    year: i64,
    month: u32,
    day: u32,
    seconds: u64,
}

impl Journal {
    pub(crate) fn start(command: &str, args: &[String], ci: Ci) -> Self {
        Self {
            started: Instant::now(),
            at: SystemTime::now(),
            command: command.to_owned(),
            args: args.to_vec(),
            ci,
            load: Duration::ZERO,
        }
    }

    pub(crate) fn loaded(&mut self) {
        self.load = self.started.elapsed();
    }

    /// The command's line, appended to this month's file in `dir`.
    pub(crate) fn write(
        &self,
        dir: &Path,
        project: &Path,
        outcomes: &[Outcome],
        end: &End,
    ) -> std::io::Result<PathBuf> {
        let elapsed = self.started.elapsed();
        let since_epoch = self
            .at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let utc = Utc::from_unix(since_epoch.as_secs());
        let (code, error) = match end {
            End::Passed => (0, None),
            End::Failed => (1, None),
            End::Error(message) => (2, Some(message.as_str())),
        };
        let steps: Vec<serde_json::Value> = outcomes.iter().map(step).collect();
        let line = serde_json::json!({
            "at": utc.rfc3339(),
            "unix_ms": since_epoch.as_millis(),
            "project": project,
            "command": self.command,
            "args": self.args,
            "exit": code,
            "error": error,
            "seconds": seconds(elapsed),
            "load_seconds": seconds(self.load),
            "driver": tool::first_line(tool::command("git").args(["describe", "--always", "--dirty"]).current_dir(env!("CARGO_MANIFEST_DIR"))),
            "rustc": tool::first_line(tool::command("rustc").arg("-V").current_dir(project)),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cpus": std::thread::available_parallelism().map_or(0, usize::from),
            "ci": self.ci == Ci::GitHub,
            "steps": steps,
        });
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{:04}-{:02}.jsonl", utc.year, utc.month));
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        // One write of the whole line: runs beside this one append to the same file.
        file.write_all(format!("{line}\n").as_bytes())?;
        Ok(path)
    }
}

fn step(outcome: &Outcome) -> serde_json::Value {
    serde_json::json!({
        "name": outcome.name,
        "status": outcome.status.name(),
        "seconds": seconds(outcome.elapsed),
        "overhead_seconds": seconds(outcome.overhead),
        "reused": outcome.reused,
        "errors": outcome.errors,
        "summary": outcome.summary,
        "note": outcome.note,
        "compiled": outcome.compiled,
    })
}

/// Seconds to the millisecond, which is as fine as a step's time means anything.
fn seconds(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1000.0).round() / 1000.0
}

impl Utc {
    /// The proleptic Gregorian date of a Unix time, by Howard Hinnant's `civil_from_days`.
    fn from_unix(unix: u64) -> Self {
        let days = i64::try_from(unix / 86_400).expect("a Unix time in days fits i64");
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).expect("a day of the month");
        let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).expect("a month");
        let year = yoe + era * 400 + i64::from(month <= 2);
        Self {
            year,
            month,
            day,
            seconds: unix % 86_400,
        }
    }

    fn rfc3339(&self) -> String {
        let (h, m, s) = (
            self.seconds / 3_600,
            self.seconds / 60 % 60,
            self.seconds % 60,
        );
        format!(
            "{:04}-{:02}-{:02}T{h:02}:{m:02}:{s:02}Z",
            self.year, self.month, self.day
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::Status;

    #[test]
    fn utc_dates() {
        // 0 is 1970-01-01; 951_782_400 is 2000-02-29, a leap day of a century year; 1_760_745_600
        // is 2025-10-18; one second before 2001-01-01 ends 2000.
        let date = |unix| {
            let utc = Utc::from_unix(unix);
            (utc.year, utc.month, utc.day)
        };
        assert_eq!(date(0), (1970, 1, 1));
        assert_eq!(date(951_782_400), (2000, 2, 29));
        assert_eq!(date(1_760_745_600), (2025, 10, 18));
        assert_eq!(date(978_307_199), (2000, 12, 31));
        assert_eq!(
            Utc::from_unix(978_307_199).rfc3339(),
            "2000-12-31T23:59:59Z"
        );
        assert_eq!(Utc::from_unix(3_723).rfc3339(), "1970-01-01T01:02:03Z");
    }

    #[test]
    fn writes_one_line_a_command() {
        let dir = std::env::temp_dir().join(format!("drive-journal-{}", std::process::id()));
        let mut journal = Journal::start("test", &["name".to_owned()], Ci::No);
        journal.loaded();
        let mut lint = Outcome::reused("lint", "no input changed since it passed".to_owned());
        lint.overhead = Duration::from_millis(12);
        let mut tests = Outcome::skipped("tests", String::new());
        tests.status = Status::Fail;
        tests.elapsed = Duration::from_millis(1_234);
        tests.compiled = vec!["core".to_owned()];
        tests.errors = 2;
        let outcomes = [lint, tests];
        let path = journal
            .write(&dir, Path::new("/w"), &outcomes, &End::Failed)
            .unwrap();
        let error = End::Error("cannot write /w/target/drive/log/tests.log".to_owned());
        journal
            .write(&dir, Path::new("/w"), &outcomes[..1], &error)
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        let ran = &lines[0];
        assert_eq!(
            (ran["command"].as_str(), ran["exit"].as_i64()),
            (Some("test"), Some(1))
        );
        assert_eq!(ran["args"], serde_json::json!(["name"]));
        assert_eq!(ran["project"], "/w");
        assert_eq!(ran["ci"], false);
        assert!(ran["error"].is_null());
        assert_eq!(ran["steps"][0]["reused"], true);
        assert_eq!(ran["steps"][0]["overhead_seconds"], 0.012);
        assert_eq!(ran["steps"][1]["status"], "FAIL");
        assert_eq!(ran["steps"][1]["seconds"], 1.234);
        assert_eq!(ran["steps"][1]["compiled"], serde_json::json!(["core"]));
        assert_eq!(ran["steps"][1]["errors"], 2);
        assert_eq!(lines[1]["exit"], 2);
        // An error keeps the steps that ran before it.
        assert_eq!(
            lines[1]["error"],
            "cannot write /w/target/drive/log/tests.log"
        );
        assert_eq!(lines[1]["steps"][0]["name"], "lint");
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".jsonl")
        );
    }
}
