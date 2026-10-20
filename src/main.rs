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
//! A run ends on its verdict, one line for an agent that reads only the last: the result, the first
//! cause of a failure, and `target/drive/last-<command>.txt`, which holds every line the run
//! printed. Above it, each failed step shows the first lines it printed. Libtest's passes stay out
//! of the step logs, as nextest's do.
//!
//! `cargo install --locked --git https://github.com/xorza/drive` puts `cargo-drive` on `PATH`,
//! which cargo runs as `cargo drive`. It checks the workspace of the folder it runs in, and each
//! root names the driver it needs in its table's `version`.

#![expect(
    clippy::print_stderr,
    reason = "a command-line tool warns on its terminal"
)]

mod cli;
mod console;
mod context;
mod digest;
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
mod tools;
mod version;

use std::path::PathBuf;
use std::process::ExitCode;

use crate::cli::Task;
use crate::context::Context;
use crate::digest::Digest;
use crate::error::{Error, Result};
use crate::invocation::Invocation;
use crate::journal::{End, Journal, Record};
use crate::output::Ci;
use crate::project::Project;
use crate::runner::{Finished, Runner};
use crate::step::Format;

fn main() -> ExitCode {
    let task = Task::parse();
    // GitHub sets it to `true`; an empty or other value is no runner of its.
    let ci = if std::env::var_os("GITHUB_ACTIONS").is_some_and(|v| v == "true") {
        Ci::GitHub
    } else {
        Ci::No
    };
    let mut journal = Journal::start(task.name(), &task.args(), ci);
    let (here, missing) = match std::env::current_dir() {
        Ok(here) => (here, None),
        Err(source) => (PathBuf::new(), Some(source)),
    };
    let mut cx = Context::new(ci, here);
    let result = match missing {
        None => run(&task, &mut journal, &mut cx),
        Some(source) => Err(Error::Io {
            path: PathBuf::from("."),
            source,
        }),
    };
    let end = match &result {
        Ok(Finished {
            error: Some(error), ..
        })
        | Err(error) => End::Error(error.to_string()),
        Ok(finished) if finished.passed() => End::Passed,
        Ok(_) => End::Failed,
    };
    let finished = result.as_ref().ok();
    let outcomes = finished.map_or(&[][..], |f| f.outcomes.as_slice());
    let project = finished.map(|f| f.top.as_path());
    // The log serves later analysis: a failure to write it fails no check.
    if let Some(dir) = platform::log_dir() {
        let driver = cx.tools.driver().to_owned();
        // Out of a project, the toolchain of the folder the driver ran in.
        let rustc = cx.tools.rustc(project.unwrap_or(&cx.here)).to_owned();
        let record = Record {
            project,
            outcomes,
            end: &end,
            driver: &driver,
            rustc: &rustc,
        };
        if let Err(error) = journal.write(&dir, &record) {
            eprintln!(
                "warning: cannot write the run log in {}: {error}",
                dir.display()
            );
        }
    }
    let error = match &end {
        End::Error(error) => Some(error.as_str()),
        End::Passed | End::Failed => None,
    };
    let console = &mut cx.console;
    for (index, line) in error.unwrap_or_default().lines().enumerate() {
        console.line(&if index == 0 {
            format!("error: {line}")
        } else {
            line.to_owned()
        });
    }
    let title = task.title();
    let file = (finished.filter(|_| console.keeps())).map(|f| f.own.join(task.output_file()));
    let digest = Digest {
        command: &title,
        outcomes,
        error,
        file: file.as_deref(),
        base: &cx.here,
    };
    for line in digest.lines() {
        console.line(&line);
    }
    if let Some(file) = &file
        && let Err(error) = console.save(file)
    {
        eprintln!("warning: cannot write {}: {error}", file.display());
    }
    match end {
        End::Passed => ExitCode::SUCCESS,
        End::Failed => ExitCode::FAILURE,
        End::Error(_) => ExitCode::from(2),
    }
}

/// The task on the workspace that holds the folder the driver runs in.
fn run(task: &Task, journal: &mut Journal, cx: &mut Context) -> Result<Finished> {
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
        && cx.console.ci() == Ci::GitHub
    {
        return Err(Error::LocalInCi {
            command: sweep.name(),
        });
    }
    let project = Project::load(&cx.here)?;
    journal.loaded();
    let invocation = Invocation::new(task, &project)?;
    Ok(Runner::new(&project, invocation.reuse, cx)?.run(&invocation.plans, invocation.on_fail))
}
