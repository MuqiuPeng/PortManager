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
    let root = PathBuf::from(run(path, &["rev-parse", "--show-toplevel"])?);

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
            .map(PathBuf::from)
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
                path: PathBuf::from(raw.trim()),
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

fn run(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
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
