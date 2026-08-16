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

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::json;

use peerbeam_chat::{
    flush_to_session, prepare_file_send, send_file_ref, ChatRecord, ChatStore, Direction, FileMeta,
    Kind, ReceivedSink, Status,
};
use peerbeam_config::EngineConfig;
use peerbeam_domain::id::DeviceId;
use peerbeam_engine::{Engine, ManagedDevice, RouteManager};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{send_file_on_session, SendRequest, TransferControl};
use peerbeam_transfer_quic::QuicTransport;
use tokio::sync::mpsc;

use crate::cli::ChatAction;
use crate::commands::{self, SecureCtx};
use crate::engine::{build_engine, me};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;
use crate::session_transfer;

pub async fn chat(ctx: &Ctx, action: ChatAction, path_override: Option<&str>) -> CliResult {
    match action {
        // clap enforces exactly one of `text`/`file` via `required_unless_present`
        // + `conflicts_with` (see `cli::ChatAction::Send`); the two error arms
        // below only guard a library caller that builds `ChatAction` directly
        // (e.g. a test), bypassing that parser-level guarantee.
        ChatAction::Send {
            to,
            addr,
            text,
            file,
        } => match (text, file) {
            (Some(text), None) => send(ctx, to, addr, text, path_override).await,
            (None, Some(path)) => send_file(ctx, to, addr, path, path_override).await,
            (Some(_), Some(_)) => Err(CliError::Usage(
                "provide either message text or --file, not both".into(),
            )),
            (None, None) => Err(CliError::Usage(
                "provide message text or --file <path>".into(),
            )),
        },
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
            let mut line = json!({
                "event": "chat_received",
                "id": rec.id,
                "peer": rec.peer_id,
                "body": rec.body,
                "timestamp": rec.timestamp,
                "kind": rec.kind,
            });
            if let Some(file) = &rec.file {
                line["file"] = json!(file);
            }
            println!("{}", serde_json::to_string(&line).unwrap_or_default());
        } else {
            // A `Kind::File` record's `body` is always empty (see
            // `ChatRecord::file_out`/`file_in`) — printing it the way a text
            // record's line does would render a blank line, so a file gets
            // its own shape naming what was actually offered.
            let line = match (&rec.kind, &rec.file) {
                (Kind::File, Some(file)) => render_file_line(&rec, file),
                _ => format!("[{}] {}", rec.peer_id, rec.body),
            };
            if color {
                println!("\x1b[32m{line}\x1b[0m");
            } else {
                println!("{line}");
            }
        }
    })
}

/// Human-readable direction label, matching `history`'s existing text
/// rendering (`"out"`/`"in"`).
fn dir_str(d: Direction) -> &'static str {
    match d {
        Direction::Out => "out",
        Direction::In => "in",
    }
}

/// Lowercase wire-form of a [`Status`] (its `serde(rename_all = "lowercase")`
/// form — the same word `chat history --json`/`chat_received`'s JSON would
/// carry for it), so a human-mode file line reads consistently with the
/// JSON one. `serde_json::to_value` on a fieldless-variant enum cannot fail
/// in practice; the fallback just avoids ever panicking on it.
fn status_str(status: Status) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{status:?}").to_lowercase())
}

