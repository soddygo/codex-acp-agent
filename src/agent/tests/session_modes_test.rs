#![cfg(test)]

use agent_client_protocol as acp;
use std::collections::HashSet;

use crate::agent::session;

/// Ensure available_modes returns a non-empty list with valid structure
#[test]
fn available_modes_non_empty() {
    let available = session::available_modes();
    assert!(!available.is_empty(), "available_modes should not be empty");

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
    for mode in session::available_modes() {
        let found = session::find_preset_by_mode_id(&mode.id);
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

/// Mode IDs should be unique
#[test]
fn mode_ids_unique() {
    let available = session::available_modes();
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
        available.len(),
        "unique mode id count should match available modes count"
    );
}

/// The helper is_read_only_mode should detect the read-only mode id.
/// For a non-read-only mode, we pick any other available mode id to ensure false.
#[test]
fn is_read_only_detection() {
    let read_only = acp::SessionModeId("read-only".into());
    assert!(session::is_read_only_mode(&read_only));

    let non_read_only_opt = session::available_modes()
        .into_iter()
        .map(|m| m.id)
        .find(|mid| mid.0.as_ref() != "read-only");

    if let Some(non_ro) = non_read_only_opt {
        assert!(
            !session::is_read_only_mode(&non_ro),
            "expected false for non-read-only id={}, got true",
            non_ro.0.as_ref()
        );
    }
}

/// Basic test for is_read_only_mode
#[test]
fn is_read_only_mode_basic() {
    assert!(session::is_read_only_mode(&acp::SessionModeId(
        "read-only".into()
    )));
    assert!(!session::is_read_only_mode(&acp::SessionModeId(
        "not-read-only".into()
    )));
}

/// Test is_custom_provider detection
#[test]
fn is_custom_provider_detection() {
    // OpenAI is a builtin provider
    assert!(!session::is_custom_provider("openai"));

    // Other providers are considered custom
    assert!(session::is_custom_provider("anthropic"));
    assert!(session::is_custom_provider("custom-llm"));
    assert!(session::is_custom_provider("my-provider"));
    assert!(session::is_custom_provider(""));
}

// Note: Tests for available_models_from_profiles would require constructing
// a CodexConfig which doesn't have a Default implementation. These tests
// would be better as integration tests with a real config file.
