//! PM2 as a backend, alongside processes and containers.
//!
//! Five services on this machine were under PM2 before the runtime knew about
//! any of them, and until now the only thing it could do about that was refuse
//! to interfere. Refusing is right for *deleting* — an entry removed from PM2
//! usually stops starting at boot, and that is a decision about the machine —
//! but it is far too strong for stopping something for a minute.
//!
//! So the same split Docker already gets: PM2 owns what these services are and
//! whether they come back after a reboot; the runtime owns whether they are
//! running right now. `start`, `stop` and `restart` are named, reversible
//! operations PM2 offers itself, and using them leaves its registry exactly as
//! it was. `pm2 delete` is deliberately not wrapped here.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use runtime_types::{Result, RuntimeError};

/// How long a listing is reused, matching the Docker backend.
const TTL: Duration = Duration::from_millis(1_500);
/// How long to wait before looking for PM2 again once it is absent.
const ABSENT_TTL: Duration = Duration::from_secs(60);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pm2Action {
    Start,
    Stop,
    Restart,
}

impl Pm2Action {
    fn verb(self) -> &'static str {
        match self {
            Pm2Action::Start => "start",
            Pm2Action::Stop => "stop",
            Pm2Action::Restart => "restart",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "start" => Some(Pm2Action::Start),
            "stop" => Some(Pm2Action::Stop),
            "restart" => Some(Pm2Action::Restart),
            _ => None,
        }
    }
}

/// One entry in PM2's own registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pm2Process {
    pub name: String,
    /// `online`, `stopped`, `errored`, `waiting restart`, …
    pub status: String,
    pub pid: Option<u32>,
    pub cwd: Option<PathBuf>,
    /// The interpreter and script PM2 runs, joined for display.
    pub command: String,
    pub restarts: u32,
    /// True when PM2 runs it with `NODE_ENV=production` or a `start` argument.
    ///
    /// Kept because it is what makes a restart dangerous: a Next project in
    /// production mode needs a build in `.next`, and a dev server run from the
    /// same directory replaces that build with one that has no `BUILD_ID`. The
    /// service keeps serving until something restarts it, and then it cannot
    /// start at all.
    pub production: bool,
    pub out_log: Option<PathBuf>,
    pub error_log: Option<PathBuf>,
    /// The mode-selecting variables PM2 launched it with.
    ///
    /// Read from PM2 rather than from the process because it is also there for
    /// an entry that is currently stopped — and because PM2 is the authority
    /// on how it will be started next time.
    pub mode_environment: Vec<(String, String)>,
}

impl Pm2Process {
    pub fn is_running(&self) -> bool {
        self.status == "online" || self.status == "launching"
    }
}

/// A cached view of the local PM2 daemon.
#[derive(Debug)]
pub struct Pm2 {
    binary: Option<PathBuf>,
    cache: Mutex<Option<(Instant, Vec<Pm2Process>)>>,
    absent_until: Mutex<Option<Instant>>,
}

impl Default for Pm2 {
    fn default() -> Self {
        Self::new()
    }
}

impl Pm2 {
    pub fn new() -> Self {
        Self {
            binary: find_binary(),
            cache: Mutex::new(None),
            absent_until: Mutex::new(None),
        }
    }

    pub fn available(&self) -> bool {
        self.binary.is_some()
    }

    pub fn processes(&self) -> Vec<Pm2Process> {
        if let Ok(cache) = self.cache.lock() {
            if let Some((at, processes)) = cache.as_ref() {
                if at.elapsed() < TTL {
                    return processes.clone();
                }
            }
        }
        if let Ok(absent) = self.absent_until.lock() {
            if absent.is_some_and(|until| Instant::now() < until) {
                return Vec::new();
            }
        }

        let Some(binary) = self.binary.as_ref() else {
            return Vec::new();
        };
        let Some(raw) = run(binary, &["jlist"]) else {
            // Not installed, or the daemon is not answering. Either way, do not
            // pay for the attempt on every lookup.
            if let Ok(mut absent) = self.absent_until.lock() {
                *absent = Some(Instant::now() + ABSENT_TTL);
            }
            return Vec::new();
        };

        let processes = parse(&raw);
        if let Ok(mut cache) = self.cache.lock() {
            *cache = Some((Instant::now(), processes.clone()));
        }
        processes
    }

    pub fn process(&self, name: &str) -> Option<Pm2Process> {
        self.processes().into_iter().find(|p| p.name == name)
    }

