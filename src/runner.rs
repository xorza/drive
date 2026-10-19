use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::git::Repo;
use crate::outcome::{Outcome, Status};
use crate::output::{self, Ci};
use crate::passes::{Passes, Snapshot};
use crate::project::Project;
use crate::root::{Reach, Root};
use crate::step::{Kind, Program, Runs, Step};
use crate::tool;

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
    ci: Ci,
    /// `<top target>/drive`: logs, passes and scratch.
    own: PathBuf,
    passes: Option<Passes>,
    shown: BTreeSet<String>,
    outcomes: Vec<Outcome>,
    rustc: Option<String>,
    nextest: Option<String>,
    /// Each repository's tree, as it stands since the last step that ran: a step that runs can
    /// write files, as `fmt` does.
    trees: BTreeMap<PathBuf, String>,
}

impl<'p> Runner<'p> {
    pub(crate) fn new(project: &'p Project, ci: Ci, reuse: Reuse) -> Result<Self> {
        let own = project.top().target_dir.join("drive");
        let logs = own.join("log");
        std::fs::create_dir_all(&logs).map_err(|source| Error::Io { path: logs, source })?;
        // CI's checkout is new, so it keeps no passes.
        let passes =
            (reuse == Reuse::Passes && ci == Ci::No).then(|| Passes::load(own.join("passes.json")));
        Ok(Self {
            project,
            ci,
            own,
            passes,
            shown: BTreeSet::new(),
            outcomes: Vec::new(),
            rustc: None,
            nextest: None,
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
                println!("{}", outcome.line(width));
                self.outcomes.push(outcome);
            }
        }
        // Diagnostics between the lines leave them apart: the lines again, under a heading, so no
        // one reads them as a second run.
        if self.outcomes.len() > 1 && self.outcomes.iter().any(|o| o.printed > 0) {
            println!("\nsummary");
            for outcome in &self.outcomes {
                println!("{}", outcome.line(width));
            }
        }
        Finished {
            outcomes: self.outcomes,
            error,
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
        key: String,
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
        let mut command = match &step.program {
            Program::Cargo => tool::cargo(),
            Program::Other(path) => tool::command(path),
            Program::Copy { from, to } => {
                let started = Instant::now();
                let copied = copy(from, to);
                drop(file);
                std::fs::rename(&aside, &log).map_err(io(log.clone()))?;
                return Ok(Outcome::copied(step, copied, started.elapsed(), log));
            }
        };
        command.args(&step.argv).current_dir(&root.dir);
        for (name, value) in &step.env {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        if self.ci == Ci::GitHub {
            println!("::group::{}", step.name);
        }
        self.trees.clear();
        let started = Instant::now();
        let log_name = log.display().to_string();
        let report = output::run(
            &mut command,
            step.kind,
            self.ci,
            file,
            &log_name,
            &mut self.shown,
        )
        .map_err(|source| Error::Spawn {
            program: step.program_name(),
            source,
        })?;
        let elapsed = started.elapsed();
        std::fs::rename(&aside, &log).map_err(io(log.clone()))?;
        if self.ci == Ci::GitHub {
            println!("::endgroup::");
        }
        let status = if report.code == Some(0) && report.errors == 0 {
            Status::Ok
        } else {
            Status::Fail
        };
        // A lint that fails with no diagnostic failed before rustc ran: no build gets further.
        let broken = report.hard > 0
            || (status == Status::Fail && step.kind == Kind::Check && report.errors == 0);
        let mut notes: Vec<String> = [step.label.as_str(), report.summary.as_str()]
            .into_iter()
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .collect();
        if report.errors > 0 {
            let plural = if report.errors == 1 { "" } else { "s" };
            let repeated = if report.repeated > 0 {
                format!(", {} shown above", report.repeated)
            } else {
                String::new()
            };
            notes.push(format!("{} error{plural}{repeated}", report.errors));
        }
        Ok(Outcome {
            name: step.name.clone(),
            status,
            elapsed,
            log: Some(log),
            printed: report.printed,
            overhead: Duration::ZERO,
            errors: report.errors,
            summary: report.summary,
            compiled: report.compiled,
            note: notes.join("; "),
            broken,
            reused: false,
        })
    }

    /// The trees of the repositories `root`'s steps read; `None` outside git.
    fn snapshot(&mut self, root: &Root) -> Result<Option<Snapshot>> {
        let scratch = self.own.join(format!("tree-index-{}", std::process::id()));
        let mut snapshot = Snapshot::new();
        for each in std::iter::once(root).chain(self.project.outside(root)) {
            let Some(repo) = &each.repo else {
                return Ok(None);
            };
            if snapshot.contains_key(&repo.top) {
                continue;
            }
            if !self.trees.contains_key(&repo.top) {
                let Some(tree) = repo.tree(&scratch)? else {
                    return Ok(None);
                };
                self.trees.insert(repo.top.clone(), tree);
            }
            snapshot.insert(repo.top.clone(), self.trees[&repo.top].clone());
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
            .map(|r| (&r.top, r))
            .collect();
        let mut paths = Vec::new();
        for (top, tree) in now {
            let old = last.get(top)?;
            if old != tree {
                paths.extend(repos.get(top)?.changed(old, tree)?);
            }
        }
        Some(paths)
    }

    /// A step's identity: what changes its result from outside the working files. Its command,
    /// not its name: `check`'s lint and `gate`'s lint of the same flags share a pass.
    fn key(&mut self, root: &Root, step: &Step) -> String {
        let rustc = self
            .rustc
            .get_or_insert_with(|| {
                tool::first_line(tool::command("rustc").arg("-V").current_dir(&root.dir))
            })
            .clone();
        let mut tools = vec![rustc];
        if step.kind == Kind::Nextest {
            let nextest = self.nextest.get_or_insert_with(|| {
                tool::first_line(
                    tool::cargo()
                        .args(["nextest", "--version"])
                        .current_dir(&root.dir),
                )
            });
            tools.push(nextest.clone());
        }
        // `vars` panics on any variable that is not UTF-8, also one no key reads. Each value keeps
        // its bytes escaped, so no two values, UTF-8 or not, read alike.
        let mut env: BTreeMap<String, Option<String>> = std::env::vars_os()
            .filter(|(name, _)| !tool::is_drivers_own(name))
            .filter_map(|(name, value)| {
                let name = name.into_string().ok()?;
                let value = value.as_encoded_bytes().escape_ascii().to_string();
                (name.starts_with("RUST") || name.starts_with("CARGO"))
                    .then_some((name, Some(value)))
            })
            .collect();
        // The steps of one command share its identity, which holds each step's own variables.
        if let Some(shared) = &step.shared {
            return serde_json::json!([root.dir, shared.command, env, tools[0]]).to_string();
        }
        env.extend(step.env.iter().cloned());
        let identity = serde_json::json!([root.dir, step.argv, env, tools]);
        identity.to_string()
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
