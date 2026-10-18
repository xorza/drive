use crate::flag_set::FlagSet;
use crate::lanes::TestRunner;
use crate::root::Root;
use crate::sweep::Sweep;

/// Cargo's messages as JSON, each diagnostic short: the driver tells a lint from code that does
/// not compile, and prints each diagnostic once. The format is in no fingerprint.
const JSON: &str = "--message-format=json-diagnostic-short";
/// Clippy's arguments after `--` are in each unit's fingerprint, so every lint passes these.
const DENY: [&str; 3] = ["--", "-D", "warnings"];
const QUIET_NEXTEST: [&str; 10] = [
    "--show-progress",
    "none",
    "--status-level",
    "fail",
    "--final-status-level",
    "slow",
    "--failure-output",
    "final",
    "--success-output",
    "never",
];

/// What a step runs, which says what its output holds and what its failure means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `cargo fmt`, which builds nothing.
    Format,
    /// A build or a lint, which fails only through the diagnostics it reports.
    Check,
    /// A nextest run: a build, then the tests.
    Nextest,
    /// A `cargo test` run: a build, then each test binary under libtest.
    CargoTest,
    /// `cargo test --doc`: a build, then the doc tests.
    Doctests,
    /// A program whose output is its own: a bench.
    Program,
}

impl Kind {
    pub(crate) const fn diagnoses(self) -> bool {
        matches!(
            self,
            Self::Check | Self::Nextest | Self::CargoTest | Self::Doctests
        )
    }

    /// A run whose results libtest prints, `test result:` lines and all.
    pub(crate) const fn libtest(self) -> bool {
        matches!(self, Self::CargoTest | Self::Doctests)
    }
}

/// When a step runs again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Runs {
    /// When an input changed since it passed; a test step runs the tests the change reaches.
    OnChange,
    /// Each time, every test it names; a pass it makes is still kept for later commands.
    Always,
}

/// Whether `cargo fmt` writes files or lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Write,
    Check,
}

/// One cargo command on one root. Every lane's command is built here alone: each shape of a
/// cargo command keeps a cache of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    /// Cargo's arguments.
    pub(crate) argv: Vec<String>,
    /// Variables set, or removed for `None`.
    pub(crate) env: Vec<(String, Option<String>)>,
    pub(crate) runs: Runs,
    /// The flags the step's line names.
    pub(crate) label: String,
}

impl Step {
    fn new(root: &Root, name: &str, kind: Kind, argv: Vec<&str>) -> Self {
        Self {
            name: root.step_name(name),
            kind,
            argv: argv.into_iter().map(str::to_owned).collect(),
            // Rustdoc's flags are in the doc tests' fingerprint, and only the doc lane sets them.
            env: vec![("RUSTDOCFLAGS".to_owned(), None)],
            runs: Runs::OnChange,
            label: String::new(),
        }
    }

    /// A name for the `index`th of `count` lanes of one kind.
    fn numbered(name: &str, index: usize, count: usize) -> String {
        if count == 1 {
            name.to_owned()
        } else {
            format!("{name}.{}", index + 1)
        }
    }

    /// The members alone: `--all` also formats path dependencies, which their own roots format.
    pub(crate) fn fmt(root: &Root, format: Format) -> Self {
        let mut argv = vec!["fmt"];
        for member in &root.members {
            argv.extend(["-p", member.name.as_str()]);
        }
        if format == Format::Check {
            argv.push("--check");
        }
        Self::new(root, "fmt", Kind::Format, argv)
    }

    pub(crate) fn lints(root: &Root, sets: &[FlagSet]) -> Vec<Self> {
        let lane = |(i, set): (usize, &FlagSet)| {
            let mut argv = vec!["clippy", "--workspace", "--all-targets"];
            argv.extend(set.args());
            argv.extend(["--locked", "--keep-going", JSON]);
            argv.extend(DENY);
            Self {
                label: set.label(),
                ..Self::new(
                    root,
                    &Self::numbered("lint", i, sets.len()),
                    Kind::Check,
                    argv,
                )
            }
        };
        sets.iter().enumerate().map(lane).collect()
    }

    /// The build that ships, with default features. A member that turns `internals` on through a
    /// normal dependency unifies it into the crates below it, so it is left out; the lint lane
    /// lints its code.
    pub(crate) fn ship(root: &Root) -> Self {
        let mut argv = vec!["clippy", "--workspace", "--locked", "--keep-going", JSON];
        for name in &root.unifies_internals {
            argv.extend(["--exclude", name]);
        }
        argv.extend(DENY);
        Self::new(root, "ship", Kind::Check, argv)
    }

