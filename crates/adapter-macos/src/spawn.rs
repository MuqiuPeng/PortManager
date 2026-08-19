//! Launching services on macOS.

use std::os::unix::process::CommandExt;
use std::process::Command;

use runtime_adapter::spawn::SpawnProvider;
use runtime_types::Result;

#[derive(Debug, Default)]
pub struct MacSpawnProvider;

impl MacSpawnProvider {
    pub fn new() -> Self {
        Self
    }
}

impl SpawnProvider for MacSpawnProvider {
    fn shell(&self) -> (String, Vec<String>) {
        // A login shell, so `nvm`, `pyenv` and friends resolve exactly as they
        // do in the terminal the user would otherwise have typed this into.
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        (shell, vec!["-lc".to_string()])
    }

    fn prepare(&self, command: &mut Command) -> Result<()> {
        // pgid 0 means "new group led by the child", which is what makes
        // whole-tree termination possible later.
        command.process_group(0);
        Ok(())
    }
}
