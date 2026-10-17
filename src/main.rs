//! The one way to check, test, document and bench a Cargo workspace, for an agent, for a person
//! and for CI alike.
//!
//! Cargo keeps a cache for each shape of a command: its features, profile, mode and flags. The
//! driver owns every cargo command, and each lane is one shape over the whole workspace, so an
//! edit rebuilds each crate it reaches once. What a check narrows is what it runs: a step does
//! not run again when no input changed since it passed, and the tests run only for the crates a
//! change reaches. A git submodule a member depends on by path is a root of its own, checked in
//! its own folder.
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
mod lanes;
mod member;
mod metadata;
mod outcome;
mod output;
mod passes;
mod project;
mod root;
mod runner;
mod step;
mod tool;

use std::path::Path;
use std::process::ExitCode;

use crate::cli::Task;
use crate::error::Result;
use crate::lanes::Lane;
use crate::output::Ci;
use crate::project::Project;
use crate::runner::{Plan, Reuse, Runner};
use crate::step::{Format, Runs, Step};

fn main() -> ExitCode {
    let task = Task::parse();
    match run(&task) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(task: &Task) -> Result<ExitCode> {
    for name in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"] {
        if std::env::var_os(name).is_some() {
            eprintln!(
                "warning: {name} replaces the rustflags of .cargo/config.toml, \
                 so this build shares no artifacts with a build without it"
            );
        }
    }
    let ci = if std::env::var_os("GITHUB_ACTIONS").is_some() {
        Ci::GitHub
    } else {
        Ci::No
    };
    let top = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the driver sits in its root");
    let project = Project::load(top)?;
    let root = project.top();
    match task {
        Task::Check | Task::Gate => {
            let (name, format) = if *task == Task::Check {
                ("check", Format::Write)
            } else {
                ("gate", Format::Check)
            };
            let plans: Vec<Plan<'_>> = project
                .roots()
                .iter()
                .map(|r| {
                    let lanes = if *task == Task::Check {
                        &r.lanes.check
                    } else {
                        &r.lanes.gate
                    };
                    Plan {
                        root: r,
                        steps: Step::every(r, lanes, format),
                    }
                })
                .collect();
            Runner::new(&project, name, ci, Reuse::Passes)?.run(&plans)
        }
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
            Runner::new(&project, "test", ci, reuse)?.run(&[Plan { root, steps }])
        }
        Task::Bench(args) => {
            let Some(step) = Step::bench(root, args) else {
                eprintln!("error: no member of {} has a bench", root.dir.display());
                return Ok(ExitCode::from(2));
            };
            Runner::new(&project, "bench", ci, Reuse::Never)?.run(&[Plan {
                root,
                steps: vec![step],
            }])
        }
    }
}
