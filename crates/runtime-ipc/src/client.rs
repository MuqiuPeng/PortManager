//! Client side of the IPC protocol.
//!
//! Used by the CLI today and by the MCP server and desktop app later, so that
//! all three reach the daemon through the same code path.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use runtime_core::events::RuntimeEvent;
use runtime_core::paths;
use runtime_types::{Result, RuntimeError};

use crate::protocol::{Frame, Request, ResponseBody};
use crate::transport::{connect, Connection};

pub struct Client {
    connection: Connection,
    next_id: u64,
    /// Events that arrived while waiting for a response.
    pending_events: VecDeque<RuntimeEvent>,
}

impl Client {
    pub async fn connect_default() -> Result<Self> {
        Self::connect_at(&paths::socket_path()?).await
    }

    pub async fn connect_at(path: &Path) -> Result<Self> {
        Ok(Self {
            connection: connect(path).await?,
            next_id: 1,
            pending_events: VecDeque::new(),
        })
    }

    /// Send a request and wait for its answer.
    ///
    /// Event frames that arrive in the meantime are buffered rather than
    /// dropped, so subscribing and calling on one connection is safe.
    pub async fn call(&mut self, request: Request) -> Result<ResponseBody> {
        let id = self.next_id;
        self.next_id += 1;

        self.connection
            .send(&Frame::Request { id, request })
            .await?;

        loop {
            let Some(frame) = self.connection.recv::<Frame>().await? else {
                return Err(RuntimeError::io(
                    "the daemon closed the connection".to_string(),
                ));
            };
            match frame {
                Frame::Response { id: response_id, result } if response_id == id => {
                    return Ok(result)
                }
                Frame::Error {
                    id: response_id,
                    error,
                    ..
                } if response_id == id => return Err(error),
                Frame::Event { event } => self.pending_events.push_back(event),
                // A frame for another id can only mean a protocol bug; say so
                // rather than blocking forever waiting for the right one.
                other => {
                    return Err(RuntimeError::internal(format!(
                        "unexpected frame while awaiting response {id}: {other:?}"
                    )))
                }
            }
        }
    }

    /// Wait for the next event. Requires a prior [`Request::Subscribe`].
    pub async fn next_event(&mut self) -> Result<Option<RuntimeEvent>> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(Some(event));
        }
        loop {
            let Some(frame) = self.connection.recv::<Frame>().await? else {
                return Ok(None);
            };
            if let Frame::Event { event } = frame {
                return Ok(Some(event));
            }
        }
    }
}

/// True when a daemon is reachable.
pub async fn is_running() -> bool {
    let Ok(path) = paths::socket_path() else {
        return false;
    };
    match Client::connect_at(&path).await {
        Ok(mut client) => client.call(Request::Ping).await.is_ok(),
        Err(_) => false,
    }
}

/// How long to wait for a freshly spawned daemon to start listening.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// One caller at a time may decide the daemon needs starting.
///
/// Without this, everything that could not connect started one. The desktop
/// app polls, so a daemon that was merely slow to answer produced a spawn per
/// poll: 1703 of them on one machine, of which twenty were still alive, all
/// holding the same socket path and all reconciling the same database against
/// each other. The daemon itself refuses to be the second one — but it refuses
/// *after* starting, which makes it an exited child rather than no child.
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Connect, starting the daemon if it is not already running.
///
/// Every client uses this, so a user's first command works without a separate
/// install step and the desktop app does not need its own copy of the logic.
pub async fn connect_or_start() -> Result<Client> {
    if let Ok(client) = Client::connect_default().await {
        return Ok(client);
    }
    // Serialised, and then asked again inside the gate. A queue of callers
    // that all failed to connect a moment ago is the common case, and all but
    // the first are asking about a daemon that has since come up.
    // A tokio lock, because it is held across the connect and the wait that
    // follow it, and a blocking guard held across an await is not `Send`.
    let _turn = STARTING.lock().await;
    if let Ok(client) = Client::connect_default().await {
        return Ok(client);
    }
    spawn_daemon()?;
    wait_for_daemon(STARTUP_TIMEOUT).await
}

/// Launch the daemon, detached from whoever launched it.
///
/// The detaching is the point. A daemon left in its launcher's process group
/// dies with it: Ctrl-C in the terminal that ran a `runtime` command, or any
/// signal aimed at the desktop app's group, takes it down — and with it the
/// supervision of every service it started. Those services survive, because
/// each gets its own group, but they become orphans nobody is watching.
///
/// The daemon is meant to outlive every client. That has to be arranged, not
/// assumed.
pub fn spawn_daemon() -> Result<()> {
    let binary = daemon_binary()?;
    let mut command = std::process::Command::new(&binary);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut command);

    let child = command
        .spawn()
        .map_err(|err| RuntimeError::io(format!("failed to start {}: {err}", binary.display())))?;
    remember(child);
    Ok(())
}

