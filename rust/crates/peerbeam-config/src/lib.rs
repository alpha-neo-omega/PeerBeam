//! Typed configuration for PeerBeam.
//!
//! [`EngineConfig`] is the single configuration object the engine is built
//! from. It has sensible defaults derived from the [`peerbeam_platform`]
//! layer, and can be loaded from / saved to disk as JSON. Frontends may
//! override individual fields before building the engine.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod rules;

pub use rules::{Destination, Fallback, RuleError, SaveRule};

/// Errors from loading or saving configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Reading or writing the config file failed.
    #[error("config io: {0}")]
    Io(String),
    /// The config file could not be parsed or serialized.
    #[error("config parse: {0}")]
    Parse(String),
}

/// Top-level engine configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    /// This device's identity settings.
    pub device: DeviceConfig,
    /// Discovery behaviour.
    pub discovery: DiscoveryConfig,
    /// Transfer behaviour.
    pub transfer: TransferConfig,
    /// Encryption behaviour.
    pub encryption: EncryptionConfig,
    /// Storage locations.
    pub storage: StorageConfig,
    /// Logging behaviour.
    pub log: LogConfig,
}

/// Device identity configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceConfig {
    /// Human-friendly device name.
    pub name: String,
    /// Automatically accept transfers from already-trusted devices.
    pub auto_accept_trusted: bool,
    /// Require an explicit first-contact pairing-code confirmation before
    /// accepting a transfer from a newly pinned peer (optional MITM check).
    /// Off by default so zero-config first contact stays frictionless.
    pub require_pairing_confirmation: bool,
    /// Largest file that may be attached in a chat, in bytes.
    ///
    /// Staging is uniform — it runs on every chat send, not only on sends that
    /// queue — so this is a backstop against the absurd, not a product limit.
    /// A low value here would be a capability regression: the plain transfer
    /// path streams a file of any size straight from the source.
    pub max_queued_file_bytes: u64,
    /// Refuse to stage if doing so would leave less than this much free.
    /// Filling the disk to zero can break unrelated applications.
    pub min_free_bytes: u64,
    /// *"Share device status with trusted devices"* — the presence opt-in.
    ///
    /// **Default off**, and that default is the feature's whole privacy story
    /// (I11). While it is false this device sends no status at all, to anyone;
    /// it still receives and displays what its peers share. Turning it on
    /// shares battery, free storage, network kind and app version — and only
    /// ever with devices in the trust store, which is not configurable here or
    /// anywhere else.
    pub share_presence: bool,
    /// *"Tell people when you have read their messages"* — the read-receipt
    /// opt-in.
    ///
    /// **Default off**, for the same reason [`share_presence`] is
    /// (I11): a read receipt is a disclosure about *you* — it tells a peer the
    /// moment you looked at something, which is a claim about your attention,
    /// not about the message. Reading someone's message must never be the thing
    /// that reports on you.
    ///
    /// While it is false this device sends no receipts at all, to anyone; it
    /// still **applies** receipts its peers send, so a peer that has opted in
    /// is shown as having read your messages either way. The setting governs
    /// what this device discloses, never what it will accept — the same
    /// asymmetry presence has.
    pub share_read_receipts: bool,
    /// *"Keep a short clipboard history on this device"* — the history opt-in.
    ///
    /// **Default off**, and deliberately separate from clipboard sync. Syncing
    /// your clipboard and keeping a record of it are different decisions with
    /// different risks: bundling them would hand a stored log to someone who
    /// only wanted two machines to share a clipboard, which is precisely what
    /// clipboard sync promised not to create.
    ///
    /// History is bounded, kept only on this device, and never put on the wire.
    pub clipboard_history: bool,
    /// Folders this device offers for read-only browsing.
    ///
    /// **Empty by default, and that is the whole safety story.** Granting a
    /// device the `browse` permission decides *who* may look; this decides
    /// *what there is to look at*, and with nothing listed a permitted peer
    /// still sees nothing. Sharing a folder is a deliberate act, never a
    /// consequence of trusting someone.
    ///
    /// A path that cannot be resolved is dropped rather than kept: an
    /// unresolvable root cannot be compared against safely.
    pub shared_directories: Vec<String>,
    /// A command to run after a file is received, or empty for none.
    ///
    /// **Empty by default.** Running a program because someone sent you a file
    /// is a large amount of trust, and it is trust in *this machine's own
    /// configuration*, never in the peer: nothing a sender controls decides
    /// whether a hook runs or which one.
    ///
    /// Executed directly, **never through a shell** — the received path is
    /// passed as a single argument, so a file named `; rm -rf ~` is an
    /// argument, not a command. That is why this is one program and not a
    /// command line: supporting `cmd && other` would mean invoking a shell,
    /// and a shell is exactly what must not see a peer-supplied name.
    ///
    /// The hook receives the saved path as argv\[1\], the sender's device id as
    /// argv\[2\], and nothing else.
    pub receive_hook: String,
}

