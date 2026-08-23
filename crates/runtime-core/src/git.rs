//! Git awareness.
//!
//! Worktrees are a first-class concept here rather than an afterthought: the
//! same project checked out three times is three workspaces, and each one needs
//! its own stable ports.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use runtime_types::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitInfo {
    /// Root of *this* checkout.
    pub root: PathBuf,
    /// Root of the primary checkout, which is what defines the project.
    pub main_root: PathBuf,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub is_worktree: bool,
    pub remote_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub commit: Option<String>,
    /// The primary checkout is listed first by `git worktree list`.
    pub is_main: bool,
}

/// Read git state for a directory, or `None` when it is not a repository.
pub fn info(path: &Path) -> Option<GitInfo> {
    let root = git_path(&run(path, &["rev-parse", "--show-toplevel"])?);

    // `--git-dir` points into `.git/worktrees/<name>` for a linked worktree,
    // while `--git-common-dir` always points at the primary `.git`. Comparing
    // them is the cheapest reliable worktree test.
    let git_dir = run(path, &["rev-parse", "--absolute-git-dir"]);
    let common_dir = run(path, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let is_worktree = match (&git_dir, &common_dir) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    };

    let main_root = if is_worktree {
        common_dir
            .as_deref()
            .map(git_path)
            .and_then(|dir| dir.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| root.clone())
    } else {
        root.clone()
    };

    let branch = run(path, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD");

    Some(GitInfo {
        root,
        main_root,
        branch,
        commit: run(path, &["rev-parse", "HEAD"]),
        is_worktree,
        remote_url: run(path, &["remote", "get-url", "origin"]),
    })
}

/// Every checkout of the repository containing `path`.
pub fn worktrees(path: &Path) -> Result<Vec<WorktreeEntry>> {
    let Some(output) = run(path, &["worktree", "list", "--porcelain"]) else {
        return Ok(Vec::new());
    };

    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for line in output.lines() {
        if let Some(raw) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(WorktreeEntry {
                path: git_path(raw.trim()),
                branch: None,
                commit: None,
                is_main: entries.is_empty(),
            });
        } else if let Some(raw) = line.strip_prefix("HEAD ") {
            if let Some(entry) = current.as_mut() {
                entry.commit = Some(raw.trim().to_string());
            }
        } else if let Some(raw) = line.strip_prefix("branch ") {
            if let Some(entry) = current.as_mut() {
                entry.branch = Some(raw.trim().trim_start_matches("refs/heads/").to_string());
            }
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    Ok(entries)
}

/// Turn a path as git prints it into the spelling the rest of the system uses.
///
/// git reports POSIX separators on every platform — `E:/projects/app` — while
/// working directories and `canonicalize` give `E:\projects\app` on Windows.
/// Left alone, the two spellings of one directory become two projects.
fn git_path(text: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(text.replace('/', "\\"))
    } else {
        PathBuf::from(text)
    }
}

fn run(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    runtime_adapter::without_a_console(&mut command);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_paths_use_the_platform_separator() {
        let path = git_path("E:/projects/app");
        if cfg!(windows) {
            assert_eq!(path, PathBuf::from(r"E:\projects\app"));
        } else {
            assert_eq!(path, PathBuf::from("E:/projects/app"));
        }
    }

    /// The repository this test runs in, read through the real `git` binary.
    #[test]
    fn info_reports_a_root_in_the_platform_spelling() {
        let Some(info) = info(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
            return; // not a checkout; nothing to assert
        };
        let text = info.main_root.to_string_lossy().into_owned();
        if cfg!(windows) {
            assert!(!text.contains('/'), "forward slashes survived: {text}");
        }
    }
}
