use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::root::Root;

/// The workspace the driver runs in, then the workspace of each path dependency outside it, and so
/// on: each is a root of its own, checked in its own folder, and a change there reaches the
/// builds of the roots that depend on it.
#[derive(Debug)]
pub(crate) struct Project {
    roots: Vec<Root>,
}

impl Project {
    /// The workspace that holds `dir`, and the roots outside it that it reaches.
    pub(crate) fn load(dir: &Path) -> Result<Self> {
        let mut roots = vec![Root::load(dir)?];
        let top = roots[0].dir.clone();
        let mut next = 0;
        while next < roots.len() {
            let outside: Vec<PathBuf> = roots[next]
                .members
                .iter()
                .flat_map(|m| m.outside.clone())
                .collect();
            for dir in outside {
                if roots.iter().any(|r| r.holds(&dir)) {
                    continue;
                }
                let mut found = Root::load(&dir)?;
                if roots.iter().all(|r| r.dir != found.dir) {
                    let prefix = (found.dir.strip_prefix(&top))
                        .unwrap_or_else(|_| Path::new(found.dir.file_name().unwrap_or_default()));
                    found.prefix = prefix
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/");
                    roots.push(found);
                }
            }
            next += 1;
        }
        Ok(Self { roots })
    }

    pub(crate) fn roots(&self) -> &[Root] {
        &self.roots
    }

    pub(crate) fn top(&self) -> &Root {
        &self.roots[0]
    }

    /// The files a tool reads at the top of each root in the repository at `repo_dir`, whether or
    /// not they exist. Every root of the repository shares its tree, so each names them all.
    pub(crate) fn tool_files(&self, repo_dir: &Path) -> Vec<PathBuf> {
        (self.roots.iter())
            .filter(|root| root.repo.as_ref().is_some_and(|repo| repo.dir == repo_dir))
            .flat_map(Root::tool_files)
            .collect()
    }

    /// The roots `root` builds through path dependencies, through other roots too.
    pub(crate) fn outside(&self, root: &Root) -> Vec<&Root> {
        let mut found: Vec<&Root> = Vec::new();
        let mut queue = vec![root];
        while let Some(next) = queue.pop() {
            for other in &self.roots {
                let new =
                    !std::ptr::eq(other, root) && !found.iter().any(|f| std::ptr::eq(*f, other));
                if new && next.depends_on(&other.dir) {
                    found.push(other);
                    queue.push(other);
                }
            }
        }
        found
    }
}

#[cfg(test)]
pub(crate) mod internals {
    use crate::project::Project;
    use crate::root::Root;

    impl Project {
        /// A project of `roots`, the top one first.
        pub(crate) const fn fixture(roots: Vec<Root>) -> Self {
            Self { roots }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Repo;

    #[test]
    fn tool_files_name_every_root_of_the_repository_and_no_other() {
        let at = |dir: &str, top: Option<&str>| {
            let mut root = Root::fixture();
            root.dir = PathBuf::from(dir);
            root.repo = top.map(|top| Repo::fixture(Path::new(top)));
            root
        };
        let project = Project {
            roots: vec![
                at("/w", Some("/w")),
                at("/w/vendor/lib", Some("/w")),
                at("/v", Some("/v")),
                at("/u", None),
            ],
        };
        let files = project.tool_files(Path::new("/w"));
        // Each of the two roots in `/w`'s repository names all 11 files at its top.
        assert_eq!(files.len(), 2 * 11);
        assert_eq!(files[0], Path::new("/w/Cargo.lock"));
        assert_eq!(files[11], Path::new("/w/vendor/lib/Cargo.lock"));
        assert!(files.contains(&PathBuf::from("/w/vendor/lib/.cargo/config.toml")));
        assert!(files.iter().all(|f| f.starts_with("/w")));
        assert_eq!(project.tool_files(Path::new("/v")).len(), 11);
        assert!(project.tool_files(Path::new("/x")).is_empty());
    }
}
