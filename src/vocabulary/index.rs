use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write;

/// A list of the index, by what its entries name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexSection {
    /// Structs, enums, unions, traits and type aliases, an associated type among them.
    Types,
    /// Functions and methods but tests, and `macro_rules!` macros.
    Functions,
    Fields,
    Variants,
    /// Constants and statics, an associated constant among them.
    Values,
}

impl IndexSection {
    const ALL: [Self; 5] = [
        Self::Types,
        Self::Functions,
        Self::Fields,
        Self::Variants,
        Self::Values,
    ];

    const fn title(self) -> &'static str {
        match self {
            Self::Types => "Types",
            Self::Functions => "Functions",
            Self::Fields => "Fields",
            Self::Variants => "Variants",
            Self::Values => "Values",
        }
    }
}

/// A name a Rust file declares.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NameListing {
    pub(crate) name: String,
    /// What follows the name: `: ` and the type of a field or a value, else nothing.
    pub(crate) detail: String,
    /// What declares it: its kind for an item of a module, else the line of the item it is in,
    /// as `impl Display for Unit`.
    pub(crate) owner: String,
    /// Its file's path, then `::` and each inline module it is in.
    pub(crate) location: String,
}

impl NameListing {
    /// The index's order: by name with case aside, then by name, owner, location and detail.
    fn order(&self, other: &Self) -> Ordering {
        let lower = self.name.chars().flat_map(char::to_lowercase);
        lower
            .cmp(other.name.chars().flat_map(char::to_lowercase))
            .then_with(|| {
                (&self.name, &self.owner, &self.location, &self.detail).cmp(&(
                    &other.name,
                    &other.owner,
                    &other.location,
                    &other.detail,
                ))
            })
    }
}

/// The names the Rust files declare, by section, and how many names of the index and of the
/// layout hold each word.
#[derive(Debug, Default)]
pub(crate) struct Index {
    sections: [Vec<NameListing>; IndexSection::ALL.len()],
    words: BTreeMap<String, u32>,
}

impl Index {
    pub(crate) fn add(&mut self, section: IndexSection, entry: NameListing) {
        self.add_words(&entry.name);
        self.sections[section as usize].push(entry);
    }

    /// Counts the words of `name`, an identifier or a file's name without its extension, each
    /// once however often the name holds it.
    pub(crate) fn add_words(&mut self, name: &str) {
        let mut words = Vec::new();
        split_words(name.strip_prefix("r#").unwrap_or(name), &mut words);
        words.sort_unstable();
        words.dedup();
        for word in words {
            *self.words.entry(word).or_default() += 1;
        }
    }

    /// Writes each section, its entries by name, case aside, then each word with its count.
    pub(crate) fn write(&self, text: &mut String) {
        for section in IndexSection::ALL {
            let entries = &self.sections[section as usize];
            writeln!(text, "\n== {} ({}) ==\n", section.title(), entries.len())
                .expect("a String takes every write");
            let mut sorted: Vec<&NameListing> = entries.iter().collect();
            sorted.sort_by(|a, b| a.order(b));
            for entry in sorted {
                writeln!(
                    text,
                    "{}{}  {}  {}",
                    entry.name, entry.detail, entry.owner, entry.location
                )
                .expect("a String takes every write");
            }
        }
        writeln!(text, "\n== Words ({}) ==\n", self.words.len())
            .expect("a String takes every write");
        for (word, count) in &self.words {
            writeln!(text, "{word}  {count}").expect("a String takes every write");
        }
    }
}

/// The lowercase words of `name`: its parts between characters that are no letter or digit,
/// each split again before an uppercase letter that follows a lowercase one or a digit, and
/// before the last of a run of uppercase letters that a lowercase one follows, as `HTTPServer`
/// is `http` and `server`; a part of digits alone is no word.
fn split_words(name: &str, words: &mut Vec<String>) {
    for part in name.split(|c: char| !c.is_alphanumeric()) {
        let chars: Vec<char> = part.chars().collect();
        let mut start = 0;
        for at in 1..chars.len() {
            let (before, here) = (chars[at - 1], chars[at]);
            let after_lower =
                here.is_uppercase() && (before.is_lowercase() || before.is_ascii_digit());
            let acronym_ends = here.is_uppercase()
                && before.is_uppercase()
                && chars.get(at + 1).is_some_and(|next| next.is_lowercase());
            if after_lower || acronym_ends {
                push_word(&chars[start..at], words);
                start = at;
            }
        }
        push_word(&chars[start..], words);
    }
}

fn push_word(chars: &[char], words: &mut Vec<String>) {
    if !chars.iter().all(char::is_ascii_digit) {
        words.push(chars.iter().collect::<String>().to_lowercase());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(name: &str) -> Vec<String> {
        let mut words = Vec::new();
        split_words(name, &mut words);
        words
    }

    #[test]
    fn a_name_splits_into_its_lowercase_words() {
        assert_eq!(words("unit_kind"), ["unit", "kind"]);
        assert_eq!(words("UnitKind"), ["unit", "kind"]);
        assert_eq!(words("MAX_LEN"), ["max", "len"]);
        assert_eq!(words("HTTPServer"), ["http", "server"]);
        assert_eq!(words("Vec2D"), ["vec2", "d"]);
        assert_eq!(words("u8"), ["u8"]);
        assert_eq!(words("02-engine-core"), ["engine", "core"]);
        assert_eq!(words("__private"), ["private"]);
        assert_eq!(words(""), Vec::<String>::new());
    }

    #[test]
    fn the_index_lists_each_section_by_name_and_counts_each_word() {
        let entry = |name: &str, detail: &str, owner: &str| NameListing {
            name: name.to_owned(),
            detail: detail.to_owned(),
            owner: owner.to_owned(),
            location: "a.rs".to_owned(),
        };
        let mut index = Index::default();
        index.add(
            IndexSection::Fields,
            entry("unit_id", ": UnitId", "struct Order"),
        );
        index.add(
            IndexSection::Fields,
            entry("id", ": PlayerId", "struct Player"),
        );
        index.add(IndexSection::Types, entry("unit", "", "struct"));
        index.add(IndexSection::Types, entry("Order", "", "struct"));
        index.add(IndexSection::Functions, entry("r#type", "", "impl Unit"));
        index.add_words("unit_kind");
        // A word a name holds twice counts once.
        index.add_words("UnitOfUnits");
        let mut text = String::new();
        index.write(&mut text);
        // `Order` goes before `unit` with case aside; `r#type` counts the word `type`.
        let expected = concat!(
            "\n== Types (2) ==\n\n",
            "Order  struct  a.rs\n",
            "unit  struct  a.rs\n",
            "\n== Functions (1) ==\n\n",
            "r#type  impl Unit  a.rs\n",
            "\n== Fields (2) ==\n\n",
            "id: PlayerId  struct Player  a.rs\n",
            "unit_id: UnitId  struct Order  a.rs\n",
            "\n== Variants (0) ==\n\n",
            "\n== Values (0) ==\n\n",
            "\n== Words (7) ==\n\n",
            "id  2\n",
            "kind  1\n",
            "of  1\n",
            "order  1\n",
            "type  1\n",
            "unit  4\n",
            "units  1\n",
        );
        assert_eq!(text, expected);
    }
}
