//! `peerbeam chat` — send / history / watch.
//!
//! Every chat session rides the same PeerSession machinery as file transfers
//! (`session_transfer`): the Chat capability is advertised on every dial and
//! every accept, so a `chat send` is accepted regardless of what the session
//! was otherwise established for. This module only resolves peers, wires the
//! store, and presents the result — the actual send/receive logic lives in
//! `peerbeam_chat` (`flush_to_session`, `ChatHandler`), reused unchanged.
//!
//! **Offline delivery (1b).** `send` always enqueues (`ChatStore::enqueue`)
//! before touching the network, then makes one opportunistic dial+flush —
//! an unreachable peer simply stays queued. `watch` and `serve_loop`
//! (`commands.rs`, shared by `receive`/`daemon start`) each run a periodic
//! [`drain_tick`] alongside their accept loop, and push a peer's queued
//! outbox the moment a session with them is accepted (flush-on-connect).
//! Every dial/accept in this file registers the *same* chat wiring
//! (`store` + [`received_sink`]) regardless of why the session was
//! established — a session with no `ChatHandler` on one side silently drops
//! any CHAT frame pushed to it from the other side, so symmetry here is a
//! correctness requirement, not a nicety (see the FFI's `Manager::chat_wiring`
//! for the bug this mirrors-and-avoids).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::json;

use peerbeam_chat::{flush_to_session, ChatRecord, ChatStore, Direction, ReceivedSink};
use peerbeam_config::EngineConfig;
use peerbeam_domain::id::DeviceId;
use peerbeam_engine::{Engine, ManagedDevice, RouteManager};
use peerbeam_transfer_quic::QuicTransport;

use crate::cli::ChatAction;
use crate::commands::{self, SecureCtx};
use crate::engine::{build_engine, me};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;
use crate::session_transfer;

pub async fn chat(ctx: &Ctx, action: ChatAction, path_override: Option<&str>) -> CliResult {
    match action {
        ChatAction::Send { to, addr, text } => send(ctx, to, addr, text, path_override).await,
        ChatAction::History { peer } => history(ctx, peer, path_override).await,
        ChatAction::Watch { port } => watch(ctx, port, path_override).await,
    }
}

/// How often a running host (`serve_loop` / `watch`) sweeps the outbox for
/// peers that have since become reachable. Mirrors the FFI's
/// `runtime::DRAIN_EVERY` (kept independent, not shared, since the two
/// frontends have no common crate to host a single constant in).
pub(crate) const DRAIN_EVERY: Duration = Duration::from_secs(15);

/// Build the `ReceivedSink` every accept and drain-tick dial registers: prints
/// (or, in `--json` mode, emits a `chat_received` line for) an inbound chat
/// message. Extracted from `watch`'s previously-inline closure so `watch`,
/// `serve_loop`, `send`'s opportunistic dial, and `drain_tick`'s per-peer
/// dials all notify identically — one sink shape, never a second one invented
/// per call site.
pub(crate) fn received_sink(ctx: &Ctx) -> ReceivedSink {
    // Captured by value (rather than `ctx` by reference) because the sink must
    // be `'static` — it outlives this call, held by the `ChatHandler` inside
    // each session's config.
    let json = ctx.json;
    let color = ctx.color;
    Arc::new(move |rec: ChatRecord| {
        if json {
            let line = json!({
                "event": "chat_received",
                "id": rec.id,
                "peer": rec.peer_id,
                "body": rec.body,
                "timestamp": rec.timestamp,
            });
            println!("{}", serde_json::to_string(&line).unwrap_or_default());
        } else {
            let line = format!("[{}] {}", rec.peer_id, rec.body);
            if color {
                println!("\x1b[32m{line}\x1b[0m");
            } else {
                println!("{line}");
            }
        }
    })
}

/// From `peers` (outbox peer ids) and the current device snapshot, select the
/// devices that are actually dialable right now: online, with at least one
/// advertised address and a nonzero port. Factored out of [`drain_tick`] so
/// this decision is unit-testable without a live session/dial.
fn reachable_targets<'a>(
    peers: &[DeviceId],
    online: &'a [ManagedDevice],
) -> Vec<&'a ManagedDevice> {
    peers
        .iter()
        .filter_map(|peer| {
            online
                .iter()
                .find(|m| m.device.id == *peer && m.online)
                .filter(|m| !m.device.addresses.is_empty() && m.device.port != 0)
        })
        .collect()
}

