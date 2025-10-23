use std::sync::LazyLock;

use agent_client_protocol as acp;
use codex_common::approval_presets::{ApprovalPreset, builtin_approval_presets};
use codex_core::config::Config as CodexConfig;

/// All available approval presets used to derive ACP session modes.
pub static APPROVAL_PRESETS: LazyLock<Vec<ApprovalPreset>> =
    LazyLock::new(builtin_approval_presets);

/// Compute the ACP `SessionModeState` (current + available) based on the provided Codex config.
///
/// Returns `None` if no matching preset exists for the config's approval and sandbox policies.
pub fn session_modes_for_config(config: &CodexConfig) -> Option<acp::SessionModeState> {
    let current_mode_id = current_mode_id_for_config(config)?;

    Some(acp::SessionModeState {
        current_mode_id,
        available_modes: available_modes(),
        meta: None,
    })
}

/// Return the current ACP session mode id by matching the preset for the provided config.
///
/// Returns `None` when no preset matches the (approval_policy, sandbox_policy) pair.
pub fn current_mode_id_for_config(config: &CodexConfig) -> Option<acp::SessionModeId> {
    APPROVAL_PRESETS
        .iter()
        .find(|preset| {
            preset.approval == config.approval_policy && preset.sandbox == config.sandbox_policy
        })
        .map(|preset| acp::SessionModeId(preset.id.into()))
}

/// Return the list of ACP `SessionMode` entries derived from the approval presets.
pub fn available_modes() -> Vec<acp::SessionMode> {
    APPROVAL_PRESETS
        .iter()
        .map(|preset| acp::SessionMode {
            id: acp::SessionModeId(preset.id.into()),
            name: preset.label.to_owned(),
            description: Some(preset.description.to_owned()),
            meta: None,
        })
        .collect()
}

/// Find an approval preset by ACP session mode id.
pub fn find_preset_by_mode_id(mode_id: &acp::SessionModeId) -> Option<&'static ApprovalPreset> {
    let target = mode_id.0.as_ref();
    APPROVAL_PRESETS.iter().find(|preset| preset.id == target)
}

pub fn is_read_only_mode(mode_id: &acp::SessionModeId) -> bool {
    mode_id.0.as_ref() == "read-only"
}
