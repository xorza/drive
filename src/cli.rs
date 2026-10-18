use clap::{Arg, ArgAction, ArgMatches, Command};

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
        }
    }

    pub(crate) fn args(&self) -> Vec<String> {
        match self {
            Self::Test(args) | Self::Bench(args) => args.clone(),
            Self::Sweep(Sweep::GateCi) => vec!["--ci".to_owned()],
            Self::Sweep(Sweep::Check | Sweep::Gate) => Vec::new(),
        }
    }

    fn from_matches(matches: &ArgMatches) -> Self {
        let (name, sub) = matches.subcommand().expect("a subcommand is required");
        let args = || {
            sub.get_many::<String>("args")
                .map(|v| v.cloned().collect())
                .unwrap_or_default()
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
    Command::new("drive")
        .bin_name("cargo drive")
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
                    "Every step CI runs: fmt, each lint set, ship, the tests, and rustdoc and the \
                     doc tests where `docs = true`; the first failure ends it",
                )
                .arg(
                    Arg::new("ci")
                        .long("ci")
                        .action(ArgAction::SetTrue)
                        .help("For CI: list the unformatted files, and run past failures"),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Task {
        Task::from_matches(&command().try_get_matches_from(argv).unwrap())
    }

    #[test]
    fn passes_arguments_through() {
        let check = parse(&["drive", "check"]);
        assert_eq!(check, Task::Sweep(Sweep::Check));
        assert_eq!((check.name(), check.args()), ("check", Vec::new()));
        let gate = parse(&["drive", "gate"]);
        assert_eq!(gate, Task::Sweep(Sweep::Gate));
        assert_eq!((gate.name(), gate.args()), ("gate", Vec::new()));
        let ci = parse(&["drive", "gate", "--ci"]);
        assert_eq!(ci, Task::Sweep(Sweep::GateCi));
        assert_eq!((ci.name(), ci.args()), ("gate", vec!["--ci".to_owned()]));
        let bench = parse(&["drive", "bench", "--test"]);
        assert_eq!(
            (bench.name(), bench.args()),
            ("bench", vec!["--test".to_owned()])
        );
        assert_eq!(parse(&["drive", "test"]), Task::Test(Vec::new()));
        assert_eq!(
            parse(&["drive", "test", "-E", "package(lumos)", "--no-capture"]),
            Task::Test(
                ["-E", "package(lumos)", "--no-capture"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert_eq!(
            parse(&["drive", "bench", "--test", "^atomic/"]),
            Task::Bench(["--test", "^atomic/"].map(str::to_owned).to_vec())
        );
        let fails = |argv: &[&str]| command().try_get_matches_from(argv).is_err();
        assert!(fails(&["drive"]));
        assert!(fails(&["drive", "check", "-p", "x"]));
        assert!(fails(&["drive", "check", "--ci"]));
        assert!(fails(&["drive", "lint"]));
    }
}
