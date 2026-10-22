use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::capture::Capture;
use crate::ci::Ci;
use crate::config::TestRunner;
use crate::context::Context;
use crate::error::{Error, Result};
use crate::outcome::{Outcome, Status};
use crate::passes::{PassKey, Passes, Snapshot};
use crate::process;
use crate::project::Project;
use crate::repo::Repo;
use crate::root::{Reach, Root};
use crate::step::{Action, Kind, Runs, Step};
use crate::vocabulary::Vocabulary;

/// Whether a command reuses and keeps the passes of its steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reuse {
    Passes,
    Never,
}

/// Whether a failed step ends the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnFail {
    /// Every later step is skipped, on every root.
    Stop,
    /// Later steps run, but a root's builds stop at code that does not compile.
    Continue,
}

/// Why the next step does not run.
#[derive(Debug)]
struct Stops {
    on_fail: OnFail,
    /// The first step that failed, under [`OnFail::Stop`].
    failed: Option<String>,
    /// The first step of the current root that found code that does not compile.
    broken: Option<String>,
}

impl Stops {
    const fn new(on_fail: OnFail) -> Self {
        Self {
            on_fail,
            failed: None,
            broken: None,
        }
    }

    fn next_root(&mut self) {
        self.broken = None;
    }

    /// `None` when a step of `kind` runs: `fmt` builds nothing, so only a failure stops it.
    fn reason(&self, kind: Kind) -> Option<String> {
        match (&self.failed, &self.broken) {
            (Some(by), _) => Some(format!("{by} failed")),
            (None, Some(by)) if kind != Kind::Format => {
                Some(format!("{by} found code that does not compile"))
            }
            _ => None,
        }
    }

    fn record(&mut self, outcome: &Outcome) {
        if outcome.broken && self.broken.is_none() {
            self.broken = Some(outcome.name.clone());
        }
        if self.on_fail == OnFail::Stop && outcome.status == Status::Fail && self.failed.is_none() {
            self.failed = Some(outcome.name.clone());
        }
    }
}

/// How a command's steps came out.
#[derive(Debug)]
pub(crate) struct Finished {
    pub(crate) outcomes: Vec<Outcome>,
    /// A driver error that ended the command before its last step; the steps before it stay.
    pub(crate) error: Option<Error>,
    /// The top root's folder.
    pub(crate) top: PathBuf,
    /// `<top target>/drive`, which holds the logs and the run's output file.
    pub(crate) own: PathBuf,
}

impl Finished {
    /// Every step passed or was reused, and no driver error ended the command.
    pub(crate) fn passed(&self) -> bool {
        self.error.is_none() && self.outcomes.iter().all(|o| o.status == Status::Ok)
    }
}

/// The steps of one root, in order.
#[derive(Debug)]
pub(crate) struct Plan<'p> {
    pub(crate) root: &'p Root,
    pub(crate) steps: Vec<Step>,
}

/// Runs a command's steps, each after the one before it, as its [`OnFail`] allows.
#[derive(Debug)]
pub(crate) struct Runner<'p> {
    project: &'p Project,
    cx: &'p mut Context,
    /// `<top target>/drive`: logs, passes and scratch.
    own: PathBuf,
    passes: Option<Passes>,
    outcomes: Vec<Outcome>,
    /// Each repository's tree, as it stands since the last step that ran: a step that runs can
    /// write files, as `fmt` does.
    trees: BTreeMap<PathBuf, String>,
}

impl<'p> Runner<'p> {
    pub(crate) fn new(project: &'p Project, reuse: Reuse, cx: &'p mut Context) -> Result<Self> {
        let own = project.top().target_dir.join("drive");
        let logs = own.join("log");
        std::fs::create_dir_all(&logs).map_err(|source| Error::Io {
            path: logs.clone(),
            source,
        })?;
        for dir in [&own, &logs] {
            sweep_aside(dir, SystemTime::now());
        }
        // CI's checkout is new, so it keeps no passes.
        let passes = (reuse == Reuse::Passes && cx.console.ci() == Ci::No)
            .then(|| Passes::load(own.join("passes.json"), cx.tools.driver()));
        Ok(Self {
            project,
            cx,
            own,
            passes,
            outcomes: Vec::new(),
            trees: BTreeMap::new(),
        })
    }

