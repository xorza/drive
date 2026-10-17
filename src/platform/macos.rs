//! macOS: `~/Library/Logs`, where the system keeps each tool's logs.

use std::path::PathBuf;

use crate::platform::Var;

pub(super) fn log_dir(var: Var<'_>) -> Option<PathBuf> {
    Some(PathBuf::from(var("HOME")?).join("Library/Logs/drive"))
}
