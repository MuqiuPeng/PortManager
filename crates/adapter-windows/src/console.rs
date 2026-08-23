//! Asking a service to stop, rather than telling it.
//!
//! The Unix half of `TerminationMode::Graceful` is `SIGTERM`. Windows has no
//! signals; the nearest equivalent for a console program is the event Ctrl+Break
//! delivers, which a dev server can catch to flush state and release its port
//! before it goes.
//!
//! There is one obstacle. `GenerateConsoleCtrlEvent` reaches only processes that
//! share the *caller's* console, and the daemon deliberately has none — it is
//! started with `DETACHED_PROCESS` so that closing a terminal cannot take it
//! down. So the console is borrowed for the length of the call: attach to the
//! service's, send the event, detach again.
//!
//! Sending it to the service's process group rather than to the whole console
//! is what keeps the daemon safe. `WindowsSpawnProvider` starts every service
//! with `CREATE_NEW_PROCESS_GROUP`, which makes the service's pid a group id and
//! puts everything it spawns inside; the daemon is in a group of its own and
//! never sees the event. Addressing the console as a whole (group 0) would
//! include the sender.

use std::sync::Mutex;

use runtime_types::{Result, RuntimeError};
use windows_sys::Win32::System::Console::{
    AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT,
};

/// Serialises the attach/detach dance.
///
/// A console is a property of the whole process, not of one call, so two stops
/// running at once would attach to two different services and detach each
/// other's console out from under them.
#[derive(Debug, Default)]
pub struct Console {
    borrowing: Mutex<()>,
}

impl Console {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deliver Ctrl+Break to `pid` and everything in its process group.
    ///
    /// `Ok(false)` means the event could not be delivered — the process has no
    /// console to borrow, or this process already has one of its own that must
    /// not be torn down to make room. Callers should escalate rather than wait
    /// out a grace period nobody heard the start of.
    pub fn interrupt(&self, pid: u32) -> Result<bool> {
        let _guard = self
            .borrowing
            .lock()
            .map_err(|_| RuntimeError::internal("the console mutex was poisoned"))?;

        // SAFETY: attaching to a console the caller does not have. A daemon that
        // already has one fails here with ERROR_ACCESS_DENIED, which is the
        // answer we want — better to escalate than to free a console that
        // something else is using.
        if unsafe { AttachConsole(pid) } == 0 {
            return Ok(false);
        }

        // SAFETY: the console is attached for the rest of this block, and the
        // group id excludes this process.
        let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
        // SAFETY: releasing the console attached immediately above. Detaching
        // matters even when the event failed: the borrow must not outlive it.
        unsafe { FreeConsole() };

        Ok(sent != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A caller that already owns a console cannot borrow another, and must be
    /// told so rather than have its own freed underneath it.
    ///
    /// The test harness has a console, which makes this the case under test:
    /// `interrupt` has to report "could not deliver" so that `terminate_tree`
    /// escalates instead of waiting out a grace period nobody heard.
    #[test]
    fn a_caller_with_its_own_console_is_refused_rather_than_stripped() {
        let console = Console::new();
        // Its own pid: a process is always attached to its own console, so if
        // this ever reported success it would mean the daemon had signalled
        // itself.
        let delivered = console.interrupt(std::process::id()).expect("no error");
        assert!(!delivered, "borrowing a console over an existing one must fail");
    }
}
