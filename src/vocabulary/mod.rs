//! The vocabulary of a project, for a review of its names: the layout of each repository that
//! holds a root, each Rust file's items under it with their fields, variants and methods, then an
//! index of every name those files declare by kind, and the count of each word of those names, in
//! one text file whose order is fixed, so two runs compare by a diff.

mod index;
mod outline;
mod repo_file;
mod token_text;

use std::ffi::OsStr;
use std::fmt::{self, Write};
use std::path::Path;
use std::str::Utf8Error;

use crate::error::{Error, Result};
use crate::project::Project;
use crate::repo::Repo;
use crate::vocabulary::index::Index;
use crate::vocabulary::outline::Outline;
use crate::vocabulary::repo_file::{FileKind, RepoFile};

/// What the text says it holds, at its head.
const HEADER: &str = "\
The vocabulary of the project, written by `cargo drive vocabulary`.

Layout: each repository that holds a root, the top root's first, then each file git tracks or
would track, ignored ones left out, a nested repository as one line; under each Rust file its
items, and under each item what it holds, without `use` items, `mod` declarations, items in
function bodies, and `where` clauses.
Index: each name the Rust files declare, by section and by name, case aside, then what declares
it and where, its path from the first repository's folder; a test function and the items of a
trait impl, which the trait names, are in the layout alone.
Words: each lowercase word of the index's names and of the layouts' folders and files, with how
many names hold it.
";

/// The layout of each repository, each Rust file's items in it, and the index of their names.
#[derive(Debug, Default)]
pub(crate) struct Vocabulary {
    layouts: Vec<Layout>,
    index: Index,
    dirs: usize,
    files: usize,
    rust_files: usize,
    unparsed: Vec<Unparsed>,
}

/// The layout of one repository.
#[derive(Debug)]
struct Layout {
    /// Its folder relative to the first repository's, empty for the first.
    prefix: String,
    text: String,
}

/// A Rust file that does not parse, which the layout marks.
#[derive(Debug)]
pub(crate) struct Unparsed {
    /// Its path from the first repository's folder.
    path: String,
    why: Unparsable,
}

/// Why a Rust file does not parse.
#[derive(Debug)]
enum Unparsable {
    /// Rust source is UTF-8.
    NotUtf8(Utf8Error),
    Syntax(syn::Error),
}

impl fmt::Display for Unparsable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUtf8(error) => write!(f, "not UTF-8: {error}"),
            Self::Syntax(error) => write!(f, "{error}"),
        }
    }
}

impl fmt::Display for Unparsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.why)
    }
}

impl Vocabulary {
    /// The vocabulary of each repository that holds a root of `project`, in the roots' order.
    pub(crate) fn of(project: &Project) -> Result<Self> {
        let mut repos: Vec<&Repo> = Vec::new();
        for root in project.roots() {
            let repo = root.repo.as_ref().ok_or_else(|| Error::NoRepository {
                dir: root.dir.clone(),
            })?;
            if !repos.iter().any(|known| known.dir == repo.dir) {
                repos.push(repo);
            }
        }
        let first = &repos[0].dir;
        let mut vocabulary = Self::default();
        for repo in repos {
            let files = RepoFile::list(repo)?;
            vocabulary.add(&prefix(first, &repo.dir), &files, |path| {
                let path = repo.dir.join(path);
                std::fs::read(&path).map_err(|source| Error::Io { path, source })
            })?;
        }
        Ok(vocabulary)
    }

