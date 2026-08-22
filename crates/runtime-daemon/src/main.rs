//! The runtime daemon.
//!
//! Holds all runtime state so that the GUI, the CLI and any number of coding
//! agents observe the same machine. Closing the desktop app does not stop a
//! service, and two clients cannot overwrite each other's view of what is
//! running.

mod handler;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use handler::Dispatcher;
use runtime_core::{paths, Runtime};
use runtime_ipc::protocol::{Frame, Request};
use runtime_ipc::transport::{Connection, Listener};
use runtime_types::Result;

#[derive(Debug, Parser)]
#[command(name = "runtime-daemon", version, about = "Local runtime daemon")]
struct Args {
    /// Override the data directory (database, socket, logs).
    #[arg(long, env = "LOCAL_RUNTIME_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Override the IPC endpoint.
    #[arg(long, env = "LOCAL_RUNTIME_SOCKET")]
    socket: Option<PathBuf>,

    /// Stop every managed service when the daemon exits.
    #[arg(long)]
    stop_services_on_exit: bool,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = Args::parse();
    init_tracing();

    match run(args).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(%err, "daemon exited with an error");
            eprintln!("error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<()> {
    if let Some(dir) = &args.data_dir {
        std::env::set_var(paths::DATA_DIR_ENV, dir);
    }
    if let Some(socket) = &args.socket {
        std::env::set_var("LOCAL_RUNTIME_SOCKET", socket);
    }
    paths::ensure_data_dir()?;

    let socket_path = paths::socket_path()?;
    // Refuse to start a second daemon: two authorities over the same state is
    // exactly the failure mode the daemon exists to prevent.
    if runtime_ipc::client::is_running().await {
        return Err(runtime_types::RuntimeError::already_exists(format!(
            "a daemon is already listening at {}",
            socket_path.display()
        )));
    }

    let runtime = Arc::new(Runtime::open_default()?);
    match runtime.prune_logs() {
        Ok(removed) if removed > 0 => tracing::info!(removed, "removed logs for deleted services"),
        Err(err) => tracing::warn!(%err, "could not prune old logs"),
        _ => {}
    }
    let corrected = runtime.reconcile()?;
    if corrected > 0 {
        tracing::info!(corrected, "closed out instances that died while the daemon was down");
    }

    let listener = Listener::bind(&socket_path).await?;
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let dispatcher = Arc::new(Dispatcher::new(Arc::clone(&runtime), shutdown_tx.clone()));

    let info = runtime.info()?;
    tracing::info!(
        platform = %info.platform,
        socket = %socket_path.display(),
        database = %info.database_path.display(),
        "daemon ready"
    );

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok(connection) => {
                        let dispatcher = Arc::clone(&dispatcher);
                        tokio::spawn(async move {
                            if let Err(err) = serve(dispatcher, connection).await {
                                tracing::debug!(%err, "connection closed");
                            }
                        });
                    }
                    Err(err) => tracing::warn!(%err, "failed to accept a connection"),
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    tracing::info!("shutting down");
    if args.stop_services_on_exit {
        let stopped = runtime.stop_all().await?;
        tracing::info!(stopped, "stopped managed services");
    }
    #[cfg(unix)]
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

/// Serve one connection until the client disconnects.
async fn serve(dispatcher: Arc<Dispatcher>, mut connection: Connection) -> Result<()> {
    let mut events: Option<tokio::sync::broadcast::Receiver<_>> = None;

    loop {
        // A subscribed connection must forward events while still answering
        // requests, so both are awaited together.
        let frame = match events.as_mut() {
            Some(receiver) => {
                tokio::select! {
                    incoming = connection.recv::<serde_json::Value>() => incoming?,
                    event = receiver.recv() => {
                        match event {
                            Ok(event) => {
                                connection.send(&Frame::Event { event }).await?;
                                continue;
                            }
                            // A lagging subscriber has missed events; keep the
                            // connection rather than dropping the client.
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(skipped, "event subscriber fell behind");
                                continue;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                events = None;
                                continue;
                            }
                        }
                    }
                }
            }
            None => connection.recv::<serde_json::Value>().await?,
        };

        // Parsed leniently: a client built against a newer protocol sends
        // methods this daemon does not know, and answering "unknown request"
        // is far easier to diagnose than dropping the connection.
        let Some(value) = frame else {
            return Ok(()); // end of stream
        };
        let request = match serde_json::from_value::<Frame>(value.clone()) {
            Ok(Frame::Request { id, request }) => Some((id, request)),
            // A frame only a server should send; ignore it rather than reply.
            Ok(_) => None,
            Err(err) => {
                let id = value.get("id").and_then(serde_json::Value::as_u64).unwrap_or(0);
                connection
                    .send(&Frame::error(
                        id,
                        // Naming the versions rather than only the request.
                        // The three programs ship in one bundle, so a request
                        // this daemon does not know means one of them came
                        // from somewhere else — an older copy on the PATH,
                        // usually — and knowing which two versions are talking
                        // is what makes that findable. The raw serde error
                        // stays: it names the request, which says what broke.
                        runtime_types::RuntimeError::invalid(format!(
                            "this daemon is {} (protocol {}) and does not know that request — \
                             whatever sent it was built against a different version, so one of \
                             them is an older copy left on your PATH: {err}",
                            env!("CARGO_PKG_VERSION"),
                            runtime_ipc::PROTOCOL_VERSION
                        )),
                    ))
                    .await?;
                continue;
            }
        };
        let Some((id, request)) = request else {
            continue;
        };

        if matches!(request, Request::Subscribe) {
            events = Some(dispatcher.runtime().events().subscribe());
        }

        match dispatcher.dispatch(request).await {
            Ok(result) => connection.send(&Frame::Response { id, result }).await?,
            Err(error) => connection.send(&Frame::error(id, error)).await?,
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("LOCAL_RUNTIME_LOG")
        .unwrap_or_else(|_| EnvFilter::new("runtime_daemon=info,runtime_core=info"));
    fmt().with_env_filter(filter).with_target(false).init();
}
