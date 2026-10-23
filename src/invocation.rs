use crate::error::{Error, Result};
use crate::project::Project;
use crate::runner::{OnFail, Plan, Reuse};
use crate::step::Runs;
use crate::step::planner::{Files, Planner};
use crate::task::Task;

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
                    if root.config.docs {
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
                    .config
                    .command(name)
                    .ok_or_else(|| Error::UnknownCommand {
                        name: name.clone(),
                        known: root
                            .config
                            .commands
                            .iter()
                            .map(|c| c.name.clone())
                            .collect(),
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
                if root.config.package.is_empty() {
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
            Task::Vocabulary(out) => {
                let out = (out.clone())
                    .unwrap_or_else(|| root.target_dir.join("drive").join("vocabulary.txt"));
                top(vec![planner.vocabulary(out)], Reuse::Never, OnFail::Stop)
            }
            Task::Surface { package, out } => {
                let roots = project.roots();
                let (root, member) = match package {
                    Some(name) => (roots.iter())
                        .find_map(|root| {
                            let member = root.members.iter().find(|m| &m.name == name)?;
                            Some((root, member))
                        })
                        .ok_or_else(|| Error::UnknownPackage {
                            name: name.clone(),
                            known: (roots.iter())
                                .flat_map(|root| &root.members)
                                .map(|m| m.name.clone())
                                .collect(),
                        })?,
                    None => (root.members.iter())
                        .find(|m| m.dir == root.dir)
                        .map(|member| (root, member))
                        .ok_or_else(|| Error::NoRootPackage {
                            dir: root.dir.clone(),
                        })?,
                };
                let lib = member.lib.as_deref().ok_or_else(|| Error::NoLibrary {
                    package: member.name.clone(),
                })?;
                let out = (out.clone())
                    .unwrap_or_else(|| member.dir.join(".notes").join("API_SURFACE.md"));
                Self {
                    plans: vec![Plan {
                        root,
                        steps: Planner::new(root, files).surface(member, lib, out),
                    }],
                    reuse: Reuse::Never,
                    on_fail: OnFail::Stop,
                }
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::root::Root;
    use crate::step::Action;

    fn surface<'p>(project: &'p Project, package: Option<&str>) -> Result<Invocation<'p>> {
        let task = Task::Surface {
            package: package.map(str::to_owned),
            out: None,
        };
        Invocation::new(&task, project, Files::Write)
    }

    fn out(invocation: &Invocation<'_>) -> PathBuf {
        let steps = &invocation.plans[0].steps;
        match &steps.last().unwrap().action {
            Action::Surface { out } => out.clone(),
            other => panic!("the last step writes no surface: {other:?}"),
        }
    }

    #[test]
    fn a_surface_finds_its_member_in_any_root_and_defaults_to_the_top_package() {
        // The fixture's root `/w` is no package; `gui`, at `/w/gui`, is the second root's.
        let mut gui = Root::fixture();
        gui.dir = PathBuf::from("/w/gui");
        gui.members[0].name = "gui".to_owned();
        gui.members[0].dir = PathBuf::from("/w/gui");
        let project = Project::fixture(vec![Root::fixture(), gui]);

        let core = surface(&project, Some("core")).unwrap();
        assert_eq!(core.plans.len(), 1);
        assert_eq!(core.plans[0].root.dir, Path::new("/w"));
        assert_eq!(out(&core), Path::new("/w/core/.notes/API_SURFACE.md"));
        assert_eq!((core.reuse, core.on_fail), (Reuse::Never, OnFail::Stop));
        let gui = surface(&project, Some("gui")).unwrap();
        assert_eq!(gui.plans[0].root.dir, Path::new("/w/gui"));
        assert_eq!(out(&gui), Path::new("/w/gui/.notes/API_SURFACE.md"));
        let given = Task::Surface {
            package: Some("gui".to_owned()),
            out: Some(PathBuf::from("api.md")),
        };
        let given = Invocation::new(&given, &project, Files::Write).unwrap();
        assert_eq!(out(&given), Path::new("api.md"));

        let error = |package| surface(&project, package).unwrap_err().to_string();
        assert_eq!(
            error(None),
            "/w is a workspace of no package of its own; name a member with -p"
        );
        assert_eq!(
            error(Some("x")),
            "no member `x`; the members: core, app, harness, check, gui, app, harness, check"
        );
        assert_eq!(
            error(Some("check")),
            "`check` has no library, which the surface documents"
        );
        let mut packaged = Root::fixture();
        packaged.members[0].dir = PathBuf::from("/w");
        let packaged = Project::fixture(vec![packaged]);
        assert_eq!(
            out(&surface(&packaged, None).unwrap()),
            Path::new("/w/.notes/API_SURFACE.md")
        );
    }
}
