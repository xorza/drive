use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::process;
use crate::version;

/// What changes a step's result from outside its working files, each read once a run, when a step
/// or the run log first asks: the driver's build, the rustc of each root, nextest, and the
/// variables cargo and rustc read.
#[derive(Debug, Default)]
pub(crate) struct Tools {
    driver: Option<String>,
    /// By the root's folder: a `rust-toolchain.toml` picks the toolchain of the folder it is in.
    rustc: BTreeMap<PathBuf, String>,
    nextest: Option<String>,
    env: Option<BTreeMap<String, Option<String>>>,
}

impl Tools {
    pub(crate) fn driver(&mut self) -> &str {
        self.driver.get_or_insert_with(version::identity)
    }

    pub(crate) fn rustc(&mut self, dir: &Path) -> &str {
        self.rustc.entry(dir.to_owned()).or_insert_with(|| {
            process::first_line(process::command("rustc").arg("-V").current_dir(dir))
        })
    }

    pub(crate) fn nextest(&mut self, dir: &Path) -> &str {
        self.nextest.get_or_insert_with(|| {
            process::first_line(
                process::cargo()
                    .args(["nextest", "--version"])
                    .current_dir(dir),
            )
        })
    }

    /// Each `RUST*` and `CARGO*` variable but the driver's own. `vars` panics on any variable
    /// that is not UTF-8, also one no step reads; each value keeps its bytes escaped, so no two
    /// values, UTF-8 or not, read alike.
    pub(crate) fn env(&mut self) -> &BTreeMap<String, Option<String>> {
        self.env.get_or_insert_with(|| {
            std::env::vars_os()
                .filter(|(name, _)| !process::is_drivers_own(name))
                .filter_map(|(name, value)| {
                    let name = name.into_string().ok()?;
                    let value = value.as_encoded_bytes().escape_ascii().to_string();
                    (name.starts_with("RUST") || name.starts_with("CARGO"))
                        .then_some((name, Some(value)))
                })
                .collect()
        })
    }
}
