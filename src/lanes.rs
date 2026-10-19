use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

use crate::error::ConfigError;
use crate::flag_set::{FlagSet, Targets};
use crate::member::Member;
use crate::project_command::{CommandTable, ProjectCommand};
use crate::reads::{Reader, ReaderTable};

/// The OSes a `cross-lint` table may name, as Rust's `std::env::consts::OS` names them.
const CROSS_HOSTS: [&str; 3] = ["linux", "macos", "windows"];

/// A root's lanes and its own commands, from its `drive` table: `[workspace.metadata.drive]`, or
/// `[package.metadata.drive]` in a root that is one package.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Lanes {
    /// `clippy --all-targets`, one lane each.
    pub(crate) lint: Vec<FlagSet>,
    /// nextest, one lane each; the doc tests take the first one's features.
    pub(crate) test: Vec<FlagSet>,
    pub(crate) doc: FlagSet,
    pub(crate) bench: FlagSet,
    /// Rustdoc and the doc tests, which only a table that asks for them runs: rustdoc builds the
    /// docs of each crate a change reaches again, with no incremental cache, and each doc test is
    /// a binary.
    pub(crate) docs: bool,
    pub(crate) test_runner: TestRunner,
    /// Whether the gate runs each bench case once, so a bench that no longer runs fails it.
    pub(crate) gate_benches: bool,
    /// The targets a local gate lints for besides the host's, on this host: the OS of each is
    /// checked on that OS in CI, and a local run catches its code that does not compile.
    pub(crate) cross_lint: Vec<String>,
    /// The binaries `package` ships, each by its name.
    pub(crate) package: Vec<String>,
    pub(crate) reads: Vec<Reader>,
    /// The root's own commands, by name.
    pub(crate) commands: Vec<ProjectCommand>,
}

/// What runs the test lanes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TestRunner {
    /// A process for each test, which ends a stuck one; a change runs only the tests of the crates
    /// it reaches.
    #[default]
    Nextest,
    /// Each test binary under libtest's threads, with no tool to install; a change that reaches
    /// any crate runs every test.
    CargoTest,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    lint: Option<Vec<Set>>,
    test: Option<Vec<Set>>,
    doc: Option<Set>,
    bench: Option<String>,
    docs: Option<bool>,
    #[serde(rename = "test-runner")]
    test_runner: Option<TestRunner>,
    #[serde(rename = "gate-benches", default)]
    gate_benches: bool,
    /// Each host's targets, by the OS's name.
    #[serde(rename = "cross-lint", default)]
    cross_lint: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    package: Vec<String>,
    #[serde(default)]
    reads: Vec<ReaderTable>,
    #[serde(default)]
    commands: BTreeMap<String, CommandTable>,
}

/// Cargo's feature and target flags, or every feature of every member but the ones named.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Set {
    Flags(String),
    AllBut(AllBut),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllBut {
    #[serde(rename = "all-features-but")]
    excluded: Vec<String>,
}

/// Each member's features, by member name, as its manifest declares them.
pub(crate) type Features<'m> = BTreeMap<&'m str, &'m BTreeMap<String, Vec<String>>>;

