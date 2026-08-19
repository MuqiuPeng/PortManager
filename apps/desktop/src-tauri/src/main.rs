// Prevents a console window from opening alongside the app on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LOCAL_RUNTIME_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("runtime_desktop=info")),
        )
        .init();
    runtime_desktop_lib::run()
}
