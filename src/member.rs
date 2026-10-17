use std::path::PathBuf;

use crate::metadata::{self, TargetKind};

/// A workspace member, as much of it as the lanes and the reach of a change read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) name: String,
    pub(crate) dir: PathBuf,
    /// A library target rustdoc runs doc tests on.
    pub(crate) doctest: bool,
    pub(crate) benches: bool,
    /// The folders of its path dependencies outside the workspace.
    pub(crate) outside: Vec<PathBuf>,
}

impl Member {
    pub(crate) fn new(package: &metadata::Package, members: &[&str]) -> Self {
        let targets = &package.targets;
        Self {
            name: package.name.clone(),
            dir: package
                .manifest_path
                .parent()
                .expect("a manifest path names a file")
                .to_owned(),
            doctest: targets
                .iter()
                .any(|t| t.doctest && t.kind.iter().any(|k| k.is_library())),
            benches: targets.iter().any(|t| t.kind.contains(&TargetKind::Bench)),
            outside: package
                .dependencies
                .iter()
                .filter(|d| !members.contains(&d.name.as_str()))
                .filter_map(|d| d.path.clone())
                .collect(),
        }
    }
}
