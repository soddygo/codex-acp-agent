use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

use agent_client_protocol as acp;
use codex_core::{
    CodexConversation,
    protocol::{AskForApproval, SandboxPolicy, TokenUsage},
};

/// Per-session state shared across the agent runtime.
///
/// Notes:
/// - `fs_session_id` is the session id used by the FS bridge. It may differ
///   from the ACP session id (which is the key in the `sessions` map).
/// - `conversation` is lazily loaded on demand; `None` until first use.
/// - Reasoning text is aggregated across streaming events.
#[derive(Clone)]
pub struct SessionState {
    pub fs_session_id: String,
    pub conversation: Option<Arc<CodexConversation>>,
    pub current_approval: AskForApproval,
    pub current_sandbox: SandboxPolicy,
    pub current_mode: acp::SessionModeId,
    pub token_usage: Option<TokenUsage>,
}

/// Read-only helper for looking up session-mode related info.
///
/// This type intentionally only exposes query methods to keep mutation
/// centralized inside the agent. The inner store is shared via `Rc<RefCell<...>>`
/// because the agent runs on the current-thread runtime.
#[derive(Clone)]
pub struct SessionModeLookup {
    // crate-visible so the agent can construct directly without extra glue
    pub(crate) inner: Rc<RefCell<HashMap<String, SessionState>>>,
}

impl SessionModeLookup {
    /// Create a new lookup wrapper from an existing shared session store.
    /// Return the current mode for the given ACP session id.
    ///
    /// This will also resolve when the provided id matches an FS session id
    /// held inside a `SessionState`.
    pub fn current_mode(&self, session_id: &acp::SessionId) -> Option<acp::SessionModeId> {
        let sessions = self.inner.borrow();
        if let Some(state) = sessions.get(session_id.0.as_ref()) {
            return Some(state.current_mode.clone());
        }

        sessions
            .values()
            .find(|state| state.fs_session_id == session_id.0.as_ref())
            .map(|state| state.current_mode.clone())
    }

    /// Whether the resolved session is currently read-only.
    pub fn is_read_only(&self, session_id: &acp::SessionId) -> bool {
        self.current_mode(session_id)
            .map(|mode| crate::agent::modes::is_read_only_mode(&mode))
            .unwrap_or(false)
    }

    /// If the provided `session_id` refers to an FS session id, return the
    /// corresponding ACP session id. Otherwise, return the original ACP id.
    pub fn resolve_acp_session_id(&self, session_id: &acp::SessionId) -> Option<acp::SessionId> {
        let sessions = self.inner.borrow();
        if sessions.contains_key(session_id.0.as_ref()) {
            return Some(session_id.clone());
        }

        sessions.iter().find_map(|(key, state)| {
            if state.fs_session_id == session_id.0.as_ref() {
                Some(acp::SessionId(key.clone().into()))
            } else {
                None
            }
        })
    }
}
