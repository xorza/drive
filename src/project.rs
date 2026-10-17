use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::root::Root;

/// The root the driver sits in, then each root nested in it through a git submodule that a
/// member depends on by path: a change there reaches the top root's builds.
#[derive(Debug)]
pub(crate) struct Project {
    roots: Vec<Root>,
}

impl Project {
    pub(crate) fn load(top: &Path) -> Result<Self> {
        let mut roots = vec![Root::load(top, String::new())?];
        let mut next = 0;
        while next < roots.len() {
            let found: Vec<PathBuf> = roots[next]
                .submodules()
                .into_iter()
                .filter(|sub| roots[next].depends_on(sub) && sub.join("Cargo.toml").is_file())
                .filter(|sub| roots.iter().all(|r| &r.dir != sub))
                .collect();
            for sub in found {
                let rel = sub.strip_prefix(top).unwrap_or(&sub);
                let label = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                roots.push(Root::load(&sub, label)?);
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
