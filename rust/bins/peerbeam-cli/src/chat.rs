//! `peerbeam chat` — send / history / watch.
//!
//! Every chat session rides the same PeerSession machinery as file transfers
//! (`session_transfer`): the Chat capability is advertised on every dial and
//! every accept, so a `chat send` is accepted regardless of what the session
//! was otherwise established for. This module only resolves peers, wires the
//! store, and presents the result — the actual send/receive logic lives in
//! `peerbeam_chat` (`flush_to_session`, `ChatHandler`), reused unchanged.
//!
//! **Offline delivery (1b, extended to files in 2b).** `send` always enqueues
//! (`ChatStore::enqueue`) before touching the network, then makes one
//! opportunistic dial+flush — an unreachable peer simply stays queued.
//! [`send_file`] now has the same shape: stage into the outbox's own storage,
//! enqueue, then one opportunistic dial+send. `watch` and `serve_loop`
//! (`commands.rs`, shared by `receive`/`daemon start`) each run a periodic
//! [`drain_tick`] alongside their accept loop, and push a peer's queued
//! outbox the moment a session with them is accepted (flush-on-connect).
//!
//! **That drain is text-and-declines only.** [`flush_to_session`] skips
//! `Kind::File` by design: a queued file's bytes need a transfer, and
//! `peerbeam-chat` owns no transfer engine, so the *caller* must run one. This
//! binary does not yet — a queued file is delivered by a running PeerBeam app
//! (the FFI runtime's drain, over the same appstore and blob root, keyed by the
//! same identity), or dropped with [`cancel`]. So text and files do **not**
//! report the same outcome here, and [`report_queued`] deliberately does not
//! borrow text's wording: it names what actually delivers a queued file.
//!
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
    begin_file_send, flush_to_session, send_file_ref, stage_file_send, ChatRecord, ChatStore,
    Direction, FileMeta, FileRef, Kind, ReceivedSink, Retention, SendError, StagedFile,
    StagingLimits, StagingStore, Status,
};
use peerbeam_config::EngineConfig;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::ChannelType;
use peerbeam_engine::{Engine, ManagedDevice, RouteManager};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    accept_pipe, send_file_on_session, PipeConsent, SendRequest, TransferControl,
};
use peerbeam_transfer_quic::QuicTransport;
use tokio::sync::mpsc;

use crate::cli::ChatAction;
use crate::commands::{self, SecureCtx};
use crate::engine::{build_engine, me};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;
use crate::prompt;
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
            reply_to,
        } => match (text, file) {
            (Some(text), None) => send(ctx, to, addr, text, reply_to, path_override).await,
            (None, Some(path)) => send_file(ctx, to, addr, path, path_override).await,
            (Some(_), Some(_)) => Err(CliError::Usage(
                "provide either message text or --file, not both".into(),
            )),
            (None, None) => Err(CliError::Usage(
                "provide message text or --file <path>".into(),
            )),
        },
        ChatAction::Cancel { peer, id } => cancel(ctx, peer, id, path_override).await,
        ChatAction::Delete { peer } => delete(ctx, peer, path_override).await,
        ChatAction::Retention { peer, after, off } => {
            retention(ctx, peer, after.as_deref(), off, path_override).await
        }
        ChatAction::Prune { peer } => prune(ctx, peer, path_override).await,
        ChatAction::React {
            peer,
            id,
            emoji,
            remove,
        } => react(ctx, peer, id, &emoji, remove, path_override).await,
        ChatAction::History { peer, mark_read } => {
            history(ctx, peer, mark_read, path_override).await
        }
        ChatAction::Search { query, limit } => search(ctx, &query, limit, path_override),
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

