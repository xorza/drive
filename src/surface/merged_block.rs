//! [`MergedBlock`]: one item's block as the union of the blocks its builds render.

use std::collections::BTreeSet;

/// Builds, by their place in the surface's build list.
pub(crate) type BuildIdxs = BTreeSet<usize>;

/// One item's lines over every build that exports it, as the tree their indents give: a line
/// owns the deeper lines below it. Siblings two builds share are one line, whose children merge
/// in turn; each build's own siblings keep their order.
#[derive(Debug, Default)]
pub(crate) struct MergedBlock<'a> {
    roots: Vec<MergedLine<'a>>,
}

/// A line of a [`MergedBlock`], the builds whose block has it under the same parent, and the
/// lines it owns.
#[derive(Debug)]
struct MergedLine<'a> {
    text: &'a str,
    builds: BuildIdxs,
    children: Vec<Self>,
}

/// A line of a [`MergedBlock`] as printed, with its builds and those of the line that owns it,
/// or of the item for a line at the top.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ShownLine<'s> {
    pub(crate) text: &'s str,
    pub(crate) builds: &'s BuildIdxs,
    pub(crate) owner: &'s BuildIdxs,
}

impl<'a> MergedBlock<'a> {
    /// The union of `blocks`, each the block of the build at its index, in build order.
    pub(crate) fn from_builds(blocks: impl IntoIterator<Item = (usize, &'a [String])>) -> Self {
        let mut merged = Self::default();
        for (build, block) in blocks {
            let tree = MergedLine::parse(build, &mut block.iter().peekable(), 0);
            MergedLine::merge(&mut merged.roots, tree);
        }
        merged
    }

    /// The lines in order, as printed under an item exported by `item`.
    pub(crate) fn shown<'s>(&'s self, item: &'s BuildIdxs) -> Vec<ShownLine<'s>> {
        let mut shown = Vec::new();
        MergedLine::show(&self.roots, item, &mut shown);
        shown
    }
}

impl<'a> MergedLine<'a> {
    /// The lines of `build` at `indent` or deeper, from the front of `lines`, as a tree.
    fn parse(
        build: usize,
        lines: &mut std::iter::Peekable<std::slice::Iter<'a, String>>,
        indent: usize,
    ) -> Vec<Self> {
        let mut siblings = Vec::new();
        while let Some(text) = lines.next_if(|text| indent_of(text) >= indent) {
            let children = lines
                .peek()
                .map(|next| indent_of(next))
                .filter(|&deeper| deeper > indent_of(text))
                .map(|deeper| Self::parse(build, lines, deeper))
                .unwrap_or_default();
            siblings.push(Self {
                text,
                builds: BTreeSet::from([build]),
                children,
            });
        }
        siblings
    }