/// Discovery configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscoveryConfig {
    /// Master switch for discovery.
    pub enabled: bool,
    /// How often to re-scan, in milliseconds.
    pub scan_interval_ms: u64,
    /// UDP discovery port to bind and broadcast on
    /// (`peerbeam_discovery_udp::Config::port`). `0` lets the OS assign an
    /// ephemeral port — useful in tests, meaningless in production since
    /// peers only find each other by announcing on a shared, known port.
    /// A non-default value only makes sense when something else on the LAN
    /// already occupies the well-known port: peers configured with
    /// different discovery ports will not discover one another.
    pub port: u16,
}

/// Transfer configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferConfig {
    /// Preferred data chunk size in bytes.
    pub chunk_size: u64,
    /// Maximum simultaneous transfers.
    pub max_concurrent: usize,
    /// Enable payload compression when beneficial.
    pub enable_compression: bool,
    /// Enable checkpoint-based resume.
    pub enable_resume: bool,
    /// Port the transfer server (QUIC) listens on and advertises.
    pub port: u16,
}

/// Encryption configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EncryptionConfig {
    /// Require encryption for all transfers.
    pub required: bool,
    /// Require a PIN to be confirmed before a *new* device is pinned.
    ///
    /// **Off by default**, because turning it on means no device can pair
    /// without a person present at both ends — correct for someone who wants
    /// it, and a broken first run for someone who does not expect it.
    ///
    /// When on, trust-on-first-use is no longer trust-on-first-*sight*: the
    /// unknown peer must prove it knows a PIN shown on this device before its
    /// key is pinned. It has no effect on devices already trusted; re-pairing a
    /// known device is not what this protects.
    pub require_pin_pairing: bool,
}

/// Storage/location configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Directory received files are written to.
    pub save_directory: String,
    /// Directory for application data (checkpoints, trust store).
    pub data_directory: String,
    /// Ordered auto-save rules: **where** an accepted item lands.
    ///
    /// Consulted after acceptance, on the way to disk — never as part of
    /// deciding whether to accept (see [`rules`], and I6). The **first**
    /// matching rule wins, so the order of this list is the user's tie-break;
    /// an empty list — the default, and what every config file written before
    /// this field existed deserializes to — means every item goes to
    /// [`StorageConfig::save_directory`] exactly as before.
    ///
    /// Absolute paths, so this applies on desktop and headless only; Android
    /// receives through SAF and cannot write arbitrary paths.
    pub rules: Vec<SaveRule>,
}

/// Logging configuration consumed by `peerbeam-telemetry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// `tracing` env-filter directive (e.g. `peerbeam=info`).
    pub filter: String,
    /// Include the emitting target/module in log lines.
    pub show_target: bool,
    /// Emit logs as JSON (useful for headless/daemon deployments).
    pub json: bool,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            name: peerbeam_platform::hostname(),
            auto_accept_trusted: false,
            require_pairing_confirmation: false,
            // 16 GiB — comfortably past any file a person shares in a chat,
            // and far short of a value that could quietly fill a disk.
            max_queued_file_bytes: 17_179_869_184,
            // 512 MiB — enough headroom that the OS and other applications
            // keep working even if a staged copy lands right at the floor.
            min_free_bytes: 536_870_912,
            // Opt-in. Nothing about this device leaves it until the user says
            // so; see the field's own doc comment.
            share_presence: false,
            share_read_receipts: false,
            clipboard_history: false,
            shared_directories: Vec::new(),
            receive_hook: String::new(),
        }
    }
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scan_interval_ms: 2000,
            // Kept as a literal rather than a dependency on
            // peerbeam-discovery-udp (this crate stays a dependency leaf).
            // Source of truth: `peerbeam_discovery_udp::DEFAULT_DISCOVERY_PORT`.
            // `peerbeam-ffi` already depends on both crates, so its test
            // suite (`runtime.rs`'s `discovery_config_default_port_matches_udp_default`)
            // asserts the two agree so they cannot silently drift apart.
            port: 49500,
        }
    }
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            // 64 KiB: fine-grained, smooth progress. Progress emission is
            // time-throttled so small chunks don't flood the event bridge.
            chunk_size: 64 * 1024,
            max_concurrent: 3,
            enable_compression: true,
            enable_resume: true,
            port: 49600,
        }
    }
}