impl Lanes {
    /// `workspace` is `[workspace.metadata.drive]`, `package` the root package's; `dir` is the
    /// root's folder, which the paths `reads` names are taken from.
    pub(crate) fn from_tables(
        workspace: Option<serde_json::Value>,
        package: Option<serde_json::Value>,
        features: &Features<'_>,
        dir: &Path,
        members: &[Member],
    ) -> Result<Self, ConfigError> {
        let table = match (workspace, package) {
            (Some(_), Some(_)) => return Err(ConfigError::TwoTables),
            (Some(value), None) | (None, Some(value)) => {
                Table::deserialize(value).map_err(ConfigError::Shape)?
            }
            (None, None) => Table::default(),
        };
        let all = || vec![Set::Flags("--all-features".to_owned())];
        let sets = |key: &'static str,
                    sets: Option<Vec<Set>>,
                    targets|
         -> Result<Vec<FlagSet>, ConfigError> {
            let sets = sets.unwrap_or_else(all);
            if sets.is_empty() {
                return Err(ConfigError::EmptyList { key });
            }
            sets.iter()
                .map(|s| s.resolve(key, targets, features))
                .collect()
        };
        let docs = table.docs.unwrap_or_default();
        let doc = match table.doc {
            Some(_) if !docs => return Err(ConfigError::DocWithoutDocs),
            Some(set) => set.resolve("doc", Targets::Refused, features)?,
            None => FlagSet::all_features(),
        };
        if let Some(host) = table
            .cross_lint
            .keys()
            .find(|host| !CROSS_HOSTS.contains(&host.as_str()))
        {
            return Err(ConfigError::UnknownHost { host: host.clone() });
        }
        let binary = |name: &String| members.iter().any(|m| m.binaries.contains(name));
        if let Some(name) = table.package.iter().find(|name| !binary(name)) {
            return Err(ConfigError::UnknownBinary { name: name.clone() });
        }
        let is_member = |name: &str| members.iter().any(|m| m.name == name);
        let reads = (table.reads.into_iter())
            .map(|reader| Reader::resolve(reader, dir, is_member))
            .collect::<Result<_, _>>()?;
        let test_runner = table.test_runner.unwrap_or_default();
        let commands = (table.commands.into_iter())
            .map(|(name, command)| ProjectCommand::resolve(name, command, test_runner))
            .collect::<Result<_, _>>()?;
        let mut cross_lint = table.cross_lint;
        Ok(Self {
            lint: sets("lint", table.lint, Targets::Refused)?,
            test: sets("test", table.test, Targets::Allowed)?,
            doc,
            bench: FlagSet::parse(
                table.bench.as_deref().unwrap_or(""),
                "bench",
                Targets::Refused,
            )?,
            docs,
            test_runner,
            gate_benches: table.gate_benches,
            cross_lint: cross_lint.remove(std::env::consts::OS).unwrap_or_default(),
            package: table.package,
            reads,
            commands,
        })
    }

    /// The root's command `name`.
    pub(crate) fn command(&self, name: &str) -> Option<&ProjectCommand> {
        self.commands.iter().find(|command| command.name == name)
    }
}

impl Set {
    fn resolve(
        &self,
        key: &'static str,
        targets: Targets,
        features: &Features<'_>,
    ) -> Result<FlagSet, ConfigError> {
        match self {
            Self::Flags(text) => FlagSet::parse(text, key, targets),
            Self::AllBut(AllBut { excluded }) => all_features_but(key, excluded, features),
        }
    }
}

