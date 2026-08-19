//! Client side of the IPC protocol.
//!
//! Used by the CLI today and by the MCP server and desktop app later, so that
//! all three reach the daemon through the same code path.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
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

/// Connect, starting the daemon if it is not already running.
///
/// Every client uses this, so a user's first command works without a separate
/// install step and the desktop app does not need its own copy of the logic.
pub async fn connect_or_start() -> Result<Client> {
    if let Ok(client) = Client::connect_default().await {
        return Ok(client);
    }
    spawn_daemon()?;
    wait_for_daemon(STARTUP_TIMEOUT).await
}

/// Launch the daemon detached from the calling process.
pub fn spawn_daemon() -> Result<()> {
    let binary = daemon_binary()?;
    std::process::Command::new(&binary)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| RuntimeError::io(format!("failed to start {}: {err}", binary.display())))?;
    Ok(())
}

/// Look for the daemon next to the calling binary first, so a checkout's
/// `target/debug` build never picks up a different installed copy.
pub fn daemon_binary() -> Result<PathBuf> {
    let name = if cfg!(windows) {
        "runtime-daemon.exe"
    } else {
        "runtime-daemon"
    };
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // Fall back to PATH.
    Ok(PathBuf::from(name))
}

pub async fn wait_for_daemon(timeout: Duration) -> Result<Client> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(client) = Client::connect_default().await {
            return Ok(client);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(RuntimeError::io(format!(
                "the daemon did not come up within {}s; run `runtime-daemon` directly to see why",
                timeout.as_secs()
            )));
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}
