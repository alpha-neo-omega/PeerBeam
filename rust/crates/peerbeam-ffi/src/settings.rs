//! Versioned, persisted settings. A single JSON document under the data
//! directory is the source of truth for UI-facing settings; `get`/`set`/`reset`
//! read/merge/replace it and emit `settings_changed`. Settings are applied to
//! the engine on next init (no live engine-mutation API exists yet).

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Value};

use peerbeam_config::EngineConfig;

use crate::error::Code;
use crate::events;

/// Settings schema version (bump on a breaking field change).
const SCHEMA: u32 = 1;

static SETTINGS_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static TRUST_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

type Op = Result<Value, (Code, String)>;

/// Point settings + trust storage at the engine's data directory.
pub fn configure(data_dir: &str) {
    let base = PathBuf::from(data_dir);
    *SETTINGS_PATH.lock().unwrap() = Some(base.join("ffi_settings.json"));
    *TRUST_PATH.lock().unwrap() = Some(base.join("trust.json"));
}

fn defaults() -> Value {
    let c = EngineConfig::default();
    json!({
        "version": SCHEMA,
        "device_name": c.device.name,
        "transfer_directory": c.storage.save_directory,
        "auto_accept": c.device.auto_accept_trusted,
        "theme": "system",
        "discovery_enabled": c.discovery.enabled,
        "notifications": true,
        "logging": c.log.filter,
        "experimental": {},
    })
}

/// Overlay the persisted settings onto an engine config (device identity,
/// save directory, auto-accept). Called during init so what the user set in
/// the UI actually reaches the engine, not just the JSON file.
pub fn overlay(config: &mut EngineConfig) {
    let s = load();
    if let Some(name) = s.get("device_name").and_then(|v| v.as_str()) {
        if !name.trim().is_empty() {
            config.device.name = name.trim().to_string();
        }
    }
    if let Some(dir) = s.get("transfer_directory").and_then(|v| v.as_str()) {
        if !dir.trim().is_empty() {
            config.storage.save_directory = dir.trim().to_string();
        }
    }
    if let Some(auto) = s.get("auto_accept").and_then(|v| v.as_bool()) {
        config.device.auto_accept_trusted = auto;
    }
}

fn path() -> Option<PathBuf> {
    SETTINGS_PATH.lock().unwrap().clone()
}

fn load() -> Value {
    match path().and_then(|p| std::fs::read(p).ok()) {
        Some(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| defaults()),
        None => defaults(),
    }
}

fn save(value: &Value) -> Result<(), (Code, String)> {
    let p = path().ok_or((Code::NotInitialised, "settings not configured".into()))?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| (Code::Storage, format!("settings dir: {e}")))?;
    }
    let json = serde_json::to_vec_pretty(value).expect("settings serializable");
    std::fs::write(&p, json).map_err(|e| (Code::Storage, format!("write settings: {e}")))
}

/// Trusted devices from the TOFU store (best-effort; empty if none/unreadable).
fn trusted() -> Value {
    let records = TRUST_PATH
        .lock()
        .unwrap()
        .clone()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(json!([]));
    records
}

/// Current settings (with live trusted-devices list attached).
pub fn get() -> Op {
    let mut s = load();
    if let Value::Object(m) = &mut s {
        m.insert("trusted_devices".into(), trusted());
    }
    Ok(s)
}

/// Merge a partial settings object, persist, and emit `settings_changed`.
pub fn set(partial: &Value) -> Op {
    let obj = partial.as_object().ok_or((
        Code::InvalidArgument,
        "settings must be a JSON object".into(),
    ))?;
    let mut current = load();
    if let Value::Object(m) = &mut current {
        for (k, v) in obj {
            if k == "version" || k == "trusted_devices" {
                continue; // managed fields
            }
            m.insert(k.clone(), v.clone());
        }
        m.insert("version".into(), json!(SCHEMA));
    }
    save(&current)?;
    emit_changed(&current);
    Ok(json!({ "updated": true }))
}

/// Restore defaults, persist, and emit `settings_changed`.
pub fn reset() -> Op {
    let d = defaults();
    save(&d)?;
    emit_changed(&d);
    Ok(json!({ "reset": true }))
}

fn emit_changed(settings: &Value) {
    events::emit(&json!({
        "type": "settings_changed",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "payload": { "settings": settings },
    }));
}