/// Periodically retry delivery of every peer's queued outbox messages once
/// discovery reports them reachable. Shared by `serve_loop` (`commands.rs`)
/// and `watch` — the periodic-drain half of offline delivery; the other half
/// is flush-on-connect (an explicit [`flush_to_session`] call right after each
/// accepted session, in both callers' accept arm).
///
/// `sink` must be the SAME [`ReceivedSink`] the caller registered on its
/// `accept` call: every dial here also registers a chat handler (`Some`, not
/// `None`), because a session we dial can just as easily have the peer push
/// something back (their own flush-on-connect, or a reply) that would
/// otherwise be silently dropped — see this module's top doc comment.
pub(crate) async fn drain_tick(
    engine: &Engine,
    store: &ChatStore,
    quic: &Arc<QuicTransport>,
    routes: &RouteManager,
    sc: &SecureCtx,
    sink: &ReceivedSink,
) {
    let peers = store.outbox_peers().unwrap_or_default();
    if peers.is_empty() {
        return; // common case: skip `engine.devices()` entirely.
    }
    let online = engine.devices();
    for md in reachable_targets(&peers, &online) {
        if let Ok(session) = session_transfer::dial(
            quic,
            routes,
            &md.device,
            "chat",
            &sc.ident,
            &sc.enc,
            &sc.trust,
            Some((store.clone(), sink.clone())),
        )
        .await
        {
            let p = DeviceId::from(session.peer_id.clone());
            let _ = flush_to_session(&session.handle, store, &p).await;
            session.close().await;
        }
    }
}

/// `chat send` — resolve the peer exactly like `commands::send`, dial a
/// PeerSession advertising Chat (no handler needed on this side — we only
/// send), and send one message.
async fn send(
    ctx: &Ctx,
    to: Option<String>,
    addr: Option<String>,
    text: String,
    path_override: Option<&str>,
) -> CliResult {
    let config = commands::load_config(path_override)?;

    // Resolve the target peer — directly (--addr) or via discovery. Mirrors
    // `commands::send`'s resolution exactly; not duplicated logic, just reuse
    // of the same helpers.
    let target = if let Some(addr) = &addr {
        let sa = commands::resolve_addr(addr)?;
        commands::target_device(addr.clone(), sa.ip().to_string(), sa.port())
    } else {
        let devices = commands::snapshot(config.clone(), 2).await;
        let candidates: Vec<(String, String)> = devices
            .iter()
            .map(|m| (m.device.id.to_string(), m.device.name.clone()))
            .collect();
        let index = commands::resolve_peer(ctx, &candidates, &to)?;
        let dev = devices[index].device.clone();
        if dev.addresses.is_empty() {
            return Err(CliError::NotFound(format!(
                "no reachable address for {}",
                dev.name
            )));
        }
        dev
    };

    if target.port == 0 {
        return Err(CliError::NotFound(format!(
            "{} did not advertise a transfer port",
            target.name
        )));
    }

    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);

    // Persist Pending + enqueue immediately: the message id is durable and
    // visible in `chat history` before this call ever touches the network —
    // mirrors the FFI's `chat_send` (offline-first send). Keyed by
    // `target.id`, which for a `--to` (discovery-resolved) peer is its real
    // device id, but for `--addr` is the routing placeholder `"addr"` — see
    // this fn's note near `target`'s resolution above. That means an `--addr`
    // send's opportunistic flush below (keyed by the *authenticated* peer)
    // will not find this entry even when the dial succeeds, so it always
    // reports "queued" for `--addr`; the message is not lost — `chat history
    // addr` still shows it Pending — but nothing currently reconciles it to
    // the authenticated peer's outbox. Only `--to` sends can be opportunistically
    // flushed by this call.
    let msg = peerbeam_chat::ChatMessage::new(&text).map_err(CliError::from)?;
    store.enqueue(&target.id, &msg).map_err(CliError::from)?;
    let id = msg.id.clone();

    // Opportunistic immediate delivery: dial once and flush this peer's
    // outbox (which now includes the message just enqueued, for `--to`
    // sends). If the peer is unreachable it stays queued for a running host
    // (daemon / chat watch) to drain later — non-fatal, mirrors the FFI's
    // `chat_send` opportunistic flush.
    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    let routes = RouteManager::new(quic.clone());
    let sink = received_sink(ctx);
    let delivered = match session_transfer::dial(
        &quic,
        &routes,
        &target,
        "chat",
        &sc.ident,
        &sc.enc,
        &sc.trust,
        Some((store.clone(), sink)),
    )
    .await
    {
        Ok(session) => {
            let newly_trusted = session.newly_trusted;
            let pairing_code = session.pairing_code.clone();
            if newly_trusted && !ctx.json {
                ctx.line(&ctx.dim(&format!("pinned new peer {}", session.peer_id)));
                ctx.line(&format!("  pairing code: {}", ctx.bold(&pairing_code)));
            }
            // The authenticated peer, not the (possibly placeholder, for
            // `--addr`) routing target id — chat delivery/history are
            // namespaced by device id, so using the routing id would let two
            // different `--addr` targets collide under (or split across) the
            // same conversation.
            let peer = DeviceId::from(session.peer_id.clone());
            let flushed = flush_to_session(&session.handle, &store, &peer)
                .await
                .unwrap_or_default();
            session.close().await;
            !flushed.is_empty()
        }
        Err(_) => false,
    };

    if ctx.json {
        ctx.json_line(&json!({
            "event": "chat_sent",
            "id": id,
            "peer": target.id.0,
            "delivered": delivered,
        }));
    } else if delivered {
        ctx.line(&ctx.green(&format!("sent to {}", target.name)));
    } else {
        ctx.line(&ctx.dim(&format!(
            "queued for {} (offline — a running daemon/watch will deliver)",
            target.name
        )));
    }
    Ok(())
}

