//! A package's public API surface: every item it exports, rendered as its declaration from
//! rustdoc JSON, over a build with no features and one build per public feature, in one text
//! file whose order is fixed, so two runs compare by a diff.

pub(crate) mod build;
mod decl;
mod renderer;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

use crate::aside;
use crate::error::{Error, Result};
use crate::surface::build::Build;
use crate::surface::renderer::{BuildSurface, Renderer};

/// Every export path over every build, with the builds that export it.
#[derive(Debug, Default)]
pub(crate) struct Surface {
    builds: Vec<(Build, BuildSurface)>,
}

/// The one field of rustdoc JSON read before the rest, so another format fails on it rather
/// than on a field it renamed.
#[derive(Debug, Deserialize)]
struct FormatVersion {
    format_version: u32,
}

impl Surface {
    /// Adds the exports of `build`, from the rustdoc JSON at `json`.
    pub(crate) fn read(&mut self, build: Build, json: &Path) -> Result<()> {
        let bytes = std::fs::read(json).map_err(|source| Error::Io {
            path: json.to_owned(),
            source,
        })?;
        let parse = |source| Error::RustdocJson {
            path: json.to_owned(),
            source,
        };
        let FormatVersion { format_version } = serde_json::from_slice(&bytes).map_err(parse)?;
        if format_version != rustdoc_types::FORMAT_VERSION {
            return Err(Error::RustdocFormat {
                path: json.to_owned(),
                found: format_version,
            });
        }
        let krate: rustdoc_types::Crate = serde_json::from_slice(&bytes).map_err(parse)?;
        self.builds.push((build, Renderer::render(&krate)));
        Ok(())
    }

    /// Writes the surface document to `out`, making its folder.
    pub(crate) fn write(&self, out: &Path) -> Result<()> {
        let io = |path: &Path| {
            let path = path.to_owned();
            move |source| Error::Io { path, source }
        };
        if let Some(dir) = out.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(io(dir))?;
        }
        aside::write(out, self.markdown()).map_err(io(out))
    }

    fn paths(&self) -> BTreeSet<&str> {
        self.builds
            .iter()
            .flat_map(|(_, surface)| surface.blocks.keys().map(String::as_str))
            .collect()
    }

    pub(crate) fn item_count(&self) -> usize {
        self.paths().len()
    }

    pub(crate) fn unreached(&self) -> BTreeSet<&str> {
        self.builds
            .iter()
            .flat_map(|(_, surface)| surface.unreached.iter().map(String::as_str))
            .collect()
    }

    /// The tag after a path that not every build exports alike: the features any of which
    /// brings it, or every build that drops it, and whether its declaration differs between
    /// them.
    fn tag(&self, path: &str) -> String {
        let holders: Vec<&(Build, BuildSurface)> = self
            .builds
            .iter()
            .filter(|(_, surface)| surface.blocks.contains_key(path))
            .collect();
        let names = |builds: &mut dyn Iterator<Item = &Build>, separator: &str| {
            builds
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(separator)
        };
        let in_bare = holders.iter().any(|(build, _)| *build == Build::NoDefault);
        let mut tag = if !in_bare {
            format!(
                "  [features: {}]",
                names(&mut holders.iter().map(|(build, _)| build), " or ")
            )
        } else if holders.len() < self.builds.len() {
            let dropping = self
                .builds
                .iter()
                .filter(|(_, surface)| !surface.blocks.contains_key(path))
                .map(|(build, _)| build);
            format!("  [dropped by: {}]", names(&mut { dropping }, ", "))
        } else {
            String::new()
        };
        let shapes: BTreeSet<&Vec<String>> = holders
            .iter()
            .map(|(_, surface)| &surface.blocks[path])
            .collect();
        if shapes.len() > 1 {
            tag += "  [shape differs by feature]";
        }
        tag
    }

    /// The surface document: the exports, the prelude, and the public items no path reaches.
    fn markdown(&self) -> String {
        let prelude: BTreeSet<&str> = self
            .builds
            .iter()
            .flat_map(|(_, surface)| surface.prelude.iter().map(String::as_str))
            .collect();
        let prelude: Vec<String> = prelude.iter().map(|name| format!("`{name}`")).collect();
        // The first build to export a path gives its block: the bare build, where it has one.
        let blocks: BTreeMap<&str, &Vec<String>> = self
            .builds
            .iter()
            .rev()
            .flat_map(|(_, surface)| surface.blocks.iter().map(|(p, b)| (p.as_str(), b)))
            .collect();
        let mut lines: Vec<String> = [
            "# Public API surface",
            "",
            "Every item the crate exports, rendered as its declaration from rustdoc JSON",
            "(`cargo drive surface`), sorted by path. The surface is the union of a build with",
            "no features and one build per public feature; a tag names the builds an item needs or",
            "loses. `internals` is the crate's own test surface and is left out.",
            "",
        ]
        .map(String::from)
        .into();
        if !prelude.is_empty() {
            lines.push(format!(
                "`prelude` re-exports these root items: {}.",
                prelude.join(", ")
            ));
            lines.push(String::new());
        }
        lines.push("```text".to_owned());
        for (path, block) in blocks {
            lines.push(format!("{path}{}", self.tag(path)));
            lines.extend(block.iter().map(|line| format!("    {line}")));
            lines.push(String::new());
        }
        lines.extend(
            [
                "```",
                "",
                "## Public but not exported",
                "",
                "Items declared `pub` that no path from the crate root reaches. A caller cannot name",
                "them; each one is either dead surface or a type that leaks through a signature.",
                "",
                "```text",
            ]
            .map(String::from),
        );
        let unreached = self.unreached();
        if unreached.is_empty() {
            lines.push("(none)".to_owned());
        }
        lines.extend(unreached.into_iter().map(String::from));
        lines.push("```".to_owned());
        lines.push(String::new());
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_its_own_format_and_writes_into_a_new_folder() {
        let dir = std::env::temp_dir().join(format!("drive-surface-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json = dir.join("lib.json");
        let mut surface = Surface::default();
        std::fs::write(&json, r#"{ "format_version": 1, "root": 0 }"#).unwrap();
        let other = surface.read(Build::NoDefault, &json).unwrap_err();
        assert!(matches!(other, Error::RustdocFormat { found: 1, .. }));
        std::fs::write(&json, "{").unwrap();
        let broken = surface.read(Build::NoDefault, &json).unwrap_err();
        assert!(matches!(broken, Error::RustdocJson { .. }));
        let version = rustdoc_types::FORMAT_VERSION;
        std::fs::write(&json, format!(r#"{{ "format_version": {version} }}"#)).unwrap();
        let fields = surface.read(Build::NoDefault, &json).unwrap_err();
        assert!(matches!(fields, Error::RustdocJson { .. }));
        assert!(surface.builds.is_empty());

        let out = dir.join("notes").join("API_SURFACE.md");
        surface.write(&out).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        let empty = "\
# Public API surface

Every item the crate exports, rendered as its declaration from rustdoc JSON
(`cargo drive surface`), sorted by path. The surface is the union of a build with
no features and one build per public feature; a tag names the builds an item needs or
loses. `internals` is the crate's own test surface and is left out.

```text
```

## Public but not exported

Items declared `pub` that no path from the crate root reaches. A caller cannot name
them; each one is either dead surface or a type that leaks through a signature.

```text
(none)
```
";
        assert_eq!(text, empty);
        assert_eq!(std::fs::read_dir(out.parent().unwrap()).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
