use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::git::Repo;
use crate::outcome::{Outcome, Status};
use crate::output::{self, Ci};
use crate::passes::{Passes, Snapshot};
use crate::project::Project;
use crate::root::{Reach, Root};
use crate::step::{Kind, Runs, Step};
use crate::tool;

/// Whether a command reuses and keeps the passes of its steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reuse {
    Passes,
    Never,
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

/// Runs a command's steps, each after the one before it and past failures: once a step finds
/// code that does not compile, no later step of its root builds.
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

    pub(crate) fn run(mut self, plans: &[Plan<'_>]) -> Finished {
        let width = plans
            .iter()
            .flat_map(|p| &p.steps)
            .map(|s| s.name.len())
            .max()
            .unwrap_or(0);
        let mut error = None;
        'plans: for plan in plans {
            let mut broken: Option<String> = None;
            for step in &plan.steps {
                let outcome = match &broken {
                    Some(by) if step.kind != Kind::Format => Outcome::skipped(
                        &step.name,
                        format!("{by} found code that does not compile"),
                    ),
                    _ => {
                        let started = Instant::now();
                        match self.step(plan.root, step) {
                            Ok(mut outcome) => {
                                outcome.overhead =
                                    started.elapsed().saturating_sub(outcome.elapsed);
                                outcome
                            }
                            Err(stopped) => {
                                error = Some(stopped);
                                break 'plans;
                            }
                        }
                    }
                };
                if outcome.broken && broken.is_none() {
                    broken = Some(outcome.name.clone());
                }
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
            return self.execute(root, step, "");
        }
        let Some(snapshot) = self.snapshot(root)? else {
            return self.execute(root, step, "");
        };
        let key = self.key(root, step);
        let last = self.passes.as_ref().and_then(|p| p.get(&key)).cloned();
        let change = last.and_then(|last| self.changed(&last, &snapshot));
        let mut run = step.clone();
        let mut note = String::new();
        if let Some(paths) = change
            && step.runs == Runs::OnChange
        {
            match root.reach(&paths, &self.project.outside(root)) {
                None => return self.reuse(step, key, snapshot, "no input changed since it passed"),
                Some(Reach::Members(reached)) if step.kind == Kind::Nextest => {
                    let hit: Vec<&str> = root
                        .members
                        .iter()
                        .map(|m| m.name.as_str())
                        .filter(|n| reached.contains(*n))
                        .collect();
                    if hit.is_empty() {
                        return self.reuse(step, key, snapshot, "no change reaches its tests");
                    }
                    if hit.len() < root.members.len() {
                        note = format!(
                            "the tests of {} of {} crates",
                            hit.len(),
                            root.members.len()
                        );
                        run = step.narrowed(hit);
                    }
                }
                // `cargo test` selects crates only through `-p`, which builds another shape.
                Some(Reach::Members(reached)) if step.kind == Kind::CargoTest => {
                    if !root.members.iter().any(|m| reached.contains(&m.name)) {
                        return self.reuse(step, key, snapshot, "no change reaches its tests");
                    }
                }
                Some(Reach::Members(reached)) if step.kind == Kind::Doctests => {
                    if !root
                        .members
                        .iter()
                        .any(|m| m.doctest && reached.contains(&m.name))
                    {
                        return self.reuse(step, key, snapshot, "no change reaches its doc tests");
                    }
                }
                Some(_) => {}
            }
        }
        let outcome = self.execute(root, &run, &note)?;
        if outcome.status == Status::Ok && self.snapshot(root)?.as_ref() == Some(&snapshot) {
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

    fn execute(&mut self, root: &Root, step: &Step, note: &str) -> Result<Outcome> {
        let log = self.own.join("log").join(step.log_name());
        // Written aside and moved in whole, so a run beside this one leaves each log whole.
        let aside = log.with_file_name(format!("{}-{}.tmp", step.log_name(), std::process::id()));
        let io = |path: PathBuf| move |source| Error::Io { path, source };
        let file = File::create(&aside).map_err(io(aside.clone()))?;
        let mut command = tool::cargo();
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
            program: format!("cargo {}", step.argv[0]),
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
        let mut notes: Vec<String> = [note, step.label.as_str(), report.summary.as_str()]
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

    /// A step's identity: what changes its result from outside the working files.
    fn key(&mut self, root: &Root, step: &Step) -> String {
        let rustc = self
            .rustc
            .get_or_insert_with(|| version(Command::new("rustc").arg("-V"), &root.dir))
            .clone();
        let mut tools = vec![rustc];
        if step.kind == Kind::Nextest {
            let nextest = self.nextest.get_or_insert_with(|| {
                version(tool::cargo().args(["nextest", "--version"]), &root.dir)
            });
            tools.push(nextest.clone());
        }
        let mut env: BTreeMap<String, Option<String>> = std::env::vars()
            .filter(|(name, _)| name.starts_with("RUST") || name.starts_with("CARGO"))
            .map(|(name, value)| (name, Some(value)))
            .collect();
        env.extend(step.env.iter().cloned());
        let identity = serde_json::json!([root.dir, step.name, step.argv, env, tools]);
        identity.to_string()
    }
}

fn version(command: &mut Command, dir: &Path) -> String {
    let out = command.current_dir(dir).output();
    out.map(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_owned()
    })
    .unwrap_or_default()
}
