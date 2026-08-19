// Prevents a console window from opening alongside the app on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// The port `tauri.conf.json` points a dev build at.
#[cfg(dev)]
const DEV_SERVER_PORT: u16 = 1420;

/// A dev build loads the frontend from Vite, not from the binary.
///
/// Running `target/debug/runtime-desktop` directly therefore opens a blank
/// window, and nothing on screen says why. `tauri-build` sets the `dev` cfg
/// from the Tauri CLI's environment rather than the cargo profile, so even a
/// release build made with plain `cargo build` behaves this way.
#[cfg(dev)]
fn warn_if_dev_server_is_missing() {
    use std::net::TcpStream;
    use std::time::Duration;

    let reachable = TcpStream::connect_timeout(
        &([127, 0, 0, 1], DEV_SERVER_PORT).into(),
        Duration::from_millis(300),
    )
    .is_ok();

    if !reachable {
        eprintln!(
            "\nLocal Runtime: no dev server on port {DEV_SERVER_PORT}, so the window will be blank.\n\
             This binary was built by cargo, which always produces a dev build.\n\
             Run it with `pnpm --dir apps/desktop tauri dev`, or build a\n\
             standalone app with `pnpm --dir apps/desktop tauri build`.\n"
        );
    }
}

#[cfg(not(dev))]
fn warn_if_dev_server_is_missing() {}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LOCAL_RUNTIME_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("runtime_desktop=info")),
        )
        .init();
    warn_if_dev_server_is_missing();
    runtime_desktop_lib::run()
}
