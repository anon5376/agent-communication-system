//! Rust port of the agent-communication-system coordination core.
//!
//! Same SQLite file, same schema, same token files and inbox signal files as the
//! TypeScript implementation in `src/` — a bus.db opened here is interchangeable
//! with one opened by `qagent` on Node.

pub mod adapters;
pub mod bus;
pub mod cli;
pub mod config;
pub mod db;
pub mod error;
pub mod identity;
pub mod import;
pub mod mcp;
pub mod mcp_config;
pub mod render;
pub mod supervisor;
pub mod types;
pub mod wait;
pub mod watcher;

pub use bus::Bus;
pub use error::{BusError, Code, Result};
pub use identity::Identity;
