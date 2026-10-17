//! Linux and the other Unix systems: the XDG Base Directory specification's state folder, which
//! holds "actions history (logs, history, recently used files, …)".

use std::path::PathBuf;

use crate::platform::Var;

pub(super) fn log_dir(var: Var<'_>) -> Option<PathBuf> {
    // The specification ignores a relative path in the variable.
    let state = var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let state =
        state.or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(state.join("drive"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn dir(vars: &[(&str, &str)]) -> Option<PathBuf> {
        log_dir(&|name| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| OsString::from(v))
        })
    }

    #[test]
    fn state_home_then_home() {
        assert_eq!(
            dir(&[("XDG_STATE_HOME", "/s"), ("HOME", "/h")]),
            Some(PathBuf::from("/s/drive"))
        );
        assert_eq!(
            dir(&[("XDG_STATE_HOME", "rel"), ("HOME", "/h")]),
            Some(PathBuf::from("/h/.local/state/drive"))
        );
        assert_eq!(
            dir(&[("HOME", "/h")]),
            Some(PathBuf::from("/h/.local/state/drive"))
        );
        assert_eq!(dir(&[]), None);
    }
}
