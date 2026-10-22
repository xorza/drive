/// Whether the driver runs in GitHub Actions, which prints each step's whole output in a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ci {
    No,
    GitHub,
}
