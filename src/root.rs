use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::Repo;
use crate::lanes::{Features, Lanes};
use crate::member::Member;
use crate::metadata::Metadata;
use crate::tool;

/// A file at a root's top whose change can alter every member's build.
const ROOT_WIDE: [&str; 8] = [
    "Cargo.lock",
    "Cargo.toml",
    "clippy.toml",
    ".clippy.toml",
    "rustfmt.toml",
    ".rustfmt.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
];

/// A Cargo workspace: one folder, one lockfile, one target folder, and its lanes.
#[derive(Debug)]
pub(crate) struct Root {
    pub(crate) dir: PathBuf,
    /// Its folder relative to the top root's, empty for the top root: the prefix of its steps.
    pub(crate) label: String,
    pub(crate) members: Vec<Member>,
    /// The members that depend on each member, by a dependency of any kind.
    dependents: BTreeMap<String, BTreeSet<String>>,
    /// Each member whose build that ships turns `internals` on, which the ship lane leaves out.
    pub(crate) unifies_internals: Vec<String>,
    pub(crate) target_dir: PathBuf,
    pub(crate) lanes: Lanes,
    pub(crate) repo: Option<Repo>,
}

/// The members whose tests a change can alter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reach {
    /// Every member: a manifest, the lock, a lint or toolchain setting, or a file in no member.
    All,
    Members(BTreeSet<String>),
}

impl Root {
    pub(crate) fn load(dir: &Path, label: String) -> Result<Self> {
        let mut cargo = tool::cargo();
        cargo.args(["metadata", "--format-version", "1", "--no-deps"]);
        let json = tool::stdout(&mut cargo, dir)?;
        let meta: Metadata = serde_json::from_str(&json).map_err(|source| Error::Metadata {
            dir: dir.to_owned(),
            source,
        })?;
        Self::from_metadata(meta, label)
    }

    fn from_metadata(meta: Metadata, label: String) -> Result<Self> {
        let packages: Vec<_> = meta
            .packages
            .iter()
            .filter(|p| meta.workspace_members.contains(&p.id))
            .collect();
        let names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        let mut dependents: BTreeMap<String, BTreeSet<String>> = names
            .iter()
            .map(|&n| (n.to_owned(), BTreeSet::new()))
            .collect();
        // The members each member's build that ships builds, and the ones that turn `internals`
        // on in a member they build so.
        let mut normal_dependents = dependents.clone();
        let mut unifying = BTreeSet::new();
        for package in &packages {
            for dep in package
                .dependencies
                .iter()
                .filter(|d| names.contains(&d.name.as_str()))
            {
                dependents
                    .get_mut(&dep.name)
                    .expect("a member")
                    .insert(package.name.clone());
                if dep.kind.is_none() && !dep.optional {
                    normal_dependents
                        .get_mut(&dep.name)
                        .expect("a member")
                        .insert(package.name.clone());
                    if dep.features.iter().any(|f| f == "internals") {
                        unifying.insert(package.name.clone());
                    }
                }
            }
        }
        let root_package = packages
            .iter()
            .find(|p| p.manifest_path.parent() == Some(meta.workspace_root.as_path()));
        let features: Features<'_> = packages
            .iter()
            .map(|p| (p.name.as_str(), &p.features))
            .collect();
        let drive = |m: Option<&serde_json::Value>| m.and_then(|m| m.get("drive")).cloned();
        let manifest = meta.workspace_root.join("Cargo.toml");
        let members: Vec<Member> = packages.iter().map(|p| Member::new(p, &names)).collect();
        let lanes = Lanes::from_tables(
            drive(meta.metadata.as_ref()),
            drive(root_package.and_then(|p| p.metadata.as_ref())),
            &features,
            &meta.workspace_root,
            &members,
        )
        .map_err(|reason| Error::Config { manifest, reason })?;
        Ok(Self {
            repo: Repo::find(&meta.workspace_root),
            members,
            unifies_internals: closure(unifying, &normal_dependents).into_iter().collect(),
            dependents,
            target_dir: meta.target_directory,
            lanes,
            dir: meta.workspace_root,
            label,
        })
    }

