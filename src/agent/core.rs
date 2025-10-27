use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, RwLock},
};

use agent_client_protocol as acp;
use codex_core::{
    AuthManager, CodexConversation, ConversationManager, config::Config as CodexConfig,
    config_profile::ConfigProfile, protocol::Op,
};
use codex_protocol::ConversationId;
use tokio::sync::{mpsc, oneshot};

use crate::fs::FsBridge;

use super::{context::SessionContext, session::SessionState};

/// The main ACP agent implementation.
///
/// This struct manages sessions, conversations, and coordinates between
/// the client, Codex conversation engine, and filesystem bridge.
pub struct CodexAgent {
    pub(super) session_update_tx:
        mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
    pub(super) sessions: Rc<RefCell<HashMap<String, SessionState>>>,
    pub(super) config: CodexConfig,
    pub(super) profiles: HashMap<String, ConfigProfile>,
    pub(super) conversation_manager: ConversationManager,
    pub(super) auth_manager: Arc<RwLock<Arc<AuthManager>>>,
    pub(super) client_tx: mpsc::UnboundedSender<super::context::ClientOp>,
    pub(super) client_capabilities: RefCell<acp::ClientCapabilities>,
    pub(super) fs_bridge: Option<Arc<FsBridge>>,
}

impl CodexAgent {
    /// Create a new CodexAgent with the provided configuration.
    pub fn with_config(
        session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, oneshot::Sender<()>)>,
        client_tx: mpsc::UnboundedSender<super::context::ClientOp>,
        config: CodexConfig,
        profiles: HashMap<String, ConfigProfile>,
        fs_bridge: Option<Arc<FsBridge>>,
    ) -> Self {
        let auth = AuthManager::shared(config.codex_home.clone(), false);
        let conversation_manager =
            ConversationManager::new(auth.clone(), codex_core::protocol::SessionSource::Unknown);

        Self {
            session_update_tx,
            sessions: Rc::new(RefCell::new(HashMap::new())),
            config,
            profiles,
            conversation_manager,
            auth_manager: Arc::new(RwLock::new(auth)),
            client_tx,
            client_capabilities: RefCell::new(Default::default()),
            fs_bridge,
        }
    }

    /// Get or load the conversation for a session.
    ///
    /// This will reuse a cached conversation if available, otherwise load it
    /// from the conversation manager and cache it in the session state.
    pub(super) async fn get_conversation(
        &self,
        session_id: &acp::SessionId,
    ) -> Result<Arc<CodexConversation>, acp::Error> {
        let conversation_opt = {
            let sessions = self.sessions.borrow();
            let state = sessions
                .get(session_id.0.as_ref())
                .ok_or_else(|| acp::Error::invalid_params().with_data("session not found"))?;
            state.conversation.clone()
        };

        if let Some(conversation) = conversation_opt {
            return Ok(conversation);
        }

        let conversation_id = ConversationId::from_string(session_id.0.as_ref())
            .map_err(|e| acp::Error::from(anyhow::anyhow!(e)))?;

        let conversation = self
            .conversation_manager
            .get_conversation(conversation_id)
            .await
            .map_err(|e| acp::Error::from(anyhow::anyhow!(e)))?;

        self.with_session_state_mut(session_id, |state| {
            state.conversation = Some(conversation.clone());
        });
        Ok(conversation)
    }

    /// Send a session update notification to the client.
    pub async fn send_session_update(
        &self,
        session_id: &acp::SessionId,
        update: acp::SessionUpdate,
    ) -> Result<(), acp::Error> {
        let (tx, rx) = oneshot::channel();
        let notification = acp::SessionNotification {
            session_id: session_id.clone(),
            update,
            meta: None,
        };
        self.session_update_tx
            .send((notification, tx))
            .map_err(acp::Error::into_internal_error)?;
        rx.await.map_err(acp::Error::into_internal_error)
    }

    /// Send a message content chunk to the client.
    pub async fn send_message_chunk(
        &self,
        session_id: &acp::SessionId,
        content: acp::ContentBlock,
    ) -> Result<(), acp::Error> {
        let chunk = acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk {
            content,
            meta: None,
        });
        self.send_session_update(session_id, chunk).await
    }

    /// Send a thought content chunk to the client.
    pub async fn send_thought_chunk(
        &self,
        session_id: &acp::SessionId,
        content: acp::ContentBlock,
    ) -> Result<(), acp::Error> {
        let chunk = acp::SessionUpdate::AgentThoughtChunk(acp::ContentChunk {
            content,
            meta: None,
        });
        self.send_session_update(session_id, chunk).await
    }

    /// Mutate session state with a function.
    ///
    /// Returns `None` if the session is not found.
    pub(super) fn with_session_state_mut<R, F>(
        &self,
        session_id: &acp::SessionId,
        f: F,
    ) -> Option<R>
    where
        F: FnOnce(&mut SessionState) -> R,
    {
        let mut sessions = self.sessions.borrow_mut();
        let key: &str = session_id.0.as_ref();
        sessions.get_mut(key).map(f)
    }

    /// Helper to apply turn context overrides while preserving session state.
    ///
    /// This encapsulates the common pattern of:
    /// 1. Reading current session state to get context (approval, sandbox, model, effort)
    /// 2. Applying an `Op::OverrideTurnContext` with selective overrides
    /// 3. Updating session state with the new values
    ///
    /// Returns an error if the session is not found or if the operation fails.
    pub(super) async fn apply_context_override<F>(
        &self,
        session_id: &acp::SessionId,
        build_override: F,
        update_state: impl FnOnce(&mut SessionState),
    ) -> Result<(), acp::Error>
    where
        F: FnOnce(&SessionContext) -> Op,
    {
        // Read current session state to build context
        let ctx = {
            let sessions = self.sessions.borrow();
            let state = sessions
                .get(session_id.0.as_ref())
                .ok_or_else(|| acp::Error::invalid_params().with_data("session not found"))?;
            SessionContext {
                approval: state.current_approval,
                sandbox: state.current_sandbox.clone(),
                model: state.current_model.clone(),
                effort: state.current_effort,
            }
        };

        // Build and submit the override operation
        let op = build_override(&ctx);
        self.get_conversation(session_id)
            .await?
            .submit(op)
            .await
            .map_err(|e| acp::Error::from(anyhow::anyhow!(e)))?;

        // Update session state
        self.with_session_state_mut(session_id, update_state);

        Ok(())
    }

    /// Check if the client supports terminal operations.
    pub(super) fn support_terminal(&self) -> bool {
        self.client_capabilities.borrow().terminal
    }
}
