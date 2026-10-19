use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::flag_set::FlagSet;
use crate::lanes::TestRunner;
use crate::member::Member;
use crate::platform::{self, SplitBy};
use crate::project_command::{CommandStep, Joins, ProjectCommand, StepWhat};
use crate::root::Root;
use crate::sweep::Sweep;

/// Cargo's messages as JSON, each diagnostic short: the driver tells a lint from code that does
/// not compile, and prints each diagnostic once. The format is in no fingerprint.
const JSON: &str = "--message-format=json-diagnostic-short";
/// Clippy's arguments after `--` are in each unit's fingerprint, so every lint passes these.
const DENY: [&str; 3] = ["--", "-D", "warnings"];
/// The bench targets alone: `--benches` also selects each library and binary, whose `bench` flag
/// is on by default, and hands them the bench binaries' arguments.
const EVERY_BENCH: [&str; 2] = ["--bench", "*"];
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
    /// A program whose output is its own: a bench, a project's command, a packaging move.
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
    /// When an input changed since it passed, whole: a step whose own filter or program no change
    /// narrows.
    OnInput,
    /// Each time, every test it names; a pass it makes is still kept for later commands.
    Always,
}

/// What a step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Program {
    /// Cargo, with the step's arguments.
    Cargo,
    /// Another program, by its path or by its name on `PATH`, with the step's arguments.
    Other(PathBuf),
    /// No process: a copy of the file or the folder `from`, in place of what `to` holds.
    Copy { from: PathBuf, to: PathBuf },
}

/// Whether `cargo fmt` writes files or lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Write,
    Check,
}

/// One command on one root, cargo's or another program's. Every lane's command is built here
/// alone: each shape of a cargo command keeps a cache of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    pub(crate) program: Program,
    /// The program's arguments.
    pub(crate) argv: Vec<OsString>,
    /// Variables set, or removed for `None`.
    pub(crate) env: Vec<(String, Option<String>)>,
    pub(crate) runs: Runs,
    /// What the step's line names: its flags, and the crates a narrowed run tests.
    pub(crate) label: String,
    /// The pass it shares with the other steps of its project command, which runs whole or not
    /// at all: a later step reads what an earlier one built, which a step between them, as the
    /// tests', may build over.
    pub(crate) shared: Option<SharedPass>,
}

/// A pass the steps of one project command share: its identity, and whether this step is the
/// last, which alone keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedPass {
    pub(crate) command: String,
    pub(crate) last: bool,
}

impl Step {
    fn new(root: &Root, name: &str, kind: Kind, argv: Vec<&str>) -> Self {
        Self {
            name: root.step_name(name),
            kind,
            program: Program::Cargo,
            argv: argv.into_iter().map(OsString::from).collect(),
            // Rustdoc's flags are in the doc tests' fingerprint, and only the doc lane sets them.
            env: vec![("RUSTDOCFLAGS".to_owned(), None)],
            runs: Runs::OnChange,
            label: String::new(),
            shared: None,
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
            Self::lint(root, &Self::numbered("lint", i, sets.len()), set, None)
        };
        sets.iter().enumerate().map(lane).collect()
    }

