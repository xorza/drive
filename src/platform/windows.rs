//! Windows: the local application data folder, which stays on the machine; and a release
//! binary's debug info in the PDB the linker writes beside it.

use std::path::{Path, PathBuf};

use crate::platform::{Split, SplitBy, Var};

pub(super) fn log_dir(var: Var<'_>) -> Option<PathBuf> {
    Some(
        PathBuf::from(var("LOCALAPPDATA")?)
            .join("drive")
            .join("logs"),
    )
}

/// The MSVC linker writes each binary's PDB beside it already.
pub(super) const PACKAGE_ENV: &[(&str, &str)] = &[];

/// The binary and its PDB, which Cargo names after the crate, `-` taken for `_`, copied to `out`.
pub(super) fn split_debug(name: &str, built: &Path, out: &Path) -> Vec<Split> {
    let pdb = format!("{}.pdb", name.replace('-', "_"));
    let binary = built.file_name().expect("a binary's path names a file");
    vec![
        Split {
            makes: "binary",
            by: SplitBy::Copy {
                from: built.to_owned(),
                to: out.join(binary),
            },
        },
        Split {
            makes: "debug",
            by: SplitBy::Copy {
                from: built.with_file_name(&pdb),
                to: out.join(&pdb),
            },
        },
    ]
}
