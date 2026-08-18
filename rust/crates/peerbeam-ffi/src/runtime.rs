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
use peerbeam_discovery_udp::{Config as UdpConfig, UdpDiscovery};
use peerbeam_domain::entity::{Device, DeviceType};
use peerbeam_domain::event::DeviceChange;
use peerbeam_domain::id::DeviceId;
use peerbeam_engine::{Engine, EngineBuilder, RouteManager, SessionDiagnostics};
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
/// PeerSession diagnostics: the single, shared source of truth for live
/// session/channel/transport/recovery state that the `pb_session_*` /
/// `pb_channels_json` / `pb_migration_json` / `pb_recovery_json` /
/// `pb_diagnostics_json` calls read. Reuses the engine's `SessionDiagnostics` —
/// no duplicated state.
static DIAGNOSTICS: Mutex<Option<Arc<SessionDiagnostics>>> = Mutex::new(None);
/// The concrete UDP discovery provider, held alongside the `Arc<dyn
/// DiscoveryProvider>` clone registered with the engine. `DiscoveryProvider`
/// has no `bound_port()` of its own (only `UdpDiscovery` does — it is
/// meaningless for e.g. Tailscale), and the bound port can only be read
/// *after* the socket exists, i.e. after `start_discovery` has run — so
/// `discovery_start` reaches through this static rather than the engine.
static UDP_DISCOVERY: Mutex<Option<Arc<UdpDiscovery>>> = Mutex::new(None);
/// Tracks whether discovery is currently running, so a live rename knows
/// whether to re-announce (no equivalent query exists on `Engine` itself).
static DISCOVERING: AtomicBool = AtomicBool::new(false);
/// Handle to the background chat drain task ([`chat_drain_loop`]), spawned
/// fresh in [`init`]. It holds its own `Arc<Engine>`/`Arc<Manager>` clones
/// (independent of the statics above), so [`shutdown`] must explicitly abort
/// it — otherwise it would keep ticking, and keep that whole engine graph
/// alive, forever across every `pb_init`/`pb_shutdown` cycle.
static CHAT_DRAIN: Mutex<Option<tokio::task::JoinHandle<()>>> = Mutex::new(None);

type OpResult = Result<Value, (Code, String)>;

/// How often the background chat drain sweeps the outbox for peers that have
/// since become reachable. See [`chat_drain_loop`].
const DRAIN_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

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

/// Block on `fut` on the shared runtime, whether or not a tokio context is
/// already entered (mirrors [`shutdown`]'s re-entrancy handling). Used by the
/// synchronous diagnostics FFI to read an async channel snapshot.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => rt().block_on(fut),
    }
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