    /// Adds the repository at `prefix`, whose `files` go by their paths' names in order, each Rust
    /// file's bytes given by `read` from its path.
    fn add(
        &mut self,
        prefix: &str,
        files: &[RepoFile],
        mut read: impl FnMut(&str) -> Result<Vec<u8>>,
    ) -> Result<()> {
        debug_assert!(
            files.is_sorted_by(|a, b| a.path.split('/').lt(b.path.split('/'))),
            "each folder's files come together, and each file once"
        );
        let mut text = String::new();
        let mut open: Vec<&str> = Vec::new();
        for file in files {
            let mut dirs: Vec<&str> = file.path.split('/').collect();
            let name = dirs.pop().expect("a path has a name");
            let shared = open.iter().zip(&dirs).take_while(|(a, b)| a == b).count();
            open.truncate(shared);
            for &dir in &dirs[shared..] {
                outline::line(&mut text, open.len(), format_args!("{dir}/"));
                self.index.add_words(dir);
                self.dirs += 1;
                open.push(dir);
            }
            let depth = dirs.len();
            match file.kind {
                FileKind::Repository => {
                    outline::line(
                        &mut text,
                        depth,
                        format_args!("{name}/ (a repository of its own)"),
                    );
                    self.index.add_words(name);
                    self.dirs += 1;
                }
                FileKind::Link => {
                    outline::line(&mut text, depth, format_args!("{name} (a link)"));
                    self.index.add_words(stem(name));
                    self.files += 1;
                }
                FileKind::File => {
                    outline::line(&mut text, depth, format_args!("{name}"));
                    self.index.add_words(stem(name));
                    self.files += 1;
                    if Path::new(name).extension().is_some_and(|ext| ext == "rs") {
                        let source = read(&file.path)?;
                        let path = if prefix.is_empty() {
                            file.path.clone()
                        } else {
                            format!("{prefix}/{}", file.path)
                        };
                        self.outline(&mut text, path, &source, depth + 1);
                    }
                }
            }
        }
        self.layouts.push(Layout {
            prefix: prefix.to_owned(),
            text,
        });
        Ok(())
    }

    pub(crate) fn unparsed(&self) -> &[Unparsed] {
        &self.unparsed
    }

    pub(crate) fn text(&self) -> String {
        let mut text = String::from(HEADER);
        let repositories = match self.layouts.len() {
            1 => "1 repository".to_owned(),
            n => format!("{n} repositories"),
        };
        writeln!(
            text,
            "\n{repositories}, {} folders, {} files, {} Rust files, {} that do not parse",
            self.dirs,
            self.files,
            self.rust_files,
            self.unparsed.len()
        )
        .expect("a String takes every write");
        for layout in &self.layouts {
            let prefix = if layout.prefix.is_empty() {
                "."
            } else {
                &layout.prefix
            };
            writeln!(text, "\n== Layout of {prefix} ==\n").expect("a String takes every write");
            text.push_str(&layout.text);
        }
        self.index.write(&mut text);
        text
    }

    /// Writes the text to `out` aside and moves it in whole, so a run beside this one leaves the
    /// file whole.
    pub(crate) fn write(&self, out: &Path) -> Result<()> {
        let name = out.file_name().unwrap_or_default().to_string_lossy();
        let aside = out.with_file_name(format!("{name}-{}.tmp", std::process::id()));
        std::fs::write(&aside, self.text()).map_err(|source| Error::Io {
            path: aside.clone(),
            source,
        })?;
        std::fs::rename(&aside, out).map_err(|source| {
            std::fs::remove_file(&aside).ok();
            Error::Io {
                path: out.to_owned(),
                source,
            }
        })
    }

    /// The layout's lines and the index's entries of the Rust file at `path`, which holds
    /// `source`, its items at `depth`.
    fn outline(&mut self, text: &mut String, path: String, source: &[u8], depth: usize) {
        self.rust_files += 1;
        let parsed = str::from_utf8(source)
            .map_err(Unparsable::NotUtf8)
            .and_then(|source| syn::parse_file(source).map_err(Unparsable::Syntax));
        match parsed {
            Ok(file) => Outline::new(text, &mut self.index, path).items(&file.items, depth),
            Err(why) => {
                outline::line(text, depth, format_args!("(does not parse: {why})"));
                self.unparsed.push(Unparsed { path, why });
            }
        }
    }
}

/// The folder `dir` relative to `first`, its names joined by `/`: empty for `first` itself, and
/// the whole path for one outside it.
fn prefix(first: &Path, dir: &Path) -> String {
    let Ok(relative) = dir.strip_prefix(first) else {
        return dir.display().to_string();
    };
    let names: Vec<_> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    names.join("/")
}

