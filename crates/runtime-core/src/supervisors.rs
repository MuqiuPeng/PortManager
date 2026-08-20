//! Who else is already keeping a service alive.
//!
//! This machine had five services under PM2 before the runtime was told about
//! any of them, and nothing in the runtime knew it. Killing one and watching
//! PM2 put it straight back is the harmless version of that gap. The damaging
//! version is taking a service over without noticing: "stop it and start it
//! here" quietly means "delete it from the supervisor that starts it at boot",
//! which is a decision about the machine that nobody asked for.
//!
//! So the runtime looks up the process tree and says who it finds. It does not
//! act on the answer — a service under another supervisor is reported, not
//! reclaimed, and taking it over stays something a person asks for once they
//! have been told what it costs.

use runtime_adapter::ProcessInfo;

/// Something other than this runtime that starts and restarts a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supervisor {
    /// Short name for display: `pm2`, `systemd`, `launchd`.
    pub kind: String,
    /// The supervising process, for a caller that wants to go and look.
    pub pid: u32,
    /// What taking this service over would actually involve.
    pub taking_over: String,
}

/// Identify whichever supervisor owns a process, if any.
///
/// Walks parents rather than guessing from the command: a dev server started
/// by PM2 looks exactly like one started from a terminal, and the only
/// difference — the thing that will restart it in a second — is above it in
/// the tree.
///
/// `argv_of` exists because the bulk process listing does not carry command
/// lines on macOS — reading them means one `sysctl` per process, which is not
/// worth paying for every process on the machine. The chain above one port is
/// a handful of processes, so they are asked for individually. Supervisors
/// that rename their own process, as PM2 does, are invisible without this.
pub fn detect(
    pid: u32,
    processes: &[ProcessInfo],
    argv_of: impl Fn(u32) -> Option<String>,
) -> Option<Supervisor> {
    let mut current = pid;
    // Bounded: a corrupt parent chain must not become an infinite walk, and
    // nothing legitimate is this deep.
    for _ in 0..32 {
        let process = processes.iter().find(|candidate| candidate.pid == current)?;
        let command = match process.command_string() {
            listed if !listed.trim().is_empty() => listed,
            _ => argv_of(current).unwrap_or_default(),
        };
        if let Some(supervisor) = identify(process, &command) {
            return Some(supervisor);
        }
        let parent = process.parent_pid?;
        if parent == 0 || parent == current {
            return None;
        }
        // Reaching init means nobody in between claimed it. On macOS that is
        // also where orphans end up, so it is not evidence of launchd owning
        // anything: `launchctl` would have to be asked, and being wrong here
        // would put a "managed elsewhere" warning on half the machine.
        if parent == 1 {
            return None;
        }
        current = parent;
    }
    None
}

fn identify(process: &ProcessInfo, command: &str) -> Option<Supervisor> {
    // `PM2 v6.0.14: God Daemon (/Users/x/.pm2)` — PM2 renames its own process,
    // so this is the process title rather than an executable path.
    if command.starts_with("PM2 ") && command.contains("God Daemon") {
        return Some(Supervisor {
            kind: "pm2".to_string(),
            pid: process.pid,
            taking_over: "`pm2 delete` removes it from PM2, which also changes what starts at boot"
                .to_string(),
        });
    }

    if process.name == "systemd" || command.starts_with("/lib/systemd/systemd") {
        return Some(Supervisor {
            kind: "systemd".to_string(),
            pid: process.pid,
            taking_over: "`systemctl disable --now` stops it and takes it out of the boot sequence"
                .to_string(),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, parent: Option<u32>, name: &str, argv: &[&str]) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid: parent,
            name: name.to_string(),
            executable: None,
            cwd: None,
            command_line: argv.iter().map(|part| part.to_string()).collect(),
            start_time_ms: 0,
        }
    }

    /// The shape found on this machine: server -> God Daemon -> launchd.
    fn under_pm2() -> Vec<ProcessInfo> {
        vec![
            process(29106, Some(4042), "node", &["node", "server.mjs"]),
            process(
                4042,
                Some(1),
                "PM2",
                &["PM2", "v6.0.14:", "God", "Daemon", "(/Users/x/.pm2)"],
            ),
            process(1, None, "launchd", &["/sbin/launchd"]),
        ]
    }

    #[test]
    fn a_service_started_by_pm2_is_reported_as_pm2s() {
        let found = detect(29106, &under_pm2(), |_| None).expect("pm2 not found");
        assert_eq!(found.kind, "pm2");
        assert_eq!(found.pid, 4042);
        assert!(found.taking_over.contains("pm2 delete"));
    }

    #[test]
    fn a_service_started_from_a_terminal_has_no_supervisor() {
        let processes = vec![
            process(500, Some(400), "node", &["node", "server.mjs"]),
            process(400, Some(1), "zsh", &["-zsh"]),
            process(1, None, "launchd", &["/sbin/launchd"]),
        ];
        assert!(detect(500, &processes, |_| None).is_none());
    }

    #[test]
    fn an_orphan_is_not_mistaken_for_a_launchd_job() {
        // Its launcher exited, so it hangs off init — which on macOS says
        // nothing at all about who is responsible for it.
        let processes = vec![
            process(500, Some(1), "node", &["node", "server.mjs"]),
            process(1, None, "launchd", &["/sbin/launchd"]),
        ];
        assert!(detect(500, &processes, |_| None).is_none());
    }

    #[test]
    fn a_supervisor_that_renames_itself_is_found_through_argv() {
        // macOS does not report command lines in the bulk listing, and PM2
        // rewrites its process title, so the listing alone shows only "node".
        let processes = vec![
            process(29106, Some(4042), "node", &["node", "server.mjs"]),
            process(4042, Some(1), "node", &[]),
            process(1, None, "launchd", &["/sbin/launchd"]),
        ];
        let argv = |pid: u32| {
            (pid == 4042).then(|| "PM2 v6.0.14: God Daemon (/Users/x/.pm2)".to_string())
        };
        assert_eq!(detect(29106, &processes, argv).unwrap().kind, "pm2");
    }

    #[test]
    fn a_parent_chain_that_loops_terminates() {
        let processes = vec![
            process(10, Some(11), "a", &["a"]),
            process(11, Some(10), "b", &["b"]),
        ];
        assert!(detect(10, &processes, |_| None).is_none());
    }

    #[test]
    fn a_missing_parent_is_not_an_error() {
        let processes = vec![process(10, Some(999), "a", &["a"])];
        assert!(detect(10, &processes, |_| None).is_none());
    }
}