/// One-line human rendering of a `Kind::File` record: names the file, its
/// size, and its delivery status instead of the (always empty) `body`.
/// Shared by `history` and [`received_sink`] — and, through the sink, by
/// every call site that registers it (`watch`, `serve_loop`'s
/// flush-on-connect, the drain tick, and `send`'s own opportunistic dial) —
/// so a file row never renders as a blank line anywhere a chat record is
/// printed.
fn render_file_line(r: &ChatRecord, file: &FileMeta) -> String {
    format!(
        "[{}] {} file: {} ({} bytes) — {}",
        r.timestamp,
        dir_str(r.direction),
        file.name,
        file.size,
        status_str(r.status),
    )
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

/// Spawn `work` on its own task, unless a previous call's `work` is still
/// running — in which case this call is a silent no-op, and whatever
/// triggered it (a timer tick, here) is simply skipped. `flight` is the
/// shared guard: swapped to `true` before spawning, and cleared once `work`
/// completes, via a drop guard so a panic inside `work` can never leave the
/// flag stuck `true` forever (which would silently and permanently disable
/// every future call).
///
/// Factored out from [`spawn_drain_tick`] purely so the single-flight
/// mechanism is unit-testable on its own, independent of `drain_tick`'s
/// network behavior (see the tests below) — [`spawn_drain_tick`] is its only
/// real caller.
fn spawn_single_flight(flight: &Arc<AtomicBool>, work: impl Future<Output = ()> + Send + 'static) {
    if flight.swap(true, Ordering::AcqRel) {
        return; // already running; this call is skipped.
    }
    let flight = flight.clone();
    tokio::spawn(async move {
        struct ClearOnDrop(Arc<AtomicBool>);
        impl Drop for ClearOnDrop {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _clear = ClearOnDrop(flight);
        work.await;
    });
}

/// Single-flight, non-blocking wrapper around [`drain_tick`]: spawns the
/// sweep on its own task (via [`spawn_single_flight`]) so the caller's
/// `tokio::select!` accept loop is never blocked waiting for it. A
/// synchronous drain dials every reachable peer serially, each subject to
/// the QUIC connect timeout per route candidate — with a backlog of
/// unreachable/firewalled peers that can run tens of seconds, during which
/// an inline `.await` here would starve the same `select!`'s accept arm and
/// stall inbound connections. Since `tokio::time::interval`'s first tick
/// fires immediately, an inline await would do this on every
/// `serve_loop`/`chat watch` startup, not just occasionally.
///
/// `draining` guards against overlapping sweeps: if the previous tick's
/// sweep is still running when the next one fires, this tick is simply
/// skipped — the periodic cadence already tolerates a skipped tick
/// (`MissedTickBehavior::Skip`, set by both callers), and the next tick will
/// pick up whatever is still queued.
pub(crate) fn spawn_drain_tick(
    draining: &Arc<AtomicBool>,
    engine: &Engine,
    store: &ChatStore,
    quic: &Arc<QuicTransport>,
    sc: &SecureCtx,
    sink: &ReceivedSink,
) {
    let engine = engine.clone();
    let store = store.clone();
    let quic = quic.clone();
    let sc = sc.clone();
    let sink = sink.clone();
    spawn_single_flight(draining, async move {
        // Reconstructed rather than shared: both callers build `routes` as a
        // bare `RouteManager::new(quic.clone())` with no further
        // configuration, so an equivalent instance here is simpler than
        // giving `RouteManager` a `Clone` impl just to move it into this task.
        let routes = RouteManager::new(quic.clone());
        drain_tick(&engine, &store, &quic, &routes, &sc, &sink).await;
    });
}

/// `chat send` — resolve the peer exactly like `commands::send`, enqueue the
/// message, and make one opportunistic dial+flush attempt. The dial registers
/// chat wiring on this side too (not "we only send" — a session we dial can
/// just as easily have the peer push something back over it, e.g. their own
/// flush-on-connect, which would be silently dropped without a `ChatHandler`
/// here; see this module's top doc comment on chat-wiring symmetry).
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
            // `flush_to_session` sends a peer's queued entries FIFO and stops
            // at the first per-message failure, returning only the ids it
            // actually got through — so a non-empty `flushed` does NOT mean
            // *this* message (the newest/last FIFO entry, if the peer already
            // had backlog) was among them. Check membership by id, not
            // emptiness, or a backlog message succeeding while this one is
            // still Pending would misreport as delivered.
            flushed.contains(&id)
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

/// `chat send --file <path>` — attach a file to a conversation.
///
/// **Still online-only on this surface.** The bytes are now staged into the
/// outbox's own storage first (`prepare_file_send`), exactly as the desktop and
/// mobile surfaces do, so both frontends derive the wire `FileRef` and the
/// transfer from the same blob and cannot disagree about what is being sent.
/// But an unreachable peer is still a hard failure here, not a queue: this
/// binary has no drain for a queued *file* yet, so leaving one behind would
/// promise a delivery nothing in this process will ever make. Every terminal
/// path therefore dequeues the entry and deletes the blob — `finish` below is
/// the single place that does it. Queueing (and the CLI-side drain that earns
/// it) is a separate task.
///
/// The bytes ride the TRANSFER stream channel exactly like a plain `send`; a
/// small `FileRef` control message rides the CHAT channel so the file gets a row
/// in the peer's own conversation — `SendRequest.transfer_id` is set to the
/// `FileRef`'s id so the two are the SAME id, which is the whole point of the
/// feature.
///
/// Order matters: the row is validated + persisted BEFORE any network work, and
/// the `FileRef` is sent BEFORE the bytes — so a peer who could see the offer
/// always sees it before (or never after) the transfer starts. The session is
/// closed on every exit — dial failure (none established), a
/// `supports_file_ref` refusal, a `send_file_ref` failure, and the transfer's
/// own outcome — never via a bare `?` that could skip it.
async fn send_file(
    ctx: &Ctx,
    to: Option<String>,
    addr: Option<String>,
    path: String,
    path_override: Option<&str>,
) -> CliResult {
    let config = commands::load_config(path_override)?;

    // Resolve the target peer — identical to `send`'s (text) resolution.
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
    let staging = commands::staging_store(&config);
    let ctrl = TransferControl::new();

    // Validate the path, persist the row, and copy the bytes into the outbox's
    // own storage BEFORE any network work — a refused path (missing, a
    // directory, a bad name) leaves no row and touches no network at all, and
    // what gets sent from here on is the blob, not a path the user could change
    // underneath us.
    let (stage_tx, _stage_rx) = mpsc::unbounded_channel();
    let (file_ref, staged) = prepare_file_send(
        &store,
        &staging,
        &target.id,
        &path,
        commands::staging_limits(&config),
        &ctrl,
        &stage_tx,
    )
    .await
    .map_err(CliError::from)?;
    let id = file_ref.id.clone();

    // Every exit below goes through this: the row's final status, the queue
    // entry gone, and the staged blob deleted. Leaving either behind would
    // strand bytes on disk that nothing in this process will ever send.
    let finish = |status: Status| {
        let _ = store.set_status(&target.id, &id, status);
        let _ = store.outbox_remove(&id);
        staging.remove(&staged.staged_path);
    };

    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    let routes = RouteManager::new(quic.clone());
    let sink = received_sink(ctx);
    let session = match session_transfer::dial(
        &quic,
        &routes,
        &target,
        &id,
        &sc.ident,
        &sc.enc,
        &sc.trust,
        Some((store.clone(), sink)),
    )
    .await
    {
        Ok(session) => session,
        Err(e) => {
            // No session was established — nothing to close. This surface is
            // online only: an unreachable peer is a hard failure, not a queue.
            finish(Status::Failed);
            return Err(CliError::Connection(format!(
                "cannot reach {} to send {}: {e}",
                target.name, file_ref.name
            )));
        }
    };

    let newly_trusted = session.newly_trusted;
    let pairing_code = session.pairing_code.clone();
    if newly_trusted && !ctx.json {
        ctx.line(&ctx.dim(&format!("pinned new peer {}", session.peer_id)));
        ctx.line(&format!("  pairing code: {}", ctx.bold(&pairing_code)));
    }

    // NEVER silently fall back to a plain transfer: a peer that never
    // negotiated the FileRef feature has no way to place the file in a
    // conversation, so refuse outright rather than sending bytes it cannot
    // surface — the user must never be told an attachment "landed" somewhere
    // the peer can't see it.
    if !session.supports_file_ref() {
        session.close().await;
        finish(Status::Failed);
        return Err(CliError::Other(format!(
            "{} cannot receive chat attachments — its build predates file sharing in chat. Send {} as a plain transfer instead.",
            target.name, file_ref.name
        )));
    }

    // The FileRef goes out on CHAT before a single byte moves on TRANSFER, so
    // the peer's row exists before (never after) the transfer starts.
    if let Err(e) = send_file_ref(&session.handle, &file_ref).await {
        session.close().await;
        finish(Status::Failed);
        return Err(CliError::Other(format!(
            "could not offer {} to {}: {e}",
            file_ref.name, target.name
        )));
    }

    let chunk = commands::clamp_chunk_size(config.transfer.chunk_size);
    let storage = FsStorage::new();
    let (ptx, mut prx) = mpsc::unbounded_channel();
    let req = SendRequest {
        transfer_id: id.clone(), // the FileRef's id — the shared correlation the feature rests on.
        name: file_ref.name.clone(),
        // The staged blob, never `path`: the user may have deleted, moved or
        // rewritten their file since we copied it, and the `FileRef` the peer is
        // being shown was derived from the blob.
        path: staged.staged_path.clone(),
        size: file_ref.size,
        chunk_size: chunk,
    };
    let bar = ctx.bar(file_ref.size, &file_ref.name);

    let handle = &session.handle;
    let send = async {
        let r = send_file_on_session(handle, &storage, req, &ctrl, &ptx, 3).await;
        drop(ptx);
        r
    };
    let pump = async {
        while let Some(p) = prx.recv().await {
            bar.update(p.transferred_bytes);
        }
        bar.finish();
    };
    let (result, _) = tokio::join!(send, pump);
    // Close on every path, including the transfer's own failure — capture the
    // result first, then close, then propagate (never a `?` before `close()`,
    // which would skip it on exactly this branch).
    session.close().await;

    match result {
        Ok(_outcome) => {
            finish(Status::Sent);
            if ctx.json {
                ctx.json_line(&json!({
                    "event": "chat_file_sent",
                    "id": id,
                    "peer": target.id.0,
                    "delivered": true,
                }));
            } else {
                ctx.line(&ctx.green(&format!("sent {} to {}", file_ref.name, target.name)));
            }
            Ok(())
        }
        Err(e) => {
            finish(Status::Failed);
            Err(CliError::from(e))
        }
    }
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
        match (&r.kind, &r.file) {
            (Kind::File, Some(file)) => ctx.line(&render_file_line(r, file)),
            _ => {
                let dir = dir_str(r.direction);
                ctx.line(&format!("[{}] {}: {}", r.timestamp, dir, r.body));
            }
        }
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
                "kind": r.kind,
                "file": r.file,
            })
        })
        .collect();
    json!({ "messages": messages })
}