/// The `ReceivedSink` for a command whose **stdout is data**, not a report:
/// `peerbeam pipe`, in both directions.
///
/// It persists and displays nothing, and that is not the same as passing no
/// chat wiring at all. The `ChatHandler` this is handed to still decodes,
/// dedups and **stores** every inbound message — a `chat history` after the
/// pipe shows it — so nothing is lost. What is suppressed is only the display,
/// because [`received_sink`] writes to stdout, and on a `pipe --listen` stdout
/// is the byte stream the user redirected into a file. A chat message printed
/// there would land *inside* their archive.
///
/// Registering the handler at all is deliberate: a session with none does not
/// error on an inbound CHAT frame, it silently drops it (see this module's top
/// doc), so `None` here would reintroduce that bug class on two new call sites
/// to avoid a display problem this solves properly.
pub(crate) fn silent_sink() -> ReceivedSink {
    Arc::new(|_rec: ChatRecord| {})
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

/// The `chat` permission, as a [`CliError`].
///
/// Thin on purpose: [`peerbeam_chat::may_exchange_chat`] is the decision and the
/// tested unit; this only turns a `false` into what an operator reads. Exit `1`
/// (`Other`) rather than `2`: nothing about the invocation was wrong, this
/// machine is declining.
fn permit_chat(trust: &peerbeam_trust_fs::FsTrust, peer: &DeviceId) -> Result<(), CliError> {
    if peerbeam_chat::may_exchange_chat(trust, peer) {
        return Ok(());
    }
    Err(CliError::Other(format!(
        "messages to {} are not permitted: its `chat` permission was revoked \
         (`peerbeam trust permit {} chat` restores it)",
        peer.0, peer.0
    )))
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
    reply_to: Option<String>,
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
        let devices = commands::snapshot(config.clone(), 2).await?;
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
    // Refuse **before** enqueueing: a message that will never be sent has no
    // business sitting in the thread looking Pending. Asked of the routing
    // target here, and of the *authenticated* peer again after the dial — the
    // same predicate both times, so the two can never disagree.
    permit_chat(&sc.trust, &target.id)?;
    let msg =
        peerbeam_chat::ChatMessage::replying(&text, reply_to.as_deref()).map_err(CliError::from)?;
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
            // Asked again here, of the authenticated identity, because that is
            // the device the permission is actually about — a `--addr` send
            // resolved nothing until the handshake completed.
            let flushed = if peerbeam_chat::may_exchange_chat(sc.trust.as_ref(), &peer) {
                flush_to_session(&session.handle, &store, &peer)
                    .await
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
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

/// How many progress reports one staging copy may produce, at most.
///
/// `StagingStore::stage` reports after every 64 KiB — about 262,000 updates for
/// a file at the 16 GiB cap. Neither consumer gains anything from that
/// resolution: the human bar is 28 characters wide and prints a whole-number
/// percentage, so it cannot render more than ~100 distinct states, and an
/// NDJSON reader would have a quarter of a million `chat_staging` lines to wade
/// through. Reporting at percent granularity is visually identical and machine-
/// readable.
const STAGING_STEPS: u64 = 100;

/// Which reporting step `done`-of-`total` bytes falls in — the throttle behind
/// [`STAGING_STEPS`]. Pure, so the throttle is testable without a copy.
///
/// A `total` of 0 (an empty file) collapses to a single step, and a source that
/// outgrew its own metadata mid-copy simply carries on past the last step
/// rather than pinning at 100% — `Bar::update` clamps the display, and a JSON
/// consumer sees the real byte counts either way.
fn staging_step(done: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    done.saturating_mul(STAGING_STEPS) / total
}

/// Copy the picked file into the outbox's own storage, reporting progress.
///
/// Wraps [`stage_file_send`] with the CLI's ordinary progress idiom —
/// `ctx.bar`, which is already a no-op unless `ctx.progress`, so `--json` and
/// non-TTY runs stay clean automatically — plus a `chat_staging` NDJSON line
/// for machine consumers. Both are throttled by [`staging_step`].
///
/// The progress channel is drained *alongside* the copy rather than after it
/// (`tokio::join!`, the same shape the transfer pump below uses): an unbounded
/// channel nobody reads would hold every one of a multi-GB copy's reports in
/// memory, which is exactly the allocation streaming exists to avoid.
///
/// `Ok(None)` is [`stage_file_send`]'s "the conversation was deleted while we
/// copied" answer — another process sharing this data directory (the app, or a
/// second CLI run) removed the thread mid-stage. Nothing was queued and no blob
/// is left; the caller must offer the peer nothing.
#[allow(clippy::too_many_arguments)] // mirrors `stage_file_send`'s own arity, plus `ctx`.
async fn stage_with_progress(
    ctx: &Ctx,
    store: &ChatStore,
    staging: &StagingStore,
    peer: &DeviceId,
    file_ref: &mut FileRef,
    path: &str,
    limits: StagingLimits,
    ctrl: &TransferControl,
) -> Result<Option<StagedFile>, CliError> {
    // Captured before `copy` borrows `file_ref` mutably; `name` is also what a
    // refusal has to name, so it must outlive the borrow either way.
    let id = file_ref.id.clone();
    let name = file_ref.name.clone();
    let total = file_ref.size;
    let peer_id = peer.0.clone();

    let (tx, mut rx) = mpsc::unbounded_channel::<u64>();
    let bar = ctx.bar(total, &name);
    let copy = async {
        let staged = stage_file_send(store, staging, peer, file_ref, path, limits, ctrl, &tx).await;
        drop(tx); // ends `pump` below; without this it would wait forever.
        staged
    };
    let pump = async {
        let mut last = None;
        while let Some(done) = rx.recv().await {
            let step = staging_step(done, total);
            if last == Some(step) {
                continue;
            }
            last = Some(step);
            bar.update(done);
            if ctx.json {
                ctx.json_line(&json!({
                    "event": "chat_staging",
                    "id": id,
                    "peer": peer_id,
                    "done": done,
                    "total": total,
                }));
            }
        }
        bar.finish();
    };
    let (staged, ()) = tokio::join!(copy, pump);
    staged.map_err(|e| stage_refusal(&name, e))
}

/// The refusal for a conversation that was deleted out from under a stage.
///
/// Not [`stage_refusal`]: nothing about the file or the bounds was wrong, so
/// naming `device.max_queued_file_bytes` would send the user to a setting that
/// had nothing to do with it. What happened is that the thread this attachment
/// belongs to stopped existing, and the honest outcome is a non-zero exit that
/// says so — the file was not queued and will not be sent.
fn deleted_mid_stage(peer_name: &str, name: &str) -> CliError {
    CliError::Other(format!(
        "the conversation with {peer_name} was deleted while {name} was being staged, \
         so nothing was queued — send it again to retry"
    ))
}

/// Render a staging refusal honestly.
///
/// [`SendError`]'s own `Display` prefixes its `Session` variant with "chat
/// session error:", which is false for everything that can fail here — nothing
/// has been dialed and no session exists. What [`stage_file_send`] actually put
/// in that variant is `StagingError`'s message, which names the reason and
/// every number behind it: "{size} bytes is over the {max}-byte limit for a
/// chat attachment", or "staging {need} bytes would leave less than the {floor}
/// bytes that must stay free (only {free} available)".
///
/// So the numbers are not repeated here. What the library cannot know is that
/// those two bounds are **configurable and what they are called** — and that is
/// the part a user needs, because this surface streamed straight from the source
/// until increment 2b and a file that used to send is now refused. Naming the
/// keys is what turns a refusal into something they can act on.
fn stage_refusal(name: &str, e: SendError) -> CliError {
    match e {
        SendError::Session(reason) => CliError::Other(format!(
            "cannot stage {name}: {reason}. Both staging bounds are configurable: \
             device.max_queued_file_bytes and device.min_free_bytes."
        )),
        // A `Chat` variant (a name or body the wire format rejects) keeps its
        // own mapping — `ChatError::TooLarge` is a usage error with its own
        // exit code, which flattening to `Other` here would silently change.
        other => CliError::from(other),
    }
}

/// The same honesty for [`begin_file_send`]'s refusals — a missing path, a
/// directory, a name with no basename. No session exists on that path either,
/// so its message must not claim one; unlike [`stage_refusal`] there are no
/// limits to name, because nothing has been copied or measured yet.
fn begin_refusal(e: SendError) -> CliError {
    match e {
        SendError::Session(reason) => CliError::Other(reason),
        other => CliError::from(other),
    }
}

/// Report a file share that is staged and queued but not yet delivered.
///
/// **This deliberately does NOT reuse text's [`send`] wording.** Text says "a
/// running daemon/watch will deliver", which is true of a queued *message* and
/// false of a queued *file* on this surface: `drain_tick` — this binary's only
/// drain, shared by `chat watch` and `daemon start`/`receive` — delivers
/// through [`flush_to_session`], which skips `Kind::File` by design, because a
/// queued file's bytes need a transfer and `peerbeam-chat` owns no transfer
/// engine. Text and files genuinely differ here, and one accurate sentence
/// beats a familiar one followed by a correction nobody reads.
///
/// What does deliver a queued file is a running PeerBeam app: the FFI runtime's
/// own drain, reading the same `<data_directory>/appstore` and the same
/// `<data_directory>/outbox-blobs` this CLI just wrote, unlocked by the same
/// identity-derived key. That is a property of sharing the data directory, not
/// of the app being on this machine by coincidence — an invocation pointed
/// elsewhere with `--config` queues somewhere that app cannot see.
///
/// **And it is true only of a `--to` send.** An `--addr` target carries the
/// routing placeholder [`commands::ADDR_PEER_ID`], so its row and its outbox
/// entry are filed under the literal peer id `addr` — while *both* drains
/// (`drain_tick` here, `runtime::chat_drain_loop` in the FFI) resolve the peers
/// they flush out of discovery, which never yields that id. Nothing delivers
/// such an entry, and no running app changes that, so the paragraph above must
/// not be printed for one: see [`queued_lines`].
///
/// The staged size is named because the queue now holds a full copy of the
/// file, and the second line is the way to get those bytes back — the one thing
/// that is true of both cases.
fn report_queued(ctx: &Ctx, target: &peerbeam_domain::entity::Device, file_ref: &FileRef) {
    if ctx.json {
        // The same event and the same keys the delivered case emits — only
        // `delivered` differs, exactly as text's `chat_sent` already does. No
        // new key, no changed type: a 2a consumer keeps parsing this. It
        // carries no prose, so the claim corrected above never appears here.
        ctx.json_line(&json!({
            "event": "chat_file_sent",
            "id": file_ref.id,
            "peer": target.id.0,
            "delivered": false,
        }));
        return;
    }
    for line in queued_lines(&target.name, &target.id.0, file_ref) {
        ctx.line(&ctx.dim(&line));
    }
}

/// The two lines [`report_queued`] prints, as plain strings.
///
/// Extracted so the *claim they make* is unit-testable. This wording is
/// load-bearing rather than cosmetic: the obvious edit is to reach for text's
/// "a running daemon/watch will deliver", which reads naturally, is one word
/// shorter, and is false for a file on this surface. A test can catch that; a
/// comment cannot.
///
/// **Two different claims, because there are two different outcomes**, and the
/// discriminator is `peer_id` itself rather than a flag threaded down from the
/// call site — the entry's deliverability is a property of the key it is filed
/// under, not of how the caller happened to spell the target. A `--to` send is
/// filed under the peer's real device id, which a running app's drain can look
/// up in discovery and come back for. An `--addr` send is filed under
/// [`commands::ADDR_PEER_ID`], which no drain can ever resolve, so the first
/// line must promise nothing at all. The second line is identical either way:
/// `chat cancel` takes whichever id the row is actually under, and releasing the
/// bytes is the one action that works in both cases.
fn queued_lines(peer_name: &str, peer_id: &str, file_ref: &FileRef) -> [String; 2] {
    let first = if peer_id == commands::ADDR_PEER_ID {
        format!(
            "staged for {peer_name} — {} bytes copied and the row recorded, but nothing will \
             deliver it: an `--addr` send is filed under the placeholder peer id `{}`, and every \
             drain resolves its peers through discovery, which never yields that id. A queued \
             file drains only for a peer addressed by name (`--to`); with `--addr` the peer must \
             be reachable at send time.",
            file_ref.size,
            commands::ADDR_PEER_ID,
        )
    } else {
        format!(
            "queued for {peer_name} — {} bytes staged; a running PeerBeam app sharing this data \
             directory delivers it (`daemon`/`receive`/`chat watch` drain queued text and \
             declines, not files)",
            file_ref.size
        )
    };
    [
        first,
        format!(
            "`chat cancel {peer_id} {}` drops it and frees the staged bytes",
            file_ref.id
        ),
    ]
}

/// `chat send --file <path>` — attach a file to a conversation.
///
/// **Offline-first (2b), the same shape as text.** The path is validated and
/// the row persisted, the bytes are copied into the outbox's own storage
/// ([`stage_file_send`]) and queued, and only then is one opportunistic dial +
/// send attempted. An unreachable peer is **not** a failure: the entry simply
/// stays queued, exactly as `send`'s text does. That replaces 2a's hard
/// `CliError::Connection`.
///
/// The same *shape*, but not the same *outcome*, and [`report_queued`] says so:
/// this binary's drain delivers queued text and declines, never a queued file.
///
/// Staging is what makes a queued file honest — between queueing and delivery
/// the user may delete, move or rewrite what they picked, so the blob (never
/// the path) is what gets sent. It is also why this surface now **refuses**
/// sends 2a would have streamed: `stage_file_send` enforces
/// `device.max_queued_file_bytes` and `device.min_free_bytes`, and
/// [`stage_refusal`] is what carries the reason, its numbers and the key behind
/// the bound out to the user rather than failing generically.
///
/// **A peer without `CHAT_FEAT_FILEREF` stays a hard error**, deliberately not
/// folded into the queue above: an unreachable peer is one who is merely away,
/// and waiting helps; a peer whose build predates file-in-chat can never
/// receive this at all, so queueing would promise a delivery that can never
/// happen. Same for a failed offer or a failed transfer — a session *was*
/// established, so these are real failures, and each dequeues the entry and
/// deletes the blob through `finish` rather than stranding bytes this binary
/// has no drain to retry.
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
/// closed on every exit — a `supports_file_ref` refusal, a `send_file_ref`
/// failure, and the transfer's own outcome — never via a bare `?` that could
/// skip it. (A failed dial established nothing, so there is nothing to close.)
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
        let devices = commands::snapshot(config.clone(), 2).await?;
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
    let limits = commands::staging_limits(&config);
    // Honours `transfer.max_send_bytes_per_sec`: a chat attachment is a transfer
    // like any other, and a ceiling that applied to `send` but not to this one
    // would be a ceiling nobody could reason about.
    let ctrl = sc.control();

    // Validate the path and persist the row BEFORE any copy or network work — a
    // refused path (missing, a directory, a bad name) leaves no row, copies
    // nothing and touches no network at all. The row reads `Staging` from here
    // until the copy below finishes, so a multi-GB attach is visible as work in
    // progress rather than looking hung.
    let mut file_ref = begin_file_send(&store, &target.id, &path).map_err(begin_refusal)?;
    let id = file_ref.id.clone();

    // Copy the bytes into the outbox's own storage and queue them. What gets
    // sent from here on is the blob, not a path the user could change
    // underneath us — and, being queued, it survives this process exiting.
    let Some(staged) = stage_with_progress(
        ctx,
        &store,
        &staging,
        &target.id,
        &mut file_ref,
        &path,
        limits,
        &ctrl,
    )
    .await?
    else {
        return Err(deleted_mid_stage(&target.name, &file_ref.name));
    };

    // Every terminal exit below goes through this: the row's final status, the
    // queue entry gone, and the staged blob deleted. Deliberately NOT called on
    // the unreachable-peer path — that one leaves both in place, which is what
    // "queued" means.
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
        Err(_) => {
            // Unreachable right now. No session was established — nothing to
            // close, and nothing to undo: the row stays `Pending` and the entry
            // stays queued, exactly as text's `send` leaves an undelivered
            // message. The dial error itself is deliberately swallowed rather
            // than reported, for the same reason `send` swallows it: "the peer
            // is not up" is the expected outcome here, not a fault.
            report_queued(ctx, &target, &file_ref);
            return Ok(());
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

/// `chat cancel <peer> <id>` — call off a file we are sharing.
///
/// **The authorization is [`ChatRecord::is_cancellable_outgoing_file`], and
/// only that.** This deletes bytes on the strength of two strings a user typed,
/// so a key match at `(peer, id)` is not enough: it would let `chat cancel`
/// name a text row, a file the peer is offering *us* (refusing that is the
/// approval gate's business — I6 — never this), or a share that already
/// completed. The rule lives in `peerbeam-chat` precisely so both surfaces
/// share one copy of it; the FFI's `pb_chat_cancel` passes the same check.
///
/// The row is fetched from **`peer`'s own namespace**, which is load-bearing:
/// scanning conversations for the id instead would leave the direction check as
/// the only thing between one cancel and another peer's file.
///
/// No path is ever built from the arguments. The blob unlinked is the
/// `staged_path` read back off the queue entry — a string `StagingStore` itself
/// produced under its own root — and the entry is found through `outbox_for`,
/// which filters by peer. (The record keys those arguments do reach are
/// hex-encoded by `FsAppStore` before they touch the filesystem, so neither can
/// escape its namespace either.)
///
/// What this cannot do, unlike the FFI's equivalent, is stop a copy or a
/// transfer that is *running right now*: those live inside a `chat send --file`
/// process, and one CLI invocation has no IPC to another. It stops everything
/// that outlives that process — the queue entry, the bytes, and a row left
/// stranded by a send that was interrupted (Ctrl-C during a stage leaves
/// exactly that).
/// `peerbeam snippet --to <PEER> [--title T]` — send stdin as a message.
///
/// Terminal output arrives on a pipe, so that is where this reads it from.
/// **Truncated to fit a chat message rather than refused**: a 200 MB log piped
/// by accident should not fail with a size error after the command that
/// produced it has already finished — the head of it is what someone wanted to
/// show anyway, and the message says it was cut.
pub async fn snippet(
    ctx: &Ctx,
    args: crate::cli::SnippetArgs,
    path_override: Option<&str>,
) -> CliResult {
    use std::io::Read;

    let mut body = String::new();
    std::io::stdin()
        .read_to_string(&mut body)
        .map_err(|e| CliError::Other(format!("reading the snippet from stdin: {e}")))?;
    if body.trim().is_empty() {
        return Err(CliError::Usage(
            "nothing on stdin — pipe something in, e.g. `cmd | peerbeam snippet --to laptop`"
                .into(),
        ));
    }

    let title = args.title.map(|t| format!("{t}\n")).unwrap_or_default();
    // Leave room for the title and the truncation notice, so the assembled
    // message is inside the cap rather than just the body.
    const NOTICE: &str = "\n… (truncated)";
    let room = peerbeam_chat::MAX_BODY - title.len() - NOTICE.len();
    let text = if body.len() > room {
        // Cut on a char boundary: slicing a UTF-8 string mid-character panics.
        let mut cut = room;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{title}{}{NOTICE}", &body[..cut])
    } else {
        format!("{title}{body}")
    };

    // Never a reply: this is a note being shared, not an answer to anything.
    send(ctx, Some(args.to), None, text, None, path_override).await
}

/// `peerbeam chat react <PEER> <ID> <EMOJI> [--remove]`.
///
/// Applies to this device's own history first and regardless of reachability —
/// it is our record of our own gesture — then tries to tell the peer. The two
/// answers are reported separately for the same reason the FFI separates them:
/// a peer that is offline, or too old to have negotiated the reaction
/// capability, must not be described as having seen it.
///
/// Reactions are not queued. `chat send` leaves an undelivered message in the
/// outbox to flow later; a reaction that missed its moment is noise by the time
/// it would arrive, so it is applied locally and simply not delivered.
async fn react(
    ctx: &Ctx,
    peer: String,
    id: String,
    emoji: &str,
    remove: bool,
    path_override: Option<&str>,
) -> CliResult {
    if emoji.is_empty() || emoji.len() > peerbeam_chat::MAX_REACTION {
        return Err(CliError::Usage(format!(
            "reaction must be 1..={} bytes",
            peerbeam_chat::MAX_REACTION
        )));
    }
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    // Same resolution `chat history` and `chat cancel` use, so an id read out
    // of `chat history bob` can be reacted to with the same word `bob`.
    let peer_id = resolve_history_peer(ctx, &config, &store, peer).await;

    let applied = store
        .apply_reaction(&peer_id, &id, emoji, Direction::Out, remove)
        .map_err(|e| CliError::Other(e.to_string()))?;
    if !applied && !ctx.json {
        ctx.line(&ctx.dim(
            "nothing changed — no such message in this conversation, \
             or the reaction was already in that state",
        ));
    }

    let delivered = deliver_reaction(&config, &sc, &store, ctx, &peer_id, &id, emoji, remove).await;

    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "peer": peer_id.0,
            "id": id,
            "emoji": emoji,
            "removed": remove,
            "applied": applied,
            "delivered": delivered,
        }));
    } else if applied {
        let verb = if remove { "withdrew" } else { "reacted" };
        let where_ = if delivered {
            "delivered"
        } else {
            "not delivered (peer offline, or too old for reactions)"
        };
        ctx.line(&format!("{verb} {emoji} on {id} — {where_}"));
    }
    Ok(())
}

