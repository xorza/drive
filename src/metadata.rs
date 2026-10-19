//! The parts of `cargo metadata --format-version 1 --no-deps` the driver reads.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct Metadata {
    pub(crate) packages: Vec<Package>,
    pub(crate) workspace_members: Vec<String>,
    pub(crate) workspace_root: PathBuf,
    pub(crate) target_directory: PathBuf,
    /// `[workspace.metadata]`.
    pub(crate) metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Package {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) manifest_path: PathBuf,
    pub(crate) dependencies: Vec<Dependency>,
    pub(crate) targets: Vec<Target>,
    pub(crate) features: BTreeMap<String, Vec<String>>,
    /// `[package.metadata]`.
    pub(crate) metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Dependency {
    pub(crate) name: String,
    /// `None` for a normal dependency.
    pub(crate) kind: Option<DependencyKind>,
    pub(crate) optional: bool,
    pub(crate) features: Vec<String>,
    /// The folder of a path dependency.
    pub(crate) path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DependencyKind {
    Dev,
    Build,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Target {
    pub(crate) name: String,
    pub(crate) kind: Vec<TargetKind>,
    pub(crate) doctest: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TargetKind {
    Lib,
    Rlib,
    Dylib,
    ProcMacro,
    Bench,
    Bin,
    #[serde(other)]
    Other,
}

impl TargetKind {
    /// A kind whose crate rustdoc can run doc tests on.
    pub(crate) const fn is_library(self) -> bool {
        matches!(self, Self::Lib | Self::Rlib | Self::Dylib | Self::ProcMacro)
    }
}
