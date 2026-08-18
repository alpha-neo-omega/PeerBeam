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
        // "Verify new devices with a pairing code" — **default off**, and the
        // default is the compatibility contract: off means an accept behaves
        // exactly as it did before this gate existed. On, a transfer from a
        // device pinned by this very handshake cannot be accepted until the
        // user confirms both screens show the same code. The key name is the
        // config field the CLI already reads, so one setting means one thing in
        // both frontends (I7).
        "require_pairing_confirmation": c.device.require_pairing_confirmation,
        "theme": "system",
        "discovery_enabled": c.discovery.enabled,
        "notifications": true,
        "logging": c.log.filter,
        // "Share device status with trusted devices" — **default off**, and
        // that default is load-bearing (I11): until the user says otherwise,
        // this device's battery, free disk and network kind leave for nobody.
        // `presence::sharing_enabled` also falls back to `false` when the key
        // is missing or unparseable, so a settings file this build cannot read
        // is never mistaken for consent.
        crate::presence::SHARE_KEY: false,
        // "Tell people when you have read their messages" — **default off**,
        // for the reason presence is (I11): a read receipt discloses when *you*
        // looked, which is a fact about your attention rather than about the
        // message. Reading someone's message must never be the act that reports
        // on you. Like the others, an absent or unparseable key reads as false,
        // so a settings document written before receipts existed is never
        // mistaken for consent. It gates **sending** only: receipts a peer
        // sends are still applied, so opting out costs you nothing others do.
        crate::chat_receipts::SHARE_KEY: false,
        // "Keep a short clipboard history on this device" — **default off**,
        // and separate from the sync toggle on purpose. Syncing your clipboard
        // and keeping a record of it are different decisions: bundling them
        // would hand a stored log to someone who only wanted two machines to
        // share a clipboard, which is exactly what sync promised not to do.
        crate::clipboard::HISTORY_KEY: false,
        // "Sync clipboard with trusted devices" — **default off**, and that
        // default carries even more weight than presence's (I11): the
        // clipboard is the one buffer guaranteed to sometimes hold a password,
        // and this build cannot tell when. `clipboard::sync_enabled` also falls
        // back to `false` when the key is missing or unparseable, so a settings
        // document written before this feature existed is never mistaken for
        // consent.
        crate::clipboard::SYNC_KEY: false,
        // Auto-save rules: an **ordered** list, first match wins. Empty by
        // default, and an empty list means every received file goes to
        // `transfer_directory` exactly as it always has — this feature adds
        // nothing to the experience of a user who never opens it.
        crate::rules::RULES_KEY: [],
        "experimental": {},
    })
}

/// Overlay the persisted settings onto an engine config (device identity,
/// save directory, auto-accept). Called during init so what the user set in
/// the UI actually reaches the engine, not just the JSON file.
///
/// Only an actually-persisted document applies — a missing settings file must
/// never override the caller's explicit config with defaults. First run seeds
/// the document from the effective config so `get` reflects reality.
pub fn overlay(config: &mut EngineConfig) {
    let Some(s) = load_persisted() else {
        let mut seeded = defaults();
        if let Value::Object(m) = &mut seeded {
            m.insert("device_name".into(), json!(config.device.name));
            m.insert(
                "transfer_directory".into(),
                json!(config.storage.save_directory),
            );
            m.insert(
                "auto_accept".into(),
                json!(config.device.auto_accept_trusted),
            );
        }
        let _ = save(&seeded);
        return;
    };
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
    // Absent -> the config's own value (off by default). A settings document
    // written before this gate existed must not be read as *disabling* a check
    // the user never saw, nor as enabling one they never asked for; leaving the
    // config value alone does both.
    if let Some(require) = s
        .get("require_pairing_confirmation")
        .and_then(|v| v.as_bool())
    {
        config.device.require_pairing_confirmation = require;
    }
    // Auto-save rules travel the same road as the save directory: persisted in
    // this document, copied onto the engine's config here, read from there by
    // the one matcher. `from_settings` is total — an absent, malformed or
    // unsupported-platform list is an empty one — so an unreadable rule can
    // never stop this device receiving files, it only stops it sorting them.
    config.storage.rules = crate::rules::from_settings(&s);
}

fn path() -> Option<PathBuf> {
    SETTINGS_PATH.lock().unwrap().clone()
}

fn load() -> Value {
    load_persisted().unwrap_or_else(defaults)
}

/// The persisted document only — None when no settings file exists yet (or it
/// is unreadable). Callers that must not fall back to defaults use this.
fn load_persisted() -> Option<Value> {
    let bytes = path().and_then(|p| std::fs::read(p).ok())?;
    serde_json::from_slice(&bytes).ok()
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
        // Whether this platform can honour auto-save rules at all. A managed
        // field, like `trusted_devices`: it is a fact about the build, not a
        // preference, so `set` refuses to write it. The UI reads it to explain
        // the limitation rather than offering a list that would silently do
        // nothing.
        m.insert("rules_supported".into(), json!(crate::rules::SUPPORTED));
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
            if k == "version" || k == "trusted_devices" || k == "rules_supported" {
                continue; // managed fields
            }
            m.insert(k.clone(), v.clone());
        }
        m.insert("version".into(), json!(SCHEMA));
    }
    save(&current)?;
    // Push the delta into the running engine so save-dir / auto-accept changes
    // take effect without a restart (no-op if not yet initialised).
    crate::runtime::apply_live_settings(partial);
    emit_changed(&current);
    Ok(json!({ "updated": true }))
}

/// Restore defaults, persist, and emit `settings_changed`.
pub fn reset() -> Op {
    let d = defaults();
    save(&d)?;
    crate::runtime::apply_live_settings(&d);
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
