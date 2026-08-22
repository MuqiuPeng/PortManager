//! Putting the command line and the MCP server where other tools look.
//!
//! The app carries all three programs — the daemon, the CLI and the MCP server
//! — because they speak one protocol to each other and an update that moved
//! one without the others produces a version skew nobody can see. That has
//! already happened here once: an MCP server built before a rename went on
//! sending request names the daemon had stopped accepting, and every call
//! failed while every check passed.
//!
//! So the bundle is the unit that moves, and what lives outside it is two
//! shims that point back into it. They are rewritten whenever the version they
//! name is not this one, which makes an update to the app an update to all
//! three by construction rather than by remembering.
//!
//! Best effort throughout: a shim that cannot be written is a terminal without
//! a `runtime` command, which is worth a log line and not worth refusing to
//! start a window over.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

/// Where a unix-ish system looks for a user's own commands.
fn bin_dir() -> Option<PathBuf> {
    dirs_home().map(|home| home.join(".local").join("bin"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Write the shims if they are missing or name another version.
pub fn ensure_shims(app: &AppHandle) {
    if let Err(err) = write_shims(app) {
        tracing::warn!(%err, "could not install the command line shims");
    }
}

fn write_shims(app: &AppHandle) -> std::io::Result<()> {
    let Some(bin) = bin_dir() else {
        return Ok(());
    };
    std::fs::create_dir_all(&bin)?;

    let version = app.package_info().version.to_string();
    let resources = app
        .path()
        .resource_dir()
        .map_err(|err| std::io::Error::other(err.to_string()))?;

    // The sidecars sit beside the executable, named for the target they were
    // built for — which is what Tauri strips when it installs them.
    let exe = std::env::current_exe()?;
    let here = exe.parent().unwrap_or(Path::new(".")).to_path_buf();

    write_shim(
        &bin.join(shim_name("runtime")),
        &version,
        &here.join(exe_name("runtime")),
        None,
    )?;
    write_shim(
        &bin.join(shim_name("runtime-mcp")),
        &version,
        &here.join(exe_name("runtime")),
        Some(&resources.join("mcp").join("index.js")),
    )?;
    Ok(())
}

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn shim_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.cmd")
    } else {
        stem.to_string()
    }
}

/// The line that says which app wrote this, and which version of it.
///
/// Read before writing: a shim already naming this version is left alone, so
/// this costs one small read per launch rather than a write.
fn stamp(version: &str) -> String {
    format!("# written by Local Runtime {version}")
}

fn write_shim(
    path: &Path,
    version: &str,
    target: &Path,
    node_script: Option<&Path>,
) -> std::io::Result<()> {
    let stamp = stamp(version);
    // Read as bytes, not as text. The first version of this asked for a
    // string, so a file that is not valid UTF-8 — which is what somebody
    // else's `runtime` would be, being a compiled program — failed to read and
    // fell through to being overwritten. The guard protected text and nothing
    // else, which is the opposite of what is at risk.
    if let Ok(existing) = std::fs::read(path) {
        let ours = b"written by Local Runtime";
        if !existing.windows(ours.len()).any(|window| window == ours) {
            tracing::info!(path = %path.display(), "leaving a command that is not ours alone");
            return Ok(());
        }
        if existing
            .windows(stamp.len())
            .any(|window| window == stamp.as_bytes())
        {
            return Ok(());
        }
    }

    let body = match node_script {
        // The MCP server is a node script, and the agent that launches it has
        // no version manager on its PATH — so the interpreter is whatever the
        // shell finds, and the failure when there is none says so.
        Some(script) => {
            if cfg!(windows) {
                format!(
                    "@echo off\r\nrem {stamp}\r\nnode \"{}\" %*\r\n",
                    script.display()
                )
            } else {
                format!(
                    "#!/bin/sh\n{stamp}\ncommand -v node >/dev/null 2>&1 || {{\n  echo 'the MCP server needs node on your PATH' >&2\n  exit 127\n}}\nexec node \"{}\" \"$@\"\n",
                    script.display()
                )
            }
        }
        None if cfg!(windows) => {
            format!("@echo off\r\nrem {stamp}\r\n\"{}\" %*\r\n", target.display())
        }
        None => format!(
            "#!/bin/sh\n{stamp}\nexec \"{}\" \"$@\"\n",
            target.display()
        ),
    };

    std::fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    tracing::info!(path = %path.display(), "installed a shim for {version}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Somebody else's `runtime` is not ours to replace.
    ///
    /// The first version of this guard read the file as a string, so anything
    /// that is not valid UTF-8 — a compiled program, which is exactly what a
    /// developer's own `runtime` would be — failed to read and fell through to
    /// being overwritten. It protected text files and nothing else, which is
    /// the opposite of what is at risk.
    #[test]
    fn a_command_that_is_not_ours_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime");
        // Bytes no string could hold, the way a real executable begins.
        let theirs = [0x7f, b'E', b'L', b'F', 0xff, 0xfe, 0x00, 0x80];
        std::fs::write(&path, theirs).unwrap();

        write_shim(&path, "9.9.9", Path::new("/somewhere/runtime"), None).unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            theirs,
            "a program that was not ours was overwritten"
        );
    }

    /// Ours, and out of date, is replaced.
    #[test]
    fn our_own_shim_is_brought_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime");
        std::fs::write(&path, "#!/bin/sh\n# written by Local Runtime 0.0.1\nexec old\n").unwrap();

        write_shim(&path, "9.9.9", Path::new("/somewhere/runtime"), None).unwrap();

        let now = std::fs::read_to_string(&path).unwrap();
        assert!(now.contains("9.9.9"), "{now}");
        assert!(now.contains("/somewhere/runtime"), "{now}");
    }
}
