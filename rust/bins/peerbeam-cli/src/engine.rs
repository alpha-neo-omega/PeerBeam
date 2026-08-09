//! Engine wiring shared by commands that need discovery.

use std::path::PathBuf;
use std::sync::Arc;

use peerbeam_config::EngineConfig;
use peerbeam_crypto::AeadCrypto;
use peerbeam_discovery_mdns::MdnsDiscovery;
use peerbeam_discovery_tailscale::{Config as TsConfig, TailscaleDiscovery};
use peerbeam_discovery_udp::UdpDiscovery;
use peerbeam_domain::entity::{Device, DeviceType};
use peerbeam_domain::error::Result as DResult;
use peerbeam_domain::id::DeviceId;
use peerbeam_engine::{Engine, EngineBuilder};
use peerbeam_identity_fs::FsIdentity;

/// Path to the config file (honours `--config`).
pub fn config_path(override_path: Option<&str>) -> PathBuf {
    match override_path {
        Some(p) => PathBuf::from(p),
        None => peerbeam_platform::config_dir().join("config.json"),
    }
}

/// This device's stable id, derived from its long-term identity keypair
/// (generated once and persisted at `<data_directory>/identity.json` — the
/// very same file [`crate::commands::SecureCtx::build`] authenticates with).
/// Discovery announces this id and the transfer handshake authenticates with
/// it, so a peer always sees the same `pb-<fingerprint>` for this device,
/// never a per-run id.
pub fn device_id(config: &EngineConfig) -> DResult<DeviceId> {
    let path = std::path::Path::new(&config.storage.data_directory).join("identity.json");
    let enc = AeadCrypto::new();
    let ident = peerbeam_transfer::load_or_generate(
        &FsIdentity::open(path),
        &enc,
        config.device.name.clone(),
    )?;
    Ok(ident.device_id)
}

/// A `Device` describing this machine, from config + platform.
pub fn me(config: &EngineConfig) -> DResult<Device> {
    Ok(Device {
        id: device_id(config)?,
        name: config.device.name.clone(),
        device_type: DeviceType::Desktop,
        platform: peerbeam_platform::current(),
        addresses: vec![],
        port: config.transfer.port,
        last_seen: chrono::Utc::now(),
    })
}

/// Build an engine with every discovery provider wired. mDNS is skipped if its
/// daemon can't start; Tailscale is scan-only and harmless when absent.
pub fn build_engine(config: EngineConfig) -> DResult<Engine> {
    let id = device_id(&config)?;
    // Stamp our transfer port on Tailscale-discovered peers so they're
    // dialable (tailscale status reports IPs only; peers share the port).
    let ts = TsConfig {
        peer_port: config.transfer.port,
        ..TsConfig::default()
    };
    let mut builder =
        EngineBuilder::new(config).with_discovery(Arc::new(UdpDiscovery::new(id.clone())));

    if let Ok(mdns) = MdnsDiscovery::new(id.clone()) {
        builder = builder.with_discovery(Arc::new(mdns));
    }
    builder = builder.with_discovery(Arc::new(TailscaleDiscovery::new(ts)));

    // No required singleton providers, so build never fails here.
    Ok(builder.build().expect("engine builds"))
}
