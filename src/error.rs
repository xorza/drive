use std::fmt;
use std::io;
use std::path::PathBuf;

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
}

/// What is wrong with a root's `drive` table.
#[derive(Debug)]
pub(crate) enum ConfigError {
    Shape(serde_json::Error),
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
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape(source) => write!(f, "drive table: {source}"),
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
        }
    }
}
