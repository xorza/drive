//! The driver's version, and the one a root's `drive.version` requires.

use std::fmt;
use std::hash::{DefaultHasher, Hasher};

/// A release, `MAJOR.MINOR.PATCH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

/// `drive.version`: `MAJOR[.MINOR[.PATCH]]` with cargo's caret meaning, so `0.2` takes 0.2.0 up
/// to 0.3.0, and a driver that changes how a table reads is a new minor before 1.0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Requirement {
    floor: Version,
    /// The parts written: `0.0` takes 0.0.x, `0.0.3` takes 0.0.3 alone.
    parts: usize,
}

const _: () = assert!(
    env!("CARGO_PKG_VERSION_PRE").is_empty(),
    "a requirement takes no pre-release"
);

impl Version {
    pub(crate) const DRIVER: Self = Self {
        major: digits(env!("CARGO_PKG_VERSION_MAJOR")),
        minor: digits(env!("CARGO_PKG_VERSION_MINOR")),
        patch: digits(env!("CARGO_PKG_VERSION_PATCH")),
    };

    const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl Requirement {
    /// `None` for text that is not one to three dot-separated decimal numbers without leading
    /// zeros.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut parts = [0; 3];
        let mut count = 0;
        for part in text.split('.') {
            let slot = parts.get_mut(count)?;
            let digits = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
            if !digits || (part.len() > 1 && part.starts_with('0')) {
                return None;
            }
            *slot = part.parse().ok()?;
            count += 1;
        }
        let [major, minor, patch] = parts;
        Some(Self {
            floor: Version {
                major,
                minor,
                patch,
            },
            parts: count,
        })
    }

    /// The requirement the driver's own version meets first: `0.2` for 0.2.0, `1.4` for 1.4.0.
    pub(crate) const fn of(version: Version) -> Self {
        Self {
            floor: Version {
                patch: 0,
                ..version
            },
            parts: 2,
        }
    }

    pub(crate) fn matches(&self, version: Version) -> bool {
        let Version {
            major,
            minor,
            patch,
        } = self.floor;
        // `None` past the largest part, where every version from the floor up matches.
        let ceiling = if major > 0 || self.parts == 1 {
            major.checked_add(1).map(|major| Version::new(major, 0, 0))
        } else if minor > 0 || self.parts == 2 {
            minor.checked_add(1).map(|minor| Version::new(0, minor, 0))
        } else {
            patch.checked_add(1).map(|patch| Version::new(0, 0, patch))
        };
        self.floor <= version && ceiling.is_none_or(|ceiling| version < ceiling)
    }

    /// Whether every version this one takes is newer than `version`.
    pub(crate) fn wants_newer(&self, version: Version) -> bool {
        version < self.floor
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Version {
            major,
            minor,
            patch,
        } = self.floor;
        match self.parts {
            1 => write!(f, "{major}"),
            2 => write!(f, "{major}.{minor}"),
            _ => write!(f, "{major}.{minor}.{patch}"),
        }
    }
}

/// The running driver: its version and a hash of its executable, which a rebuild of the same
/// version changes too, so no pass another driver kept is reused.
pub(crate) fn identity() -> String {
    let hash = std::env::current_exe()
        .and_then(std::fs::read)
        .map(|bytes| {
            let mut hasher = DefaultHasher::new();
            hasher.write(&bytes);
            format!("{:016x}", hasher.finish())
        });
    match hash {
        Ok(hash) => format!("{} {hash}", Version::DRIVER),
        // No executable to read: the version alone, which a rebuild of it does not change.
        Err(_) => Version::DRIVER.to_string(),
    }
}

const fn digits(text: &str) -> u64 {
    let bytes = text.as_bytes();
    assert!(!bytes.is_empty(), "a version part has digits");
    let mut value = 0;
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i].is_ascii_digit(), "a version part is decimal");
        value = value * 10 + (bytes[i] - b'0') as u64;
        i += 1;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_requirement_takes_what_cargo_s_caret_takes() {
        // (requirement, the lowest version it takes, the lowest above it it refuses)
        let cases = [
            ("1.2.3", (1, 2, 3), (2, 0, 0)),
            ("1.2", (1, 2, 0), (2, 0, 0)),
            ("1", (1, 0, 0), (2, 0, 0)),
            ("0.2.3", (0, 2, 3), (0, 3, 0)),
            ("0.2", (0, 2, 0), (0, 3, 0)),
            ("0.0.3", (0, 0, 3), (0, 0, 4)),
            ("0.0", (0, 0, 0), (0, 1, 0)),
            ("0", (0, 0, 0), (1, 0, 0)),
        ];
        for (text, (a, b, c), (x, y, z)) in cases {
            let req = Requirement::parse(text).unwrap();
            assert_eq!(req.to_string(), text);
            let lowest = Version::new(a, b, c);
            assert!(req.matches(lowest), "{text} takes {lowest}");
            assert!(!req.wants_newer(lowest), "{text}");
            let refused = Version::new(x, y, z);
            assert!(!req.matches(refused), "{text} refuses {refused}");
            assert!(!req.wants_newer(refused), "{text}");
            if c > 0 {
                let below = Version::new(a, b, c - 1);
                assert!(
                    !req.matches(below) && req.wants_newer(below),
                    "{text} refuses {below}"
                );
            }
        }
        // Just under the ceiling still matches.
        assert!(
            Requirement::parse("0.2")
                .unwrap()
                .matches(Version::new(0, 2, 99))
        );
        assert!(
            Requirement::parse("1.2")
                .unwrap()
                .matches(Version::new(1, 99, 0))
        );
        assert!(
            !Requirement::parse("0.2")
                .unwrap()
                .matches(Version::new(0, 1, 9))
        );
        // The largest major has no ceiling.
        let top = Requirement::parse(&u64::MAX.to_string()).unwrap();
        assert!(top.matches(Version::new(u64::MAX, u64::MAX, u64::MAX)));
        assert!(!top.matches(Version::new(u64::MAX - 1, 0, 0)));
    }

    #[test]
    fn a_requirement_is_one_to_three_plain_numbers() {
        for bad in [
            "", ".", "0.", ".2", "0.2.3.4", "^0.2", "=0.2", "+1", "0.2-rc", "01.2", "0.02", " 0.2",
            "0x1",
        ] {
            assert_eq!(Requirement::parse(bad), None, "{bad:?}");
        }
        assert_eq!(
            Requirement::parse("10.0.20").unwrap().to_string(),
            "10.0.20"
        );
    }

    #[test]
    fn the_driver_meets_its_own_requirement() {
        let own = Requirement::of(Version::DRIVER);
        assert!(own.matches(Version::DRIVER));
        assert_eq!(
            own.to_string(),
            env!("CARGO_PKG_VERSION").rsplit_once('.').unwrap().0
        );
        assert_eq!(Version::DRIVER.to_string(), env!("CARGO_PKG_VERSION"));
        assert_eq!(digits("0"), 0);
        assert_eq!(digits("120"), 120);
    }
}