    pub(crate) fn doc(root: &Root) -> Self {
        let set = &root.lanes.doc;
        let mut argv = vec![
            "doc",
            "--workspace",
            "--no-deps",
            "--document-private-items",
        ];
        argv.extend(set.args());
        argv.extend(["--locked", JSON]);
        let env = vec![("RUSTDOCFLAGS".to_owned(), Some("-D warnings".to_owned()))];
        Self {
            env,
            label: set.label(),
            ..Self::new(root, "doc", Kind::Check, argv)
        }
    }

    /// One step per test lane, each with `extra`, the runner's arguments: nextest's, or the test
    /// binaries' under `cargo test`.
    pub(crate) fn tests(root: &Root, extra: &[String], runs: Runs) -> Vec<Self> {
        let sets = &root.lanes.test;
        let lane = |(i, set): (usize, &FlagSet)| {
            let (kind, mut argv) = match root.lanes.test_runner {
                TestRunner::Nextest => (Kind::Nextest, vec!["nextest", "run", "--workspace"]),
                TestRunner::CargoTest => (Kind::CargoTest, vec!["test", "--workspace"]),
            };
            argv.extend(set.args());
            match root.lanes.test_runner {
                TestRunner::Nextest => {
                    argv.extend([
                        "--locked",
                        "--cargo-message-format",
                        "json-diagnostic-short",
                    ]);
                    argv.extend(QUIET_NEXTEST);
                }
                TestRunner::CargoTest => {
                    // The doc tests are a lane of their own.
                    if set.targets.is_empty() {
                        argv.push("--tests");
                    }
                    argv.extend(["--locked", JSON]);
                    if !extra.is_empty() {
                        argv.push("--");
                    }
                }
            }
            argv.extend(extra.iter().map(String::as_str));
            let step = Self::new(root, &Self::numbered("tests", i, sets.len()), kind, argv);
            Self {
                runs,
                label: set.label(),
                ..step
            }
        };
        sets.iter().enumerate().map(lane).collect()
    }

    /// The doc tests, under the first test lane's features: targets select test binaries.
    pub(crate) fn doctests(root: &Root, runs: Runs) -> Option<Self> {
        if !root.members.iter().any(|m| m.doctest) {
            return None;
        }
        let set = root.lanes.test[0].features_only();
        let mut argv = vec!["test", "--workspace", "--doc"];
        argv.extend(set.args());
        argv.extend(["--locked", JSON]);
        Some(Self {
            runs,
            label: set.label(),
            ..Self::new(root, "doctests", Kind::Doctests, argv)
        })
    }

    /// The benches of every member with one: each run selects the same crates, so a case
    /// measures the same code whichever cases run.
    pub(crate) fn bench(root: &Root, extra: &[String]) -> Option<Self> {
        let benches: Vec<&str> = root
            .members
            .iter()
            .filter(|m| m.benches)
            .map(|m| m.name.as_str())
            .collect();
        if benches.is_empty() {
            return None;
        }
        let mut argv = vec!["bench", "--locked"];
        for name in benches {
            argv.extend(["-p", name]);
        }
        argv.push("--benches");
        argv.extend(root.lanes.bench.args());
        if !extra.is_empty() {
            argv.push("--");
        }
        argv.extend(extra.iter().map(String::as_str));
        let step = Self::new(root, "bench", Kind::Program, argv);
        Some(Self {
            runs: Runs::Always,
            label: root.lanes.bench.label(),
            ..step
        })
    }

    /// The steps of `sweep`, the lints first, as they answer in seconds, and the docs last.
    pub(crate) fn every(root: &Root, sweep: Sweep) -> Vec<Self> {
        let mut steps = vec![Self::fmt(root, sweep.format())];
        match sweep {
            Sweep::Check => steps.extend(Self::lints(root, &[FlagSet::all_features()])),
            Sweep::Gate | Sweep::GateCi => {
                steps.extend(Self::lints(root, &root.lanes.lint));
                steps.push(Self::ship(root));
                steps.extend(Self::tests(root, &[], Runs::OnChange));
                if root.lanes.docs {
                    steps.push(Self::doc(root));
                    steps.extend(Self::doctests(root, Runs::OnChange));
                }
            }
        }
        steps
    }

