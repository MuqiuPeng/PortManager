//! Where the runtime keeps its state.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use runtime_types::{Result, RuntimeError};

/// Overrides the whole data directory. Used by tests and by `runtime --data-dir`.
pub const DATA_DIR_ENV: &str = "LOCAL_RUNTIME_DATA_DIR";

/// `~/Library/Application Support/dev.localruntime.runtime` on macOS,
/// `%APPDATA%\localruntime\runtime` on Windows.
pub fn data_dir() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var(DATA_DIR_ENV) {
        if !custom.trim().is_empty() {
            return Ok(PathBuf::from(custom));
        }
    }
    let dirs = ProjectDirs::from("dev", "localruntime", "runtime")
        .ok_or_else(|| RuntimeError::internal("could not resolve a home directory"))?;
    Ok(dirs.data_dir().to_path_buf())
}

pub fn database_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("runtime.db"))
}

/// Unix domain socket paths are limited to `sun_path`, which is 104 bytes on
/// macOS. Anything longer is rejected at bind time.
const MAX_SOCKET_PATH: usize = 100;

/// The IPC endpoint: a Unix socket path, or a named pipe name on Windows.
pub fn socket_path() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("LOCAL_RUNTIME_SOCKET") {
        if !custom.trim().is_empty() {
            return Ok(PathBuf::from(custom));
        }
    }
    if cfg!(windows) {
        // Named pipes have no such limit and are not filesystem paths.
        return Ok(PathBuf::from(r"\\.\pipe\local-runtime"));
    }

    let dir = data_dir()?;
    let preferred = dir.join("runtime.sock");
    if preferred.as_os_str().len() <= MAX_SOCKET_PATH {
        return Ok(preferred);
    }

    // A deep data directory (a sandbox, a long user name) would otherwise make
    // the daemon unstartable. Fall back to a short, stable name derived from
    // the data directory, so different data dirs still get different sockets.
    Ok(std::env::temp_dir().join(format!("local-runtime-{}.sock", short_hash(&dir))))
}

/// FNV-1a over the path bytes.
///
/// Deliberately not `DefaultHasher`: its algorithm is unspecified and differs
/// between Rust versions, so a non-Rust client — the MCP server, an IDE plugin —
/// could not compute the same socket name. FNV-1a is ten lines in any language.
fn short_hash(path: &Path) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}


/// Holds the daemon pid so a second daemon refuses to start.
pub fn lock_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("daemon.pid"))
}

pub fn log_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("logs"))
}

pub fn ensure_data_dir() -> Result<PathBuf> {
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hash_matches_the_documented_fnv1a_values() {
        // Reference vectors, so a port of this function can be checked against
        // the same inputs without running the daemon.
        assert_eq!(short_hash(Path::new("")), "cbf29ce484222325");
        assert_eq!(short_hash(Path::new("a")), "af63dc4c8601ec8c");
        assert_eq!(short_hash(Path::new("/tmp/runtime")), "0d2e09ab25316fd0");
    }
}