    /// The lint of every target under `set`, for `target`, or for the host's with none.
    fn lint(root: &Root, name: &str, set: &FlagSet, target: Option<&str>) -> Self {
        let mut argv = vec!["clippy", "--workspace", "--all-targets"];
        argv.extend(set.args());
        argv.extend(["--locked", "--keep-going", JSON]);
        if let Some(target) = target {
            argv.extend(["--target", target]);
        }
        argv.extend(DENY);
        Self {
            label: set.label(),
            ..Self::new(root, name, Kind::Check, argv)
        }
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

    /// One step per test lane, each with `extra`.
    pub(crate) fn tests(root: &Root, extra: &[String], runs: Runs) -> Vec<Self> {
        let sets = &root.lanes.test;
        let lane = |(i, set): (usize, &FlagSet)| Self {
            runs,
            ..Self::test_lane(root, &Self::numbered("tests", i, sets.len()), set, extra)
        };
        sets.iter().enumerate().map(lane).collect()
    }

    /// The tests of `set`, with `extra`, the runner's arguments: nextest's, or the test binaries'
    /// under `cargo test`.
    fn test_lane(root: &Root, name: &str, set: &FlagSet, extra: &[String]) -> Self {
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
        Self {
            label: set.label(),
            ..Self::new(root, name, kind, argv)
        }
    }

    /// The gate's build of every bench, which runs none: the dev profile and the first test
    /// lane's features build no unit the tests did not but the benches' own, where the bench
    /// profile links each bench again with the release's LTO.
    pub(crate) fn gate_benches(root: &Root) -> Option<Self> {
        if !root.members.iter().any(|m| m.benches) {
            return None;
        }
        let set = root.lanes.test[0].features_only();
        let mut argv = vec!["build", "--workspace"];
        argv.extend(EVERY_BENCH);
        argv.extend(set.args());
        argv.extend(["--locked", JSON]);
        Some(Self {
            label: set.label(),
            ..Self::new(root, "benches", Kind::Check, argv)
        })
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
        argv.extend(EVERY_BENCH);
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

    /// The lint of every target with every feature for each of `lanes.cross_lint`'s targets, as
    /// `check`'s lint is for the host's.
    pub(crate) fn cross_lints(root: &Root) -> Vec<Self> {
        let targets = &root.lanes.cross_lint;
        let lane = |(i, target): (usize, &String)| {
            let name = Self::numbered("cross", i, targets.len());
            Self {
                label: target.clone(),
                ..Self::lint(root, &name, &FlagSet::all_features(), Some(target))
            }
        };
        targets.iter().enumerate().map(lane).collect()
    }

    /// The steps of the root's command `command`, with `args`, the command's arguments, given to
    /// the step that takes them: each named after the command, numbered when it has several.
    pub(crate) fn command(
        root: &Root,
        command: &ProjectCommand,
        args: &[String],
        runs: Runs,
    ) -> Vec<Self> {
        let count = command.steps.len();
        let lane = |(i, step): (usize, &CommandStep)| {
            let name = Self::numbered(&command.name, i, count);
            let given = if step.takes_args { args } else { &[] };
            let mut built = match &step.what {
                StepWhat::Build(cargo) => {
                    let mut argv = strs(cargo);
                    argv.extend(["--locked", JSON]);
                    argv.extend(given.iter().map(String::as_str));
                    Self::new(root, &name, Kind::Check, argv)
                }
                StepWhat::Cargo(cargo) => {
                    let mut argv = strs(cargo);
                    argv.push("--locked");
                    if !given.is_empty() {
                        argv.push("--");
                    }
                    argv.extend(given.iter().map(String::as_str));
                    Self::new(root, &name, Kind::Program, argv)
                }
                StepWhat::Program(program) => {
                    let (path, rest) = program.split_first().expect("a program step names one");
                    let path = PathBuf::from(placed(path, &root.target_dir));
                    // A path with a folder in it is the project's; a bare name, one on `PATH`.
                    let path = if path.components().count() > 1 {
                        root.dir.join(path)
                    } else {
                        path
                    };
                    let mut argv: Vec<OsString> = (rest.iter())
                        .map(|word| placed(word, &root.target_dir))
                        .collect();
                    argv.extend(given.iter().map(OsString::from));
                    Self {
                        program: Program::Other(path),
                        argv,
                        ..Self::new(root, &name, Kind::Program, Vec::new())
                    }
                }
                StepWhat::Tests(filter) => {
                    let mut lane = Self::test_lane(root, &name, &root.lanes.test[0], given);
                    lane.argv
                        .extend([OsString::from("-E"), OsString::from(filter)]);
                    Self {
                        label: String::new(),
                        ..lane
                    }
                }
            };
            built.env.extend(
                step.env
                    .iter()
                    .map(|(name, value)| (name.clone(), Some(value.clone()))),
            );
            built.runs = runs;
            built
        };
        let mut steps: Vec<Self> = command.steps.iter().enumerate().map(lane).collect();
        if count > 1 {
            let identity = (steps.iter()).map(|step| format!("{:?}", (&step.argv, &step.env)));
            let identity = format!(
                "{}: {}",
                command.name,
                identity.collect::<Vec<_>>().join(" ")
            );
            for (at, step) in steps.iter_mut().enumerate() {
                step.shared = Some(SharedPass {
                    command: identity.clone(),
                    last: at + 1 == count,
                });
            }
        }
        steps
    }

    /// The steps of every command of the root that joins `joins`, each run whole once an input
    /// changed: no change narrows a project's own filter or program.
    fn joined(root: &Root, joins: Joins) -> Vec<Self> {
        let commands = root.lanes.commands.iter();
        let joining = commands.filter(|command| command.joins == Some(joins));
        joining
            .flat_map(|command| Self::command(root, command, &[], Runs::OnInput))
            .collect()
    }

    /// The steps of `test`: the commands that join it, then the test lanes, with `extra`.
    pub(crate) fn test_command(root: &Root, extra: &[String]) -> Vec<Self> {
        let mut steps = Self::joined(root, Joins::Test);
        steps.extend(Self::tests(root, extra, Runs::Always));
        steps
    }

    /// The release build of the binaries the root ships, then each binary stripped in `out`,
    /// with its debug info beside it, as the OS keeps it.
    pub(crate) fn package(root: &Root, out: &Path) -> Vec<Self> {
        let mut argv = vec!["build", "--release", "--locked", JSON];
        for name in &root.lanes.package {
            argv.extend(["--bin", name.as_str()]);
        }
        let mut build = Self::new(root, "release", Kind::Check, argv);
        build.runs = Runs::Always;
        build.env.extend(
            platform::package_env()
                .iter()
                .map(|&(name, value)| (name.to_owned(), Some(value.to_owned()))),
        );
        let release = root.target_dir.join("release");
        let mut steps = vec![build];
        for name in &root.lanes.package {
            let built = release.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            for split in platform::split_debug(name, &built, out) {
                let step_name = format!("{} {name}", split.makes);
                let (program, argv) = match split.by {
                    SplitBy::Tool { program, args } => (Program::Other(program.into()), args),
                    SplitBy::Copy { from, to } => (Program::Copy { from, to }, Vec::new()),
                };
                steps.push(Self {
                    program,
                    argv,
                    runs: Runs::Always,
                    ..Self::new(root, &step_name, Kind::Program, Vec::new())
                });
            }
        }
        steps
    }

    /// The steps of `sweep`, the lints first, as they answer in seconds, and the docs last. A
    /// local gate lints the targets of other OSes after the host's, as CI lints each on its own
    /// OS; the gate's own commands and its build of every bench follow the tests.
    pub(crate) fn every(root: &Root, sweep: Sweep) -> Vec<Self> {
        let mut steps = vec![Self::fmt(root, sweep.format())];
        match sweep {
            Sweep::Check => steps.extend(Self::lints(root, &[FlagSet::all_features()])),
            Sweep::Gate | Sweep::GateCi => {
                steps.extend(Self::lints(root, &root.lanes.lint));
                steps.push(Self::ship(root));
                if sweep == Sweep::Gate {
                    steps.extend(Self::cross_lints(root));
                }
                steps.extend(Self::tests(root, &[], Runs::OnChange));
                steps.extend(Self::joined(root, Joins::Gate));
                if root.lanes.gate_benches {
                    steps.extend(Self::gate_benches(root));
                }
                if root.lanes.docs {
                    steps.push(Self::doc(root));
                    steps.extend(Self::doctests(root, Runs::OnChange));
                }
            }
        }
        steps
    }

    /// The program it runs, as an error names it.
    pub(crate) fn program_name(&self) -> String {
        match &self.program {
            Program::Cargo => {
                let first = self.argv.first().map(|arg| arg.to_string_lossy());
                format!("cargo {}", first.unwrap_or_default())
            }
            Program::Other(path) => path.display().to_string(),
            Program::Copy { from, .. } => format!("a copy of {}", from.display()),
        }
    }

    /// What a change that reaches the members `reached` leaves of this step to run: nothing when
    /// it reaches none of the members whose tests the step runs, a nextest run narrowed to the
    /// ones it reaches, or the step whole. `cargo test` selects crates only through `-p`, which
    /// builds another shape.
    pub(crate) fn for_reach(&self, root: &Root, reached: &BTreeSet<String>) -> Option<Self> {
        let tested: fn(&Member) -> bool = match self.kind {
            Kind::Nextest | Kind::CargoTest => |_| true,
            Kind::Doctests => |member| member.doctest,
            Kind::Format | Kind::Check | Kind::Program => return Some(self.clone()),
        };
        let tests: Vec<&str> = (root.members.iter())
            .filter(|m| tested(m))
            .map(|m| m.name.as_str())
            .collect();
        let hit: Vec<&str> = (tests.iter().copied())
            .filter(|name| reached.contains(*name))
            .collect();
        if hit.is_empty() {
            None
        } else if self.kind == Kind::Nextest && hit.len() < tests.len() {
            Some(self.narrowed(&hit, tests.len()))
        } else {
            Some(self.clone())
        }
    }

    /// This nextest step, running only the tests of `members`, of the `of` it runs whole.
    /// Nextest unites its filters, so a narrowed step can hold no filter of its own.
    fn narrowed(&self, members: &[&str], of: usize) -> Self {
        debug_assert!(self.kind == Kind::Nextest && !self.argv.iter().any(|a| a == "-E"));
        let expression: Vec<String> = members.iter().map(|m| format!("package(={m})")).collect();
        let mut argv = self.argv.clone();
        argv.extend([
            OsString::from("-E"),
            OsString::from(expression.join(" | ")),
            OsString::from("--no-tests=pass"),
        ]);
        let tested = format!("the tests of {} of {of} crates", members.len());
        let label = if self.label.is_empty() {
            tested
        } else {
            format!("{tested}; {}", self.label)
        };
        Self {
            argv,
            label,
            ..self.clone()
        }
    }

    pub(crate) fn log_name(&self) -> String {
        format!("{}.log", self.name.replace('/', "-"))
    }
}

/// Each of `words`, borrowed.
fn strs(words: &[String]) -> Vec<&str> {
    words.iter().map(String::as_str).collect()
}

/// `word` of a program step with its placeholders filled: `{target}` with `target`, the root's
/// target folder, and `{exe}` with the OS's suffix of an executable. The folder is put in as the
/// OS holds it, so a name that is not UTF-8 comes through whole.
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "the placeholders of a command's program step, which no format reads"
)]
fn placed(word: &str, target: &Path) -> OsString {
    let word = word.replace("{exe}", std::env::consts::EXE_SUFFIX);
    let mut out = OsString::new();
    let mut parts = word.split("{target}");
    out.push(parts.next().unwrap_or_default());
    for part in parts {
        out.push(target);
        out.push(part);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag_set::Targets;

    /// The step's arguments, as text.
    fn words(step: &Step) -> Vec<String> {
        let words = step
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned());
        words.collect()
    }

    #[test]
    fn lane_commands() {
        let root = Root::fixture();
        let join = |step: &Step| words(step).join(" ");
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
        let mut cargo_test = Root::fixture();
        cargo_test.lanes.test_runner = TestRunner::CargoTest;
        let libtest = Step::tests(&cargo_test, &["name".to_owned()], Runs::OnChange);
        assert_eq!(libtest[0].kind, Kind::CargoTest);
        assert_eq!(
            join(&libtest[0]),
            "test --workspace --all-features --tests --locked --message-format=json-diagnostic-short -- name"
        );

        assert_eq!(
            join(&Step::doctests(&root, Runs::OnChange).unwrap()),
            "test --workspace --doc --all-features --locked --message-format=json-diagnostic-short"
        );
        assert_eq!(
            join(&Step::bench(&root, &["--test".to_owned()]).unwrap()),
            "bench --locked -p app --bench * -- --test"
        );
    }

    #[test]
    fn a_change_narrows_only_the_test_steps_it_reaches() {
        // The fixture's members in order: `core` and `harness` with doc tests, `app`, `check`.
        let root = Root::fixture();
        let reached =
            |names: &[&str]| -> BTreeSet<String> { names.iter().map(|&n| n.to_owned()).collect() };
        let tests = Step::tests(&root, &[], Runs::OnChange).remove(0);
        let narrowed = tests.for_reach(&root, &reached(&["app", "core"])).unwrap();
        assert!(
            words(&narrowed)
                .join(" ")
                .ends_with("-E package(=core) | package(=app) --no-tests=pass")
        );
        assert_eq!(narrowed.label, "the tests of 2 of 4 crates; --all-features");
        let all = reached(&["core", "app", "harness", "check"]);
        assert_eq!(tests.for_reach(&root, &all), Some(tests.clone()));
        assert_eq!(tests.for_reach(&root, &reached(&["gui"])), None);

        // The doc tests count only the members that have them, and run whole.
        let doctests = Step::doctests(&root, Runs::OnChange).unwrap();
        assert_eq!(doctests.for_reach(&root, &reached(&["app"])), None);
        assert_eq!(
            doctests.for_reach(&root, &reached(&["harness"])),
            Some(doctests)
        );
        let mut cargo_test = Root::fixture();
        cargo_test.lanes.test_runner = TestRunner::CargoTest;
        let libtest = Step::tests(&cargo_test, &[], Runs::OnChange).remove(0);
        assert_eq!(
            libtest.for_reach(&cargo_test, &reached(&["check"])),
            Some(libtest.clone())
        );
        assert_eq!(libtest.for_reach(&cargo_test, &reached(&[])), None);
        // A step that runs no member's tests runs whole whatever the change reaches.
        let lint = Step::lints(&root, &[FlagSet::all_features()]).remove(0);
        assert_eq!(lint.for_reach(&root, &reached(&[])), Some(lint));
    }

    /// A command of `steps`, a table's, joining `joins`.
    fn command(name: &str, joins: Option<&str>, steps: &serde_json::Value) -> ProjectCommand {
        let table = serde_json::json!({ "joins": joins, "steps": steps });
        let table = serde_json::from_value(table).unwrap();
        ProjectCommand::resolve(name.to_owned(), table, TestRunner::Nextest).unwrap()
    }

    #[test]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "the placeholders of a command's program step, which no format reads"
    )]
    fn a_project_command_s_steps() {
        let root = Root::fixture();
        let lan = command(
            "lan",
            Some("gate"),
            &serde_json::json!([
                { "build": "build --bins -p app" },
                { "program": ["{target}/debug/app{exe}", "{target}/drive/lan"], "args": true },
                { "cargo": "run -p app", "env": { "BLESS": "1" } },
                { "tests": "test(=index)" },
                { "program": ["objcopy", "x"] },
            ]),
        );
        let args = ["out".to_owned()];
        let steps = Step::command(&root, &lan, &args, Runs::OnInput);
        let names: Vec<&str> = steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["lan.1", "lan.2", "lan.3", "lan.4", "lan.5"]);
        // The steps of one command share one pass, which the last keeps.
        assert!(steps.iter().all(|s| s.runs == Runs::OnInput));
        let shared: Vec<bool> = (steps.iter())
            .map(|s| {
                s.shared
                    .as_ref()
                    .expect("a step of a command of several")
                    .last
            })
            .collect();
        assert_eq!(shared, [false, false, false, false, true]);
        assert!(steps.iter().all(|s| s.shared.as_ref().map(|p| &p.command)
            == steps[0].shared.as_ref().map(|p| &p.command)));
        let one = command("one", None, &serde_json::json!([{ "build": "build" }]));
        assert_eq!(
            Step::command(&root, &one, &[], Runs::OnInput)[0].shared,
            None
        );
        assert_eq!(
            (words(&steps[0]).join(" "), steps[0].kind),
            (
                "build --bins -p app --locked --message-format=json-diagnostic-short".to_owned(),
                Kind::Check
            )
        );
        let exe = std::env::consts::EXE_SUFFIX;
        assert_eq!(
            steps[1].program,
            Program::Other(PathBuf::from(format!("/w/target/debug/app{exe}")))
        );
        assert_eq!(words(&steps[1]), ["/w/target/drive/lan", "out"]);
        // The arguments go to the one step that takes them.
        assert_eq!(
            (words(&steps[2]).join(" "), steps[2].kind),
            ("run -p app --locked".to_owned(), Kind::Program)
        );
        assert!(
            steps[2]
                .env
                .contains(&("BLESS".to_owned(), Some("1".to_owned())))
        );
        let tests = words(&steps[3]).join(" ");
        assert!(tests.starts_with("nextest run --workspace --all-features --locked"));
        assert!(tests.ends_with("--success-output never -E test(=index)"));
        assert_eq!(steps[3].kind, Kind::Nextest);
        // A bare name is a program on `PATH`.
        assert_eq!(steps[4].program, Program::Other(PathBuf::from("objcopy")));
        assert_eq!(
            Step::command(&root, &lan, &[], Runs::Always)[0].runs,
            Runs::Always
        );
        let target = Path::new("/t");
        assert_eq!(
            placed("{target}/a{target}", target),
            OsString::from("/t/a/t")
        );
    }

    #[test]
    fn the_commands_that_join_test_and_gate_and_the_root_s_other_lanes() {
        let mut root = Root::fixture();
        root.lanes.commands = vec![
            command(
                "index",
                Some("test"),
                &serde_json::json!([{ "tests": "test(=index)" }]),
            ),
            command(
                "lan",
                Some("gate"),
                &serde_json::json!([{ "build": "build" }]),
            ),
            command("zero-hour", None, &serde_json::json!([{ "cargo": "run" }])),
        ];
        root.lanes.gate_benches = true;
        root.lanes.cross_lint = vec!["x86_64-pc-windows-gnu".to_owned()];
        let names =
            |steps: Vec<Step>| -> Vec<String> { steps.into_iter().map(|s| s.name).collect() };
        // `test` writes the index before the tests, whole once an input changed.
        let test = Step::test_command(&root, &[]);
        assert_eq!(names(test.clone()), ["index", "tests"]);
        assert_eq!([test[0].runs, test[1].runs], [Runs::OnInput, Runs::Always]);
        // A local gate lints for the other OS after the host's lanes; CI lints each OS on its own.
        let gate = ["fmt", "lint", "ship", "cross", "tests", "lan", "benches"];
        assert_eq!(names(Step::every(&root, Sweep::Gate)), gate);
        assert_eq!(
            names(Step::every(&root, Sweep::GateCi)),
            ["fmt", "lint", "ship", "tests", "lan", "benches"]
        );
        assert_eq!(names(Step::every(&root, Sweep::Check)), ["fmt", "lint"]);
        let every = Step::every(&root, Sweep::Gate);
        assert_eq!(
            (words(&every[3]).join(" "), every[3].label.as_str()),
            (
                "clippy --workspace --all-targets --all-features --locked --keep-going \
                 --message-format=json-diagnostic-short --target x86_64-pc-windows-gnu -- -D \
                 warnings"
                    .to_owned(),
                "x86_64-pc-windows-gnu"
            )
        );
        // The gate builds the bench targets alone, under the first test lane's features and not
        // the bench lane's, with none of its targets, and runs no case.
        assert_eq!(
            (
                words(&every[6]).join(" "),
                every[6].kind,
                every[6].label.as_str()
            ),
            (
                "build --workspace --bench * --all-features --locked \
                 --message-format=json-diagnostic-short"
                    .to_owned(),
                Kind::Check,
                "--all-features"
            )
        );
        root.lanes.bench = FlagSet::all_features();
        root.lanes.test[0] = FlagSet {
            features: vec!["--features".to_owned(), "golden".to_owned()],
            targets: vec!["--lib".to_owned()],
            but: Vec::new(),
        };
        assert_eq!(
            words(&Step::gate_benches(&root).unwrap()).join(" "),
            "build --workspace --bench * --features golden --locked \
             --message-format=json-diagnostic-short"
        );
        root.members.iter_mut().for_each(|m| m.benches = false);
        assert_eq!(Step::gate_benches(&root), None);
        assert_eq!(
            names(Step::every(&root, Sweep::GateCi)).last().unwrap(),
            "lan"
        );
    }

    #[test]
    fn package_builds_the_release_then_splits_each_binary() {
        let mut root = Root::fixture();
        root.lanes.package = vec!["app".to_owned()];
        let steps = Step::package(&root, Path::new("/w/target/package"));
        assert_eq!(
            words(&steps[0]).join(" "),
            "build --release --locked --message-format=json-diagnostic-short --bin app"
        );
        let env = platform::package_env();
        assert!(env.iter().all(|&(name, value)| {
            steps[0]
                .env
                .contains(&(name.to_owned(), Some(value.to_owned())))
        }));
        // Each binary's moves, as this OS splits them, each named by what it makes.
        let exe = std::env::consts::EXE_SUFFIX;
        let built = PathBuf::from(format!("/w/target/release/app{exe}"));
        let splits = platform::split_debug("app", &built, Path::new("/w/target/package"));
        assert_eq!(steps.len(), 1 + splits.len());
        for (step, split) in steps[1..].iter().zip(&splits) {
            assert_eq!(step.name, format!("{} app", split.makes));
            assert!(steps.iter().all(|s| s.runs == Runs::Always));
        }
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
