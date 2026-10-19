//! Running the tools the driver wraps.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// The cargo that runs this binary, which `cargo drive` names in `CARGO`.
pub(crate) fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
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
