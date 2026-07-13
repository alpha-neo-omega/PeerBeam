//! Engine wiring shared by commands that need discovery.

use std::path::PathBuf;
use std::sync::Arc;

use peerbeam_config::EngineConfig;
use peerbeam_discovery_mdns::MdnsDiscovery;
use peerbeam_discovery_tailscale::{Config as TsConfig, TailscaleDiscovery};
use peerbeam_discovery_udp::UdpDiscovery;
use peerbeam_domain::entity::{Device, DeviceType};
use peerbeam_domain::id::DeviceId;
use peerbeam_engine::{Engine, EngineBuilder};

/// Path to the config file (honours `--config`).
pub fn config_path(override_path: Option<&str>) -> PathBuf {
    match override_path {
        Some(p) => PathBuf::from(p),
        None => peerbeam_platform::config_dir().join("config.json"),
    }
}

/// This device's ephemeral id for a one-shot CLI run.
pub fn device_id() -> DeviceId {
    DeviceId::from(format!("cli-{}", std::process::id()))
}

/// A `Device` describing this machine, from config + platform.
pub fn me(config: &EngineConfig) -> Device {
    Device {
        id: device_id(),
        name: config.device.name.clone(),
        device_type: DeviceType::Desktop,
        platform: peerbeam_platform::current(),
        addresses: vec![],
        port: config.transfer.port,
        last_seen: chrono::Utc::now(),
    }
}

/// Build an engine with every discovery provider wired. mDNS is skipped if its
/// daemon can't start; Tailscale is scan-only and harmless when absent.
pub fn build_engine(config: EngineConfig) -> Engine {
    let id = device_id();
    let mut builder =
        EngineBuilder::new(config).with_discovery(Arc::new(UdpDiscovery::new(id.clone())));

    if let Ok(mdns) = MdnsDiscovery::new(id.clone()) {
        builder = builder.with_discovery(Arc::new(mdns));
    }
    builder = builder.with_discovery(Arc::new(TailscaleDiscovery::new(TsConfig::default())));

    // No required singleton providers, so build never fails here.
    builder.build().expect("engine builds")
}
