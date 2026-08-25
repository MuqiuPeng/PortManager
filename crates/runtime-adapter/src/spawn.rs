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

    /// Take ownership of a process the runtime has just started, so that
    /// everything it goes on to spawn can be terminated as one unit.
    ///
    /// Defaulted to nothing: only Windows has a mechanism (Job Objects) that
    /// survives a descendant re-parenting itself. Elsewhere termination walks
    /// the process tree, which is enough because a process group already
    /// travels with the children.
    fn confine(&self, _pid: u32) -> Result<()> {
        Ok(())
    }

    /// The service has exited; take down whatever it left behind.
    ///
    /// Anything still inside the confinement when the service itself is gone is
    /// a descendant that outlived it — the orphan the whole mechanism exists to
    /// catch. Called on every exit, however it happened: a service asked
    /// politely and shutting itself down cleanly leaves orphans exactly as
    /// readily as one that was killed.
    fn release(&self, _pid: u32) {}

    /// Build a ready-to-spawn command for a service's shell command line.
    fn build(&self, command_line: &str) -> Result<Command> {
        let (program, prefix) = self.shell();
        let mut command = Command::new(program);
        command.args(prefix);
        // Set here rather than by every caller: it belongs to the shell this
        // builds, not to the service, and a caller that forgot it would lose
        // the PATH silently and only on some machines.
        if let Some(path) = std::env::var_os("PATH") {
            command.env(CARRIED_PATH, path);
        }
        append_command_line(&mut command, &insist_on_our_path(command_line));
        self.prepare(&mut command)?;
        Ok(command)
    }
}

/// The variable the daemon's `PATH` travels in, so the script below never has
/// to quote a path.
const CARRIED_PATH: &str = "LOCAL_RUNTIME_PATH";

/// Put the daemon's `PATH` back after the shell's profile has had its turn.
///
/// Services run through a login shell so that a profile's `JAVA_HOME`, its
/// `pyenv` hooks and everything else are set exactly as they would be in a
/// terminal. But a login shell also rebuilds `PATH` — `export PATH="…:$PATH"`
/// is what every profile does — and puts its own directories in front of the
/// ones the daemon resolved at startup.
///
/// That is not cosmetic. The daemon asks both an interactive and a login shell
/// where commands live, because a version manager installs itself in the rc
/// file and a login shell never reads it. On this machine `zsh -lc` resolves
/// `node` to /usr/local/bin — v22 — while `zsh -ic` resolves it to nvm's v24,
/// and a Prisma migration run under the first died inside a dependency the
/// older one cannot load, reporting an ESM error that named no version at all.
///
/// So the profile runs, and then the answer the daemon already worked out is
/// restored over the top. Carried in a variable rather than written into the
/// script, because a `PATH` holds directory names that would otherwise need
/// quoting and one of them, on this machine, has a space in it.
fn insist_on_our_path(command_line: &str) -> String {
    if cfg!(windows) {
        return command_line.to_string();
    }
    format!(
        "[ -n \"${{{CARRIED_PATH}}}\" ] && export PATH=\"${{{CARRIED_PATH}}}\"\n{command_line}"
    )
}

/// Hand a whole command line to the shell without re-quoting it.
///
/// On Windows the two halves disagree about quoting. `Command::arg` escapes by
/// the rules the C runtime uses to split a command line, and `cmd.exe` does not
/// use those rules — so a command containing quotes arrives with them doubled or
/// stripped, and the shell runs something that is not what was written.
/// `python -c "import x"` reaches Python as a string literal rather than a
/// statement, which fails in a way that points at the command rather than at
/// the quoting.
///
/// `raw_arg` passes the text through untouched, which is what a shell that does
/// its own parsing needs.
#[cfg(windows)]
fn append_command_line(command: &mut Command, command_line: &str) {
    use std::os::windows::process::CommandExt;
    command.raw_arg(command_line);
}

#[cfg(not(windows))]
fn append_command_line(command: &mut Command, command_line: &str) {
    command.arg(command_line);
}

/// Run a short-lived helper without giving it a console window.
///
/// Windows hands a console-subsystem child a console of its own whenever the
/// parent has none — and the daemon is started detached precisely so that it
/// has none. Every helper the runtime shells out to (git, docker, pm2) would
/// otherwise flash a black rectangle on the desktop, and discovery runs them
/// in the hundreds: once per candidate directory, across every directory a
/// scan reaches.
///
/// Services are exempt on purpose. They go through [`SpawnProvider::prepare`],
/// which applies this alongside the process group they need, so that a service
/// can still be sent Ctrl+Break.
pub fn without_a_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}