/// Best-effort live delivery of one reaction. Every negative answer — cannot
/// resolve, cannot dial, peer never negotiated the capability, send failed — is
/// simply `false`; none of them is an error for the caller, whose local history
/// is already correct.
#[allow(clippy::too_many_arguments)]
async fn deliver_reaction(
    config: &EngineConfig,
    sc: &SecureCtx,
    store: &ChatStore,
    ctx: &Ctx,
    peer_id: &DeviceId,
    id: &str,
    emoji: &str,
    remove: bool,
) -> bool {
    // A discovery window that never opened is not a peer that isn't there.
    // This path cannot propagate — the reaction is already in local history and
    // the command has succeeded — but "not delivered (peer offline)" would
    // otherwise be printed about a machine that never got looked for.
    let devices = match commands::snapshot(config.clone(), 2).await {
        Ok(d) => d,
        Err(e) => {
            commands::report_problem(ctx, &format!("could not look for {peer_id}: {e}"));
            return false;
        }
    };
    let Some(meta) = devices.iter().find(|m| m.device.id == *peer_id) else {
        return false;
    };
    let Ok(quic) = QuicTransport::new() else {
        return false;
    };
    let quic = Arc::new(quic);
    let routes = RouteManager::new(quic.clone());
    let sink = received_sink(ctx);
    let Ok(session) = session_transfer::dial(
        &quic,
        &routes,
        &meta.device,
        "react",
        &sc.ident,
        &sc.enc,
        &sc.trust,
        Some((store.clone(), sink)),
    )
    .await
    else {
        return false;
    };
    // Asked of the authenticated identity, like every other chat send.
    let peer = DeviceId::from(session.peer_id.clone());
    let ok = if peerbeam_chat::may_exchange_chat(sc.trust.as_ref(), &peer)
        && session.supports_reaction()
    {
        let r = if remove {
            peerbeam_chat::Reaction::remove(id, emoji)
        } else {
            peerbeam_chat::Reaction::add(id, emoji)
        };
        peerbeam_chat::send_reaction(&session.handle, &r)
            .await
            .is_ok()
    } else {
        false
    };
    session.close().await;
    ok
}

async fn cancel(ctx: &Ctx, peer: String, id: String, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    let staging = commands::staging_store(&config);
    // Same resolution `chat history` uses, so an id a user read out of
    // `chat history bob` can be cancelled with the same word `bob`.
    let peer_id = resolve_history_peer(ctx, &config, &store, peer).await;

    let row = store
        .get(&peer_id, &id)
        .map_err(|e| CliError::Other(e.to_string()))?;
    let Some(row) = row.filter(ChatRecord::is_cancellable_outgoing_file) else {
        return Err(CliError::NotFound(format!(
            "no file left to cancel under {id} in the conversation with {peer_id} — \
             it is not an outgoing file share of ours, or it has already been sent or declined"
        )));
    };

    // The queue entry and the bytes it owns.
    let queued = store
        .outbox_for(&peer_id)
        .unwrap_or_default()
        .into_iter()
        .find(|e| e.message_id == id);
    if let Some(entry) = &queued {
        if let Some(staged) = &entry.file {
            staging.remove(&staged.staged_path);
        }
        store
            .outbox_remove(&id)
            .map_err(|e| CliError::Other(e.to_string()))?;
    }
    // The row — but only if it *still* authorizes the write. See
    // [`settle_cancelled`]: the gate above and this write are far apart, and the
    // writer that can land between them is in another process.
    let settled = settle_cancelled(&store, &peer_id, &id)?;

    let name = row
        .file
        .as_ref()
        .map_or_else(|| id.clone(), |f| f.name.clone());
    let dequeued = queued.is_some();
    if ctx.json {
        ctx.json_line(&json!({
            "event": "chat_cancelled",
            "id": id,
            "peer": peer_id.0,
            // True only where this call genuinely undid something: it settled
            // the row, or it let go of bytes that were still queued. A share
            // that settled under us is neither.
            "cancelled": settled || dequeued,
            // Whether bytes were actually let go, or this only settled a row
            // whose queue entry had already gone.
            "dequeued": dequeued,
        }));
    } else {
        ctx.line(&match (settled, dequeued) {
            (true, true) => ctx.green(&format!("cancelled {name} ({id})")),
            (true, false) => ctx.dim(&format!(
                "cancelled {name} ({id}) — nothing was still queued"
            )),
            // The row settled under us — another process draining this queue,
            // or an earlier cancel. The queued bytes are genuinely gone, but
            // the share itself was not cancelled: never claim it was.
            (false, true) => ctx.dim(&format!(
                "dropped the queued copy of {name} ({id}) — its row had already settled"
            )),
            (false, false) => ctx.dim(&format!(
                "nothing left to cancel for {name} ({id}) — it settled before this cancel could"
            )),
        });
    }
    Ok(())
}

/// Move a cancelled share's row to `Failed` — but only if the row *still*
/// authorizes it. Returns whether this call changed anything, so a cancel that
/// genuinely only tidied a stranded row still reports honestly, and one that
/// found the row already settled never claims a cancellation it did not make.
///
/// **This re-reads the row and re-applies
/// [`ChatRecord::is_cancellable_outgoing_file`] — the same single rule
/// [`cancel`] authorized with, not a weaker one.** The two reads are far apart:
/// between them sits a whole [`ChatStore::outbox_for`] (an AppStore `list` plus
/// an AEAD decrypt of every queued record) and the unlink of the blob. Here the
/// writer that lands inside that window is in another **process**, and that is
/// not hypothetical — it is the documented delivery mechanism. This binary's
/// own drain does not deliver queued files (see the module header), so
/// [`report_queued`] tells the user a running PeerBeam app sharing this data
/// directory is what will: that app writes `Sent` on completion and `Declined`
/// when a `FileDecline` arrives, both states the shared rule calls final, and
/// `FsAppStore` has no cross-process lock. Settling on the *earlier* read would
/// overwrite a delivered file with `Failed` and print `"cancelled": true`: the
/// sender's history would permanently claim a file the receiver is holding was
/// cancelled, and a peer's refusal would be relabelled as our own cancellation.
/// The row that gets written is the row that was checked.
///
/// A row that has vanished (`Ok(None)`) and a store that cannot be read (`Err`)
/// both settle nothing, for the same reason: neither is a row this call checked.
///
/// `Failed` is excluded on top of the shared rule (which permits it, since a
/// failed row may still have a queue entry a later drain would retry): an
/// earlier cancel — or the sending process's own failure path — may have landed
/// it already, and a second identical write is not something this call did.
///
/// This mirrors the FFI's `Manager::settle_cancelled` deliberately: both
/// surfaces settle a cancel against one rule, and it is the same rule.
///
/// [`ChatRecord::is_cancellable_outgoing_file`]: peerbeam_chat::ChatRecord::is_cancellable_outgoing_file
/// [`ChatStore::outbox_for`]: peerbeam_chat::ChatStore::outbox_for
fn settle_cancelled(store: &ChatStore, peer_id: &DeviceId, id: &str) -> Result<bool, CliError> {
    let Ok(Some(row)) = store.get(peer_id, id) else {
        return Ok(false);
    };
    if !row.is_cancellable_outgoing_file() || row.status == Status::Failed {
        return Ok(false);
    }
    // `Failed` is where a cancelled share lands — there is no `Cancelled`
    // status, by the same decision the FFI's `settle_cancelled` records.
    store
        .set_status(peer_id, id, Status::Failed)
        .map_err(|e| CliError::Other(e.to_string()))?;
    Ok(true)
}

// ── delete ──────────────────────────────────────────────────────────────────

/// What a `chat delete` may do, given how it was invoked and what — if
/// anything — the operator answered.
///
/// The same shape as [`trust::approval_gate`], and for the same reason: the
/// decision is a pure function of its inputs, so it is testable without a
/// terminal and there is one confirmation idiom in this CLI rather than two.
///
/// It differs in the one place that matters. `approve` treats `--json` as
/// consent, because it *grants* standing and a machine consuming NDJSON has no
/// way to reply. This erases history nothing else on the device keeps a second
/// copy of, so silence is never consent here: a script has to say `--yes` out
/// loud before anything is destroyed.
///
/// [`trust::approval_gate`]: crate::trust::approval_gate
#[derive(Debug, PartialEq, Eq)]
pub enum DeleteGate {
    /// `--yes`, or an explicit "yes" at the prompt.
    Proceed,
    /// Someone was asked and said no.
    Declined,
    /// Nobody could be asked (`--json`, no TTY, redirected stdin, EOF) and
    /// `--yes` was not given. Told apart from [`DeleteGate::Declined`] so the
    /// refusal can name the flag that would have worked, instead of reporting
    /// a "no" nobody actually said.
    Unanswerable,
}

