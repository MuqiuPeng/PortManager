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

/// A connection that repairs itself.
///
/// The daemon can be restarted underneath the app (an upgrade, a crash, a
/// `runtime daemon stop`); rather than surfacing that to every screen, the
/// first failed call reconnects and retries once.
#[derive(Clone)]
pub struct DaemonHandle {
    client: Arc<Mutex<Option<Client>>>,
}

impl DaemonHandle {
    pub fn new() -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn call(&self, request: Request) -> Result<ResponseBody> {
        let mut guard = self.client.lock().await;

        if guard.is_none() {
            *guard = Some(runtime_ipc::client::connect_or_start().await?);
        }

        let first = guard
            .as_mut()
            .expect("connection was just established")
            .call(request.clone())
            .await;

        match first {
            Ok(response) => Ok(response),
            // A transport error means the connection is dead, not that the
            // request was invalid — drop it and try once on a fresh one.
            Err(RuntimeError::Io(_)) => {
                *guard = None;
                let mut client = runtime_ipc::client::connect_or_start().await?;
                let response = client.call(request).await?;
                *guard = Some(client);
                Ok(response)
            }
            Err(other) => Err(other),
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
