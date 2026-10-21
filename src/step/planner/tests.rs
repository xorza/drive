use super::*;
use crate::flag_set::Targets;

fn join(step: &Step) -> String {
    step.words().join(" ")
}

/// The planner that keeps the files, as CI's.
fn keep(root: &Root) -> Planner<'_> {
    Planner::new(root, Files::Keep)
}

#[test]
fn lane_commands() {
    let root = Root::fixture();
    let ci = keep(&root);
    assert_eq!(
        join(&ci.fmt()),
        "fmt -p core -p app -p harness -p check --check"
    );
    let lints = ci.lints(&root.lanes.lint);
    assert_eq!(lints.len(), 1);
    assert_eq!(lints[0].name, "lint");
    assert_eq!(
        join(&lints[0]),
        "clippy --workspace --all-targets --all-features --locked --keep-going \
         --message-format=json-diagnostic-short -- -D warnings"
    );
    assert_eq!(
        join(&ci.ship()),
        "clippy --workspace --locked --keep-going --message-format=json-diagnostic-short \
         --exclude check --exclude harness -- -D warnings"
    );
    let doc = ci.doc();
    assert_eq!(
        doc.env,
        [("RUSTDOCFLAGS".to_owned(), Some("-D warnings".to_owned()))]
    );
    assert_eq!(lints[0].env, [("RUSTDOCFLAGS".to_owned(), None)]);

    let tests = ci.tests(&["name".to_owned()], Runs::Always);
    assert_eq!(tests[0].runs, Runs::Always);
    assert!(join(&tests[0]).starts_with("nextest run --workspace --all-features --locked"));
    assert!(join(&tests[0]).ends_with("--success-output never name"));
    let mut cargo_test = Root::fixture();
    cargo_test.lanes.test_runner = TestRunner::CargoTest;
    let libtest = keep(&cargo_test).tests(&["name".to_owned()], Runs::OnChange);
    assert_eq!(libtest[0].kind, Kind::CargoTest);
    assert_eq!(
        join(&libtest[0]),
        "test --workspace --all-features --tests --locked --message-format=json-diagnostic-short -- name"
    );

    assert_eq!(
        join(&ci.doctests(Runs::OnChange).unwrap()),
        "test --workspace --doc --all-features --locked --message-format=json-diagnostic-short"
    );
    assert_eq!(
        join(&ci.bench(&["--test".to_owned()]).unwrap()),
        "bench --locked -p app --bench * -- --test"
    );
}

