use crate::runner::OnFail;
use crate::step::planner::Files;

/// A command that runs fixed lanes on every root: `check`, or `gate` with or without `--ci`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sweep {
    /// The check after a change: fmt, and a lint of every target with every feature, to the first
    /// failure.
    Check,
    /// Every lane: fmt, a lint with each set of the root, ship, the lints of other OSes, the
    /// tests, the root's commands that join it, and the build of every bench, rustdoc and the doc
    /// tests where the root asks for them, to the first failure.
    Gate,
    /// Every lane for CI, which can change no file, the lockfile among them, and reports every
    /// failure of one run.
    GateCi,
}

impl Sweep {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Gate | Self::GateCi => "gate",
        }
    }

    pub(crate) const fn files(self) -> Files {
        match self {
            Self::Check | Self::Gate => Files::Write,
            Self::GateCi => Files::Keep,
        }
    }

    pub(crate) const fn on_fail(self) -> OnFail {
        match self {
            Self::Check | Self::Gate => OnFail::Stop,
            Self::GateCi => OnFail::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ci_keeps_the_files_and_runs_past_failures() {
        let of = |s: Sweep| (s.name(), s.files(), s.on_fail());
        assert_eq!(
            [Sweep::Check, Sweep::Gate, Sweep::GateCi].map(of),
            [
                ("check", Files::Write, OnFail::Stop),
                ("gate", Files::Write, OnFail::Stop),
                ("gate", Files::Keep, OnFail::Continue),
            ]
        );
    }
}
