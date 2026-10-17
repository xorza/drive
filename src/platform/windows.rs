//! Windows: the local application data folder, which stays on the machine.

use std::path::PathBuf;

use crate::platform::Var;

pub(super) fn log_dir(var: Var<'_>) -> Option<PathBuf> {
    Some(
        PathBuf::from(var("LOCALAPPDATA")?)
            .join("drive")
            .join("logs"),
    )
}
