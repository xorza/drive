//! What differs between the OSes: where the run log lives, and how a release binary's debug info
//! goes beside it. Each OS's rule is a file of its own, picked here alone.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as os;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as os;

#[cfg(all(unix, not(target_os = "macos")))]
mod xdg;
#[cfg(all(unix, not(target_os = "macos")))]
use xdg as os;

/// The folder of the run log, as the OS's convention places a tool's history; `None` when the
/// environment names no home folder, and then nothing is logged.
pub(crate) fn log_dir() -> Option<PathBuf> {
    os::log_dir(&|name| std::env::var_os(name))
}

/// An environment variable's value, for a rule that reads them.
type Var<'v> = &'v dyn Fn(&str) -> Option<OsString>;

/// The variables of a release build whose binaries `split_debug` then splits, as the OS's way
/// asks of the build.
pub(crate) const fn package_env() -> &'static [(&'static str, &'static str)] {
    os::PACKAGE_ENV
}

/// The moves that put `built`, the release binary `name`, in `out`, without its debug info, and
/// that info beside it in the OS's own form.
pub(crate) fn split_debug(name: &str, built: &Path, out: &Path) -> Vec<Split> {
    os::split_debug(name, built, out)
}

/// One move of a binary's packaging, by what it makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Split {
    /// What it makes, as its step names it: `debug`, `strip` or `binary`.
    pub(crate) makes: &'static str,
    pub(crate) by: SplitBy,
}

/// How a move of a binary's packaging runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SplitBy {
    /// A tool of the OS, by its name on `PATH`, with its arguments.
    Tool {
        program: &'static str,
        args: Vec<OsString>,
    },
    /// A copy of a file or a folder, in place of one at `to`.
    #[cfg_attr(
        all(unix, not(target_os = "macos")),
        expect(
            dead_code,
            reason = "objcopy writes each file there, so no copy is made"
        )
    )]
    Copy { from: PathBuf, to: PathBuf },
}