/// `--features` with every feature of every member but `excluded`, each `member/feature`.
fn all_features_but(
    key: &'static str,
    excluded: &[String],
    features: &Features<'_>,
) -> Result<FlagSet, ConfigError> {
    for name in excluded {
        let known = name
            .split_once('/')
            .is_some_and(|(m, f)| features.get(m).is_some_and(|fs| fs.contains_key(f)));
        if !known {
            return Err(ConfigError::UnknownFeature {
                key,
                feature: name.clone(),
            });
        }
    }
    let mut on = BTreeSet::new();
    for (member, declared) in features {
        for feature in declared.keys() {
            let name = format!("{member}/{feature}");
            if !excluded.contains(&name) {
                on.insert(name);
            }
        }
    }
    // A feature turns on its own package's features by name, and a member's as `member/feature`
    // or `member?/feature`.
    for name in &on {
        let (member, feature) = name.split_once('/').expect("each name is `member/feature`");
        for enabled in &features[member][feature] {
            let target = match enabled.split_once('/') {
                Some((dep, f)) => format!("{}/{f}", dep.trim_end_matches('?')),
                None if !enabled.starts_with("dep:") => format!("{member}/{enabled}"),
                None => continue,
            };
            if excluded.contains(&target) {
                return Err(ConfigError::FeatureComesBack {
                    key,
                    feature: target,
                    by: name.clone(),
                });
            }
        }
    }
    let list: Vec<String> = on.into_iter().collect();
    Ok(FlagSet {
        features: vec!["--features".to_owned(), list.join(",")],
        targets: Vec::new(),
        but: excluded.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn features() -> BTreeMap<String, BTreeMap<String, Vec<String>>> {
        let owned = |pairs: &[(&str, &[&str])]| -> BTreeMap<String, Vec<String>> {
            pairs
                .iter()
                .map(|&(k, v)| (k.to_owned(), v.iter().map(|&s| s.to_owned()).collect()))
                .collect()
        };
        BTreeMap::from([
            (
                "app".to_owned(),
                owned(&[
                    ("tracy", &["gui/tracy"]),
                    ("internals", &["core/internals", "dep:criterion"]),
                ]),
            ),
            (
                "core".to_owned(),
                owned(&[("internals", &[]), ("ml", &["dep:ort"]), ("real-data", &[])]),
            ),
        ])
    }

    fn lanes(
        workspace: Option<serde_json::Value>,
        package: Option<serde_json::Value>,
    ) -> Result<Lanes, ConfigError> {
        let owned = features();
        let features: Features<'_> = owned.iter().map(|(k, v)| (k.as_str(), v)).collect();
        let member = |name: &str, binaries: &[&str]| Member {
            name: name.to_owned(),
            dir: Path::new("/w").join(name),
            doctest: false,
            benches: false,
            binaries: binaries.iter().map(|&b| b.to_owned()).collect(),
            outside: Vec::new(),
        };
        let members = [member("app", &["app", "tool"]), member("core", &[])];
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        Lanes::from_tables(workspace, package, &features, dir, &members)
    }

    fn lanes_of(workspace: serde_json::Value) -> Lanes {
        lanes(Some(workspace), None).unwrap()
    }

    fn set(text: &str, targets: Targets) -> FlagSet {
        FlagSet::parse(text, "test", targets).unwrap()
    }

    #[test]
    fn defaults_and_either_table() {
        let defaults = lanes(None, None).unwrap();
        assert_eq!(defaults.lint, [set("--all-features", Targets::Refused)]);
        assert_eq!(defaults.test, [set("--all-features", Targets::Refused)]);
        assert_eq!(defaults.doc, set("--all-features", Targets::Refused));
        assert_eq!(defaults.bench, FlagSet::default());
        assert!(!defaults.docs);
        assert!(lanes_of(json!({ "docs": true })).docs);
        assert_eq!(defaults.test_runner, TestRunner::Nextest);
        assert!(!defaults.gate_benches && defaults.cross_lint.is_empty());
        assert!(defaults.package.is_empty() && defaults.reads.is_empty());
        assert!(defaults.commands.is_empty());
        assert_eq!(
            lanes_of(json!({ "test-runner": "cargo-test" })).test_runner,
            TestRunner::CargoTest
        );

        let table = json!({ "lint": ["--all-features", ""], "test": ["--lib --test alloc"], "bench": "--features wgpu" });
        let from_package = lanes(None, Some(table.clone())).unwrap();
        assert_eq!(from_package, lanes(Some(table), None).unwrap());
        assert_eq!(
            from_package.lint,
            [set("--all-features", Targets::Refused), FlagSet::default()]
        );
        assert_eq!(
            from_package.test,
            [set("--lib --test alloc", Targets::Allowed)]
        );
        assert_eq!(from_package.bench, set("--features wgpu", Targets::Refused));
    }

    #[test]
    fn all_features_but_lists_every_other_member_feature() {
        let table = json!({ "test": [{ "all-features-but": ["core/real-data", "app/tracy"] }] });
        // app/tracy turns on `gui/tracy`, a feature of no member, and is left out by name.
        let test = lanes(Some(table), None).unwrap().test;
        let expected = FlagSet {
            but: vec!["core/real-data".to_owned(), "app/tracy".to_owned()],
            ..set(
                "--features app/internals,core/internals,core/ml",
                Targets::Refused,
            )
        };
        assert_eq!(test, [expected]);
        assert_eq!(
            test[0].label(),
            "all features but core/real-data, app/tracy"
        );
        assert_eq!(test[0].features_only(), test[0]);
    }

    #[test]
    fn the_root_s_own_keys() {
        let own = lanes_of(json!({
            "gate-benches": true,
            "cross-lint": { (std::env::consts::OS): ["x86_64-pc-windows-gnu"] },
            "package": ["tool", "app"],
            "reads": [{ "member": "core", "paths": ["src"] }],
            "commands": {
                "lan": { "joins": "gate", "steps": [{ "build": "build -p app" }] },
                "index": { "steps": [{ "tests": "test(=index)" }] },
            },
        }));
        assert!(own.gate_benches);
        // The targets of this host's OS, the one OS a run reads.
        assert_eq!(own.cross_lint, ["x86_64-pc-windows-gnu"]);
        assert_eq!(own.package, ["tool", "app"]);
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
        assert!(own.reads[0].reads(&src));
        // The commands, by name.
        let names: Vec<&str> = own.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["index", "lan"]);
        assert_eq!(own.command("lan").map(|c| c.steps.len()), Some(1));
        assert!(own.command("zero-hour").is_none());
        let other = if std::env::consts::OS == "linux" {
            "macos"
        } else {
            "linux"
        };
        assert!(
            lanes_of(json!({ "cross-lint": { (other): ["t"] } }))
                .cross_lint
                .is_empty()
        );

        let fails =
            |workspace: serde_json::Value| lanes(Some(workspace), None).unwrap_err().to_string();
        assert_eq!(
            fails(json!({ "cross-lint": { "plan9": ["t"] } })),
            "drive.cross-lint: `plan9` is not `linux`, `macos` or `windows`"
        );
        assert_eq!(
            fails(json!({ "package": ["core"] })),
            "drive.package: `core` names no binary of a member"
        );
        assert_eq!(
            fails(json!({ "reads": [{ "member": "gui", "paths": ["."] }] })),
            "drive.reads: `gui` names no member"
        );
    }

    #[test]
    fn refuses_a_bad_table() {
        let fails =
            |workspace: serde_json::Value| lanes(Some(workspace), None).unwrap_err().to_string();
        assert_eq!(
            fails(json!({ "lint": [] })),
            "drive.lint: must be a non-empty list"
        );
        assert_eq!(
            fails(json!({ "doc": "--lib", "docs": true })),
            "drive.doc: `--lib` is not a feature or target flag"
        );
        for docs in [json!({ "doc": "" }), json!({ "doc": "", "docs": false })] {
            assert_eq!(
                fails(docs),
                "drive.doc: rustdoc runs only with `docs = true`"
            );
        }
        assert_eq!(
            lanes_of(json!({ "doc": "--features gpu", "docs": true })).doc,
            set("--features gpu", Targets::Refused)
        );
        assert_eq!(
            fails(json!({ "test": [{ "all-features-but": ["core/gpu"] }] })),
            "drive.test: `core/gpu` names no feature of a member"
        );
        // app/internals turns on core/internals.
        assert_eq!(
            fails(json!({ "test": [{ "all-features-but": ["core/internals"] }] })),
            "drive.test: `app/internals` turns `core/internals` on again"
        );
        assert!(fails(json!({ "tests": [] })).starts_with("drive table: unknown field `tests`"));
        assert_eq!(
            fails(json!({ "lint": [] })),
            "drive.lint: must be a non-empty list"
        );
        // The lanes of each command are fixed.
        assert!(fails(json!({ "gate": ["fmt"] })).starts_with("drive table: unknown field `gate`"));
        assert!(matches!(
            lanes(Some(json!({})), Some(json!({}))),
            Err(ConfigError::TwoTables)
        ));
    }
}
