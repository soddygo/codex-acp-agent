use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{Arc, LazyLock},
};

use agent_client_protocol::{
    Error, ModelId, ModelInfo, ReadTextFileRequest, ReadTextFileResponse, RequestPermissionRequest,
    RequestPermissionResponse, SessionId, SessionMode, SessionModeId, SessionModeState,
    WriteTextFileRequest, WriteTextFileResponse,
};
use codex_common::approval_presets::{ApprovalPreset, builtin_approval_presets};
use codex_core::{
    CodexConversation,
    config::Config as CodexConfig,
    config_profile::ConfigProfile,
    protocol::{AskForApproval, SandboxPolicy, TokenUsage},
    protocol_config_types::ReasoningEffort,
};
use tokio::sync::oneshot::Sender;

/// All available approval presets used to derive ACP session modes.
static APPROVAL_PRESETS: LazyLock<Vec<ApprovalPreset>> = LazyLock::new(builtin_approval_presets);

/// Context needed for applying turn context overrides.
///
/// This encapsulates the current session state that needs to be preserved
/// or selectively overridden when changing session modes or models.
pub(super) struct SessionContext {
    pub approval: AskForApproval,
    pub sandbox: SandboxPolicy,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
}

/// Operations that require client interaction.
///
/// These operations are sent to the client handler to request permissions,
/// read files, or write files based on client capabilities.
pub enum ClientOp {
    RequestPermission {
        session_id: SessionId,
        request: RequestPermissionRequest,
        response_tx: Sender<Result<RequestPermissionResponse, Error>>,
    },
    ReadTextFile {
        session_id: SessionId,
        request: ReadTextFileRequest,
        response_tx: Sender<Result<ReadTextFileResponse, Error>>,
    },
    WriteTextFile {
        session_id: SessionId,
        request: WriteTextFileRequest,
        response_tx: Sender<Result<WriteTextFileResponse, Error>>,
    },
}

/// Compute the ACP `SessionModeState` (current + available) based on the provided Codex config.
///
/// Returns `None` if no matching preset exists for the config's approval and sandbox policies.
pub fn session_modes_for_config(config: &CodexConfig) -> Option<SessionModeState> {
    let current_mode_id = current_mode_id_for_config(config)?;

    Some(SessionModeState {
        current_mode_id,
        available_modes: available_modes(),
        meta: None,
    })
}

/// Return the current ACP session mode id by matching the preset for the provided config.
///
/// Returns `None` when no preset matches the (approval_policy, sandbox_policy) pair.
pub fn current_mode_id_for_config(config: &CodexConfig) -> Option<SessionModeId> {
    APPROVAL_PRESETS
        .iter()
        .find(|preset| {
            preset.approval == config.approval_policy && preset.sandbox == config.sandbox_policy
        })
        .map(|preset| SessionModeId(preset.id.into()))
}

/// Return the list of ACP `SessionMode` entries derived from the approval presets.
pub fn available_modes() -> Vec<SessionMode> {
    APPROVAL_PRESETS
        .iter()
        .map(|preset| SessionMode {
            id: SessionModeId(preset.id.into()),
            name: preset.label.to_owned(),
            description: Some(preset.description.to_owned()),
            meta: None,
        })
        .collect()
}

/// Find an approval preset by ACP session mode id.
pub fn find_preset_by_mode_id(mode_id: &SessionModeId) -> Option<&'static ApprovalPreset> {
    let target = mode_id.0.as_ref();
    APPROVAL_PRESETS.iter().find(|preset| preset.id == target)
}

pub fn is_read_only_mode(mode_id: &SessionModeId) -> bool {
    mode_id.0.as_ref() == "read-only"
}

/// Check if a provider is a custom (non-builtin) provider.
///
/// Builtin providers are: "openai"
/// All other providers are considered custom and may require additional authentication.
pub fn is_custom_provider(provider_id: &str) -> bool {
    !matches!(provider_id, "openai")
}

/// Model context containing provider, model name, and associated reasoning effort.
#[derive(Debug, Clone)]
pub struct ModelContext {
    pub provider_id: String,
    pub model_name: String,
    pub effort: Option<ReasoningEffort>,
}

impl ModelContext {
    /// Format as "provider@model" string.
    pub fn to_model_id(&self) -> String {
        format!("{}@{}", self.provider_id, self.model_name)
    }

    /// Create from config's current model settings.
    pub fn from_config(config: &CodexConfig) -> Self {
        Self {
            provider_id: config.model_provider_id.clone(),
            model_name: config.model.clone(),
            effort: config.model_reasoning_effort,
        }
    }
}

/// Return the current model ID in the format "provider@model".
pub fn current_model_id_from_config(config: &CodexConfig) -> ModelId {
    ModelId(ModelContext::from_config(config).to_model_id().into())
}

