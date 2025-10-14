//! Codex AI Agent Library
//!
//! 提供基于 OpenAI Codex 的 AI 代理服务，通过 ACP (Agent Client Protocol) 协议实现
//! 与 OpenAI API 的集成。
//!
//! # Examples
//!
//! ```rust,no_run
//! use codex_acp::{CodexAgent, Config};
//! use agent_client_protocol::{Agent, InitializeRequest};
//! use tokio::sync::mpsc;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let (session_update_tx, _) = mpsc::unbounded_channel();
//!     let (client_tx, _) = mpsc::unbounded_channel();
//!
//!     let config = Config::default();
//!     let agent = CodexAgent::with_config(session_update_tx, client_tx, config, None);
//!
//!     // Initialize the agent
//!     let init_response = agent.initialize(InitializeRequest::default()).await?;
//!     println!("Agent initialized: {:?}", init_response);
//!
//!     Ok(())
//! }
//! ```

mod agent;
mod fs;

// Core agent functionality
pub use agent::{APPROVAL_PRESETS, ClientOp, CodexAgent, SessionModeLookup};

// File system bridge functionality
pub use fs::{FsBridge, run_mcp_server};

// Re-export commonly used protocol types
pub use agent_client_protocol as acp;
pub use codex_core::{
    AuthManager, CodexConversation, ConversationManager, config::Config as CodexConfig,
};

// Re-export commonly used types from codex protocol
pub use codex_protocol::ConversationId;
