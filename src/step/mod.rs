pub(crate) mod planner;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::member::Member;
use crate::passes::PassKey;
use crate::root::Root;

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

/// One command on one root, cargo's or another program's.
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

    /// Its slot is its root and its command, its own variables among it, not its name: `check`'s
    /// lint and `gate`'s lint of the same flags share a pass. Its identity is what changes its
    /// result from outside the working files: the tools' `versions`, and the variables of `env`
    /// it does not set itself. The steps of one project command share its slot, which holds each
    /// step's command and variables.
    pub(crate) fn pass_key(
        &self,
        dir: &Path,
        env: &BTreeMap<String, Option<String>>,
        versions: &[String],
    ) -> PassKey {
        let (slot, own) = if let Some(shared) = &self.shared {
            (serde_json::json!([dir, shared.command]), &[][..])
        } else {
            let slot = serde_json::json!([dir, self.argv, self.env]);
            (slot, self.env.as_slice())
        };
        let env: Vec<_> = env
            .iter()
            .filter(|(name, _)| !own.iter().any(|(set, _)| set == *name))
            .collect();
        PassKey {
            slot: slot.to_string(),
            identity: serde_json::json!([env, versions]).to_string(),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag_set::FlagSet;
    use crate::lanes::TestRunner;
    use crate::step::planner::{Files, Planner};

    impl Step {
        /// Its arguments, as text.
        pub(crate) fn words(&self) -> Vec<String> {
            let words = self
                .argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned());
            words.collect()
        }
    }

    #[test]
    fn a_pass_key_slots_the_command_and_identifies_the_tools_and_the_variables_it_leaves_alone() {
        let root = Root::fixture();
        let dir = Path::new("/w");
        let lint = Planner::new(&root, Files::Write)
            .lints(&[FlagSet::all_features()])
            .remove(0);
        let env = |pairs: &[(&str, &str)]| -> BTreeMap<String, Option<String>> {
            (pairs.iter())
                .map(|&(n, v)| (n.to_owned(), Some(v.to_owned())))
                .collect()
        };
        let one = env(&[("RUSTFLAGS", "-Cx"), ("RUSTDOCFLAGS", "-Da")]);
        let rustc = ["rustc 1".to_owned()];
        let key = lint.pass_key(dir, &one, &rustc);
        // Its name is in neither: `check`'s `lint` and `gate`'s `lint.1` share a pass.
        let renamed = Step {
            name: "lint.1".to_owned(),
            ..lint.clone()
        };
        assert_eq!(renamed.pass_key(dir, &one, &rustc), key);
        // Another toolchain or another variable it reads: the same slot, another identity.
        let other = lint.pass_key(dir, &one, &["rustc 2".to_owned()]);
        assert_eq!(other.slot, key.slot);
        assert_ne!(other.identity, key.identity);
        let flags = env(&[("RUSTFLAGS", "-Cy"), ("RUSTDOCFLAGS", "-Da")]);
        assert_ne!(lint.pass_key(dir, &flags, &rustc).identity, key.identity);
        // A variable it sets itself, as each step unsets RUSTDOCFLAGS, is its command's: the
        // outside value changes nothing, and another value of its own is another slot.
        let rustdoc = env(&[("RUSTFLAGS", "-Cx"), ("RUSTDOCFLAGS", "-Db")]);
        assert_eq!(lint.pass_key(dir, &rustdoc, &rustc), key);
        let mut blessed = lint.clone();
        blessed.env.push(("BLESS".to_owned(), Some("1".to_owned())));
        assert_ne!(blessed.pass_key(dir, &one, &rustc).slot, key.slot);
        // The steps of one project command share its slot.
        let shared = |at: &Step, last| Step {
            shared: Some(SharedPass {
                command: "lan: …".to_owned(),
                last,
            }),
            ..at.clone()
        };
        assert_eq!(
            shared(&lint, false).pass_key(dir, &one, &rustc),
            shared(&blessed, true).pass_key(dir, &one, &rustc)
        );
    }

    #[test]
    fn a_change_narrows_only_the_test_steps_it_reaches() {
        // The fixture's members in order: `core` and `harness` with doc tests, `app`, `check`.
        let root = Root::fixture();
        let planner = Planner::new(&root, Files::Write);
        let reached =
            |names: &[&str]| -> BTreeSet<String> { names.iter().map(|&n| n.to_owned()).collect() };
        let tests = planner.tests(&[], Runs::OnChange).remove(0);
        let narrowed = tests.for_reach(&root, &reached(&["app", "core"])).unwrap();
        assert!(
            narrowed
                .words()
                .join(" ")
                .ends_with("-E package(=core) | package(=app) --no-tests=pass")
        );
        assert_eq!(narrowed.label, "the tests of 2 of 4 crates; --all-features");
        let all = reached(&["core", "app", "harness", "check"]);
        assert_eq!(tests.for_reach(&root, &all), Some(tests.clone()));
        assert_eq!(tests.for_reach(&root, &reached(&["gui"])), None);

        // The doc tests count only the members that have them, and run whole.
        let doctests = planner.doctests(Runs::OnChange).unwrap();
        assert_eq!(doctests.for_reach(&root, &reached(&["app"])), None);
        assert_eq!(
            doctests.for_reach(&root, &reached(&["harness"])),
            Some(doctests)
        );
        let mut cargo_test = Root::fixture();
        cargo_test.lanes.test_runner = TestRunner::CargoTest;
        let libtest = Planner::new(&cargo_test, Files::Write)
            .tests(&[], Runs::OnChange)
            .remove(0);
        assert_eq!(
            libtest.for_reach(&cargo_test, &reached(&["check"])),
            Some(libtest.clone())
        );
        assert_eq!(libtest.for_reach(&cargo_test, &reached(&[])), None);
        // A step that runs no member's tests runs whole whatever the change reaches.
        let lint = planner.lints(&[FlagSet::all_features()]).remove(0);
        assert_eq!(lint.for_reach(&root, &reached(&[])), Some(lint));
    }

    #[test]
    fn a_log_is_named_after_the_step_with_its_root() {
        let mut root = Root::fixture();
        root.label = "palantir".to_owned();
        let fmt = Planner::new(&root, Files::Write).fmt();
        assert_eq!(fmt.name, "palantir/fmt");
        assert_eq!(fmt.log_name(), "palantir-fmt.log");
    }
}