/// Build a `ModelInfo` for display to the client.
fn build_model_info(config: &CodexConfig, model_ctx: &ModelContext) -> Option<ModelInfo> {
    let provider_info = config.model_providers.get(&model_ctx.provider_id)?;
    let model_id = model_ctx.to_model_id();

    Some(ModelInfo {
        model_id: ModelId(model_id.into()),
        name: format!("{}@{}", provider_info.name, model_ctx.model_name),
        description: Some(format!(
            "Provider: {}, Model: {}",
            provider_info.name, model_ctx.model_name
        )),
        meta: None,
    })
}

/// Return the list of ACP `ModelInfo` entries derived from profiles.
///
/// Each ModelInfo represents a {provider}@{model} combination from the profiles configuration.
/// Only includes custom (non-builtin) providers.
pub fn available_models_from_profiles(
    config: &CodexConfig,
    profiles: &HashMap<String, ConfigProfile>,
) -> Vec<ModelInfo> {
    let mut models = Vec::new();
    let mut seen = HashSet::new();

    // Add the current model from config first (only if it's a custom provider)
    let current_ctx = ModelContext::from_config(config);
    if is_custom_provider(&current_ctx.provider_id)
        && let Some(model_info) = build_model_info(config, &current_ctx)
    {
        seen.insert(current_ctx.to_model_id());
        models.push(model_info);
    }

    // Extract unique model combinations from profiles (only custom providers)
    for profile in profiles.values() {
        if let (Some(model_name), Some(provider_id)) = (&profile.model, &profile.model_provider) {
            // Skip builtin providers
            if !is_custom_provider(provider_id) {
                continue;
            }

            let model_ctx = ModelContext {
                provider_id: provider_id.clone(),
                model_name: model_name.clone(),
                effort: profile.model_reasoning_effort,
            };
            let model_id = model_ctx.to_model_id();

            // Skip if already added
            if seen.contains(&model_id) {
                continue;
            }

            if let Some(model_info) = build_model_info(config, &model_ctx) {
                seen.insert(model_id);
                models.push(model_info);
            }
        }
    }

    models
}

/// Parse and validate a model_id in the format "provider@model".
///
/// Returns a `ModelContext` containing provider, model name, and associated effort.
/// Returns `None` if:
/// - Format is invalid
/// - Provider doesn't exist in config
/// - Model combination is not found in profiles or current config
pub fn parse_and_validate_model(
    config: &CodexConfig,
    profiles: &HashMap<String, ConfigProfile>,
    model_id: &ModelId,
) -> Option<ModelContext> {
    let id_str = model_id.0.as_ref();
    let parts: Vec<&str> = id_str.split('@').collect();

    if parts.len() != 2 {
        return None;
    }

    let provider_id = parts[0].to_string();
    let model_name = parts[1].to_string();

    // Validate that the provider exists
    if !config.model_providers.contains_key(&provider_id) {
        return None;
    }

    // Check if this is the current config model
    if provider_id == config.model_provider_id && model_name == config.model {
        return Some(ModelContext {
            provider_id,
            model_name,
            effort: config.model_reasoning_effort,
        });
    }

    // Search in profiles for matching provider@model combination
    for profile in profiles.values() {
        if profile.model.as_ref() == Some(&model_name)
            && profile.model_provider.as_ref() == Some(&provider_id)
        {
            return Some(ModelContext {
                provider_id,
                model_name,
                effort: profile.model_reasoning_effort,
            });
        }
    }

    None
}

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
    pub current_mode: SessionModeId,
    pub current_model: Option<String>,
    pub current_effort: Option<ReasoningEffort>,
    pub token_usage: Option<TokenUsage>,
}

impl SessionState {
    /// Create a new SessionState initialized from config.
    pub fn new(
        fs_session_id: String,
        conversation: Option<Arc<CodexConversation>>,
        config: &CodexConfig,
        current_mode: SessionModeId,
    ) -> Self {
        let model_ctx = ModelContext::from_config(config);
        Self {
            fs_session_id,
            conversation,
            current_approval: config.approval_policy,
            current_sandbox: config.sandbox_policy.clone(),
            current_mode,
            current_model: Some(model_ctx.to_model_id()),
            current_effort: model_ctx.effort,
            token_usage: None,
        }
    }

    /// Update the model context for this session.
    pub fn set_model(&mut self, model_ctx: &ModelContext) {
        self.current_model = Some(model_ctx.to_model_id());
        self.current_effort = model_ctx.effort;
    }
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
    pub fn current_mode(&self, session_id: &SessionId) -> Option<SessionModeId> {
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
    pub fn is_read_only(&self, session_id: &SessionId) -> bool {
        self.current_mode(session_id)
            .map(|mode| is_read_only_mode(&mode))
            .unwrap_or(false)
    }

    /// If the provided `session_id` refers to an FS session id, return the
    /// corresponding ACP session id. Otherwise, return the original ACP id.
    pub fn resolve_acp_session_id(&self, session_id: &SessionId) -> Option<SessionId> {
        let sessions = self.inner.borrow();
        if sessions.contains_key(session_id.0.as_ref()) {
            return Some(session_id.clone());
        }

        sessions.iter().find_map(|(key, state)| {
            if state.fs_session_id == session_id.0.as_ref() {
                Some(SessionId(key.clone().into()))
            } else {
                None
            }
        })
    }
}
