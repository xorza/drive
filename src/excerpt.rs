//! [`Excerpt`]: the lines of its own a failed step shows again at the end of the run, and the
//! [`Picker`] that picks them as they come.

use std::collections::VecDeque;

/// Lines from one end of a step's own lines, and how many it had in all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Excerpt {
    lines: Vec<String>,
    end: End,
    total: usize,
}

/// The end of a step's lines an excerpt holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum End {
    /// The first, as a tool prints a failure's name and its first error first.
    #[default]
    First,
    /// The last, of a program whose lines hold none of its error texts: the end of its output
    /// says how it ended.
    Last,
}

impl Excerpt {
    /// The lines it holds: a lint's error is one line, and a test's failure four at most.
    pub(crate) const LINES: usize = 20;

    /// The last of a step's `total` lines, `last`.
    pub(crate) fn last(last: impl IntoIterator<Item = String>, total: usize) -> Self {
        let lines: Vec<String> = last.into_iter().collect();
        debug_assert!(lines.len() <= Self::LINES.min(total));
        Self {
            lines,
            end: End::Last,
            total,
        }
    }

    /// Counts `line`, kept while it holds fewer than [`Self::LINES`].
    pub(crate) fn push(&mut self, line: &str) {
        debug_assert_eq!(self.end, End::First, "a step's last lines come whole");
        if self.lines.len() < Self::LINES {
            self.lines.push(line.to_owned());
        }
        self.total += 1;
    }

    /// Adds `rest` after its lines, as many as it has room for, and counts them all; `rest` whole
    /// where it holds none.
    pub(crate) fn append(&mut self, rest: Self) {
        if self.total == 0 {
            *self = rest;
            return;
        }
        debug_assert_eq!((self.end, rest.end), (End::First, End::First));
        let room = Self::LINES - self.lines.len();
        self.lines.extend(rest.lines.into_iter().take(room));
        self.total += rest.total;
    }

    /// The line that says why the step failed: its first, or the last of its last lines.
    pub(crate) fn cause(&self) -> Option<&str> {
        let line = match self.end {
            End::First => self.lines.first(),
            End::Last => self.lines.last(),
        };
        line.map(|line| line.trim_start())
    }

    /// Its lines as the digest prints them, indented, with the count of those left out on the
    /// side they were left out.
    pub(crate) fn digest(&self, out: &mut Vec<String>) {
        let left = self.total - self.lines.len();
        if left > 0 && self.end == End::Last {
            out.push(format!("  … {left} before"));
        }
        out.extend(self.lines.iter().map(|l| format!("  {}", l.trim_start())));
        if left > 0 && self.end == End::First {
            out.push(format!("  … {left} more"));
        }
    }
}

/// Picks a step's excerpt from its own lines as they come: the first, or for a step with error
/// texts, the lines that hold one, else its last.
#[derive(Debug)]
pub(crate) struct Picker<'e> {
    errors: &'e [String],
    picked: Excerpt,
    /// The last lines, which a step with error texts shows where none holds one.
    last: VecDeque<String>,
    /// How many lines it had, those of `errors` or not.
    seen: usize,
}

impl<'e> Picker<'e> {
    pub(crate) const fn new(errors: &'e [String]) -> Self {
        Self {
            errors,
            picked: Excerpt {
                lines: Vec::new(),
                end: End::First,
                total: 0,
            },
            last: VecDeque::new(),
            seen: 0,
        }
    }

    pub(crate) fn push(&mut self, line: &str) {
        if self.errors.is_empty() {
            self.picked.push(line);
            return;
        }
        if self.last.len() == Excerpt::LINES {
            self.last.pop_front();
        }
        self.last.push_back(line.to_owned());
        self.seen += 1;
        if self
            .errors
            .iter()
            .any(|error| line.contains(error.as_str()))
        {
            self.picked.push(line);
        }
    }

    pub(crate) fn finish(self) -> Excerpt {
        if self.errors.is_empty() || self.picked.total > 0 || self.seen == 0 {
            return self.picked;
        }
        Excerpt::last(self.last, self.seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Excerpt {
        /// The first of `total` lines, `lines`.
        pub(crate) fn first(lines: &[&str], total: usize) -> Self {
            debug_assert!(lines.len() <= Self::LINES.min(total));
            Self {
                lines: lines.iter().map(|&line| line.to_owned()).collect(),
                end: End::First,
                total,
            }
        }

        pub(crate) fn lines(&self) -> &[String] {
            &self.lines
        }

        pub(crate) const fn end(&self) -> End {
            self.end
        }

        pub(crate) const fn total(&self) -> usize {
            self.total
        }
    }

    #[test]
    fn keeps_one_end_and_counts_what_it_leaves_out_on_that_side() {
        let mut first = Excerpt::default();
        for i in 0..=Excerpt::LINES {
            first.push(&format!("    line {i}"));
        }
        assert_eq!((first.lines().len(), first.total()), (Excerpt::LINES, 21));
        assert_eq!(first.cause(), Some("line 0"));
        let mut out = Vec::new();
        first.digest(&mut out);
        assert_eq!(out[0], "  line 0");
        assert_eq!(out.last().unwrap(), "  … 1 more");

        let last = Excerpt::last(["INFO a play ran".to_owned(), "it stopped".to_owned()], 9);
        assert_eq!((last.end(), last.cause()), (End::Last, Some("it stopped")));
        out.clear();
        last.digest(&mut out);
        assert_eq!(out, ["  … 7 before", "  INFO a play ran", "  it stopped"]);

        assert_eq!(Excerpt::default().cause(), None);
        out.clear();
        Excerpt::default().digest(&mut out);
        assert!(out.is_empty());

        // An excerpt appended after another fills its room and counts every line.
        let mut failures = Excerpt::default();
        failures.push("FAIL a");
        failures.append(first.clone());
        assert_eq!(
            (failures.lines().len(), failures.total()),
            (Excerpt::LINES, 22)
        );
        assert_eq!(failures.lines()[..2], ["FAIL a", "    line 0"]);
        let mut empty = Excerpt::default();
        empty.append(last.clone());
        assert_eq!(empty, last);
    }

    #[test]
    fn a_picker_keeps_the_lines_of_its_error_texts_or_its_last() {
        let errors = ["ERROR".to_owned()];
        let pick = |errors: &[String], lines: &[&str]| {
            let mut picker = Picker::new(errors);
            for line in lines {
                picker.push(line);
            }
            picker.finish()
        };
        let run = ["INFO a", "ERROR b", "INFO c"];
        assert_eq!(pick(&errors, &run), Excerpt::first(&["ERROR b"], 1));
        assert_eq!(pick(&[], &run), Excerpt::first(&run, 3));
        let quiet = ["INFO a", "INFO c"];
        assert_eq!(
            pick(&errors, &quiet),
            Excerpt::last(quiet.map(str::to_owned), 2)
        );
        assert_eq!(pick(&errors, &[]), Excerpt::default());
    }
}
