//! Owns the single global tokio runtime + `Engine` (the source of truth) and
//! the high-level operations the FFI surface wraps. All async work lives here;
//! FFI functions are thin and non-blocking (discovery start/stop are quick).

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::broadcast::{self, error::RecvError};

use peerbeam_config::EngineConfig;
use peerbeam_crypto::AeadCrypto;
use peerbeam_discovery_mdns::MdnsDiscovery;
use peerbeam_discovery_tailscale::{Config as TsConfig, TailscaleDiscovery};
use peerbeam_discovery_udp::UdpDiscovery;
use peerbeam_domain::entity::{Device, DeviceType};
use peerbeam_domain::event::DeviceChange;
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
/// Tracks whether discovery is currently running, so a live rename knows
/// whether to re-announce (no equivalent query exists on `Engine` itself).
static DISCOVERING: AtomicBool = AtomicBool::new(false);

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

/// Push a persisted settings delta into the running engine so it takes effect
/// without a restart. Only the keys present in `partial` are applied; a no-op
/// when the engine isn't initialised (nothing to update). Called from
/// `settings::set`/`reset` after the change is persisted.
pub fn apply_live_settings(partial: &Value) {
    let Ok(m) = manager() else { return };
    if let Some(d) = partial.get("transfer_directory").and_then(|v| v.as_str()) {
        let d = d.trim();
        if !d.is_empty() {
            m.set_save_dir(d.to_string());
        }
    }
    if let Some(a) = partial.get("auto_accept").and_then(|v| v.as_bool()) {
        m.set_auto_accept(a);
    }
    if let Some(name) = partial.get("device_name").and_then(|v| v.as_str()) {
        let name = name.trim();
        if !name.is_empty() {
            apply_live_device_name(&m, name);
        }
    }
}

/// Rename the running device live: update the transfer identity (so the next
/// handshake presents the new name) and re-announce to discovery peers.
///
/// Best-effort: the engine may not be initialised yet (nothing to update) or
/// discovery may not be running (nothing to re-announce) — both are no-ops,
/// not errors.
fn apply_live_device_name(m: &Arc<Manager>, name: &str) {
    m.set_identity_name(name.to_string());

    let Some(mut me) = lock(&ME).clone() else {
        return;
    };
    if me.name == name {
        return;
    }
    me.name = name.to_string();
    *lock(&ME) = Some(me.clone());

    // Re-announce so peers see the new name — but only if discovery is
    // actually running; otherwise this would have the side effect of
    // starting it. `UdpDiscovery`/`MdnsDiscovery` snapshot the `me` passed to
    // `advertise()` once and no-op on a second call while already advertising
    // (see their `advertising` guard), so a plain `start_discovery(me)` would
    // not propagate the rename — restart discovery so `advertise()` runs
    // again with the updated device.
    if DISCOVERING.load(Ordering::SeqCst) {
        if let Ok(engine) = engine() {
            rt().block_on(async {
                let _ = engine.stop_discovery().await;
                let _ = engine.start_discovery(me).await;
            });
        }
    }
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

/// Forward device-change broadcasts to `emit` until the channel closes.
///
/// `broadcast::Receiver::recv()` can return `Err(Lagged(n))` — a RECOVERABLE
/// error meaning the sender outran this receiver's buffer and `n`
/// intermediate changes were dropped — whenever a burst (e.g. a large network
/// coming online, or an `offline_all()` storm on stop/rename-restart) emits
/// more than the channel capacity while the consumer is briefly behind. That
/// is distinct from `Err(Closed)`, which means every sender was dropped and
/// the stream is truly finished. Treating `Lagged` as terminal would silently
/// end device-list updates for the rest of the process; only `Closed` ends
/// the loop. On `Lagged` we also emit a resync hint so the consumer can
/// re-pull the authoritative list and recover the dropped transitions.
async fn forward_device_changes(
    mut changes: broadcast::Receiver<DeviceChange>,
    emit: impl Fn(&Value),
) {
    loop {
        match changes.recv().await {
            Ok(change) => emit(&dto::device_event(&change)),
            Err(RecvError::Lagged(_)) => {
                emit(&dto::device_resync_event());
                continue;
            }
            Err(RecvError::Closed) => break,
        }
    }
}

/// Initialise the runtime + engine and start the event forwarder.
///
/// Idempotent: a second call without an intervening [`shutdown`] (e.g. a
/// Flutter hot-restart re-entering `pb_init`) tears down the previous
/// engine/daemon first. Without this, the old daemon task — which holds its
/// own `Arc<Manager>` — keeps running and keeps the QUIC transfer port bound,
/// so the new daemon's bind would fail (silently, since `start_daemon`'s
/// result used to be discarded) while the statics were overwritten to point
/// at the new, half-working instance.
pub fn init(config_json: &str) -> OpResult {
    if lock(&ENGINE).is_some() {
        shutdown();
    }

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
    let changes = engine.device_changes();
    rt().spawn(forward_device_changes(changes, events::emit));

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
    // transfers; controllable via pb_daemon_*. Propagate failure instead of
    // discarding it — otherwise init() would report `{"initialised": true}`
    // while incoming transfers silently have no listener.
    manager.start_daemon()?;

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
        match tokio::runtime::Handle::try_current() {
            // The calling thread already has a tokio context entered (e.g.
            // `init`'s idempotent teardown re-entering `shutdown` from a test
            // harness that drives `pb_init`/`pb_shutdown` from inside an
            // `async fn`; real Dart callers never have one). `rt().block_on`
            // would panic ("cannot start a runtime from within a runtime"),
            // so drive the future on the already-current handle instead —
            // `block_in_place` makes it legal to block this worker thread.
            Ok(handle) => {
                let _ = tokio::task::block_in_place(|| handle.block_on(engine.stop_discovery()));
            }
            Err(_) => {
                let _ = rt().block_on(engine.stop_discovery());
            }
        }
    }
    DISCOVERING.store(false, Ordering::SeqCst);
    // Stop the daemon task explicitly: it holds its own `Arc<Manager>`, so
    // merely dropping the global handle below would leave it running and the
    // QUIC port bound — a later `pb_init()` would then fail to rebind.
    if let Ok(manager) = manager() {
        let _ = manager.stop_daemon();
    }
    *lock(&ENGINE) = None;
    *lock(&ME) = None;
    *lock(&MANAGER) = None;
    // Drain any in-flight emit() before returning: set_callback(None) takes
    // an exclusive lock that blocks until every emitter's shared (read) guard
    // has released, so once this returns no emitter can still be holding the
    // callback pointer Dart is about to free.
    crate::events::set_callback(None);
}