    /// PM2 entries whose working directory is inside a checkout.
    pub fn processes_in(&self, directory: &std::path::Path) -> Vec<Pm2Process> {
        self.processes()
            .into_iter()
            .filter(|process| {
                process
                    .cwd
                    .as_ref()
                    .is_some_and(|cwd| cwd.starts_with(directory))
            })
            .collect()
    }

    /// Switch one entry on or off.
    ///
    /// Only the three reversible verbs. `delete` is not offered: it takes the
    /// entry out of PM2's registry, which is usually also what stops it coming
    /// back after a reboot, and nothing in this runtime should make that
    /// change on a user's behalf.
    pub fn control(&self, name: &str, action: Pm2Action) -> Result<()> {
        let Some(binary) = self.binary.as_ref() else {
            return Err(RuntimeError::unsupported("pm2 is not installed"));
        };
        if self.process(name).is_none() {
            return Err(RuntimeError::invalid(format!("pm2 has no entry '{name}'")));
        }

        let output = run(binary, &[action.verb(), name]);
        self.invalidate();
        if output.is_none() {
            return Err(RuntimeError::io(format!(
                "`pm2 {} {name}` did not complete",
                action.verb()
            )));
        }
        Ok(())
    }

    pub fn invalidate(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            *cache = None;
        }
    }
}

/// Where PM2 might be, given that a daemon launched by the app does not have a
/// shell's `PATH`.
///
/// Searched rather than shelled out to. `which` is not a program on Windows,
/// and running one to find another is a dependency on the very environment
/// this is compensating for.
fn find_binary() -> Option<PathBuf> {
    for name in EXECUTABLE_NAMES {
        if let Some(path) = on_path(name) {
            return Some(path);
        }
    }
    // A node version manager puts it under a versioned prefix that is on a
    // shell's PATH and nothing else's. Newest wins, matching what a shell
    // would have picked.
    let home = home_dir()?;
    let mut found: Vec<PathBuf> = std::fs::read_dir(home.join(".nvm/versions/node"))
        .ok()?
        .flatten()
        .flat_map(|entry| {
            EXECUTABLE_NAMES
                .iter()
                .map(move |name| entry.path().join("bin").join(name))
        })
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    found.pop()
}

/// What the executable is called. Windows needs the extension.
#[cfg(windows)]
const EXECUTABLE_NAMES: &[&str] = &["pm2.cmd", "pm2.exe", "pm2"];
#[cfg(not(windows))]
const EXECUTABLE_NAMES: &[&str] = &["pm2"];

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Read `pm2 jlist`.
pub fn parse(raw: &str) -> Vec<Pm2Process> {
    let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(raw) else {
        return Vec::new();
    };

    entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name")?.as_str()?.to_string();
            let env = entry.get("pm2_env").unwrap_or(&serde_json::Value::Null);

            let args: Vec<String> = env
                .get("args")
                .and_then(|value| value.as_array())
                .map(|list| {
                    list.iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();

            let script = env
                .get("pm_exec_path")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();

            let node_env = env.get("NODE_ENV").and_then(|value| value.as_str());
            // `next start` is production whether or not NODE_ENV says so.
            let production =
                node_env == Some("production") || args.first().map(String::as_str) == Some("start");

            let mut command = script.clone();
            if !args.is_empty() {
                command.push(' ');
                command.push_str(&args.join(" "));
            }

            Some(Pm2Process {
                name,
                status: env
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                // PM2 reports 0 for a stopped entry, which is not a pid.
                pid: entry
                    .get("pid")
                    .and_then(|value| value.as_u64())
                    .filter(|pid| *pid > 0)
                    .map(|pid| pid as u32),
                cwd: env
                    .get("pm_cwd")
                    .and_then(|value| value.as_str())
                    .map(PathBuf::from),
                command: command.trim().to_string(),
                restarts: env
                    .get("restart_time")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0) as u32,
                production,
                mode_environment: crate::launch::MODE_VARIABLES
                    .iter()
                    .filter_map(|key| {
                        env.get(*key)
                            .and_then(|value| value.as_str())
                            .map(|value| ((*key).to_string(), value.to_string()))
                    })
                    .collect(),
                out_log: env
                    .get("pm_out_log_path")
                    .and_then(|value| value.as_str())
                    .map(PathBuf::from),
                error_log: env
                    .get("pm_err_log_path")
                    .and_then(|value| value.as_str())
                    .map(PathBuf::from),
            })
        })
        .collect()
}