/// Daemons this process started, kept only so that they can be reaped.
///
/// A `Child` that is dropped is not waited for, and on Unix an unwaited child
/// becomes a zombie the moment it exits. A daemon that finds another already
/// listening exits immediately, so every redundant spawn left one behind —
/// 1703 of them under a single desktop process, which is how this was found.
static SPAWNED: Mutex<Vec<std::process::Child>> = Mutex::new(Vec::new());

/// Hold on to a spawned daemon, and clear out any that have since exited.
///
/// Swept here rather than on a timer: the only moment this process is known to
/// care is when it is about to spawn another, and `try_wait` does not block on
/// the ones still running.
fn remember(child: std::process::Child) {
    let mut spawned = SPAWNED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    spawned.retain_mut(|earlier| !matches!(earlier.try_wait(), Ok(Some(_))));
    spawned.push(child);
}

#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // A group of its own, so a signal to the launcher's group misses it.
    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    /// Ctrl-C in the launching console does not reach it.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    /// No console of its own, and none inherited.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
}

/// Locate the daemon binary.
///
/// The order matters. Next to the calling binary comes first, so a checkout's
/// `target/debug` build never picks up a different installed copy, and so the
/// bundled sidecar inside a `.app` is found before anything else.
///
/// PATH is the *last* resort rather than the only one: an app launched from
/// Finder inherits a minimal PATH that contains none of the places a developer
/// installs things, which is exactly how this fails in the wild.
pub fn daemon_binary() -> Result<PathBuf> {
    let name = if cfg!(windows) {
        "runtime-daemon.exe"
    } else {
        "runtime-daemon"
    };

    let mut searched = Vec::new();
    for directory in search_directories() {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        searched.push(candidate);
    }

    // Fall back to PATH resolution by the OS.
    if which(name).is_some() {
        return Ok(PathBuf::from(name));
    }

    Err(RuntimeError::not_found(
        "runtime-daemon",
        format!(
            "looked in {} and on PATH",
            searched
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ))
}

/// Directories to look in, most specific first.
fn search_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();

    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            directories.push(dir.to_path_buf());
            // A macOS bundle may keep helpers beside the executable directory.
            directories.push(dir.join("..").join("Resources"));
        }
    }

    if let Some(home) = directories::BaseDirs::new() {
        directories.push(home.home_dir().join(".cargo").join("bin"));
        directories.push(home.home_dir().join(".local").join("bin"));
    }

    directories.push(PathBuf::from("/usr/local/bin"));
    directories.push(PathBuf::from("/opt/homebrew/bin"));
    directories
}

/// Minimal PATH lookup; `which` as a crate would be a dependency for six lines.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

pub async fn wait_for_daemon(timeout: Duration) -> Result<Client> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match Client::connect_default().await {
            Ok(client) => return Ok(client),
            // Reported rather than discarded: "it did not come up" describes
            // the symptom, and why the connection was refused is the only part
            // of it a reader can act on.
            Err(err) if tokio::time::Instant::now() >= deadline => {
                return Err(RuntimeError::io(format!(
                    "the daemon did not come up within {}s ({err}); \
                     run `runtime-daemon` directly to see why",
                    timeout.as_secs()
                )))
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A process's state letter, or `None` once it has been reaped away.
    fn state_of(pid: u32) -> Option<String> {
        let out = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if text.is_empty() { None } else { Some(text) }
    }

    /// A daemon that exits must not be left behind as a zombie.
    ///
    /// This is the shape the bug had in the wild: a daemon started when one was
    /// already listening exits at once, and a `Child` nobody waits for becomes
    /// defunct and stays. One desktop process had accumulated 1703 of them.
    ///
    /// `true` stands in for that daemon because it is the same case — a child
    /// that exits immediately — without needing a built binary or a socket.
    #[test]
    fn a_spawned_daemon_that_exits_is_not_left_defunct() {
        let mut pids = Vec::new();
        for _ in 0..6 {
            let child = std::process::Command::new("true").spawn().expect("spawn");
            pids.push(child.id());
            remember(child);
            std::thread::sleep(Duration::from_millis(40));
        }
        // The sweep runs on the next spawn, so ask after one more.
        let last = std::process::Command::new("true").spawn().expect("spawn");
        remember(last);
        std::thread::sleep(Duration::from_millis(40));

        let defunct: Vec<_> = pids
            .iter()
            .filter(|pid| state_of(**pid).is_some_and(|s| s.starts_with('Z')))
            .collect();
        assert!(
            defunct.is_empty(),
            "{} of {} exited daemons are still zombies: {defunct:?}",
            defunct.len(),
            pids.len()
        );
    }
}
