//! Service launches the runtime was told about but did not perform.
//!
//! An agent or a terminal starting `pnpm dev` directly is the normal case, not
//! a failure to be prevented. What the runtime lacks in that case is not
//! control but *knowledge*: it can see a port appear and walk back to a pid,
//! yet the one thing it cannot recover from a running process is the command
//! that would start it again. Inference fills that gap with a guess, and a
//! guess here is expensive — a project whose `dev` script and `start` script
//! write to the same build directory is corrupted by restarting it under the
//! wrong one.
//!
//! So the runtime asks to be *told*, and does nothing else. A launch is
//! recorded verbatim before it runs; the port scan that follows decides whether
//! anything came of it. Nothing is rewritten, nothing is intercepted, and a
//! recording that never produces a listener simply expires. The command the
//! caller typed is the command that runs.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use runtime_types::{LaunchObservation, LaunchState, ServiceId, StartedBy};

/// How long a recording waits for a port before it is discarded.
///
/// Generous, because the gap being measured is a dev server's entire startup:
/// a cold Next build or a Postgres that initialises a data directory can take
/// most of a minute. The cost of waiting too long is a stale row nobody sees;
/// the cost of waiting too little is the whole feature failing on exactly the
/// slow-starting services that are hardest to reason about.
const TTL: i64 = 90;

/// Recorded launches, newest last.
///
/// Deliberately in memory. An observation is meaningful only while the process
/// it describes might still be starting, so surviving a daemon restart would
/// preserve nothing worth having — and a launch whose daemon died mid-startup
/// is exactly the case where a stale recording would bind to the wrong thing.
#[derive(Debug, Default)]
pub struct LaunchLog {
    entries: Mutex<Vec<LaunchObservation>>,
}

impl LaunchLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Note a launch that is about to happen.
    pub fn record(
        &self,
        command: String,
        cwd: PathBuf,
        source: StartedBy,
        session: Option<String>,
    ) -> LaunchObservation {
        let observation = LaunchObservation {
            id: format!("{}-{}", Utc::now().timestamp_millis(), short_hash(&command)),
            command,
            // The caller's directory and the kernel's are not always spelled the
            // same: on macOS a shell in `/tmp/x` is reported by the process
            // table as `/private/tmp/x`, and comparing the two as text would
            // quietly fail to match every launch under a symlinked path.
            cwd: crate::strip_verbatim(std::fs::canonicalize(&cwd).unwrap_or(cwd)),
            source,
            session,
            observed_at: Utc::now(),
            state: LaunchState::Pending,
            port: None,
            pid: None,
            service_id: None,
        };

        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|entry| !expired(entry));
            entries.push(observation.clone());
        }
        observation
    }

    /// Recordings still waiting for a port, newest first.
    ///
    /// Newest first because a directory can be launched into twice while the
    /// first is still starting, and the later command is the better
    /// explanation for a port that appears after it.
    pub fn pending(&self) -> Vec<LaunchObservation> {
        let Ok(entries) = self.entries.lock() else {
            return Vec::new();
        };
        let mut pending: Vec<_> = entries
            .iter()
            .filter(|entry| entry.state == LaunchState::Pending && !expired(entry))
            .cloned()
            .collect();
        pending.reverse();
        pending
    }

    /// Everything still held, newest first, including what has been bound.
    pub fn all(&self) -> Vec<LaunchObservation> {
        let Ok(entries) = self.entries.lock() else {
            return Vec::new();
        };
        let mut all: Vec<_> = entries.iter().filter(|e| !expired(e)).cloned().collect();
        all.reverse();
        all
    }

    /// Attach a recording to what it turned out to start.
    pub fn bind(&self, id: &str, port: u16, pid: u32, service_id: Option<ServiceId>) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        for entry in entries.iter_mut() {
            if entry.id == id {
                entry.state = LaunchState::Bound;
                entry.port = Some(port);
                entry.pid = Some(pid);
                entry.service_id = service_id;
                return;
            }
        }
    }

    /// Roots of process trees the runtime has direct evidence for.
    ///
    /// A bound recording is not a guess: something asked for this command in
    /// this directory, and this pid answered on a port moments later. That is
    /// enough to say the pid's children belong to it too.
    pub fn bound_pids(&self) -> Vec<u32> {
        let Ok(entries) = self.entries.lock() else {
            return Vec::new();
        };
        entries
            .iter()
            .filter(|entry| !expired(entry))
            .filter_map(|entry| entry.pid)
            .collect()
    }

    pub fn sweep(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|entry| !expired(entry));
        }
    }
}

