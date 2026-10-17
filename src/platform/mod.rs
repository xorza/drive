//! What differs between the OSes: where the run log lives. Each OS's rule is a file of its own,
//! picked here alone.

use std::ffi::OsString;
use std::path::PathBuf;

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
