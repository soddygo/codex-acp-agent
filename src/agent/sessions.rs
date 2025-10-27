use agent_client_protocol as acp;
use codex_core::{NewConversation, protocol::Op};
use tokio::{sync::oneshot, task};
use tracing::{info, warn};
use uuid::Uuid;

use super::{commands, core::CodexAgent, session};

impl CodexAgent {
    /// Create a new session with the given configuration.
    ///
    /// This initializes a new Codex conversation, sets up the session state,
    /// and advertises available commands and models to the client.
    pub(super) async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse, acp::Error> {
        info!(?args, "Received new session request");
        let fs_session_id = Uuid::new_v4().to_string();

        let modes = session::session_modes_for_config(&self.config);
        let current_mode = modes
            .as_ref()
            .map(|m| m.current_mode_id.clone())
            .unwrap_or(acp::SessionModeId("auto".into()));

        let session_config = self.build_session_config(&fs_session_id, args.mcp_servers)?;

        let new_conv = self
            .conversation_manager
            .new_conversation(session_config)
            .await;

        let (conversation, conversation_id) = match new_conv {
            Ok(NewConversation {
                conversation,
                conversation_id,
                ..
            }) => (conversation, conversation_id),
            Err(e) => {
                warn!(error = %e, "Failed to create Codex conversation");
                return Err(acp::Error::into_internal_error(e));
            }
        };

        let acp_session_id = conversation_id.to_string();

        // Initialize session state from config
        self.sessions.borrow_mut().insert(
            acp_session_id.clone(),
            session::SessionState::new(
                fs_session_id.clone(),
                Some(conversation.clone()),
                &self.config,
                current_mode.clone(),
            ),
        );

        // Advertise available slash commands to the client right after
        // the session is created. Send it asynchronously to avoid racing
        // with the NewSessionResponse delivery.
        {
            let session_id = acp_session_id.clone();
            let available_commands = commands::AVAILABLE_COMMANDS.to_vec();
            let tx_updates = self.session_update_tx.clone();
            task::spawn_local(async move {
                let (tx, rx) = oneshot::channel();
                let _ = tx_updates.send((
                    acp::SessionNotification {
                        session_id: acp::SessionId(session_id.clone().into()),
                        update: acp::SessionUpdate::AvailableCommandsUpdate(
                            acp::AvailableCommandsUpdate {
                                available_commands,
                                meta: None,
                            },
                        ),
                        meta: None,
                    },
                    tx,
                ));
                let _ = rx.await;
            });
        }

        // Build models response with current model and available models from profiles
        let models = Some(acp::SessionModelState {
            current_model_id: session::current_model_id_from_config(&self.config),
            available_models: session::available_models_from_profiles(&self.config, &self.profiles),
            meta: None,
        });

        Ok(acp::NewSessionResponse {
            session_id: acp::SessionId(acp_session_id.clone().into()),
            modes,
            models,
            meta: None,
        })
    }

    /// Load an existing session and return its current state.
    pub(super) async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> Result<acp::LoadSessionResponse, acp::Error> {
        info!(?args, "Received load session request");
        let (current_mode, _current_model) = {
            let sessions = self.sessions.borrow();
            let state = sessions
                .get(args.session_id.0.as_ref())
                .ok_or_else(|| acp::Error::invalid_params().with_data("session not found"))?;
            (state.current_mode.clone(), state.current_model.clone())
        };

        // Use stored model or derive from config
        let current_model_id = if let Some(ref stored_model) = _current_model {
            // If model was set via set_session_model, it's already in "model@provider" format
            acp::ModelId(stored_model.clone().into())
        } else {
            // Otherwise, construct from current config
            session::current_model_id_from_config(&self.config)
        };

        let models = Some(acp::SessionModelState {
            current_model_id,
            available_models: session::available_models_from_profiles(&self.config, &self.profiles),
            meta: None,
        });

        Ok(acp::LoadSessionResponse {
            modes: Some(acp::SessionModeState {
                current_mode_id: current_mode,
                available_modes: session::available_modes(),
                meta: None,
            }),
            models,
            meta: None,
        })
    }

    /// Change the approval and sandbox mode for a session.
    ///
    /// This preserves the current model and effort settings while updating
    /// the approval policy and sandbox policy based on the selected preset.
    pub(super) async fn set_session_mode(
        &self,
        args: acp::SetSessionModeRequest,
    ) -> Result<acp::SetSessionModeResponse, acp::Error> {
        info!(?args, "Received set session mode request");
        let preset = session::find_preset_by_mode_id(&args.mode_id)
            .ok_or_else(|| acp::Error::invalid_params().with_data("invalid mode id"))?;

        self.apply_context_override(
            &args.session_id,
            |ctx| Op::OverrideTurnContext {
                cwd: None,
                approval_policy: Some(preset.approval),
                sandbox_policy: Some(preset.sandbox.clone()),
                model: ctx.model.clone(),
                effort: Some(ctx.effort),
                summary: None,
            },
            |state| {
                state.current_approval = preset.approval;
                state.current_sandbox = preset.sandbox.clone();
                state.current_mode = args.mode_id.clone();
            },
        )
        .await?;

        Ok(acp::SetSessionModeResponse::default())
    }

    /// Change the model for a session.
    ///
    /// This preserves the current approval and sandbox settings while updating
    /// the model and its associated reasoning effort level.
    pub(super) async fn set_session_model(
        &self,
        args: acp::SetSessionModelRequest,
    ) -> Result<acp::SetSessionModelResponse, acp::Error> {
        info!(?args, "Received set session model request");

        // Parse and validate the model_id, extracting provider, model name, and effort
        let model_ctx =
            session::parse_and_validate_model(&self.config, &self.profiles, &args.model_id)
                .ok_or_else(|| {
                    acp::Error::invalid_params()
                        .with_data("invalid model id format or provider/model not found")
                })?;

        self.apply_context_override(
            &args.session_id,
            |ctx| Op::OverrideTurnContext {
                cwd: None,
                approval_policy: Some(ctx.approval),
                sandbox_policy: Some(ctx.sandbox.clone()),
                model: Some(model_ctx.to_model_id()),
                effort: Some(model_ctx.effort),
                summary: None,
            },
            |state| {
                state.set_model(&model_ctx);
            },
        )
        .await?;

        Ok(acp::SetSessionModelResponse::default())
    }
}
