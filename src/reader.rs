use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::error::ConfigError;

/// A member whose tests read files outside its own folder, as a test of the workspace's rules
/// reads every crate, or a test of the citations reads the design documents beside the root: a
/// change to a file it reads is an input, and reaches its tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reader {
    pub(crate) member: String,
    /// The folders and files it reads, absolute, with no `.` or `..`.
    paths: Vec<PathBuf>,
}

/// A `reads` entry as the table gives it: the member, and its paths relative to the root.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReaderTable {
    member: String,
    paths: Vec<String>,
}

impl Reader {
    /// The reader of `table`, its paths taken from `dir`, the root's folder; an error for a
    /// member `is_member` does not know, for no path, and for a path that names nothing, which
    /// no change would ever reach.
    pub(crate) fn resolve(
        table: ReaderTable,
        dir: &Path,
        is_member: impl Fn(&str) -> bool,
    ) -> Result<Self, ConfigError> {
        let ReaderTable { member, paths } = table;
        if !is_member(&member) {
            return Err(ConfigError::UnknownMember {
                key: "reads",
                member,
            });
        }
        if paths.is_empty() {
            return Err(ConfigError::EmptyList { key: "reads.paths" });
        }
        let paths = paths
            .into_iter()
            .map(|path| {
                let resolved = normal(&dir.join(&path));
                if resolved.exists() {
                    Ok(resolved)
                } else {
                    Err(ConfigError::MissingPath { key: "reads", path })
                }
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { member, paths })
    }

    /// Whether it reads `path`, a file a change names.
    pub(crate) fn reads(&self, path: &Path) -> bool {
        self.paths.iter().any(|read| path.starts_with(read))
    }
}

/// `path` with each `.` dropped and each `..` taking off the name before it, read off its names
/// alone: a file a change names may be gone from the disk.
fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod internals {
    use std::path::PathBuf;

    use crate::reader::Reader;

    impl Reader {
        /// A reader of `paths`, absolute and normal, with no look at the disk.
        pub(crate) fn fixture(member: &str, paths: &[&str]) -> Self {
            Self {
                member: member.to_owned(),
                paths: paths.iter().map(PathBuf::from).collect(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn table(value: serde_json::Value) -> ReaderTable {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn paths_resolve_from_the_root_and_must_name_something() {
        // The driver's own folder: `src` is in it, and its parent holds it.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let known = |name: &str| name == "rules";
        let reader = Reader::resolve(
            table(json!({ "member": "rules", "paths": [".", "./src/../src", ".."] })),
            dir,
            known,
        )
        .unwrap();
        let parent = dir.parent().unwrap();
        assert_eq!(
            reader.paths,
            [dir.to_owned(), dir.join("src"), parent.to_owned()]
        );
        assert!(reader.reads(&dir.join("src/main.rs")));
        assert!(reader.reads(&parent.join("docs/x.md")));
        let only_src = Reader::fixture("rules", &["/w/src"]);
        assert!(only_src.reads(Path::new("/w/src/a.rs")));
        assert!(!only_src.reads(Path::new("/w/srcs/a.rs")));

        let fails = |value| {
            Reader::resolve(table(value), dir, known)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            fails(json!({ "member": "app", "paths": ["."] })),
            "drive.reads: `app` names no member"
        );
        assert_eq!(
            fails(json!({ "member": "rules", "paths": [] })),
            "drive.reads.paths: must be a non-empty list"
        );
        assert_eq!(
            fails(json!({ "member": "rules", "paths": ["no-such-folder"] })),
            "drive.reads: `no-such-folder` names no file or folder"
        );
        assert_eq!(normal(Path::new("/w/a/../b/./c")), PathBuf::from("/w/b/c"));
    }
}