/// The name of the file `name` without its extension.
fn stem(name: &str) -> &str {
    Path::new(name)
        .file_stem()
        .and_then(OsStr::to_str)
        .expect("a UTF-8 file name has a UTF-8 stem")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_layout_nests_each_file_in_its_folders_and_each_item_in_its_file() {
        let top = [
            RepoFile::fixture("Cargo.toml", FileKind::File),
            RepoFile::fixture("docs/b.md", FileKind::File),
            RepoFile::fixture("docs/design/a.md", FileKind::File),
            RepoFile::fixture("source/bad.rs", FileKind::File),
            RepoFile::fixture("source/gui", FileKind::Repository),
            RepoFile::fixture("source/latin1.rs", FileKind::File),
            RepoFile::fixture("source/link.rs", FileKind::Link),
            RepoFile::fixture("source/src/lib.rs", FileKind::File),
        ];
        let gui = [RepoFile::fixture("src/lib.rs", FileKind::File)];
        let mut read = Vec::new();
        let mut vocabulary = Vocabulary::default();
        let mut reader = |path: &str| {
            read.push(path.to_owned());
            Ok(match path {
                "source/bad.rs" => b"fn 1() {}".to_vec(),
                "source/latin1.rs" => b"// caf\xe9 au lait".to_vec(),
                _ => b"pub struct UnitKind { id: u8 }".to_vec(),
            })
        };
        vocabulary.add("", &top, &mut reader).unwrap();
        vocabulary.add("source/gui", &gui, &mut reader).unwrap();
        assert_eq!(
            read,
            [
                "source/bad.rs",
                "source/latin1.rs",
                "source/src/lib.rs",
                "src/lib.rs"
            ]
        );
        let unparsed: Vec<String> = (vocabulary.unparsed().iter())
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            unparsed,
            [
                "source/bad.rs: expected identifier",
                "source/latin1.rs: not UTF-8: invalid utf-8 sequence of 1 bytes from index 6"
            ]
        );
        let text = vocabulary.text();
        let layouts = text
            .split_once("\n== Layout of . ==\n\n")
            .and_then(|(_, rest)| rest.split_once("\n== Types"))
            .map(|(layouts, _)| layouts)
            .unwrap();
        let expected = concat!(
            "Cargo.toml\n",
            "docs/\n",
            "  b.md\n",
            "  design/\n",
            "    a.md\n",
            "source/\n",
            "  bad.rs\n",
            "    (does not parse: expected identifier)\n",
            "  gui/ (a repository of its own)\n",
            "  latin1.rs\n",
            "    (does not parse: not UTF-8: invalid utf-8 sequence of 1 bytes from index 6)\n",
            "  link.rs (a link)\n",
            "  src/\n",
            "    lib.rs\n",
            "      pub struct UnitKind\n",
            "        id: u8\n",
            "\n== Layout of source/gui ==\n\n",
            "src/\n",
            "  lib.rs\n",
            "    pub struct UnitKind\n",
            "      id: u8\n",
        );
        assert_eq!(layouts, expected);
        // Four folders and a repository in the first, `src` in the second; eight files, a link
        // and four Rust files among them.
        assert!(
            text.contains(
                "\n2 repositories, 6 folders, 8 files, 4 Rust files, 2 that do not parse\n"
            )
        );
        // An entry's location starts at the first repository's folder.
        assert!(text.contains("\nUnitKind  struct  source/gui/src/lib.rs\n"));
        assert!(text.contains("\nUnitKind  struct  source/src/lib.rs\n"));
        // The words of the layouts' names count with the index's.
        assert!(text.contains("\nsource  1\n"));
        assert!(text.contains("\nsrc  2\n"));
        assert!(text.contains("\nunit  2\n"));
        assert!(text.contains("\nid  2\n"));
    }

    #[test]
    fn a_prefix_is_the_path_from_the_first_repository() {
        let first = Path::new("/w");
        assert_eq!(prefix(first, first), "");
        assert_eq!(prefix(first, Path::new("/w/gui/theme")), "gui/theme");
        assert_eq!(prefix(first, Path::new("/v/lib")), "/v/lib");
    }

    #[test]
    fn the_text_goes_to_its_file_whole() {
        let dir = std::env::temp_dir().join(format!("drive-vocabulary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("vocabulary.txt");
        std::fs::write(&out, "old").unwrap();
        let mut one = Vocabulary::default();
        one.add("", &[], |_| unreachable!("no file to read"))
            .unwrap();
        assert!(
            one.text()
                .contains("\n1 repository, 0 folders, 0 files, 0 Rust files")
        );
        let vocabulary = Vocabulary::default();
        vocabulary.write(&out).unwrap();
        let written = std::fs::read_to_string(&out).unwrap();
        let left = std::fs::read_dir(&dir).unwrap().count();
        let missing = vocabulary.write(&dir.join("none/vocabulary.txt"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(written, vocabulary.text());
        assert!(written.contains("\n0 repositories, 0 folders, 0 files, 0 Rust files"));
        assert_eq!(left, 1);
        assert!(matches!(missing, Err(Error::Io { .. })));
    }
}
