use std::collections::BTreeMap;

use serde::Deserialize;

use crate::config::TestRunner;
use crate::error::ConfigError;

/// A command of the project's own, `cargo drive run <name> [args]`, as its `commands.<name>`
/// table gives it: its steps, in order, the command of the driver whose run it joins too, and the
/// texts that mark a line of its plain steps' output as an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectCommand {
    pub(crate) name: String,
    pub(crate) joins: Option<Joins>,
    pub(crate) steps: Vec<CommandStep>,
    /// A failed plain step shows its lines that hold one, or its last lines where none does:
    /// a program's first lines are seldom its error.
    pub(crate) errors: Vec<String>,
}

/// The command of the driver whose run a project command joins, where it runs there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Joins {
    /// `test`, before the tests, as a step that writes what the tests read.
    Test,
    /// `gate`, after the tests, with `--ci` too.
    Gate,
}

/// One step of a project command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandStep {
    pub(crate) action: CommandAction,
    /// Variables it sets.
    pub(crate) env: Vec<(String, String)>,
    /// Whether the command's arguments go to it, after cargo's own for a cargo step.
    pub(crate) takes_args: bool,
}

/// What a step of a project command runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandAction {
    /// A cargo build or lint, whose diagnostics the driver reads: cargo's arguments.
    Build(Vec<String>),
    /// Another cargo command, whose output is its own, as `run`: cargo's arguments.
    Cargo(Vec<String>),
    /// A program, by its path from the root, then its arguments: `{target}` names the root's
    /// target folder, and `{exe}` the OS's suffix of an executable.
    Program(Vec<String>),
    /// The tests nextest's filter selects, under the first test lane's flags.
    Tests(String),
}

impl CommandAction {
    /// Its output is its own, which the driver does not read as diagnostics or tests.
    pub(crate) const fn plain(&self) -> bool {
        matches!(self, Self::Cargo(_) | Self::Program(_))
    }
}

/// A `commands.<name>` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandTable {
    joins: Option<Joins>,
    steps: Vec<StepTable>,
    errors: Option<Vec<String>>,
}

/// A step's table: one of `build`, `cargo`, `program` and `tests`, with its variables, and
/// whether the command's arguments go to it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepTable {
    build: Option<String>,
    cargo: Option<String>,
    program: Option<Vec<String>>,
    tests: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    args: bool,
}

impl ProjectCommand {
    /// The command `name` of `table`; an error for a command of no step, a step of no kind or of
    /// two, the arguments taken by two steps, tests under a runner other than nextest, which
    /// alone takes a filter, and error texts that are empty, or that no plain step prints.
    pub(crate) fn resolve(
        name: String,
        table: CommandTable,
        runner: TestRunner,
    ) -> Result<Self, ConfigError> {
        let CommandTable {
            joins,
            steps,
            errors,
        } = table;
        if steps.is_empty() {
            return Err(ConfigError::EmptyList {
                key: "commands.steps",
            });
        }
        if steps.iter().filter(|step| step.args).count() > 1 {
            return Err(ConfigError::ArgsTwice { command: name });
        }
        let steps: Vec<CommandStep> = steps
            .into_iter()
            .enumerate()
            .map(|(at, step)| step.resolve(&name, at, runner))
            .collect::<Result<_, _>>()?;
        let errors = match errors {
            None => Vec::new(),
            Some(errors) if errors.is_empty() => {
                return Err(ConfigError::EmptyList {
                    key: "commands.errors",
                });
            }
            Some(errors) if errors.iter().any(String::is_empty) => {
                return Err(ConfigError::EmptyErrorText { command: name });
            }
            Some(_) if !steps.iter().any(|step| step.action.plain()) => {
                return Err(ConfigError::ErrorsUnread { command: name });
            }
            Some(errors) => errors,
        };
        Ok(Self {
            name,
            joins,
            steps,
            errors,
        })
    }

    /// Whether a step takes the command's arguments.
    pub(crate) fn takes_args(&self) -> bool {
        self.steps.iter().any(|step| step.takes_args)
    }
}

