//! Listening-socket enumeration.

use runtime_types::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

/// One socket bound on this machine, with the pids that hold it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBinding {
    pub port: u16,
    pub protocol: Protocol,
    /// Local bind address, e.g. `127.0.0.1` or `::`.
    pub address: String,
    /// Usually one pid; more when a socket is shared across forked workers.
    pub pids: Vec<u32>,
}

impl PortBinding {
    pub fn primary_pid(&self) -> Option<u32> {
        self.pids.first().copied()
    }

    /// True when the binding is reachable from other machines.
    pub fn is_loopback(&self) -> bool {
        self.address.starts_with("127.") || self.address == "::1"
    }
}

pub trait PortProvider: Send + Sync {
    /// Every TCP socket in the listening state.
    fn listening_ports(&self) -> Result<Vec<PortBinding>>;

    fn binding_for(&self, port: u16) -> Result<Option<PortBinding>> {
        Ok(self
            .listening_ports()?
            .into_iter()
            .find(|binding| binding.port == port))
    }

    /// Whether the port can be bound right now.
    ///
    /// Defaults to "nothing is listening on it", which is what callers mean in
    /// practice; adapters may override with an actual bind probe.
    fn is_port_free(&self, port: u16) -> Result<bool> {
        Ok(self.binding_for(port)?.is_none())
    }
}