    /// This test step, running only the tests of `members`. Nextest unites its filters, so a
    /// narrowed step can hold no filter of its own.
    pub(crate) fn narrowed<'a>(&self, members: impl IntoIterator<Item = &'a str>) -> Self {
        debug_assert!(self.kind == Kind::Nextest && !self.argv.iter().any(|a| a == "-E"));
        let expression: Vec<String> = members
            .into_iter()
            .map(|m| format!("package(={m})"))
            .collect();
        let mut argv = self.argv.clone();
        argv.extend([
            "-E".to_owned(),
            expression.join(" | "),
            "--no-tests=pass".to_owned(),
        ]);
        Self {
            argv,
            ..self.clone()
        }
    }

    pub(crate) fn log_name(&self) -> String {
        format!("{}.log", self.name.replace('/', "-"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag_set::Targets;

    #[test]
    fn lane_commands() {
        let root = Root::fixture();
        let join = |step: &Step| step.argv.join(" ");
        assert_eq!(
            join(&Step::fmt(&root, Format::Check)),
            "fmt -p core -p app -p harness -p check --check"
        );
        let lints = Step::lints(&root, &root.lanes.lint);
        assert_eq!(lints.len(), 1);
        assert_eq!(lints[0].name, "lint");
        assert_eq!(
            join(&lints[0]),
            "clippy --workspace --all-targets --all-features --locked --keep-going \
             --message-format=json-diagnostic-short -- -D warnings"
        );
        assert_eq!(
            join(&Step::ship(&root)),
            "clippy --workspace --locked --keep-going --message-format=json-diagnostic-short \
             --exclude check --exclude harness -- -D warnings"
        );
        let doc = Step::doc(&root);
        assert_eq!(
            doc.env,
            [("RUSTDOCFLAGS".to_owned(), Some("-D warnings".to_owned()))]
        );
        assert_eq!(lints[0].env, [("RUSTDOCFLAGS".to_owned(), None)]);

        let tests = Step::tests(&root, &["name".to_owned()], Runs::Always);
        assert_eq!(tests[0].runs, Runs::Always);
        assert!(join(&tests[0]).starts_with("nextest run --workspace --all-features --locked"));
        assert!(join(&tests[0]).ends_with("--success-output never name"));
        let narrowed = Step::tests(&root, &[], Runs::OnChange)[0].narrowed(["app", "core"]);
        let mut cargo_test = Root::fixture();
        cargo_test.lanes.test_runner = TestRunner::CargoTest;
        let libtest = Step::tests(&cargo_test, &["name".to_owned()], Runs::OnChange);
        assert_eq!(libtest[0].kind, Kind::CargoTest);
        assert_eq!(
            join(&libtest[0]),
            "test --workspace --all-features --tests --locked --message-format=json-diagnostic-short -- name"
        );
        assert!(join(&narrowed).ends_with("-E package(=app) | package(=core) --no-tests=pass"));

        assert_eq!(
            join(&Step::doctests(&root, Runs::OnChange).unwrap()),
            "test --workspace --doc --all-features --locked --message-format=json-diagnostic-short"
        );
        assert_eq!(
            join(&Step::bench(&root, &["--test".to_owned()]).unwrap()),
            "bench --locked -p app --benches -- --test"
        );
    }

    #[test]
    fn every_runs_the_fixed_lanes_of_the_sweep() {
        let mut root = Root::fixture();
        let none = FlagSet::parse("--no-default-features", "lint", Targets::Refused).unwrap();
        root.lanes.lint = vec![none.clone(), FlagSet::all_features()];
        root.lanes.test.push(none);
        let names = |root: &Root, sweep| -> Vec<String> {
            Step::every(root, sweep)
                .into_iter()
                .map(|s| s.name)
                .collect()
        };
        assert_eq!(names(&root, Sweep::Check), ["fmt", "lint"]);
        let gate = ["fmt", "lint.1", "lint.2", "ship", "tests.1", "tests.2"];
        assert_eq!(names(&root, Sweep::Gate), gate);
        assert_eq!(names(&root, Sweep::GateCi), gate);
        root.lanes.docs = true;
        assert_eq!(names(&root, Sweep::Gate)[6..], ["doc", "doctests"]);
        assert_eq!(names(&root, Sweep::Check), ["fmt", "lint"]);

        // `check` lints every feature whatever sets the root lists, in the command of `gate`'s
        // lint with the same flags, and so shares its pass.
        let check = Step::every(&root, Sweep::Check);
        let gate = Step::every(&root, Sweep::Gate);
        assert_eq!(
            (check[1].argv.as_slice(), check[1].label.as_str()),
            (gate[2].argv.as_slice(), "--all-features")
        );
        assert_ne!(gate[1].argv, gate[2].argv);
        assert_eq!(check[0], gate[0]);
    }

    #[test]
    fn numbered_lanes_and_logs() {
        assert_eq!(Step::numbered("lint", 0, 1), "lint");
        assert_eq!(Step::numbered("lint", 1, 3), "lint.2");
        let mut root = Root::fixture();
        root.label = "palantir".to_owned();
        let fmt = Step::fmt(&root, Format::Write);
        assert_eq!(fmt.name, "palantir/fmt");
        assert_eq!(fmt.log_name(), "palantir-fmt.log");
    }
}
