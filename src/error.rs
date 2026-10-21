use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::version::{Requirement, Version};

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

/// A usage or project error: printed, then exit 2.
#[derive(Debug)]
pub(crate) enum Error {
    Spawn {
        program: String,
        source: io::Error,
    },
    Tool {
        command: String,
        dir: PathBuf,
        stderr: String,
    },
    Metadata {
        dir: PathBuf,
        source: serde_json::Error,
    },
    Config {
        manifest: PathBuf,
        reason: ConfigError,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
    NoBenches {
        dir: PathBuf,
    },
    /// A command that formats the checkout, updates its lockfile and stops at the first failure,
    /// run in CI.
    LocalInCi {
        command: &'static str,
    },
    /// `run` of a name the root's `commands` table does not hold.
    UnknownCommand {
        name: String,
        known: Vec<String>,
    },
    /// Arguments to a project command none of whose steps takes them.
    ArgsRefused {
        command: String,
    },
    /// `package` in a root whose table names no binary to ship.
    NothingToPackage {
        dir: PathBuf,
    },
}

/// What is wrong with a root's `drive` table.
#[derive(Debug)]
pub(crate) enum ConfigError {
    Shape(serde_json::Error),
    /// No `version`, or no table to hold one.
    NoVersion {
        running: Version,
    },
    BadVersion {
        text: String,
    },
    /// A `version` the running driver does not meet.
    OtherVersion {
        required: Requirement,
        running: Version,
    },
    /// Both `[workspace.metadata.drive]` and the root package's `[package.metadata.drive]`.
    TwoTables,
    EmptyList {
        key: &'static str,
    },
    FeaturesNeedsValue {
        key: &'static str,
    },
    NotAFlag {
        key: &'static str,
        token: String,
    },
    /// An `all-features-but` entry that names no feature of a member.
    UnknownFeature {
        key: &'static str,
        feature: String,
    },
    /// An `all-features-but` entry that another feature turns on again.
    FeatureComesBack {
        key: &'static str,
        feature: String,
        by: String,
    },
    /// A `doc` set in a table without `docs = true`, which no lane would read.
    DocWithoutDocs,
    UnknownMember {
        key: &'static str,
        member: String,
    },
    /// A path that names no file or folder, which no change would reach.
    MissingPath {
        key: &'static str,
        path: String,
    },
    /// A `cross-lint` key that names no OS the driver knows.
    UnknownHost {
        host: String,
    },
    /// A `package` entry that names no binary of a member.
    UnknownBinary {
        name: String,
    },
    /// A step of a project command that names no kind, or two.
    StepKind {
        command: String,
        step: usize,
    },
    ArgsTwice {
        command: String,
    },
    /// A `tests` step in a root whose tests `cargo test` runs, which takes no filter.
    TestsNeedNextest {
        command: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { program, source } => write!(f, "cannot run `{program}`: {source}"),
            Self::Tool {
                command,
                dir,
                stderr,
            } => {
                write!(f, "{command} in {}:\n{}", dir.display(), stderr.trim())
            }
            Self::Metadata { dir, source } => {
                write!(f, "cargo metadata in {}: {source}", dir.display())
            }
            Self::Config { manifest, reason } => write!(f, "{}: {reason}", manifest.display()),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::NoBenches { dir } => write!(f, "no member of {} has a bench", dir.display()),
            Self::LocalInCi { command } => write!(
                f,
                "`cargo drive {command}` formats the checkout, updates its lockfile and stops at \
                 the first failure; CI runs `cargo drive gate --ci`"
            ),
            Self::UnknownCommand { name, known } if known.is_empty() => {
                write!(
                    f,
                    "no command `{name}`: the root's drive table holds no command"
                )
            }
            Self::UnknownCommand { name, known } => {
                write!(
                    f,
                    "no command `{name}`; the root's commands: {}",
                    known.join(", ")
                )
            }
            Self::ArgsRefused { command } => {
                write!(f, "no step of `{command}` takes arguments")
            }
            Self::NothingToPackage { dir } => write!(
                f,
                "the drive table of {} names no binary to package",
                dir.display()
            ),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape(source) => write!(f, "drive table: {source}"),
            Self::NoVersion { running } => write!(
                f,
                "drive.version: missing; name the driver the root needs, as `version = \"{}\"` \
                 for this one, {running}",
                Requirement::of(*running)
            ),
            Self::BadVersion { text } => {
                write!(f, "drive.version: `{text}` is not MAJOR[.MINOR[.PATCH]]")
            }
            Self::OtherVersion { required, running } if required.wants_newer(*running) => write!(
                f,
                "drive.version: the root needs drive {required}, and this one is {running}; \
                 update it: cargo install --locked --git {}",
                env!("CARGO_PKG_REPOSITORY")
            ),
            Self::OtherVersion { required, running } => write!(
                f,
                "drive.version: the root needs drive {required}, and this one is {running}; \
                 move the root to `version = \"{}\"`",
                Requirement::of(*running)
            ),
            Self::TwoTables => f.write_str(
                "both [workspace.metadata.drive] and [package.metadata.drive]; keep one",
            ),
            Self::EmptyList { key } => write!(f, "drive.{key}: must be a non-empty list"),
            Self::FeaturesNeedsValue { key } => {
                write!(f, "drive.{key}: `--features` needs a value")
            }
            Self::NotAFlag { key, token } => {
                write!(f, "drive.{key}: `{token}` is not a feature or target flag")
            }
            Self::UnknownFeature { key, feature } => {
                write!(f, "drive.{key}: `{feature}` names no feature of a member")
            }
            Self::FeatureComesBack { key, feature, by } => {
                write!(f, "drive.{key}: `{by}` turns `{feature}` on again")
            }
            Self::DocWithoutDocs => f.write_str("drive.doc: rustdoc runs only with `docs = true`"),
            Self::UnknownMember { key, member } => {
                write!(f, "drive.{key}: `{member}` names no member")
            }
            Self::MissingPath { key, path } => {
                write!(f, "drive.{key}: `{path}` names no file or folder")
            }
            Self::UnknownHost { host } => write!(
                f,
                "drive.cross-lint: `{host}` is not `linux`, `macos` or `windows`"
            ),
            Self::UnknownBinary { name } => {
                write!(f, "drive.package: `{name}` names no binary of a member")
            }
            Self::StepKind { command, step } => write!(
                f,
                "drive.commands.{command}: step {step} names not one of `build`, `cargo`, \
                 `program` and `tests`"
            ),
            Self::ArgsTwice { command } => {
                write!(
                    f,
                    "drive.commands.{command}: two steps take the command's arguments"
                )
            }
            Self::TestsNeedNextest { command } => write!(
                f,
                "drive.commands.{command}: a `tests` step takes a nextest filter, and the root \
                 runs `cargo test`"
            ),
        }
    }
}
