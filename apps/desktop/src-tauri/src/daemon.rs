//! The app's connection to the runtime daemon.
//!
//! The desktop app holds no runtime state of its own — it is a client, exactly
//! like the CLI. Closing the window stops nothing, and two windows cannot
//! disagree about what is running.

use std::sync::Arc;

use runtime_ipc::protocol::{Request, ResponseBody};
use runtime_ipc::Client;
use runtime_types::{Result, RuntimeError};
use tokio::sync::Mutex;

/// Connections to the daemon, lent out one call at a time.
///
/// A pool rather than a single connection, because a single one made every
/// screen wait for every other. The handle is shared by every command the app
/// issues, so one slow call held the rest behind it: a scan walks the disk for
/// several seconds, and the ports view — which polls — sat behind it with
/// nothing to show. What looked like a slow port table was a queue.
///
/// Each connection is still used by one call at a time, which is what the
/// protocol expects; the lock is held only long enough to take one out and put
/// it back, never across the call itself.
///
/// It also repairs itself. The daemon can be restarted underneath the app — an
/// upgrade, a crash, a `runtime daemon stop` — and rather than surfacing that
/// to every screen, a call that fails on a dead connection retries once on a
/// fresh one, and the dead one is not returned to the pool.
#[derive(Clone)]
pub struct DaemonHandle {
    idle: Arc<Mutex<Vec<Client>>>,
}

/// How many connections to keep once the burst that needed them is over.
///
/// Each one is a named pipe instance the daemon holds open, so the pool grows
/// to whatever concurrency demands and then settles back to something small.
const KEPT_IDLE: usize = 4;

impl DaemonHandle {
    pub fn new() -> Self {
        Self {
            idle: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn call(&self, request: Request) -> Result<ResponseBody> {
        let mut client = match self.take().await {
            Some(client) => client,
            None => runtime_ipc::client::connect_or_start().await?,
        };

        match client.call(request.clone()).await {
            Ok(response) => {
                self.give_back(client).await;
                Ok(response)
            }
            // A transport error means the connection is dead, not that the
            // request was invalid. Drop it — returning it to the pool would
            // hand the same corpse to the next caller — and try once on a new
            // one.
            Err(RuntimeError::Io { .. }) => {
                drop(client);
                let mut fresh = runtime_ipc::client::connect_or_start().await?;
                let response = fresh.call(request).await?;
                self.give_back(fresh).await;
                Ok(response)
            }
            // The daemon answered, and the answer was a refusal. The connection
            // is fine and the error belongs to the caller.
            Err(other) => {
                self.give_back(client).await;
                Err(other)
            }
        }
    }

    async fn take(&self) -> Option<Client> {
        self.idle.lock().await.pop()
    }

    async fn give_back(&self, client: Client) {
        let mut idle = self.idle.lock().await;
        if idle.len() < KEPT_IDLE {
            idle.push(client);
        }
    }
}

impl Default for DaemonHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Open a second connection dedicated to the event stream.
///
/// Kept separate from the request connection so a long-lived subscription
/// cannot interleave with a command the user just clicked.
pub async fn subscribe() -> Result<Client> {
    let mut client = runtime_ipc::client::connect_or_start().await?;
    client.call(Request::Subscribe).await?;
    Ok(client)
}
