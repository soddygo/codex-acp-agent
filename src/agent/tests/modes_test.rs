#![cfg(test)]

use codex_core::config::Config as CodexConfig;

#[test]
fn is_read_only_mode_basic() {
    assert!(crate::agent::modes::is_read_only_mode(
        &agent_client_protocol::SessionModeId("read-only".into())
    ));
    assert!(!crate::agent::modes::is_read_only_mode(
        &agent_client_protocol::SessionModeId("not-read-only".into())
    ));
}

#[test]
fn current_mode_id_for_config_matches_read_only_if_available() {
    if let Some(ro) = crate::agent::modes::APPROVAL_PRESETS
        .iter()
        .find(|p| p.id == "read-only")
    {
        let mut cfg = CodexConfig::default();
        cfg.approval_policy = ro.approval;
        cfg.sandbox_policy = ro.sandbox.clone();
        let mid = crate::agent::modes::current_mode_id_for_config(&cfg);
        assert_eq!(mid.as_ref().map(|m| m.0.as_ref()), Some("read-only"));
    }
}

#[test]
fn current_mode_id_for_config_none_on_mismatch() {
    let presets = &*crate::agent::modes::APPROVAL_PRESETS;
    if presets.len() >= 2 {
        let a = &presets[0];
        let mut b = &presets[1];
        if a.id == b.id {
            if let Some(other) = presets.iter().find(|p| p.id != a.id) {
                b = other;
            }
        }
        let mut cfg = CodexConfig::default();
        cfg.approval_policy = a.approval;
        cfg.sandbox_policy = b.sandbox.clone();
        let mid = crate::agent::modes::current_mode_id_for_config(&cfg);
        assert!(
            mid.is_none(),
            "expected None for mismatched approval/sandbox"
        );
    }
}

use std::collections::HashSet;

use agent_client_protocol as acp;

use crate::agent::modes;

/// Ensure available_modes aligns with APPROVAL_PRESETS (1:1 by id)
#[test]
fn available_modes_match_presets() {
    let presets_len = modes::APPROVAL_PRESETS.len();
    assert!(presets_len > 0, "approval presets must not be empty");

    let available = modes::available_modes();
    assert_eq!(
        available.len(),
        presets_len,
        "available_modes length should match presets length"
    );

    // Compare IDs as sets for equality
    let preset_ids: HashSet<String> = modes::APPROVAL_PRESETS
        .iter()
        .map(|p| p.id.to_string())
        .collect();

    let mode_ids: HashSet<String> = available
        .iter()
        .map(|m| m.id.0.as_ref().to_string())
        .collect();

    assert_eq!(
        preset_ids, mode_ids,
        "available mode IDs must equal preset IDs"
    );

    // Sanity checks on naming/description presence
    for m in available {
        assert!(
            !m.name.trim().is_empty(),
            "mode name should not be empty for id={}",
            m.id.0.as_ref()
        );
        // descriptions are optional, but when present shouldn't be empty
        if let Some(desc) = m.description {
            assert!(
                !desc.trim().is_empty(),
                "mode description should not be empty for id={}",
                m.id.0.as_ref()
            );
        }
    }
}

/// Ensure find_preset_by_mode_id returns the matching preset for each available mode.
#[test]
fn find_preset_roundtrip() {
    for mode in modes::available_modes() {
        let found = modes::find_preset_by_mode_id(&mode.id);
        assert!(
            found.is_some(),
            "find_preset_by_mode_id should return Some for id={}",
            mode.id.0.as_ref()
        );
        let preset = found.unwrap();
        assert_eq!(
            preset.id,
            mode.id.0.as_ref(),
            "preset id should match mode id"
        );
        // Spot check that label/description correspond to preset
        assert_eq!(
            mode.name, preset.label,
            "mode name should match preset label"
        );
        if let Some(desc) = &mode.description {
            assert_eq!(
                desc, &preset.description,
                "mode description should match preset description"
            );
        }
    }
}

/// Mode IDs should be unique and stable per preset set.
#[test]
fn mode_ids_unique() {
    let available = modes::available_modes();
    let mut uniq = HashSet::new();
    for mode in &available {
        let inserted = uniq.insert(mode.id.0.as_ref().to_string());
        assert!(
            inserted,
            "duplicate mode id encountered: {}",
            mode.id.0.as_ref()
        );
    }
    assert_eq!(
        uniq.len(),
        modes::APPROVAL_PRESETS.len(),
        "unique mode id count should match presets count"
    );
}

/// The helper is_read_only_mode should detect the read-only mode id.
/// For a non-read-only mode, we pick any other available mode id to ensure false.
#[test]
fn is_read_only_detection() {
    let read_only = acp::SessionModeId("read-only".into());
    assert!(modes::is_read_only_mode(&read_only));

    let non_read_only_opt = modes::available_modes()
        .into_iter()
        .map(|m| m.id)
        .find(|mid| mid.0.as_ref() != "read-only");

    if let Some(non_ro) = non_read_only_opt {
        assert!(
            !modes::is_read_only_mode(&non_ro),
            "expected false for non-read-only id={}, got true",
            non_ro.0.as_ref()
        );
    }
}