pub fn discovery_start() -> OpResult {
    let engine = engine()?;
    let me = lock(&ME)
        .clone()
        .ok_or((Code::NotInitialised, "no local identity".into()))?;
    rt().block_on(engine.start_discovery(me))
        .map_err(crate::error::from_engine)?;
    DISCOVERING.store(true, Ordering::SeqCst);
    Ok(json!({ "discovering": true }))
}

pub fn discovery_stop() -> OpResult {
    let engine = engine()?;
    rt().block_on(engine.stop_discovery())
        .map_err(crate::error::from_engine)?;
    DISCOVERING.store(false, Ordering::SeqCst);
    Ok(json!({ "discovering": false }))
}

pub fn devices() -> OpResult {
    let engine = engine()?;
    let list: Vec<DeviceDto> = engine.devices().iter().map(DeviceDto::from).collect();
    Ok(json!({ "devices": list }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// A burst larger than the broadcast channel's capacity must not kill the
    /// forwarder: `recv()` returns `Err(Lagged(_))` once the receiver falls
    /// behind, and the loop must emit a resync hint and keep going rather
    /// than treating it as terminal (the bug being fixed: the old `while let
    /// Ok` loop exited on the very first `Lagged` and never recovered).
    #[tokio::test]
    async fn forward_device_changes_continues_past_lagged_and_stops_on_closed() {
        // Capacity 2: sending 6 changes before anyone consumes guarantees the
        // receiver has lagged by the time it starts polling.
        let (tx, rx) = broadcast::channel(2);
        let received: Arc<StdMutex<Vec<Value>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink = received.clone();

        let id = DeviceId::from("dev-1");
        for _ in 0..5 {
            let _ = tx.send(DeviceChange::StatusChanged {
                id: id.clone(),
                online: true,
            });
        }
        // A uniquely identifiable change sent last, so we can confirm it was
        // still delivered *after* the lag.
        let _ = tx.send(DeviceChange::Removed(DeviceId::from("sentinel")));
        // Dropping every sender closes the channel once buffered items drain,
        // which is what lets the forwarder loop terminate below instead of
        // awaiting forever.
        drop(tx);

        forward_device_changes(rx, move |v: &Value| {
            sink.lock().unwrap().push(v.clone());
        })
        .await;

        let events = received.lock().unwrap();
        assert!(
            events.iter().any(|v| v["type"] == "device_resync"),
            "expected a resync hint after the Lagged burst, got: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|v| v["type"] == "device_removed" && v["id"] == "sentinel"),
            "expected the forwarder to keep delivering changes after Lagged, got: {events:?}"
        );
    }
}
