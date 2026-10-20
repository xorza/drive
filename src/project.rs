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
                    let label = (found.dir.strip_prefix(&top))
                        .unwrap_or_else(|_| Path::new(found.dir.file_name().unwrap_or_default()));
                    found.label = label
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
