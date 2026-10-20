//! Running the tools the driver wraps.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// The cargo that runs this binary, which `cargo drive` names in `CARGO`.
pub(crate) fn cargo() -> Command {
    command(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
}

/// A process of `program`, by its path or its name on `PATH`, without the variables cargo set for
/// the driver alone.
#[expect(
    clippy::disallowed_methods,
    reason = "the one place the driver makes a process"
)]
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os().filter(|(name, _)| is_drivers_own(name)) {
        command.env_remove(name);
    }
    command
}

/// Whether cargo set the variable for the driver: `CARGO`, as for each subcommand, and under
/// `cargo run` the variables of the driver's own package. A build the driver starts that saw one
/// would differ from one started outside it, as a build script that runs again when
/// `CARGO_MANIFEST_DIR` changes, as `ring`'s does, and each crate above it, would build again
/// after each build outside the driver.
pub(crate) fn is_drivers_own(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        matches!(name, "CARGO" | "CARGO_MANIFEST_DIR" | "CARGO_MANIFEST_PATH")
            || name.starts_with("CARGO_PKG_")
    })
}

/// Stdout of a command run in `dir`; a failure carries its stderr.
pub(crate) fn stdout(command: &mut Command, dir: &Path) -> Result<String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let out = command
        .current_dir(dir)
        .output()
        .map_err(|source| Error::Spawn { program, source })?;
    if !out.status.success() {
        let subcommand = command.get_args().next().unwrap_or_default();
        return Err(Error::Tool {
            command: format!("{} {}", file_stem(command), subcommand.to_string_lossy()),
            dir: dir.to_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn file_stem(command: &Command) -> String {
    let program = Path::new(command.get_program());
    program
        .file_stem()
        .unwrap_or(program.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The first line of what `command` prints, as a tool's version; empty when it does not run or
/// fails, which no check then tells from another version.
pub(crate) fn first_line(command: &mut Command) -> String {
    let out = command.output().ok().filter(|o| o.status.success());
    out.map_or_default(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_keeps_no_variable_of_the_drivers_own() {
        let own = [
            "CARGO",
            "CARGO_MANIFEST_DIR",
            "CARGO_MANIFEST_PATH",
            "CARGO_PKG_NAME",
            "CARGO_PKG_VERSION_PRE",
        ];
        // The proxy's and the user's variables reach a build outside the driver too.
        let others = [
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            "CARGO_PKGS",
            "RUSTFLAGS",
            "RUSTUP_TOOLCHAIN",
            "PATH",
        ];
        assert!(own.iter().all(|name| is_drivers_own(OsStr::new(name))));
        assert!(!others.iter().any(|name| is_drivers_own(OsStr::new(name))));
        // The test runs under cargo, which sets its package's variables as `cargo run` does.
        let command = command("x");
        let removed: Vec<&OsStr> = (command.get_envs())
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name)
            .collect();
        assert!(removed.contains(&OsStr::new("CARGO_MANIFEST_DIR")));
        assert!(removed.iter().all(|name| is_drivers_own(name)));
    }
}
