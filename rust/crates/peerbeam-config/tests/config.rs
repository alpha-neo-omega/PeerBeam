//! `EngineConfig` load/save contract tests.
//!
//! The config file is a persisted, user-editable contract: a serde change that
//! silently breaks round-trip or default handling would corrupt every user's
//! settings on upgrade. These tests pin that behaviour.

use peerbeam_config::{ConfigError, EngineConfig};

/// Compare two configs structurally (neither derives `PartialEq`).
fn same(a: &EngineConfig, b: &EngineConfig) -> bool {
    serde_json::to_value(a).unwrap() == serde_json::to_value(b).unwrap()
}

#[test]
fn save_then_load_round_trips_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");

    let mut cfg = EngineConfig::default();
    cfg.device.name = "round-trip-box".into();
    cfg.transfer.chunk_size = 512 * 1024;
    cfg.transfer.max_concurrent = 7;
    cfg.encryption.required = false;
    cfg.log.json = true;

    cfg.save(&path).unwrap();
    let loaded = EngineConfig::load(&path).unwrap();
    assert!(
        same(&cfg, &loaded),
        "loaded config must equal the saved one"
    );
}

#[test]
fn save_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    // Two levels that do not exist yet.
    let path = dir.path().join("nested/deeper/config.json");
    assert!(!path.parent().unwrap().exists());

    EngineConfig::default().save(&path).unwrap();
    assert!(path.exists(), "save must create parent directories");
    EngineConfig::load(&path).unwrap();
}

#[test]
fn load_or_default_returns_default_when_file_absent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does-not-exist.json");

    let cfg = EngineConfig::load_or_default(&path).unwrap();
    assert!(
        same(&cfg, &EngineConfig::default()),
        "missing file must yield defaults, not an error"
    );
}

#[test]
fn load_missing_file_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nope.json");

    match EngineConfig::load(&path) {
        Err(ConfigError::Io(_)) => {}
        other => panic!("expected Io error for missing file, got {other:?}"),
    }
}

#[test]
fn malformed_json_is_a_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.json");
    std::fs::write(&path, "{ this is not json ]").unwrap();

    match EngineConfig::load(&path) {
        Err(ConfigError::Parse(_)) => {}
        other => panic!("expected Parse error for malformed json, got {other:?}"),
    }
}

#[test]
fn missing_required_field_is_a_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.json");

    // Valid JSON, but a whole required section is absent. Fields have no
    // `#[serde(default)]`, so this must fail loudly rather than half-load.
    let mut value = serde_json::to_value(EngineConfig::default()).unwrap();
    value.as_object_mut().unwrap().remove("transfer");
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

    match EngineConfig::load(&path) {
        Err(ConfigError::Parse(_)) => {}
        other => panic!("expected Parse error for missing field, got {other:?}"),
    }
}

#[test]
fn unknown_fields_are_ignored_for_forward_compat() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.json");

    // A newer version wrote an extra key. An older binary must still load it
    // (serde ignores unknown fields by default), so downgrades don't brick.
    let mut value = serde_json::to_value(EngineConfig::default()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("future_feature".into(), serde_json::json!({ "x": 1 }));
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

    let cfg = EngineConfig::load(&path).unwrap();
    assert!(same(&cfg, &EngineConfig::default()));
}