    pub(crate) fn run(mut self, plans: &[Plan<'_>], on_fail: OnFail) -> Finished {
        let width = plans
            .iter()
            .flat_map(|p| &p.steps)
            .map(|s| s.name.len())
            .max()
            .unwrap_or(0);
        let mut error = None;
        let mut stops = Stops::new(on_fail);
        'plans: for plan in plans {
            stops.next_root();
            for step in &plan.steps {
                let outcome = if let Some(reason) = stops.reason(step.kind) {
                    Outcome::skipped(&step.name, reason)
                } else {
                    let started = Instant::now();
                    match self.step(plan.root, step) {
                        Ok(mut outcome) => {
                            outcome.overhead = started.elapsed().saturating_sub(outcome.elapsed);
                            outcome
                        }
                        Err(stopped) => {
                            error = Some(stopped);
                            break 'plans;
                        }
                    }
                };
                stops.record(&outcome);
                let line = outcome.line(width, &self.cx.here);
                self.cx.console.line(&line);
                self.outcomes.push(outcome);
            }
        }
        Finished {
            outcomes: self.outcomes,
            error,
            top: self.project.top().dir.clone(),
            own: self.own,
        }
    }

    /// A step runs only as its passes allow: not when no input changed since it passed, and a
    /// test step only on the tests a change reaches. A pass is kept only when no file changed
    /// while the step ran.
    fn step(&mut self, root: &Root, step: &Step) -> Result<Outcome> {
        if self.passes.is_none() {
            return self.execute(root, step);
        }
        let Some(snapshot) = self.snapshot(root)? else {
            return self.execute(root, step);
        };
        let key = self.key(root, step);
        let last = self.passes.as_ref().and_then(|p| p.get(&key)).cloned();
        let change = last.and_then(|last| self.changed(&last, &snapshot));
        let mut run = step.clone();
        if let Some(paths) = change
            && step.runs != Runs::Always
        {
            match root.reach(&paths, &self.project.outside(root)) {
                None => return self.reuse(step, key, snapshot, "no input changed since it passed"),
                Some(Reach::Members(reached)) if step.runs == Runs::OnChange => {
                    let Some(narrowed) = step.for_reach(root, &reached) else {
                        return self.reuse(step, key, snapshot, "no change reaches its tests");
                    };
                    run = narrowed;
                }
                Some(_) => {}
            }
        }
        let outcome = self.execute(root, &run)?;
        let keeps = step.shared.as_ref().is_none_or(|shared| shared.last);
        if keeps && outcome.status == Status::Ok && self.snapshot(root)?.as_ref() == Some(&snapshot)
        {
            self.passes
                .as_mut()
                .expect("checked above")
                .keep(key, snapshot)?;
        }
        Ok(outcome)
    }

    fn reuse(
        &mut self,
        step: &Step,
        key: PassKey,
        snapshot: Snapshot,
        note: &str,
    ) -> Result<Outcome> {
        self.passes
            .as_mut()
            .expect("reuse needs passes")
            .keep(key, snapshot)?;
        Ok(Outcome::reused(&step.name, note.to_owned()))
    }