/// Decide whether `chat delete` may proceed. See [`DeleteGate`].
pub fn delete_gate(assume_yes: bool, answer: Option<bool>) -> DeleteGate {
    if assume_yes {
        return DeleteGate::Proceed;
    }
    match answer {
        Some(true) => DeleteGate::Proceed,
        Some(false) => DeleteGate::Declined,
        None => DeleteGate::Unanswerable,
    }
}

/// The question an interactive `chat delete` asks — **the count first**.
///
/// The number is inside the question rather than printed above it because
/// `prompt::confirm` writes its argument unconditionally while [`Ctx::line`]
/// honours `--quiet`: this way there is no combination of flags under which
/// somebody confirms a delete without having been told how much it takes.
///
/// It also says what the delete is *not*. Every other messenger's "delete
/// conversation" reaches the other person's screen too, and someone who
/// assumes that here would be deleting their own evidence of a thread the peer
/// still has in full.
fn delete_question(peer: &DeviceId, records: usize) -> String {
    format!(
        "  peer      {}\n  messages  {records}\nThis erases them from **this device only** — \
         the peer keeps its own copy, and there is no undo.\nA message still queued to send is \
         kept, along with the file it owns.\nDelete this conversation?",
        peer.0,
    )
}

/// `peerbeam chat retention <PEER> [--after 30m | --off]`.
///
/// **A local window, not a protocol.** Nothing is sent to the peer, and the
/// help text says so — a "disappearing message" feature that quietly relies on
/// the other side deleting its copy is a promise this architecture cannot keep,
/// and stating it would be worse than not having the feature.
async fn retention(
    ctx: &Ctx,
    peer: String,
    after: Option<&str>,
    off: bool,
    path_override: Option<&str>,
) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    let id = resolve_history_peer(ctx, &config, &store, peer).await;

    if !off && after.is_none() {
        let current = store
            .retention(&id)
            .map_err(|e| CliError::Other(e.to_string()))?;
        if ctx.json {
            ctx.json_line(&serde_json::json!({
                "event": "chat_retention",
                "peer": id.0,
                "seconds": current.ttl_secs,
            }));
        } else if current.is_off() {
            ctx.line("messages are kept until you delete them");
        } else {
            ctx.line(&format!(
                "messages disappear from this device after {}",
                commands::humantime(std::time::Duration::from_secs(
                    current.ttl_secs.unwrap_or(0)
                ))
            ));
        }
        return Ok(());
    }

    let next = if off {
        Retention::OFF
    } else {
        let secs = commands::parse_duration(after.unwrap_or_default())
            .map_err(|e| CliError::Usage(e.to_string()))?;
        Retention::for_secs(
            secs.num_seconds()
                .try_into()
                .map_err(|_| CliError::Usage("that window is not a length of time".into()))?,
        )
        .map_err(|e| CliError::Usage(e.to_string()))?
    };
    store
        .set_retention(&id, next)
        .map_err(|e| CliError::Other(e.to_string()))?;

    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "event": "chat_retention_set",
            "peer": id.0,
            "seconds": next.ttl_secs,
        }));
        return Ok(());
    }
    if off {
        // Said explicitly: turning the window off restores what was hidden but
        // cannot restore what a prune already deleted.
        ctx.line("disappearing messages off — anything already deleted is gone");
    } else {
        ctx.line(&format!(
            "messages will disappear from this device after {}\n{}",
            commands::humantime(std::time::Duration::from_secs(next.ttl_secs.unwrap_or(0))),
            ctx.dim("the peer keeps its own copy — this device cannot delete that")
        ));
    }
    Ok(())
}

/// `peerbeam chat prune [PEER]` — delete what the window has already hidden.
async fn prune(ctx: &Ctx, peer: Option<String>, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    let now = chrono::Utc::now();

    let pruned = match peer {
        Some(p) => {
            let id = resolve_history_peer(ctx, &config, &store, p).await;
            store
                .prune(&id, now)
                .map_err(|e| CliError::Other(e.to_string()))?
        }
        None => {
            // The staging store too: an expired file entry owns a copy of the
            // bytes, and deleting only the row would leave them on disk.
            let staging = commands::staging_store(&config);
            peerbeam_chat::prune_all_conversations(&store, &staging, now)
                .map_err(|e| CliError::Other(e.to_string()))?
        }
    };

    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "event": "chat_pruned",
            "messages": pruned.records,
            "queued": pruned.queued,
            "staged_blobs": pruned.staged.len(),
        }));
    } else if pruned.is_empty() {
        ctx.line("nothing had aged out");
    } else {
        ctx.line(&format!(
            "deleted {} message(s) and {} queued item(s)",
            pruned.records, pruned.queued
        ));
    }
    Ok(())
}

/// `peerbeam chat delete <PEER>` — forget a whole conversation on this device.
///
/// The engine call is [`ChatStore::delete_conversation`] — the same one the app
/// reaches through `pb_chat_delete`, not a second implementation of it, so the
/// keep rule that saves a queued file's record cannot drift between the two
/// surfaces (I7).
///
/// **Not [`cancel`], in either direction.** That calls off *one file we are
/// still sharing*: it dequeues the entry, unlinks the bytes the outbox staged,
/// and settles the row. This deletes *history* — every message in the thread,
/// text rows and file rows alike — and deliberately leaves the queue alone.
/// Cancelling does not forget the thread; deleting does not stop a send.
///
/// `kept` is counted off the store **after** the delete rather than from the
/// rule that chose what to keep, exactly as the FFI's `Manager::chat_delete`
/// counts it: a number the user is shown should be an observation, not a
/// restatement of the intent. Both counts go through
/// [`ChatStore::record_count`], which counts by stored key — a row written by a
/// newer schema is real and present even though this build cannot decode it,
/// and counting through `history` (which skips exactly those) would describe a
/// thread as empty to somebody deciding whether to erase it.
///
/// [`ChatStore::delete_conversation`]: peerbeam_chat::ChatStore::delete_conversation
/// [`ChatStore::record_count`]: peerbeam_chat::ChatStore::record_count
async fn delete(ctx: &Ctx, peer: String, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    // The same resolution `chat history` and `chat cancel` use, so a thread
    // read with `chat history bob` is deleted with the same word `bob`.
    let peer_id = resolve_history_peer(ctx, &config, &store, peer).await;

    let before = store
        .record_count(&peer_id)
        .map_err(|e| CliError::Other(e.to_string()))?;
    // Nothing to lose, so nothing to ask about — and a clean exit, not an
    // error. Re-running a delete must converge rather than fail the second
    // time, the way `trust approve` already treats an already-approved device;
    // it is also the honest answer for a name that resolved to a peer this
    // device has never spoken to.
    if before > 0 {
        let answer = if ctx.interactive {
            Some(prompt::confirm(
                ctx,
                &delete_question(&peer_id, before),
                false,
            ))
        } else {
            None
        };
        match delete_gate(ctx.assume_yes, answer) {
            DeleteGate::Proceed => {}
            DeleteGate::Declined => return Err(CliError::Cancelled),
            DeleteGate::Unanswerable => {
                return Err(CliError::Usage(format!(
                    "deleting the conversation with {} cannot be undone and nobody could be \
                     asked here — re-run with `--yes` to confirm it in advance",
                    peer_id.0
                )))
            }
        }
    }

    let removed = store
        .delete_conversation(&peer_id)
        .map_err(|e| CliError::Other(e.to_string()))?;
    let kept = store
        .record_count(&peer_id)
        .map_err(|e| CliError::Other(e.to_string()))?;

    if ctx.json {
        ctx.json_line(&json!({
            "event": "chat_deleted",
            "peer": peer_id.0,
            "removed": removed,
            "kept": kept,
        }));
    } else if removed == 0 && kept == 0 {
        ctx.line(&ctx.dim(&format!(
            "no conversation with {} on this device",
            peer_id.0
        )));
    } else if removed == 0 {
        // Every row was kept. Leading with "deleted 0" would read as a failure
        // of the command rather than what it is — a thread that is still on its
        // way out — so the kept rows are the whole sentence here. A thread of
        // nothing but queued messages is the ordinary shape of this: `chat
        // send` to an offline peer enqueues, and enqueued rows are exactly the
        // ones the keep rule protects.
        ctx.line(&ctx.dim(&format!(
            "nothing deleted from the conversation with {} — all {kept} message(s) are \
             still waiting to send",
            peer_id.0
        )));
    } else {
        ctx.line(&ctx.green(&format!(
            "deleted {removed} message(s) from the conversation with {}",
            peer_id.0
        )));
        if kept > 0 {
            // Named, not silently subtracted: the user asked for the whole
            // thread and did not get all of it, and the reason is a send still
            // on its way out rather than a failure.
            ctx.line(&ctx.dim(&format!(
                "  {kept} kept — still waiting to send; removing them would drop the \
                 files they own"
            )));
        }
    }
    Ok(())
}

/// `chat history <peer>` — print a conversation's persisted history. `peer`
/// may be a raw device id (`pb-<fingerprint>`) or a friendly name; see
/// [`resolve_history_peer`] for how the two are told apart.
async fn history(
    ctx: &Ctx,
    peer: String,
    mark_read: bool,
    path_override: Option<&str>,
) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    let peer_id = resolve_history_peer(ctx, &config, &store, peer).await;
    let records = store
        .history(&peer_id)
        .map_err(|e| CliError::Other(e.to_string()))?;

    // The watermark is the newest message the peer sent us: telling it "I read
    // up to here" is only meaningful about its own messages.
    let newest_theirs = records
        .iter()
        .rev()
        .find(|r| r.direction == peerbeam_chat::Direction::In)
        .map(|r| r.id.clone());

    if ctx.json {
        ctx.json_line(&render_history_json(&records));
        if mark_read {
            if let Some(id) = &newest_theirs {
                let _ = send_read_receipt(&config, &sc, &store, ctx, &peer_id, id).await;
            }
        }
        return Ok(());
    }
    if records.is_empty() {
        ctx.line(&ctx.dim("no messages yet"));
        return Ok(());
    }
    // Resolved against the rows being shown, so a quote can only cite a message
    // that is actually on screen — and a reply whose parent is not gets the
    // "no longer here" marker rather than passing as an ordinary message. The
    // chat crate's own doc calls dropping the marker the worst of the options,
    // because nothing about the result looks wrong.
    let contexts = peerbeam_chat::resolve_replies(&records);
    for (r, reply) in records.iter().zip(contexts.iter()) {
        if let Some(quote) = reply_prefix(reply) {
            ctx.line(&ctx.dim(&quote));
        }
        match (&r.kind, &r.file) {
            (Kind::File, Some(file)) => ctx.line(&render_file_line(r, file)),
            _ => {
                let dir = dir_str(r.direction);
                // A read marker only on our own messages: it is the peer
                // reporting on what we sent. Absent is the norm, since
                // receipts are opt-in on the *other* device.
                let read = if r.read_at.is_some() { " (read)" } else { "" };
                ctx.line(&format!("[{}] {}: {}{}", r.timestamp, dir, r.body, read));
            }
        }
    }
    if mark_read {
        if let Some(id) = &newest_theirs {
            let sent = send_read_receipt(&config, &sc, &store, ctx, &peer_id, id).await;
            if !sent {
                ctx.line(&ctx.dim(
                    "read receipt not sent — turn on device.share_read_receipts, \
                     or the peer is offline or too old for receipts",
                ));
            }
        }
    }
    Ok(())
}

