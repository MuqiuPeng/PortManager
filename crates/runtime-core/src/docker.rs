//! Container awareness.
//!
//! Every containerised service on a machine publishes its ports through one
//! Docker process, so the chain this runtime is built on —
//! `port -> pid -> cwd -> project` — dead-ends at a single pid whose working
//! directory is Docker's own. Five services become five identical rows reading
//! `com.docker.docker`.
//!
//! Compose labels carry the missing link: `com.docker.compose.project.working_dir`
//! is the directory the compose file lives in, which is the project root by the
//! same definition used everywhere else here.
//!
//! Containers can also be started and stopped here. That does not contradict
//! the rule that the runtime never terminates a process it did not start: that
//! rule exists because signalling an arbitrary pid is dangerous and pids are
//! recycled. `docker stop` is neither — it is a graceful operation on a named,
//! restartable object, and exactly what the developer would type themselves.
//!
//! What this deliberately does *not* do is replace compose. Building images,
//! ordering dependencies, networks and volumes stay where they are; the value
//! on offer is one picture and one switch, not a second orchestrator.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use runtime_types::{Result, RuntimeError};
use serde::{Deserialize, Serialize};

/// How long a container listing is reused.
///
/// Long enough that listing every port costs one `docker` invocation rather
/// than dozens, short enough that starting a container shows up promptly.
const CACHE_TTL: Duration = Duration::from_secs(3);

/// How long to wait before retrying after Docker turns out to be absent.
///
/// Shelling out on every port lookup only to fail is the expensive case, and
/// most machines that lack Docker will lack it for the whole session.
const UNAVAILABLE_TTL: Duration = Duration::from_secs(60);

/// Docker can be slow to answer while it is starting; do not block the daemon.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    /// Compose project name, absent for containers started with `docker run`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_service: Option<String>,
    /// Directory the compose file lives in — the project root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
    /// `running`, `exited`, `paused`, …
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    /// Host ports this container publishes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub published_ports: Vec<u16>,
}

