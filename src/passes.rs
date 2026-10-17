use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::error::{Error, Result};

/// A tree for each repository a step reads: its root's, and those of the roots it builds
/// through path dependencies.
pub(crate) type Snapshot = BTreeMap<PathBuf, String>;

/// The snapshot each step last passed on, by the step's identity: its root, name, command,
/// environment and tool versions.
#[derive(Debug)]
pub(crate) struct Passes {
    path: PathBuf,
    kept: BTreeMap<String, Snapshot>,
}

impl Passes {
    pub(crate) fn load(path: PathBuf) -> Self {
        let kept = Self::read(&path);
        Self { path, kept }
    }

    fn read(path: &PathBuf) -> BTreeMap<String, Snapshot> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Snapshot> {
        self.kept.get(key)
    }

    /// Another run may have kept passes since this one loaded: they stay.
    pub(crate) fn keep(&mut self, key: String, snapshot: Snapshot) -> Result<()> {
        self.kept = Self::read(&self.path);
        self.kept.insert(key, snapshot);
        let aside = self
            .path
            .with_file_name(format!("passes-{}.tmp", std::process::id()));
        let text = serde_json::to_string_pretty(&self.kept).expect("a map of strings serializes");
        let io = |source| Error::Io {
            path: self.path.clone(),
            source,
        };
        std::fs::write(&aside, text).map_err(io)?;
        std::fs::rename(&aside, &self.path).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_merges_with_what_another_run_kept() {
        let dir = std::env::temp_dir().join(format!("drive-passes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("passes.json");
        let snapshot = |tree: &str| Snapshot::from([(PathBuf::from("/w"), tree.to_owned())]);

        let mut first = Passes::load(path.clone());
        let mut second = Passes::load(path.clone());
        assert!(first.get("lint").is_none());
        first.keep("lint".to_owned(), snapshot("a")).unwrap();
        second.keep("tests".to_owned(), snapshot("b")).unwrap();
        let read = Passes::load(path);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read.get("lint"), Some(&snapshot("a")));
        assert_eq!(read.get("tests"), Some(&snapshot("b")));
    }
}
