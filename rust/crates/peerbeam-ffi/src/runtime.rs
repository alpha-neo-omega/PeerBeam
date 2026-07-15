//! Owns the single global tokio runtime + `Engine` (the source of truth) and
//! the high-level operations the FFI surface wraps. All async work lives here;
//! FFI functions are thin and non-blocking (discovery start/stop are quick).

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};
use tokio::runtime::{Builder, Runtime};

use peerbeam_config::EngineConfig;
use peerbeam_crypto::AeadCrypto;
use peerbeam_discovery_mdns::MdnsDiscovery;
use peerbeam_discovery_tailscale::{Config as TsConfig, TailscaleDiscovery};
use peerbeam_discovery_udp::UdpDiscovery;
use peerbeam_domain::entity::{Device, DeviceType};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::EncryptionProvider;
use peerbeam_engine::{Engine, EngineBuilder, RouteManager};
use peerbeam_transfer::Identity;
use peerbeam_transfer_quic::QuicTransport;
use peerbeam_trust_fs::FsTrust;

use crate::dto::DeviceDto;
use crate::error::Code;
use crate::transfer::Manager;
use crate::{dto, events};

static RT: OnceLock<Runtime> = OnceLock::new();
static ENGINE: Mutex<Option<Arc<Engine>>> = Mutex::new(None);
static ME: Mutex<Option<Device>> = Mutex::new(None);
static MANAGER: Mutex<Option<Arc<Manager>>> = Mutex::new(None);

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

/// Spawn a background task on the shared runtime.
pub fn spawn<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    rt().spawn(future);
}

/// Spawn a task and return its handle (so it can be aborted — e.g. the daemon).
pub fn spawn_handle<F>(future: F) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    rt().spawn(future)
}

/// Recover a poisoned lock instead of panicking. These statics hold only an
/// `Option<Arc<…>>`; a panic in some unrelated call while the lock was held must
/// not brick every subsequent FFI call by poisoning the mutex forever.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The transfer manager, if initialised.
pub fn manager() -> Result<Arc<Manager>, (Code, String)> {
    lock(&MANAGER)
        .clone()
        .ok_or((Code::NotInitialised, "engine not initialised".into()))
}

fn engine() -> Result<Arc<Engine>, (Code, String)> {
    lock(&ENGINE)
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
    let mut config: EngineConfig = if config_json.trim().is_empty() {
        EngineConfig::default()
    } else {
        serde_json::from_str(config_json)
            .map_err(|e| (Code::InvalidArgument, format!("bad config json: {e}")))?
    };

    // quinn (QUIC) endpoint creation + spawns require a tokio runtime context;
    // init runs on the caller's (Dart) thread, so enter the runtime here.
    let _guard = rt().enter();

    // Capture engine logs + point settings storage at the data directory,
    // then overlay the user's persisted settings (device name, save dir,
    // auto-accept) so they actually take effect.
    crate::logs::install();
    crate::settings::configure(&config.storage.data_directory);
    crate::settings::overlay(&mut config);

    let id = device_id();
    let mut builder =
        EngineBuilder::new(config.clone()).with_discovery(Arc::new(UdpDiscovery::new(id.clone())));
    if let Ok(mdns) = MdnsDiscovery::new(id.clone()) {
        builder = builder.with_discovery(Arc::new(mdns));
    }
    builder = builder.with_discovery(Arc::new(TailscaleDiscovery::new(TsConfig {
        peer_port: config.transfer.port,
        ..TsConfig::default()
    })));
    let engine = Arc::new(builder.build().map_err(crate::error::from_engine)?);

    // Forward device-list changes to Dart as events (no polling).
    let mut changes = engine.device_changes();
    rt().spawn(async move {
        while let Ok(change) = changes.recv().await {
            events::emit(&dto::device_event(&change));
        }
    });

    // Transfer manager: its own QUIC transport (dial + serve) + identity.
    let quic = Arc::new(QuicTransport::new().map_err(crate::error::from_domain)?);
    let route_manager = Arc::new(RouteManager::new(quic.clone()));
    let enc = Arc::new(AeadCrypto::new());
    let keypair = enc.generate_keypair();
    let identity = Identity {
        device_id: id.clone(),
        name: config.device.name.clone(),
        keypair,
    };
    let trust_path = std::path::Path::new(&config.storage.data_directory).join("trust.json");
    let trust = Arc::new(FsTrust::open(trust_path).map_err(crate::error::from_domain)?);
    let manager = Arc::new(Manager::new(
        route_manager,
        quic,
        enc,
        trust,
        identity,
        config.storage.save_directory.clone(),
        config.device.auto_accept_trusted,
        config.transfer.chunk_size as u32,
        config.transfer.port,
        Some(std::path::Path::new(&config.storage.data_directory).join("history.json")),
    ));

    // Start the receive server (the "daemon") so accept/reject have incoming
    // transfers; controllable via pb_daemon_*.
    let _ = manager.start_daemon();

    *lock(&ME) = Some(me(&config));
    *lock(&ENGINE) = Some(engine);
    *lock(&MANAGER) = Some(manager);
    Ok(json!({ "initialised": true }))
}

/// Aggregate runtime status.
pub fn status() -> OpResult {
    let engine = engine()?;
    let manager = crate::runtime::manager()?;
    Ok(json!({
        "runtime": "running",
        "build": crate::status::build_info(),
        "devices": engine.devices().len(),
        "active_transfers": manager.active_len(),
        "daemon": manager.daemon_status(),
        "memory_bytes": crate::status::rss_bytes(),
    }))
}

/// Stop work and release the engine.
pub fn shutdown() {
    if let Ok(engine) = engine() {
        let _ = rt().block_on(engine.stop_discovery());
    }
    // Stop the daemon task explicitly: it holds its own `Arc<Manager>`, so
    // merely dropping the global handle below would leave it running and the
    // QUIC port bound — a later `pb_init()` would then fail to rebind.
    if let Ok(manager) = manager() {
        let _ = manager.stop_daemon();
    }
    *lock(&ENGINE) = None;
    *lock(&ME) = None;
    *lock(&MANAGER) = None;
}

pub fn discovery_start() -> OpResult {
    let engine = engine()?;
    let me = lock(&ME)
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
