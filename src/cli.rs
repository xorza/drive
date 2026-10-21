use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::output::Ci;
use crate::step::planner::Files;
use crate::sweep::Sweep;

/// One command of `cargo drive`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Task {
    /// `check` or `gate`: fixed lanes on every root, each as its passes allow.
    Sweep(Sweep),
    /// The tests, every one or those nextest's arguments select, each time.
    Test(Vec<String>),
    /// The benches, with the bench binaries' arguments.
    Bench(Vec<String>),
    /// A command of the root's own, by its name, with its arguments.
    Run { name: String, args: Vec<String> },
    /// The release build of the binaries the root ships, stripped, with their debug info beside.
    Package,
}

impl Task {
    pub(crate) fn parse() -> Self {
        Self::from_matches(&command().get_matches())
    }

    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Sweep(sweep) => sweep.name(),
            Self::Test(_) => "test",
            Self::Bench(_) => "bench",
            Self::Run { .. } => "run",
            Self::Package => "package",
        }
    }

    /// What the verdict calls the command: `check`, `run lan`.
    pub(crate) fn title(&self) -> String {
        match self {
            Self::Run { name, .. } => format!("run {name}"),
            _ => self.name().to_owned(),
        }
    }

    /// The name of the file that holds what the run printed: `last-check.txt`,
    /// `last-run-lan.txt`.
    pub(crate) fn output_file(&self) -> String {
        let title = self.title();
        let name = title.replace(|c: char| !c.is_ascii_alphanumeric() && c != '_', "-");
        format!("last-{name}.txt")
    }

    /// Whether it may change the files it checks, run in `ci`: a sweep as it says, a release
    /// build never, any other command only outside CI. A release and a run in CI stand for the
    /// commit, so they build its lockfile.
    pub(crate) const fn files(&self, ci: Ci) -> Files {
        match (self, ci) {
            (Self::Sweep(sweep), _) => sweep.files(),
            (Self::Package, _) | (_, Ci::GitHub) => Files::Keep,
            (Self::Test(_) | Self::Bench(_) | Self::Run { .. }, Ci::No) => Files::Write,
        }
    }

    pub(crate) fn args(&self) -> Vec<String> {
        match self {
            Self::Test(args) | Self::Bench(args) => args.clone(),
            Self::Run { name, args } => std::iter::once(name).chain(args).cloned().collect(),
            Self::Sweep(Sweep::GateCi) => vec!["--ci".to_owned()],
            Self::Sweep(Sweep::Check | Sweep::Gate) | Self::Package => Vec::new(),
        }
    }

    fn from_matches(matches: &ArgMatches) -> Self {
        let drive = matches
            .subcommand_matches("drive")
            .expect("cargo runs the driver as `cargo-drive drive`");
        let (name, sub) = drive.subcommand().expect("a subcommand is required");
        let args = || {
            sub.get_many::<String>("args")
                .map_or_default(|v| v.cloned().collect())
        };
        match name {
            "check" => Self::Sweep(Sweep::Check),
            "test" => Self::Test(args()),
            "bench" => Self::Bench(args()),
            "gate" => Self::Sweep(if sub.get_flag("ci") {
                Sweep::GateCi
            } else {
                Sweep::Gate
            }),
            "run" => Self::Run {
                name: sub
                    .get_one::<String>("name")
                    .expect("clap requires a name")
                    .clone(),
                args: args(),
            },
            "package" => Self::Package,
            _ => unreachable!("clap knows only these subcommands"),
        }
    }
}

