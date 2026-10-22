use std::path::PathBuf;

use crate::ci::Ci;
use crate::console::Console;
use crate::tools::Tools;

/// What every part of one run shares: the folder it runs in, its terminal, and the tools it
/// reads once.
#[derive(Debug)]
pub(crate) struct Context {
    /// The folder the driver runs in, which the workspace is found from and paths are named
    /// from.
    pub(crate) here: PathBuf,
    pub(crate) console: Console,
    pub(crate) tools: Tools,
}

impl Context {
    pub(crate) fn new(ci: Ci, here: PathBuf) -> Self {
        Self {
            here,
            console: Console::new(ci),
            tools: Tools::default(),
        }
    }
}
