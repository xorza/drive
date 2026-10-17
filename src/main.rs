//! The one way to check, test, document and bench a Cargo workspace, for an agent, for a person
//! and for CI alike.
//!
//! Cargo keeps a cache for each shape of a command: its features, profile, mode and flags. The
//! driver owns every cargo command, and each lane is one shape over the whole workspace, so an
//! edit rebuilds each crate it reaches once. What a check narrows is what it runs: a step does
//! not run again when no input changed since it passed, and the tests run only for the crates a
//! change reaches. The workspace of a path dependency outside the workspace, such as a git
//! submodule, is a root of its own, checked in its own folder.
//!
//! Each command appends one JSON line to the run log, with its steps' times, the driver's own
//! time around them, and the crates cargo compiled: one file a month in the OS's state folder,
//! `$XDG_STATE_HOME/drive/` or `~/.local/state/drive/` on Linux, `~/Library/Logs/drive/` on
//! macOS, `%LOCALAPPDATA%\drive\logs\` on Windows.
//!
//! A project holds this crate as a git submodule at its workspace root, outside its workspace,
//! and names it in `.cargo/config.toml`: `drive = "run -q --locked --manifest-path
//! drive/Cargo.toml --"`.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line tool reports on its terminal"
)]

mod cli;
mod error;
mod flag_set;
mod git;
mod journal;
mod lanes;
mod member;
mod metadata;
mod outcome;
mod output;
mod passes;
mod platform;
mod project;
mod root;
mod runner;
mod step;
mod tool;

use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitCode;

use crate::cli::Task;
use crate::error::{Error, Result};
use crate::journal::{End, Journal};
use crate::lanes::Lane;
use crate::output::Ci;
use crate::project::Project;
use crate::root::Root;
use crate::runner::{Finished, Plan, Reuse, Runner};
use crate::step::{Format, Runs, Step};

fn main() -> ExitCode {
    let task = Task::parse();
    let ci = if std::env::var_os("GITHUB_ACTIONS").is_some() {
        Ci::GitHub
    } else {
        Ci::No
    };
    let mut journal = Journal::start(task.name(), task.args(), ci);
    let top = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the driver sits in its root");
    let result = run(&task, top, ci, &mut journal);
    let end = match &result {
        Ok(Finished {
            error: Some(error), ..
        })
        | Err(error) => End::Error(error.to_string()),
        Ok(finished) if finished.passed() => End::Passed,
        Ok(_) => End::Failed,
    };
    let outcomes = result.as_ref().map_or(&[][..], |f| f.outcomes.as_slice());
    // The log serves later analysis: a failure to write it fails no check.
    if let Some(dir) = platform::log_dir()
        && let Err(error) = journal.write(&dir, top, outcomes, &end)
    {
        eprintln!(
            "warning: cannot write the run log in {}: {error}",
            dir.display()
        );
    }
    match end {
        End::Passed => ExitCode::SUCCESS,
        End::Failed => ExitCode::FAILURE,
        End::Error(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(task: &Task, top: &Path, ci: Ci, journal: &mut Journal) -> Result<Finished> {
    for name in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"] {
        if std::env::var_os(name).is_some() {
            eprintln!(
                "warning: {name} replaces the rustflags of .cargo/config.toml, \
                 so this build shares no artifacts with a build without it"
            );
        }
    }
    let project = Project::load(top)?;
    journal.loaded();
    let root = project.top();
    match task {
        Task::Check => every_root(&project, ci, Format::Write, |r| &r.lanes.check),
        Task::Gate => every_root(&project, ci, Format::Check, |r| &r.lanes.gate),
        Task::Test(args) => {
            let mut steps = Step::tests(root, args, Runs::Always);
            // nextest's arguments select no doc test; a whole run keeps its pass for the checks,
            // with the doc tests when a command of the root runs them.
            let lanes = &root.lanes;
            let reuse = if args.is_empty() {
                if lanes.check.contains(&Lane::Doctests) || lanes.gate.contains(&Lane::Doctests) {
                    steps.extend(Step::doctests(root, Runs::Always));
                }
                Reuse::Passes
            } else {
                Reuse::Never
            };
            Ok(Runner::new(&project, ci, reuse)?.run(&[Plan { root, steps }]))
        }
        Task::Bench(args) => {
            let step = Step::bench(root, args).ok_or_else(|| Error::NoBenches {
                dir: root.dir.clone(),
            })?;
            Ok(Runner::new(&project, ci, Reuse::Never)?.run(&[Plan {
                root,
                steps: vec![step],
            }]))
        }
    }
}

/// The lanes `pick` chooses, on every root of the project.
fn every_root(
    project: &Project,
    ci: Ci,
    format: Format,
    pick: impl Fn(&Root) -> &BTreeSet<Lane>,
) -> Result<Finished> {
    let plans: Vec<Plan<'_>> = project
        .roots()
        .iter()
        .map(|r| Plan {
            root: r,
            steps: Step::every(r, pick(r), format),
        })
        .collect();
    Ok(Runner::new(project, ci, Reuse::Passes)?.run(&plans))
}
