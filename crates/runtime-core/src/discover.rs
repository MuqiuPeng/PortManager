//! Project discovery.
//!
//! Asking a developer to register their projects by hand contradicts the point
//! of the tool: it already knows what is listening, which pid owns it and what
//! directory that pid is in. Walking up from those directories to a repository
//! root finds the projects that actually matter — the ones running right now —
//! with no configuration and no false positives.
//!
//! A directory walk is offered as well, for projects that happen to be stopped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use runtime_adapter::PlatformAdapter;
use serde::{Deserialize, Serialize};

use crate::detect;
use crate::docker::Docker;
use crate::git;
use crate::store::Store;
use runtime_types::Result;

/// Files that mark a directory as the root of a project.
const MARKERS: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "requirements.txt",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "Gemfile",
    "composer.json",
    "docker-compose.yml",
    "compose.yml",
    ".runtime.json",
];

/// Never descended into, and never reported as a project root.
const SKIP_DIRECTORIES: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".cache",
    "Pods",
];

/// Path prefixes that are the operating system's, not the user's work.
///
/// Without this a sandboxed chat app's container directory looks exactly like
/// a project: it has a working directory and it is listening on a port.
#[cfg(target_os = "macos")]
const SYSTEM_PREFIXES: &[&str] = &[
    "/System",
    "/Library",
    "/usr",
    "/bin",
    "/sbin",
    "/opt",
    "/Applications",
    "/private/var",
    "/var",
    "/dev",
];

#[cfg(windows)]
const SYSTEM_PREFIXES: &[&str] = &[
    "C:\\Windows",
    "C:\\Program Files",
    "C:\\Program Files (x86)",
    "C:\\ProgramData",
];

#[cfg(not(any(target_os = "macos", windows)))]
const SYSTEM_PREFIXES: &[&str] = &["/usr", "/bin", "/sbin", "/opt", "/var", "/proc", "/sys", "/dev"];

/// Directory-name fragments that mark a per-application sandbox or cache.
const SYSTEM_FRAGMENTS: &[&str] = &[
    "/Library/Containers/",
    "/Library/Group Containers/",
    "/Library/Caches/",
    "/.Trash/",
    "\\AppData\\Local\\Temp\\",
];

/// How deep a directory walk goes below each root.
pub const MAX_SCAN_DEPTH: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    pub root_path: PathBuf,
    pub name: String,
    /// True when something inside this directory is listening right now.
    pub running: bool,
    /// Ports observed under this root, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<u16>,
    /// Why this looks like a project: `git`, `package.json`, …
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// Service names inference would create.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_services: Vec<String>,
    /// True when this project is already in the registry.
    pub registered: bool,
}

/// Find projects without being told where they are.
///
/// `roots` adds directory trees to walk; discovery from running processes
/// always happens, because it is both free and the most reliable signal.
pub fn discover(
    store: &Store,
    adapter: &dyn PlatformAdapter,
    docker: &Docker,
    roots: &[PathBuf],
) -> Result<Vec<Discovery>> {
    let mut found: BTreeMap<PathBuf, Discovery> = BTreeMap::new();

    // A compose file's directory with containers running out of it is a project
    // by the same standard as a directory with a process running out of it.
    let running = roots_from_running_processes(adapter)?
        .into_iter()
        .chain(docker.project_roots());

    for (root, ports) in running {
        let entry = found.entry(root.clone()).or_insert_with(|| describe(&root));
        entry.running = true;
        for port in ports {
            if !entry.ports.contains(&port) {
                entry.ports.push(port);
            }
        }
        entry.ports.sort_unstable();
    }

    for root in roots {
        for candidate in walk(root, MAX_SCAN_DEPTH) {
            found.entry(candidate.clone()).or_insert_with(|| describe(&candidate));
        }
    }

    // Every directory the runtime already knows, not only the ones it knows as
    // projects. A second clone of a repository is registered as a checkout of
    // it, so asking whether a directory is a project reported one the runtime
    // was already managing as unknown — and adopting it registered it as the
    // checkout it already was, which changed nothing, so it stayed "not
    // registered" however many times you asked.
    let mut registered: Vec<PathBuf> = Vec::new();
    for project in store.list_projects()? {
        registered.push(project.root_path.clone());
        for workspace in store.list_workspaces(&project.id)? {
            registered.push(workspace.path);
        }
    }

    let mut discoveries: Vec<Discovery> = found.into_values().collect();
    for discovery in &mut discoveries {
        discovery.registered = registered.contains(&discovery.root_path);
    }

    // Running projects first — they are what the user is actually looking at —
    // then alphabetically.
    discoveries.sort_by(|a, b| {
        b.running
            .cmp(&a.running)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(discoveries)
}

/// Map every listening socket back to a project root.
fn roots_from_running_processes(
    adapter: &dyn PlatformAdapter,
) -> Result<BTreeMap<PathBuf, Vec<u16>>> {
    let mut roots: BTreeMap<PathBuf, Vec<u16>> = BTreeMap::new();

    for binding in adapter.port().listening_ports()? {
        let Some(pid) = binding.primary_pid() else {
            continue;
        };
        let Some(process) = adapter.process().process_info(pid)? else {
            continue;
        };
        let Some(cwd) = process.cwd else {
            continue;
        };
        let Some(root) = project_root_for(&cwd) else {
            continue;
        };
        roots.entry(root).or_default().push(binding.port);
    }
    Ok(roots)
}

/// Walk up from a working directory to the root of the project containing it.
///
/// The git root wins when there is one, because that is the boundary a
/// developer thinks in; otherwise the nearest directory with a build manifest.
pub fn project_root_for(cwd: &Path) -> Option<PathBuf> {
    if is_system_path(cwd) {
        return None;
    }

    // A repository answers this exactly, including for worktrees.
    if let Some(info) = git::info(cwd) {
        return (!is_system_path(&info.main_root)).then_some(info.main_root);
    }

    let mut current = Some(cwd);
    while let Some(directory) = current {
        if is_system_path(directory) {
            return None;
        }
        if has_marker(directory) && !is_skipped(directory) {
            return Some(directory.to_path_buf());
        }
        current = directory.parent();
    }
    None
}

fn describe(root: &Path) -> Discovery {
    let detection = detect::detect(root);
    let mut markers: Vec<String> = MARKERS
        .iter()
        .filter(|marker| root.join(marker).exists())
        .map(|marker| (*marker).to_string())
        .collect();

    let git = git::info(root);
    if git.is_some() {
        markers.insert(0, "git".to_string());
    }

    Discovery {
        name: detection.name,
        suggested_services: detection
            .services
            .iter()
            .map(|service| service.name.clone())
            .collect(),
        git_branch: git.and_then(|info| info.branch),
        root_path: root.to_path_buf(),
        running: false,
        ports: Vec::new(),
        markers,
        registered: false,
    }
}

/// Directories under `root` that look like project roots.
///
/// A candidate is not descended into: a repository's subdirectories are parts
/// of it, not separate projects.
fn walk(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_into(root, depth, &mut out);
    out
}

fn walk_into(directory: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if is_system_path(directory) || is_skipped(directory) {
        return;
    }
    if has_marker(directory) || directory.join(".git").exists() {
        out.push(directory.to_path_buf());
        return;
    }
    if depth == 0 {
        return;
    }

    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Symlinks are not followed: a link into the home directory would
        // otherwise turn a bounded walk into an unbounded one.
        if entry.file_type().is_ok_and(|kind| kind.is_dir())
            && !entry.file_name().to_string_lossy().starts_with('.')
        {
            walk_into(&path, depth - 1, out);
        }
    }
}

/// True when a path lives inside a hidden directory.
///
/// Agent tooling keeps scratch worktrees in places like `.claude/worktrees`;
/// they are checkouts as far as git is concerned but not projects a developer
/// manages, and they come and go.
pub fn is_tool_managed_path(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .starts_with('.')
    })
}

