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
fn missing_section_loads_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.json");

    // Valid JSON with a whole section absent — a config from an older version.
    // Forward/backward compatibility: it must load, with defaults filling in.
    let mut value = serde_json::to_value(EngineConfig::default()).unwrap();
    value.as_object_mut().unwrap().remove("transfer");
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

    let cfg = EngineConfig::load(&path).expect("partial config loads");
    assert_eq!(cfg.transfer.port, 49600, "missing section -> defaults");
}

#[test]
fn wrong_type_is_still_a_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.json");

    // Compatibility tolerates *missing* fields, not corrupt ones: a value of
    // the wrong type must still fail loudly rather than half-load.
    std::fs::write(&path, r#"{"transfer":{"port":"not-a-number"}}"#).unwrap();

    match EngineConfig::load(&path) {
        Err(ConfigError::Parse(_)) => {}
        other => panic!("expected Parse error for wrong type, got {other:?}"),
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

/// `save` fsyncs the temp file's data before the rename (and best-effort
/// fsyncs the parent directory afterwards) so a crash can't leave
/// config.json present-but-empty. This can't simulate a real power cut, but
/// it pins the surrounding contract: the rename lands, the temp file never
/// leaks, and the written bytes are immediately readable back in full —
/// regressions in the fsync-then-rename sequencing would show up as a
/// leftover `.tmp` file or a truncated/missing config.json.
#[test]
fn save_is_durable_and_leaves_no_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");

    let mut cfg = EngineConfig::default();
    cfg.device.name = "durable-box".into();

    for _ in 0..5 {
        cfg.save(&path).unwrap();
    }

    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        entries,
        vec![std::ffi::OsString::from("config.json")],
        "no .tmp files must be left behind after save"
    );

    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.is_empty(), "the saved file must not be truncated");
    let loaded = EngineConfig::load(&path).unwrap();
    assert!(same(&cfg, &loaded));
}
