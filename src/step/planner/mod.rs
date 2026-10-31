//! Every lane's command, built here alone: each shape of a cargo command keeps a cache of its own.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::config::TestRunner;
use crate::flag_set::FlagSet;
use crate::member::Member;
use crate::platform::{self, SplitBy};
use crate::project_command::{CommandAction, CommandStep, Joins, ProjectCommand};
use crate::root::{Lockfile, Root};
use crate::step::{Action, Kind, Runs, SharedPass, Step};
use crate::surface::build::Build;
use crate::sweep::Sweep;

/// Cargo's messages as JSON, each diagnostic short: the driver tells a lint from code that does
/// not compile, and prints each diagnostic once. The format is in no fingerprint.
const JSON: &str = "--message-format=json-diagnostic-short";
/// Clippy's arguments after `--` are in each unit's fingerprint, so every lint passes these.
const DENY: [&str; 3] = ["--", "-D", "warnings"];
/// The bench targets alone: `--benches` also selects each library and binary, whose `bench` flag
/// is on by default, and hands them the bench binaries' arguments.
const EVERY_BENCH: [&str; 2] = ["--bench", "*"];
/// Features that are not public surface: the default set, and the crate's own test surface.
const NOT_SURFACE: [&str; 2] = ["default", "internals"];
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

/// Whether a run may change the files it checks: `cargo fmt` writes them or lists them, and cargo
/// updates the lockfile or fails where it would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Files {
    Write,
    Keep,
}

/// What a test lane's step runs.
#[derive(Debug, Clone, Copy)]
enum Lane<'a> {
    /// The tests, with the runner's arguments: nextest's, or the test binaries' under
    /// `cargo test`.
    Tests(&'a [String]),
    /// The build of the tests alone.
    Build,
}

/// The steps of one root's lanes, each as `files` allows.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Planner<'r> {
    root: &'r Root,
    files: Files,
}

impl<'r> Planner<'r> {
    pub(crate) const fn new(root: &'r Root, files: Files) -> Self {
        Self { root, files }
    }