/// `chat history <peer>` — print a conversation's persisted history. `peer`
/// may be a raw device id (`pb-<fingerprint>`) or a friendly name; see
/// [`resolve_history_peer`] for how the two are told apart.
async fn history(ctx: &Ctx, peer: String, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    let peer_id = resolve_history_peer(ctx, &config, &store, peer).await;
    let records = store
        .history(&peer_id)
        .map_err(|e| CliError::Other(e.to_string()))?;

    if ctx.json {
        ctx.json_line(&render_history_json(&records));
        return Ok(());
    }
    if records.is_empty() {
        ctx.line(&ctx.dim("no messages yet"));
        return Ok(());
    }
    for r in &records {
        let dir = match r.direction {
            Direction::Out => "out",
            Direction::In => "in",
        };
        ctx.line(&format!("[{}] {}: {}", r.timestamp, dir, r.body));
    }
    Ok(())
}

/// Resolve a `chat history` peer argument to a [`DeviceId`]. `peer` may
/// already be the raw device id chat history is actually keyed by
/// (`pb-<fingerprint>`), or a friendly name that only resolves while the
/// device is currently discoverable — history itself is local, on-disk data,
/// so a peer that's offline right now must still be readable by its id.
///
/// Order of attempts, cheapest first:
/// 1. Treat `peer` as a literal device id and check for existing history
///    under it. If there is any (or `peer` already looks like a `pb-` id),
///    use it directly — no discovery, no delay, and it's the only path that
///    works for a peer that isn't online right now.
/// 2. Otherwise, `peer` looks like a name that hasn't been seen locally yet
///    (e.g. this is the *first* `history` call for it) — spend a short
///    discovery window (mirrors `chat send`'s resolution) and try to resolve
///    it against currently-known devices.
/// 3. If that doesn't resolve either (not discoverable right now), fall back
///    to the literal id from step 1 (its — empty — history is the correct,
///    honest answer for "no conversation found").
async fn resolve_history_peer(
    ctx: &Ctx,
    config: &EngineConfig,
    store: &ChatStore,
    peer: String,
) -> DeviceId {
    let raw_id = DeviceId::from(peer.clone());
    let has_offline_history = store
        .history(&raw_id)
        .map(|h| !h.is_empty())
        .unwrap_or(false);
    if has_offline_history || peer.starts_with("pb-") {
        return raw_id;
    }

    let devices = commands::snapshot(config.clone(), 2).await;
    let candidates: Vec<(String, String)> = devices
        .iter()
        .map(|m| (m.device.id.to_string(), m.device.name.clone()))
        .collect();
    match commands::resolve_peer(ctx, &candidates, &Some(peer)) {
        Ok(index) => devices[index].device.id.clone(),
        Err(_) => raw_id, // not discoverable right now — read local history by the literal id.
    }
}

