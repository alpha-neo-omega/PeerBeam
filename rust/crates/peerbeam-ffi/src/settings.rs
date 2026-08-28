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
static DATA_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
static TRUST_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

type Op = Result<Value, (Code, String)>;

/// Point settings + trust storage at the engine's data directory.
pub fn configure(data_dir: &str) {
    let base = PathBuf::from(data_dir);
    *SETTINGS_PATH.lock().unwrap() = Some(base.join("ffi_settings.json"));
    *TRUST_PATH.lock().unwrap() = Some(base.join("trust.json"));
    *DATA_DIR.lock().unwrap() = Some(base);
}

/// The configured data directory, once `configure` has run.
///
/// Exposed because the process temp directory is not a place this app may write
/// on every platform it runs on — on Android it is `/data/local/tmp`, outside
/// the sandbox — so anything defaulting a path needs somewhere the host actually
/// gave us.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    DATA_DIR.lock().unwrap().clone()
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
///
/// A document that exists but cannot be read is a third case, not the second:
/// the caller's config still stands, the reason is logged, and the file is kept
/// (see [`preserve_unreadable`]) rather than written over.
pub fn overlay(config: &mut EngineConfig) {
    let s = match read_stored() {
        Stored::Doc(s) => s,
        Stored::Missing => {
            seed(config);
            return;
        }
        // **Not the same as no file.** A document with one bad byte used to
        // arrive here as `None`, indistinguishable from a first run — and this
        // function's answer to a first run is to write defaults, so a settings
        // file a person had hand-edited was replaced, silently, by a launch
        // that said nothing. The reason is now named out loud (this crate's
        // `tracing` lines reach the surface's own log stream and the log file),
        // and the seeding write below moves the old file aside instead of
        // landing on top of it — see `preserve_unreadable`.
        Stored::Unreadable(why) => {
            tracing::warn!(
                error = %why,
                "the persisted settings could not be read; keeping the file aside and starting from defaults"
            );
            seed(config);
            return;
        }
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
    // Shared folders, so a person can choose what they offer without editing a
    // config file. **Applied live** via `browse::configure`, because the
    // alternative — taking effect at the next restart — means someone who has
    // just un-shared a private folder is still sharing it.
    if let Some(dirs) = s.get("shared_directories").and_then(|v| v.as_array()) {
        let dirs: Vec<String> = dirs
            .iter()
            .filter_map(|d| d.as_str())
            .map(str::to_string)
            .collect();
        config.device.shared_directories = dirs;
    }
    if let Some(bps) = s
        .get("max_send_bytes_per_sec")
        .and_then(serde_json::Value::as_u64)
    {
        config.transfer.max_send_bytes_per_sec = bps;
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
    match read_stored() {
        Stored::Doc(v) => v,
        Stored::Missing | Stored::Unreadable(_) => defaults(),
    }
}

/// What is actually on disk where the settings document should be.
///
/// The three cases are kept apart because two of them used to collapse into
/// one: `load_persisted` was `fs::read(p).ok()? -> from_slice(..).ok()`, which
/// answers `None` both for "this device has never had settings" and for "this
/// document exists and I cannot read it". Only the first of those is an
/// invitation to write defaults over the path.
enum Stored {
    /// No settings file yet — or no data directory configured at all. First run.
    Missing,
    /// The file is there and this build cannot turn it into settings: a parse
    /// error, or an IO failure that is not "not found". Carries the reason so
    /// something can say it out loud.
    Unreadable(String),
    /// A document that parsed.
    Doc(Value),
}

fn read_stored() -> Stored {
    match path() {
        Some(p) => read_at(&p),
        None => Stored::Missing,
    }
}

/// [`read_stored`] against an explicit path — the whole classification, with no
/// global state, so it can be tested for exactly the distinction it exists to
/// make.
fn read_at(p: &std::path::Path) -> Stored {
    let bytes = match std::fs::read(p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Stored::Missing,
        Err(e) => return Stored::Unreadable(e.to_string()),
    };
    match serde_json::from_slice(&bytes) {
        Ok(v) => Stored::Doc(v),
        Err(e) => Stored::Unreadable(e.to_string()),
    }
}

/// Write the document a first run starts from: defaults, with the keys the
/// caller's own config already decided stamped over them, so `get` reflects
/// reality rather than what the defaults happen to say.
///
/// Best-effort by design (an unwritable data directory is not a reason to fail
/// `init`), and shared by the missing and unreadable paths so the two cannot
/// drift apart.
fn seed(config: &EngineConfig) {
    let mut seeded = defaults();
    if let Value::Object(m) = &mut seeded {
        m.insert("device_name".into(), json!(config.device.name));
        m.insert(
            "transfer_directory".into(),
            json!(config.storage.save_directory),
        );
        m.insert(
            "shared_directories".into(),
            json!(config.device.shared_directories),
        );
        m.insert(
            "max_send_bytes_per_sec".into(),
            json!(config.transfer.max_send_bytes_per_sec),
        );
        m.insert(
            "auto_accept".into(),
            json!(config.device.auto_accept_trusted),
        );
    }
    if let Err((_, e)) = save(&seeded) {
        tracing::warn!(error = %e, "could not write the initial settings document");
    }
}

/// Where an unreadable document is kept. Timestamped rather than a fixed
/// `.corrupt` name: a second bad document must not silently delete the first
/// one, which is the entire failure being fixed.
fn quarantine_path(p: &std::path::Path, stamp: &str) -> PathBuf {
    let mut name = p.as_os_str().to_owned();
    name.push(format!(".corrupt-{stamp}"));
    PathBuf::from(name)
}

/// Move a document this build cannot read out of the way, so the write that
/// follows cannot destroy it.
///
/// Every write goes through [`save`], so this is the single place that has to
/// hold — including `reset`, which is an explicit request for defaults but not
/// an informed decision to throw away a file nobody could show the user.
/// A no-op for the ordinary case (a document that parses) and for a first run.
///
/// A rename that fails **refuses the write**: the whole point is that these
/// bytes survive, and overwriting them because the backup failed would be the
/// original bug with an extra step.
fn preserve_unreadable(p: &std::path::Path) -> Result<(), (Code, String)> {
    let Stored::Unreadable(why) = read_at(p) else {
        return Ok(());
    };
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let kept = quarantine_path(p, &stamp);
    std::fs::rename(p, &kept).map_err(|e| {
        (
            Code::Storage,
            format!("refusing to overwrite unreadable settings: {e}"),
        )
    })?;
    tracing::warn!(
        error = %why,
        kept_at = %kept.display(),
        "the settings file could not be read; it has been kept aside and defaults written in its place"
    );
    Ok(())
}

fn save(value: &Value) -> Result<(), (Code, String)> {
    let p = path().ok_or((Code::NotInitialised, "settings not configured".into()))?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| (Code::Storage, format!("settings dir: {e}")))?;
    }
    // Before the write, never after: what is on disk right now may be the only
    // copy of something this build cannot read.
    preserve_unreadable(&p)?;
    let json = serde_json::to_vec_pretty(value).expect("settings serializable");
    write_atomically(&p, &json)
}

