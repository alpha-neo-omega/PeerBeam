//! Typed configuration for PeerBeam.
//!
//! [`EngineConfig`] is the single configuration object the engine is built
//! from. It has sensible defaults derived from the [`peerbeam_platform`]
//! layer, and can be loaded from / saved to disk as JSON. Frontends may
//! override individual fields before building the engine.

use std::path::Path;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct DeviceConfig {
    /// Human-friendly device name.
    pub name: String,
    /// Automatically accept transfers from already-trusted devices.
    pub auto_accept_trusted: bool,
}

/// Discovery configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    /// Master switch for discovery.
    pub enabled: bool,
    /// How often to re-scan, in milliseconds.
    pub scan_interval_ms: u64,
}

/// Transfer configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct EncryptionConfig {
    /// Require encryption for all transfers.
    pub required: bool,
}

/// Storage/location configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Directory received files are written to.
    pub save_directory: String,
    /// Directory for application data (checkpoints, trust store).
    pub data_directory: String,
}

/// Logging configuration consumed by `peerbeam-telemetry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// `tracing` env-filter directive (e.g. `peerbeam=info`).
    pub filter: String,
    /// Include the emitting target/module in log lines.
    pub show_target: bool,
    /// Emit logs as JSON (useful for headless/daemon deployments).
    pub json: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            device: DeviceConfig {
                name: peerbeam_platform::hostname(),
                auto_accept_trusted: false,
            },
            discovery: DiscoveryConfig {
                enabled: true,
                scan_interval_ms: 2000,
            },
            transfer: TransferConfig {
                chunk_size: 1024 * 1024,
                max_concurrent: 3,
                enable_compression: true,
                enable_resume: true,
                port: 49600,
            },
            encryption: EncryptionConfig { required: true },
            storage: StorageConfig {
                save_directory: peerbeam_platform::download_dir()
                    .to_string_lossy()
                    .into_owned(),
                data_directory: peerbeam_platform::data_dir().to_string_lossy().into_owned(),
            },
            log: LogConfig::default(),
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
        // Atomic write: temp + rename, so an interrupted save can't leave a
        // truncated config that fails to parse on next load.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| ConfigError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| ConfigError::Io(e.to_string()))
    }
}