/// Send one read watermark, if the user has opted in and the peer can take it.
///
/// The opt-in is checked here rather than by the caller so there is one place
/// that decides whether this device discloses read times at all.
async fn send_read_receipt(
    config: &EngineConfig,
    sc: &SecureCtx,
    store: &ChatStore,
    ctx: &Ctx,
    peer_id: &DeviceId,
    read_through: &str,
) -> bool {
    if !config.device.share_read_receipts {
        return false;
    }
    // Same as `deliver_reaction`: best-effort, so it cannot propagate — but a
    // receipt that was never even attempted because discovery failed is worth
    // one line on stderr, not silence.
    let devices = match commands::snapshot(config.clone(), 2).await {
        Ok(d) => d,
        Err(e) => {
            commands::report_problem(ctx, &format!("could not look for {peer_id}: {e}"));
            return false;
        }
    };
    let Some(meta) = devices.iter().find(|m| m.device.id == *peer_id) else {
        return false;
    };
    let Ok(quic) = QuicTransport::new() else {
        return false;
    };
    let quic = Arc::new(quic);
    let routes = RouteManager::new(quic.clone());
    let sink = received_sink(ctx);
    let Ok(session) = session_transfer::dial(
        &quic,
        &routes,
        &meta.device,
        "receipt",
        &sc.ident,
        &sc.enc,
        &sc.trust,
        Some((store.clone(), sink)),
    )
    .await
    else {
        return false;
    };
    let peer = DeviceId::from(session.peer_id.clone());
    let ok = if peerbeam_chat::may_exchange_chat(sc.trust.as_ref(), &peer)
        && session.supports_receipt()
    {
        let r = peerbeam_chat::Receipt::read_through(read_through);
        peerbeam_chat::send_receipt(&session.handle, &r)
            .await
            .is_ok()
    } else {
        false
    };
    session.close().await;
    ok
}

/// `chat search <query>` — find messages in this device's stored conversations.
///
/// **A local read and nothing else.** No peer is resolved, no discovery window
/// is opened, nothing is dialled: this reads the same on-disk history
/// [`history`] reads, so it works on a headless box with no network at all, and
/// a thread whose device is long gone is searchable exactly like one that is
/// online. That is also why it is not `async` — there is nothing to await.
///
/// Human output is a table; `--json` is a single object, deliberately, and not
/// one line per hit. `truncated` has to be somewhere a script cannot miss it,
/// and a stream of hit lines with a marker at the end is exactly the shape a
/// consumer reading the first N lines drops on the floor — which is how a
/// script comes to report "3 matches" for a search that had four hundred. It
/// matches `chat history --json`, which is a single object for the same reason.
///
/// Finding nothing is a **successful** search, so it exits 0 rather than 3. A
/// missing peer or a bad index is a lookup that failed; an empty result set is
/// an answer.
fn search(ctx: &Ctx, query: &str, limit: u64, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    // `limit` is already bounded to `1..=MAX_SEARCH_LIMIT` by the parser, so
    // the cast cannot lose anything.
    let found = store
        .search(query, limit as usize)
        .map_err(|e| CliError::Other(e.to_string()))?;

    if ctx.json {
        ctx.json_line(&render_search_json(&found, limit));
        return Ok(());
    }
    if found.hits.is_empty() {
        ctx.line(&ctx.dim(&format!("no messages match {query:?}")));
        return Ok(());
    }
    let rows: Vec<Vec<String>> = found.hits.iter().map(search_row_cells).collect();
    ctx.table(&["WHEN", "PEER", "DIR", "MESSAGE"], &rows);
    if found.truncated {
        ctx.line("");
        // Said out loud, every time. A bounded search whose limit is invisible
        // reads as "that is all there is" — for a search over your own history
        // that is a wrong answer, not a partial one.
        let shown = if limit == 1 {
            "the newest match".to_string()
        } else {
            format!("the newest {limit} matches")
        };
        ctx.line(&ctx.yellow(&format!(
            "showing {shown} — there are more. Narrow the query, or raise \
             --limit (max {}).",
            peerbeam_chat::MAX_SEARCH_LIMIT
        )));
    }
    Ok(())
}

/// One human table row for a hit: when, which conversation, which way, and the
/// stored text around the match.
///
/// The snippet is printed as the engine cut it — a substring of what is
/// stored — with only its line breaks flattened, because a body may hold them
/// and a table cell may not. That flattening is the one and only edit made to
/// it, and it happens here, in the human renderer, rather than in the engine or
/// in `--json`, where the caller is owed the bytes.
fn search_row_cells(hit: &peerbeam_chat::SearchHit) -> Vec<String> {
    vec![
        hit.timestamp.clone(),
        hit.peer_id.clone(),
        dir_str(hit.direction).to_string(),
        hit.snippet.replace(['\n', '\r'], " "),
    ]
}

/// `chat search --json`'s contract: one object, hits newest first, and
/// `truncated` alongside them rather than after them.
///
/// Factored out (like [`render_history_json`]) so it is unit-testable against a
/// seeded store without a live PeerSession.
fn render_search_json(found: &peerbeam_chat::SearchResults, limit: u64) -> serde_json::Value {
    let hits: Vec<serde_json::Value> = found
        .hits
        .iter()
        .map(|h| {
            json!({
                "peer_id": h.peer_id,
                "message_id": h.message_id,
                "timestamp": h.timestamp,
                "direction": h.direction,
                "kind": h.kind,
                "snippet": h.snippet,
            })
        })
        .collect();
    json!({ "hits": hits, "truncated": found.truncated, "limit": limit })
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

    let devices = match commands::snapshot(config.clone(), 2).await {
        Ok(d) => d,
        Err(e) => {
            // Falling back to the literal id is still right — local history is
            // readable without discovery — but the empty history that follows
            // would otherwise read as "no conversation with that name", an
            // answer built entirely out of a failure to look.
            commands::report_problem(ctx, &format!("could not look for {peer}: {e}"));
            return raw_id;
        }
    };
    let candidates: Vec<(String, String)> = devices
        .iter()
        .map(|m| (m.device.id.to_string(), m.device.name.clone()))
        .collect();
    match commands::resolve_peer(ctx, &candidates, &Some(peer)) {
        Ok(index) => devices[index].device.id.clone(),
        Err(_) => raw_id, // not discoverable right now — read local history by the literal id.
    }
}