fn expired(entry: &LaunchObservation) -> bool {
    // A bound recording outlives the window: it is the evidence behind a
    // service definition, and dropping it would make the runtime forget why it
    // believes what it believes.
    if entry.state == LaunchState::Bound {
        return Utc::now() - entry.observed_at > ChronoDuration::seconds(TTL * 8);
    }
    Utc::now() - entry.observed_at > ChronoDuration::seconds(TTL)
}

/// Whether a recording could explain a process listening on a port.
///
/// The command is only half the test and the weaker half. A rule that decides
/// on the command alone has to answer "is `python app.py` a server?", which is
/// unanswerable — so this asks only whether the *directory* lines up, and
/// leaves the real question to whether a port actually appeared.
pub fn explains(entry: &LaunchObservation, process_cwd: Option<&Path>, started: DateTime<Utc>) -> bool {
    // Started before it was announced, so something else started it. A little
    // slack: the recording and the process clock are not the same clock, and
    // the hook fires a moment before the shell does.
    if started < entry.observed_at - ChronoDuration::seconds(2) {
        return false;
    }

    let Some(process_cwd) = process_cwd else {
        // Without a directory there is nothing to match on, and "it appeared at
        // roughly the right time" is how unrelated services get adopted.
        return false;
    };

    // `cd frontend && pnpm dev` runs deeper than the directory it was announced
    // from, and a package in a monorepo runs deeper still.
    let process_cwd = crate::strip_verbatim(
        std::fs::canonicalize(process_cwd).unwrap_or_else(|_| process_cwd.to_path_buf()),
    );
    process_cwd.starts_with(&entry.cwd)
}

/// Variables that decide which *mode* a service runs in.
///
/// Deliberately a short list of switches, not an attempt at the environment. A
/// service's environment holds database URLs and API keys; copying it into a
/// registry writes those to disk, and printing it puts them in a transcript.
/// What is actually missing when a service is adopted is narrower than that
/// and much more dangerous to get wrong: `node server.mjs` is the development
/// server or the production one depending on `NODE_ENV` alone, and the two
/// write to the same build directory. Starting the wrong one leaves the
/// project unable to boot.
pub const MODE_VARIABLES: &[&str] = &[
    "NODE_ENV",
    "RAILS_ENV",
    "RACK_ENV",
    "APP_ENV",
    "FLASK_ENV",
    "FLASK_DEBUG",
    "DJANGO_SETTINGS_MODULE",
    "ASPNETCORE_ENVIRONMENT",
    "GIN_MODE",
    "DENO_ENV",
    "MIX_ENV",
];

/// Commands not worth remembering.
///
/// Not a judgement about what a service is — that question is settled by
/// whether a port appears. This only keeps the log from filling with the
/// hundreds of `git status` calls between one `pnpm dev` and the next, and
/// every entry it is wrong about costs nothing but a row that expires.
pub fn is_instantaneous(command: &str) -> bool {
    const NEVER_SERVES: &[&str] = &[
        "awk", "basename", "cat", "cd", "chmod", "cp", "date", "diff", "dirname", "echo", "env",
        "export", "false", "find", "grep", "head", "hostname", "id", "ls", "mkdir", "mv", "printf",
        "pwd", "readlink", "rg", "rm", "sed", "sleep", "sort", "stat", "tail", "touch", "tr",
        "true", "uname", "uniq", "wc", "which", "whoami",
    ];

    let head = command.trim_start();
    // Only the simple case: anything with a pipe, a redirect or a chain may end
    // in something long-running, and guessing which end matters is the kind of
    // string surgery this design exists to avoid.
    if head.contains("&&") || head.contains("||") || head.contains('|') || head.contains(';') {
        return false;
    }

    let Some(first) = head.split_whitespace().next() else {
        return true;
    };
    let first = first.rsplit('/').next().unwrap_or(first);
    NEVER_SERVES.contains(&first) || first == "git"
}

