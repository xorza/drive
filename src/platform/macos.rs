//! macOS: `~/Library/Logs`, where the system keeps each tool's logs; and a release binary's debug
//! info in a `.dSYM` bundle beside it.

use std::path::{Path, PathBuf};

use crate::platform::{Split, SplitBy, Var};

pub(super) fn journal_dir(var: Var<'_>) -> Option<PathBuf> {
    Some(PathBuf::from(var("HOME")?).join("Library/Logs/drive"))
}

/// Cargo packs each binary's debug info into a `.dSYM` beside it.
pub(super) const PACKAGE_ENV: &[(&str, &str)] =
    &[("CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO", "packed")];

/// `strip -S` of the binary into `out`, and its `.dSYM` copied beside it.
pub(super) fn split_debug(name: &str, built: &Path, out: &Path) -> Vec<Split> {
    vec![
        Split {
            makes: "strip",
            by: SplitBy::Tool {
                program: "strip",
                args: vec![
                    "-S".into(),
                    "-o".into(),
                    out.join(name).into(),
                    built.into(),
                ],
            },
        },
        Split {
            makes: "debug",
            by: SplitBy::Copy {
                from: built.with_file_name(format!("{name}.dSYM")),
                to: out.join(format!("{name}.dSYM")),
            },
        },
    ]
}