/// `chat watch`'s [`ReceivedSink`]: [`received_sink`]'s rendering, plus a
/// one-time operator notice whenever an incoming `FileRef` lands. `chat
/// watch` never reads a peer-opened TRANSFER channel — its accept loop below
/// (`while session.next_incoming().await.is_some() {}`) discards every
/// incoming stream channel without touching its bytes, and its drain tick's
/// own dials (`drain_tick`) close right after flushing the CHAT outbox,
/// without ever awaiting one either. So a bare `chat watch` can display a
/// file offer but can never actually receive it: the sender would otherwise
/// block writing to a stream nobody reads until the transport's own timeout,
/// with no explanation on either side. This notice tells the operator up
/// front which command actually accepts file bytes.
///
/// Deliberately NOT folded into [`received_sink`] itself: that sink is also
/// registered by `serve_loop` (`receive`/`daemon`), where this note would be
/// actively wrong — that process IS running an accept loop that receives the
/// bytes.
fn watch_sink(ctx: &Ctx) -> ReceivedSink {
    let base = received_sink(ctx);
    let json = ctx.json;
    Arc::new(move |rec: ChatRecord| {
        if rec.kind == Kind::File && rec.direction == Direction::In {
            if let Some(file) = &rec.file {
                if json {
                    let line = json!({
                        "event": "chat_file_needs_receiver",
                        "id": rec.id,
                        "peer": rec.peer_id,
                        "name": file.name,
                        "size": file.size,
                    });
                    println!("{}", serde_json::to_string(&line).unwrap_or_default());
                } else {
                    println!(
                        "note: '{}' ({} bytes) offered — `chat watch` cannot receive file bytes; run `peerbeam receive` or `peerbeam daemon start` to accept it",
                        file.name, file.size
                    );
                }
            }
        }
        base(rec);
    })
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
    // Same `QuicTransport` instance serves (below) and dials (the drain tick,
    // via `spawn_drain_tick`'s own `RouteManager::new(quic.clone())`):
    // `serve_channels_on` binds its own server-side endpoint per call, entirely
    // independent of the client endpoint `dial_channels` uses (created once in
    // `QuicTransport::new`) — no second transport needed. Mirrors the FFI's
    // `Manager`, which reuses a single `self.quic` for both `serve()` and
    // `chat_flush_peer`'s dial.
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

    // `watch_sink`, not the plain `received_sink`: every session this process
    // touches (both this accept loop and the drain tick's own dials below)
    // never reads a peer-opened TRANSFER channel, so an incoming `FileRef`
    // needs the extra operator notice `watch_sink` adds — see its doc comment.
    let sink = watch_sink(ctx);
    let mut drain = tokio::time::interval(DRAIN_EVERY);
    // If a tick is missed (e.g. we were busy dispatching a long-lived accepted
    // session), resume the plain periodic cadence rather than firing a burst
    // of catch-up ticks — mirrors the FFI's `chat_drain_loop`.
    drain.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Single-flight guard for `spawn_drain_tick` — see its doc comment for
    // why the sweep is spawned rather than awaited inline here (an inline
    // await would stall this loop's accept arm, including on the very first
    // tick, which fires immediately).
    let draining = Arc::new(AtomicBool::new(false));

    loop {
        tokio::select! {
            _ = drain.tick() => {
                if let Some(eng) = &engine {
                    spawn_drain_tick(&draining, eng, &store, &quic, &sc, &sink);
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
    use super::{
        reachable_targets, render_file_line, render_history_json, resolve_history_peer, send,
        send_file, spawn_single_flight,
    };
    use crate::commands::{self, SecureCtx};
    use crate::output::Ctx;
    use peerbeam_chat::{ChatMessage, ChatRecord, ChatStore, FileMeta, FileRef, Kind, Status};
    use peerbeam_config::EngineConfig;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::entity::{Device, DeviceType};
    use peerbeam_domain::id::DeviceId;
    use peerbeam_domain::port::EncryptionProvider;
    use peerbeam_engine::ManagedDevice;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

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

    /// A `Kind::Text` record's JSON must keep decoding the same way it always
    /// has (`kind`/`file` are additive), with the two new fields present and
    /// honest about there being no file.
    #[test]
    fn render_history_json_text_record_carries_kind_text_and_null_file() {
        let peer = DeviceId::from("pb-bob");
        let m = ChatMessage::new("hi").expect("msg");
        let rec = ChatRecord::sent(&peer, &m);
        let value = render_history_json(&[rec]);
        let messages = value["messages"].as_array().expect("messages array");
        assert_eq!(messages[0]["kind"], "text");
        assert!(messages[0]["file"].is_null());
    }

    /// The test the brief asks for: a `Kind::File` record's JSON must carry
    /// `kind` and `file` — the fields a JSON consumer needs since `body` is
    /// always empty for a file share.
    #[test]
    fn render_history_json_file_record_carries_kind_file_and_file_meta() {
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).expect("file ref");
        let meta = FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: Some("/tmp/report.pdf".into()),
        };
        let rec = ChatRecord::file_out(&peer, &r, meta, Status::Transferring);
        let value = render_history_json(&[rec]);
        let messages = value["messages"].as_array().expect("messages array");
        assert_eq!(messages[0]["kind"], "file");
        assert_eq!(messages[0]["body"], "", "body stays empty for a file row");
        assert_eq!(messages[0]["file"]["name"], "report.pdf");
        assert_eq!(messages[0]["file"]["size"], 4096);
        assert_eq!(messages[0]["file"]["local_path"], "/tmp/report.pdf");
        assert_eq!(messages[0]["status"], "transferring");
    }

    /// The other half of the brief's rendering test: the human-mode line for
    /// a `Kind::File` record must name the file and its size — never the
    /// (always empty) `body`, which is what a blank line would come from.
    #[test]
    fn render_file_line_names_the_file_size_and_status_not_a_blank_body() {
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).expect("file ref");
        let meta = FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: None,
        };
        let rec = ChatRecord::file_out(&peer, &r, meta.clone(), Status::Transferring);
        let line = render_file_line(&rec, &meta);
        assert_eq!(
            line,
            format!(
                "[{}] out file: report.pdf (4096 bytes) — transferring",
                r.timestamp
            )
        );
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

    // `chat send --file` to an unreachable peer must FAIL — increment 2a has
    // no file outbox, so (unlike text's queue-and-return-`Ok`) this must
    // surface as a command error, and the row `prepare_file_send` persisted
    // up front must be marked `Failed` rather than left `Transferring`
    // forever (which reconciliation would only later flip to `Interrupted`).
    #[tokio::test]
    async fn send_file_to_unreachable_addr_fails_and_marks_the_record_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");

        let file_path = dir.path().join("report.pdf");
        std::fs::write(&file_path, vec![7u8; 128]).expect("write file");

        let ctx = quiet_ctx();
        let result = send_file(
            &ctx,
            None,
            Some("127.0.0.1:1".to_string()),
            file_path.to_string_lossy().into_owned(),
            Some(cfg_path.to_str().expect("utf8 path")),
        )
        .await;
        assert!(
            result.is_err(),
            "chat send --file to an unreachable peer must fail, not queue: {result:?}"
        );

        // Read back through a fresh store, exactly what a real `chat history`
        // would show afterwards.
        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);
        let target_id = DeviceId::from("addr"); // `--addr`'s routing placeholder id.

        let hist = store.history(&target_id).expect("history");
        assert_eq!(
            hist.len(),
            1,
            "the row is persisted before any network work, whatever happens next"
        );
        assert_eq!(hist[0].kind, Kind::File);
        assert_eq!(
            hist[0].status,
            Status::Failed,
            "an unreachable peer must mark the row Failed, never leave it Transferring"
        );
        assert_eq!(hist[0].file.as_ref().expect("file meta").name, "report.pdf");
    }

    // **The refusal.** A peer whose build predates file-in-chat advertises
    // CHAT with no `CHAT_FEAT_FILEREF`, so `CapabilitySet::intersect` ANDs the
    // bit away and `Session::supports_file_ref()` is false. `chat send --file`
    // must then fail loudly rather than degrade into a plain transfer: the peer
    // would receive an ordinary file with no row in any conversation while our
    // user was told the attachment had been sent (invariant I9 — never a silent
    // downgrade).
    //
    // The FFI's equivalent branch is covered end to end and mutation-confirmed;
    // the CLI's was not. This is that test, and it asserts the part that makes
    // the refusal meaningful: **no transfer is started** — no `FileRef` on the
    // peer's CHAT channel, and no transfer stream opened at all.
    //
    // The peer deliberately advertises a full TRANSFER capability, so the only
    // thing that can produce this refusal is the missing feature bit.
    #[tokio::test]
    async fn send_file_refuses_a_peer_that_cannot_receive_attachments() {
        use peerbeam_chat::{ChatHandler, ReceivedSink};
        use peerbeam_domain::port::{ChannelTransport, TrustStore};
        use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType, MessageHandler};
        use peerbeam_transfer::{
            HandlerRegistry, Identity, PeerSession, SessionConfig, SessionRole,
        };
        use peerbeam_transfer_quic::QuicTransport;
        use std::sync::Mutex;

        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");

        let file_path = dir.path().join("report.pdf");
        std::fs::write(&file_path, vec![7u8; 128]).expect("write file");

        // A peer from before file-in-chat: CHAT with `features: 0`, plus a full
        // TRANSFER stream capability.
        let peer_enc = AeadCrypto::new();
        let keypair = peer_enc.generate_keypair();
        let identity = Identity {
            device_id: DeviceId::from("legacy-peer"),
            name: "legacy-peer".into(),
            keypair,
        };
        let peer_trust = peerbeam_trust_fs::FsTrust::open(dir.path().join("peer-trust.json"))
            .expect("peer trust");
        let peer_store = {
            let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
            let key = derive_subkey(&[77u8; 32], b"peerbeam-appstore-v1");
            ChatStore::new(Arc::new(peerbeam_appstore_fs::FsAppStore::open(
                dir.path().join("peer-appstore"),
                key,
                enc,
            )))
        };

        // If the CLI wrongly offered a FileRef anyway, this records it.
        let offered: Arc<Mutex<Vec<ChatRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let offered_cl = offered.clone();
        let sink: ReceivedSink = Arc::new(move |rec| offered_cl.lock().unwrap().push(rec));
        let (handler, peer_slot) = ChatHandler::new(peer_store, sink);

        // …and this records any transfer stream it opened. Must stay zero.
        let streams = Arc::new(AtomicUsize::new(0));
        let streams_cl = streams.clone();

        let quic = QuicTransport::new().expect("quic");
        let (addr, mut incoming) = quic
            .serve_channels_on("127.0.0.1:0".parse().expect("addr"))
            .await
            .expect("listen");

        let peer_task = tokio::spawn(async move {
            use futures::StreamExt;
            let qc = incoming
                .next()
                .await
                .expect("an inbound connection")
                .expect("accepted");
            let transport: Arc<dyn ChannelTransport> = Arc::new(qc);
            let enc: Arc<dyn EncryptionProvider> = Arc::new(peer_enc);
            let trust: Arc<dyn TrustStore> = Arc::new(peer_trust);
            let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
            let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
            let (inc, mut inc_rx) = tokio::sync::mpsc::unbounded_channel();
            let caps = CapabilitySet::new()
                .with(Capability::new(ChannelType::CHAT)) // features: 0
                .with(Capability::new(ChannelType::TRANSFER));
            let cfg = SessionConfig::new(caps)
                .with_stream_channel_type(ChannelType::TRANSFER)
                .with_handlers(HandlerRegistry::new().with(handler as Arc<dyn MessageHandler>));
            let mut ps = PeerSession::open(
                transport,
                SessionRole::Responder,
                cfg,
                ev,
                ch,
                inc,
                None,
                identity,
                enc,
                trust,
            )
            .await
            .expect("responder session");
            let _ = peer_slot.set(ps.peer().clone());
            tokio::spawn(async move {
                while inc_rx.recv().await.is_some() {
                    streams_cl.fetch_add(1, Ordering::SeqCst);
                }
            });
            let _ = ps.run().await;
        });

        let ctx = quiet_ctx();
        let err = send_file(
            &ctx,
            None,
            Some(format!("127.0.0.1:{}", addr.port())),
            file_path.to_string_lossy().into_owned(),
            Some(cfg_path.to_str().expect("utf8 path")),
        )
        .await
        .expect_err("a peer without the feature bit must be refused");
        assert!(
            err.to_string().contains("cannot receive chat attachments"),
            "the refusal must name the actual problem, got: {err}"
        );

        // The row says so too, and is still a file row.
        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);
        let hist = store.history(&DeviceId::from("addr")).expect("history"); // `--addr`'s routing placeholder id
        assert_eq!(
            hist.len(),
            1,
            "the row is persisted before any network work"
        );
        assert_eq!(hist[0].kind, Kind::File);
        assert_eq!(hist[0].status, Status::Failed);

        // Nothing was transferred, and nothing was offered.
        assert!(
            offered.lock().unwrap().is_empty(),
            "no FileRef may be offered to a peer that cannot understand it"
        );
        assert_eq!(
            streams.load(Ordering::SeqCst),
            0,
            "a refused attachment must open no transfer stream at all"
        );

        // The refusal path closes the session it dialed rather than leaking it,
        // so the peer's own run loop ends on its own.
        let ended = tokio::time::timeout(Duration::from_secs(10), peer_task).await;
        assert!(
            ended.is_ok(),
            "the refusal path must close the session it dialed"
        );
    }

    // clap's `required_unless_present`/`conflicts_with` (see `cli::ChatAction::
    // Send`) keep a real CLI invocation from ever reaching `chat()` with both
    // or neither of `text`/`file` — but `chat()` itself is a library function
    // any caller can invoke directly (as these tests do), so its own arms for
    // those two cases are exercised here rather than left dead.
    #[tokio::test]
    async fn chat_send_rejects_both_text_and_file() {
        let ctx = quiet_ctx();
        let action = crate::cli::ChatAction::Send {
            to: None,
            addr: None,
            text: Some("hi".to_string()),
            file: Some("/tmp/a.bin".to_string()),
        };
        let result = super::chat(&ctx, action, None).await;
        assert!(result.is_err(), "text and file are mutually exclusive");
    }

    #[tokio::test]
    async fn chat_send_rejects_neither_text_nor_file() {
        let ctx = quiet_ctx();
        let action = crate::cli::ChatAction::Send {
            to: None,
            addr: None,
            text: None,
            file: None,
        };
        let result = super::chat(&ctx, action, None).await;
        assert!(result.is_err(), "one of text or file is required");
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

    // Regression coverage for the drain-tick-blocks-the-accept-loop fix:
    // `spawn_drain_tick` is `serve_loop`/`watch`'s only caller of
    // `drain_tick`, and it delegates its single-flight, spawn-not-await
    // guard entirely to `spawn_single_flight`. A full end-to-end
    // reproduction (real QUIC sockets, an actually-unreachable peer, timing
    // the accept arm against the ~8s connect timeout) would need a much
    // heavier network test harness than this file otherwise uses, so instead
    // this exercises the guard mechanism itself — the part that is new and
    // load-bearing — with a controllable fake `work` future standing in for
    // `drain_tick`. That the caller (`spawn_drain_tick`, and in turn the
    // `select!` arm in `serve_loop`/`watch`) can never block on `work` is
    // additionally guaranteed by construction: `spawn_single_flight` is a
    // plain, non-`async` function, so it has no `.await` point of its own
    // and cannot itself suspend the caller — the only way to reintroduce the
    // original bug would be to make it `async` and await `work` inline
    // again, which the doc comments above now explicitly warn against.
    #[tokio::test]
    async fn spawn_single_flight_skips_overlap_and_clears_after_completion() {
        let flight = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Notify::new());

        // First "tick": records that it started, then parks on `gate` —
        // standing in for `drain_tick` mid-dial against an unreachable peer.
        {
            let started = started.clone();
            let finished = finished.clone();
            let gate = gate.clone();
            spawn_single_flight(&flight, async move {
                started.fetch_add(1, Ordering::SeqCst);
                gate.notified().await;
                finished.fetch_add(1, Ordering::SeqCst);
            });
        }
        wait_until(|| started.load(Ordering::SeqCst) == 1).await;

        // A second "tick" while the first is still parked must be skipped
        // entirely, not queued — proving overlapping ticks never pile up a
        // second concurrent sweep against the same store/peers.
        {
            let started = started.clone();
            spawn_single_flight(&flight, async move {
                started.fetch_add(1, Ordering::SeqCst);
            });
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "an overlapping call must be skipped while the first is still running"
        );

        // Release the first sweep; the guard must clear once it completes.
        gate.notify_one();
        wait_until(|| finished.load(Ordering::SeqCst) == 1).await;

        // With the guard clear, a later call must run normally — proving the
        // flag can never get stuck `true` forever (e.g. after a completed,
        // or even a panicking, sweep).
        {
            let started = started.clone();
            spawn_single_flight(&flight, async move {
                started.fetch_add(1, Ordering::SeqCst);
            });
        }
        wait_until(|| started.load(Ordering::SeqCst) == 2).await;
    }

    /// Poll `pred` until it's true, panicking after a generous bound. Used
    /// instead of a fixed sleep so the test fails fast (rather than hanging)
    /// if the single-flight guard regresses, while tolerating scheduling
    /// jitter under a loaded CI runner.
    async fn wait_until(mut pred: impl FnMut() -> bool) {
        for _ in 0..200 {
            if pred() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("condition not met within the test's timeout budget");
    }
}