impl ContainerInfo {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

impl ContainerInfo {
    /// How to name this container to a human.
    ///
    /// The compose service name where there is one, else the container name —
    /// never a guess about which project an unlabelled container belongs to.
    pub fn display_service(&self) -> &str {
        self.compose_service.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug)]
struct Cache {
    containers: Vec<ContainerInfo>,
    fetched_at: Option<Instant>,
    available: bool,
}

/// A cached view of the local Docker daemon.
#[derive(Debug)]
pub struct Docker {
    cache: Mutex<Cache>,
}

impl Default for Docker {
    fn default() -> Self {
        Self::new()
    }
}

impl Docker {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(Cache {
                containers: Vec::new(),
                fetched_at: None,
                available: true,
            }),
        }
    }

    /// Every container, running or not, refreshed at most once per [`CACHE_TTL`].
    ///
    /// Stopped ones are included because a switch that can only turn things off
    /// is half a switch.
    pub fn containers(&self) -> Vec<ContainerInfo> {
        let Ok(mut cache) = self.cache.lock() else {
            return Vec::new();
        };

        let ttl = if cache.available {
            CACHE_TTL
        } else {
            UNAVAILABLE_TTL
        };
        if cache.fetched_at.is_some_and(|at| at.elapsed() < ttl) {
            return cache.containers.clone();
        }

        match inspect_running() {
            Some(containers) => {
                cache.containers = containers;
                cache.available = true;
            }
            None => {
                cache.containers.clear();
                cache.available = false;
            }
        }
        cache.fetched_at = Some(Instant::now());
        cache.containers.clone()
    }

    /// The container publishing `port`, if any.
    pub fn container_for_port(&self, port: u16) -> Option<ContainerInfo> {
        self.containers()
            .into_iter()
            .find(|container| container.is_running() && container.published_ports.contains(&port))
    }

    pub fn container(&self, name: &str) -> Option<ContainerInfo> {
        self.containers()
            .into_iter()
            .find(|container| container.name == name || container.id.starts_with(name))
    }

    /// Containers belonging to a compose project rooted at `directory`.
    pub fn containers_in(&self, directory: &Path) -> Vec<ContainerInfo> {
        self.containers()
            .into_iter()
            .filter(|container| container.working_dir.as_deref() == Some(directory))
            .collect()
    }

    /// Start, stop or restart a container by name.
    ///
    /// Named rather than by pid: a container id is stable and unambiguous,
    /// which is why these are safe to offer for containers the runtime did not
    /// create.
    pub fn control(&self, name: &str, action: ContainerAction) -> Result<()> {
        let container = self
            .container(name)
            .ok_or_else(|| RuntimeError::not_found("container", name))?;

        let docker = docker_binary()
            .ok_or_else(|| RuntimeError::unsupported("docker is not installed"))?;
        let verb = match action {
            ContainerAction::Start => "start",
            ContainerAction::Stop => "stop",
            ContainerAction::Restart => "restart",
        };
        run(&docker, &[verb, &container.id]).ok_or_else(|| {
            RuntimeError::io(format!("`docker {verb} {}` failed", container.name))
        })?;

        // The cached view is now wrong, and the caller is about to read it.
        self.invalidate();
        Ok(())
    }

    /// A container's own output, which the runtime never captured itself.
    pub fn logs(&self, name: &str, max_lines: usize) -> Result<Vec<String>> {
        let container = self
            .container(name)
            .ok_or_else(|| RuntimeError::not_found("container", name))?;
        let docker = docker_binary()
            .ok_or_else(|| RuntimeError::unsupported("docker is not installed"))?;

        let tail = max_lines.to_string();
        let raw = run(&docker, &["logs", "--tail", &tail, &container.id])
            .ok_or_else(|| RuntimeError::io(format!("`docker logs {}` failed", container.name)))?;
        Ok(raw.lines().map(str::to_string).collect())
    }

    pub fn invalidate(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.fetched_at = None;
        }
    }

    /// Compose project roots, for discovery.
    ///
    /// A directory containing a compose file with something running out of it
    /// is a project by exactly the same standard applied to native processes.
    pub fn project_roots(&self) -> Vec<(PathBuf, Vec<u16>)> {
        let mut roots: HashMap<PathBuf, Vec<u16>> = HashMap::new();
        for container in self.containers() {
            let Some(directory) = container.working_dir else {
                continue;
            };
            let entry = roots.entry(directory).or_default();
            for port in container.published_ports {
                if !entry.contains(&port) {
                    entry.push(port);
                }
            }
        }
        let mut out: Vec<(PathBuf, Vec<u16>)> = roots.into_iter().collect();
        for (_, ports) in &mut out {
            ports.sort_unstable();
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// One `docker inspect` over every running container.
///
/// Returns `None` when Docker is not installed or not answering, which the
/// caller caches so an absent Docker is not paid for on every lookup.
fn inspect_running() -> Option<Vec<ContainerInfo>> {
    let docker = docker_binary()?;

    // `--all`: a stopped container is still something the user may want to
    // switch back on, and it still says which project it belongs to.
    let ids = run(&docker, &["ps", "--all", "--quiet", "--no-trunc"])?;
    let ids: Vec<&str> = ids.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if ids.is_empty() {
        return Some(Vec::new());
    }

    let mut args = vec!["inspect", "--format", "{{json .}}"];
    args.extend(ids.iter().copied());
    let raw = run(&docker, &args)?;

    Some(
        raw.lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|value| parse_container(&value))
            .collect(),
    )
}

fn parse_container(value: &serde_json::Value) -> Option<ContainerInfo> {
    let labels = value.pointer("/Config/Labels");
    let label = |key: &str| -> Option<String> {
        labels
            .and_then(|l| l.get(key))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };

    Some(ContainerInfo {
        id: value.get("Id")?.as_str()?.to_string(),
        // Docker prefixes container names with a slash.
        name: value
            .get("Name")?
            .as_str()?
            .trim_start_matches('/')
            .to_string(),
        image: value
            .pointer("/Config/Image")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        compose_project: label("com.docker.compose.project"),
        compose_service: label("com.docker.compose.service"),
        working_dir: label("com.docker.compose.project.working_dir").map(PathBuf::from),
        status: value
            .pointer("/State/Status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        health: value
            .pointer("/State/Health/Status")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        published_ports: published_ports(value),
    })
}

/// Host ports from `NetworkSettings.Ports`.
///
/// The shape is `{"5432/tcp": [{"HostIp": "0.0.0.0", "HostPort": "5433"}]}`,
/// with a null value for a port that is exposed but not published.
fn published_ports(value: &serde_json::Value) -> Vec<u16> {
    let Some(ports) = value.pointer("/NetworkSettings/Ports").and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for bindings in ports.values() {
        let Some(bindings) = bindings.as_array() else {
            continue;
        };
        for binding in bindings {
            let Some(port) = binding
                .get("HostPort")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse::<u16>().ok())
            else {
                continue;
            };
            // IPv4 and IPv6 bindings of the same port arrive separately.
            if !out.contains(&port) {
                out.push(port);
            }
        }
    }
    out.sort_unstable();
    out
}

/// Run a docker command with a deadline.
///
/// The output is drained on a separate thread while the parent waits. Polling
/// for exit without reading first deadlocks the moment the output exceeds the
/// pipe buffer — 64KB, which `docker inspect` passes at a handful of
/// containers — and the symptom is not a hang but silence: the command is
/// killed at the deadline and every container quietly disappears.
fn run(docker: &PathBuf, args: &[&str]) -> Option<String> {
    let mut child = Command::new(docker)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buffer);
        buffer
    });

    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // Docker Desktop starting up is when this actually happens.
                let _ = child.kill();
                tracing::debug!(?args, "docker command timed out");
                let _ = reader.join();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                let _ = reader.join();
                return None;
            }
        }
    };

    let output = reader.join().ok()?;
    if !status.success() {
        return None;
    }
    String::from_utf8(output).ok()
}

