use crate::cli::Task;
use crate::error::{Error, Result};
use crate::project::Project;
use crate::runner::{OnFail, Plan, Reuse};
use crate::step::Runs;
use crate::step::planner::{Files, Planner};

/// What a task runs: the steps of each root, whether they reuse passes, and whether a failure
/// ends them.
#[derive(Debug)]
pub(crate) struct Invocation<'p> {
    pub(crate) plans: Vec<Plan<'p>>,
    pub(crate) reuse: Reuse,
    pub(crate) on_fail: OnFail,
}

impl<'p> Invocation<'p> {
    /// The steps of `task` on `project`, each as `files` allows.
    pub(crate) fn new(task: &Task, project: &'p Project, files: Files) -> Result<Self> {
        let root = project.top();
        let planner = Planner::new(root, files);
        let top = |steps, reuse, on_fail| Self {
            plans: vec![Plan { root, steps }],
            reuse,
            on_fail,
        };
        Ok(match task {
            Task::Sweep(sweep) => Self {
                plans: (project.roots().iter())
                    .map(|root| Plan {
                        root,
                        steps: Planner::new(root, files).every(*sweep),
                    })
                    .collect(),
                reuse: Reuse::Passes,
                on_fail: sweep.on_fail(),
            },
            Task::Test(args) => {
                let mut steps = planner.test_command(args);
                // nextest's arguments select no doc test; a whole run keeps its pass for `gate`,
                // with the doc tests where it runs them.
                let reuse = if args.is_empty() {
                    if root.lanes.docs {
                        steps.extend(planner.doctests(Runs::Always));
                    }
                    Reuse::Passes
                } else {
                    Reuse::Never
                };
                top(steps, reuse, OnFail::Continue)
            }
            Task::Run { name, args } => {
                let command = root
                    .lanes
                    .command(name)
                    .ok_or_else(|| Error::UnknownCommand {
                        name: name.clone(),
                        known: root.lanes.commands.iter().map(|c| c.name.clone()).collect(),
                    })?;
                if !args.is_empty() && !command.takes_args() {
                    return Err(Error::ArgsRefused {
                        command: name.clone(),
                    });
                }
                // Its passes stay for the commands it joins.
                let steps = planner.command(command, args, Runs::Always);
                top(steps, Reuse::Passes, OnFail::Stop)
            }
            Task::Package => {
                if root.lanes.package.is_empty() {
                    return Err(Error::NothingToPackage {
                        dir: root.dir.clone(),
                    });
                }
                let out = root.target_dir.join("package");
                std::fs::create_dir_all(&out).map_err(|source| Error::Io {
                    path: out.clone(),
                    source,
                })?;
                top(planner.package(&out), Reuse::Never, OnFail::Stop)
            }
            Task::Bench(args) => {
                let step = planner.bench(args).ok_or_else(|| Error::NoBenches {
                    dir: root.dir.clone(),
                })?;
                top(vec![step], Reuse::Never, OnFail::Continue)
            }
        })
    }
}