    /// Cargo's flags for the lockfile: `--locked` where the run keeps the files and the commit
    /// holds one to keep. It is in no fingerprint, so a run that keeps the files reuses the build
    /// of one that writes them.
    const fn lock(self) -> &'static [&'static str] {
        match (self.files, self.root.lockfile) {
            (Files::Keep, Lockfile::Committed) => &["--locked"],
            (Files::Write, _) | (_, Lockfile::Free) => &[],
        }
    }

    /// The members alone: `--all` also formats path dependencies, which their own roots format.
    pub(crate) fn fmt(self) -> Step {
        let mut argv = vec!["fmt"];
        for member in &self.root.members {
            argv.extend(["-p", member.name.as_str()]);
        }
        if self.files == Files::Keep {
            argv.push("--check");
        }
        Step::new(self.root, "fmt", Kind::Format, argv)
    }

    pub(crate) fn lints(self, sets: &[FlagSet]) -> Vec<Step> {
        let lane =
            |(i, set): (usize, &FlagSet)| self.lint(&numbered("lint", i, sets.len()), set, None);
        sets.iter().enumerate().map(lane).collect()
    }

    /// The lint of every target under `set`, for `target`, or for the host's with none.
    fn lint(self, name: &str, set: &FlagSet, target: Option<&str>) -> Step {
        let mut argv = vec!["clippy", "--workspace", "--all-targets"];
        argv.extend(set.args());
        argv.extend(self.lock());
        argv.extend(["--keep-going", JSON]);
        if let Some(target) = target {
            argv.extend(["--target", target]);
        }
        argv.extend(DENY);
        Step {
            label: set.label(),
            ..Step::new(self.root, name, Kind::Check, argv)
        }
    }

    /// The build that ships, with default features. A member that turns `internals` on through a
    /// normal dependency unifies it into the crates below it, so it is left out; the lint lane
    /// lints its code.
    pub(crate) fn ship(self) -> Step {
        let mut argv = vec!["clippy", "--workspace"];
        argv.extend(self.lock());
        argv.extend(["--keep-going", JSON]);
        for name in &self.root.unifies_internals {
            argv.extend(["--exclude", name]);
        }
        argv.extend(DENY);
        Step::new(self.root, "ship", Kind::Check, argv)
    }

    pub(crate) fn doc(self) -> Step {
        let set = &self.root.config.doc;
        let mut argv = vec![
            "doc",
            "--workspace",
            "--no-deps",
            "--document-private-items",
        ];
        argv.extend(set.args());
        argv.extend(self.lock());
        argv.push(JSON);
        let env = vec![("RUSTDOCFLAGS".to_owned(), Some("-D warnings".to_owned()))];
        Step {
            env,
            label: set.label(),
            ..Step::new(self.root, "doc", Kind::Check, argv)
        }
    }

    /// One step per test lane, each with `extra`.
    pub(crate) fn tests(self, extra: &[String], runs: Runs) -> Vec<Step> {
        let sets = &self.root.config.test;
        let lane = |(i, set): (usize, &FlagSet)| Step {
            runs,
            ..self.test_lane(&numbered("tests", i, sets.len()), set, Lane::Tests(extra))
        };
        sets.iter().enumerate().map(lane).collect()
    }

    /// The tests of `set` as `lane` runs them: nextest's command, or `cargo test`'s. The build
    /// alone is the same cargo command, less what selects and prints the tests.
    fn test_lane(self, name: &str, set: &FlagSet, lane: Lane<'_>) -> Step {
        let runner = self.root.config.test_runner;
        let mut argv = match runner {
            TestRunner::Nextest => vec!["nextest", "run", "--workspace"],
            TestRunner::CargoTest => vec!["test", "--workspace"],
        };
        argv.extend(set.args());
        match runner {
            TestRunner::Nextest => {
                argv.extend(self.lock());
                argv.extend(["--cargo-message-format", "json-diagnostic-short"]);
            }
            TestRunner::CargoTest => {
                // The doc tests are a lane of their own.
                if set.targets.is_empty() {
                    argv.push("--tests");
                }
                argv.extend(self.lock());
                argv.push(JSON);
            }
        }
        let kind = match (lane, runner) {
            (Lane::Build, _) => {
                argv.push("--no-run");
                Kind::Check
            }
            (Lane::Tests(extra), TestRunner::Nextest) => {
                argv.extend(QUIET_NEXTEST);
                argv.extend(extra.iter().map(String::as_str));
                Kind::Nextest
            }
            (Lane::Tests(extra), TestRunner::CargoTest) => {
                if !extra.is_empty() {
                    argv.push("--");
                }
                argv.extend(extra.iter().map(String::as_str));
                Kind::CargoTest
            }
        };
        Step {
            label: set.label(),
            ..Step::new(self.root, name, kind, argv)
        }
    }

    /// The gate's build of every bench, which runs none: the dev profile and the first test
    /// lane's features build no unit the tests did not but the benches' own, where the bench
    /// profile links each bench again with the release's LTO.
    pub(crate) fn gate_benches(self) -> Option<Step> {
        if !self.root.members.iter().any(|m| m.benches) {
            return None;
        }
        let set = self.root.config.test[0].features_only();
        let mut argv = vec!["build", "--workspace"];
        argv.extend(EVERY_BENCH);
        argv.extend(set.args());
        argv.extend(self.lock());
        argv.push(JSON);
        Some(Step {
            label: set.label(),
            ..Step::new(self.root, "benches", Kind::Check, argv)
        })
    }

    /// The doc tests, under the first test lane's features: targets select test binaries.
    pub(crate) fn doctests(self, runs: Runs) -> Option<Step> {
        if !self.root.members.iter().any(|m| m.doctest) {
            return None;
        }
        let set = self.root.config.test[0].features_only();
        let mut argv = vec!["test", "--workspace", "--doc"];
        argv.extend(set.args());
        argv.extend(self.lock());
        argv.push(JSON);
        Some(Step {
            runs,
            label: set.label(),
            ..Step::new(self.root, "doctests", Kind::Doctests, argv)
        })
    }

    /// The benches of every member with one: each run selects the same crates, so a case
    /// measures the same code whichever cases run. A run with criterion's `--test`, which times
    /// nothing and runs each case once, builds them under the test profile, as `cargo test`
    /// builds a bench, rather than the bench profile's release LTO, a long link it would not use.
    pub(crate) fn bench(self, extra: &[String]) -> Option<Step> {
        let benches: Vec<&str> = (self.root.members.iter())
            .filter(|m| m.benches)
            .map(|m| m.name.as_str())
            .collect();
        if benches.is_empty() {
            return None;
        }
        let once = extra.iter().any(|arg| arg == "--test");
        let mut argv = vec![if once { "test" } else { "bench" }];
        argv.extend(self.lock());
        for name in benches {
            argv.extend(["-p", name]);
        }
        argv.extend(EVERY_BENCH);
        argv.extend(self.root.config.bench.args());
        if !extra.is_empty() {
            argv.push("--");
        }
        argv.extend(extra.iter().map(String::as_str));
        let step = Step::new(self.root, "bench", Kind::Plain, argv);
        Some(Step {
            runs: Runs::Always,
            label: self.root.config.bench.label(),
            ..step
        })
    }

    /// The lint of every target with every feature for each of `lanes.cross_lint`'s targets, as
    /// `check`'s lint is for the host's.
    pub(crate) fn cross_lints(self) -> Vec<Step> {
        let targets = &self.root.config.cross_lint;
        let lane = |(i, target): (usize, &String)| {
            let name = numbered("cross", i, targets.len());
            Step {
                label: target.clone(),
                ..self.lint(&name, &FlagSet::all_features(), Some(target))
            }
        };
        targets.iter().enumerate().map(lane).collect()
    }

    /// The steps of the root's command `command`, with `args`, the command's arguments, given to
    /// the step that takes them: each named after the command, numbered when it has several.
    pub(crate) fn command(
        self,
        command: &ProjectCommand,
        args: &[String],
        runs: Runs,
    ) -> Vec<Step> {
        let root = self.root;
        let count = command.steps.len();
        let lane = |(i, step): (usize, &CommandStep)| {
            let name = numbered(&command.name, i, count);
            let given = if step.takes_args { args } else { &[] };
            let mut built = match &step.action {
                CommandAction::Build(cargo) => {
                    let mut argv = strs(cargo);
                    argv.extend(self.lock());
                    argv.push(JSON);
                    argv.extend(given.iter().map(String::as_str));
                    Step::new(root, &name, Kind::Check, argv)
                }
                CommandAction::Cargo(cargo) => {
                    let mut argv = strs(cargo);
                    argv.extend(self.lock());
                    if !given.is_empty() {
                        argv.push("--");
                    }
                    argv.extend(given.iter().map(String::as_str));
                    Step::new(root, &name, Kind::Plain, argv)
                }
                CommandAction::Program(program) => {
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
                    Step {
                        action: Action::Program(path),
                        argv,
                        ..Step::new(root, &name, Kind::Plain, Vec::new())
                    }
                }
                CommandAction::Tests(filter) => {
                    let mut lane = self.test_lane(&name, &root.config.test[0], Lane::Tests(given));
                    lane.argv
                        .extend([OsString::from("-E"), OsString::from(filter)]);
                    Step {
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
            if step.action.plain() {
                built.errors.clone_from(&command.errors);
            }
            built.runs = runs;
            built
        };
        let mut steps: Vec<Step> = command.steps.iter().enumerate().map(lane).collect();
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
    fn joined(self, joins: Joins) -> Vec<Step> {
        let commands = self.root.config.commands.iter();
        let joining = commands.filter(|command| command.joins == Some(joins));
        joining
            .flat_map(|command| self.command(command, &[], Runs::OnInput))
            .collect()
    }

    /// The steps of `test`: the build of each test lane, the commands that join it, then the
    /// test lanes, with `extra`.
    pub(crate) fn test_command(self, extra: &[String]) -> Vec<Step> {
        let mut steps = self.test_builds();
        steps.extend(self.joined(Joins::Test));
        steps.extend(self.tests(extra, Runs::Always));
        steps
    }

    /// Each test lane's build, which runs no test: a test that does not compile fails here, not
    /// in the first step that runs a test, as a command that joins `test` and runs one. It builds
    /// in its lane's command, so the lane's step finds every unit fresh.
    fn test_builds(self) -> Vec<Step> {
        let sets = &self.root.config.test;
        let lane = |(i, set): (usize, &FlagSet)| Step {
            runs: Runs::OnInput,
            ..self.test_lane(&numbered("build", i, sets.len()), set, Lane::Build)
        };
        sets.iter().enumerate().map(lane).collect()
    }

    /// The release build of the binaries the root ships, then each binary stripped in `out`,
    /// with its debug info beside it, as the OS keeps it.
    pub(crate) fn package(self, out: &Path) -> Vec<Step> {
        let root = self.root;
        let mut argv = vec!["build", "--release"];
        argv.extend(self.lock());
        argv.push(JSON);
        for name in &root.config.package {
            argv.extend(["--bin", name.as_str()]);
        }
        let mut build = Step::new(root, "release", Kind::Check, argv);
        build.runs = Runs::Always;
        build.env.extend(
            platform::package_env()
                .iter()
                .map(|&(name, value)| (name.to_owned(), Some(value.to_owned()))),
        );
        let release = root.target_dir.join("release");
        let mut steps = vec![build];
        for name in &root.config.package {
            let built = release.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            for split in platform::split_debug(name, &built, out) {
                let step_name = format!("{} {name}", split.makes);
                let (action, argv) = match split.by {
                    SplitBy::Tool { program, args } => (Action::Program(program.into()), args),
                    SplitBy::Copy { from, to } => (Action::Copy { from, to }, Vec::new()),
                };
                steps.push(Step {
                    action,
                    argv,
                    runs: Runs::Always,
                    ..Step::new(root, &step_name, Kind::Plain, Vec::new())
                });
            }
        }
        steps
    }

    /// The vocabulary of every root's repository, written to `out`, which the driver lists
    /// itself.
    pub(crate) fn vocabulary(self, out: PathBuf) -> Step {
        Step {
            action: Action::Vocabulary { out },
            runs: Runs::Always,
            ..Step::new(self.root, "vocabulary", Kind::Plain, Vec::new())
        }
    }

    /// The public API surface of `member`, whose library's crate name is `lib`, written to `out`:
    /// rustdoc JSON with no features, then with each public feature alone, each read before the
    /// next build writes the same file. Stable rustdoc writes JSON under `RUSTC_BOOTSTRAP`, in a
    /// target folder of its own: the build scripts that read the variable, as `proc-macro2`'s,
    /// would otherwise run again in the next build of every other step.
    pub(crate) fn surface(self, member: &Member, lib: &str, out: PathBuf) -> Vec<Step> {
        let target = self.root.target_dir.join("drive").join("surface");
        let json = target.join("doc").join(format!("{lib}.json"));
        let features = (member.features.iter())
            .filter(|feature| !NOT_SURFACE.contains(&feature.as_str()))
            .cloned()
            .map(Build::Feature);
        let mut steps = Vec::new();
        for build in std::iter::once(Build::NoDefault).chain(features) {
            let name = build.to_string();
            let mut argv = vec![
                "rustdoc",
                "-p",
                member.name.as_str(),
                "--lib",
                "--no-default-features",
            ];
            if let Build::Feature(feature) = &build {
                argv.extend(["--features", feature.as_str()]);
            }
            argv.extend(self.lock());
            argv.extend(["-Zunstable-options", "--output-format", "json", JSON]);
            let mut rustdoc = Step::new(self.root, &format!("rustdoc.{name}"), Kind::Check, argv);
            rustdoc.argv.extend([
                OsString::from("--target-dir"),
                target.clone().into(),
                OsString::from("--"),
                OsString::from("--document-hidden-items"),
            ]);
            rustdoc
                .env
                .push(("RUSTC_BOOTSTRAP".to_owned(), Some("1".to_owned())));
            rustdoc.runs = Runs::Always;
            steps.push(rustdoc);
            steps.push(Step {
                action: Action::SurfaceBuild {
                    build,
                    json: json.clone(),
                },
                runs: Runs::Always,
                ..Step::new(
                    self.root,
                    &format!("surface.{name}"),
                    Kind::Plain,
                    Vec::new(),
                )
            });
        }
        steps.push(Step {
            action: Action::Surface { out },
            runs: Runs::Always,
            ..Step::new(self.root, "surface", Kind::Plain, Vec::new())
        });
        steps
    }

    /// The steps of `sweep`, the lints first, as they answer in seconds, and the docs last. A
    /// local gate lints the targets of other OSes after the host's, as CI lints each on its own
    /// OS; the gate's own commands and its build of every bench follow the tests.
    pub(crate) fn every(self, sweep: Sweep) -> Vec<Step> {
        let root = self.root;
        let mut steps = vec![self.fmt()];
        match sweep {
            Sweep::Check => steps.extend(self.lints(&[FlagSet::all_features()])),
            Sweep::Gate | Sweep::GateCi => {
                steps.extend(self.lints(&root.config.lint));
                steps.push(self.ship());
                if sweep == Sweep::Gate {
                    steps.extend(self.cross_lints());
                }
                steps.extend(self.tests(&[], Runs::OnChange));
                steps.extend(self.joined(Joins::Gate));
                if root.config.gate_benches {
                    steps.extend(self.gate_benches());
                }
                if root.config.docs {
                    steps.push(self.doc());
                    steps.extend(self.doctests(Runs::OnChange));
                }
            }
        }
        steps
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
mod tests;