/// The quote line to print above a reply, or `None` for an ordinary message.
///
/// Two cases, and the second is the one worth having: a reply whose parent this
/// device no longer holds says so. The preview is what is *stored*, cut to
/// [`PREVIEW_CHARS`](peerbeam_chat::PREVIEW_CHARS) by the resolver with nothing
/// added, so this only wraps it.
fn reply_prefix(reply: &peerbeam_chat::ReplyContext) -> Option<String> {
    use peerbeam_chat::ReplyContext;
    match reply {
        ReplyContext::NotAReply => None,
        ReplyContext::Quoting(parent) => Some(format!(
            "  ┌ replying to {}: {}",
            dir_str(parent.direction),
            parent.preview
        )),
        ReplyContext::Orphaned { id } => Some(format!(
            "  ┌ replying to {id} — original message no longer here"
        )),
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
                "reactions": r.reactions,
                // The message this one answers, or null. A script that renders
                // a reply as an ordinary message says something the sender did
                // not: "sure, go ahead" answering "shall I delete the backups?"
                // and answering "can I borrow a pen?" are the same seven
                // characters. Present even when the parent is gone — the id is
                // still what the sender pointed at.
                "in_reply_to": r.in_reply_to,
                // Null both when unread and when the peer does not send
                // receipts — the two are not distinguishable, and a script
                // must not read the absence of a time as a refusal.
                "read_at": r.read_at,
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
    // One RouteManager for this serve loop: presence asks it to classify each
    // inbound connection's remote address, so an accepted session reports the
    // same route vocabulary a dialled one does. Same construction the drain
    // tick uses.
    let accept_routes = RouteManager::new(quic.clone());

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
                    &accept_routes,
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
                //
                // **The listen gate**, for the same reason `serve_loop` has one:
                // `chat watch` is a long-lived background process and is not
                // `peerbeam pipe --listen`, so an inbound pipe is refused
                // explicitly rather than left to the bare discard below. A
                // silent discard would already deny the peer its bytes, but it
                // would say nothing to this side's operator and nothing about
                // *why* — and an unexplained discard is exactly the kind of
                // thing a later change "tidies up" into an accept.
                while let Some(incoming_ch) = session.next_incoming().await {
                    if incoming_ch.channel_type != ChannelType::PIPE {
                        continue; // a transfer stream: discarded unread, as before
                    }
                    let consent = PipeConsent {
                        listening: false,
                        trust: sc.trust.as_ref(),
                        only_from: None,
                        negotiated: session.capabilities(),
                    };
                    let peer = DeviceId::from(session.peer_id.clone());
                    // `listening: false` refuses before a byte is read, and the
                    // sink is where those bytes would go if it ever did not.
                    let mut nowhere = futures::io::sink();
                    if let Err(e) =
                        accept_pipe(incoming_ch, &session.handle, &peer, &consent, &mut nowhere)
                            .await
                    {
                        if !ctx.json {
                            ctx.line(&ctx.dim(&e.to_string()));
                        }
                    }
                }
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
        reachable_targets, render_file_line, render_history_json, render_search_json, reply_prefix,
        resolve_history_peer, search, search_row_cells, send, send_file, spawn_single_flight,
        DeleteGate,
    };
    use crate::commands::{self, SecureCtx};
    use crate::exit::CliError;
    use crate::output::Ctx;
    use peerbeam_chat::{
        ChatMessage, ChatRecord, ChatStore, Direction, FileMeta, FileRef, Kind, Status,
    };
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

    /// **A reply must not read as an ordinary message.** The CLI had neither
    /// half of replies: no way to send one, and `chat history` printed a reply
    /// received from the app as though it answered nothing — which the chat
    /// crate's own module doc calls the worst of the options, because nothing
    /// about the output looks wrong. "Sure, go ahead" answering "shall I delete
    /// the backups?" and answering "can I borrow a pen?" are the same seven
    /// characters.
    #[test]
    fn render_history_json_carries_the_reply_marker() {
        let (store, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        let parent = ChatMessage::new("shall I delete the backups?").expect("parent");
        store
            .append(&ChatRecord::received(&peer, &parent))
            .expect("append parent");
        let answer = ChatMessage::replying("sure, go ahead", Some(&parent.id)).expect("reply");
        store
            .append(&ChatRecord::sent(&peer, &answer))
            .expect("append reply");

        let records = store.history(&peer).expect("history");
        let value = render_history_json(&records);
        let messages = value["messages"].as_array().expect("messages array");
        assert_eq!(messages[0]["in_reply_to"], serde_json::Value::Null);
        assert_eq!(
            messages[1]["in_reply_to"], parent.id,
            "the reply lost what it was answering"
        );
    }

    /// The human line says it too, and says it even when the parent has gone —
    /// an orphaned reply is the case that only shows up after a retention window
    /// closes, which is to say not while anyone is looking.
    #[test]
    fn the_human_line_marks_a_reply_and_admits_a_missing_parent() {
        use peerbeam_chat::{ReplyContext, ReplyParent};

        let quoting = ReplyContext::Quoting(ReplyParent {
            id: "m-1".into(),
            direction: Direction::In,
            kind: Kind::Text,
            preview: "shall I delete the backups?".into(),
        });
        let quote = reply_prefix(&quoting).expect("a reply must be marked");
        assert!(
            quote.contains("shall I delete the backups?"),
            "the quote does not show what is being answered: {quote}"
        );

        let orphaned = reply_prefix(&ReplyContext::Orphaned { id: "m-9".into() })
            .expect("an orphaned reply must still be marked");
        assert!(
            orphaned.contains("m-9") && orphaned.contains("no longer here"),
            "an orphaned reply must say the original is gone: {orphaned}"
        );

        assert!(
            reply_prefix(&ReplyContext::NotAReply).is_none(),
            "an ordinary message must not gain a quote line"
        );
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

    /// The two states 2b added to this surface. `status_str` is serde-based, so
    /// both words appear without a per-status match to forget to extend — this
    /// pins that they read sensibly and, above all, that **neither reads as
    /// though the file were sent**, which is the one way this line could lie.
    #[test]
    fn render_file_line_reads_sensibly_for_a_staging_and_a_queued_row() {
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("movie.mkv", 5_368_709_120).expect("file ref");
        let meta = FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: None,
        };
        for (status, word) in [(Status::Staging, "staging"), (Status::Pending, "pending")] {
            let rec = ChatRecord::file_out(&peer, &r, meta.clone(), status);
            let line = render_file_line(&rec, &meta);
            assert_eq!(
                line,
                format!(
                    "[{}] out file: movie.mkv (5368709120 bytes) — {word}",
                    r.timestamp
                )
            );
            assert!(
                !line.contains("sent"),
                "a share that has not been delivered must never render as sent: {line}"
            );
        }
    }

    /// The same two states through the JSON contract, which a machine consumer
    /// reads: `status` carries the identical lowercase word, and `kind`/`file`
    /// are still there. A 2a-era row (no `Staging` in its vocabulary) is
    /// unaffected — nothing about the shape changed, only which words can
    /// appear in `status`.
    #[test]
    fn render_history_json_carries_the_staging_and_pending_words() {
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("movie.mkv", 64).expect("file ref");
        let meta = FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: None,
        };
        for (status, word) in [(Status::Staging, "staging"), (Status::Pending, "pending")] {
            let value =
                render_history_json(&[ChatRecord::file_out(&peer, &r, meta.clone(), status)]);
            let messages = value["messages"].as_array().expect("messages array");
            assert_eq!(messages[0]["status"], word);
            assert_eq!(messages[0]["kind"], "file");
            assert_eq!(messages[0]["file"]["name"], "movie.mkv");
        }
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

    // ── chat search ─────────────────────────────────────────────

    /// Seed two conversations under an isolated config and return the config
    /// path `chat search` should be pointed at, so the command is driven end to
    /// end (config → identity → store → render) rather than through the store
    /// alone.
    fn seeded_searchable_config(dir: &std::path::Path) -> (EngineConfig, std::path::PathBuf) {
        let config = isolated_config(dir);
        let cfg_path = dir.join("config.json");
        config.save(&cfg_path).expect("save config");
        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);

        let alice = DeviceId::from("pb-alice");
        let bob = DeviceId::from("pb-bob");
        store
            .append(&ChatRecord::sent(
                &alice,
                &ChatMessage::new("the quarterly Invoice").expect("msg"),
            ))
            .expect("seed alice");
        store
            .append(&ChatRecord::received(
                &bob,
                &ChatMessage::new("lunch tomorrow?").expect("msg"),
            ))
            .expect("seed bob text");
        let r = FileRef::new("invoice-2026.pdf", 9).expect("file ref");
        let meta = FileMeta {
            name: r.name.clone(),
            size: r.size,
            // Never searched: it is where the file sits on this disk, not
            // anything anyone said.
            local_path: Some("/home/someone/Downloads/private-dir/x.bin".into()),
        };
        store
            .append(&ChatRecord::file_out(&bob, &r, meta, Status::Sent))
            .expect("seed bob file");
        (config, cfg_path)
    }

    /// The command end to end: it reads local history, finds a text body and a
    /// file name in two different conversations, and never touches the network
    /// (there is none — the isolated config's transfer port is 0 and no peer
    /// exists).
    #[test]
    fn search_finds_text_and_file_names_without_touching_the_network() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (config, cfg_path) = seeded_searchable_config(dir.path());

        let ctx = quiet_ctx();
        let result = search(
            &ctx,
            "invoice",
            peerbeam_chat::DEFAULT_SEARCH_LIMIT as u64,
            Some(cfg_path.to_str().expect("utf8 path")),
        );
        assert!(result.is_ok(), "search must succeed offline: {result:?}");

        // And the same query through the store the command built, so the
        // assertions can be about the hits rather than about stdout.
        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);
        let found = store.search("INVOICE", 50).expect("search");
        let peers: Vec<&str> = found.hits.iter().map(|h| h.peer_id.as_str()).collect();
        assert_eq!(found.hits.len(), 2, "{:?}", found.hits);
        assert!(peers.contains(&"pb-alice") && peers.contains(&"pb-bob"));
        // The path only exists on this machine and is not conversation
        // content.
        assert!(store
            .search("private-dir", 50)
            .expect("search")
            .hits
            .is_empty());
    }

    /// Finding nothing is a successful search: exit 0, not the not-found code a
    /// failed lookup uses.
    #[test]
    fn search_with_no_matches_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_config, cfg_path) = seeded_searchable_config(dir.path());
        let result = search(
            &quiet_ctx(),
            "nothing-here-matches-this",
            50,
            Some(cfg_path.to_str().expect("utf8 path")),
        );
        assert!(
            result.is_ok(),
            "an empty result set is an answer: {result:?}"
        );
    }

    /// `--json` is one object, and `truncated` sits inside it with the hits —
    /// not on a trailing line a consumer reading the first N can drop.
    #[test]
    fn render_search_json_is_one_object_carrying_truncated_and_limit() {
        let (store, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        for _ in 0..4 {
            store
                .append(&ChatRecord::sent(
                    &peer,
                    &ChatMessage::new("invoice").expect("msg"),
                ))
                .expect("append");
        }

        let cut = store.search("invoice", 2).expect("search");
        let value = render_search_json(&cut, 2);
        assert_eq!(value["hits"].as_array().expect("hits").len(), 2);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["limit"], 2);
        let hit = &value["hits"][0];
        assert_eq!(hit["peer_id"], "pb-bob");
        assert_eq!(hit["kind"], "text");
        assert_eq!(hit["direction"], "out");
        assert_eq!(hit["snippet"], "invoice");
        assert!(hit["message_id"].as_str().is_some_and(|s| !s.is_empty()));

        let whole = store.search("invoice", 4).expect("search");
        assert_eq!(render_search_json(&whole, 4)["truncated"], false);
    }

    #[test]
    fn render_search_json_with_no_hits_is_an_empty_array_not_null() {
        let (store, _dir) = seeded_store();
        let value = render_search_json(&store.search("nothing", 50).expect("search"), 50);
        assert_eq!(value["hits"], serde_json::json!([]));
        assert_eq!(value["truncated"], false);
    }

    /// The human table flattens line breaks — a body may hold them and a table
    /// cell may not — and changes nothing else about what was stored.
    #[test]
    fn search_row_cells_flatten_line_breaks_and_keep_the_rest_verbatim() {
        let (store, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        store
            .append(&ChatRecord::sent(
                &peer,
                &ChatMessage::new("first line\nInvoice line\rthird").expect("msg"),
            ))
            .expect("append");

        let found = store.search("invoice", 50).expect("search");
        let cells = search_row_cells(&found.hits[0]);
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[1], "pb-bob");
        assert_eq!(cells[2], "out");
        assert_eq!(cells[3], "first line Invoice line third");
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
            None,
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

    // **The 2b behaviour change.** `chat send --file` to an unreachable peer
    // must QUEUE, exactly as text's `send` does — exit 0, the row `Pending`,
    // an outbox entry, and the bytes copied into storage the outbox owns. 2a
    // failed here with `CliError::Connection` and marked the row `Failed`;
    // that is what this replaces.
    #[tokio::test]
    async fn send_file_to_unreachable_addr_queues_instead_of_failing() {
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
            result.is_ok(),
            "chat send --file to an unreachable peer must queue, not fail: {result:?}"
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
            Status::Pending,
            "an unreachable peer leaves the share queued, never Failed or Sent"
        );
        assert_eq!(hist[0].file.as_ref().expect("file meta").name, "report.pdf");

        // …and it is genuinely queued, with bytes of its own.
        let outbox = store.outbox_for(&target_id).expect("outbox_for");
        assert_eq!(outbox.len(), 1, "the file must still be queued: {outbox:?}");
        assert_eq!(outbox[0].kind, Kind::File);
        let staged = outbox[0].file.as_ref().expect("a staged blob");
        assert_eq!(
            std::fs::read(&staged.staged_path).expect("read the staged blob"),
            vec![7u8; 128],
            "the outbox must own its own copy of the bytes, not a path"
        );
    }

    // The refusal 2b introduced on this surface: 2a streamed straight from the
    // source, so nothing was ever refused for size. A refusal must name the
    // reason, the measured numbers AND the config key that sets the bound —
    // and must not claim to be a session failure, since nothing was dialed.
    #[tokio::test]
    async fn send_file_over_the_attachment_cap_is_refused_naming_the_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = isolated_config(dir.path());
        config.device.max_queued_file_bytes = 64;
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");

        let file_path = dir.path().join("big.bin");
        std::fs::write(&file_path, vec![7u8; 128]).expect("write file");

        let ctx = quiet_ctx();
        let err = send_file(
            &ctx,
            None,
            Some("127.0.0.1:1".to_string()),
            file_path.to_string_lossy().into_owned(),
            Some(cfg_path.to_str().expect("utf8 path")),
        )
        .await
        .expect_err("a file over the cap must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("128 bytes is over the 64-byte limit"),
            "the refusal must name both numbers, got: {msg}"
        );
        assert!(
            msg.contains("device.max_queued_file_bytes"),
            "the refusal must name the knob that sets the cap, got: {msg}"
        );
        assert!(
            !msg.contains("chat session error"),
            "a stage that never dialed anything is not a session failure: {msg}"
        );

        // Nothing was queued, and the row says so rather than sitting on
        // `Staging` forever.
        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);
        let target_id = DeviceId::from("addr");
        assert!(store.outbox_for(&target_id).expect("outbox_for").is_empty());
        let hist = store.history(&target_id).expect("history");
        assert_eq!(hist[0].status, Status::Failed);
    }

    // The other bound. `StagingError::NotEnoughSpace` now names all three
    // numbers itself (the floor included — without it the message reads as
    // though the send should have succeeded), so what this proves at the CLI
    // layer is that they survive to the user, and that the *knob* behind them
    // is named too, which only this layer knows.
    #[tokio::test]
    async fn send_file_below_the_free_space_floor_is_refused_naming_the_floor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = isolated_config(dir.path());
        config.device.min_free_bytes = u64::MAX; // unsatisfiable on any disk
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");

        let file_path = dir.path().join("report.pdf");
        std::fs::write(&file_path, vec![7u8; 128]).expect("write file");

        let ctx = quiet_ctx();
        let err = send_file(
            &ctx,
            None,
            Some("127.0.0.1:1".to_string()),
            file_path.to_string_lossy().into_owned(),
            Some(cfg_path.to_str().expect("utf8 path")),
        )
        .await
        .expect_err("an unsatisfiable free-space floor must refuse");
        let msg = err.to_string();
        assert!(
            msg.contains("staging 128 bytes would leave less than the"),
            "the refusal must name what the copy needed, got: {msg}"
        );
        assert!(
            msg.contains(&u64::MAX.to_string()),
            "…and the floor it would have breached, which is the only reason it failed: {msg}"
        );
        assert!(
            msg.contains("available"),
            "…and what was actually free: {msg}"
        );
        assert!(
            msg.contains("device.min_free_bytes"),
            "the refusal must name the floor's knob — only this layer knows it: {msg}"
        );
        assert!(
            !msg.contains("chat session error"),
            "a stage that never dialed anything is not a session failure: {msg}"
        );
    }

    /// **The queued message must be true on this surface.** `drain_tick` →
    /// `flush_to_session` skips `Kind::File`, so `daemon`/`receive`/`chat
    /// watch` deliver queued text and declines and never a queued file — which
    /// makes text's "a running daemon/watch will deliver" wording false here.
    /// The tempting edit is to borrow it anyway; this is what stops that being
    /// invisible.
    ///
    /// **And the `--addr` half, which is a stronger claim.** That target is
    /// filed under the routing placeholder `commands::ADDR_PEER_ID`, and both
    /// drains — `drain_tick` above and the FFI's `chat_drain_loop` — pick the
    /// peers they flush out of discovery, which never yields that id. So the
    /// running-app sentence is not merely unhelpful there, it is false: nothing
    /// delivers that entry, ever. The two cases are pinned in one test on
    /// purpose, so an edit to either wording has to look at the other.
    #[test]
    fn the_queued_message_names_what_actually_delivers_a_queued_file() {
        let r = FileRef::new("movie.mkv", 5_368_709_120).expect("file ref");
        let [first, second] = super::queued_lines("bob", "pb-bob", &r);

        assert!(
            !first.contains("daemon/watch will deliver"),
            "text's wording is false for a file on this surface: {first}"
        );
        assert!(
            first.contains("5368709120 bytes staged"),
            "the queue now holds a full copy — say how much: {first}"
        );
        assert!(
            first.contains("a running PeerBeam app"),
            "the FIRST line must name what actually delivers it: {first}"
        );
        assert!(
            first.contains("drain queued text and declines, not files"),
            "…and must not leave the reader to assume the CLI's own drain will: {first}"
        );
        assert!(first.starts_with("queued for bob"), "{first}");

        // The second line is the way to get the bytes back, and it must be
        // copy-pasteable: the peer key the row is filed under, and the id.
        assert_eq!(
            second,
            format!(
                "`chat cancel pb-bob {}` drops it and frees the staged bytes",
                r.id
            )
        );

        // ── `--addr`: same staging, no delivery at all. ──────────────────
        let [addr_first, addr_second] =
            super::queued_lines("127.0.0.1:9999", commands::ADDR_PEER_ID, &r);

        assert!(
            !addr_first.contains("a running PeerBeam app"),
            "the `--to` promise is FALSE here — the entry is filed under a placeholder id no \
             drain can resolve: {addr_first}"
        );
        assert!(
            addr_first.contains("nothing will deliver it"),
            "an `--addr` queue must say plainly that it never drains: {addr_first}"
        );
        assert!(
            addr_first.contains("5368709120 bytes"),
            "the bytes are on disk either way — say how many: {addr_first}"
        );
        assert!(
            addr_first.contains("--to"),
            "…and must name the only routing that lets a queued file drain: {addr_first}"
        );

        // `chat cancel` is the sole way out for this one, so it must stay
        // copy-pasteable under the placeholder id the row is really filed under.
        assert_eq!(
            addr_second,
            format!(
                "`chat cancel addr {}` drops it and frees the staged bytes",
                r.id
            )
        );
    }

    /// The throttle behind `chat_staging`/the bar: percent granularity, one
    /// report per step, and no division by zero on an empty file.
    #[test]
    fn staging_step_reports_at_percent_granularity() {
        assert_eq!(super::staging_step(0, 1000), 0);
        assert_eq!(super::staging_step(5, 1000), 0, "same step: no new report");
        assert_eq!(super::staging_step(10, 1000), 1);
        assert_eq!(super::staging_step(1000, 1000), super::STAGING_STEPS);
        assert_eq!(super::staging_step(0, 0), 0, "an empty file is one step");
        // A source that outgrew its own metadata mid-copy keeps reporting
        // rather than pinning at the last step.
        assert!(super::staging_step(2000, 1000) > super::STAGING_STEPS);
        // 64 KiB reports across a 16 GiB file collapse to ~100, not ~262_000.
        let total = 17_179_869_184u64;
        let steps: std::collections::HashSet<u64> = (0..4096)
            .map(|n| super::staging_step(n * 65_536, total))
            .collect();
        assert!(steps.len() <= 2, "the first 256 MiB is at most 2 steps");
    }

    /// Queue one file for an unreachable peer and hand back everything a
    /// cancel test needs: the config path, the store, the message id and the
    /// blob the outbox owns. Extracted so both cancel tests exercise a queue
    /// built the way a real `chat send --file` builds one, rather than a
    /// hand-assembled one that could drift from it.
    async fn queue_one_file(
        ctx: &Ctx,
        dir: &std::path::Path,
        config: &EngineConfig,
        cfg_path: &std::path::Path,
    ) -> (ChatStore, String, String) {
        let file_path = dir.join("report.pdf");
        std::fs::write(&file_path, vec![7u8; 128]).expect("write file");
        send_file(
            ctx,
            None,
            Some("127.0.0.1:1".to_string()),
            file_path.to_string_lossy().into_owned(),
            Some(cfg_path.to_str().expect("utf8 path")),
        )
        .await
        .expect("an unreachable peer must queue");

        let sc = SecureCtx::build(config).expect("secure ctx");
        let store = commands::chat_store(config, &sc.enc, &sc.ident);
        let entry = store
            .outbox_for(&DeviceId::from("addr"))
            .expect("outbox_for")
            .pop()
            .expect("one queued entry");
        let staged = entry.file.expect("a staged blob").staged_path;
        (store, entry.message_id, staged)
    }

    // `chat cancel` on a queued share: the entry goes, the bytes go, and the
    // row settles. This is the only thing that reclaims a queued file's disk
    // space on this surface, so all three parts are load-bearing.
    #[tokio::test]
    async fn chat_react_applies_locally_even_with_no_peer_to_tell() {
        // The reaction is this device's record of its own gesture. An
        // unreachable peer means "not delivered", never "not applied".
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");
        let ctx = quiet_ctx();
        let cfg_str = cfg_path.to_str().expect("utf8 path");

        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);
        let peer = DeviceId::from("pb-bob");
        let msg = peerbeam_chat::ChatMessage::new("ship it").expect("message");
        store
            .append(&ChatRecord::sent(&peer, &msg))
            .expect("append");

        super::react(
            &ctx,
            "pb-bob".to_string(),
            msg.id.clone(),
            "\u{1F44D}",
            false,
            Some(cfg_str),
        )
        .await
        .expect("reacting to our own row is allowed");

        let hist = store.history(&peer).expect("history");
        assert_eq!(hist[0].reactions.len(), 1);
        assert_eq!(hist[0].reactions[0].by, peerbeam_chat::Direction::Out);
    }

    #[tokio::test]
    async fn chat_react_refuses_an_empty_or_oversized_reaction() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");
        let ctx = quiet_ctx();
        let cfg_str = cfg_path.to_str().expect("utf8 path");

        for bad in [String::new(), "e".repeat(peerbeam_chat::MAX_REACTION + 1)] {
            let err = super::react(
                &ctx,
                "pb-bob".to_string(),
                "m1".to_string(),
                &bad,
                false,
                Some(cfg_str),
            )
            .await
            .expect_err("an empty or oversized reaction is a usage error");
            assert!(matches!(err, CliError::Usage(_)));
        }
    }

    #[tokio::test]
    async fn chat_cancel_dequeues_a_queued_file_and_deletes_its_blob() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");
        let ctx = quiet_ctx();
        let (store, id, staged) = queue_one_file(&ctx, dir.path(), &config, &cfg_path).await;
        assert!(
            std::path::Path::new(&staged).exists(),
            "precondition: the blob was staged"
        );

        super::cancel(
            &ctx,
            "addr".to_string(), // `--addr`'s routing placeholder id
            id.clone(),
            Some(cfg_path.to_str().expect("utf8 path")),
        )
        .await
        .expect("a queued outgoing file is ours to cancel");

        let peer = DeviceId::from("addr");
        assert!(
            store.outbox_for(&peer).expect("outbox_for").is_empty(),
            "the queue entry must be gone"
        );
        assert!(
            !std::path::Path::new(&staged).exists(),
            "the bytes the outbox owned must be gone: {staged}"
        );
        assert_eq!(
            store.get(&peer, &id).expect("get").expect("row").status,
            Status::Failed,
            "a cancelled share settles Failed — there is no Cancelled status"
        );
    }

    // The authorization, not a key match. `chat cancel` must refuse anything
    // `ChatRecord::is_cancellable_outgoing_file` refuses — here a text row and
    // an already-sent file — and must leave it untouched, because the whole
    // point of routing through that one rule is that this command deletes
    // bytes. (Direction and the settled states are proved exhaustively by that
    // rule's own tests in `peerbeam-chat`; this proves the CLI actually asks
    // it rather than reimplementing it.)
    #[tokio::test]
    async fn chat_cancel_refuses_what_the_shared_rule_refuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");
        let ctx = quiet_ctx();
        let cfg_arg = Some(cfg_path.to_str().expect("utf8 path"));

        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);
        let peer = DeviceId::from("pb-bob");

        // A text row: nothing staged, nothing to stop.
        let m = ChatMessage::new("hi").expect("msg");
        store.append(&ChatRecord::sent(&peer, &m)).expect("append");
        // A file share that already completed.
        let done = FileRef::new("done.bin", 1).expect("file ref");
        store
            .append(&ChatRecord::file_out(
                &peer,
                &done,
                FileMeta::new(&done.name, done.size, None),
                Status::Sent,
            ))
            .expect("append");

        for id in [&m.id, &done.id] {
            let err = super::cancel(&ctx, "pb-bob".to_string(), id.clone(), cfg_arg)
                .await
                .expect_err("the shared rule must refuse this");
            assert!(
                matches!(err, CliError::NotFound(_)),
                "a refusal must be NotFound (exit 3), got: {err:?}"
            );
        }
        // Untouched: neither row was rewritten on the way out.
        assert_eq!(
            store.get(&peer, &m.id).expect("get").expect("row").status,
            Status::Sent,
            "a refused cancel must not rewrite the row it refused"
        );
        assert_eq!(
            store
                .get(&peer, &done.id)
                .expect("get")
                .expect("row")
                .status,
            Status::Sent
        );

        // An id nobody ever minted is a clean NotFound too, not a panic.
        let err = super::cancel(
            &ctx,
            "pb-bob".to_string(),
            "0000000000001".to_string(),
            cfg_arg,
        )
        .await
        .expect_err("an unknown id has nothing to cancel");
        assert!(matches!(err, CliError::NotFound(_)), "got: {err:?}");
    }

    // **The second write**, which the two tests above do not reach: they prove
    // the gate refuses a settled row, single-threaded, and stop there.
    // `cancel` authorizes on one read and settles on another, and the window
    // between them is wide — a whole `outbox_for` (an AppStore `list` plus an
    // AEAD decrypt of every queued record) and an unlink. The writer that lands
    // inside it is in another *process*, and that is the documented delivery
    // mechanism, not a hypothetical: this binary's drain does not deliver
    // queued files, so `report_queued` tells the user a running PeerBeam app
    // sharing this data directory is what does — and that app writes `Sent` on
    // completion and `Declined` on an arriving `FileDecline`, over an
    // `FsAppStore` with no cross-process lock.
    //
    // So the settle must re-apply `ChatRecord::is_cancellable_outgoing_file`
    // rather than write unconditionally. This drives `settle_cancelled`
    // directly — that IS the second read, and reaching it through `cancel`
    // would need the row to change mid-call, which no single-threaded test can
    // stage. Both rows must survive unchanged, and the whole command must
    // never report a cancellation of either.
    #[tokio::test]
    async fn a_cancel_that_lost_the_race_never_overwrites_a_settled_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");
        let ctx = quiet_ctx();
        let cfg_arg = Some(cfg_path.to_str().expect("utf8 path"));

        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);
        let peer = DeviceId::from("pb-bob");

        // Exactly the two states the shared rule calls final, and exactly the
        // two the draining process can land while this cancel is in flight.
        let mut seeded = Vec::new();
        for (name, status, why) in [
            (
                "delivered.mp4",
                Status::Sent,
                "a delivered file must not be relabelled cancelled",
            ),
            (
                "refused.mp4",
                Status::Declined,
                "a peer's refusal must not be relabelled as our cancellation",
            ),
        ] {
            let r = FileRef::new(name, 4).expect("file ref");
            store
                .append(&ChatRecord::file_out(
                    &peer,
                    &r,
                    FileMeta::new(&r.name, r.size, None),
                    status,
                ))
                .expect("seed the settled row");
            seeded.push((r.id, status, why));
        }

        for (id, status, why) in &seeded {
            assert!(
                !super::settle_cancelled(&store, &peer, id).expect("settle_cancelled"),
                "settle_cancelled must report it changed nothing: {why}"
            );
            assert_eq!(
                store
                    .get(&peer, id)
                    .expect("get")
                    .expect("the row survives")
                    .status,
                *status,
                "{why}"
            );
            // …and the whole command agrees: it refuses outright, so nothing
            // it prints can tell the user a settled share was cancelled.
            let err = super::cancel(&ctx, "pb-bob".to_string(), id.clone(), cfg_arg)
                .await
                .expect_err("a settled row is not ours to cancel");
            assert!(
                matches!(err, CliError::NotFound(_)),
                "a refusal must be NotFound (exit 3), got: {err:?}"
            );
            assert_eq!(
                store
                    .get(&peer, id)
                    .expect("get")
                    .expect("the row survives")
                    .status,
                *status,
                "still untouched after the public call: {why}"
            );
        }
    }

    // ── chat delete ─────────────────────────────────────────────────────────

    // The gate is where "irreversible" is enforced, so it is tested as the pure
    // function it is: `--yes` proceeds, an explicit "no" declines, and — the
    // arm that separates this from `trust approve` — nobody-to-ask is neither.
    #[test]
    fn delete_gate_treats_only_yes_as_consent_and_silence_as_neither() {
        assert_eq!(super::delete_gate(true, None), DeleteGate::Proceed);
        assert_eq!(super::delete_gate(true, Some(false)), DeleteGate::Proceed);
        assert_eq!(super::delete_gate(false, Some(true)), DeleteGate::Proceed);
        assert_eq!(super::delete_gate(false, Some(false)), DeleteGate::Declined);
        assert_eq!(super::delete_gate(false, None), DeleteGate::Unanswerable);
    }

    // The count and the "this device only" caveat must be inside the question,
    // not printed beside it: `--quiet` suppresses `Ctx::line` but cannot
    // suppress what `prompt::confirm` writes.
    #[test]
    fn delete_question_carries_the_count_and_says_the_peer_keeps_its_copy() {
        let q = super::delete_question(&DeviceId::from("pb-bob"), 7);
        assert!(q.contains("pb-bob"), "the question names the peer: {q}");
        assert!(q.contains('7'), "the question names the count: {q}");
        assert!(
            q.contains("this device only"),
            "the question says it is not an unsend: {q}"
        );
    }

    #[tokio::test]
    async fn chat_delete_removes_a_conversations_history_from_this_device() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");
        let ctx = quiet_ctx(); // `--yes` is implied: the gate is proved above.

        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);
        let peer = DeviceId::from("pb-bob");
        let other = DeviceId::from("pb-ann");
        for body in ["hi", "there"] {
            let m = ChatMessage::new(body).expect("message");
            store.append(&ChatRecord::sent(&peer, &m)).expect("append");
        }
        let keep = ChatMessage::new("untouched").expect("message");
        store
            .append(&ChatRecord::sent(&other, &keep))
            .expect("append");

        super::delete(
            &ctx,
            "pb-bob".to_string(),
            Some(cfg_path.to_str().expect("utf8 path")),
        )
        .await
        .expect("deleting our own history is allowed");

        assert_eq!(
            store.record_count(&peer).expect("record_count"),
            0,
            "every row in the named thread is gone"
        );
        assert_eq!(
            store.record_count(&other).expect("record_count"),
            1,
            "a delete is scoped to one conversation, never the store"
        );
    }

    // The safety property: a script that never says `--yes` must not be able to
    // destroy a thread by accident. `--json` is non-interactive by construction,
    // so there is nobody to ask — and unlike `trust approve`, that is refused
    // rather than taken as consent.
    #[tokio::test]
    async fn chat_delete_without_yes_refuses_and_leaves_the_conversation_intact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");
        // `quiet_ctx` but with `--yes` withheld: json ⇒ not interactive.
        let ctx = Ctx::new(true, true, 0, true, false);

        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);
        let peer = DeviceId::from("pb-bob");
        let m = ChatMessage::new("hi").expect("message");
        store.append(&ChatRecord::sent(&peer, &m)).expect("append");

        let err = super::delete(
            &ctx,
            "pb-bob".to_string(),
            Some(cfg_path.to_str().expect("utf8 path")),
        )
        .await
        .expect_err("an unconfirmed delete must refuse");
        match err {
            CliError::Usage(m) => assert!(
                m.contains("--yes"),
                "the refusal names the flag that would have worked: {m}"
            ),
            other => panic!("expected a usage refusal, got {other:?}"),
        }
        assert_eq!(
            store.record_count(&peer).expect("record_count"),
            1,
            "a refused delete rewrites nothing"
        );
    }

    // `delete` and `cancel` are not two words for one thing, and this is the
    // half that costs a file if they are conflated: the row backing a **queued**
    // send survives, along with the bytes the outbox staged for it. Dropping the
    // record would make the next drain read "nothing will ever settle this" and
    // release the blob — the file would vanish without ever being sent.
    #[tokio::test]
    async fn chat_delete_keeps_the_row_backing_a_queued_file_and_its_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");
        let ctx = quiet_ctx();
        let (store, id, staged) = queue_one_file(&ctx, dir.path(), &config, &cfg_path).await;

        // A settled text row in the same thread, to prove the delete still does
        // its job around the one row it must keep.
        let peer = DeviceId::from("addr");
        let m = ChatMessage::new("chatter").expect("message");
        store.append(&ChatRecord::sent(&peer, &m)).expect("append");

        super::delete(
            &ctx,
            "addr".to_string(), // `--addr`'s routing placeholder id
            Some(cfg_path.to_str().expect("utf8 path")),
        )
        .await
        .expect("deleting a thread with a queued file is allowed");

        assert!(
            store.get(&peer, &id).expect("get").is_some(),
            "the row a queued send depends on must survive"
        );
        assert!(
            store.get(&peer, &m.id).expect("get").is_none(),
            "the settled row around it is still deleted"
        );
        assert_eq!(
            store
                .outbox_for(&peer)
                .expect("outbox_for")
                .into_iter()
                .filter(|e| e.message_id == id)
                .count(),
            1,
            "delete leaves the queue alone — that is `chat cancel`'s job"
        );
        assert!(
            std::path::Path::new(&staged).exists(),
            "the staged bytes must still be there: {staged}"
        );
    }

    // The ordinary shape of a thread with an offline peer, and the one that
    // looks like a bug until you know the rule: `chat send` to an unreachable
    // device *enqueues*, and an enqueued row is exactly what the keep rule
    // protects — so a delete here removes nothing and keeps everything. It must
    // still succeed, and the rows must survive intact: they are what the drain
    // settles when the peer comes back.
    #[tokio::test]
    async fn chat_delete_of_an_all_queued_thread_keeps_everything_and_still_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");
        let ctx = quiet_ctx();
        let cfg_str = cfg_path.to_str().expect("utf8 path");

        send(
            &ctx,
            None,
            Some("127.0.0.1:1".to_string()),
            "still on its way".to_string(),
            None,
            Some(cfg_str),
        )
        .await
        .expect("an unreachable peer queues rather than erroring");

        super::delete(&ctx, "addr".to_string(), Some(cfg_str))
            .await
            .expect("a thread of queued messages is still a legal delete");

        let sc = SecureCtx::build(&config).expect("secure ctx");
        let store = commands::chat_store(&config, &sc.enc, &sc.ident);
        let peer = DeviceId::from("addr");
        assert_eq!(
            store.record_count(&peer).expect("record_count"),
            1,
            "a queued message's row is kept, not deleted"
        );
        assert_eq!(
            store.outbox_for(&peer).expect("outbox_for").len(),
            1,
            "and the queue entry it belongs to is untouched"
        );
    }

    // Converging, not failing: a second `delete` — or one aimed at a peer this
    // device has never spoken to — has nothing to remove and says so.
    #[tokio::test]
    async fn chat_delete_of_a_conversation_that_is_not_there_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = isolated_config(dir.path());
        let cfg_path = dir.path().join("config.json");
        config.save(&cfg_path).expect("save config");
        // No `--yes`: with nothing to lose there is nothing to confirm, so this
        // must not be refused either.
        let ctx = Ctx::new(true, true, 0, true, false);

        super::delete(
            &ctx,
            "pb-nobody".to_string(),
            Some(cfg_path.to_str().expect("utf8 path")),
        )
        .await
        .expect("deleting nothing is a clean no-op, not an error");
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
            reply_to: None,
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
            reply_to: None,
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
