//! Local transport.
//!
//! A Unix domain socket on macOS and Linux, a named pipe on Windows. Both are
//! authenticated by the OS through filesystem or pipe permissions, so there is
//! no token to store and nothing listening on a TCP port that another machine
//! could reach.

use std::path::Path;

use runtime_types::{Result, RuntimeError};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Anything that can carry the protocol.
pub trait IpcStream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T> IpcStream for T where T: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

/// One connection, framed as newline-delimited JSON.
pub struct Connection {
    reader: BufReader<tokio::io::ReadHalf<Box<dyn IpcStream>>>,
    writer: tokio::io::WriteHalf<Box<dyn IpcStream>>,
    line: String,
}

impl Connection {
    pub fn new<S: IpcStream>(stream: S) -> Self {
        let boxed: Box<dyn IpcStream> = Box::new(stream);
        let (reader, writer) = tokio::io::split(boxed);
        Self {
            reader: BufReader::new(reader),
            writer,
            line: String::new(),
        }
    }

    pub async fn send<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let mut payload = serde_json::to_vec(value)
            .map_err(|err| RuntimeError::internal(format!("failed to encode frame: {err}")))?;
        payload.push(b'\n');
        self.writer.write_all(&payload).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Read one frame, or `None` at end of stream.
    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        self.line.clear();
        let read = self.reader.read_line(&mut self.line).await?;
        if read == 0 {
            return Ok(None);
        }
        let trimmed = self.line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        serde_json::from_str(trimmed)
            .map(Some)
            .map_err(|err| RuntimeError::internal(format!("malformed frame: {err}")))
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use tokio::net::{UnixListener, UnixStream};

    pub struct Listener {
        inner: UnixListener,
    }

    impl Listener {
        pub async fn bind(path: &Path) -> Result<Self> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // A socket file left behind by a crashed daemon would otherwise
            // make every future bind fail with EADDRINUSE.
            if path.exists() && UnixStream::connect(path).await.is_err() {
                std::fs::remove_file(path)?;
            }
            let inner = UnixListener::bind(path)?;
            Ok(Self { inner })
        }

        pub async fn accept(&self) -> Result<Connection> {
            let (stream, _) = self.inner.accept().await?;
            Ok(Connection::new(stream))
        }
    }

    pub async fn connect(path: &Path) -> Result<Connection> {
        let stream = UnixStream::connect(path).await.map_err(|err| {
            RuntimeError::io(format!("cannot reach the daemon at {}: {err}", path.display()))
        })?;
        Ok(Connection::new(stream))
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

    pub struct Listener {
        name: String,
        /// Named pipes accept one client per instance, so the next instance is
        /// created eagerly and handed out on the following accept.
        next: tokio::sync::Mutex<Option<NamedPipeServer>>,
    }

    impl Listener {
        pub async fn bind(path: &Path) -> Result<Self> {
            let name = path.to_string_lossy().to_string();
            let server = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)
                .map_err(|err| {
                    RuntimeError::io(format!("cannot create named pipe {name}: {err}"))
                })?;
            Ok(Self {
                name,
                next: tokio::sync::Mutex::new(Some(server)),
            })
        }

        pub async fn accept(&self) -> Result<Connection> {
            let mut guard = self.next.lock().await;
            let server = match guard.take() {
                Some(server) => server,
                None => ServerOptions::new().create(&self.name)?,
            };
            server.connect().await?;
            *guard = Some(ServerOptions::new().create(&self.name)?);
            drop(guard);
            Ok(Connection::new(server))
        }
    }

    pub async fn connect(path: &Path) -> Result<Connection> {
        let name = path.to_string_lossy().to_string();
        let client = ClientOptions::new().open(&name).map_err(|err| {
            RuntimeError::io(format!("cannot reach the daemon at {name}: {err}"))
        })?;
        Ok(Connection::new(client))
    }
}

pub use imp::{connect, Listener};