    /// The step name with this root's prefix.
    pub(crate) fn step_name(&self, name: &str) -> String {
        if self.label.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", self.label)
        }
    }

    /// What a change to these files reaches, the files of `outside` roots this one depends on
    /// among them; `None` when no file is an input.
    pub(crate) fn reach(&self, paths: &[PathBuf], outside: &[&Self]) -> Option<Reach> {
        let mut owners = BTreeSet::new();
        // The members whose tests read a changed file outside their own folders: their tests
        // alone, not those of the members that depend on them.
        let mut readers = BTreeSet::new();
        let mut input = false;
        for path in paths {
            for reader in &self.lanes.reads {
                if reader.reads(path) {
                    input = true;
                    readers.insert(reader.member.clone());
                }
            }
            let nested = outside
                .iter()
                .filter(|o| path.starts_with(&o.dir))
                .max_by_key(|o| o.dir.components().count());
            if let Some(nested) = nested {
                if nested.is_input(path) {
                    input = true;
                    let direct: Vec<&Member> = self
                        .members
                        .iter()
                        .filter(|m| m.outside.iter().any(|d| d.starts_with(&nested.dir)))
                        .collect();
                    // A root reached only through another root: which members it reaches is that
                    // root's to tell.
                    if direct.is_empty() {
                        return Some(Reach::All);
                    }
                    owners.extend(direct.iter().map(|m| m.name.clone()));
                }
                continue;
            }
            if !path.starts_with(&self.dir) || !self.is_input(path) {
                continue;
            }
            input = true;
            let rel = path.strip_prefix(&self.dir).expect("checked above");
            let root_wide = rel.starts_with(".cargo")
                || rel.starts_with(".config")
                || ROOT_WIDE.iter().any(|f| rel == Path::new(f));
            let holder = self
                .members
                .iter()
                .filter(|m| path.starts_with(&m.dir))
                .max_by_key(|m| m.dir.components().count());
            match holder {
                Some(member)
                    if !root_wide && path.file_name().is_none_or(|n| n != "Cargo.toml") =>
                {
                    owners.insert(member.name.clone());
                }
                _ => return Some(Reach::All),
            }
        }
        input.then(|| {
            let mut reached = closure(owners, &self.dependents);
            reached.extend(readers);
            Reach::Members(reached)
        })
    }

    /// Whether a file is one the builds and tests read: not in a hidden folder but `.cargo` and
    /// `.config`, which hold cargo's and nextest's settings, no file of git's own, and no agent's
    /// instructions.
    fn is_input(&self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.dir) else {
            return false;
        };
        let names: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let Some((file, folders)) = names.split_last() else {
            return false;
        };
        let settings =
            |i: usize, folder: &str| i == 0 && (folder == ".cargo" || folder == ".config");
        let hidden = folders
            .iter()
            .enumerate()
            .any(|(i, f)| f.starts_with('.') && !settings(i, f));
        // Git's own files: a change to what git tracks shows as the files it adds or drops.
        let git = file.starts_with(".git");
        !hidden && !git && file != "AGENTS.md" && file != "CLAUDE.md"
    }

    /// The folder of the workspace that holds the package in `dir`.
    pub(crate) fn workspace_of(dir: &Path) -> Result<PathBuf> {
        let mut cargo = tool::cargo();
        cargo.args(["locate-project", "--workspace", "--message-format", "plain"]);
        let manifest = tool::stdout(&mut cargo, dir)?;
        let manifest = Path::new(manifest.trim());
        Ok(manifest
            .parent()
            .expect("a manifest path names a file")
            .to_owned())
    }

    /// Whether a member's folder holds `dir`.
    pub(crate) fn holds(&self, dir: &Path) -> bool {
        self.members.iter().any(|m| dir.starts_with(&m.dir))
    }

    /// Whether a member depends by path on a crate in `dir`.
    pub(crate) fn depends_on(&self, dir: &Path) -> bool {
        self.members
            .iter()
            .any(|m| m.outside.iter().any(|d| d.starts_with(dir)))
    }
}