fn command() -> Command {
    let passed = |help: &'static str| {
        Arg::new("args")
            .num_args(0..)
            .trailing_var_arg(true)
            .allow_hyphen_values(true)
            .value_name("ARGS")
            .help(help)
    };
    // Cargo runs `cargo drive …` as `cargo-drive drive …`: the driver is a subcommand of cargo's.
    let drive = Command::new("drive")
        .about("The one way to check, test, document and bench the workspace")
        .subcommand_required(true)
        .subcommand(Command::new("check").about(
            "The check after a change: fmt, then clippy on every target with every feature; \
             the first failure ends it",
        ))
        .subcommand(
            Command::new("test")
                .about("The tests, each time; with arguments, only the ones nextest selects")
                .arg(passed(
                    "nextest arguments: a test name, or -E 'package(name)'",
                )),
        )
        .subcommand(
            Command::new("bench")
                .about("The benches of every member that has one")
                .arg(passed(
                    "criterion arguments: a filter, or --test to run each case once",
                )),
        )
        .subcommand(
            Command::new("gate")
                .about(
                    "Every step CI runs: fmt, each lint set, ship, the lints of other OSes, the \
                     tests, the root's commands that join it, and the build of every bench, \
                     rustdoc and the doc tests where the drive table asks; the first failure \
                     ends it",
                )
                .arg(Arg::new("ci").long("ci").action(ArgAction::SetTrue).help(
                    "For CI: list the unformatted files, fail on a lockfile cargo would \
                     update, and run past failures",
                )),
        )
        .subcommand(
            Command::new("run")
                .about(
                    "A command of the root's own, from `commands` in its drive table, each \
                     time; with arguments for the step that takes them",
                )
                .arg(Arg::new("name").required(true).value_name("NAME"))
                .arg(passed("the arguments of the step that takes them")),
        )
        .subcommand(Command::new("package").about(
            "The release build of the binaries `package` names, each stripped in \
             target/package with its debug info beside it",
        ));
    Command::new("cargo")
        .bin_name("cargo")
        .subcommand_required(true)
        .subcommand(drive)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Task {
        Task::from_matches(&command().try_get_matches_from(argv).unwrap())
    }

    #[test]
    fn passes_arguments_through() {
        let check = parse(&["cargo-drive", "drive", "check"]);
        assert_eq!(check, Task::Sweep(Sweep::Check));
        assert_eq!((check.name(), check.args()), ("check", Vec::new()));
        let gate = parse(&["cargo-drive", "drive", "gate"]);
        assert_eq!(gate, Task::Sweep(Sweep::Gate));
        assert_eq!((gate.name(), gate.args()), ("gate", Vec::new()));
        let ci = parse(&["cargo-drive", "drive", "gate", "--ci"]);
        assert_eq!(ci, Task::Sweep(Sweep::GateCi));
        assert_eq!((ci.name(), ci.args()), ("gate", vec!["--ci".to_owned()]));
        let bench = parse(&["cargo-drive", "drive", "bench", "--test"]);
        assert_eq!(
            (bench.name(), bench.args()),
            ("bench", vec!["--test".to_owned()])
        );
        assert_eq!(
            parse(&["cargo-drive", "drive", "test"]),
            Task::Test(Vec::new())
        );
        assert_eq!(
            parse(&[
                "cargo-drive",
                "drive",
                "test",
                "-E",
                "package(lumos)",
                "--no-capture"
            ]),
            Task::Test(
                ["-E", "package(lumos)", "--no-capture"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert_eq!(
            parse(&["cargo-drive", "drive", "bench", "--test", "^atomic/"]),
            Task::Bench(["--test", "^atomic/"].map(str::to_owned).to_vec())
        );
        let lan = parse(&["cargo-drive", "drive", "run", "lan", "--bless", "out"]);
        assert_eq!(
            lan,
            Task::Run {
                name: "lan".to_owned(),
                args: ["--bless", "out"].map(str::to_owned).to_vec()
            }
        );
        assert_eq!(
            (lan.name(), lan.title(), lan.args().len()),
            ("run", "run lan".to_owned(), 3)
        );
        assert_eq!(gate.title(), "gate");
        assert_eq!(lan.output_file(), "last-run-lan.txt");
        let odd = Task::Run {
            name: "a/b c".to_owned(),
            args: Vec::new(),
        };
        assert_eq!(
            (odd.title(), odd.output_file()),
            ("run a/b c".to_owned(), "last-run-a-b-c.txt".to_owned())
        );
        let package = parse(&["cargo-drive", "drive", "package"]);
        assert_eq!((package.name(), package.args()), ("package", Vec::new()));
        let files = |task: &Task| [Ci::No, Ci::GitHub].map(|ci| task.files(ci));
        assert_eq!(files(&check), [Files::Write; 2]);
        assert_eq!(files(&ci), [Files::Keep; 2]);
        assert_eq!(files(&package), [Files::Keep; 2]);
        for task in [&bench, &lan, &Task::Test(Vec::new())] {
            assert_eq!(files(task), [Files::Write, Files::Keep]);
        }
        let fails = |argv: &[&str]| command().try_get_matches_from(argv).is_err();
        assert!(fails(&["cargo-drive", "drive", "run"]));
        assert!(fails(&["cargo-drive", "drive", "package", "x"]));
        assert!(fails(&["cargo-drive", "drive"]));
        assert!(fails(&["cargo-drive", "drive", "check", "-p", "x"]));
        assert!(fails(&["cargo-drive", "drive", "check", "--ci"]));
        assert!(fails(&["cargo-drive", "drive", "lint"]));
        assert!(fails(&["cargo-drive", "check"]));
    }
}