    /// Adds `incoming`, the siblings of one build, to `merged`: a longest common subsequence of
    /// the two by text is shared and its children merge, and every other incoming line goes in
    /// after the merged lines that precede it.
    fn merge(merged: &mut Vec<Self>, incoming: Vec<Self>) {
        let (n, m) = (merged.len(), incoming.len());
        let width = m + 1;
        // `common[i * width + j]`: the longest common subsequence of `merged[i..]` and
        // `incoming[j..]`.
        let mut common = vec![0_usize; (n + 1) * width];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                common[i * width + j] = if merged[i].text == incoming[j].text {
                    common[(i + 1) * width + j + 1] + 1
                } else {
                    common[(i + 1) * width + j].max(common[i * width + j + 1])
                };
            }
        }
        let mut out = Vec::with_capacity(n + m);
        let mut lines = std::mem::take(merged).into_iter().peekable();
        let mut incoming = incoming.into_iter().peekable();
        let (mut i, mut j) = (0, 0);
        while let Some(line) = lines.peek() {
            let Some(next) = incoming.peek() else {
                out.extend(lines);
                break;
            };
            if line.text == next.text {
                let (mut line, next) = (
                    lines.next().expect("peeked"),
                    incoming.next().expect("peeked"),
                );
                line.builds.extend(next.builds);
                Self::merge(&mut line.children, next.children);
                out.push(line);
                i += 1;
                j += 1;
            } else if common[(i + 1) * width + j] >= common[i * width + j + 1] {
                out.push(lines.next().expect("peeked"));
                i += 1;
            } else {
                out.push(incoming.next().expect("peeked"));
                j += 1;
            }
        }
        out.extend(incoming);
        *merged = out;
    }

    fn show<'s>(lines: &'s [Self], owner: &'s BuildIdxs, shown: &mut Vec<ShownLine<'s>>) {
        for line in lines {
            shown.push(ShownLine {
                text: line.text,
                builds: &line.builds,
                owner,
            });
            Self::show(&line.children, &line.builds, shown);
        }
    }
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|&line| line.to_owned()).collect()
    }

    fn idxs(builds: &[usize]) -> BuildIdxs {
        builds.iter().copied().collect()
    }

    /// A line's text, and its builds where they differ from its owner's.
    #[derive(Debug, PartialEq, Eq)]
    struct Tagged<'s> {
        text: &'s str,
        builds: Option<&'s BuildIdxs>,
    }

    fn tagged<'s>(merged: &'s MergedBlock<'_>, item: &'s BuildIdxs) -> Vec<Tagged<'s>> {
        merged
            .shown(item)
            .into_iter()
            .map(|line| Tagged {
                text: line.text,
                builds: (line.builds != line.owner).then_some(line.builds),
            })
            .collect()
    }

    const fn shown<'s>(text: &'s str, builds: Option<&'s BuildIdxs>) -> Tagged<'s> {
        Tagged { text, builds }
    }

    #[test]
    fn a_line_under_a_tagged_line_of_the_same_builds_is_not_tagged_again() {
        let bare = block(&["struct W", "    fn new", "    trait Debug"]);
        let seek = block(&[
            "struct W",
            "    fn new",
            "    impl Seek",
            "        fn stream",
            "    trait Debug",
        ]);
        let lost = block(&["struct W", "    trait Debug"]);
        let merged = MergedBlock::from_builds([
            (0, bare.as_slice()),
            (1, seek.as_slice()),
            (2, lost.as_slice()),
        ]);
        let all = idxs(&[0, 1, 2]);
        let (kept, seek_only) = (idxs(&[0, 1]), idxs(&[1]));
        // `fn stream` has the builds of the `impl Seek` that owns it, so only the impl is tagged;
        // `fn new` differs from its item's builds, as build 2 drops it.
        assert_eq!(
            tagged(&merged, &all),
            [
                shown("struct W", None),
                shown("    fn new", Some(&kept)),
                shown("    impl Seek", Some(&seek_only)),
                shown("        fn stream", None),
                shown("    trait Debug", None),
            ]
        );
    }

    #[test]
    fn lines_two_builds_render_differently_are_both_kept_each_over_its_own_members() {
        // The feature bounds the impl differently: its members belong to its own header, so each
        // header keeps its `fn new`, untagged beneath it, rather than one `fn new` under one header.
        let bare = block(&[
            "struct R",
            "    impl<S: Read> R",
            "        fn new",
            "    impl Debug for R",
        ]);
        let feature = block(&[
            "struct R",
            "    impl<S: Read + Seek> R",
            "        fn new",
            "    impl Debug for R",
        ]);
        let merged = MergedBlock::from_builds([(0, bare.as_slice()), (1, feature.as_slice())]);
        let both = idxs(&[0, 1]);
        let (zero, one) = (idxs(&[0]), idxs(&[1]));
        assert_eq!(
            tagged(&merged, &both),
            [
                shown("struct R", None),
                shown("    impl<S: Read> R", Some(&zero)),
                shown("        fn new", None),
                shown("    impl<S: Read + Seek> R", Some(&one)),
                shown("        fn new", None),
                shown("    impl Debug for R", None),
            ]
        );
    }

    #[test]
    fn one_build_merges_into_its_own_lines_untagged() {
        let only = block(&["fn f", "    fn g", "        fn h", "    fn i"]);
        let merged = MergedBlock::from_builds([(3, only.as_slice())]);
        let item = idxs(&[3]);
        assert_eq!(
            tagged(&merged, &item),
            [
                shown("fn f", None),
                shown("    fn g", None),
                shown("        fn h", None),
                shown("    fn i", None),
            ]
        );
        assert!(MergedBlock::from_builds([]).shown(&item).is_empty());
    }
}