fn has_marker(directory: &Path) -> bool {
    MARKERS.iter().any(|marker| directory.join(marker).exists())
}

fn is_skipped(directory: &Path) -> bool {
    directory
        .file_name()
        .map(|name| SKIP_DIRECTORIES.contains(&name.to_string_lossy().as_ref()))
        .unwrap_or(false)
}

fn is_system_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    if text == "/" {
        return true;
    }
    if SYSTEM_FRAGMENTS.iter().any(|fragment| text.contains(fragment)) {
        return true;
    }
    SYSTEM_PREFIXES.iter().any(|prefix| {
        text == *prefix || text.starts_with(&format!("{prefix}{}", std::path::MAIN_SEPARATOR))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory the discovery filters will not reject.
    ///
    /// The obvious `tempfile::tempdir()` lands under `/var/folders` on macOS,
    /// which `is_system_path` excludes — correctly, since a temp directory is
    /// not somebody's project. The home directory is outside both that filter
    /// and (normally) any git repository, so a fixture there exercises the walk
    /// rather than the filters.
    fn scratch() -> tempfile::TempDir {
        let home = directories::BaseDirs::new().expect("a home directory");
        tempfile::Builder::new()
            .prefix(".local-runtime-test-")
            .tempdir_in(home.home_dir())
            .expect("a writable home directory")
    }

    #[test]
    fn system_paths_are_never_projects() {
        // The prefixes differ by platform, so the examples have to as well —
        // `/usr/local/bin` is not a system path on Windows and asserting that
        // it is only proves the test was written somewhere else.
        #[cfg(not(windows))]
        {
            assert!(is_system_path(Path::new("/")));
            assert!(is_system_path(Path::new("/usr/local/bin")));
            assert!(!is_system_path(Path::new("/Users/dev/projects/loom")));
        }
        #[cfg(windows)]
        {
            assert!(is_system_path(Path::new("C:\\Windows\\System32")));
            assert!(is_system_path(Path::new("C:\\Program Files\\nodejs")));
            assert!(!is_system_path(Path::new("C:\\Users\\dev\\projects\\loom")));
        }

        // The case that motivated the filter, and the one that is the same
        // everywhere: a sandboxed app's container has a working directory and
        // listens on a port, and is not a project.
        assert!(is_system_path(Path::new(
            "/Users/dev/Library/Containers/com.tencent.qq/Data"
        )));
    }

    #[test]
    fn walks_up_to_the_nearest_manifest() {
        let dir = scratch();
        let root = dir.path().join("app");
        let nested = root.join("packages").join("api").join("src");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();

        assert_eq!(project_root_for(&nested), Some(root));
    }

    #[test]
    fn a_directory_with_no_manifest_is_not_a_project() {
        let dir = scratch();
        let nested = dir.path().join("scratch");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(project_root_for(&nested), None);
    }

    #[test]
    fn the_walk_stops_at_a_project_and_skips_dependency_directories() {
        let dir = scratch();
        let app = dir.path().join("app");
        std::fs::create_dir_all(app.join("node_modules").join("left-pad")).unwrap();
        std::fs::write(app.join("package.json"), "{}").unwrap();
        // A vendored dependency has its own manifest and must not be reported.
        std::fs::write(app.join("node_modules").join("left-pad").join("package.json"), "{}")
            .unwrap();

        let found = walk(dir.path(), MAX_SCAN_DEPTH);
        assert_eq!(found, vec![app]);
    }
}