fn docker_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) { "docker.exe" } else { "docker" };

    if let Some(path) = std::env::var_os("PATH") {
        if let Some(found) = std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
        {
            return Some(found);
        }
    }

    [
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/usr/bin",
        "/Applications/Docker.app/Contents/Resources/bin",
    ]
    .iter()
    .map(|directory| PathBuf::from(directory).join(name))
    .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspect_fixture() -> serde_json::Value {
        serde_json::json!({
            "Id": "abc123",
            "Name": "/stockviewer-db",
            "Config": {
                "Image": "postgres:16-alpine",
                "Labels": {
                    "com.docker.compose.project": "onlinestockviewer",
                    "com.docker.compose.service": "db",
                    "com.docker.compose.project.working_dir": "/Users/dev/projects/OnlineStockViewer"
                }
            },
            "State": { "Status": "running", "Health": { "Status": "healthy" } },
            "NetworkSettings": {
                "Ports": {
                    "5432/tcp": [
                        { "HostIp": "0.0.0.0", "HostPort": "5432" },
                        { "HostIp": "::", "HostPort": "5432" }
                    ]
                }
            }
        })
    }

    #[test]
    fn compose_labels_supply_the_project_root() {
        let container = parse_container(&inspect_fixture()).unwrap();

        assert_eq!(container.name, "stockviewer-db");
        assert_eq!(container.compose_project.as_deref(), Some("onlinestockviewer"));
        assert_eq!(container.display_service(), "db");
        assert_eq!(
            container.working_dir,
            Some(PathBuf::from("/Users/dev/projects/OnlineStockViewer"))
        );
        assert_eq!(container.health.as_deref(), Some("healthy"));
        // The IPv4 and IPv6 bindings of one published port are one port.
        assert_eq!(container.published_ports, vec![5432]);
    }

    #[test]
    fn an_unlabelled_container_is_named_but_never_attributed() {
        let mut value = inspect_fixture();
        value["Config"]["Labels"] = serde_json::json!({});
        value["Name"] = serde_json::json!("/loom-postgres");

        let container = parse_container(&value).unwrap();

        // `loom-postgres` obviously belongs to Loom, and guessing that is
        // exactly the false positive discovery is built to avoid. Name it and
        // let the human decide.
        assert_eq!(container.compose_project, None);
        assert_eq!(container.working_dir, None);
        assert_eq!(container.display_service(), "loom-postgres");
    }

    #[test]
    fn a_stopped_container_is_still_reported() {
        let mut value = inspect_fixture();
        value["State"] = serde_json::json!({ "Status": "exited" });

        let container = parse_container(&value).unwrap();
        assert!(!container.is_running());
        // A switch that can only turn things off is half a switch, so stopped
        // containers have to be visible to be startable.
        assert_eq!(container.compose_service.as_deref(), Some("db"));
    }

    #[test]
    fn exposed_but_unpublished_ports_are_not_reported() {
        let mut value = inspect_fixture();
        value["NetworkSettings"]["Ports"] = serde_json::json!({ "5432/tcp": null });

        assert!(parse_container(&value).unwrap().published_ports.is_empty());
    }
}