/// `start` and every name its `edges` reach.
fn closure(
    start: BTreeSet<String>,
    edges: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut found = start;
    let mut queue: Vec<String> = found.iter().cloned().collect();
    while let Some(name) = queue.pop() {
        for next in &edges[&name] {
            if found.insert(next.clone()) {
                queue.push(next.clone());
            }
        }
    }
    found
}

#[cfg(test)]
pub(crate) mod internals {
    use serde_json::json;

    use crate::metadata::Metadata;
    use crate::root::Root;
    use crate::version::{Requirement, Version};

    impl Root {
        /// A root at `/w`: `core`, `app` on `core` (dev-dependency on `core` with `internals`),
        /// `harness` on `core` with `internals`, `check` on `harness`, and `app` on `/w/gui`
        /// by path.
        pub(crate) fn fixture() -> Self {
            let dep = |name: &str, kind: Option<&str>, features: &[&str], path: Option<&str>| json!({ "name": name, "kind": kind, "optional": false, "features": features, "path": path });
            let package = |name: &str,
                           dir: &str,
                           deps: Vec<serde_json::Value>,
                           targets: serde_json::Value| {
                json!({
                    "id": name, "name": name, "manifest_path": format!("{dir}/Cargo.toml"),
                    "dependencies": deps, "targets": targets, "features": {}, "metadata": null,
                })
            };
            let lib = json!([{ "name": "lib", "kind": ["lib"], "doctest": true }]);
            let bin = json!([{ "name": "app", "kind": ["bin"], "doctest": false }, { "name": "app", "kind": ["bench"], "doctest": false }]);
            let meta = json!({
                "packages": [
                    package("core", "/w/core", vec![], lib.clone()),
                    package("app", "/w/app", vec![
                        dep("core", None, &[], Some("/w/core")),
                        dep("core", Some("dev"), &["internals"], Some("/w/core")),
                        dep("gui", None, &[], Some("/w/gui")),
                    ], bin),
                    package("harness", "/w/harness", vec![dep("core", None, &["internals"], Some("/w/core"))], lib),
                    package("check", "/w/check", vec![dep("harness", None, &[], Some("/w/harness"))], json!([])),
                ],
                "workspace_members": ["core", "app", "harness", "check"],
                "workspace_root": "/w",
                "target_directory": "/w/target",
                "metadata": { "drive": { "version": Requirement::of(Version::DRIVER).to_string() } },
            });
            let meta: Metadata = serde_json::from_value(meta).unwrap();
            Self::from_metadata(meta, String::new()).unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reads::Reader;

    fn reach(root: &Root, paths: &[&str], outside: &[&Root]) -> Option<Reach> {
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        root.reach(&paths, outside)
    }

    fn members(names: &[&str]) -> Reach {
        Reach::Members(names.iter().map(|&n| n.to_owned()).collect())
    }

    #[test]
    fn graph_from_metadata() {
        let root = Root::fixture();
        // harness turns `internals` on in core as a normal dependency; check builds harness.
        assert_eq!(root.unifies_internals, ["check", "harness"]);
        assert_eq!(
            root.members
                .iter()
                .map(|m| (m.name.as_str(), m.doctest, m.benches))
                .collect::<Vec<_>>(),
            [
                ("core", true, false),
                ("app", false, true),
                ("harness", true, false),
                ("check", false, false),
            ]
        );
        assert_eq!(root.members[1].outside, [PathBuf::from("/w/gui")]);
        assert!(root.depends_on(Path::new("/w/gui")));
        assert!(root.holds(Path::new("/w/core/src")));
        assert!(!root.holds(Path::new("/w/gui")));
        assert!(!root.depends_on(Path::new("/w/other")));
        assert_eq!(root.step_name("lint"), "lint");
    }

    #[test]
    fn reach_follows_dependents() {
        let root = Root::fixture();
        assert_eq!(
            reach(&root, &["/w/core/src/lib.rs"], &[]),
            Some(members(&["app", "check", "core", "harness"]))
        );
        assert_eq!(
            reach(&root, &["/w/harness/src/lib.rs"], &[]),
            Some(members(&["check", "harness"]))
        );
        assert_eq!(
            reach(&root, &["/w/app/src/main.rs", "/w/check/x.rs"], &[]),
            Some(members(&["app", "check"]))
        );
    }

    #[test]
    fn reach_of_files_outside_members() {
        let root = Root::fixture();
        for wide in [
            "/w/Cargo.lock",
            "/w/Cargo.toml",
            "/w/core/Cargo.toml",
            "/w/.cargo/config.toml",
            "/w/.config/nextest.toml",
            "/w/clippy.toml",
            "/w/README.md",
            "/w/docs/design.md",
        ] {
            assert_eq!(reach(&root, &[wide], &[]), Some(Reach::All), "{wide}");
        }
        // Hidden folders, agents' instructions, and files outside the root are no input.
        assert_eq!(
            reach(
                &root,
                &[
                    "/w/.gitmodules",
                    "/w/core/.gitignore",
                    "/w/.notes/ISSUES.md",
                    "/w/core/.zed/x",
                    "/w/AGENTS.md",
                    "/w/core/AGENTS.md",
                    "/elsewhere/x.rs"
                ],
                &[]
            ),
            None
        );
        assert_eq!(reach(&root, &[], &[]), None);
    }

    #[test]
    fn a_reader_s_files_reach_its_tests_alone() {
        let mut root = Root::fixture();
        // `check` reads the documents beside the root, and `core` every file of the root.
        root.lanes.reads = vec![
            Reader::fixture("check", &["/docs"]),
            Reader::fixture("core", &["/w"]),
        ];
        assert_eq!(
            reach(&root, &["/docs/design.md"], &[]),
            Some(members(&["check"]))
        );
        // A member's file reaches it and its dependents, and the root's reader of every file;
        // an agent's instructions, no input of the builds, still reach the reader of them.
        assert_eq!(
            reach(&root, &["/w/harness/src/lib.rs"], &[]),
            Some(members(&["check", "core", "harness"]))
        );
        assert_eq!(
            reach(&root, &["/w/AGENTS.md"], &[]),
            Some(members(&["core"]))
        );
        assert_eq!(reach(&root, &["/elsewhere/x.rs"], &[]), None);
    }

    #[test]
    fn reach_into_an_outside_root() {
        let root = Root::fixture();
        let mut gui = Root::fixture();
        gui.dir = PathBuf::from("/w/gui");
        assert_eq!(
            reach(&root, &["/w/gui/src/lib.rs"], &[&gui]),
            Some(members(&["app"]))
        );
        assert_eq!(reach(&root, &["/w/gui/AGENTS.md"], &[&gui]), None);
        // A root inside gui, which the root reaches only through gui: the inner one holds the
        // file, whichever comes first.
        let mut theme = Root::fixture();
        theme.dir = PathBuf::from("/w/gui/theme");
        assert_eq!(
            reach(&root, &["/w/gui/theme/src/lib.rs"], &[&gui, &theme]),
            Some(Reach::All)
        );
        assert_eq!(
            reach(&root, &["/w/gui/theme/src/lib.rs"], &[&theme, &gui]),
            Some(Reach::All)
        );
        assert_eq!(
            reach(&root, &["/w/gui/src/lib.rs"], &[&theme, &gui]),
            Some(members(&["app"]))
        );
    }
}
