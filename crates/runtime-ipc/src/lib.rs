//! IPC between the runtime daemon and its clients.
//!
//! The daemon is the single authority for runtime state; the CLI, the desktop
//! app and the MCP server are all clients of this protocol. That is what keeps
//! a service started from the terminal visible to an agent, and vice versa.

pub mod client;
pub mod protocol;
pub mod transport;

pub use client::Client;
pub use protocol::{Frame, Request, ResponseBody, PROTOCOL_VERSION};
pub use transport::{connect, Connection, Listener};
