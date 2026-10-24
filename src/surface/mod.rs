//! A package's public API surface: every item it exports, rendered as its declaration from
//! rustdoc JSON, over a build with no features and one build per public feature, in one text
//! file whose order is fixed, so two runs compare by a diff.

pub(crate) mod build;
mod decl;
mod merged_block;
mod renderer;

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use crate::aside;
use crate::error::{Error, Result};
use crate::surface::build::Build;
use crate::surface::merged_block::{BuildIdxs, MergedBlock};
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

    /// The builds that export `path`.
    fn holders(&self, path: &str) -> BuildIdxs {
        self.builds
            .iter()
            .enumerate()
            .filter(|(_, (_, surface))| surface.blocks.contains_key(path))
            .map(|(idx, _)| idx)
            .collect()
    }

    /// The tag of an item or a line `builds` export within `owner`, the builds of the line that
    /// owns it or every build for an item, where the two differ: the features any of which
    /// brings it, or each build of `owner` that drops it.
    fn tag(&self, builds: &BuildIdxs, owner: &BuildIdxs) -> String {
        debug_assert!(
            builds.is_subset(owner),
            "a line is in no build its owner is not"
        );
        if builds == owner {
            return String::new();
        }
        let names = |idxs: &mut dyn Iterator<Item = &usize>, separator: &str| {
            idxs.map(|&idx| self.builds[idx].0.to_string())
                .collect::<Vec<_>>()
                .join(separator)
        };
        let in_bare = builds
            .iter()
            .any(|&idx| self.builds[idx].0 == Build::NoDefault);
        if in_bare {
            format!(
                "  [dropped by: {}]",
                names(&mut owner.difference(builds), ", ")
            )
        } else {
            format!("  [features: {}]", names(&mut builds.iter(), " or "))
        }
    }

    /// `path`'s block over every build that exports it.
    fn merged(&self, path: &str) -> MergedBlock<'_> {
        MergedBlock::from_builds(self.builds.iter().enumerate().filter_map(
            |(idx, (_, surface))| {
                surface
                    .blocks
                    .get(path)
                    .map(|block| (idx, block.as_slice()))
            },
        ))
    }

    /// The surface document: the exports, the prelude, and the public items no path reaches.
    fn markdown(&self) -> String {
        let prelude: BTreeSet<&str> = self
            .builds
            .iter()
            .flat_map(|(_, surface)| surface.prelude.iter().map(String::as_str))
            .collect();
        let prelude: Vec<String> = prelude.iter().map(|name| format!("`{name}`")).collect();
        let mut lines: Vec<String> = [
            "# Public API surface",
            "",
            "Every item the crate exports, rendered as its declaration from rustdoc JSON",
            "(`cargo drive surface`), sorted by path. The surface is the union of a build with",
            "no features and one build per public feature; a tag names the builds an item or a",
            "line needs or loses. `internals` is the crate's own test surface and is left out.",
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
        let every: BuildIdxs = (0..self.builds.len()).collect();
        for path in self.paths() {
            let holders = self.holders(path);
            lines.push(format!("{path}{}", self.tag(&holders, &every)));
            let merged = self.merged(path);
            lines.extend(
                merged
                    .shown(&holders)
                    .into_iter()
                    .map(|line| format!("    {}{}", line.text, self.tag(line.builds, line.owner))),
            );
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

    fn build_surface(blocks: &[(&str, &[&str])]) -> BuildSurface {
        BuildSurface {
            blocks: blocks
                .iter()
                .map(|&(path, lines)| {
                    let lines = lines.iter().map(|&line| line.to_owned()).collect();
                    (path.to_owned(), lines)
                })
                .collect(),
            prelude: BTreeSet::new(),
            unreached: BTreeSet::new(),
        }
    }

    /// The exports of `markdown`, between its first code fence and the next.
    fn exports(surface: &Surface) -> String {
        let text = surface.markdown();
        let start = text.find("```text\n").unwrap() + "```text\n".len();
        let end = start + text[start..].find("```").unwrap();
        text[start..end].to_owned()
    }

    #[test]
    fn a_method_a_feature_adds_is_printed_with_the_features_that_bring_it() {
        let writer = [
            "struct Writer",
            "    impl Writer",
            "        pub fn new() -> Self",
            "        pub fn write(&mut self)",
        ];
        let compressing = [
            "struct Writer",
            "    impl Writer",
            "        pub fn new() -> Self",
            "        pub fn write_compressed(&mut self)",
            "        pub fn write(&mut self)",
        ];
        let feature = |name: &str, writer_block: &[&str]| {
            (
                Build::Feature(name.to_owned()),
                build_surface(&[
                    ("Writer", writer_block),
                    ("compress", &["mod"]),
                    ("plain", &["fn plain()"]),
                ]),
            )
        };
        let surface = Surface {
            builds: vec![
                (
                    Build::NoDefault,
                    build_surface(&[("Writer", &writer), ("plain", &["fn plain()"])]),
                ),
                feature("compression", &compressing),
                feature("parallel", &compressing),
                feature("serde", &writer[..3]),
                (
                    Build::Feature("wasm".to_owned()),
                    build_surface(&[("plain", &["fn plain()"])]),
                ),
            ],
        };
        // `write_compressed` comes with either feature and `compress` too; `serde` drops `write`,
        // which names `serde` alone, as `wasm` drops the whole `Writer`.
        let expected = "\
Writer  [dropped by: wasm]
    struct Writer
        impl Writer
            pub fn new() -> Self
            pub fn write_compressed(&mut self)  [features: compression or parallel]
            pub fn write(&mut self)  [dropped by: serde]

compress  [features: compression or parallel or serde]
    mod

plain
    fn plain()

";
        assert_eq!(exports(&surface), expected);
    }

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
no features and one build per public feature; a tag names the builds an item or a
line needs or loses. `internals` is the crate's own test surface and is left out.

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