/// How many times a rename is retried before giving up, and how long between.
///
/// Windows refuses a rename while *any* handle to the destination is open — a
/// concurrent reader is enough — so the same retry the app store uses applies
/// here. Twenty attempts at ten milliseconds fails a genuinely broken write in
/// a fifth of a second.
const COMMIT_ATTEMPTS: u32 = 20;
const COMMIT_BACKOFF: std::time::Duration = std::time::Duration::from_millis(10);

/// Write the settings document durably: temp, fsync, atomic rename.
///
/// **`std::fs::write` truncates first.** A crash, a power loss or a full disk
/// between the truncate and the write left a zero-length or half-written
/// `ffi_settings.json` — which `load` cannot parse, so it quarantines the file
/// and writes defaults. Every preference the user set is silently gone, and one
/// of them is `require_pairing_confirmation`, which defaults **off**: a torn
/// write turns a security gate the user switched on back off, with no error and
/// nothing on screen.
///
/// Renaming a fully written temp over the old document means the old one
/// survives every failure, which is what makes the reset impossible rather than
/// merely unlikely. Same shape as `peerbeam-appstore-fs::write_private`.
fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> Result<(), (Code, String)> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    write_tmp(&tmp, bytes)?;

    let mut last = None;
    for attempt in 0..COMMIT_ATTEMPTS {
        match std::fs::rename(&tmp, path) {
            Ok(()) => {
                // The rename itself is durable only once the directory entry
                // is. Best-effort: a filesystem that refuses to fsync a
                // directory is not a reason to report the write as failed.
                if let Some(dir) = path.parent() {
                    if let Ok(d) = std::fs::File::open(dir) {
                        let _ = d.sync_all();
                    }
                }
                return Ok(());
            }
            Err(e) => {
                last = Some(e);
                if attempt + 1 < COMMIT_ATTEMPTS {
                    std::thread::sleep(COMMIT_BACKOFF);
                }
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err((
        Code::Storage,
        format!(
            "write settings after {COMMIT_ATTEMPTS} attempts: {}",
            last.expect("a failure is recorded before the loop ends")
        ),
    ))
}

#[cfg(unix)]
fn write_tmp(tmp: &std::path::Path, bytes: &[u8]) -> Result<(), (Code, String)> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(tmp)
        .map_err(|e| (Code::Storage, format!("create settings tmp: {e}")))?;
    let r = (|| {
        f.write_all(bytes)
            .map_err(|e| (Code::Storage, format!("write settings tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| (Code::Storage, format!("fsync settings tmp: {e}")))
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    r
}

#[cfg(not(unix))]
fn write_tmp(tmp: &std::path::Path, bytes: &[u8]) -> Result<(), (Code, String)> {
    use std::io::Write;
    let r = (|| {
        let mut f = std::fs::File::create(tmp)
            .map_err(|e| (Code::Storage, format!("create settings tmp: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| (Code::Storage, format!("write settings tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| (Code::Storage, format!("fsync settings tmp: {e}")))
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    r
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings path is a process-wide static, so every test here drives
    /// the same one — `#[serial_test::serial]`, the same convention the
    /// init/shutdown tests in this crate use.
    fn point_at(dir: &std::path::Path) -> PathBuf {
        configure(&dir.to_string_lossy());
        path().expect("configured")
    }

    const BROKEN: &[u8] = b"{ \"device_name\": \"MyLaptop\", \"theme\": ";

    /// The distinction the old reader could not make: a parse error and no file
    /// at all were both `None`, and only one of them means "write defaults
    /// here".
    #[test]
    #[serial_test::serial]
    fn a_parse_error_is_not_the_same_answer_as_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nothing-here.json");
        let broken = dir.path().join("broken.json");
        let good = dir.path().join("good.json");
        std::fs::write(&broken, BROKEN).unwrap();
        std::fs::write(&good, br#"{"device_name":"MyLaptop"}"#).unwrap();

        assert!(matches!(read_at(&missing), Stored::Missing));
        assert!(matches!(read_at(&broken), Stored::Unreadable(_)));
        assert!(matches!(read_at(&good), Stored::Doc(_)));
    }

    /// **The data loss.** A settings document this build cannot parse used to
    /// be replaced by defaults on the very next launch, with nothing logged and
    /// no copy kept — a file the user may have hand-edited, and which a later
    /// build might well have read, gone for good.
    #[test]
    #[serial_test::serial]
    fn an_unreadable_settings_document_is_kept_rather_than_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let p = point_at(dir.path());
        std::fs::write(&p, BROKEN).unwrap();

        let mut config = EngineConfig::default();
        config.device.name = "Chosen By The Caller".into();
        overlay(&mut config);

        let kept: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|q| {
                q.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(".corrupt-"))
            })
            .collect();
        assert_eq!(kept.len(), 1, "the unreadable document must still exist");
        assert_eq!(
            std::fs::read(&kept[0]).unwrap(),
            BROKEN,
            "kept byte for byte — a later build, or the user, may still read it"
        );
        assert!(
            matches!(read_at(&p), Stored::Doc(_)),
            "and the live document is usable again"
        );
        assert_eq!(
            config.device.name, "Chosen By The Caller",
            "an unreadable document must not overrule the config it could not be read against"
        );
    }

    /// The same protection on the `set` path, which is the other way the
    /// document gets written: every write goes through `save`, so a settings
    /// change made while a corrupt file is on disk must not be what destroys
    /// it.
    #[test]
    #[serial_test::serial]
    fn a_settings_change_preserves_a_document_it_could_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let p = point_at(dir.path());
        std::fs::write(&p, BROKEN).unwrap();

        set(&json!({ "theme": "dark" })).expect("a change must still be possible");

        let kept = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .expect("the unreadable document must still exist");
        assert_eq!(std::fs::read(kept.path()).unwrap(), BROKEN);
        assert_eq!(load()["theme"], json!("dark"), "and the change took effect");
    }

    /// A second bad document must not delete the first one's copy — the
    /// quarantine name carries when it happened.
    #[test]
    fn quarantine_names_are_per_incident() {
        let p = std::path::Path::new("/data/ffi_settings.json");
        assert_eq!(
            quarantine_path(p, "20260819T101500Z"),
            PathBuf::from("/data/ffi_settings.json.corrupt-20260819T101500Z")
        );
        assert_ne!(
            quarantine_path(p, "20260819T101500Z"),
            quarantine_path(p, "20260820T090000Z")
        );
    }
}
