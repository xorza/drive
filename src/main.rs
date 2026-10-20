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
//! `cargo install --locked --git https://github.com/xorza/drive` puts `cargo-drive` on `PATH`,
//! which cargo runs as `cargo drive`. It checks the workspace of the folder it runs in, and each
//! root names the driver it needs in its table's `version`.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line tool reports on its terminal"
)]

mod cli;
mod error;
mod flag_set;
mod git;
mod invocation;
mod journal;
mod lanes;
mod member;
mod metadata;
mod outcome;
mod output;
mod passes;
mod platform;
mod project;
mod project_command;
mod reads;
mod root;
mod runner;
mod step;
mod sweep;
mod tool;
mod version;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cli::Task;
use crate::error::{Error, Result};
use crate::invocation::Invocation;
use crate::journal::{End, Journal};
use crate::output::Ci;
use crate::project::Project;
use crate::root::Root;
use crate::runner::{Finished, Runner};
use crate::step::Format;

fn main() -> ExitCode {
    let task = Task::parse();
    let ci = if std::env::var_os("GITHUB_ACTIONS").is_some() {
        Ci::GitHub
    } else {
        Ci::No
    };
    let mut journal = Journal::start(task.name(), &task.args(), ci);
    let (top, result) = match top() {
        Ok(top) => {
            let result = run(&task, &top, ci, &mut journal);
            (Some(top), result)
        }
        Err(error) => (None, Err(error)),
    };
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
        && let Err(error) = journal.write(&dir, top.as_deref(), outcomes, &end)
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

/// The workspace of the folder the driver runs in.
fn top() -> Result<PathBuf> {
    let here = std::env::current_dir().map_err(|source| Error::Io {
        path: PathBuf::from("."),
        source,
    })?;
    Root::workspace_of(&here)
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
    if let Task::Sweep(sweep) = task
        && sweep.format() == Format::Write
        && ci == Ci::GitHub
    {
        return Err(Error::LocalInCi {
            command: sweep.name(),
        });
    }
    let project = Project::load(top)?;
    journal.loaded();
    let invocation = Invocation::new(task, &project)?;
    Ok(Runner::new(&project, ci, invocation.reuse)?.run(&invocation.plans, invocation.on_fail))
}
