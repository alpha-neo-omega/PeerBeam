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
}

/// Discovery configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscoveryConfig {
    /// Master switch for discovery.
    pub enabled: bool,
    /// How often to re-scan, in milliseconds.
    pub scan_interval_ms: u64,
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
}

/// Storage/location configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Directory received files are written to.
    pub save_directory: String,
    /// Directory for application data (checkpoints, trust store).
    pub data_directory: String,
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
        }
    }
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scan_interval_ms: 2000,
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
        Self { required: true }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            save_directory: peerbeam_platform::download_dir()
                .to_string_lossy()
                .into_owned(),
            data_directory: peerbeam_platform::data_dir().to_string_lossy().into_owned(),
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
}