    fn execute(&mut self, root: &Root, step: &Step) -> Result<Outcome> {
        let log = self.own.join("log").join(step.log_name());
        // Written aside and moved in whole, so a run beside this one leaves each log whole.
        let aside = log.with_file_name(format!("{}-{}.tmp", step.log_name(), std::process::id()));
        let io = |path: PathBuf| move |source| Error::Io { path, source };
        let file = File::create(&aside).map_err(io(aside.clone()))?;
        let mut command = match &step.action {
            Action::Cargo => process::cargo(),
            Action::Program(path) => process::command(path),
            Action::Copy { from, to } => {
                let started = Instant::now();
                let copied = copy(from, to);
                drop(file);
                std::fs::rename(&aside, &log).map_err(io(log.clone()))?;
                return Ok(Outcome::copied(step, copied, started.elapsed(), log));
            }
            Action::Vocabulary { out } => {
                let started = Instant::now();
                let vocabulary = Vocabulary::of(self.project)?;
                vocabulary.write(out)?;
                let unparsed: Vec<String> = (vocabulary.unparsed().iter())
                    .map(ToString::to_string)
                    .collect();
                let mut file = file;
                for line in &unparsed {
                    writeln!(file, "{line}").map_err(io(aside.clone()))?;
                }
                drop(file);
                std::fs::rename(&aside, &log).map_err(io(log.clone()))?;
                let out = out.strip_prefix(&self.cx.here).unwrap_or(out);
                return Ok(Outcome::listed(step, unparsed, out, started.elapsed(), log));
            }
        };
        command.args(&step.argv).current_dir(&root.dir);
        for (name, value) in &step.env {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        let ci = self.cx.console.ci();
        if ci == Ci::GitHub {
            self.cx.console.line(&format!("::group::{}", step.name));
        }
        self.trees.clear();
        let started = Instant::now();
        let log_name = log
            .strip_prefix(&self.cx.here)
            .unwrap_or(&log)
            .display()
            .to_string();
        let capture = Capture {
            kind: step.kind,
            log: file,
            log_name: &log_name,
        };
        let report = capture
            .run(&mut command, &mut self.cx.console)
            .map_err(|source| Error::Spawn {
                program: step.action_name(),
                source,
            })?;
        let elapsed = started.elapsed();
        std::fs::rename(&aside, &log).map_err(io(log.clone()))?;
        if ci == Ci::GitHub {
            self.cx.console.line("::endgroup::");
        }
        let status = if report.passed() {
            Status::Ok
        } else {
            Status::Fail
        };
        // A lint that fails with no diagnostic failed before rustc ran: no build gets further.
        let broken = report.hard > 0
            || (status == Status::Fail && step.kind == Kind::Check && report.errors == 0);
        let note = report.note(&step.label);
        Ok(Outcome {
            name: step.name.clone(),
            status,
            elapsed,
            log: Some(log),
            first: report.first,
            lines: report.lines,
            overhead: Duration::ZERO,
            errors: report.errors,
            summary: report.summary,
            compiled: report.compiled,
            note,
            broken,
            reused: false,
        })
    }

    /// The trees of the repositories `root`'s steps read; `None` outside git.
    fn snapshot(&mut self, root: &Root) -> Result<Option<Snapshot>> {
        let scratch = self
            .own
            .join(format!("tree-index-{}.tmp", std::process::id()));
        let mut snapshot = Snapshot::new();
        for each in std::iter::once(root).chain(self.project.outside(root)) {
            let Some(repo) = &each.repo else {
                return Ok(None);
            };
            if snapshot.contains_key(&repo.dir) {
                continue;
            }
            if !self.trees.contains_key(&repo.dir) {
                let forced = self.project.tool_files(&repo.dir);
                let Some(tree) = repo.tree(&scratch, &forced)? else {
                    return Ok(None);
                };
                self.trees.insert(repo.dir.clone(), tree);
            }
            snapshot.insert(repo.dir.clone(), self.trees[&repo.dir].clone());
        }
        Ok(Some(snapshot))
    }

    /// The files that changed from `last` to `now`; `None` when git cannot tell.
    fn changed(&self, last: &Snapshot, now: &Snapshot) -> Option<Vec<PathBuf>> {
        let repos: BTreeMap<&PathBuf, &Repo> = self
            .project
            .roots()
            .iter()
            .filter_map(|r| r.repo.as_ref())
            .map(|r| (&r.dir, r))
            .collect();
        let mut paths = Vec::new();
        for (dir, tree) in now {
            let old = last.get(dir)?;
            if old != tree {
                paths.extend(repos.get(dir)?.changed(old, tree)?);
            }
        }
        Some(paths)
    }

    /// The versions of the tools `step` runs, read once a run; the passes file names the driver.
    fn key(&mut self, root: &Root, step: &Step) -> PassKey {
        let tools = &mut self.cx.tools;
        let mut versions = vec![tools.rustc(&root.dir).to_owned()];
        // The steps of a shared pass need one identity, and any of them may run the tests.
        let tests = match step.shared {
            Some(_) => root.config.test_runner == TestRunner::Nextest,
            None => step.kind == Kind::Nextest,
        };
        if tests {
            versions.push(tools.nextest(&root.dir).to_owned());
        }
        step.pass_key(&root.dir, tools.env(), &versions)
    }
}

/// How old a file written aside, `*.tmp`, is before a run takes it for one an interrupted run
/// left: a run writes its own as it goes, and none runs a day.
const ASIDE_AGE: Duration = Duration::from_hours(24);

/// Removes the files written aside in `dir` that runs left as they were interrupted, as `now`
/// tells their age. Their names hold the process's id, so no later run writes over one. A failure
/// to remove one fails no check.
fn sweep_aside(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let aside = entry.file_name().to_string_lossy().ends_with(".tmp");
        let old = (entry.metadata().and_then(|m| m.modified()))
            .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > ASIDE_AGE));
        if aside && old && entry.file_type().is_ok_and(|t| !t.is_dir()) {
            std::fs::remove_file(entry.path()).ok();
        }
    }
}

