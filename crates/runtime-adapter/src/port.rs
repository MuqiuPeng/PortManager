//! Listening-socket enumeration.

pub use runtime_types::Protocol;
use runtime_types::Result;
use serde::{Deserialize, Serialize};


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
    /// Every socket accepting traffic: TCP in the listening state, and UDP,
    /// which has no states — a bound UDP socket is already receiving.
    fn listening_ports(&self) -> Result<Vec<PortBinding>>;

    /// The binding a user means when they name a port.
    ///
    /// TCP wins when both protocols hold the same number, because a dev server
    /// is what the question is about; a UDP socket sharing the port would
    /// otherwise shadow it and report the wrong owner.
    fn binding_for(&self, port: u16) -> Result<Option<PortBinding>> {
        let bindings = self.listening_ports()?;
        Ok(bindings
            .iter()
            .find(|binding| binding.port == port && binding.protocol == Protocol::Tcp)
            .or_else(|| bindings.iter().find(|binding| binding.port == port))
            .cloned())
    }

    /// Whether the port can be bound right now.
    ///
    /// Defaults to "nothing is listening on it", which is what callers mean in
    /// practice; adapters may override with an actual bind probe.
    fn is_port_free(&self, port: u16) -> Result<bool> {
        Ok(self.binding_for(port)?.is_none())
    }
}