impl StepTable {
    fn resolve(
        self,
        command: &str,
        at: usize,
        runner: TestRunner,
    ) -> Result<CommandStep, ConfigError> {
        let words = |text: String| text.split_whitespace().map(str::to_owned).collect();
        let action = match (self.build, self.cargo, self.program, self.tests) {
            (Some(build), None, None, None) => CommandAction::Build(words(build)),
            (None, Some(cargo), None, None) => CommandAction::Cargo(words(cargo)),
            (None, None, Some(program), None) if !program.is_empty() => {
                CommandAction::Program(program)
            }
            (None, None, None, Some(tests)) if runner == TestRunner::Nextest => {
                CommandAction::Tests(tests)
            }
            (None, None, None, Some(_)) => {
                return Err(ConfigError::TestsNeedNextest {
                    command: command.to_owned(),
                });
            }
            _ => {
                return Err(ConfigError::StepKind {
                    command: command.to_owned(),
                    step: at + 1,
                });
            }
        };
        Ok(CommandStep {
            action,
            env: self.env.into_iter().collect(),
            takes_args: self.args,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn resolve(value: serde_json::Value, runner: TestRunner) -> Result<ProjectCommand, String> {
        let table: CommandTable = serde_json::from_value(value).map_err(|e| e.to_string())?;
        ProjectCommand::resolve("lan".to_owned(), table, runner).map_err(|e| e.to_string())
    }

    #[test]
    fn a_command_holds_steps_of_one_kind_each() {
        let lan = resolve(
            json!({
                "joins": "gate",
                "steps": [
                    { "build": "build --bins  -p check" },
                    { "program": ["{target}/debug/check{exe}", "out"], "args": true },
                    { "tests": "test(=index)", "env": { "BLESS": "1" } },
                    { "cargo": "run -p check" },
                ],
            }),
            TestRunner::Nextest,
        )
        .unwrap();
        assert_eq!(lan.joins, Some(Joins::Gate));
        assert!(lan.takes_args());
        let actions: Vec<&CommandAction> = lan.steps.iter().map(|step| &step.action).collect();
        let owned = |words: &[&str]| words.iter().map(|&w| w.to_owned()).collect();
        assert_eq!(
            actions,
            [
                &CommandAction::Build(owned(&["build", "--bins", "-p", "check"])),
                &CommandAction::Program(owned(&["{target}/debug/check{exe}", "out"])),
                &CommandAction::Tests("test(=index)".to_owned()),
                &CommandAction::Cargo(owned(&["run", "-p", "check"])),
            ]
        );
        assert_eq!(lan.steps[2].env, [("BLESS".to_owned(), "1".to_owned())]);
        assert!(!lan.steps[0].takes_args);

        let fails = |value, runner| resolve(value, runner).unwrap_err();
        let nextest = TestRunner::Nextest;
        assert_eq!(
            fails(json!({ "steps": [] }), nextest),
            "drive.commands.steps: must be a non-empty list"
        );
        assert_eq!(
            fails(
                json!({ "steps": [{ "build": "b", "cargo": "c" }] }),
                nextest
            ),
            "drive.commands.lan: step 1 names not one of `build`, `cargo`, `program` and `tests`"
        );
        assert_eq!(
            fails(
                json!({ "steps": [{ "build": "b" }, { "program": [] }] }),
                nextest
            ),
            "drive.commands.lan: step 2 names not one of `build`, `cargo`, `program` and `tests`"
        );
        assert_eq!(
            fails(
                json!({ "steps": [{ "build": "b", "args": true }, { "cargo": "c", "args": true }] }),
                nextest
            ),
            "drive.commands.lan: two steps take the command's arguments"
        );
        assert_eq!(
            fails(
                json!({ "steps": [{ "tests": "all()" }] }),
                TestRunner::CargoTest
            ),
            "drive.commands.lan: a `tests` step takes a nextest filter, and the root runs \
             `cargo test`"
        );
        assert!(
            fails(
                json!({ "joins": "check", "steps": [{ "build": "b" }] }),
                nextest
            )
            .contains("unknown variant `check`")
        );
        assert!(lan.errors.is_empty());
        let check = |errors| json!({ "steps": [{ "cargo": "run" }], "errors": errors });
        assert_eq!(
            resolve(check(json!(["ERROR", "error["])), nextest)
                .unwrap()
                .errors,
            ["ERROR", "error["]
        );
        assert_eq!(
            fails(check(json!([])), nextest),
            "drive.commands.errors: must be a non-empty list"
        );
        assert_eq!(
            fails(check(json!(["ERROR", ""])), nextest),
            "drive.commands.lan: an error text is empty, which every line holds"
        );
        assert_eq!(
            fails(
                json!({ "steps": [{ "build": "b" }, { "tests": "all()" }], "errors": ["E"] }),
                nextest
            ),
            "drive.commands.lan: `errors` marks the lines of a `cargo` or `program` step, and \
             the command has none"
        );
    }
}