/// The PeerSession diagnostics, if initialised (M8).
pub fn diagnostics() -> Result<Arc<SessionDiagnostics>, (Code, String)> {
    lock(&DIAGNOSTICS)
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
    // Auto-save rules, live: an edit in Settings must apply to the next file
    // that arrives, not the next launch. Only when the key is present, so
    // toggling an unrelated setting cannot clear the list — every other branch
    // here is guarded the same way.
    if partial.get(crate::rules::RULES_KEY).is_some() {
        m.set_save_rules(crate::rules::from_settings(partial));
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

fn me(config: &EngineConfig, device_id: &DeviceId) -> Device {
    Device {
        id: device_id.clone(),
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

/// Periodically deliver queued chat messages to peers that are now reachable.
/// Lives here so it can hold the engine (for `devices()`) and the manager (for
/// dial+flush) directly. Keep-forever: an unreachable peer's queue is retried
/// every tick, never dropped.
async fn chat_drain_loop(engine: Arc<Engine>, manager: Arc<Manager>) {
    let mut ticker = tokio::time::interval(DRAIN_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let peers = manager.chat_outbox_peers();
        if peers.is_empty() {
            continue;
        }
        let online = engine.devices();
        for peer in peers {
            // Resolve the peer's current address from discovery; skip if offline.
            let Some(md) = online.iter().find(|m| m.device.id == peer && m.online) else {
                continue;
            };
            if md.device.addresses.is_empty() || md.device.port == 0 {
                continue;
            }
            let _ = manager.chat_flush_peer(md.device.clone()).await;
        }
    }
}

/// Settle chat records left mid-flight by a crash or a hard restart.
///
/// A file row is written `Transferring` (sender) or `PendingApproval`
/// (receiver) and is only ever moved off that state by a live transfer event.
/// Transfer ids are process-scoped and nothing replays them, so a row that
/// survives a restart in either state would spin forever with no event coming;
/// `reconcile_peer` flips those to `Interrupted`.
///
/// **Every conversation, not just the queued ones.** This enumerates
/// [`ChatStore::conversations`], which is derived from the namespaces that
/// actually exist, so a thread is reconciled because it is *there* — not
/// because something happens to still be queued for it.
///
/// It used to enumerate `ChatStore::outbox_peers`, i.e. peers with queued
/// **text**, because the `AppStore` port had no way to list namespaces. That
/// left a hole exactly where this feature puts weight: a peer whose only
/// unsettled row is a *file* has no queued text at all, so its `Transferring`
/// row was never settled at startup and spun forever. Nothing is lost by the
/// change — a peer with a queued entry but no conversation namespace (a queued
/// decline writes an entry and no row of its own) has no history to reconcile
/// in the first place.
///
/// [`crate::pb_chat_reconcile`] remains the per-thread entry point a surface
/// calls when it opens a conversation; it additionally skips any row whose
/// transfer is live, which by then one can be.
fn reconcile_chat(chat: &peerbeam_chat::ChatStore) {
    let peers = match chat.conversations() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "chat reconcile skipped: conversations unreadable");
            return;
        }
    };
    for peer in peers {
        match chat.reconcile_peer(&peer) {
            Ok(0) => {}
            Ok(n) => tracing::info!(peer = %peer.0, count = n, "chat records marked interrupted"),
            Err(e) => tracing::warn!(error = %e, peer = %peer.0, "chat reconcile failed"),
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

    // Load the device's persistent identity once, up front: the same
    // `device_id`/keypair drive discovery (below), `me()`, and the transfer
    // manager's mutual-auth handshake — there must be exactly one source of
    // truth, not an ephemeral `app-<pid>` id plus a throwaway keypair.
    let enc = Arc::new(AeadCrypto::new());
    let identity_path = std::path::Path::new(&config.storage.data_directory).join("identity.json");
    let identity = peerbeam_transfer::load_or_generate(
        &peerbeam_identity_fs::FsIdentity::open(identity_path),
        enc.as_ref(),
        config.device.name.clone(),
    )
    .map_err(crate::error::from_domain)?;
    let device_id = identity.device_id.clone();

    // `with_config`, not `new`: the port is configurable
    // (`config.discovery.port`, default `DEFAULT_DISCOVERY_PORT`) so a
    // caller — namely our own test suite — can ask for an OS-assigned one
    // instead of the well-known port another process on the machine may
    // already hold. The concrete `Arc<UdpDiscovery>` is kept in `UDP_DISCOVERY`
    // (alongside the `Arc<dyn DiscoveryProvider>` clone handed to the engine
    // below) so `discovery_start` can read back the port actually bound.
    let udp_discovery = Arc::new(UdpDiscovery::with_config(
        device_id.clone(),
        UdpConfig {
            port: config.discovery.port,
            ..UdpConfig::default()
        },
    ));
    let mut builder = EngineBuilder::new(config.clone()).with_discovery(udp_discovery.clone());
    if let Ok(mdns) = MdnsDiscovery::new(device_id.clone()) {
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
    let trust_path = std::path::Path::new(&config.storage.data_directory).join("trust.json");
    let trust = Arc::new(FsTrust::open(trust_path).map_err(crate::error::from_domain)?);

    // Encrypted local AppStore for capability data (chat history first; future
    // capabilities share the same store under their own namespace). The data
    // key is derived from the device identity secret, not reused as-is, so a
    // leaked appstore key can never be turned back into the identity secret.
    let appstore_root = std::path::Path::new(&config.storage.data_directory).join("appstore");
    let chat_key =
        peerbeam_crypto::derive_subkey(&identity.keypair.secret.0, b"peerbeam-appstore-v1");
    let appstore: Arc<dyn peerbeam_domain::port::AppStore> = Arc::new(
        peerbeam_appstore_fs::FsAppStore::open(appstore_root, chat_key, enc.clone()),
    );
    let chat = peerbeam_chat::ChatStore::new(appstore);
    reconcile_chat(&chat);

    // The outbox's own copy of every queued file. A sibling of `appstore`
    // rather than a directory inside it: `FsAppStore` owns that tree and lists
    // it by namespace, and a blob is not a record — it is plaintext user
    // content, written `0600`, deleted the moment its entry settles.
    let staging = Arc::new(peerbeam_chat::StagingStore::new(
        std::path::Path::new(&config.storage.data_directory)
            .join("outbox-blobs")
            .to_string_lossy()
            .into_owned(),
        Arc::new(peerbeam_storage_fs::FsStorage::new()),
    ));
    let staging_limits = peerbeam_chat::StagingLimits {
        max_bytes: config.device.max_queued_file_bytes,
        min_free_bytes: config.device.min_free_bytes,
    };

    // Bytes staged by a run that crashed between staging and enqueue are owned
    // by nothing: nothing will ever send them and nothing will ever delete
    // them, so without this they sit on disk forever.
    //
    // `outbox_owned_blobs` — not the ordinary outbox readers — is what makes
    // this safe to run at boot. `sweep` deletes every blob its `keep` set does
    // not name, and the ordinary readers deliberately *skip* a row they cannot
    // decode, so a wholly-unreadable outbox would hand us an empty set that is
    // indistinguishable from an empty queue and take every queued file with
    // it. The strict reader refuses instead, and a refusal here means we sweep
    // nothing at all this run: leaking bytes is recoverable, deleting the
    // user's queued file is not.
    match chat.outbox_owned_blobs() {
        Ok(owned) => {
            let swept = staging.sweep(&owned);
            if swept > 0 {
                tracing::info!(count = swept, "removed orphaned staged files");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "orphan sweep skipped: the outbox is not fully readable");
        }
    }

    let manager = Arc::new(Manager::new(
        route_manager,
        quic,
        enc,
        trust,
        chat,
        staging,
        staging_limits,
        identity,
        config.storage.save_directory.clone(),
        // Auto-save rules, already overlaid from the persisted settings
        // document above. Empty for every install that has never opened the
        // section — and empty means "the save directory", i.e. the behaviour
        // that shipped before rules existed.
        config.storage.rules.clone(),
        config.device.auto_accept_trusted,
        // Clamp BEFORE the u32 cast: a configured chunk_size >= 2^32 would
        // otherwise truncate (e.g. 2^32 -> 0), and Manager::new's max(1) guard
        // runs after the cast, yielding a 1-byte chunk size. Mirrors the CLI's
        // clamp_chunk_size so both frontends behave identically.
        config.transfer.chunk_size.clamp(1, u32::MAX as u64) as u32,
        config.transfer.port,
        Some(std::path::Path::new(&config.storage.data_directory).join("history.json")),
    ));

    // Start the receive server (the "daemon") so accept/reject have incoming
    // transfers; controllable via pb_daemon_*. Propagate failure instead of
    // discarding it — otherwise init() would report `{"initialised": true}`
    // while incoming transfers silently have no listener.
    manager.start_daemon()?;

    // The engine's single source of truth for live PeerSession status. Empty
    // until sessions run; the additive diagnostics FFI reads it (M8).
    let diagnostics = Arc::new(SessionDiagnostics::new());

    // Background chat drain: periodically retries delivery to any peer whose
    // outbox still has queued messages once discovery reports it reachable.
    // Clone before `engine`/`manager` are moved into the statics below. The
    // handle is stashed in `CHAT_DRAIN` so `shutdown()` can abort it — see
    // that static's doc comment for why this matters.
    let drain_handle = rt().spawn(chat_drain_loop(engine.clone(), manager.clone()));

    *lock(&ME) = Some(me(&config, &device_id));
    *lock(&ENGINE) = Some(engine);
    *lock(&MANAGER) = Some(manager);
    *lock(&DIAGNOSTICS) = Some(diagnostics);
    *lock(&CHAT_DRAIN) = Some(drain_handle);
    *lock(&UDP_DISCOVERY) = Some(udp_discovery);
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
    // Abort the chat drain before releasing the engine/manager statics: it
    // holds its own `Arc<Engine>`/`Arc<Manager>` clones (captured at spawn
    // time, not borrowed from these statics), so without this the Engine
    // could never actually drop once `ENGINE` below is cleared — which in
    // turn used to keep `forward_device_changes` running past shutdown too,
    // since it only stops once the Engine's broadcast `Sender` (owned by the
    // Engine) is dropped.
    if let Some(handle) = lock(&CHAT_DRAIN).take() {
        handle.abort();
    }
    *lock(&ENGINE) = None;
    *lock(&ME) = None;
    *lock(&MANAGER) = None;
    *lock(&DIAGNOSTICS) = None;
    *lock(&UDP_DISCOVERY) = None;
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

    // Additive: `bound_port()` only resolves once `start_discovery` has bound
    // the socket (above), which is why this reads it here rather than at
    // construction. Lets a caller that requested port `0` (an OS-assigned
    // port — e.g. our own test suite) learn what it actually got; existing
    // callers that only look at `discovering` are unaffected.
    let mut result = json!({ "discovering": true });
    if let Some(port) = lock(&UDP_DISCOVERY).as_ref().and_then(|d| d.bound_port()) {
        result["port"] = json!(port);
    }
    Ok(result)
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

    /// `peerbeam-config` declares its default discovery port as a bare literal
    /// (`49500`) rather than depending on `peerbeam-discovery-udp` just to
    /// name the constant — see that field's `Default` impl for why. This
    /// crate already depends on both, so it is where the two are guarded from
    /// drifting apart: if either default ever changes without the other, this
    /// fails loudly instead of the two quietly disagreeing at runtime (an
    /// existing config's "default" port silently no longer matching the
    /// provider's own default).
    #[test]
    fn discovery_config_default_port_matches_udp_default() {
        assert_eq!(
            EngineConfig::default().discovery.port,
            peerbeam_discovery_udp::DEFAULT_DISCOVERY_PORT
        );
    }

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

    /// Regression guard for the chat-drain shutdown leak: `chat_drain_loop`
    /// captures its own `Arc<Engine>`/`Arc<Manager>` clones at spawn time,
    /// independent of the `ENGINE`/`MANAGER` statics, so if `shutdown()`
    /// only cleared those statics without also aborting the drain task, the
    /// Engine could never actually deallocate — every `pb_init`/`pb_shutdown`
    /// cycle (e.g. a Flutter hot-restart) would leak the whole engine graph
    /// and leave a stale drain ticking forever. A `Weak` reference lets this
    /// assert on that real, load-bearing outcome (the Engine drops) instead
    /// of only "`shutdown()` completes", which a leak wouldn't fail either.
    ///
    /// `#[serial_test::serial]` (same convention as `lib.rs`'s own
    /// init/shutdown tests): this drives the real global `ENGINE`/`MANAGER`/
    /// `CHAT_DRAIN` statics, so it must not run concurrently with any other
    /// test that also calls `init`/`shutdown` on them.
    #[test]
    #[serial_test::serial]
    fn shutdown_drops_the_chat_drain_and_lets_the_engine_deallocate() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = EngineConfig::default();
        cfg.storage.data_directory = dir.path().join("data").to_string_lossy().into_owned();
        cfg.storage.save_directory = dir.path().join("recv").to_string_lossy().into_owned();
        std::fs::create_dir_all(dir.path().join("recv")).unwrap();
        cfg.transfer.port = 0; // OS-assigned; this test never dials/serves real traffic.
        let cfg_json = serde_json::to_string(&cfg).unwrap();

        let res = init(&cfg_json);
        assert!(res.is_ok(), "init: {res:?}");

        let weak_engine = Arc::downgrade(&engine().expect("engine initialised after init()"));
        assert!(
            weak_engine.upgrade().is_some(),
            "engine should be alive right after init()"
        );
        assert!(
            lock(&CHAT_DRAIN).is_some(),
            "init() should stash the drain task's handle in CHAT_DRAIN"
        );

        shutdown();

        assert!(
            lock(&CHAT_DRAIN).is_none(),
            "shutdown() must clear CHAT_DRAIN (and abort the task it held)"
        );

        // Aborting a tokio task is cooperative cancellation: the runtime
        // drops the task's captured state (including its Arc<Engine> clone)
        // on its next poll, not synchronously inside `abort()` itself — so
        // poll briefly (bounded, not a fixed sleep) rather than asserting
        // immediately.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while weak_engine.upgrade().is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            weak_engine.upgrade().is_none(),
            "Engine should deallocate once shutdown() aborts the chat drain — its last Arc holder"
        );
    }

    /// The hole the old startup reconciliation left, guarded where the change
    /// actually is.
    ///
    /// `reconcile_chat` used to enumerate `ChatStore::outbox_peers` — peers
    /// with something *queued*, which before 2b could only be text. A thread
    /// whose only unsettled row is a **file**, with nothing queued at all, was
    /// therefore never reached: its `Transferring` row survived the restart
    /// and spun forever, showing an eternal progress bar for a transfer whose
    /// process no longer exists. It now enumerates `conversations()`, derived
    /// from the namespaces that actually exist, so the thread is reconciled
    /// because it is there.
    ///
    /// The `outbox_peers` assertion below is what makes this discriminating:
    /// it pins that the old enumeration genuinely had no peer to hand.
    #[test]
    fn reconcile_chat_settles_a_thread_whose_only_unsettled_row_is_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn peerbeam_domain::port::EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = peerbeam_crypto::derive_subkey(&[7u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn peerbeam_domain::port::AppStore> = Arc::new(
            peerbeam_appstore_fs::FsAppStore::open(dir.path().join("appstore"), key, enc),
        );
        let chat = peerbeam_chat::ChatStore::new(app);

        // An in-flight outgoing file and NOTHING queued — the exact shape the
        // old enumeration could not name.
        let peer = DeviceId::from("pb-file-only");
        let r = peerbeam_chat::FileRef::new("report.pdf", 4096).unwrap();
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &peer,
            &r,
            peerbeam_chat::FileMeta::new(&r.name, 4096, None),
            peerbeam_chat::Status::Transferring,
        ))
        .unwrap();
        assert!(
            chat.outbox_peers().unwrap().is_empty(),
            "nothing is queued, so the old enumeration had no peer to reconcile"
        );

        reconcile_chat(&chat);

        assert_eq!(
            chat.get(&peer, &r.id).unwrap().unwrap().status,
            peerbeam_chat::Status::Interrupted,
            "a restart must settle a row that no event will ever finish"
        );
    }
}