/// Puts a copy of `from`, a file or a folder with all it holds, at `to`, in place of what is
/// there.
fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(to) {
        Ok(there) if there.is_dir() => std::fs::remove_dir_all(to)?,
        Ok(_) => std::fs::remove_file(to)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if !from.is_dir() {
        return std::fs::copy(from, to).map(|_| ());
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ran(name: &str, status: Status, broken: bool) -> Outcome {
        Outcome {
            status,
            broken,
            ..Outcome::skipped(name, String::new())
        }
    }

    #[test]
    fn sweeps_only_old_files_written_aside() {
        let dir = std::env::temp_dir().join(format!("drive-sweep-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("folder.tmp")).unwrap();
        let names = [
            "lint.log-1.tmp",
            "tree-index-2.tmp",
            "fresh-3.tmp",
            "lint.log",
        ];
        for name in names {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        let now = SystemTime::now();
        let old = now - ASIDE_AGE - Duration::from_secs(1);
        for name in ["lint.log-1.tmp", "tree-index-2.tmp", "lint.log"] {
            let file = File::options().write(true).open(dir.join(name)).unwrap();
            file.set_modified(old).unwrap();
        }
        sweep_aside(&dir, now);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        std::fs::remove_dir_all(&dir).unwrap();
        // A file newer than a day may be a running run's, a log is no aside file, and a folder
        // is none the driver writes.
        assert_eq!(left, ["folder.tmp", "fresh-3.tmp", "lint.log"]);
    }

    #[test]
    fn a_failure_stops_every_later_step_only_under_stop() {
        let mut stop = Stops::new(OnFail::Stop);
        let mut keep_on = Stops::new(OnFail::Continue);
        for stops in [&mut stop, &mut keep_on] {
            stops.record(&ran("fmt", Status::Ok, false));
            assert_eq!(stops.reason(Kind::Check), None);
            stops.record(&ran("lint", Status::Fail, false));
            stops.record(&ran("ship", Status::Fail, false));
            stops.next_root();
        }
        // The first failure names the stop, past the root it failed in, and stops `fmt` too.
        assert_eq!(stop.reason(Kind::Format).as_deref(), Some("lint failed"));
        assert_eq!(stop.reason(Kind::Nextest).as_deref(), Some("lint failed"));
        assert_eq!(keep_on.reason(Kind::Format), None);
        assert_eq!(keep_on.reason(Kind::Nextest), None);
    }

    #[test]
    fn code_that_does_not_compile_stops_its_root_builds() {
        let mut stops = Stops::new(OnFail::Continue);
        stops.record(&ran("tests", Status::Fail, true));
        stops.record(&ran("doctests", Status::Fail, true));
        let broken = Some("tests found code that does not compile".to_owned());
        assert_eq!(stops.reason(Kind::Doctests), broken);
        assert_eq!(stops.reason(Kind::Format), None);
        stops.next_root();
        assert_eq!(stops.reason(Kind::Doctests), None);
    }
}
