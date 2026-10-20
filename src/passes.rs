use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A tree for each repository a step reads: its root's, and those of the roots it builds
/// through path dependencies.
pub(crate) type Snapshot = BTreeMap<PathBuf, String>;

/// Which pass a step reads and keeps: its slot, its root and command, holds one pass, the last;
/// its identity, the tools and variables it ran under, must match for the pass to count. A new
/// toolchain or driver replaces the slot's pass, so the file holds one pass a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PassKey {
    pub(crate) slot: String,
    pub(crate) identity: String,
}

/// The snapshot each step last passed on, by its slot, as one driver kept them: another driver
/// reads none, and its first pass starts the file again, so no pass outlives its driver.
#[derive(Debug)]
pub(crate) struct Passes {
    path: PathBuf,
    file: File,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    driver: String,
    kept: BTreeMap<String, Kept>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    identity: String,
    snapshot: Snapshot,
}

impl Passes {
    /// The passes `driver`, the running driver's identity, kept at `path`.
    pub(crate) fn load(path: PathBuf, driver: &str) -> Self {
        let file = Self::read(&path, driver);
        Self { path, file }
    }

    /// An unreadable file, or one another driver wrote, keeps no pass.
    fn read(path: &PathBuf, driver: &str) -> File {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<File>(&bytes).ok())
            .filter(|file| file.driver == driver)
            .unwrap_or_else(|| File {
                driver: driver.to_owned(),
                kept: BTreeMap::new(),
            })
    }

    pub(crate) fn get(&self, key: &PassKey) -> Option<&Snapshot> {
        let kept = self.file.kept.get(&key.slot)?;
        (kept.identity == key.identity).then_some(&kept.snapshot)
    }

    /// Another run may have kept passes since this one loaded: they stay.
    pub(crate) fn keep(&mut self, key: PassKey, snapshot: Snapshot) -> Result<()> {
        self.file = Self::read(&self.path, &self.file.driver);
        let kept = Kept {
            identity: key.identity,
            snapshot,
        };
        self.file.kept.insert(key.slot, kept);
        let aside = self
            .path
            .with_file_name(format!("passes-{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec(&self.file).expect("a map of strings serializes");
        let io = |source| Error::Io {
            path: self.path.clone(),
            source,
        };
        std::fs::write(&aside, bytes).map_err(io)?;
        std::fs::rename(&aside, &self.path).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(slot: &str, identity: &str) -> PassKey {
        PassKey {
            slot: slot.to_owned(),
            identity: identity.to_owned(),
        }
    }

    #[test]
    fn keep_merges_with_what_another_run_kept_and_replaces_its_slot() {
        let dir = std::env::temp_dir().join(format!("drive-passes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("passes.json");
        let snapshot = |tree: &str| Snapshot::from([(PathBuf::from("/w"), tree.to_owned())]);

        let load = |driver| Passes::load(path.clone(), driver);
        let mut first = load("0.2.1 a");
        let mut second = load("0.2.1 a");
        assert!(first.get(&key("lint", "rustc 1")).is_none());
        first.keep(key("lint", "rustc 1"), snapshot("a")).unwrap();
        second.keep(key("tests", "rustc 1"), snapshot("b")).unwrap();
        let read = load("0.2.1 a");
        assert_eq!(read.get(&key("lint", "rustc 1")), Some(&snapshot("a")));
        assert_eq!(read.get(&key("tests", "rustc 1")), Some(&snapshot("b")));
        // Another identity counts no pass, and its own pass takes the slot.
        assert_eq!(read.get(&key("lint", "rustc 2")), None);
        second.keep(key("lint", "rustc 2"), snapshot("c")).unwrap();
        let read = load("0.2.1 a");
        assert_eq!(read.get(&key("lint", "rustc 2")), Some(&snapshot("c")));
        assert_eq!(read.get(&key("lint", "rustc 1")), None);
        assert_eq!(read.file.kept.len(), 2);
        // Another driver reads no pass, and its first pass drops the old driver's.
        let mut rebuilt = load("0.2.1 b");
        assert!(rebuilt.get(&key("tests", "rustc 1")).is_none());
        rebuilt.keep(key("doc", "rustc 1"), snapshot("d")).unwrap();
        let read = load("0.2.1 b");
        assert_eq!(read.file.kept.keys().collect::<Vec<_>>(), ["doc"]);
        assert!(load("0.2.1 a").file.kept.is_empty());
        // A file of another shape keeps nothing.
        std::fs::write(&path, r#"{"lint":{"/w":"a"}}"#).unwrap();
        assert!(load("0.2.1 b").file.kept.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
