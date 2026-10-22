//! Linux and the other Unix systems: the XDG Base Directory specification's state folder, which
//! holds "actions history (logs, history, recently used files, …)"; and a release binary's debug
//! info moved into a file of its own by `objcopy`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::platform::{Split, SplitBy, Var};

pub(super) fn journal_dir(var: Var<'_>) -> Option<PathBuf> {
    // The specification ignores a relative path in the variable.
    let state = var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let state =
        state.or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(state.join("drive"))
}

/// The build keeps its full debug info in the binary, which `objcopy` then moves out: Cargo's
/// packed split keeps skeleton DWARF in the binary, so a strip of it orphans the `.dwp`.
pub(super) const PACKAGE_ENV: &[(&str, &str)] = &[];

/// `objcopy --only-keep-debug` into `<name>.debug`, then `--strip-debug` with a GNU debug link
/// to that file, as GDB and the distributions' debuginfo packages read it.
pub(super) fn split_debug(name: &str, built: &Path, out: &Path) -> Vec<Split> {
    let debug = out.join(format!("{name}.debug"));
    let mut link = OsString::from("--add-gnu-debuglink=");
    link.push(&debug);
    vec![
        Split {
            makes: "debug",
            by: SplitBy::Tool {
                program: "objcopy",
                args: vec!["--only-keep-debug".into(), built.into(), debug.into()],
            },
        },
        Split {
            makes: "strip",
            by: SplitBy::Tool {
                program: "objcopy",
                args: vec![
                    "--strip-debug".into(),
                    link,
                    built.into(),
                    out.join(name).into(),
                ],
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(vars: &[(&str, &str)]) -> Option<PathBuf> {
        journal_dir(&|name| {
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

    #[test]
    fn objcopy_keeps_the_debug_info_then_strips_it_with_a_link() {
        let split = split_debug("app", Path::new("/t/release/app"), Path::new("/t/package"));
        let args = |at: usize| match &split[at].by {
            SplitBy::Tool { program, args } => (*program, args.clone()),
            SplitBy::Copy { .. } => panic!("objcopy copies nothing"),
        };
        let os = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            args(0),
            (
                "objcopy",
                os(&[
                    "--only-keep-debug",
                    "/t/release/app",
                    "/t/package/app.debug"
                ])
            )
        );
        assert_eq!(
            args(1),
            (
                "objcopy",
                os(&[
                    "--strip-debug",
                    "--add-gnu-debuglink=/t/package/app.debug",
                    "/t/release/app",
                    "/t/package/app"
                ])
            )
        );
        assert_eq!([split[0].makes, split[1].makes], ["debug", "strip"]);
        assert!(PACKAGE_ENV.is_empty());
    }
}