/// How to actually execute what `find_binary` turned up.
///
/// A global npm install on Windows is a `.cmd` shim — a batch script rather
/// than an image, which the process API declines to execute. `cmd /C` is what
/// runs one, and doing it here keeps every caller from having to know.
fn launcher(binary: &PathBuf) -> Command {
    let is_script = binary
        .extension()
        .map(|extension| {
            let extension = extension.to_string_lossy().to_ascii_lowercase();
            extension == "cmd" || extension == "bat"
        })
        .unwrap_or(false);

    if cfg!(windows) && is_script {
        let mut command = Command::new("cmd");
        command.arg("/C").arg(binary);
        return command;
    }
    Command::new(binary)
}

/// Run a pm2 command with a deadline, draining output as it goes.
///
/// Same shape as the Docker runner and for the same reason: waiting for exit
/// without reading first deadlocks once the output passes the pipe buffer, and
/// the symptom is silence rather than a hang.
fn run(binary: &PathBuf, args: &[&str]) -> Option<String> {
    let mut child = launcher(binary)
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
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = reader.join();
                tracing::debug!(?args, "pm2 command timed out");
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                let _ = reader.join();
                return None;
            }
        }
    }

    let buffer = reader.join().ok()?;
    Some(String::from_utf8_lossy(&buffer).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from this machine's own `pm2 jlist`.
    const JLIST: &str = r#"[
      {"name":"flip7","pid":29106,"pm2_env":{
        "status":"online","restart_time":22,"NODE_ENV":"production",
        "pm_cwd":"/Users/x/projects/CodeBoardGamer",
        "pm_exec_path":"/Users/x/projects/CodeBoardGamer/server.mjs",
        "pm_out_log_path":"/Users/x/.pm2/logs/flip7-out.log",
        "pm_err_log_path":"/Users/x/.pm2/logs/flip7-error.log","args":[]}},
      {"name":"stockviewer","pid":37305,"pm2_env":{
        "status":"online","restart_time":0,
        "pm_cwd":"/Users/x/projects/StockViewer",
        "pm_exec_path":"/Users/x/projects/StockViewer/node_modules/.bin/next",
        "args":["start","--port","3002"]}},
      {"name":"loom-tunnel","pid":0,"pm2_env":{
        "status":"stopped","restart_time":4,
        "pm_cwd":"/Users/x/projects/Loom",
        "pm_exec_path":"/Users/x/projects/Loom/tunnel.sh","args":[]}}
    ]"#;

    #[test]
    fn reads_what_pm2_reports() {
        let processes = parse(JLIST);
        assert_eq!(processes.len(), 3);

        let flip7 = &processes[0];
        assert_eq!(flip7.name, "flip7");
        assert!(flip7.is_running());
        assert_eq!(flip7.pid, Some(29106));
        assert_eq!(flip7.restarts, 22);
        assert_eq!(flip7.cwd, Some(PathBuf::from("/Users/x/projects/CodeBoardGamer")));
    }

    #[test]
    fn a_stopped_entry_has_no_pid() {
        // PM2 reports 0, which would otherwise be signalled at.
        let stopped = parse(JLIST).into_iter().find(|p| p.name == "loom-tunnel").unwrap();
        assert_eq!(stopped.pid, None);
        assert!(!stopped.is_running());
    }

    #[test]
    fn node_env_marks_production() {
        let flip7 = parse(JLIST).into_iter().find(|p| p.name == "flip7").unwrap();
        assert!(flip7.production);
    }

    #[test]
    fn next_start_is_production_even_without_node_env() {
        // This is the case that was live on this machine: `next start` with no
        // NODE_ENV set, needing a build that a dev server had already replaced.
        let sv = parse(JLIST).into_iter().find(|p| p.name == "stockviewer").unwrap();
        assert!(sv.production);
        assert!(sv.command.contains("start --port 3002"));
    }

    #[test]
    fn a_dev_entry_is_not_production() {
        let raw = r#"[{"name":"dash","pid":1,"pm2_env":{"status":"online",
            "pm_cwd":"/x","pm_exec_path":"/x/node_modules/.bin/next","args":["dev"]}}]"#;
        assert!(!parse(raw)[0].production);
    }

    #[test]
    fn nonsense_is_not_a_panic() {
        assert!(parse("not json").is_empty());
        assert!(parse("{}").is_empty());
        assert!(parse("[]").is_empty());
    }

    #[test]
    fn only_reversible_verbs_exist() {
        // `delete` changes what starts at boot; it is not this runtime's call.
        assert!(Pm2Action::parse("delete").is_none());
        assert!(Pm2Action::parse("stop").is_some());
    }
}