#[test]
fn a_run_that_keeps_files_checks_the_format_and_locks_only_a_committed_lockfile() {
    let mut root = Root::fixture();
    root.lanes.docs = true;
    root.lanes.gate_benches = true;
    root.lanes.cross_lint = vec!["x86_64-pc-windows-gnu".to_owned()];
    root.lanes.commands = vec![command(
        "lan",
        Some("gate"),
        &serde_json::json!([{ "build": "build" }, { "cargo": "run" }]),
    )];
    root.lanes.package = vec!["app".to_owned()];
    let out = Path::new("/w/target/package");
    let steps = |root: &Root, files| {
        let planner = Planner::new(root, files);
        let mut steps = planner.every(Sweep::Gate);
        steps.extend(planner.bench(&[]));
        steps.extend(planner.package(out).into_iter().take(1));
        steps
    };
    let (kept, written) = (steps(&root, Files::Keep), steps(&root, Files::Write));
    let names: Vec<&str> = kept.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "fmt", "lint", "ship", "cross", "tests", "lan.1", "lan.2", "benches", "doc",
            "doctests", "bench", "release"
        ]
    );
    // Each kept step's command less its one flag is the written one's: `--check` for fmt,
    // `--locked` for every cargo command that resolves the dependencies.
    for (kept, written) in kept.iter().zip(&written) {
        let flag = if kept.kind == Kind::Format {
            "--check"
        } else {
            "--locked"
        };
        let mut less = kept.words();
        let at = (less.iter().position(|w| w == flag)).expect("a kept step names its flag");
        less.remove(at);
        assert!(!less.contains(&flag.to_owned()));
        assert_eq!(less, written.words(), "{}", kept.name);
    }
    // With no lockfile committed, cargo resolves again: only fmt's `--check` stays.
    root.lockfile = Lockfile::Free;
    let free = steps(&root, Files::Keep);
    assert_eq!(free[0], kept[0]);
    assert_eq!(free[1..], written[1..]);
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
    let ci = keep(&root);
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
    let steps = ci.command(&lan, &args, Runs::OnInput);
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
    assert!(
        steps.iter().all(|s| s.shared.as_ref().map(|p| &p.command)
            == steps[0].shared.as_ref().map(|p| &p.command))
    );
    let one = command("one", None, &serde_json::json!([{ "build": "build" }]));
    assert_eq!(ci.command(&one, &[], Runs::OnInput)[0].shared, None);
    assert_eq!(
        (join(&steps[0]), steps[0].kind),
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
    assert_eq!(steps[1].words(), ["/w/target/drive/lan", "out"]);
    // The arguments go to the one step that takes them.
    assert_eq!(
        (join(&steps[2]), steps[2].kind),
        ("run -p app --locked".to_owned(), Kind::Program)
    );
    assert!(
        steps[2]
            .env
            .contains(&("BLESS".to_owned(), Some("1".to_owned())))
    );
    let tests = join(&steps[3]);
    assert!(tests.starts_with("nextest run --workspace --all-features --locked"));
    assert!(tests.ends_with("--success-output never -E test(=index)"));
    assert_eq!(steps[3].kind, Kind::Nextest);
    // A bare name is a program on `PATH`.
    assert_eq!(steps[4].program, Program::Other(PathBuf::from("objcopy")));
    assert_eq!(ci.command(&lan, &[], Runs::Always)[0].runs, Runs::Always);
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
    let names = |steps: Vec<Step>| -> Vec<String> { steps.into_iter().map(|s| s.name).collect() };
    let ci = keep(&root);
    // `test` writes the index before the tests, whole once an input changed.
    let test = ci.test_command(&[]);
    assert_eq!(names(test.clone()), ["index", "tests"]);
    assert_eq!([test[0].runs, test[1].runs], [Runs::OnInput, Runs::Always]);
    // A local gate lints for the other OS after the host's lanes; CI lints each OS on its own.
    let gate = ["fmt", "lint", "ship", "cross", "tests", "lan", "benches"];
    assert_eq!(names(ci.every(Sweep::Gate)), gate);
    assert_eq!(
        names(ci.every(Sweep::GateCi)),
        ["fmt", "lint", "ship", "tests", "lan", "benches"]
    );
    assert_eq!(names(ci.every(Sweep::Check)), ["fmt", "lint"]);
    let every = ci.every(Sweep::Gate);
    assert_eq!(
        (join(&every[3]), every[3].label.as_str()),
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
        (join(&every[6]), every[6].kind, every[6].label.as_str()),
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
        join(&keep(&root).gate_benches().unwrap()),
        "build --workspace --bench * --features golden --locked \
         --message-format=json-diagnostic-short"
    );
    root.members.iter_mut().for_each(|m| m.benches = false);
    assert_eq!(keep(&root).gate_benches(), None);
    assert_eq!(
        names(keep(&root).every(Sweep::GateCi)).last().unwrap(),
        "lan"
    );
}

#[test]
fn package_builds_the_release_then_splits_each_binary() {
    let mut root = Root::fixture();
    root.lanes.package = vec!["app".to_owned()];
    let steps = keep(&root).package(Path::new("/w/target/package"));
    assert_eq!(
        join(&steps[0]),
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
        (keep(root).every(sweep).into_iter())
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
    let planner = Planner::new(&root, Files::Write);
    let check = planner.every(Sweep::Check);
    let gate = planner.every(Sweep::Gate);
    assert_eq!(
        (check[1].argv.as_slice(), check[1].label.as_str()),
        (gate[2].argv.as_slice(), "--all-features")
    );
    assert_ne!(gate[1].argv, gate[2].argv);
    assert_eq!(check[0], gate[0]);
}

#[test]
fn numbered_lanes() {
    assert_eq!(numbered("lint", 0, 1), "lint");
    assert_eq!(numbered("lint", 1, 3), "lint.2");
}
