//! The git trees a step's passes are keyed on.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::tool;

/// A repository: its top folder and its index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repo {
    pub(crate) top: PathBuf,
    index: PathBuf,
}

impl Repo {
    /// The repository that holds `dir`; `None` outside git.
    pub(crate) fn find(dir: &Path) -> Option<Self> {
        let paths = git(
            dir,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-path",
                "index",
            ],
            &[],
        )?;
        // One line for each option, in their order.
        let mut lines = paths.lines();
        let (top, index) = (lines.next()?, lines.next()?);
        Some(Self {
            top: PathBuf::from(top),
            index: PathBuf::from(index),
        })
    }

    /// The id of git's tree of the working files as they stand, tracked and new alike, as
    /// `.gitignore` leaves them. It is written through a copy of the index that `scratch` names,
    /// the process's own, so the index stays as it is and runs beside this one share no file.
    pub(crate) fn tree(&self, scratch: &Path) -> Result<Option<String>> {
        let io = |source| Error::Io {
            path: scratch.to_owned(),
            source,
        };
        match std::fs::copy(&self.index, scratch) {
            Ok(_) => {}
            // A repository with nothing staged yet has no index.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
        let env = [("GIT_INDEX_FILE", scratch.as_os_str())];
        let tree = git(&self.top, &["add", "--all", "--", ":/"], &env)
            .and_then(|_| git(&self.top, &["write-tree"], &env));
        match std::fs::remove_file(scratch) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
        Ok(tree.map(|t| t.trim().to_owned()))
    }

    /// The files that differ between two trees; `None` when git cannot compare them, as when
    /// one is gone.
    pub(crate) fn changed(&self, from: &str, to: &str) -> Option<Vec<PathBuf>> {
        let names = git(
            &self.top,
            &["diff", "--name-only", "--no-renames", "-z", from, to],
            &[],
        )?;
        Some(
            names
                .split('\0')
                .filter(|n| !n.is_empty())
                .map(|n| self.top.join(n))
                .collect(),
        )
    }
}

/// A git command's stdout; `None` when it fails.
fn git(dir: &Path, args: &[&str], env: &[(&str, &std::ffi::OsStr)]) -> Option<String> {
    let out = tool::command("git")
        .args(args)
        .envs(env.iter().copied())
        .current_dir(dir)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_follows_the_working_files() {
        let dir = std::env::temp_dir().join(format!("drive-git-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| assert!(git(&dir, args, &[]).is_some(), "git {args:?}");
        run(&["init", "-q"]);
        std::fs::write(dir.join("a.rs"), "a").unwrap();
        std::fs::write(dir.join(".gitignore"), "ignored\n").unwrap();
        let repo = Repo::find(&dir).unwrap();
        let top = PathBuf::from(
            git(&dir, &["rev-parse", "--show-toplevel"], &[])
                .unwrap()
                .trim(),
        );
        assert_eq!((&repo.top, &repo.index), (&top, &top.join(".git/index")));
        let scratch = dir.join("scratch-index");

        // Nothing is staged, so the repository has no index yet; new files count all the same.
        let first = repo.tree(&scratch).unwrap().unwrap();
        assert!(!scratch.exists());
        std::fs::write(dir.join("ignored"), "x").unwrap();
        assert_eq!(repo.tree(&scratch).unwrap().unwrap(), first);
        std::fs::write(dir.join("a.rs"), "b").unwrap();
        std::fs::write(dir.join("c.rs"), "c").unwrap();
        let second = repo.tree(&scratch).unwrap().unwrap();
        let changed = repo.changed(&first, &second).unwrap();
        // The real index stays as it was: nothing staged.
        assert_eq!(
            git(&dir, &["diff", "--cached", "--name-only"], &[]).unwrap(),
            ""
        );
        assert!(
            repo.changed(&first, "0000000000000000000000000000000000000000")
                .is_none()
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(changed, [repo.top.join("a.rs"), repo.top.join("c.rs")]);
    }
}
