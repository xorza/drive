use std::collections::BTreeSet;
use std::path::Path;

use crate::output::Ci;

/// The driver's terminal. Outside CI it keeps each line it prints for the run's output file: an
/// agent that reads only the last lines of a run reads the rest there, not in a second run. CI
/// prints each step's whole output in a group, and the job keeps its log itself.
#[derive(Debug)]
pub(crate) struct Console {
    ci: Ci,
    text: Option<String>,
    /// The error diagnostics the run printed, which a later step counts but does not print again.
    shown: BTreeSet<String>,
}

impl Console {
    pub(crate) fn new(ci: Ci) -> Self {
        Self {
            ci,
            text: (ci == Ci::No).then(String::new),
            shown: BTreeSet::new(),
        }
    }

    pub(crate) const fn ci(&self) -> Ci {
        self.ci
    }

    #[expect(
        clippy::print_stdout,
        reason = "the one place the driver prints its lines"
    )]
    pub(crate) fn line(&mut self, line: &str) {
        println!("{line}");
        if let Some(text) = &mut self.text {
            text.push_str(line);
            text.push('\n');
        }
    }

    /// Whether an earlier step printed `diagnostic`.
    pub(crate) fn has_shown(&self, diagnostic: &str) -> bool {
        self.shown.contains(diagnostic)
    }

    pub(crate) fn mark_shown(&mut self, diagnostic: String) {
        self.shown.insert(diagnostic);
    }

    /// Whether it keeps its lines for a file: outside CI.
    pub(crate) const fn keeps(&self) -> bool {
        self.text.is_some()
    }

    /// Every line printed, in place of what `path` held: written aside and moved there whole, so
    /// a run beside this one leaves the file whole.
    pub(crate) fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = self
            .text
            .as_deref()
            .expect("a console that keeps its lines");
        let name = path.file_name().expect("an output file has a name");
        let aside = path.with_file_name(format!(
            "{}-{}.tmp",
            name.to_string_lossy(),
            std::process::id()
        ));
        std::fs::write(&aside, text)?;
        std::fs::rename(&aside, path)
    }
}

#[cfg(test)]
impl Console {
    pub(crate) fn text(&self) -> &str {
        self.text.as_deref().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_replaces_the_file_and_ci_keeps_nothing() {
        let dir = std::env::temp_dir().join(format!("drive-console-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("last-check.txt");
        let mut first = Console::new(Ci::No);
        first.line("fmt   ok");
        first.line("lint  FAIL");
        first.save(&path).unwrap();
        let mut second = Console::new(Ci::No);
        second.line("drive check: ok");
        second.save(&path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        let left = std::fs::read_dir(&dir).unwrap().count();
        std::fs::remove_dir_all(&dir).unwrap();
        // The second run's lines alone, and no file left aside.
        assert_eq!((saved.as_str(), left), ("drive check: ok\n", 1));
        assert!(first.keeps());

        let mut ci = Console::new(Ci::GitHub);
        ci.line("::group::lint");
        assert!(!ci.keeps());
        assert_eq!((ci.ci(), ci.text()), (Ci::GitHub, ""));
    }

    #[test]
    fn remembers_the_diagnostics_it_showed() {
        let mut console = Console::new(Ci::No);
        assert!(!console.has_shown("e"));
        console.mark_shown("e".to_owned());
        assert!(console.has_shown("e") && !console.has_shown("f"));
    }
}
