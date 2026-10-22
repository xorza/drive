use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::error::{Error, Result};
use crate::repo::Repo;

/// A file of a repository as git lists it, by its path from the repository's top, its names
/// joined by `/`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RepoFile {
    pub(crate) path: String,
    pub(crate) kind: FileKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileKind {
    File,
    /// A symbolic link git tracks, which a read does not follow.
    Link,
    /// A repository inside this one, a submodule or an untracked one, which git lists whole.
    Repository,
}

/// The modes git gives a submodule's entry and a link's.
const SUBMODULE_MODE: &str = "160000";
const LINK_MODE: &str = "120000";

impl RepoFile {
    /// Each file of `repo` that git tracks and the worktree still holds, and each that git would
    /// track, by its path's names in order: a folder's files together, as `a/x` goes before
    /// `a-b/x`.
    pub(crate) fn list(repo: &Repo) -> Result<Vec<Self>> {
        let staged = repo.ls_files(&["--stage"])?;
        let deleted = repo.ls_files(&["--deleted"])?;
        let others = repo.ls_files(&["--others", "--exclude-standard"])?;
        Self::parse(&repo.dir, &staged, &deleted, &others)
    }

    /// The files of `git ls-files -z`'s outputs in `dir`: `staged` of `--stage`, `deleted` of
    /// `--deleted`, and `others` of `--others`.
    fn parse(dir: &Path, staged: &[u8], deleted: &[u8], others: &[u8]) -> Result<Vec<Self>> {
        let deleted: BTreeSet<&str> = records(dir, deleted).collect::<Result<_>>()?;
        let bad_record = |record: &str| Error::GitRecord {
            dir: dir.to_owned(),
            record: record.to_owned(),
        };
        // An entry in conflict has one record for each stage.
        let mut files = BTreeMap::new();
        for record in records(dir, staged) {
            let record = record?;
            let (meta, path) = record.split_once('\t').ok_or_else(|| bad_record(record))?;
            let mode = meta.split(' ').next().ok_or_else(|| bad_record(record))?;
            if deleted.contains(path) {
                continue;
            }
            let kind = match mode {
                SUBMODULE_MODE => FileKind::Repository,
                LINK_MODE => FileKind::Link,
                _ => FileKind::File,
            };
            files.insert(path, kind);
        }
        for path in records(dir, others) {
            let path = path?;
            match path.strip_suffix('/') {
                Some(path) => files.insert(path, FileKind::Repository),
                None => files.insert(path, FileKind::File),
            };
        }
        let mut files: Vec<Self> = files
            .into_iter()
            .map(|(path, kind)| Self {
                path: path.to_owned(),
                kind,
            })
            .collect();
        files.sort_by(|a, b| a.path.split('/').cmp(b.path.split('/')));
        Ok(files)
    }
}

/// The records of a `-z` output in `dir`, each a path or holding one, which must be UTF-8.
fn records<'o>(dir: &'o Path, output: &'o [u8]) -> impl Iterator<Item = Result<&'o str>> {
    output
        .split(|&byte| byte == 0)
        .filter(|record| !record.is_empty())
        .map(move |record| {
            str::from_utf8(record).map_err(|_| Error::NotUtf8Path {
                dir: dir.to_owned(),
                path: String::from_utf8_lossy(record).into_owned(),
            })
        })
}

#[cfg(test)]
pub(crate) mod internals {
    use crate::vocabulary::repo_file::{FileKind, RepoFile};

    impl RepoFile {
        pub(crate) fn fixture(path: &str, kind: FileKind) -> Self {
            Self {
                path: path.to_owned(),
                kind,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process;

    #[test]
    fn the_files_are_those_git_tracks_and_would_track_by_their_names() {
        let staged = concat!(
            "100644 aa 0\tsource/a-b/x.rs\0",
            "100644 bb 0\tsource/a/x.rs\0",
            "160000 cc 0\tsource/drive\0",
            "120000 ab 0\tsource/link.rs\0",
            "100644 dd 1\tgone.rs\0",
            "100644 ee 1\tboth.rs\0",
            "100644 ff 2\tboth.rs\0",
        );
        let top = Path::new("/r");
        let files =
            RepoFile::parse(top, staged.as_bytes(), b"gone.rs\0", b"new.md\0nested/\0").unwrap();
        assert_eq!(
            files,
            [
                RepoFile::fixture("both.rs", FileKind::File),
                RepoFile::fixture("nested", FileKind::Repository),
                RepoFile::fixture("new.md", FileKind::File),
                RepoFile::fixture("source/a/x.rs", FileKind::File),
                RepoFile::fixture("source/a-b/x.rs", FileKind::File),
                RepoFile::fixture("source/drive", FileKind::Repository),
                RepoFile::fixture("source/link.rs", FileKind::Link),
            ]
        );
        assert!(matches!(
            RepoFile::parse(top, b"100644 aa 0 a.rs\0", b"", b""),
            Err(Error::GitRecord { dir, record }) if dir == top && record == "100644 aa 0 a.rs"
        ));
        assert!(matches!(
            RepoFile::parse(top, b"", b"", b"a\xffb\0"),
            Err(Error::NotUtf8Path { dir, path }) if dir == top && path == "a\u{fffd}b"
        ));
    }

    #[test]
    fn a_listing_holds_what_git_tracks_and_would_track_but_not_what_it_ignores() {
        let dir = std::env::temp_dir().join(format!("drive-repo-file-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let run = |args: &[&str]| {
            let status = process::command("git")
                .args(args)
                .current_dir(&dir)
                .status();
            assert!(status.unwrap().success(), "git {args:?}");
        };
        run(&["init", "-q"]);
        std::fs::write(dir.join(".gitignore"), "ignored\n").unwrap();
        for name in ["src/lib.rs", "gone.rs", "ignored"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        run(&["add", "src/lib.rs", "gone.rs"]);
        std::fs::remove_file(dir.join("gone.rs")).unwrap();
        let files = RepoFile::list(&Repo::find(&dir).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            files.unwrap(),
            [
                RepoFile::fixture(".gitignore", FileKind::File),
                RepoFile::fixture("src/lib.rs", FileKind::File)
            ]
        );
        // A folder git holds no repository in.
        let plain = dir.with_extension("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let outside = RepoFile::list(&Repo::fixture(&plain));
        std::fs::remove_dir_all(&plain).unwrap();
        assert!(matches!(outside, Err(Error::Tool { dir, .. }) if dir == plain));
    }
}
