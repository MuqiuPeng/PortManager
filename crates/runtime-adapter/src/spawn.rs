//! Launching child processes.
//!
//! Spawning itself is portable (`std::process::Command`); what is not portable
//! is detaching the child into its own process group so the whole tree can be
//! signalled later. That single decision is what this trait isolates.

use std::process::Command;

use runtime_types::Result;

pub trait SpawnProvider: Send + Sync {
    /// The shell used to run a service's `command` string.
    ///
    /// Returns the program plus the arguments preceding the command itself,
    /// e.g. `("/bin/sh", ["-c"])` or `("cmd.exe", ["/C"])`.
    fn shell(&self) -> (String, Vec<String>);

    /// Apply platform flags that put the child in a new process group and
    /// detach it from the daemon's console.
    ///
    /// Called after the program and arguments are set but before spawning.
    fn prepare(&self, command: &mut Command) -> Result<()>;

    /// Build a ready-to-spawn command for a service's shell command line.
    fn build(&self, command_line: &str) -> Result<Command> {
        let (program, prefix) = self.shell();
        let mut command = Command::new(program);
        command.args(prefix);
        command.arg(command_line);
        self.prepare(&mut command)?;
        Ok(command)
    }
}