/// Render a conversation's records as `{"messages": [...]}` — the CLI's JSON
/// contract for `chat history --json`. Factored out so it's unit-testable
/// against a seeded store without a live PeerSession.
fn render_history_json(records: &[ChatRecord]) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = records
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "peer_id": r.peer_id,
                "direction": r.direction,
                "timestamp": r.timestamp,
                "body": r.body,
                "status": r.status,
            })
        })
        .collect();
    json!({ "messages": messages })
}

/// `chat watch` — serve inbound PeerSessions and print each received chat
/// message. Mirrors `commands::serve_loop`'s accept pattern (one connection at
/// a time, best-effort discoverability, no daemon IPC), but for Chat instead
/// of Transfer: there is no stream channel to await, since the `ChatHandler`
/// dispatches inbound frames from within the session's own pump task.
async fn watch(ctx: &Ctx, port: Option<u16>, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    // Same `QuicTransport` instance serves (below) and dials (the drain tick):
    // `serve_channels_on` binds its own server-side endpoint per call, entirely
    // independent of the client endpoint `dial_channels` uses (created once in
    // `QuicTransport::new`) — no second transport needed. Mirrors the FFI's
    // `Manager`, which reuses a single `self.quic` for both `serve()` and
    // `chat_flush_peer`'s dial.
    let routes = RouteManager::new(quic.clone());
    let bind_port = port.unwrap_or(config.transfer.port);
    let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, bind_port));
    let (local, mut incoming) = quic.serve_channels_on(addr).await.map_err(CliError::from)?;

    if ctx.json {
        ctx.json_line(&json!({
            "event": "listening",
            "addr": local.to_string(),
            "port": local.port(),
        }));
    } else {
        ctx.line(&format!(
            "watching for chat messages on {} (Ctrl-C to stop)",
            ctx.bold(&local.to_string())
        ));
    }

    // Best-effort discoverability (so `chat send --to <name>` can find us).
    // Identity load failure here shouldn't abort an otherwise-working watch —
    // `SecureCtx::build` above already proved the identity file is usable,
    // but this stays defensive rather than assuming that (mirrors
    // `serve_loop`). Also the drain tick's peer-reachability source.
    let engine = build_engine(config.clone()).ok();
    if let (Some(engine), Ok(self_device)) = (&engine, me(&config)) {
        let _ = engine.start_discovery(self_device).await;
    }

    let sink = received_sink(ctx);
    let mut drain = tokio::time::interval(DRAIN_EVERY);
    // If a tick is missed (e.g. we were busy dispatching a long-lived accepted
    // session), resume the plain periodic cadence rather than firing a burst
    // of catch-up ticks — mirrors the FFI's `chat_drain_loop`.
    drain.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = drain.tick() => {
                if let Some(eng) = &engine {
                    drain_tick(eng, &store, &quic, &routes, &sc, &sink).await;
                }
            }
            item = incoming.next() => {
                let qc = match item {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => {
                        if !ctx.json {
                            ctx.line(&ctx.dim(&format!("inbound rejected: {e}")));
                        }
                        continue;
                    }
                    None => break,
                };
                let mut session = match session_transfer::accept(
                    qc,
                    &sc.ident,
                    &sc.enc,
                    &sc.trust,
                    Some((store.clone(), sink.clone())),
                )
                .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        if !ctx.json {
                            ctx.line(&ctx.dim(&format!("session failed: {e}")));
                        }
                        continue;
                    }
                };
                // Flush-on-connect: push anything already queued for this peer
                // now that we have a live session with them — cheaper/faster
                // than waiting for the next drain tick. Mirrors the FFI's
                // `handle_incoming` flush-on-connect.
                let _ = flush_to_session(
                    &session.handle,
                    &store,
                    &DeviceId::from(session.peer_id.clone()),
                )
                .await;
                // Chat frames are dispatched entirely inside the session's own pump
                // task (via the just-registered `ChatHandler`); there is no stream
                // channel to await here. `next_incoming` resolves to `None` once the
                // peer closes its side — a `chat send` always closes right after
                // sending — which is the signal to close our side too and move on to
                // the next inbound connection.
                while session.next_incoming().await.is_some() {}
                session.close().await;
            }
        }
    }

    if let Some(engine) = &engine {
        let _ = engine.stop_discovery().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{reachable_targets, render_history_json, resolve_history_peer, send};
    use crate::commands::{self, SecureCtx};
    use crate::output::Ctx;
    use peerbeam_chat::{ChatMessage, ChatRecord, ChatStore, Status};
    use peerbeam_config::EngineConfig;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::entity::{Device, DeviceType};
    use peerbeam_domain::id::DeviceId;
    use peerbeam_domain::port::EncryptionProvider;
    use peerbeam_engine::ManagedDevice;
    use std::sync::Arc;

    fn quiet_ctx() -> Ctx {
        Ctx::new(true, true, 0, true, true)
    }

    fn seeded_store() -> (ChatStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[3u8; 32], b"peerbeam-appstore-v1");
        let app = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        (ChatStore::new(app), dir)
    }

    #[test]
    fn render_history_json_renders_seeded_records_chronologically() {
        let (store, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        let m1 = ChatMessage::new("hi").expect("msg1");
        let m2 = ChatMessage::new("there").expect("msg2");
        store
            .append(&ChatRecord::sent(&peer, &m1))
            .expect("append sent");
        store
            .append(&ChatRecord::received(&peer, &m2))
            .expect("append received");

        let records = store.history(&peer).expect("history");
        let value = render_history_json(&records);
        let messages = value["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["id"], m1.id);
        assert_eq!(messages[0]["peer_id"], "pb-bob");
        assert_eq!(messages[0]["body"], "hi");
        assert_eq!(messages[0]["direction"], "out");
        assert_eq!(messages[0]["status"], "sent");
        assert_eq!(messages[1]["id"], m2.id);
        assert_eq!(messages[1]["body"], "there");
        assert_eq!(messages[1]["direction"], "in");
        assert_eq!(messages[1]["status"], "received");
    }

    #[test]
    fn render_history_json_empty_history_is_empty_array() {
        let value = render_history_json(&[]);
        assert_eq!(value["messages"].as_array().expect("array").len(), 0);
    }

    // `resolve_history_peer`'s fast paths (offline history already exists, or
    // the argument already looks like a `pb-` id) must return without ever
    // touching `commands::snapshot` (which spins up a real engine + discovery
    // and sleeps ~2s) — verified here by passing a bare `EngineConfig::default()`
    // that would fail discovery outright if the fast path didn't short-circuit
    // before reaching it. Only these two fast paths are unit-tested; the
    // discovery-resolution fallback (peer given as a not-yet-seen name) is
    // inherently an integration behavior (needs a live device to discover) and
    // was instead verified in a manual two-process smoke test.

    /// `commands::snapshot` sleeps a fixed 2s while discovery runs — well
    /// clear of anything a fast path (no discovery at all) should ever take,
    /// even under heavy test-suite parallelism.
    const FAST_PATH_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

    #[tokio::test]
    async fn resolve_history_peer_fast_path_pb_prefixed_id_skips_discovery() {
        let (store, _dir) = seeded_store();
        let ctx = quiet_ctx();
        let config = EngineConfig::default();
        // No history exists yet for this id — only the `pb-` prefix shortcut
        // can be responsible for returning without discovery. Bounded by
        // `FAST_PATH_BUDGET` so a regression that falls through to
        // `commands::snapshot`'s 2s discovery sleep fails the test instead of
        // silently passing on the resolved value alone.
        let resolved = tokio::time::timeout(
            FAST_PATH_BUDGET,
            resolve_history_peer(&ctx, &config, &store, "pb-doesnotexist".to_string()),
        )
        .await
        .expect("must return without discovery");
        assert_eq!(resolved, DeviceId::from("pb-doesnotexist"));
    }

    #[tokio::test]
    async fn resolve_history_peer_fast_path_existing_offline_history_skips_discovery() {
        let (store, _dir) = seeded_store();
        let peer = DeviceId::from("legacy-bob"); // deliberately not `pb-`-prefixed
        store
            .append(&ChatRecord::sent(
                &peer,
                &ChatMessage::new("hey").expect("msg"),
            ))
            .expect("append");

        let ctx = quiet_ctx();
        let config = EngineConfig::default();
        let resolved = tokio::time::timeout(
            FAST_PATH_BUDGET,
            resolve_history_peer(&ctx, &config, &store, "legacy-bob".to_string()),
        )
        .await
        .expect("must return without discovery");
        assert_eq!(resolved, peer);
    }

    #[tokio::test]
    async fn history_raw_id_fast_path_returns_the_seeded_records() {
        // End-to-end through `resolve_history_peer` + `store.history`, as
        // `history()` itself does: a literal `pb-` id with existing history
        // reads it back without discovery.
        let (store, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        store
            .append(&ChatRecord::sent(
                &peer,
                &ChatMessage::new("hi").expect("msg"),
            ))
            .expect("append");

        let ctx = quiet_ctx();
        let config = EngineConfig::default();
        let resolved = resolve_history_peer(&ctx, &config, &store, "pb-bob".to_string()).await;
        let records = store.history(&resolved).expect("history");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].body, "hi");
    }

    /// An isolated `EngineConfig` rooted under `dir`, so `SecureCtx::build` /
    /// `commands::chat_store` never touch the real `~/.config/peerbeam`.
    fn isolated_config(dir: &std::path::Path) -> EngineConfig {
        let mut config = EngineConfig::default();
        config.storage.data_directory = dir.join("data").to_string_lossy().into_owned();
        config.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
        config.transfer.port = 0; // `send` never binds a listener.
        config
    }

    // `chat send --addr <unreachable>` must enqueue a durable Pending record
    // (and outbox entry) and return Ok, never surfacing the failed
    // opportunistic dial as a command error — an offline peer stays queued
    // for a later `chat watch`/`daemon` drain. Mirrors the brief's required
    // coverage and the FFI's equivalent `chat_send` behavior.
    #[tokio::test]
    async fn send_to_unreachable_addr_enqueues_pending_without_erroring() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");

        let ctx = quiet_ctx();
        // 127.0.0.1 with (almost certainly) nothing listening on this port:
        // the dial fails (fast, or after the transport's connect timeout) but
        // must never surface as a `send` error.
        let result = send(
            &ctx,
            None,
            Some("127.0.0.1:1".to_string()),
            "hello offline".to_string(),
            Some(cfg_path.to_str().expect("utf8 path")),
        )
        .await;
        assert!(
            result.is_ok(),
            "chat send to an unreachable peer must not error: {result:?}"
        );

        // Read back through a fresh store built from the same config/identity
        // — exactly what a real `chat history`/a later drain would see.
        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);
        let target_id = DeviceId::from("addr"); // `--addr`'s routing placeholder id.

        let hist = store.history(&target_id).expect("history");
        assert_eq!(
            hist.len(),
            1,
            "expected exactly one queued record: {hist:?}"
        );
        assert_eq!(hist[0].status, Status::Pending);
        assert_eq!(hist[0].body, "hello offline");

        let outbox = store.outbox_for(&target_id).expect("outbox_for");
        assert_eq!(
            outbox.len(),
            1,
            "message must still be queued in the outbox"
        );
        assert_eq!(outbox[0].body, "hello offline");
    }

    /// A `ManagedDevice` fixture: `id` reachable at `addresses`/`port` if
    /// `online` and both are non-empty/nonzero.
    fn managed(id: &DeviceId, online: bool, addresses: Vec<String>, port: u16) -> ManagedDevice {
        ManagedDevice {
            device: Device {
                id: id.clone(),
                name: id.0.clone(),
                device_type: DeviceType::Desktop,
                platform: peerbeam_platform::current(),
                addresses,
                port,
                last_seen: chrono::Utc::now(),
            },
            online,
            last_seen: chrono::Utc::now(),
            latency_ms: None,
            capabilities: Default::default(),
        }
    }

    #[test]
    fn reachable_targets_filters_online_addressed_peers_with_nonzero_port() {
        let a = DeviceId::from("pb-a"); // online, addressed, has port -> reachable
        let b = DeviceId::from("pb-b"); // online but no addresses -> not reachable
        let c = DeviceId::from("pb-c"); // online, addressed, port 0 -> not reachable
        let d = DeviceId::from("pb-d"); // discovered but currently offline -> not reachable
        let e = DeviceId::from("pb-e"); // not discovered at all -> not reachable

        let online = vec![
            managed(&a, true, vec!["10.0.0.1".to_string()], 49600),
            managed(&b, true, vec![], 49600),
            managed(&c, true, vec!["10.0.0.3".to_string()], 0),
            managed(&d, false, vec!["10.0.0.4".to_string()], 49600),
        ];
        let peers = vec![a.clone(), b, c, d, e];

        let targets = reachable_targets(&peers, &online);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].device.id, a);
    }

    #[test]
    fn reachable_targets_empty_peers_or_empty_online_yields_nothing() {
        assert!(reachable_targets(&[], &[]).is_empty());
        let peers = vec![DeviceId::from("pb-a")];
        assert!(reachable_targets(&peers, &[]).is_empty());
    }
}
