#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]

//! codex-acp library crate root
//!
//! This crate exposes the ACP-compatible agent as a reusable library,
//! and provides a small public surface for embedding or testing.
//!
//! Modules:
//! - `agent`: The core ACP agent implementation and its submodules.
//! - `fs`: Filesystem bridge and MCP server entrypoint used by the agent.

pub mod agent;
pub mod fs;

// Common re-exports for convenience.
pub use agent::{CodexAgent, SessionModeLookup};
pub use fs::FsBridge;

/// A small prelude with the most commonly used items when embedding the agent.
pub mod prelude {
    pub use crate::agent::{CodexAgent, SessionModeLookup};
    pub use crate::fs::FsBridge;
}
