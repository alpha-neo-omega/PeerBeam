//! Owns the single global tokio runtime + `Engine` (the source of truth) and
//! the high-level operations the FFI surface wraps. All async work lives here;
//! FFI functions are thin and non-blocking (discovery start/stop are quick).

use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};
use tokio::runtime::{Builder, Runtime};

use peerbeam_config::EngineConfig;
use peerbeam_discovery_mdns::MdnsDiscovery;
use peerbeam_discovery_tailscale::{Config as TsConfig, TailscaleDiscovery};
use peerbeam_discovery_udp::UdpDiscovery;
use peerbeam_domain::entity::{Device, DeviceType};
use peerbeam_domain::id::DeviceId;
use peerbeam_engine::{Engine, EngineBuilder};

use crate::dto::DeviceDto;
use crate::error::Code;
use crate::{dto, events};

static RT: OnceLock<Runtime> = OnceLock::new();
static ENGINE: Mutex<Option<Arc<Engine>>> = Mutex::new(None);
static ME: Mutex<Option<Device>> = Mutex::new(None);

type OpResult = Result<Value, (Code, String)>;

/// The shared multi-thread runtime (created on first use).
fn rt() -> &'static Runtime {
    RT.get_or_init(|| {
        Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
    })
}

fn engine() -> Result<Arc<Engine>, (Code, String)> {
    ENGINE
        .lock()
        .unwrap()
        .clone()
        .ok_or((Code::NotInitialised, "engine not initialised".into()))
}

fn device_id() -> DeviceId {
    DeviceId::from(format!("app-{}", std::process::id()))
}

fn me(config: &EngineConfig) -> Device {
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

/// Initialise the runtime + engine and start the event forwarder.
pub fn init(config_json: &str) -> OpResult {
    let config: EngineConfig = if config_json.trim().is_empty() {
        EngineConfig::default()
    } else {
        serde_json::from_str(config_json)
            .map_err(|e| (Code::InvalidArgument, format!("bad config json: {e}")))?
    };

    let id = device_id();
    let mut builder =
        EngineBuilder::new(config.clone()).with_discovery(Arc::new(UdpDiscovery::new(id.clone())));
    if let Ok(mdns) = MdnsDiscovery::new(id.clone()) {
        builder = builder.with_discovery(Arc::new(mdns));
    }
    builder = builder.with_discovery(Arc::new(TailscaleDiscovery::new(TsConfig::default())));
    let engine = Arc::new(builder.build().map_err(crate::error::from_engine)?);

    // Forward device-list changes to Dart as events (no polling).
    let mut changes = engine.device_changes();
    rt().spawn(async move {
        while let Ok(change) = changes.recv().await {
            events::emit(&dto::device_event(&change));
        }
    });

    *ME.lock().unwrap() = Some(me(&config));
    *ENGINE.lock().unwrap() = Some(engine);
    Ok(json!({ "initialised": true }))
}

/// Stop work and release the engine.
pub fn shutdown() {
    if let Ok(engine) = engine() {
        let _ = rt().block_on(engine.stop_discovery());
    }
    *ENGINE.lock().unwrap() = None;
    *ME.lock().unwrap() = None;
}

pub fn discovery_start() -> OpResult {
    let engine = engine()?;
    let me = ME
        .lock()
        .unwrap()
        .clone()
        .ok_or((Code::NotInitialised, "no local identity".into()))?;
    rt().block_on(engine.start_discovery(me))
        .map_err(crate::error::from_engine)?;
    Ok(json!({ "discovering": true }))
}

pub fn discovery_stop() -> OpResult {
    let engine = engine()?;
    rt().block_on(engine.stop_discovery())
        .map_err(crate::error::from_engine)?;
    Ok(json!({ "discovering": false }))
}

pub fn devices() -> OpResult {
    let engine = engine()?;
    let list: Vec<DeviceDto> = engine.devices().iter().map(DeviceDto::from).collect();
    Ok(json!({ "devices": list }))
}
