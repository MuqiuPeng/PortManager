//! A portable adapter built on `sysinfo` and `netstat2`.
//!
//! It is correct on every supported platform and is what a new platform gets
//! before a native adapter exists. Native adapters exist to be faster and to
//! reach things these crates cannot (process groups, job objects), not to make
//! the runtime work at all.

use std::path::PathBuf;
use std::process::Command;

use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState};
use runtime_types::{Result, RuntimeError};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};

use crate::port::{PortBinding, PortProvider, Protocol};
use crate::process::{ProcessIdentity, ProcessInfo, ProcessProvider, TerminationMode};
use crate::spawn::SpawnProvider;

#[derive(Debug, Default)]
pub struct GenericProcessProvider;

impl GenericProcessProvider {
    pub fn new() -> Self {
        Self
    }

    /// Exactly the fields [`Self::convert`] reads.
    ///
    /// `refresh_processes` would ask for memory, cpu and disk — which nothing
    /// here uses — and not for cwd or argv, which are the two fields
    /// port-to-project resolution is built on.
    fn refresh_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::new()
            .with_cwd(UpdateKind::Always)
            .with_cmd(UpdateKind::Always)
            .with_exe(UpdateKind::OnlyIfNotSet)
    }

    fn snapshot() -> System {
        let mut system = System::new();
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, Self::refresh_kind());
        system
    }

    /// One process rather than the whole table.
    ///
    /// `process_info` is called once per listening socket, and reading every
    /// process to answer for one means opening a handle to each of them —
    /// hundreds of times over, for a question about a single pid.
    fn snapshot_of(pid: Pid) -> System {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            Self::refresh_kind(),
        );
        system
    }

    fn convert(pid: Pid, process: &sysinfo::Process) -> ProcessInfo {
        ProcessInfo {
            pid: pid.as_u32(),
            parent_pid: process.parent().map(|p| p.as_u32()),
            name: process.name().to_string_lossy().to_string(),
            executable: process.exe().map(PathBuf::from),
            cwd: process.cwd().map(PathBuf::from),
            command_line: process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect(),
            // sysinfo reports whole seconds; ProcessIdentity compares with a
            // tolerance wide enough to absorb the lost precision.
            start_time_ms: (process.start_time() as i64) * 1000,
        }
    }
}

impl ProcessProvider for GenericProcessProvider {
    fn list_processes(&self) -> Result<Vec<ProcessInfo>> {
        let system = Self::snapshot();
        Ok(system
            .processes()
            .iter()
            .map(|(pid, process)| Self::convert(*pid, process))
            .collect())
    }

    fn process_info(&self, pid: u32) -> Result<Option<ProcessInfo>> {
        let key = Pid::from_u32(pid);
        let system = Self::snapshot_of(key);
        Ok(system.process(key).map(|process| Self::convert(key, process)))
    }

    fn terminate_tree(&self, identity: &ProcessIdentity, mode: TerminationMode) -> Result<bool> {
        let system = Self::snapshot();
        let key = Pid::from_u32(identity.pid);
        let Some(root) = system.process(key) else {
            return Ok(false);
        };
        if !Self::convert(key, root).identity().matches(identity) {
            // The pid was recycled; the process we launched is already gone.
            return Ok(false);
        }

        let mut targets = self.descendants(identity.pid)?;
        targets.push(identity.pid);

        let signal = match mode {
            TerminationMode::Graceful => Signal::Term,
            TerminationMode::Forceful => Signal::Kill,
        };
        // Children first, so a supervisor cannot respawn a worker we just
        // signalled while we are still walking the tree.
        for pid in targets {
            if let Some(process) = system.process(Pid::from_u32(pid)) {
                if process.kill_with(signal).is_none() {
                    process.kill();
                }
            }
        }
        Ok(true)
    }
}

#[derive(Debug, Default)]
pub struct GenericPortProvider;

impl GenericPortProvider {
    pub fn new() -> Self {
        Self
    }
}

impl PortProvider for GenericPortProvider {
    fn listening_ports(&self) -> Result<Vec<PortBinding>> {
        let sockets = netstat2::get_sockets_info(
            AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
            ProtocolFlags::TCP,
        )
        .map_err(|err| RuntimeError::io(format!("failed to read socket table: {err}")))?;

        let mut bindings: Vec<PortBinding> = Vec::new();
        for socket in sockets {
            let ProtocolSocketInfo::Tcp(tcp) = socket.protocol_socket_info else {
                continue;
            };
            if tcp.state != TcpState::Listen {
                continue;
            }
            // A forking server binds once and shares the socket, so the same
            // port legitimately appears under several pids.
            match bindings.iter_mut().find(|existing| {
                existing.port == tcp.local_port && existing.address == tcp.local_addr.to_string()
            }) {
                Some(existing) => {
                    for pid in socket.associated_pids {
                        if !existing.pids.contains(&pid) {
                            existing.pids.push(pid);
                        }
                    }
                }
                None => bindings.push(PortBinding {
                    port: tcp.local_port,
                    protocol: Protocol::Tcp,
                    address: tcp.local_addr.to_string(),
                    pids: socket.associated_pids,
                }),
            }
        }
        bindings.sort_by_key(|binding| binding.port);
        Ok(bindings)
    }

    fn is_port_free(&self, port: u16) -> Result<bool> {
        // Bind-probe rather than trusting the socket table: it also catches
        // ports held in a state the table does not report as LISTEN.
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => {
                drop(listener);
                Ok(self.binding_for(port)?.is_none())
            }
            Err(_) => Ok(false),
        }
    }
}

#[derive(Debug, Default)]
pub struct GenericSpawnProvider;

impl GenericSpawnProvider {
    pub fn new() -> Self {
        Self
    }
}

impl SpawnProvider for GenericSpawnProvider {
    fn shell(&self) -> (String, Vec<String>) {
        if cfg!(windows) {
            ("cmd.exe".to_string(), vec!["/C".to_string()])
        } else {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            (shell, vec!["-lc".to_string()])
        }
    }

    fn prepare(&self, _command: &mut Command) -> Result<()> {
        // No process-group isolation available portably; native adapters add it.
        Ok(())
    }
}