impl Default for EncryptionConfig {
    fn default() -> Self {
        Self {
            required: true,
            // Off by default: on, no device can pair without a person at
            // both ends, which is right for someone who chose it and a
            // broken first run for someone who did not.
            require_pin_pairing: false,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            save_directory: peerbeam_platform::download_dir()
                .to_string_lossy()
                .into_owned(),
            data_directory: peerbeam_platform::data_dir().to_string_lossy().into_owned(),
            // No rules by default: the out-of-the-box behaviour is the one
            // that shipped before rules existed.
            rules: Vec::new(),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            filter: "peerbeam=info".to_string(),
            show_target: false,
            json: false,
        }
    }
}

impl EngineConfig {
    /// Load configuration from a JSON file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io(e.to_string()))?;
        serde_json::from_str(&raw).map_err(|e| ConfigError::Parse(e.to_string()))
    }

    /// Load configuration from `path`, falling back to defaults if the file
    /// does not exist. Any other error is propagated.
    pub fn load_or_default(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw).map_err(|e| ConfigError::Parse(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Io(e.to_string())),
        }
    }

    /// Persist configuration to a JSON file, creating parent directories.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConfigError::Io(e.to_string()))?;
        }
        let json =
            serde_json::to_string_pretty(self).map_err(|e| ConfigError::Parse(e.to_string()))?;
        // Atomic write: uniquely-named temp + rename, so an interrupted save
        // can't leave a truncated config, and concurrent savers don't rename
        // the same temp out from under each other. The temp file is fsync'd
        // before the rename so its data is durable first — otherwise a crash
        // between the rename (metadata-only) landing and the data blocks
        // actually hitting disk can leave config.json present but empty, and
        // the parent directory is fsync'd afterwards (best-effort) so the
        // rename itself survives a crash too.
        let tmp = {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let mut s = path.as_os_str().to_owned();
            s.push(format!(".{}.{}.tmp", std::process::id(), n));
            std::path::PathBuf::from(s)
        };
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp).map_err(|e| ConfigError::Io(e.to_string()))?;
            f.write_all(json.as_bytes())
                .map_err(|e| ConfigError::Io(e.to_string()))?;
            f.sync_all().map_err(|e| ConfigError::Io(e.to_string()))?;
        }
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            ConfigError::Io(e.to_string())
        })?;
        if let Some(parent) = path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod compat_tests {
    use super::*;

    /// A config written by an older (or newer) version must load: missing
    /// fields fall back to defaults instead of failing the parse.
    #[test]
    fn partial_config_loads_with_defaults() {
        let json = r#"{"device":{"name":"old-box"},"transfer":{"port":50000}}"#;
        let cfg: EngineConfig = serde_json::from_str(json).expect("partial config parses");
        assert_eq!(cfg.device.name, "old-box");
        assert!(!cfg.device.auto_accept_trusted, "missing field -> default");
        assert_eq!(cfg.transfer.port, 50000);
        assert_eq!(cfg.transfer.chunk_size, 64 * 1024, "missing -> default");
        assert!(cfg.encryption.required, "missing section -> default");
    }

    /// Staging duplicates whatever it copies, so both knobs must have safe
    /// defaults *and* must be absent-tolerant: every config file already on
    /// disk predates them.
    #[test]
    fn staging_limits_default_and_survive_a_config_that_predates_them() {
        let d = DeviceConfig::default();
        assert_eq!(d.max_queued_file_bytes, 17_179_869_184, "16 GiB backstop");
        assert_eq!(d.min_free_bytes, 536_870_912, "512 MiB floor");

        // A config file written before these fields existed must still load.
        let cfg: EngineConfig =
            serde_json::from_str(r#"{"device":{"name":"old-box"},"transfer":{"port":50000}}"#)
                .expect("a pre-existing config must still parse");
        assert_eq!(cfg.device.name, "old-box");
        assert_eq!(cfg.device.max_queued_file_bytes, 17_179_869_184);
        assert_eq!(cfg.device.min_free_bytes, 536_870_912);

        // And an operator who sets them is honoured.
        let cfg: EngineConfig = serde_json::from_str(
            r#"{"device":{"max_queued_file_bytes":1024,"min_free_bytes":2048}}"#,
        )
        .unwrap();
        assert_eq!(cfg.device.max_queued_file_bytes, 1024);
        assert_eq!(cfg.device.min_free_bytes, 2048);
    }

    #[test]
    fn require_pairing_confirmation_defaults_off_and_round_trips() {
        // Default is off (zero-config stays frictionless).
        assert!(!DeviceConfig::default().require_pairing_confirmation);

        // Absent in JSON -> false via serde(default).
        let cfg: EngineConfig = serde_json::from_str(r#"{"device":{"name":"x"}}"#).unwrap();
        assert!(!cfg.device.require_pairing_confirmation);

        // Present -> honored.
        let cfg: EngineConfig =
            serde_json::from_str(r#"{"device":{"name":"x","require_pairing_confirmation":true}}"#)
                .unwrap();
        assert!(cfg.device.require_pairing_confirmation);
    }

    /// The presence opt-in defaults **off**, and — the half that matters — a
    /// config file written before this field existed loads as off rather than
    /// as on. An upgrade must never start sharing a device's battery level and
    /// free disk because a key was missing.
    #[test]
    fn share_presence_defaults_off_and_an_older_config_stays_off() {
        assert!(!DeviceConfig::default().share_presence);

        // A config file from before the field existed.
        let cfg: EngineConfig = serde_json::from_str(
            r#"{"device":{"name":"x","auto_accept_trusted":true},"storage":{"save_directory":"/tmp"}}"#,
        )
        .unwrap();
        assert!(
            !cfg.device.share_presence,
            "an upgrade must not silently opt a user in"
        );

        // Present -> honored, and it survives a save/load round trip.
        let cfg: EngineConfig =
            serde_json::from_str(r#"{"device":{"name":"x","share_presence":true}}"#).unwrap();
        assert!(cfg.device.share_presence);
        let back: EngineConfig =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert!(back.device.share_presence);
    }

    /// Auto-save rules are **additive**: every config file already on disk
    /// predates them and must load as "no rules", which is the same receive
    /// behaviour those users have today. An upgrade that started diverting
    /// files because a key was missing would be the worst possible bug in this
    /// feature.
    #[test]
    fn save_rules_default_empty_and_a_config_that_predates_them_still_loads() {
        assert!(StorageConfig::default().rules.is_empty());

        let cfg: EngineConfig = serde_json::from_str(
            r#"{"device":{"name":"old-box"},"storage":{"save_directory":"/tmp/dl","data_directory":"/tmp/data"}}"#,
        )
        .expect("a config written before rules existed must still parse");
        assert_eq!(cfg.storage.save_directory, "/tmp/dl");
        assert!(
            cfg.storage.rules.is_empty(),
            "a missing list means no rules, never a default rule"
        );

        // And a list that is present is honoured, round trip included.
        let cfg: EngineConfig = serde_json::from_str(
            r#"{"storage":{"rules":[{"extension":"pdf","directory":"/srv/pdfs"}]}}"#,
        )
        .expect("a config with rules parses");
        assert_eq!(cfg.storage.rules.len(), 1);
        assert_eq!(cfg.storage.rules[0].extension.as_deref(), Some("pdf"));
        assert_eq!(cfg.storage.rules[0].directory, "/srv/pdfs");
        assert_eq!(
            cfg.storage.rules[0].device, None,
            "an omitted criterion stays omitted rather than becoming a value"
        );

        let back: EngineConfig =
            serde_json::from_str(&serde_json::to_string(&cfg).expect("serialize")).expect("reload");
        assert_eq!(back.storage.rules, cfg.storage.rules);
    }

    /// `DiscoveryConfig::port` must default to the well-known discovery port
    /// (kept here as a literal — see the `Default` impl's comment for why —
    /// and cross-checked against the real constant by
    /// `peerbeam-ffi`'s `discovery_config_default_port_matches_udp_default`,
    /// the one crate that already depends on both), and every config file
    /// written before this field existed must still load.
    #[test]
    fn discovery_port_defaults_and_survives_a_config_that_predates_it() {
        assert_eq!(DiscoveryConfig::default().port, 49500);

        // Absent in JSON -> the default, via serde(default).
        let cfg: EngineConfig =
            serde_json::from_str(r#"{"device":{"name":"old-box"}}"#).expect("pre-existing config");
        assert_eq!(cfg.discovery.port, 49500);

        // Present -> honored (e.g. `0` for an OS-assigned port in tests).
        let cfg: EngineConfig = serde_json::from_str(r#"{"discovery":{"port":0}}"#).unwrap();
        assert_eq!(cfg.discovery.port, 0);
    }
}
