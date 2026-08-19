//! Launching services on Windows.

use std::os::windows::process::CommandExt;
use std::process::Command;

use runtime_adapter::spawn::SpawnProvider;
use runtime_types::Result;

/// The child starts a new process group, which is the precondition for
/// `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)` to reach it and nothing else.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// No console window flashes when the daemon starts a service.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Default)]
pub struct WindowsSpawnProvider;

impl WindowsSpawnProvider {
    pub fn new() -> Self {
        Self
    }
}

impl SpawnProvider for WindowsSpawnProvider {
    fn shell(&self) -> (String, Vec<String>) {
        // cmd.exe is the safe default: it is always present, and it resolves
        // the `.cmd` shims npm and pnpm install on Windows.
        //
        // TODO(windows): make this configurable so users on PowerShell profiles
        // get the same environment they see in their terminal.
        ("cmd.exe".to_string(), vec!["/C".to_string()])
    }

    fn prepare(&self, command: &mut Command) -> Result<()> {
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        // TODO(windows): create a Job Object here and assign the child to it,
        // storing the handle so terminate_tree can close it.
        Ok(())
    }
}