fn short_hash(value: &str) -> String {
    // FNV-1a, for a stable id without a dependency.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(command: &str, cwd: &str) -> LaunchObservation {
        LaunchLog::new().record(
            command.to_string(),
            PathBuf::from(cwd),
            StartedBy::ClaudeCode,
            None,
        )
    }

    #[test]
    fn a_launch_explains_a_port_opened_beneath_it() {
        let entry = observation("pnpm dev", "/repo");
        assert!(explains(
            &entry,
            Some(Path::new("/repo/packages/api")),
            Utc::now()
        ));
    }

    // Symlinks need a privilege on Windows that a test process does not have,
    // and the mismatch this guards against is a Unix one to begin with.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_still_matches() {
        // macOS reports `/tmp/x` as `/private/tmp/x`, and text comparison of
        // the two misses every launch made from a symlinked path.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("link");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let entry = LaunchLog::new().record(
            "pnpm dev".to_string(),
            link,
            StartedBy::ClaudeCode,
            None,
        );
        assert!(explains(&entry, Some(&real), Utc::now()));
    }

    #[test]
    fn a_launch_does_not_explain_a_port_outside_its_directory() {
        let entry = observation("pnpm dev", "/repo");
        assert!(!explains(&entry, Some(Path::new("/elsewhere")), Utc::now()));
    }

    #[test]
    fn a_launch_does_not_explain_a_process_older_than_itself() {
        let entry = observation("pnpm dev", "/repo");
        let before = Utc::now() - ChronoDuration::seconds(30);
        assert!(!explains(&entry, Some(Path::new("/repo")), before));
    }

    #[test]
    fn a_process_with_no_directory_is_never_claimed() {
        let entry = observation("pnpm dev", "/repo");
        assert!(!explains(&entry, None, Utc::now()));
    }

    #[test]
    fn the_log_keeps_the_command_exactly_as_given() {
        // The whole point: `dev` and `start` can destroy each other's build
        // output, so the recorded command must be the one that ran.
        let log = LaunchLog::new();
        log.record(
            "NODE_ENV=production node server.mjs".to_string(),
            PathBuf::from("/repo"),
            StartedBy::ClaudeCode,
            None,
        );
        assert_eq!(log.pending()[0].command, "NODE_ENV=production node server.mjs");
    }

    #[test]
    fn the_newest_recording_is_offered_first() {
        let log = LaunchLog::new();
        log.record("first".into(), PathBuf::from("/repo"), StartedBy::Cli, None);
        log.record("second".into(), PathBuf::from("/repo"), StartedBy::Cli, None);
        assert_eq!(log.pending()[0].command, "second");
    }

    #[test]
    fn binding_takes_a_recording_out_of_the_running() {
        let log = LaunchLog::new();
        let entry = log.record("pnpm dev".into(), PathBuf::from("/repo"), StartedBy::Cli, None);
        log.bind(&entry.id, 3000, 42, None);
        assert!(log.pending().is_empty());
        assert_eq!(log.bound_pids(), vec![42]);
    }

    #[test]
    fn everyday_shell_noise_is_not_recorded() {
        for command in ["ls -la", "git status", "cat package.json", "/bin/echo hi"] {
            assert!(is_instantaneous(command), "{command}");
        }
    }

    #[test]
    fn anything_that_might_serve_is_recorded() {
        for command in [
            "pnpm dev",
            "python app.py",
            "cargo run",
            "cd frontend && pnpm dev",
            "git log && npm start",
        ] {
            assert!(!is_instantaneous(command), "{command}");
        }
    }
}
