use clap::{Arg, ArgMatches, Command};

/// One command of `cargo drive`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Task {
    /// The check after a change: the root's `check` lanes, each as its passes allow.
    Check,
    /// The tests, every one or those nextest's arguments select, each time.
    Test(Vec<String>),
    /// The benches, with the bench binaries' arguments.
    Bench(Vec<String>),
    /// The root's `gate` lanes, which CI runs, with `fmt --check`.
    Gate,
}

impl Task {
    pub(crate) fn parse() -> Self {
        Self::from_matches(&command().get_matches())
    }

    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Test(_) => "test",
            Self::Bench(_) => "bench",
            Self::Gate => "gate",
        }
    }

    pub(crate) fn args(&self) -> &[String] {
        match self {
            Self::Test(args) | Self::Bench(args) => args,
            Self::Check | Self::Gate => &[],
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
            "check" => Self::Check,
            "test" => Self::Test(args()),
            "bench" => Self::Bench(args()),
            "gate" => Self::Gate,
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
        .subcommand(
            Command::new("check")
                .about("The check after a change: fmt, lint, ship, tests, doc tests by default"),
        )
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
            Command::new("gate").about(
                "Every step CI runs: check's lanes and rustdoc by default, with fmt --check",
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
        assert_eq!(parse(&["drive", "check"]), Task::Check);
        assert_eq!((Task::Gate.name(), Task::Gate.args()), ("gate", &[][..]));
        let bench = parse(&["drive", "bench", "--test"]);
        assert_eq!(
            (bench.name(), bench.args()),
            ("bench", &["--test".to_owned()][..])
        );
        assert_eq!(parse(&["drive", "gate"]), Task::Gate);
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
        assert!(fails(&["drive", "lint"]));
    }
}
