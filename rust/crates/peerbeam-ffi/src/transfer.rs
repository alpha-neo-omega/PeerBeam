//! Transfer orchestration behind the FFI. Wraps the production transfer engine
//! (RouteManager + authenticate + SecureLink + send/receive) into an
//! id-addressed, event-driven manager: multiple simultaneous transfers, each a
//! background task, controlled by id, reporting progress/stats/history as
//! events. No file bytes cross FFI — only paths in, metadata/progress out.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use peerbeam_chat::{
    ChatStore, Direction as ChatDirection, FileRef, PendingFile, StagingLimits, StagingStore,
    Status as ChatStatus,
};
// Only referenced by the guard test's assertions below; the guard's own
// kind check now lives solely in `ChatRecord::is_settleable_file_row`.
#[cfg(test)]
use peerbeam_chat::Kind as ChatKind;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::{
    Device, DeviceType, Direction, Progress, TransferSession, TransferStatus,
};
use peerbeam_domain::error::Result as DResult;
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::TrustStore;
use peerbeam_domain::session::CapabilitySet;
use peerbeam_engine::RouteManager;
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    peek_incoming_meta, receive_on_channel, send_file_on_session, send_folder_on_session,
    ChannelReceived, FolderSendRequest, Identity, SendRequest, TransferControl, TransferOutcome,
    BACK_PAUSE, BACK_RESUME,
};
use peerbeam_transfer_quic::{QuicChannels, QuicTransport};
use peerbeam_trust_fs::FsTrust;

use crate::error::{from_domain, Code};
use crate::events;

// ── statistics ──────────────────────────────────────────────────

struct Stats {
    total: u64,
    transferred: u64,
    current_speed: f64,
    average_speed: f64,
    eta_secs: Option<u64>,
    /// When bytes actually started moving — the first `update()` call with
    /// `transferred > 0` — used as the baseline for `average_speed` instead
    /// of registration time. A transfer can sit registered for up to
    /// `ACCEPT_TIMEOUT` waiting on the peer's accept/reject decision; that
    /// idle wait must not be counted against the transfer's average speed.
    /// `None` until that first byte is observed.
    average_started: Option<Instant>,
    last_t: Instant,
    last_bytes: u64,
}

impl Stats {
    fn new() -> Self {
        let now = Instant::now();
        Stats {
            total: 0,
            transferred: 0,
            current_speed: 0.0,
            average_speed: 0.0,
            eta_secs: None,
            average_started: None,
            last_t: now,
            last_bytes: 0,
        }
    }

    /// Reset the instantaneous-rate baseline after a pause→resume
    /// transition, so the very next `update()` doesn't compute a bogus
    /// speed/ETA from a `dt` spanning the entire pause (a near-zero rate
    /// from a huge elapsed time, and — via the same stale `current_speed` —
    /// an inflated ETA). Leaves `transferred`/`total` and the
    /// `average_speed` baseline untouched: only the EMA/instantaneous
    /// tracking restarts, as if the rate measurement began fresh from here.
    fn mark_resumed(&mut self) {
        self.last_t = Instant::now();
        self.last_bytes = self.transferred;
        self.current_speed = 0.0;
        self.eta_secs = None;
    }

    fn update(&mut self, transferred: u64, total: u64) {
        let now = Instant::now();
        self.total = total;
        let dt = now.duration_since(self.last_t).as_secs_f64();
        if dt >= 0.05 {
            let inst = transferred.saturating_sub(self.last_bytes) as f64 / dt;
            // Exponential moving average for a stable instantaneous rate.
            self.current_speed = if self.current_speed == 0.0 {
                inst
            } else {
                self.current_speed * 0.7 + inst * 0.3
            };
            self.last_t = now;
            self.last_bytes = transferred;
        }
        self.transferred = transferred;
        if self.average_started.is_none() && transferred > 0 {
            self.average_started = Some(now);
        }
        self.average_speed = match self.average_started {
            Some(start) => {
                let elapsed = now.duration_since(start).as_secs_f64();
                if elapsed > 0.0 {
                    transferred as f64 / elapsed
                } else {
                    0.0
                }
            }
            None => 0.0,
        };
        self.eta_secs = if self.current_speed > 1.0 && total >= transferred {
            Some(((total - transferred) as f64 / self.current_speed) as u64)
        } else {
            None
        };
    }

    fn dto(&self) -> Value {
        json!({
            "transferred_bytes": self.transferred,
            "total_bytes": self.total,
            "current_speed": self.current_speed,
            "average_speed": self.average_speed,
            "eta_secs": self.eta_secs,
        })
    }
}

// ── active transfer ─────────────────────────────────────────────

struct Active {
    id: String,
    direction: &'static str,
    peer: String,
    /// The peer's device id — the stable, routable identity, kept alongside the
    /// human-readable `peer` name so every terminal event this transfer emits
    /// can say *which device* it belongs to. A surface needs that to route a
    /// transfer event to a conversation: the name is neither unique nor stable,
    /// and `finish`/`finish_failed`/`record` only ever hold the `Active`, never
    /// the session it came from.
    peer_id: String,
    ctrl: TransferControl,
    stats: Arc<Mutex<Stats>>,
    file: Arc<Mutex<String>>,
    status: Mutex<String>,
    /// Local filesystem path of the transferred item, once known: the source
    /// path for sends; the save directory (folders) or None (single files —
    /// derived from the final name at history time) for receives. Lets the UI
    /// open what was transferred.
    path: Mutex<Option<String>>,
    /// The background task running this transfer, so cancel can abort it
    /// immediately even if a send is blocked on a slow link.
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Active {
    fn dto(&self) -> Value {
        json!({
            "id": self.id,
            "direction": self.direction,
            "peer": self.peer,
            "peer_id": self.peer_id,
            "file": *self.file.lock().unwrap(),
            "status": *self.status.lock().unwrap(),
            "stats": self.stats.lock().unwrap().dto(),
        })
    }
}

/// The user's decision on an incoming-transfer prompt. Accepting a transfer
/// and trusting the sending device are deliberately separate: `AcceptOnce`
/// lets this one transfer through and nothing else; only `AcceptAndTrust`
/// approves the device for future auto-accept. Never inferred from a plain
/// accept — trust is always an explicit, separate choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcceptDecision {
    Reject,
    AcceptOnce,
    AcceptAndTrust,
}

/// How an incoming transfer's approval actually resolved.
///
/// [`Manager::wait_for_accept`] used to return `bool`, which collapsed three
/// genuinely different things into one: an explicit refusal, an unanswered
/// prompt hitting [`ACCEPT_TIMEOUT`] (180 s), and a sender that dropped before
/// deciding. For the *transfer* they are identical — nothing moves either way,
/// which is why the `bool` was fine until now — but they are not identical in
/// what we are entitled to tell the sender.
///
/// Only [`Rejected`](Self::Rejected) is the user's decision. Reporting a
/// three-minute timeout to the peer as "I declined your file" would mean a user
/// who stepped away from their desk loses the file *and* has the sender's
/// history assert they refused it — a cross-device, irreversible claim built
/// from someone simply not being there. It would also short-circuit the bounded
/// backstop `OutboxEntry.offers_refused` exists to provide: that counter
/// tolerates N attempts across refusals *and* timeouts, precisely so a single
/// missed prompt is recoverable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcceptOutcome {
    /// The user accepted (`AcceptOnce`/`AcceptAndTrust`), or the transfer was
    /// auto-accepted from a previously approved device.
    Accepted,
    /// The user explicitly refused: `reject()`, or `cancel()` firing the
    /// pending sender with [`AcceptDecision::Reject`]. The only outcome that
    /// may be reported to the peer as a decline.
    Rejected,
    /// Nobody answered within [`ACCEPT_TIMEOUT`], or the sender dropped before
    /// a decision arrived. Not a decision at all — the file simply is not
    /// moving right now, and the sender is free to offer it again.
    Unanswered,
}

impl AcceptOutcome {
    /// Whether the bytes are cleared to move. Every non-accept outcome stops
    /// the transfer identically; only what is *reported* differs.
    fn accepted(self) -> bool {
        matches!(self, AcceptOutcome::Accepted)
    }
}

/// The sender's own path for a queued file, read off its chat row.
///
/// Deliberately **not** the staged blob's path, even though that is what the
/// transfer reads: the blob is deleted the moment the send settles, so a
/// history row or an "Open" pointing at it would dangle within seconds. What
/// the user wants to reopen is the file they picked.
fn sender_path(chat: &ChatStore, peer: &DeviceId, id: &str) -> Option<String> {
    chat.get(peer, id)
        .ok()
        .flatten()
        .and_then(|rec| rec.file)
        .and_then(|meta| meta.local_path)
}

/// How one send leg ended, for a caller with bookkeeping of its own to do
/// afterwards — the queued-file drain, which must decide between dequeueing,
/// deleting the staged blob, and counting a refusal against the backstop.
///
/// `Failed` carries the two facts that decide whether an attempt may be counted
/// as a refusal, because getting that wrong tells a user their peer refused a
/// file when it did not:
///
/// * **`bytes_moved > 0`** — the receiver definitely accepted.
///   `stream::send_file` sends `Meta`, waits for the receiver's
///   `Control::ResumeAck`, and only then sends chunks; the receiver reaches the
///   code that sends that ack only after its approval gate resolved in favour
///   of accepting (`handle_incoming` calls `receive_on_channel` on the accepted
///   branch alone). So any byte at all means this was a mid-stream fault:
///   retryable forever, never counted.
/// * **`local_fault`** — the failure was **ours**: a `DomainError::Storage`,
///   i.e. the staged blob unreadable or gone, a permissions error, or a failed
///   `hash_prefix` on a resumed leg. Every one of those happens *after*
///   `recv_resume_ack` returns (`send_file` does not open the source until the
///   ack is in), so they can present as zero bytes moved even though the
///   receiver said yes. Counting them would spend a refusal credit on our own
///   disk, and at the third would blame the peer for it in the user's history.
///
/// A zero-byte failure that is neither is what the backstop counts: the offer
/// reached the peer and died at its approval gate. One ambiguity remains and is
/// accepted — a link that drops in the single round trip between `ResumeAck`
/// and the first chunk is indistinguishable, without plumbing a flag out of the
/// transfer engine, from one that drops just before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegOutcome {
    Delivered,
    Cancelled,
    Failed { bytes_moved: u64, local_fault: bool },
}

/// How many offers may reach a peer and be refused (or time out) at its
/// approval gate before a queued file is given up on.
///
/// The backstop exists for peers too old to send a `FileDecline`: without it a
/// refused file is re-offered on every drain tick, re-prompting its receiver
/// forever. Three is enough that a single missed prompt — someone away from
/// their desk when the 180 s `ACCEPT_TIMEOUT` elapses — is recoverable, and
/// small enough that a genuine refusal stops being a nuisance quickly.
const MAX_OFFERS_REFUSED: u32 = 3;

/// Whether refusing the transfer `id` should put a `FileDecline` on the wire.
///
/// This is the enforcement point for "capability-advertised, not assumed"
/// (MESSAGE_REGISTRY.md §7 / I9), extracted from `handle_incoming` as a pure
/// function of data already in hand — no session, no network — so all three of
/// its legs are unit-testable and a refactor cannot delete one silently. It
/// mirrors [`caps_support_file_ref`](crate::session_exec) : the predicate is
/// the tested unit, the call site is the thin part.
///
/// All three must hold:
///
/// 1. **`outcome == Rejected`** — the user actually refused. A prompt that
///    timed out or a sender that dropped must fall through to the retry
///    backstop instead: see [`AcceptOutcome`] for why turning three minutes of
///    silence into a permanent cross-device "I declined" is the wrong trade.
/// 2. **The peer negotiated `CHAT_FEAT_FILEDECLINE`** — a 2a-era peer ANDed
///    the bit away and must never receive the message. It would skip an
///    unknown OPTIONAL type harmlessly, but sending a type the negotiation
///    says the peer does not speak is exactly the silent wire drift capability
///    negotiation exists to prevent; such a sender uses its own bounded
///    backstop instead.
/// 3. **The transfer has a row in this conversation** — only a file shared
///    *in chat* is declinable. The overwhelming majority of refusals are
///    ordinary transfers, which must put nothing extra on the wire.
///
/// Leg 3 is deliberately `contains`, not the settleable-row guard: by the time
/// this runs, `chat_settle` has already moved our row to `Declined`, so that
/// guard reads false by design. The authorization that matters runs on the
/// **receiving** side, where `ChatStore::settle_file_row` re-checks
/// kind/direction/in-flight against the sender's own stored record — a decline
/// is never trusted by the party acting on it.
fn should_send_decline(
    outcome: AcceptOutcome,
    caps: &CapabilitySet,
    chat: &ChatStore,
    peer: &DeviceId,
    id: &str,
) -> bool {
    outcome == AcceptOutcome::Rejected
        && crate::session_exec::caps_support_file_decline(caps)
        && chat.contains(peer, id).unwrap_or(false)
}

// ── manager ─────────────────────────────────────────────────────

pub struct Manager {
    rm: Arc<RouteManager>,
    quic: Arc<QuicTransport>,
    enc: Arc<AeadCrypto>,
    trust: Arc<FsTrust>,
    /// Encrypted local store for chat history, one namespace per peer. Shared
    /// (`Clone`) with `handle_incoming`'s per-connection chat wiring — the
    /// accept path registers a `ChatHandler` against a clone of this store, so
    /// every accepted session persists into the same on-disk conversation log.
    chat: ChatStore,
    /// The outbox's own copy of every file waiting to be sent. `Arc` because
    /// `StagingStore` is deliberately not `Clone` (it owns a directory), and
    /// the background send tasks need it alongside `chat`.
    staging: Arc<StagingStore>,
    /// The size cap and free-space floor a stage is held to, resolved from
    /// configuration once at construction so nothing below reads config.
    staging_limits: StagingLimits,
    /// Peers with a queued file transfer running right now, keyed by peer id —
    /// the one-file-in-flight guard. Queueing five videos must start one
    /// transfer, not five competing for the same link. Text is never gated by
    /// this: it rides CHAT while a file's bytes ride TRANSFER, so a message
    /// never waits behind bytes.
    chat_file_in_flight: Mutex<HashSet<String>>,
    identity: Identity,
    /// The presented name, split out from `identity` so a live rename
    /// (`set_identity_name`) reaches in-flight/future handshakes without a
    /// restart. `identity.name` itself is left stale; always read the name
    /// through [`Self::identity`].
    identity_name: RwLock<String>,
    /// Received-files directory. Interior-mutable so a live settings change
    /// (`set_save_dir`) reaches in-flight/future receives without a restart.
    save_dir: RwLock<String>,
    /// Approval policy. Interior-mutable so toggling auto-accept applies live.
    auto_accept: AtomicBool,
    chunk_size: u32,
    daemon_port: u16,
    active: Mutex<HashMap<String, Arc<Active>>>,
    pending: Mutex<HashMap<String, oneshot::Sender<AcceptDecision>>>,
    history: Mutex<Vec<Value>>,
    /// Where history persists across restarts (None = in-memory only, tests).
    history_path: Option<std::path::PathBuf>,
    counter: AtomicU64,
    daemon_task: Mutex<Option<JoinHandle<()>>>,
    daemon_running: AtomicBool,
}

type Op = Result<Value, (Code, String)>;

impl Manager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rm: Arc<RouteManager>,
        quic: Arc<QuicTransport>,
        enc: Arc<AeadCrypto>,
        trust: Arc<FsTrust>,
        chat: ChatStore,
        staging: Arc<StagingStore>,
        staging_limits: StagingLimits,
        identity: Identity,
        save_dir: String,
        auto_accept: bool,
        chunk_size: u32,
        daemon_port: u16,
        history_path: Option<std::path::PathBuf>,
    ) -> Self {
        let history = history_path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
            .unwrap_or_default();
        let identity_name = RwLock::new(identity.name.clone());
        Manager {
            rm,
            quic,
            enc,
            trust,
            chat,
            staging,
            staging_limits,
            chat_file_in_flight: Mutex::new(HashSet::new()),
            identity,
            identity_name,
            save_dir: RwLock::new(save_dir),
            auto_accept: AtomicBool::new(auto_accept),
            chunk_size: chunk_size.max(1),
            daemon_port,
            active: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            history: Mutex::new(history),
            history_path,
            counter: AtomicU64::new(0),
            daemon_task: Mutex::new(None),
            daemon_running: AtomicBool::new(false),
        }
    }

    // ── daemon (receive server) control ─────────────────────────

    pub fn active_len(&self) -> usize {
        self.active.lock().unwrap().len()
    }

    /// The current received-files directory (read fresh so a live change wins).
    fn save_dir(&self) -> String {
        self.save_dir.read().unwrap().clone()
    }

    /// Apply a new save directory live (persisted settings change; no restart).
    pub fn set_save_dir(&self, dir: String) {
        *self.save_dir.write().unwrap() = dir;
    }

    /// Apply the auto-accept policy live (persisted settings change; no restart).
    pub fn set_auto_accept(&self, v: bool) {
        self.auto_accept.store(v, Ordering::SeqCst);
    }

    /// The identity presented in handshakes: same device id + keypair as
    /// construction, but the name read fresh so a live rename applies to the
    /// very next handshake without a restart.
    fn identity(&self) -> Identity {
        Identity {
            device_id: self.identity.device_id.clone(),
            name: self.identity_name.read().unwrap().clone(),
            keypair: self.identity.keypair.clone(),
        }
    }

    /// Apply a new device name live (persisted settings change; no restart).
    pub fn set_identity_name(&self, name: String) {
        *self.identity_name.write().unwrap() = name;
    }

    /// The chat wiring every session registers, regardless of which side
    /// dialed. Chat frames get *pushed* over whichever session happens to
    /// exist between two peers, in either direction — `chat_send`'s
    /// opportunistic flush and `chat_flush_peer` push over a session *we*
    /// dialed; `handle_incoming`'s flush-on-connect pushes over a session the
    /// peer dialed into us. A session with no `ChatHandler` registered on a
    /// given side doesn't error on an inbound CHAT frame — the channel
    /// dispatch loop just silently drops it (see `peerbeam-transfer`'s
    /// channel actor: `if let Some(h) = &handler { h.handle(sf) }`, no
    /// `else`) — so every session, dial or accept, needs this wired or a
    /// pushed message is lost even though the sender marks it delivered.
    fn chat_wiring(&self) -> crate::session_exec::ChatWiring {
        crate::session_exec::ChatWiring {
            store: self.chat.clone(),
            sink: Arc::new(|rec| crate::events::chat(&rec)),
        }
    }

    pub fn daemon_status(&self) -> Value {
        json!({
            "running": self.daemon_running.load(Ordering::SeqCst),
            "port": self.daemon_port,
        })
    }

    /// Start the receive server if not already running (idempotent).
    pub fn start_daemon(self: &Arc<Self>) -> Op {
        if self.daemon_running.swap(true, Ordering::SeqCst) {
            return Ok(json!({ "running": true, "already_running": true }));
        }
        let me = self.clone();
        let port = self.daemon_port;
        let handle = crate::runtime::spawn_handle(async move { me.serve(port).await });
        *self.daemon_task.lock().unwrap() = Some(handle);
        daemon_event("daemon_started", self.daemon_port);
        Ok(json!({ "running": true }))
    }

    /// Stop the receive server (idempotent).
    pub fn stop_daemon(&self) -> Op {
        if let Some(handle) = self.daemon_task.lock().unwrap().take() {
            handle.abort();
        }
        self.mark_daemon_stopped();
        Ok(json!({ "running": false }))
    }

    /// Mark the receive daemon as not running and drop its task handle.
    /// Called both from `stop_daemon()` (explicit stop) and from `serve()`
    /// itself whenever it exits on its own — a bind failure, or the inbound
    /// stream ending — so `daemon_status()` never lies about a dead daemon
    /// still running, and `start_daemon()`'s guard doesn't permanently
    /// refuse to bring it back up. Idempotent.
    fn mark_daemon_stopped(&self) {
        self.daemon_running.store(false, Ordering::SeqCst);
        *self.daemon_task.lock().unwrap() = None;
        daemon_event("daemon_stopped", self.daemon_port);
    }

    /// Stop then start the receive server.
    pub fn restart_daemon(self: &Arc<Self>) -> Op {
        let _ = self.stop_daemon();
        let _ = self.start_daemon();
        daemon_event("daemon_restarted", self.daemon_port);
        Ok(json!({ "running": true }))
    }

    fn next_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        format!("tx-{}-{}", std::process::id(), n)
    }

    fn storage(&self) -> FsStorage {
        FsStorage::new()
    }

    fn session(&self, id: &str, peer: DeviceId, total: u64) -> TransferSession {
        TransferSession {
            id: TransferId::from(id),
            peer,
            direction: Direction::Sending,
            status: TransferStatus::Transferring,
            files: Vec::new(),
            total_bytes: total,
            transferred_bytes: 0,
            started_at: chrono::Utc::now(),
            completed_at: None,
            is_resume: false,
        }
    }

    // ── send ────────────────────────────────────────────────────

    /// Queue one or more files to a peer. Returns the assigned transfer ids;
    /// the actual work runs in the background and reports via events.
    ///
    /// An optional `transfer_id` in the request pins the id instead of minting
    /// one, so a caller that has already published that id out of band (the
    /// chat file-share, whose `FileRef` message id *is* the transfer id) can
    /// make the transfer and its chat row one identity. It applies to a single
    /// path only — one id cannot name several transfers — and is refused if it
    /// is already in use, rather than silently substituted: a caller asking for
    /// a specific id needs that exact id or an error, never a different one.
    pub fn send(self: &Arc<Self>, req: &Value) -> Op {
        let device = device_from(req.get("peer"))?;
        let requested_id = match req.get("transfer_id") {
            None | Some(Value::Null) => None,
            Some(v) => Some(valid_transfer_id(v)?),
        };
        let paths = req
            .get("paths")
            .and_then(|p| p.as_array())
            .ok_or((Code::InvalidArgument, "paths[] required".into()))?;
        if requested_id.is_some() && paths.len() != 1 {
            return Err((
                Code::InvalidArgument,
                "transfer_id applies to exactly one path".into(),
            ));
        }

        // Validate *every* path before registering or spawning anything, so a
        // bad entry can't leave some transfers already queued while the call
        // returns an error (the caller would never learn about the orphans).
        let mut validated: Vec<(String, String, u64)> = Vec::new();
        for p in paths {
            let path = p
                .as_str()
                .ok_or((Code::InvalidArgument, "path must be a string".into()))?;
            let sp = std::path::Path::new(path);
            if !sp.exists() {
                return Err((Code::Storage, format!("path not found: {path}")));
            }
            if sp.is_dir() {
                return Err((
                    Code::InvalidArgument,
                    format!("use send_folder for directories: {path}"),
                ));
            }
            let name = sp
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file.bin".into());
            let size = std::fs::metadata(sp).map(|m| m.len()).unwrap_or(0);
            validated.push((path.to_string(), name, size));
        }

        let mut ids = Vec::new();
        for (path, name, size) in validated {
            let (id, active) = match &requested_id {
                Some(rid) => {
                    let active = self
                        .register_vacant(
                            rid,
                            "sending",
                            &device.name,
                            &device.id.0,
                            &name,
                            Some(path.clone()),
                        )
                        .ok_or((
                            Code::InvalidArgument,
                            format!("transfer id already in use: {rid}"),
                        ))?;
                    (rid.clone(), active)
                }
                None => self.register_fresh(
                    "sending",
                    &device.name,
                    &device.id.0,
                    &name,
                    Some(path.clone()),
                ),
            };
            events::transfer(
                &id,
                "transfer_queued",
                json!({ "peer": device.name, "peer_id": device.id.0, "file": name }),
            );
            ids.push(id.clone());

            let mgr = self.clone();
            let device = device.clone();
            let active_handle = active.clone();
            let h = crate::runtime::spawn_handle(async move {
                mgr.run_send(id, active, device, path, name, size).await;
            });
            *active_handle.task.lock().unwrap() = Some(h);
        }
        Ok(json!({ "ids": ids }))
    }

    /// Queue a folder to a peer.
    pub fn send_folder(self: &Arc<Self>, req: &Value) -> Op {
        let device = device_from(req.get("peer"))?;
        let path = req
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or((Code::InvalidArgument, "path required".into()))?
            .to_string();
        let sp = std::path::Path::new(&path);
        if !sp.is_dir() {
            return Err((Code::InvalidArgument, format!("not a folder: {path}")));
        }
        let name = sp
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "folder".into());
        let (id, active) = self.register_fresh(
            "sending",
            &device.name,
            &device.id.0,
            &name,
            Some(path.clone()),
        );
        events::transfer(
            &id,
            "transfer_queued",
            json!({ "peer": device.name, "peer_id": device.id.0, "folder": name }),
        );

        let mgr = self.clone();
        let id2 = id.clone();
        let active_handle = active.clone();
        let h = crate::runtime::spawn_handle(async move {
            mgr.run_send_folder(id2, active, device, path).await;
        });
        *active_handle.task.lock().unwrap() = Some(h);
        Ok(json!({ "id": id }))
    }

    /// Register a transfer under `id` — but only if that id is not already
    /// claimed. Returns `None` when it is.
    ///
    /// A transfer id is the registry key, and the same key that
    /// `accept`/`reject`/`cancel` act on and that the terminal-event *claim*
    /// (`remove`) is taken against. Overwriting an entry would therefore splice
    /// two unrelated transfers together: the displaced one keeps running with
    /// no registry entry, its eventual terminal event removes — and reports
    /// against — the survivor's state, and a user `cancel` hits whichever of
    /// the two happens to be in the map.
    ///
    /// That used to be unreachable, because every id was minted locally by a
    /// monotonic counter. It is reachable now: an incoming transfer registers
    /// under the id the *sender* put on the wire, and a caller may supply one
    /// too. So the insert is a claim (`Entry`, under one lock acquisition — no
    /// check-then-insert window), and a collision is refused rather than
    /// merged. Callers fall back to a freshly minted id
    /// ([`register_fresh`](Self::register_fresh)) or report an error.
    ///
    /// **Scope of this guarantee:** it rules out only a *simultaneous*
    /// collision — two live transfers holding one id at the same instant. It
    /// says nothing about the same id being used again *later*, which a wire-
    /// supplied id makes ordinary rather than exotic: a cancelled chat file
    /// retried under its `FileRef` id re-uses that id by design. Terminal
    /// removal must therefore not trust the id alone — see
    /// [`claim`](Self::claim), which is what actually keeps one transfer's
    /// unwind from tearing out its successor.
    fn register_vacant(
        &self,
        id: &str,
        direction: &'static str,
        peer: &str,
        peer_id: &str,
        file: &str,
        path: Option<String>,
    ) -> Option<Arc<Active>> {
        let active = Arc::new(Active {
            id: id.to_string(),
            direction,
            peer: peer.to_string(),
            peer_id: peer_id.to_string(),
            ctrl: TransferControl::new(),
            stats: Arc::new(Mutex::new(Stats::new())),
            file: Arc::new(Mutex::new(file.to_string())),
            status: Mutex::new("queued".to_string()),
            path: Mutex::new(path),
            task: Mutex::new(None),
        });
        match self.active.lock().unwrap().entry(id.to_string()) {
            std::collections::hash_map::Entry::Occupied(_) => None,
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(active.clone());
                Some(active)
            }
        }
    }

    /// Register under a freshly minted id, skipping any id already claimed.
    ///
    /// The skip is not paranoia: a peer-supplied id now occupies the same
    /// keyspace as our own, so a peer could name a *future* `next_id()` value
    /// and lie in wait for it. `next_id()` is strictly increasing and the
    /// registry is finite, so this settles on a vacant id in at most as many
    /// steps as there are entries.
    fn register_fresh(
        &self,
        direction: &'static str,
        peer: &str,
        peer_id: &str,
        file: &str,
        path: Option<String>,
    ) -> (String, Arc<Active>) {
        loop {
            let id = self.next_id();
            if let Some(active) =
                self.register_vacant(&id, direction, peer, peer_id, file, path.clone())
            {
                return (id, active);
            }
        }
    }

    /// Atomically claim `mine` out of the registry: remove it, but **only if
    /// the entry currently registered under its id is this exact transfer**.
    /// Returns `None` — a no-op — otherwise.
    ///
    /// This is the terminal-event claim. Exactly one of {`cancel()`, the task's
    /// own unwind} may ever act on a given transfer, and the claim is what
    /// decides which. It used to be a bare `remove(id)`, which was sound only
    /// while ids were unique *for all time*: a locally minted `tx-<pid>-<n>` is
    /// never issued twice, so an id could only ever name the one transfer.
    ///
    /// A wire-supplied id is not unique for all time. Consider a receive that
    /// the user cancels: `cancel` frees the slot immediately, but the receive
    /// task keeps running (only the send paths populate [`Active::task`], so
    /// nothing aborts it) and unwinds later, through a `session.close()` whose
    /// graceful close can wait seconds on an unresponsive peer. If a *second*
    /// transfer registers under the same id inside that window — which the
    /// peer chooses, and which the file-in-chat design makes routine, since a
    /// retried file re-uses its `FileRef` id — then the first transfer's late
    /// `remove(id)` would tear out the second: it would vanish from the UI on
    /// a `transfer_cancelled` it never earned, become uncancellable ("no active
    /// transfer"), and finish silently with no completion event and no history
    /// row while still writing to disk. With two different peers, the terminal
    /// event would also carry the wrong `peer_id` and route to the wrong
    /// conversation.
    ///
    /// Comparing `Arc` identity instead of the id closes that: a stale claim
    /// finds its own `Arc` absent and correctly does nothing. The `get` and the
    /// `remove` share one lock guard, so the check and the removal are atomic.
    ///
    /// `cancel()` deliberately does **not** use this: it acts on whatever is
    /// registered under the id the user pointed at, which is the current
    /// transfer by definition.
    fn claim(&self, mine: &Arc<Active>) -> Option<Arc<Active>> {
        let mut active = self.active.lock().unwrap();
        match active.get(&mine.id) {
            Some(current) if Arc::ptr_eq(current, mine) => active.remove(&mine.id),
            _ => None,
        }
    }

    /// Establish an initiator PeerSession, retrying transient connection
    /// failures with a short backoff (Wi-Fi blips, a receiver mid-restart).
    /// Emits `transfer_retrying` per attempt; cancellation stops the retries.
    async fn open_send_retry(
        &self,
        id: &str,
        active: &Active,
        device: &Device,
        meta: &TransferSession,
    ) -> Result<crate::session_exec::Session, (Code, String)> {
        const BACKOFF: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];
        let mut attempt = 0;
        loop {
            match crate::session_exec::dial(
                &self.quic,
                &self.rm,
                device,
                meta,
                self.identity(),
                self.enc.clone(),
                self.trust.clone(),
                // Register chat wiring even for a plain file-transfer dial:
                // this session can still be the one a concurrent
                // `chat_send`/flush pushes a queued message over from the
                // other side, and without a handler that pushed frame is
                // silently dropped instead of persisted (see `chat_wiring`'s
                // doc comment).
                Some(self.chat_wiring()),
            )
            .await
            {
                Ok(s) => return Ok(s),
                Err(e) => {
                    let transient = matches!(e.0, Code::Connection);
                    if !transient || attempt >= BACKOFF.len() || active.ctrl.is_cancelled() {
                        return Err(e);
                    }
                    let delay = BACKOFF[attempt];
                    attempt += 1;
                    *active.status.lock().unwrap() = "retrying".into();
                    events::transfer(
                        id,
                        "transfer_retrying",
                        json!({
                            "peer_id": device.id.0,
                            "attempt": attempt,
                            "delay_ms": delay.as_millis() as u64,
                        }),
                    );
                    tokio::time::sleep(delay).await;
                    if active.ctrl.is_cancelled() {
                        return Err(e);
                    }
                    *active.status.lock().unwrap() = "connecting".into();
                }
            }
        }
    }

    async fn run_send(
        self: Arc<Self>,
        id: String,
        active: Arc<Active>,
        device: Device,
        path: String,
        name: String,
        size: u64,
    ) {
        *active.status.lock().unwrap() = "connecting".into();
        let meta = self.session(&id, device.id.clone(), size);

        let session = match self.open_send_retry(&id, &active, &device, &meta).await {
            Ok(s) => s,
            Err(e) => return self.finish_failed(&active, e),
        };
        self.run_send_on_session(session, &id, &active, &device, path, name, size)
            .await;
    }

    /// Send one file over an **already-established** session, then close that
    /// session and settle the transfer.
    ///
    /// Split out of [`run_send`](Self::run_send) so the chat file-share can
    /// reuse the identical body over a session it had to establish itself (it
    /// must inspect the negotiated capabilities and push a `FileRef` on the
    /// CHAT channel *before* any byte moves — see
    /// [`run_chat_file_send`](Self::run_chat_file_send)). Both callers get the
    /// same events, the same `drive` pump, the same close, and the same
    /// terminal handling, so the two paths cannot drift.
    ///
    /// Takes the session **by value** and closes it on the single exit below:
    /// there is no `?` and no early return in this function, so no path can
    /// leak it.
    ///
    /// Returns how the leg ended ([`LegOutcome`]) *after* the ordinary terminal
    /// handling has run, for the one caller that has further bookkeeping —
    /// [`run_queued_file`](Self::run_queued_file), which must tell a refusal at
    /// the peer's approval gate apart from a mid-stream fault. Every other
    /// caller ignores it; the settle, the events and the history row are
    /// unchanged and still happen here.
    #[allow(clippy::too_many_arguments)]
    async fn run_send_on_session(
        self: &Arc<Self>,
        session: crate::session_exec::Session,
        id: &str,
        active: &Arc<Active>,
        device: &Device,
        path: String,
        name: String,
        size: u64,
    ) -> LegOutcome {
        events::transfer(
            id,
            "transfer_started",
            json!({ "peer": device.name, "peer_id": device.id.0, "file": name }),
        );
        *active.status.lock().unwrap() = "transferring".into();

        let handle = session.handle.clone();
        let req = SendRequest {
            transfer_id: id.to_string(),
            name,
            path,
            size,
            chunk_size: self.chunk_size,
        };
        let storage = self.storage();
        let ctrl = active.ctrl.clone();
        let outcome = drive(
            id.to_string(),
            active.stats.clone(),
            active.file.clone(),
            active.ctrl.clone(),
            |ptx| async move {
                let r = send_file_on_session(&handle, &storage, req, &ctrl, &ptx, 3).await;
                drop(ptx);
                r
            },
            None,
            None,
        )
        .await;
        session.close().await;
        // Read the leg's shape before `finish` consumes the outcome. The byte
        // count is the transfer's own progress total, which on this (sending)
        // side is bytes handed to the wire — and no byte is ever handed to the
        // wire before the receiver's `ResumeAck`, i.e. before it accepted.
        let leg = match &outcome {
            Ok(TransferOutcome::Completed) => LegOutcome::Delivered,
            Ok(TransferOutcome::Cancelled) => LegOutcome::Cancelled,
            Err(e) => LegOutcome::Failed {
                bytes_moved: active.stats.lock().unwrap().transferred,
                // A storage error is ours, not the peer's: `send_file` does not
                // touch the source until after the receiver's `ResumeAck`, so
                // this can read as zero bytes while the peer accepted perfectly.
                local_fault: matches!(e, peerbeam_domain::error::DomainError::Storage(_)),
            },
        };
        self.finish(active, outcome);
        leg
    }

    async fn run_send_folder(
        self: Arc<Self>,
        id: String,
        active: Arc<Active>,
        device: Device,
        path: String,
    ) {
        *active.status.lock().unwrap() = "connecting".into();
        let meta = self.session(&id, device.id.clone(), 0);
        let session = match self.open_send_retry(&id, &active, &device, &meta).await {
            Ok(s) => s,
            Err(e) => return self.finish_failed(&active, e),
        };
        events::transfer(
            &id,
            "transfer_started",
            json!({ "peer": device.name, "peer_id": device.id.0 }),
        );
        *active.status.lock().unwrap() = "transferring".into();

        let handle = session.handle.clone();
        let req = FolderSendRequest {
            transfer_id: id.clone(),
            root_path: path,
            chunk_size: self.chunk_size,
        };
        let storage = self.storage();
        let ctrl = active.ctrl.clone();
        let outcome = drive(
            id.clone(),
            active.stats.clone(),
            active.file.clone(),
            active.ctrl.clone(),
            |ptx| async move {
                let r = send_folder_on_session(&handle, &storage, req, &ctrl, &ptx, 3).await;
                drop(ptx);
                r
            },
            None,
            None,
        )
        .await;
        session.close().await;
        self.finish(&active, outcome);
    }

    /// Terminal handling for `active`'s own transfer. Takes the `Arc<Active>`
    /// rather than an id string so the claim below is identity-checked — see
    /// [`claim`](Self::claim). The id is read from the entry itself
    /// (`Active::id`), so it can never disagree with the entry being claimed.
    fn finish(&self, active: &Arc<Active>, outcome: DResult<TransferOutcome>) {
        match outcome {
            Ok(TransferOutcome::Completed) => {
                self.record(active, true, "transfer_completed", json!({}));
            }
            Ok(TransferOutcome::Cancelled) => self.finish_cancelled(active),
            Err(e) => self.finish_failed(active, from_domain(e)),
        }
    }

    /// The task observed its own cancellation (it noticed `ctrl` between
    /// chunks) and unwound with `TransferOutcome::Cancelled` — most often
    /// because `cancel()` already removed the entry and emitted
    /// `transfer_cancelled` synchronously, racing ahead of the task's own
    /// unwind. [`claim`](Self::claim) settles that race: exactly one of
    /// {`cancel()`, this} ever gets `Some` back for a given transfer, so
    /// exactly one of them emits the terminal event — and because the claim is
    /// by identity, a late unwind can never emit against a *different*
    /// transfer that has since taken the same id. No history entry for a user
    /// cancel.
    fn finish_cancelled(&self, active: &Arc<Active>) {
        let Some(a) = self.claim(active) else {
            return;
        };
        *a.status.lock().unwrap() = "cancelled".into();
        events::transfer(&a.id, "transfer_cancelled", json!({ "peer_id": a.peer_id }));
        // `Status` has no `Cancelled` variant, so a cancelled chat file lands on
        // `Failed` with the reason attached — distinguishable from a network
        // failure by the reason, and far better than a row left `Transferring`
        // with no further event coming.
        self.chat_settle(&a, ChatStatus::Failed, Some("cancelled"));
    }

    /// Whether the chat record living at this transfer's id is genuinely **this
    /// transfer's own row**, and is still waiting on it.
    ///
    /// This is the authorization check for every write the transfer layer makes
    /// into the chat store. It is not a formality — without it, a transfer id
    /// is a write primitive aimed at an arbitrary conversation row:
    ///
    /// * an incoming transfer registers under the id the **peer** put on its
    ///   first frame (`handle_incoming` → `register_vacant(&preview.transfer_id,
    ///   …)`), and that id only has to pass `is_valid_transfer_id`;
    /// * chat message ids are wire fields the peer has already seen, and
    ///   `mint_id()` emits 29 ASCII alphanumerics, which pass that check
    ///   trivially.
    ///
    /// So an already-paired peer could open an ordinary, non-chat transfer
    /// whose `transfer_id` is the id of a message in *our* thread with them, and
    /// the terminal path would then stamp a status onto that unrelated row: our
    /// own outbound "sent" text becoming `Received`, or a file we `Declined`
    /// flipping to `Received` while keeping the old name and size — the
    /// conversation asserting we accepted something we refused. An honest peer
    /// reaches the same place by retrying a transfer under a settled id.
    ///
    /// `register_vacant` hardens the *active-transfer registry* against exactly
    /// this; the chat store is a second map that needs its own guard. Matching
    /// the object rather than the key is that guard. All three must hold:
    ///
    /// 1. **`kind == File`** — a text row is never a transfer's business;
    /// 2. **`direction` agrees with the transfer** — `Out` for a send, `In` for
    ///    a receive, so a peer cannot drive our outbound rows or vice versa;
    /// 3. **the row is still in flight** (`Transferring` | `PendingApproval`) —
    ///    a settled row is final, which makes every terminal write once-only and
    ///    kills the reopen-a-declined-file move.
    ///
    /// Verified against 2a's full state table: every legitimate transition
    /// starts from one of those two statuses. A sender's row is written
    /// `Transferring` by `prepare_file_send` and leaves it as `Sent`/`Failed`.
    /// A receiver's row is written `PendingApproval` by `ChatHandler`; it moves
    /// to `Transferring` the moment the bytes are cleared to start (see
    /// [`handle_incoming`](Self::handle_incoming), beside the `transfer_started`
    /// emit) and from either state leaves as `Received`/`Declined`/`Failed`.
    /// Both in-flight statuses stay writable, which is exactly what lets a
    /// receiving row pick up its landing metadata and its saved path *after*
    /// it has already gone `Transferring`.
    ///
    /// Failing the check is a silent no-op — no write, no event — which is also
    /// what keeps every ordinary transfer, the vast majority, from touching the
    /// chat store at all.
    fn is_settleable_chat_row(&self, a: &Active) -> bool {
        let peer = DeviceId::from(a.peer_id.clone());
        let Ok(Some(rec)) = self.chat.get(&peer, &a.id) else {
            return false; // no row here: an ordinary transfer, or an unreadable one
        };
        let expected = if a.direction == "sending" {
            ChatDirection::Out
        } else {
            ChatDirection::In
        };
        // The predicate itself — kind/direction/in-flight-status — lives once,
        // in `ChatRecord::is_settleable_file_row`, shared with the CLI's
        // `settle_received_chat_file`. This helper's own job is just the fetch
        // and the direction mapping around it.
        rec.is_settleable_file_row(expected)
    }

    /// Mirror a transfer's terminal outcome onto its chat row, when that row is
    /// really its own.
    ///
    /// A file shared inside a conversation is deliberately **one identity**: the
    /// `FileRef` message id *is* the transfer id, so the transfer's own terminal
    /// event is the only thing that knows how the row ended. This is the bridge
    /// — gated by [`is_settleable_chat_row`](Self::is_settleable_chat_row),
    /// which is where the real guarantee is documented: right conversation,
    /// right row, still in flight.
    fn chat_settle(&self, a: &Active, status: ChatStatus, error: Option<&str>) {
        if !self.is_settleable_chat_row(a) {
            return;
        }
        let peer = DeviceId::from(a.peer_id.clone());
        if let Err(e) = self.chat.set_status(&peer, &a.id, status) {
            tracing::warn!(error = %e, transfer_id = %a.id, "chat row status not persisted");
            return;
        }
        events::chat_status_detail(&a.peer_id, &a.id, chat_status_str(status), error);
    }

    /// Record where a received chat file landed, so its row can offer "Open".
    ///
    /// Runs under the same guard as [`chat_settle`](Self::chat_settle) — a
    /// path is as much a write as a status, and a peer must not be able to
    /// point an unrelated row at a file of its choosing.
    ///
    /// **Must be called before `chat_settle`**, not after: they share the
    /// in-flight leg of that guard, so once the row reads `Received` it is
    /// deliberately no longer writable and the path would be dropped.
    fn chat_set_local_path(&self, a: &Active, local_path: &str) {
        if !self.is_settleable_chat_row(a) {
            return;
        }
        let peer = DeviceId::from(a.peer_id.clone());
        if let Err(e) = self.chat.set_file_path(&peer, &a.id, local_path) {
            tracing::warn!(error = %e, transfer_id = %a.id, "chat row path not persisted");
        }
    }

    /// Reconcile a receiving chat row with what the **transfer** says is
    /// landing, rather than what the peer's `FileRef` claimed.
    ///
    /// The row's name and size arrive on the CHAT channel; the bytes arrive on
    /// a TRANSFER stream whose own `TransferMeta` decides what is written to
    /// disk. The only thing tying them together is the shared id — nothing
    /// forces them to agree. A paired peer can therefore offer
    /// `holiday.jpg · 180 KB` in the thread and stream `invoice-2026.pdf.exe`,
    /// leaving a bubble that describes one file directly above an Accept button
    /// while tap-to-open hands the OS another. This is what closes that.
    ///
    /// Called twice, with the same meaning both times — "the transfer says this
    /// is the file":
    ///
    /// * from [`handle_incoming`](Self::handle_incoming) with the peeked
    ///   `TransferPreview`, **before** the approval gate, so the user decides on
    ///   the real name and size (the moment that actually matters);
    /// * from [`record`](Self::record) with what genuinely landed, before the
    ///   settle, which also covers the case where the peek learned nothing.
    ///
    /// Runs under the same guard as [`chat_settle`](Self::chat_settle) — a name
    /// is as much a write as a status — and, like
    /// [`chat_set_local_path`](Self::chat_set_local_path), **must precede** the
    /// settle: a settled row is deliberately no longer writable.
    ///
    /// The approval *decision* is untouched: this only makes the metadata the
    /// decision is shown with accurate.
    fn chat_set_landing(&self, a: &Active, name: &str, size: u64) {
        if !self.is_settleable_chat_row(a) {
            return;
        }
        let peer = DeviceId::from(a.peer_id.clone());
        let expected = if a.direction == "sending" {
            ChatDirection::Out
        } else {
            ChatDirection::In
        };
        if let Err(e) = self
            .chat
            .set_file_row_landing(&peer, &a.id, expected, name, size)
        {
            tracing::warn!(error = %e, transfer_id = %a.id, "chat row landing not persisted");
        }
    }

    fn finish_failed(&self, active: &Arc<Active>, (code, msg): (Code, String)) {
        // Claim this exact transfer: only whoever successfully claims it emits
        // the terminal event. A concurrent `cancel()` may have already claimed
        // (and removed) it — in which case there is nothing left to fail here,
        // and whatever now holds the id belongs to someone else.
        let Some(a) = self.claim(active) else {
            return;
        };
        *a.status.lock().unwrap() = "failed".into();
        events::transfer(
            &a.id,
            "transfer_failed",
            json!({
                "peer_id": a.peer_id,
                "error": { "code": code.as_str(), "message": msg },
            }),
        );
        self.record_history(&a.id, &a, false);
        self.chat_settle(&a, ChatStatus::Failed, Some(&msg));
    }

    /// Success path: emit completed + append history.
    fn record(&self, active: &Arc<Active>, success: bool, event: &str, extra: Value) {
        // Same identity-checked claim as `finish_failed`: a concurrent
        // `cancel()` may have already removed this transfer, in which case
        // there is nothing left to record.
        let Some(a) = self.claim(active) else {
            return;
        };
        let id = &a.id;
        *a.status.lock().unwrap() = "completed".into();
        let file = a.file.lock().unwrap().clone();
        let path = a.path.lock().unwrap().clone().unwrap_or_else(|| {
            std::path::Path::new(&self.save_dir())
                .join(&file)
                .to_string_lossy()
                .into_owned()
        });
        let stats = a.stats.lock().unwrap().dto();
        let mut payload =
            json!({ "stats": stats, "file": file, "path": &path, "peer_id": a.peer_id });
        if let Value::Object(m) = &mut payload {
            if let Value::Object(e) = extra {
                m.extend(e);
            }
        }
        events::transfer(id, event, payload);
        self.record_history(id, &a, success);
        if success {
            // A completed chat file reads as `Sent` in our own thread when we
            // sent it and `Received` when we did not — the same two statuses a
            // text message uses, so a file row needs no special vocabulary.
            let sending = a.direction == "sending";
            if !sending {
                // A received file's row is reconciled with what ACTUALLY
                // landed — `a.file` is the name `receive_file` wrote (taken
                // from the stream's own `TransferMeta`, then sanitized), and
                // the byte count is the one the history row records, so the
                // three agree. Without this the row would keep describing the
                // peer's separate CHAT-channel `FileRef` claim forever, which
                // nothing has ever checked against the stream.
                let landed_bytes = a.stats.lock().unwrap().transferred;
                self.chat_set_landing(&a, &file, landed_bytes);
                // …and learns where it landed, so the bubble can offer "Open".
                // Strictly before the settle below: all three share the
                // in-flight guard, and settling closes the row to further
                // writes.
                self.chat_set_local_path(&a, &path);
            }
            let settled = if sending {
                ChatStatus::Sent
            } else {
                ChatStatus::Received
            };
            self.chat_settle(&a, settled, None);
        }
    }

    /// Append a history entry for an already-claimed (removed from `active`)
    /// transfer. Takes the `Active` directly rather than looking it up by id
    /// — by the time this runs the entry is no longer in the map.
    fn record_history(&self, id: &str, a: &Active, success: bool) {
        let entry = {
            let file = a.file.lock().unwrap().clone();
            // Local path of the item: explicit when known (sends, folder
            // receives); otherwise a received file's final location under the
            // save directory.
            let path = a.path.lock().unwrap().clone().unwrap_or_else(|| {
                std::path::Path::new(&self.save_dir())
                    .join(&file)
                    .to_string_lossy()
                    .into_owned()
            });
            json!({
                "id": id,
                "direction": a.direction,
                "peer": a.peer,
                "peer_id": a.peer_id,
                "file": file,
                "path": path,
                "bytes": a.stats.lock().unwrap().transferred,
                "success": success,
                "at": timestamp(),
            })
        };
        {
            let mut history = self.history.lock().unwrap();
            history.push(entry);
            // Bound growth: keep the most recent entries only.
            const MAX_HISTORY: usize = 500;
            if history.len() > MAX_HISTORY {
                let drop = history.len() - MAX_HISTORY;
                history.drain(..drop);
            }
            self.persist_history(&history);
        }
        events::event(&json!({ "type": "history_updated", "timestamp": timestamp() }));
    }

    /// Best-effort write of the history document (atomic-enough for a cache:
    /// history is convenience data, not integrity-critical).
    fn persist_history(&self, history: &[Value]) {
        let Some(path) = self.history_path.as_deref() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec(history) {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// Clear all history (persisted too) and notify.
    pub fn history_clear(&self) -> Op {
        {
            let mut history = self.history.lock().unwrap();
            history.clear();
            self.persist_history(&history);
        }
        events::event(&json!({ "type": "history_updated", "timestamp": timestamp() }));
        Ok(json!({ "cleared": true }))
    }

    // ── control ─────────────────────────────────────────────────

    pub fn pause(&self, id: &str) -> Op {
        let a = self.get_active(id)?;
        a.ctrl.pause();
        *a.status.lock().unwrap() = "paused".into();
        events::transfer(id, "transfer_paused", json!({ "peer_id": a.peer_id }));
        Ok(json!({ "paused": true }))
    }

    pub fn resume(&self, id: &str) -> Op {
        let a = self.get_active(id)?;
        a.ctrl.resume();
        // Re-anchor the rate baseline to now: without this the next progress
        // update measures `dt` across the whole pause, producing a near-zero
        // instantaneous speed and an inflated ETA (BUG 3).
        a.stats.lock().unwrap().mark_resumed();
        *a.status.lock().unwrap() = "transferring".into();
        events::transfer(id, "transfer_resumed", json!({ "peer_id": a.peer_id }));
        Ok(json!({ "resumed": true }))
    }

    pub fn cancel(&self, id: &str) -> Op {
        // Atomically claim the entry: `remove` is the same claim mechanic
        // `finish`/`finish_failed`/`record` use, so cancel() and a task's own
        // natural completion can never both emit a terminal event for the
        // same id — whichever removes it first is the sole emitter. Cancel
        // stays authoritative for the common case (it races well ahead of a
        // task that has to notice `ctrl` between chunks); the only way this
        // returns "not found" is a transfer that already reached a terminal
        // state on its own.
        let a = match self.active.lock().unwrap().remove(id) {
            Some(a) => a,
            None => return Err((Code::InvalidArgument, format!("no active transfer {id}"))),
        };
        a.ctrl.cancel();
        // Abort the running task so cancel is immediate even if a chunk send is
        // blocked on a slow link (the loop only checks `ctrl` between chunks).
        if let Some(h) = a.task.lock().unwrap().take() {
            h.abort();
        }
        // If it is still awaiting accept/reject, the receive task is parked on
        // the approval channel and never checks `ctrl`. Fire the pending sender
        // with `false` so it unblocks and cleans up.
        if let Some(tx) = self.pending.lock().unwrap().remove(id) {
            let _ = tx.send(AcceptDecision::Reject);
        }
        // An aborted task won't run finish(); do the cleanup + notify here.
        *a.status.lock().unwrap() = "cancelled".into();
        events::transfer(id, "transfer_cancelled", json!({ "peer_id": a.peer_id }));
        // A cancelled chat file is `Failed`, not left `Transferring`: the task
        // was aborted, so no terminal event is coming to settle the row, and a
        // row that spins forever is worse than one that says it did not arrive.
        self.chat_settle(&a, ChatStatus::Failed, Some("cancelled"));
        Ok(json!({ "cancelled": true }))
    }

    /// Accept an incoming transfer this one time only. Does not trust the
    /// sending device — the next incoming transfer from it still needs a
    /// decision. See [`accept_trust`](Self::accept_trust) to also trust it.
    pub fn accept(&self, id: &str) -> Op {
        match self.pending.lock().unwrap().remove(id) {
            // The receiver may already have timed out (`ACCEPT_TIMEOUT`) and
            // dropped its end of the channel in the moment between us
            // removing the entry and sending on it — `send` returning `Err`
            // means the decision landed too late to matter, so report
            // not-found rather than a success the caller already acted past.
            Some(tx) => match tx.send(AcceptDecision::AcceptOnce) {
                Ok(()) => Ok(json!({ "accepted": true })),
                Err(_) => Err((Code::InvalidArgument, format!("no pending transfer {id}"))),
            },
            None => Err((Code::InvalidArgument, format!("no pending transfer {id}"))),
        }
    }

    /// Accept an incoming transfer AND trust the sending device: future
    /// transfers from it are auto-accepted whenever auto-accept is enabled.
    /// The only path that ever approves a device — a plain [`accept`](Self::accept)
    /// never does.
    pub fn accept_trust(&self, id: &str) -> Op {
        match self.pending.lock().unwrap().remove(id) {
            // Same rationale as `accept`: a failed send means the timeout
            // already declined this transfer out from under us.
            Some(tx) => match tx.send(AcceptDecision::AcceptAndTrust) {
                Ok(()) => Ok(json!({ "accepted": true })),
                Err(_) => Err((Code::InvalidArgument, format!("no pending transfer {id}"))),
            },
            None => Err((Code::InvalidArgument, format!("no pending transfer {id}"))),
        }
    }

    pub fn reject(&self, id: &str) -> Op {
        match self.pending.lock().unwrap().remove(id) {
            // Same rationale as `accept`: a failed send means the timeout
            // already declined this transfer out from under us.
            Some(tx) => match tx.send(AcceptDecision::Reject) {
                Ok(()) => Ok(json!({ "rejected": true })),
                Err(_) => Err((Code::InvalidArgument, format!("no pending transfer {id}"))),
            },
            None => Err((Code::InvalidArgument, format!("no pending transfer {id}"))),
        }
    }

    fn get_active(&self, id: &str) -> Result<Arc<Active>, (Code, String)> {
        self.active
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or((Code::InvalidArgument, format!("no active transfer {id}")))
    }

    // ── state ───────────────────────────────────────────────────

    pub fn active_list(&self) -> Op {
        let list: Vec<Value> = self
            .active
            .lock()
            .unwrap()
            .values()
            .map(|a| a.dto())
            .collect();
        Ok(json!({ "transfers": list }))
    }

    pub fn get(&self, id: &str) -> Op {
        match self.active.lock().unwrap().get(id) {
            Some(a) => Ok(json!({ "transfer": a.dto() })),
            None => Err((Code::InvalidArgument, format!("no transfer {id}"))),
        }
    }

    pub fn history(&self) -> Op {
        Ok(json!({ "history": *self.history.lock().unwrap() }))
    }

    /// Pinned (trusted) devices, newest first.
    pub fn trust_list(&self) -> Op {
        let devices: Vec<Value> = self
            .trust
            .list()
            .into_iter()
            .map(|r| {
                json!({
                    "id": r.device.0,
                    "name": r.name,
                    "fingerprint": r.fingerprint,
                    "trusted_at": r.trusted_at.to_rfc3339(),
                })
            })
            .collect();
        Ok(json!({ "devices": devices }))
    }

    /// Revoke a pinned device; its next connection needs fresh approval.
    pub fn trust_remove(&self, req: &Value) -> Op {
        let id = req
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "id required".into()))?;
        let removed = self
            .trust
            .remove(&DeviceId::from(id))
            .map_err(from_domain)?;
        events::event(&json!({ "type": "trust_changed", "timestamp": timestamp() }));
        Ok(json!({ "removed": removed }))
    }

    // ── chat ────────────────────────────────────────────────────

    /// Queue a chat message to a peer and return immediately: persists it as
    /// `Pending` and enqueues it to the outbox synchronously (so the id is
    /// durable and visible in `chat_history` before this returns), then spawns
    /// a best-effort opportunistic flush in the background. Delivery — and
    /// the resulting `chat_status: "sent"` event — happens asynchronously via
    /// [`Self::chat_flush_peer`]; if the peer is unreachable right now the
    /// message simply stays queued for a later flush (a drain tick, or the
    /// next flush-on-connect).
    pub fn chat_send(self: &Arc<Self>, req: &Value) -> Op {
        let device = device_from(req.get("peer"))?;
        let text = req
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "text required".into()))?;
        let msg = peerbeam_chat::ChatMessage::new(text)
            .map_err(|e| (Code::InvalidArgument, e.to_string()))?;
        // Persist Pending + enqueue immediately; return without waiting on the network.
        self.chat
            .enqueue(&device.id, &msg)
            .map_err(|e| (Code::Internal, e.to_string()))?;
        let id = msg.id.clone();
        // Opportunistic delivery in the background (best-effort; a later
        // drain/flush-on-connect covers the rest if this peer is unreachable
        // right now).
        let me = self.clone();
        crate::runtime::spawn(async move {
            let _ = me.chat_flush_peer(device).await;
        });
        Ok(json!({ "id": id }))
    }

    /// Share a file inside a conversation with `peer`: `{peer, path}` → `{id}`.
    ///
    /// The returned id is one identity for two things — the `FileRef` message
    /// that puts a row in the thread, and the transfer that carries the bytes.
    /// That is the whole design: the row and the transfer are the same object
    /// seen from two channels, so progress, cancellation and history all line
    /// up without a second correlation table.
    ///
    /// Validates and persists the outgoing row **synchronously**, before this
    /// returns and before anything is dialed, so the caller's id is durable and
    /// visible in `chat_history` immediately and a later network failure can
    /// never leave a half-sent file with no row to settle. The network half runs
    /// in the background ([`run_chat_file_send`](Self::run_chat_file_send)) and
    /// reports through `chat_status` + the ordinary `transfer_*` events.
    ///
    /// **The send path is uniform (increment 2b): stage → enqueue → drain now
    /// if the peer is reachable.** There is no online/offline fork. A peer that
    /// cannot be reached right now simply leaves the file queued, exactly like
    /// text, and the online case is that queue draining without delay.
    ///
    /// That is the riskiest choice in this increment and it is deliberate: two
    /// paths would mean two sets of terminal-state handling, and terminal states
    /// were already the source of this feature's hardest defects. One path
    /// cannot drift from itself.
    ///
    /// **Why no `active` claim here any more.** 2a claimed the transfer id
    /// synchronously to protect the dial window: the row was written
    /// `Transferring` before anything was dialed, and
    /// [`chat_reconcile`](Self::chat_reconcile) settles a `Transferring` row
    /// with no entry in `active` as `Interrupted` — a status outside the
    /// writable set, so the real completion was afterwards dropped. Under 2b the
    /// row is written `Staging` and then `Pending`, and **neither is in the set
    /// `chat_reconcile` touches at all**, so the entire stage-and-queue window
    /// is immune by construction rather than by a claim. The id is claimed at
    /// the moment it stops being immune — in
    /// [`run_queued_file`](Self::run_queued_file), before that row is moved to
    /// `Transferring`.
    pub fn chat_send_file(self: &Arc<Self>, req: &Value) -> Op {
        let device = device_from(req.get("peer"))?;
        let path = req
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "path required".into()))?
            .to_string();
        // Validate + persist the outgoing row synchronously, so the caller's id
        // is durable and visible in `chat_history` before this returns and a
        // refused path (missing, a directory, a bad name) leaves no row at all.
        // The copy itself cannot happen here: it can run for minutes on a
        // multi-GB file, and this call must not block a UI thread.
        let file_ref = peerbeam_chat::begin_file_send(&self.chat, &device.id, &path)
            .map_err(|e| (Code::InvalidArgument, e.to_string()))?;
        let id = file_ref.id.clone();
        // The row is `Staging` now: say so, so a surface can show it rather
        // than an attach that appears to hang.
        events::chat_status(&device.id.0, &id, chat_status_str(ChatStatus::Staging));
        let me = self.clone();
        crate::runtime::spawn(async move { me.run_chat_file_send(device, path, file_ref).await });
        Ok(json!({ "id": id }))
    }

    /// The slow half of [`chat_send_file`](Self::chat_send_file): copy the file
    /// into the outbox's own storage, queue it, then try to deliver it right
    /// now.
    ///
    /// Staging is what makes a queued file honest. Between queueing and delivery
    /// the user may delete, move, rename or rewrite what they picked, and a
    /// queue that silently sends different bytes than the ones chosen is worse
    /// than one that fails. It also *deletes* the source-changed problem class
    /// rather than detecting it: no mtime comparison, no "the file you queued is
    /// not the file we sent", no Android content-URI instability.
    ///
    /// The copy streams (I10) — its whole memory footprint is one 64 KiB buffer
    /// whatever the file's size — and reports progress as it goes, so a
    /// multi-GB attach shows work rather than looking hung.
    ///
    /// A staging failure is immediate failure: nothing is queued, the row says
    /// `Failed` with the reason, and the user learns now instead of waiting for
    /// a delivery that was never scheduled.
    async fn run_chat_file_send(self: Arc<Self>, device: Device, path: String, file_ref: FileRef) {
        let peer_id = device.id.0.clone();
        let id = file_ref.id.clone();
        let mut file_ref = file_ref;
        let ctrl = TransferControl::new();
        let (ptx, mut prx) = mpsc::unbounded_channel::<u64>();

        // Drain the progress channel alongside the copy rather than after it:
        // one report per 64 KiB is ~262k reports for a 16 GiB file, and an
        // unbounded channel nobody reads would hold every one of them.
        // Task 8 turns these into a determinate bar; here they are consumed so
        // the queue stays bounded.
        let stage = async {
            let r = peerbeam_chat::stage_file_send(
                &self.chat,
                &self.staging,
                &device.id,
                &mut file_ref,
                &path,
                self.staging_limits,
                &ctrl,
                &ptx,
            )
            .await;
            drop(ptx);
            r
        };
        let pump = async { while prx.recv().await.is_some() {} };
        let (staged, ()) = tokio::join!(stage, pump);
        if let Err(e) = staged {
            return self.fail_chat_file(&peer_id, &id, &e.to_string());
        }

        // Queued. The online case is this queue draining without delay — and if
        // the peer is unreachable the file simply waits, exactly like text.
        let _ = self.chat_flush_peer(device).await;
    }

    /// Deliver one queued file to `peer` over an already-established session,
    /// then do the queue's own bookkeeping.
    ///
    /// This is 2a's proven online send path with the queue wrapped around it:
    /// the capability gate, the `FileRef` on CHAT before any byte moves, and
    /// then the identical `run_send_on_session` body — same events, same close,
    /// same terminal handling through `finish`.
    ///
    /// What is new is what happens *after*, in
    /// [`settle_queued_file`](Self::settle_queued_file), and one ordering
    /// detail: the `active` claim and the row's move to `Transferring` are taken
    /// together, immediately before the transfer starts. `chat_reconcile`
    /// settles a `Transferring` row with no `active` entry as `Interrupted`, so
    /// the row must not enter that state before its transfer is registered.
    ///
    /// **No silent fallback to a plain transfer.** A peer that never negotiated
    /// `CHAT_FEAT_FILEREF` has no `FileRef` handler: it would show the file as
    /// an ordinary incoming transfer with no row in any conversation, while our
    /// user was told the attachment landed in a thread the peer cannot see. So
    /// the refusal is announced on the row and **no transfer is started** — but
    /// the entry stays queued and nothing is counted against the backstop,
    /// because nobody was ever prompted and the peer may yet upgrade.
    async fn run_queued_file(
        self: Arc<Self>,
        session: crate::session_exec::Session,
        device: Device,
        peer: DeviceId,
        pending: PendingFile,
    ) {
        let id = pending.entry.message_id.clone();
        let staged = pending.file;
        let name = staged.name.clone();
        let size = staged.size;

        // The bytes must still be there before anyone is offered them. Without
        // this check a blob that has gone (app data wiped, an over-eager sweep,
        // a full disk) is still offered: the peer is prompted, accepts, and
        // *then* the send fails on our own `open_read` — three times over, each
        // one a pointless prompt for a file that can never arrive.
        //
        // SKIP, never delete. `Path::exists` is false for a transient
        // permissions or I/O error too, and deleting on that would throw away a
        // perfectly good queue entry and the user's only copy of the bytes. A
        // transient failure costs one drain tick; a permanent one costs a dial
        // per tick, which is visible in the log rather than silent.
        if !std::path::Path::new(&staged.staged_path).exists() {
            session.close().await;
            self.release_file_slot(&peer.0);
            tracing::warn!(
                message_id = %id,
                staged_path = %staged.staged_path,
                "queued file skipped: its staged bytes are not readable"
            );
            return;
        }

        // The gate. Capture the answer, then close on the refusal path — the
        // exact bug this ordering exists to prevent is a `?`-style early return
        // that skips `session.close()`.
        if !session.supports_file_ref() {
            session.close().await;
            self.release_file_slot(&peer.0);
            return self.fail_chat_file(
                &peer.0,
                &id,
                &format!(
                    "{} cannot receive chat attachments — its build predates \
                     file sharing in chat. Send {name} as a plain transfer instead.",
                    device.name
                ),
            );
        }

        // The row in the peer's thread. Sent before the bytes so the file has
        // somewhere to land: the receiver's `FileRef` handler persists a
        // `PendingApproval` row keyed by this same id, which the incoming
        // transfer then settles. Its `size` is the staged blob's, so the
        // approval prompt and the bytes that follow describe one file.
        let file_ref = FileRef {
            id: id.clone(),
            timestamp: pending.entry.timestamp.clone(),
            name: name.clone(),
            size,
        };
        if let Err(e) = peerbeam_chat::send_file_ref(&session.handle, &file_ref).await {
            session.close().await;
            self.release_file_slot(&peer.0);
            return self.fail_chat_file(&peer.0, &id, &format!("could not offer {name}: {e}"));
        }

        // Claim the id, then move the row in flight — in that order, so the row
        // is never `Transferring` with nothing registered under it.
        let Some(active) = self.register_vacant(
            &id,
            "sending",
            &device.name,
            &peer.0,
            &name,
            // The user's OWN path, read off the row — never the staged blob's,
            // which is deleted the moment this settles, so a history row
            // pointing at it would dangle.
            sender_path(&self.chat, &peer, &id),
        ) else {
            // Somebody else holds this id right now. Leave the entry queued and
            // try again on the next drain rather than splicing this share onto
            // an unrelated transfer.
            session.close().await;
            self.release_file_slot(&peer.0);
            tracing::warn!(message_id = %id, "queued file deferred: transfer id in use");
            return;
        };
        // The row must actually be in flight before the bytes are, and a
        // failure here is terminal FOR THIS ATTEMPT rather than something to
        // warn about and carry on through.
        //
        // Carrying on is the trap: the row would not be `Transferring`, so
        // `chat_settle`'s guard would silently drop the terminal write while
        // the transfer ran anyway — the file ARRIVES, and the sender's row is
        // stuck forever with the queue entry already dequeued, so nothing is
        // left to retry it. Reachable without any exotic failure: a peer
        // declines, `settle_queued_file`'s read of the row fails transiently so
        // the entry stays queued, and the next drain finds a `Declined` row
        // that `reopen_for_retry` (correctly) refuses.
        match self.chat.reopen_for_retry(&peer, &id) {
            Ok(true) => {}
            other => {
                let _ = self.claim(&active); // release the id we just claimed
                session.close().await;
                self.release_file_slot(&peer.0);
                if matches!(other, Ok(false)) && !self.row_may_still_deliver(&peer, &id) {
                    // Settled (`Sent`/`Declined`) or gone: no future drain can
                    // re-open it either, so stop retrying and let go of the
                    // bytes instead of re-dialing this peer forever.
                    self.drop_queued_file(&id, &staged.staged_path);
                }
                tracing::warn!(
                    result = ?other,
                    message_id = %id,
                    "queued file not offered: its row would not re-open"
                );
                return;
            }
        }

        events::transfer(
            &id,
            "transfer_queued",
            json!({ "peer": device.name, "peer_id": peer.0, "file": name, "size": size }),
        );
        // From here the ordinary send path owns it: same events, same close,
        // same terminal handling — and `finish` settles the row through
        // `chat_settle`, so success and failure both land on the record. The
        // bytes come from the STAGED BLOB, never the user's original.
        let leg = self
            .run_send_on_session(
                session,
                &id,
                &active,
                &device,
                staged.staged_path.clone(),
                name,
                size,
            )
            .await;
        self.settle_queued_file(&peer, &pending.entry.message_id, &staged.staged_path, leg);
        self.release_file_slot(&peer.0);
    }

    /// The queue's terminal decision for one delivery attempt — where the
    /// keep-forever promise and the bounded backstop are actually implemented.
    ///
    /// | outcome | queue | staged blob |
    /// | --- | --- | --- |
    /// | delivered | dequeued | deleted |
    /// | cancelled by the user | dequeued | deleted |
    /// | the peer declined (a `FileDecline` settled our row mid-flight) | dequeued | deleted |
    /// | failed after bytes moved (mid-stream fault) | left queued, nothing counted | kept |
    /// | failed on **our own** storage (`local_fault`) | left queued, nothing counted | kept |
    /// | failed with no bytes and no local fault (refused or unanswered at the approval gate) | counted; terminal at [`MAX_OFFERS_REFUSED`] | kept until terminal |
    ///
    /// **A connection failure never reaches this function at all** — it fails
    /// before a session exists, so nothing is counted and the file waits
    /// forever, exactly like text. That asymmetry is the point: counting
    /// attempts rather than refusals would burn the budget during a flapping
    /// link and drop a file nobody ever declined.
    ///
    /// The declined case is read off the row rather than plumbed through,
    /// because that is where it is already recorded: a `FileDecline` can only
    /// settle a row the settle guard admits, i.e. one that is still in flight,
    /// which is precisely the window this leg was running in. Re-reading after
    /// the leg therefore sees it, and the settle guard stays untouched.
    fn settle_queued_file(&self, peer: &DeviceId, id: &str, staged_path: &str, leg: LegOutcome) {
        let declined = matches!(
            self.chat.get(peer, id),
            Ok(Some(rec)) if rec.status == ChatStatus::Declined
        );
        let terminal = if declined {
            true
        } else {
            match leg {
                LegOutcome::Delivered | LegOutcome::Cancelled => true,
                // The receiver accepted and something failed afterwards: an
                // ordinary fault, retryable forever, never counted as a refusal.
                LegOutcome::Failed { bytes_moved, .. } if bytes_moved > 0 => false,
                // Our own storage failed. `send_file` opens the source only
                // after the receiver's `ResumeAck`, so this reads as zero bytes
                // while the peer may have accepted — counting it would spend a
                // refusal credit on our disk and end by blaming the peer.
                LegOutcome::Failed {
                    local_fault: true, ..
                } => false,
                LegOutcome::Failed { .. } => {
                    // The offer reached the peer and was refused, or nobody
                    // answered. Count it, and give up once the budget is spent.
                    let count = self.chat.outbox_bump_refused(id).unwrap_or(0);
                    if count >= MAX_OFFERS_REFUSED {
                        // Deliberately not "{peer} did not accept this file":
                        // zero bytes at the gate is the *likeliest* reading, but
                        // a link that dropped in the round trip after the
                        // receiver's ack looks identical from here, and a
                        // history row must not assert a refusal it cannot know.
                        self.fail_chat_file(
                            &peer.0,
                            id,
                            &format!(
                                "could not be delivered to {} after {count} attempts — \
                                 it will not be offered again",
                                peer.0
                            ),
                        );
                        true
                    } else {
                        false
                    }
                }
            }
        };
        if !terminal {
            return;
        }
        self.drop_queued_file(id, staged_path);
    }

    /// Whether a future drain could still deliver the row at `(peer, id)`.
    ///
    /// `Sent` and `Declined` are final, and a row that is not there at all can
    /// never be settled, so a queue entry pointing at either will never deliver
    /// however often it is retried — those are the only two answers that
    /// justify discarding the entry. Everything else may still become
    /// deliverable, including a stale `Transferring` that a restart's reconcile
    /// has not reached yet; and an *unreadable* store answers "yes", so a
    /// transient read error never throws a queue entry away.
    fn row_may_still_deliver(&self, peer: &DeviceId, id: &str) -> bool {
        match self.chat.get(peer, id) {
            Ok(Some(rec)) => !matches!(rec.status, ChatStatus::Sent | ChatStatus::Declined),
            Ok(None) => false,
            Err(_) => true,
        }
    }

    /// Let go of a queue entry and the bytes it owned. Split out of
    /// [`settle_queued_file`](Self::settle_queued_file) so the other place that
    /// must stop retrying — a row that will never re-open — releases exactly the
    /// same two things, in the same order.
    fn drop_queued_file(&self, id: &str, staged_path: &str) {
        if let Err(e) = self.chat.outbox_remove(id) {
            tracing::warn!(error = %e, message_id = %id, "queued file not dequeued");
        }
        self.staging.remove(staged_path);
    }

    /// Take this peer's one file-in-flight slot, if it is free.
    ///
    /// The guard is per peer, not global: two peers may receive files at once,
    /// but queueing five videos for one peer must start one transfer, not five
    /// competing for the same link. Text is never gated by it — a message rides
    /// CHAT while a file's bytes ride TRANSFER, so it never waits behind bytes.
    fn claim_file_slot(&self, peer_id: &str) -> bool {
        self.chat_file_in_flight
            .lock()
            .unwrap()
            .insert(peer_id.to_string())
    }

    /// Release this peer's file-in-flight slot. Called on **every** terminal
    /// path of [`run_queued_file`](Self::run_queued_file), including the ones
    /// that never start a transfer: a leaked slot would stop that peer's queue
    /// from ever draining again for the life of the process.
    fn release_file_slot(&self, peer_id: &str) {
        self.chat_file_in_flight.lock().unwrap().remove(peer_id);
    }

    /// Settle a chat file-share that never reached the transfer stage: mark the
    /// row `Failed` and tell every surface why.
    ///
    /// Used only where the ordinary terminal paths (`finish`/`finish_failed`)
    /// do not own the row — i.e. before any bytes moved, or once the backstop
    /// gives up on a file the peer keeps refusing. Writes directly rather than
    /// going through [`chat_settle`](Self::chat_settle)'s guard: that guard
    /// admits only an in-flight row, and every caller here is precisely the case
    /// where the row is not in flight. It is reachable **only** from local,
    /// sender-initiated paths — no peer input can drive it — which is the same
    /// argument that lets `ChatStore::reopen_for_retry` exist without relaxing
    /// the guard.
    ///
    /// A `Failed` row is not necessarily the end of the queue entry: a peer that
    /// merely cannot receive attachments today keeps its file queued (it may
    /// upgrade), and the next drain re-opens the row through `reopen_for_retry`.
    /// What is terminal is decided in
    /// [`settle_queued_file`](Self::settle_queued_file), not here.
    fn fail_chat_file(&self, peer_id: &str, id: &str, reason: &str) {
        let peer = DeviceId::from(peer_id.to_string());
        if let Err(e) = self.chat.set_status(&peer, id, ChatStatus::Failed) {
            tracing::warn!(error = %e, message_id = %id, "chat file row not marked failed");
        }
        tracing::warn!(peer_id = %peer_id, message_id = %id, reason = %reason, "chat file send failed");
        events::chat_status_detail(
            peer_id,
            id,
            chat_status_str(ChatStatus::Failed),
            Some(reason),
        );
    }

    /// Distinct peers that currently have queued (undelivered) messages.
    /// Consumed by the background chat drain (`runtime::chat_drain_loop`),
    /// which retries delivery to any of these peers once discovery reports
    /// them reachable — in addition to `chat_send`'s opportunistic flush and
    /// `handle_incoming`'s flush-on-connect.
    pub fn chat_outbox_peers(&self) -> Vec<DeviceId> {
        self.chat.outbox_peers().unwrap_or_default()
    }

    /// Dial `device` and drain its queue over a fresh session, emitting
    /// `chat_status: "sent"` for each message delivered. Best-effort: an
    /// unreachable peer leaves its queue intact for a later attempt (the next
    /// `chat_send`'s opportunistic flush, a periodic drain, or the peer's own
    /// next flush-on-connect). Returns the flushed message ids.
    ///
    /// Two halves, deliberately asymmetric:
    ///
    /// * **Text and declines** go out here, synchronously, over one CHAT
    ///   channel, and this call does not return until they have — that is what
    ///   makes the returned ids mean "delivered".
    /// * **A file** is *started*, not awaited. Its bytes ride the TRANSFER
    ///   stream channel in a task that takes ownership of the session, so a
    ///   4 GB upload cannot hold up this peer's next message, the drain loop's
    ///   other peers, or the caller. At most **one** file per peer is started
    ///   per flush, and only if that peer has no file transfer running already.
    ///
    /// A connection failure here is not a refusal: nothing is counted against
    /// the backstop, because nobody was offered anything.
    pub async fn chat_flush_peer(self: &Arc<Self>, device: Device) -> Vec<String> {
        let meta = self.session(&format!("chat-{}", device.id.0), device.id.clone(), 0);
        let session = match crate::session_exec::dial(
            &self.quic,
            &self.rm,
            &device,
            &meta,
            self.identity(),
            self.enc.clone(),
            self.trust.clone(),
            // Same rationale as `open_send_retry`: this dialed session must
            // be able to receive a CHAT frame the peer pushes back (e.g. its
            // own flush-on-connect), not just carry ours out.
            Some(self.chat_wiring()),
        )
        .await
        {
            Ok(s) => s,
            Err(_) => return Vec::new(), // unreachable; stays queued
        };
        // The authenticated peer, not the pre-dial `device.id` — mirrors the
        // same rationale documented on the old `chat_send`: the outbox/history
        // are namespaced by the authenticated identity.
        let peer = session.peer_device.clone();
        let flushed = peerbeam_chat::flush_to_session(&session.handle, &self.chat, &peer)
            .await
            .unwrap_or_default();

        // The file half. `next_file_for` decides *which* (the oldest queued);
        // `claim_file_slot` decides *whether* (one per peer at a time). Only if
        // both say yes does the session survive this call — the send task owns
        // and closes it from here.
        match peerbeam_chat::next_file_for(&self.chat, &peer).unwrap_or(None) {
            Some(pending) if self.claim_file_slot(&peer.0) => {
                let me = self.clone();
                let peer_for_task = peer.clone();
                crate::runtime::spawn(async move {
                    me.run_queued_file(session, device, peer_for_task, pending)
                        .await;
                });
            }
            _ => session.close().await,
        }

        for mid in &flushed {
            events::chat_status(&peer.0, mid, "sent");
        }
        flushed
    }

    /// Conversation history with one peer, chronological (oldest first).
    /// Settle one conversation's rows that nothing will ever finish:
    /// `{peer_id}` → `{changed}`.
    ///
    /// A file row is written `Transferring` (sender) or `PendingApproval`
    /// (receiver) and is only ever moved off that state by a live transfer
    /// event. Transfer ids are process-scoped and nothing replays them, so a
    /// row that survives a restart in either state spins forever: the sender's
    /// bubble shows an eternal progress bar, and the receiver's keeps offering
    /// an Accept button whose transfer no longer exists.
    ///
    /// Startup reconciliation ([`crate::runtime`]'s `reconcile_chat`) now
    /// reaches every conversation — it enumerates `ChatStore::conversations`,
    /// so a thread whose only unsettled row is a file is settled at boot like
    /// any other. This remains the entry point for everything a *running*
    /// process leaves behind: a row stranded after the restart it would have
    /// been settled by, and a row whose transfer died with a session rather
    /// than with the process.
    ///
    /// **Why this is a separate call and not part of
    /// [`chat_history`](Self::chat_history).** Reconciling is a write, and
    /// history is read constantly during a live conversation — on open, after
    /// every send, and again when a file settles. Folding the two together
    /// would mark a genuinely in-flight row `Interrupted` moments after
    /// `chat_send_file` created it, and since a settled row is deliberately no
    /// longer writable, its real completion would then be dropped.
    ///
    /// A row whose transfer is registered **right now** is skipped for the
    /// same reason: a user can open a thread while a share to that peer is
    /// still moving. That check is why this cannot simply delegate to
    /// `ChatStore::reconcile_peer`, which knows nothing about transfers.
    ///
    /// **The two windows where a live row has no `active` entry**, stated
    /// plainly rather than implied away, because "is it in `active`?" is the
    /// whole basis of the skip:
    ///
    /// * **The sender's dial window — closed.** The row is written
    ///   `Transferring` by `prepare_file_send` before anything is dialed, and
    ///   the dial plus `peerbeam_chat`'s `CHANNEL_OPEN_BUDGET` can take several
    ///   seconds. [`chat_send_file`](Self::chat_send_file) therefore claims the
    ///   id in `active` **synchronously**, before spawning the network task, so
    ///   this window is covered. It was not always: backing out of a thread and
    ///   re-entering inside it fired this reconcile, wrote `Interrupted` — a
    ///   status outside the writable set — and the real `Sent` was then
    ///   silently dropped, leaving a perfectly transferred file reading
    ///   "Interrupted" forever while the receiver's row said `Received`.
    ///
    ///   Since 2b that claim is no longer what closes it: a queued file's row
    ///   is `Staging` and then `Pending`, and neither is a status this function
    ///   looks at, so the whole stage-and-wait window — which can now last
    ///   minutes, or days for an offline peer — is immune by construction. The
    ///   id is claimed in `run_queued_file`, immediately before the row moves
    ///   to `Transferring`.
    ///
    /// * **The receiver's pre-first-frame window — open, and accepted.** The
    ///   inbound row is written `PendingApproval` by `ChatHandler` the moment
    ///   the peer's `FileRef` lands on the CHAT channel, while the transfer is
    ///   only registered (`register_vacant`) once the *first TRANSFER frame*
    ///   arrives and is peeked. A reconcile fired inside that gap settles the
    ///   row `Interrupted`. It is left open deliberately: the alternative is
    ///   registering a transfer on a peer's say-so before any transfer exists,
    ///   which is exactly the phantom-transfer bug the `STREAM_GRACE` wait was
    ///   introduced to remove. The gap is one round trip — the sender opens the
    ///   stream immediately after the `FileRef` — and the cost of losing that
    ///   race is a row the user can ask for again, not a wrong outcome for a
    ///   file that moved.
    pub fn chat_reconcile(&self, req: &Value) -> Op {
        let peer_id = req
            .get("peer_id")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "peer_id required".into()))?;
        let peer = DeviceId::from(peer_id.to_string());
        let history = self
            .chat
            .history(&peer)
            .map_err(|e| (Code::Internal, e.to_string()))?;
        let mut changed = 0_u64;
        for rec in history {
            if !matches!(
                rec.status,
                ChatStatus::Transferring | ChatStatus::PendingApproval
            ) {
                continue;
            }
            // Its transfer exists in this process: genuinely in flight, and the
            // terminal event that owns this row is still coming.
            if self.active.lock().unwrap().contains_key(&rec.id) {
                continue;
            }
            if let Err(e) = self
                .chat
                .set_status(&peer, &rec.id, ChatStatus::Interrupted)
            {
                tracing::warn!(error = %e, message_id = %rec.id, "chat row not marked interrupted");
                continue;
            }
            changed += 1;
            events::chat_status(&peer.0, &rec.id, chat_status_str(ChatStatus::Interrupted));
        }
        Ok(json!({ "changed": changed }))
    }

    pub fn chat_history(&self, req: &Value) -> Op {
        let peer_id = req
            .get("peer_id")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "peer_id required".into()))?;
        let peer = DeviceId::from(peer_id.to_string());
        let hist = self
            .chat
            .history(&peer)
            .map_err(|e| (Code::Internal, e.to_string()))?;
        let messages: Vec<Value> = hist.iter().map(events::record_dto).collect();
        Ok(json!({ "messages": messages }))
    }

    // ── receiving ───────────────────────────────────────────────

    /// Accept inbound connections forever; one task per incoming transfer.
    ///
    /// Every return path — a bind failure, or the inbound stream ending
    /// (transport/endpoint gone) — resets `daemon_running` via
    /// `mark_daemon_stopped()` before returning. Without that, a dead daemon
    /// still reports `running: true` from `daemon_status()`, and
    /// `start_daemon()`'s idempotency guard (`daemon_running.swap`) refuses
    /// to ever spawn a replacement, permanently wedging the receive side
    /// until the whole process restarts.
    pub async fn serve(self: Arc<Self>, port: u16) {
        let bind = format!("0.0.0.0:{port}").parse().expect("valid bind");
        let (_local, mut incoming) = match self.quic.serve_channels_on(bind).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "receive server failed to bind");
                self.mark_daemon_stopped();
                return;
            }
        };
        while let Some(item) = incoming.next().await {
            match item {
                Ok(qc) => {
                    let mgr = self.clone();
                    crate::runtime::spawn(async move { mgr.handle_incoming(qc).await });
                }
                Err(e) => tracing::warn!(error = %e, "inbound rejected"),
            }
        }
        // The incoming stream ended on its own (endpoint/transport gone) —
        // nothing called `stop_daemon()`, but the daemon is just as dead.
        self.mark_daemon_stopped();
    }

    /// Wait for the user's decision on a just-authenticated incoming
    /// transfer `id`, bounded by [`ACCEPT_TIMEOUT`] so a connection drop or
    /// an unanswered prompt can't park the caller (and the counted `active`
    /// slot) forever. The pending entry is removed before returning on every
    /// path — explicit accept, explicit accept-and-trust, explicit decline
    /// (`reject`, or `cancel` firing the sender with [`AcceptDecision::Reject`]),
    /// a dropped sender, or a timeout — so a stale id can never be acted on
    /// by a later `accept`/`accept_trust`/`reject` call. Trust is recorded
    /// only for [`AcceptDecision::AcceptAndTrust`] — a plain one-time accept
    /// never approves the device, so it never gains auto-accept on its own.
    ///
    /// Returns an [`AcceptOutcome`] rather than a `bool` so the caller can tell
    /// an explicit refusal from a prompt nobody answered. Both stop the
    /// transfer; only the former may be reported to the peer as a decline (see
    /// [`AcceptOutcome`] for why that distinction is not cosmetic).
    async fn wait_for_accept(&self, id: &str, peer_id: &DeviceId) -> AcceptOutcome {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.to_string(), tx);
        let outcome = match tokio::time::timeout(ACCEPT_TIMEOUT, rx).await {
            Ok(Ok(AcceptDecision::AcceptOnce)) => AcceptOutcome::Accepted,
            Ok(Ok(AcceptDecision::AcceptAndTrust)) => {
                // Explicit accept-and-trust: this device is now approved for
                // auto-accept on future connections. Never set on a plain
                // accept, a decline, a dropped sender, or a timeout.
                let _ = self.trust.approve(peer_id);
                AcceptOutcome::Accepted
            }
            // The user's own refusal — `reject()`, or `cancel()` firing the
            // pending sender.
            Ok(Ok(AcceptDecision::Reject)) => AcceptOutcome::Rejected,
            // `Ok(Err(_))`: the sending half dropped without ever deciding.
            // `Err(_)`: ACCEPT_TIMEOUT elapsed with the prompt unanswered.
            // Neither is the user saying no.
            Ok(Err(_)) | Err(_) => AcceptOutcome::Unanswered,
        };
        self.pending.lock().unwrap().remove(id);
        outcome
    }

    async fn handle_incoming(self: Arc<Self>, qc: QuicChannels) {
        // Establish the responder PeerSession (runs the handshake internally).
        // Chat wiring: the handler persists each inbound record via
        // `self.chat` itself, then calls the sink purely to notify — the
        // sink must not re-persist.
        let mut session = match crate::session_exec::accept(
            qc,
            self.identity(),
            self.enc.clone(),
            self.trust.clone(),
            Some(self.chat_wiring()),
        )
        .await
        {
            Ok(s) => s,
            Err((_, msg)) => {
                tracing::warn!(error = %msg, "incoming session failed");
                return;
            }
        };
        // Flush-on-connect: deliver anything queued for this peer over the
        // session we just accepted (cheaper + faster than waiting for the
        // next drain tick), independent of whatever this inbound connection
        // is actually for (a transfer, or nothing at all) — best-effort, so a
        // peer with an empty outbox costs nothing beyond the lookup.
        //
        // TEXT AND DECLINES ONLY, deliberately: a queued *file* is not started
        // here. Sending it would mean running a transfer over a session this
        // function still owns and is about to close once its own incoming
        // transfer finishes — two unrelated lifetimes on one session, with the
        // outgoing file dying whenever the inbound one happens to end. The
        // queued file goes out on the next drain tick instead, over a session
        // dialed for it; a peer that just connected to us is, by definition,
        // one discovery can see.
        {
            let flushed =
                peerbeam_chat::flush_to_session(&session.handle, &self.chat, &session.peer_device)
                    .await
                    .unwrap_or_default();
            for mid in &flushed {
                events::chat_status(&session.peer_device.0, mid, "sent");
            }
        }

        // Only a connection that actually opens a transfer stream is a
        // transfer. Since chat 1b, peers dial purely to deliver chat, so
        // registering a transfer and prompting the user before knowing whether
        // any stream is coming raised a phantom "incoming file" approval for
        // every chat message — and, with auto-accept on, wrote a failed-transfer
        // history row for it. Wait for the stream first, bounded.
        let incoming_ch = match tokio::time::timeout(STREAM_GRACE, session.next_incoming()).await {
            Ok(Some(c)) => c,
            // No stream channel: a chat-only dial. Close quietly — no `active`
            // entry, no transfer_queued, no approval prompt, no history row.
            Ok(None) | Err(_) => {
                session.close().await;
                return;
            }
        };

        // Read the transfer's opening frame WITHOUT consuming it: the sender's
        // own transfer id, the file/folder name, and the size. The returned
        // channel replays that frame, so `receive_on_channel` below runs
        // exactly as it did before this existed. This is only possible because
        // the stream channel is now obtained before any registration.
        //
        // Everything the peek yields is fail-soft: an absent, malformed,
        // undecodable or merely slow first frame gives an empty preview and we
        // fall back to a locally minted id and the "(incoming)" placeholder —
        // i.e. exactly the behaviour that predates this.
        let (incoming_ch, preview) = peek_incoming_meta(incoming_ch).await;

        // Prefer the peer's human name from the handshake; fall back to the raw
        // device id only when the peer presented no name.
        let peer = {
            let n = session.peer_name.trim();
            if n.is_empty() {
                session.peer_id.clone()
            } else {
                n.to_string()
            }
        };

        // `preview` is entirely PEER-SUPPLIED, and both fields taken from it
        // are load-bearing: the id becomes a registry key, the name is shown in
        // the approval prompt the user acts on. Neither is trusted:
        //
        //  * the name arrives already reduced to the single, sanitized path
        //    component the receive path will actually write (see
        //    `peek_incoming_meta`), so the prompt cannot be made to display a
        //    path, and the name it shows is the name that lands on disk;
        //  * the id is charset/length-checked and then only *claimed if
        //    vacant* — a peer can neither overwrite an existing transfer nor
        //    pre-empt a future one (see `register_vacant`). A refused id costs
        //    only the correlation, never the transfer.
        let display = if preview.name.is_empty() {
            "(incoming)".to_string()
        } else {
            preview.name.clone()
        };
        let claimed = if is_valid_transfer_id(&preview.transfer_id) {
            self.register_vacant(
                &preview.transfer_id,
                "receiving",
                &peer,
                &session.peer_device.0,
                &display,
                None,
            )
            .map(|a| (preview.transfer_id.clone(), a))
        } else {
            None
        };
        let (id, active) = match claimed {
            Some(pair) => pair,
            None => self.register_fresh("receiving", &peer, &session.peer_device.0, &display, None),
        };
        // Seed the total so the size shown before the first progress update is
        // the real one rather than 0; every later `update()` overwrites it from
        // the wire anyway.
        active.stats.lock().unwrap().total = preview.size;
        // If this transfer is carrying a file offered in a conversation, the
        // row was written from the peer's CHAT-channel `FileRef` — a claim
        // never checked against the stream that decides what lands on disk.
        // Correct it now, BEFORE the approval gate below: the name and size the
        // user is about to consent to must be the transfer's, not the offer's.
        // A no-op for the vast majority (an ordinary transfer has no row), and
        // a no-op when the peek learned nothing (empty name).
        self.chat_set_landing(&active, &preview.name, preview.size);
        events::transfer(
            &id,
            "transfer_queued",
            json!({
                "peer": peer,
                "peer_id": session.peer_device.0,
                "incoming": true,
                "file": display,
                "size": preview.size,
                "newly_trusted": session.newly_trusted,
                "pairing_code": session.pairing_code.clone(),
            }),
        );

        // Approval: auto-accept only peers explicitly approved by the user on
        // a prior transfer, else wait for a decision. A pinned key alone
        // (TOFU trust, MITM protection) is not consent to auto-accept — that
        // requires the user to have accepted at least once before.
        // Read the flag fresh so a live toggle applies without a restart.
        let auto = self.auto_accept.load(Ordering::SeqCst);
        let approved = self
            .trust
            .lookup(&session.peer_device)
            .ok()
            .flatten()
            .map(|r| r.approved)
            .unwrap_or(false);
        let outcome = if auto && approved {
            AcceptOutcome::Accepted
        } else {
            self.wait_for_accept(&id, &session.peer_device).await
        };
        if !outcome.accepted() {
            // Claim before emitting, and emit only if the claim lands. The
            // decision can arrive after `cancel()` already claimed this
            // transfer and announced it (a user cancel fires the pending
            // sender with `Reject`, which is one of the ways we get here), and
            // — since the id came off the wire — the entry now under it may
            // even belong to a *different*, live transfer. Emitting
            // unconditionally would announce a cancellation against whichever
            // of those the surface is currently showing.
            if self.claim(&active).is_some() {
                events::transfer(
                    &id,
                    "transfer_cancelled",
                    json!({ "peer_id": session.peer_device.0, "reason": "rejected" }),
                );
                // We turned this file down, so our own row says `Declined`
                // rather than `Failed` — nothing went wrong, we chose. Guarded
                // like every other bridge: a declined *ordinary* transfer has
                // no chat row and settles nothing.
                self.chat_settle(&active, ChatStatus::Declined, None);
            }
            // Tell the sender, so an explicit refusal is terminal for them too.
            // Without this the sender cannot tell "you declined" from "the
            // network dropped", and a queued file — retried keep-forever like
            // text — would be re-offered every drain tick, re-prompting this
            // user every single time.
            //
            // Every condition on that lives in `should_send_decline`, which is
            // where all three legs (explicit refusal, negotiated capability,
            // real chat row) are documented and unit-tested. Notably a prompt
            // that merely TIMED OUT gets here too, and must send nothing.
            //
            // Best-effort: our row is already settled locally either way, so a
            // send failure changes nothing here.
            if should_send_decline(
                outcome,
                &session.capabilities,
                &self.chat,
                &session.peer_device,
                &id,
            ) {
                let d = peerbeam_chat::FileDecline::new(&id);
                if let Err(e) = peerbeam_chat::send_file_decline(&session.handle, &d).await {
                    // The sender dropped while our prompt was open — the one
                    // case a decline cannot be delivered live. Queue it in the
                    // outbox text already uses, so the refusal still reaches
                    // them when they come back instead of being lost and
                    // costing this user three more prompts before the sender's
                    // own backstop gives up. `enqueue_decline` refuses to take
                    // an occupied key, so a sender that chose the id of a
                    // message we have queued for it cannot displace it.
                    tracing::debug!(error = %e, transfer_id = %id, "file decline not delivered live");
                    match self.chat.enqueue_decline(&session.peer_device, &d) {
                        Ok(true) => tracing::info!(transfer_id = %id, "file decline queued"),
                        Ok(false) => tracing::warn!(
                            transfer_id = %id,
                            "file decline not queued: id already in the outbox"
                        ),
                        Err(e) => {
                            tracing::warn!(error = %e, transfer_id = %id, "file decline not queued")
                        }
                    }
                }
            }
            session.close().await;
            return;
        }

        events::transfer(
            &id,
            "transfer_started",
            json!({ "peer": peer, "peer_id": session.peer_device.0 }),
        );
        *active.status.lock().unwrap() = "transferring".into();
        // The decision is made and the bytes are cleared to move, so a chat
        // file row must stop saying it is waiting on one. Until this existed,
        // a receiving row sat at `PendingApproval` for the entire download:
        // its bubble showed a dead progress bar and — worse — kept rendering
        // live-looking Accept / Trust / Decline controls for a decision that
        // had already been made, and which under auto-accept was never asked
        // (the `wait_for_accept` short-circuit above leaves no `pending` entry,
        // so Decline could not even be honoured). Guarded like every other
        // bridge: an ordinary transfer has no row and settles nothing.
        self.chat_settle(&active, ChatStatus::Transferring, None);

        let save_dir = self.save_dir();
        let storage = self.storage();
        let ctrl = active.ctrl.clone();
        // Filled in by the folder branch with the sanitized root name
        // `receive_folder` actually wrote under `save_dir`, so history/"open"
        // can point at the folder itself instead of its parent.
        let folder_root = Arc::new(std::sync::Mutex::new(None::<String>));
        let dest_dir = save_dir.clone();
        let folder_root_cell = folder_root.clone();
        let handle = session.handle.clone();
        let outcome = drive(
            id.clone(),
            active.stats.clone(),
            active.file.clone(),
            active.ctrl.clone(),
            |ptx| async move {
                let r = receive_on_channel(incoming_ch, &handle, &storage, &dest_dir, &ctrl, &ptx)
                    .await
                    .map(|received| match received {
                        ChannelReceived::File(f) => f.outcome,
                        ChannelReceived::Folder(fr) => {
                            *folder_root_cell.lock().unwrap() = Some(fr.root);
                            fr.outcome
                        }
                    });
                drop(ptx);
                r
            },
            None,
            None,
        )
        .await;
        if matches!(outcome, Ok(TransferOutcome::Completed)) {
            if let Some(root) = folder_root.lock().unwrap().clone() {
                *active.path.lock().unwrap() =
                    Some(format!("{}/{}", save_dir.trim_end_matches('/'), root));
            }
        }
        session.close().await;
        self.finish(&active, outcome);
    }
}

/// How long an incoming transfer waits for the user to accept/reject before
/// it's treated as abandoned. Without this bound, a connection that dies (or
/// a prompt nobody answers) parks the handler on the approval channel
/// forever: the transfer stays in `active` — counted by the UI/notification —
/// with no terminal event ever emitted. Long enough that a human answering a
/// prompt is never rushed; short enough that ghosts don't accumulate.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(180);

/// How long to wait for the peer's first progress report before assuming the
/// peer doesn't support the back-channel and falling back to bytes-sent.
const PEER_PROGRESS_GRACE: Duration = Duration::from_secs(3);

/// How long to wait for the peer's first transfer stream channel before
/// concluding this connection carries no transfer at all. Since chat 1b a peer
/// may dial purely to deliver chat (`chat_flush_peer`, the drain loop,
/// flush-on-connect), and such a dial must not register a transfer or raise an
/// approval prompt.
///
/// This timeout is NOT what detects a chat-only dial: a chat-only dialer
/// closes its side right after flushing (`Manager::chat_flush_peer`; the CLI's
/// own drain loop notes the same — "a `chat send` always closes right after
/// sending"), so `next_incoming()` resolves to `None` promptly and the early
/// return above fires immediately, regardless of this value. This is only a
/// backstop against a peer that opens a session and then neither opens a
/// stream nor closes it.
///
/// It must be generous: `open_stream_channel` sends no probe frame (unlike
/// `open_channel`), so the receiver's `next_incoming()` resolves only when the
/// sender performs its first application write on the stream — not when it
/// calls `open_stream_channel`. For a folder send, that first write is the
/// manifest, emitted only after the entire tree has been recursively
/// enumerated — which, on cold-cache, network-mounted, or FUSE-backed storage,
/// can take many seconds. Erring long costs only a lingering session task;
/// erring short silently drops a real transfer (the receiver times out and
/// closes while the sender is mid-enumeration, and the sender's first write
/// then fails with a bare connection error indistinguishable from a network
/// fault). It also guards any accepted session still receiving a chat
/// backlog: cutting it off early would mark in-flight messages Sent and lose
/// them (see `flush_to_session`).
const STREAM_GRACE: Duration = Duration::from_secs(60);

/// Minimum spacing between emitted progress updates (~20/s) — keeps small-chunk
/// progress smooth without flooding the event bridge.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

/// Run a transfer while pumping its progress into stats + `transfer_progress`
/// events.
///
/// `progress_out` (receiver): mirror our received-byte count to the sender over
/// the back-channel. `progress_in` (sender): once the peer starts reporting,
/// drive the displayed bar from the **peer's** confirmed bytes instead of
/// bytes-sent — so the sender sees the receiver's real progress over a slow
/// link. If the peer never reports (old build / non-QUIC), we fall back to
/// bytes-sent after a short grace.
///
/// `ctrl` is this transfer's control handle, needed here (independent of
/// whatever `run` closed over it with) for the sender side of cooperative
/// pause: a receiver-initiated pause reaches us as a
/// [`BACK_PAUSE`]/[`BACK_RESUME`] sentinel on the same back-channel that
/// otherwise only ever carries real byte counts (see `in_task` below), and
/// pausing/resuming `ctrl` here is what actually stops/resumes the send loop
/// (which was handed its own clone of the same `TransferControl`).
async fn drive<F, Fut>(
    id: String,
    stats: Arc<Mutex<Stats>>,
    file: Arc<Mutex<String>>,
    ctrl: TransferControl,
    run: F,
    progress_out: Option<Box<dyn peerbeam_domain::port::ProgressSink>>,
    progress_in: Option<Box<dyn peerbeam_domain::port::ProgressSource>>,
) -> DResult<TransferOutcome>
where
    F: FnOnce(mpsc::UnboundedSender<Progress>) -> Fut,
    Fut: std::future::Future<Output = DResult<TransferOutcome>>,
{
    let (ptx, mut prx) = mpsc::unbounded_channel::<Progress>();

    // Sender: true once the peer's back-channel has started driving the bar, so
    // the pump stops emitting bytes-sent to avoid a fight.
    let peer_driving = Arc::new(AtomicBool::new(false));
    // Sender with a peer channel: suppress the bytes-sent bar until either the
    // peer starts reporting (realtime receiver progress from ~0) or the grace
    // expires with no peer (then fall back to bytes-sent). Prevents the initial
    // jump to the QUIC send-window size that bytes-sent would show.
    let peer_expected = progress_in.is_some();
    let fell_back = Arc::new(AtomicBool::new(false));

    // Receiver → sender mirroring runs on its own task fed by a channel, so a
    // slow/absent/old peer can never stall the pump or the transfer.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<u64>();
    let out_task = async move {
        let Some(mut sink) = progress_out else {
            while out_rx.recv().await.is_some() {} // drain
            return;
        };
        // `None` means "nothing sent yet" — kept distinct from any `u64`
        // value (rather than a magic number like `u64::MAX`) because
        // `BACK_PAUSE`/`BACK_RESUME` now legitimately use the top of the
        // `u64` range: a magic-sentinel `last` would make the very first
        // pause on a fresh channel indistinguishable from "already sent
        // this" and get silently swallowed.
        let mut last: Option<u64> = None;
        while let Some(bytes) = out_rx.recv().await {
            if last == Some(bytes) {
                continue;
            }
            last = Some(bytes);
            if sink.report(bytes).await.is_err() {
                break; // peer gone / doesn't accept — stop quietly
            }
        }
    };

    // Sender: read the peer's confirmed bytes and drive the bar from them.
    let in_id = id.clone();
    let in_stats = stats.clone();
    let in_driving = peer_driving.clone();
    let in_fell_back = fell_back.clone();
    let in_task = async move {
        let Some(mut source) = progress_in else {
            return;
        };
        // First real byte report has a grace window; a pause/resume sentinel
        // arriving before it is handled immediately (see `handle_back_channel`)
        // and does not consume the grace, since it isn't the report being
        // waited for. If no real report ever comes, leave the bar to the
        // bytes-sent fallback in the pump.
        let deadline = tokio::time::sleep(PEER_PROGRESS_GRACE);
        tokio::pin!(deadline);
        let mut latest: u64;
        loop {
            tokio::select! {
                r = source.recv() => match r {
                    Ok(Some(value)) => match handle_back_channel(&in_id, &ctrl, value) {
                        Some(bytes) => {
                            in_driving.store(true, Ordering::SeqCst);
                            emit_peer(&in_id, &in_stats, bytes);
                            latest = bytes;
                            break;
                        }
                        None => continue,
                    },
                    _ => {
                        in_fell_back.store(true, Ordering::SeqCst);
                        return;
                    }
                },
                _ = &mut deadline => {
                    in_fell_back.store(true, Ordering::SeqCst);
                    return;
                }
            }
        }
        // Emit on each report (up to ~20/s), and at least once a second as a
        // heartbeat so speed/ETA keep ticking and the bar never looks frozen on
        // a slow/stalled link.
        let mut beat = tokio::time::interval(Duration::from_secs(1));
        beat.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                r = source.recv() => match r {
                    Ok(Some(value)) => {
                        if let Some(bytes) = handle_back_channel(&in_id, &ctrl, value) {
                            latest = bytes;
                            emit_peer(&in_id, &in_stats, latest);
                        }
                    }
                    _ => break,
                },
                _ = beat.tick() => emit_peer(&in_id, &in_stats, latest),
            }
        }
    };

    let pump_id = id.clone();
    let pump_driving = peer_driving.clone();
    let pump_fell_back = fell_back.clone();
    let pump = async move {
        // Throttle emission/mirroring to ~20/s so small chunks stay smooth
        // without flooding the event bridge; always let the final update through.
        let mut last = Instant::now()
            .checked_sub(PROGRESS_INTERVAL)
            .unwrap_or_else(Instant::now);
        // Tracks whether the previous message left us in a paused state, so a
        // `Progress{status: Paused}` (from a receive loop's own pause edge —
        // see `stream::receive_file`) only fires the event/back-channel
        // signal once per pause, and the matching resume fires once too.
        let mut was_paused = false;
        while let Some(p) = prx.recv().await {
            if let Some(f) = &p.current_file {
                *file.lock().unwrap() = f.clone();
            }

            // A pause/resume status change is a signal, not a byte update —
            // relay it immediately (bypassing the throttle below, which
            // exists only for high-frequency byte progress) to two places:
            // this side's own UI, via a `transfer_paused`/`transfer_resumed`
            // event (this is what delivers the event for a receiver paused
            // by a peer `Control::Pause` frame, which never goes through
            // `Manager::pause()`), and the peer, via the back-channel
            // sentinel (the receiver's half of cooperative pause, read by
            // `in_task` above on the sender's side).
            if p.status == TransferStatus::Paused {
                if !was_paused {
                    was_paused = true;
                    events::transfer(&pump_id, "transfer_paused", json!({}));
                    let _ = out_tx.send(BACK_PAUSE);
                }
                continue;
            }
            if was_paused {
                was_paused = false;
                events::transfer(&pump_id, "transfer_resumed", json!({}));
                let _ = out_tx.send(BACK_RESUME);
                // Fall through: this message may also carry a real update.
            }

            let is_final = p.total_bytes > 0 && p.transferred_bytes >= p.total_bytes;
            let due = is_final || last.elapsed() >= PROGRESS_INTERVAL;
            // If the peer channel is driving the bar, only keep `total` fresh;
            // the in_task emits the peer's real count.
            if pump_driving.load(Ordering::SeqCst) {
                stats.lock().unwrap().total = p.total_bytes;
                continue;
            }
            // Sender still waiting on the peer channel (within grace): don't show
            // bytes-sent yet — it would jump to the QUIC send-window size. Track
            // stats silently; the in_task or the grace fallback will emit. The
            // final update always passes: a completed protocol means the bytes
            // are confirmed, and a fast transfer may finish inside the grace.
            if peer_expected && !pump_fell_back.load(Ordering::SeqCst) && !is_final {
                stats
                    .lock()
                    .unwrap()
                    .update(p.transferred_bytes, p.total_bytes);
                continue;
            }
            if !due {
                stats
                    .lock()
                    .unwrap()
                    .update(p.transferred_bytes, p.total_bytes);
                continue;
            }
            last = Instant::now();
            let _ = out_tx.send(p.transferred_bytes); // receiver mirrors out
            let dto = {
                let mut s = stats.lock().unwrap();
                s.update(p.transferred_bytes, p.total_bytes);
                s.dto()
            };
            events::transfer(
                &pump_id,
                "transfer_progress",
                json!({ "stats": dto, "file": p.current_file }),
            );
        }
        drop(out_tx); // close the mirror channel so out_task ends
    };

    let work = run(ptx);
    let (r, _, _, _) = tokio::join!(work, pump, in_task, out_task);
    r
}

/// Interpret one raw back-channel value: a [`BACK_PAUSE`]/[`BACK_RESUME`]
/// sentinel updates `ctrl` and emits the matching event — so a
/// receiver-initiated pause/resume also stops/resumes this (sender) side's
/// send loop and shows up in this side's UI — and returns `None` (there is no
/// byte count to act on). Any other value is a real received-byte count,
/// returned as `Some` for the caller to use.
fn handle_back_channel(id: &str, ctrl: &TransferControl, value: u64) -> Option<u64> {
    match value {
        BACK_PAUSE => {
            ctrl.pause();
            events::transfer(id, "transfer_paused", json!({}));
            None
        }
        BACK_RESUME => {
            ctrl.resume();
            events::transfer(id, "transfer_resumed", json!({}));
            None
        }
        bytes => Some(bytes),
    }
}

/// Emit a `transfer_progress` event using the peer's confirmed byte count.
fn emit_peer(id: &str, stats: &Arc<Mutex<Stats>>, peer_bytes: u64) {
    let dto = {
        let mut s = stats.lock().unwrap();
        let total = s.total;
        s.update(peer_bytes, total);
        s.dto()
    };
    events::transfer(id, "transfer_progress", json!({ "stats": dto }));
}

// ── helpers ─────────────────────────────────────────────────────

fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Emit a daemon lifecycle event.
fn daemon_event(kind: &str, port: u16) {
    events::emit(&json!({
        "type": kind,
        "timestamp": timestamp(),
        "payload": { "port": port },
    }));
}

/// The wire spelling of a chat [`ChatStatus`] for a `chat_status` event.
///
/// Must stay byte-identical to the record's own serde representation: a
/// surface applies the event's `status` straight onto the row it already read
/// from `pb_chat_history`, so two spellings for one state would show a row that
/// disagrees with itself. Pinned by
/// `chat_status_str_matches_the_records_serialized_form`.
fn chat_status_str(status: ChatStatus) -> &'static str {
    match status {
        ChatStatus::Pending => "pending",
        ChatStatus::Sent => "sent",
        ChatStatus::Received => "received",
        ChatStatus::PendingApproval => "pendingapproval",
        ChatStatus::Transferring => "transferring",
        ChatStatus::Declined => "declined",
        ChatStatus::Failed => "failed",
        ChatStatus::Interrupted => "interrupted",
        ChatStatus::Staging => "staging",
    }
}

/// Longest transfer id accepted from outside this process. Generous next to
/// anything real (a minted `tx-<pid>-<n>` is ~20 bytes, a chat `FileRef` id is
/// exactly 29) and small enough that an id can never be a payload in its own
/// right — it is echoed into every event and into the persisted history.
const MAX_TRANSFER_ID: usize = 128;

/// Whether an id supplied from outside this process — by a caller in the
/// request JSON, or by a peer on the wire — is usable as a transfer id.
///
/// It is deliberately narrow. The id is a registry key, is echoed verbatim
/// into every event payload and into the persisted history document, and is
/// read back by surfaces that may treat it as an identifier: keeping it to a
/// bounded, boring, single-token charset means none of those can be surprised
/// by it. `.`/`..` are rejected outright — the id is never used as a path
/// today, and this keeps that true if some future consumer forgets.
fn is_valid_transfer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_TRANSFER_ID
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Read a caller-supplied `transfer_id` out of request JSON, rejecting
/// anything [`is_valid_transfer_id`] refuses.
fn valid_transfer_id(v: &Value) -> Result<String, (Code, String)> {
    match v.as_str() {
        Some(s) if is_valid_transfer_id(s) => Ok(s.to_string()),
        _ => Err((
            Code::InvalidArgument,
            format!("transfer_id must be 1-{MAX_TRANSFER_ID} chars of [A-Za-z0-9._-]"),
        )),
    }
}

/// Build a target `Device` from a `peer` JSON object.
fn device_from(peer: Option<&Value>) -> Result<Device, (Code, String)> {
    let peer = peer.ok_or((Code::InvalidArgument, "peer required".into()))?;
    let addresses: Vec<String> = peer
        .get("addresses")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if addresses.is_empty() {
        return Err((Code::InvalidArgument, "peer.addresses required".into()));
    }
    let port = peer.get("port").and_then(|p| p.as_u64()).unwrap_or(0) as u16;
    if port == 0 {
        return Err((Code::InvalidArgument, "peer.port required".into()));
    }
    let name = peer
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("peer")
        .to_string();
    let id = peer
        .get("id")
        .and_then(|i| i.as_str())
        .unwrap_or("peer")
        .to_string();
    Ok(Device {
        id: DeviceId::from(id),
        name,
        device_type: DeviceType::Desktop,
        platform: peerbeam_platform::current(),
        addresses,
        port,
        last_seen: chrono::Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::port::EncryptionProvider;
    use peerbeam_domain::session::{
        Capability, ChannelType, CHAT_FEAT_FILEDECLINE, CHAT_FEAT_FILEREF,
    };

    /// A `Manager` with no daemon/history wired up, just enough to exercise
    /// identity/name plumbing in isolation (no network I/O beyond binding an
    /// ephemeral local QUIC endpoint, no discovery).
    fn test_manager(name: &str) -> Manager {
        test_manager_with_port(name, 0)
    }

    /// Like [`test_manager`], but with an explicit `daemon_port` — needed by
    /// tests that exercise `start_daemon()`/`serve()` against a port they
    /// control (e.g. one already occupied, to force a bind failure).
    fn test_manager_with_port(name: &str, daemon_port: u16) -> Manager {
        test_manager_full(name, daemon_port).0
    }

    /// The full construction: the manager, a clone of the `ChatStore` it writes
    /// to (so a test can seed and read conversation rows against the same
    /// on-disk store), and the `TempDir` that backs both — returned so the
    /// caller can keep it alive for the duration of the test.
    fn test_manager_full(name: &str, daemon_port: u16) -> (Manager, ChatStore, tempfile::TempDir) {
        let quic = Arc::new(QuicTransport::new().expect("quic transport"));
        let rm = Arc::new(RouteManager::new(quic.clone()));
        let enc = Arc::new(AeadCrypto::new());
        let keypair = enc.generate_keypair();
        let dir = tempfile::tempdir().expect("tempdir");
        let trust = Arc::new(FsTrust::open(dir.path().join("trust.json")).expect("trust store"));
        let identity = Identity {
            device_id: DeviceId::from("test-device"),
            name: name.to_string(),
            keypair,
        };
        let chat_key =
            peerbeam_crypto::derive_subkey(&identity.keypair.secret.0, b"peerbeam-appstore-v1");
        let appstore: Arc<dyn peerbeam_domain::port::AppStore> =
            Arc::new(peerbeam_appstore_fs::FsAppStore::open(
                dir.path().join("appstore"),
                chat_key,
                enc.clone(),
            ));
        let chat = ChatStore::new(appstore);
        let staging = Arc::new(StagingStore::new(
            dir.path()
                .join("outbox-blobs")
                .to_string_lossy()
                .into_owned(),
            Arc::new(FsStorage::new()),
        ));
        let mgr = Manager::new(
            rm,
            quic,
            enc,
            trust,
            chat.clone(),
            staging,
            StagingLimits {
                max_bytes: u64::MAX,
                min_free_bytes: 0,
            },
            identity,
            dir.path().to_string_lossy().into_owned(),
            false,
            1024,
            daemon_port,
            None,
        );
        (mgr, chat, dir)
    }

    // ── the transfer → chat-record bridge ────────────────────────

    /// Collected events for the serial bridge test below. `events::CALLBACK` is
    /// process-global, so anything reading it must be `#[serial_test::serial]`
    /// (same convention as `events.rs`'s own callback test).
    static BRIDGE_EVENTS: Mutex<Vec<Value>> = Mutex::new(Vec::new());

    extern "C" fn collect_bridge(ptr: *const std::os::raw::c_char) {
        let s = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_str()
            .unwrap_or_default()
            .to_string();
        // The callback owns the string, exactly as `pb_free_string` would.
        unsafe {
            drop(std::ffi::CString::from_raw(
                ptr as *mut std::os::raw::c_char,
            ))
        };
        if let Ok(v) = serde_json::from_str(&s) {
            BRIDGE_EVENTS.lock().unwrap().push(v);
        }
    }

    fn file_meta(r: &peerbeam_chat::FileRef) -> peerbeam_chat::FileMeta {
        peerbeam_chat::FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: None,
        }
    }

    /// Register a transfer so a bridge test has a real `Active` to settle from.
    fn active_for(mgr: &Manager, id: &str, direction: &'static str, peer_id: &str) -> Arc<Active> {
        mgr.register_vacant(id, direction, "bob", peer_id, "a.bin", None)
            .expect("id vacant")
    }

    /// The guard, in one test. A terminal transfer event may only ever write to
    /// **its own** chat row: the right conversation, the right row, and one that
    /// is still in flight. Everything else — an ordinary transfer with no row at
    /// all, a peer-chosen id that lands on a *text* row, one that lands on an
    /// already-settled file row, and one whose direction disagrees with the
    /// transfer — must be a total no-op: no write, no event.
    ///
    /// The negative cases are not hypothetical. An incoming transfer registers
    /// under the id the peer put on the wire, and chat message ids are wire
    /// fields the peer already knows, so a presence-only guard turns a transfer
    /// id into a write primitive aimed at any row in that conversation: an
    /// already-paired peer could stamp `Received` onto our own outbound text, or
    /// re-open a file we declined. The positive cases are here too, so the guard
    /// cannot pass by simply refusing everything.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_settle_is_silent_for_a_transfer_that_has_no_chat_row() {
        let (mgr, chat, _dir) = test_manager_full("bridge", 0);
        let peer = DeviceId::from("pb-bob");
        let file_row = |r: &peerbeam_chat::FileRef, status| {
            chat.append(&peerbeam_chat::ChatRecord::file_out(
                &peer,
                r,
                file_meta(r),
                status,
            ))
            .expect("seed a file row");
        };

        // (+) An outbound file genuinely in flight: our own send, mid-transfer.
        let in_flight = peerbeam_chat::FileRef::new("a.bin", 3).expect("file ref");
        file_row(&in_flight, ChatStatus::Transferring);
        // (+) An inbound file awaiting our approval.
        let offered = peerbeam_chat::FileRef::new("b.bin", 4).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_in(&peer, &offered))
            .expect("seed the offered row");
        // (-) Our own outbound TEXT message, already sent.
        let text = peerbeam_chat::ChatMessage::new("hello there").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &text))
            .expect("seed the text row");
        // (-) A file share that already reached a terminal state.
        let settled = peerbeam_chat::FileRef::new("c.bin", 5).expect("file ref");
        file_row(&settled, ChatStatus::Sent);
        // (-) An outbound file row, but claimed by a *receiving* transfer.
        let wrong_way = peerbeam_chat::FileRef::new("d.bin", 6).expect("file ref");
        file_row(&wrong_way, ChatStatus::Transferring);

        BRIDGE_EVENTS.lock().unwrap().clear();
        crate::events::set_callback(Some(collect_bridge));
        // (-) An ordinary transfer to the same peer: same conversation, no row.
        mgr.chat_settle(
            &active_for(&mgr, "tx-4242-0", "sending", &peer.0),
            ChatStatus::Sent,
            None,
        );
        // (-) A transfer to a peer with no conversation at all.
        mgr.chat_settle(
            &active_for(&mgr, "tx-4242-1", "sending", "pb-nobody"),
            ChatStatus::Sent,
            None,
        );
        // (-) A peer-chosen id that happens to name our own text message.
        mgr.chat_settle(
            &active_for(&mgr, &text.id, "receiving", &peer.0),
            ChatStatus::Received,
            None,
        );
        // (-) A peer-chosen id that names an already-settled file row.
        mgr.chat_settle(
            &active_for(&mgr, &settled.id, "receiving", &peer.0),
            ChatStatus::Received,
            None,
        );
        // (-) Right row, wrong direction.
        mgr.chat_settle(
            &active_for(&mgr, &wrong_way.id, "receiving", &peer.0),
            ChatStatus::Received,
            None,
        );
        // (+) The two genuine ones.
        mgr.chat_settle(
            &active_for(&mgr, &in_flight.id, "sending", &peer.0),
            ChatStatus::Sent,
            None,
        );
        mgr.chat_settle(
            &active_for(&mgr, &offered.id, "receiving", &peer.0),
            ChatStatus::Received,
            None,
        );
        crate::events::set_callback(None);

        let events = BRIDGE_EVENTS.lock().unwrap().clone();
        let statuses: Vec<&Value> = events
            .iter()
            .filter(|e| e["type"] == "chat_status")
            .collect();
        assert_eq!(
            statuses.len(),
            2,
            "only a transfer that owns an in-flight row may announce one: {events:?}"
        );
        let announced: Vec<&Value> = statuses.iter().map(|e| &e["message_id"]).collect();
        assert!(
            announced.iter().any(|m| **m == in_flight.id)
                && announced.iter().any(|m| **m == offered.id),
            "the two genuine rows are the ones announced: {announced:?}"
        );

        let row = |id: &str| chat.get(&peer, id).expect("get").expect("row present");
        // The genuine rows moved.
        assert_eq!(row(&in_flight.id).status, ChatStatus::Sent);
        assert_eq!(row(&offered.id).status, ChatStatus::Received);
        // Nothing else did — and the text row is still text, with its body.
        let untouched = row(&text.id);
        assert_eq!(untouched.status, ChatStatus::Sent, "text row not restamped");
        assert_eq!(untouched.kind, ChatKind::Text);
        assert_eq!(untouched.body, "hello there");
        assert_eq!(
            row(&settled.id).status,
            ChatStatus::Sent,
            "a settled file row is final"
        );
        assert_eq!(
            row(&wrong_way.id).status,
            ChatStatus::Transferring,
            "a receiving transfer may not settle an outbound row"
        );

        assert_eq!(
            chat.history(&peer).expect("history").len(),
            5,
            "no phantoms"
        );
        assert!(
            chat.history(&DeviceId::from("pb-nobody"))
                .expect("history")
                .is_empty(),
            "an unrelated conversation must not be created"
        );
    }

    /// A failure reason rides the event so a surface can explain itself, and it
    /// is absent when there is nothing to explain.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_settle_carries_a_reason_only_when_there_is_one() {
        let (mgr, chat, _dir) = test_manager_full("bridge-reason", 0);
        let peer = DeviceId::from("pb-bob");
        let r = peerbeam_chat::FileRef::new("a.bin", 3).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &peer,
            &r,
            file_meta(&r),
            ChatStatus::Transferring,
        ))
        .expect("seed the file row");

        BRIDGE_EVENTS.lock().unwrap().clear();
        crate::events::set_callback(Some(collect_bridge));
        mgr.chat_settle(
            &active_for(&mgr, &r.id, "sending", &peer.0),
            ChatStatus::Failed,
            Some("peer went away"),
        );
        crate::events::set_callback(None);

        let events = BRIDGE_EVENTS.lock().unwrap().clone();
        let ev = events
            .iter()
            .find(|e| e["type"] == "chat_status")
            .unwrap_or_else(|| panic!("no chat_status: {events:?}"));
        assert_eq!(ev["status"], "failed");
        assert_eq!(ev["error"], "peer went away");
        assert_eq!(
            chat.history(&peer).expect("history")[0].status,
            ChatStatus::Failed
        );
    }

    /// A completed *incoming* chat file must record where it landed, or the
    /// receiver's bubble has no path to open. The saved path is already computed
    /// by `record` for the event payload; this is the same value reaching the
    /// row. Written before the status flips, since a settled row is closed to
    /// further writes — a check-then-write in the wrong order would silently
    /// drop the path.
    ///
    /// The negative half matters just as much: an ordinary (non-chat) transfer
    /// completing must write nothing, so a peer cannot use a completion to point
    /// an unrelated row at a file of its choosing.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_received_chat_file_records_where_it_landed() {
        let (mgr, chat, dir) = test_manager_full("recv-path", 0);
        let peer = DeviceId::from("pb-bob");

        // The row the peer's FileRef created: In/File/PendingApproval.
        let r = peerbeam_chat::FileRef::new("report.pdf", 4096).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_in(&peer, &r))
            .expect("seed the offered row");
        // Our own outbound text, as a bystander an ordinary transfer must not touch.
        let text = peerbeam_chat::ChatMessage::new("unrelated").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &text))
            .expect("seed the text row");

        // The receive completes. `record` derives the saved path from the save
        // directory + the received file name (no explicit `Active::path`).
        let active = mgr
            .register_vacant(&r.id, "receiving", "bob", &peer.0, "report.pdf", None)
            .expect("id vacant");
        mgr.record(&active, true, "transfer_completed", json!({}));

        let row = chat.get(&peer, &r.id).expect("get").expect("row present");
        assert_eq!(row.status, ChatStatus::Received);
        let expected = dir.path().join("report.pdf").to_string_lossy().into_owned();
        assert_eq!(
            row.file.expect("file meta").local_path,
            Some(expected),
            "the received file's saved path must reach the row"
        );

        // An ordinary transfer completing writes nothing at all.
        let plain = mgr
            .register_vacant("tx-9999-0", "receiving", "bob", &peer.0, "other.bin", None)
            .expect("id vacant");
        mgr.record(&plain, true, "transfer_completed", json!({}));
        let bystander = chat
            .get(&peer, &text.id)
            .expect("get")
            .expect("row present");
        assert_eq!(bystander.status, ChatStatus::Sent);
        assert!(bystander.file.is_none());
        assert_eq!(
            chat.history(&peer).expect("history").len(),
            2,
            "no row invented for a plain transfer"
        );
    }

    /// A receiving row must not sit at `PendingApproval` for the whole
    /// download. Nothing wrote `Transferring` for a receive before this: the
    /// bubble showed "Wants to send you this" with live-looking Accept / Trust
    /// / Decline controls — for a decision already made, and under auto-accept
    /// one that was never asked and could not be revoked (the `wait_for_accept`
    /// short-circuit leaves no `pending` entry for Decline to resolve) — while
    /// the progress bar, gated on `transferring`, stayed dead.
    ///
    /// The second half is what makes the fix safe: `Transferring` is inside the
    /// guard's writable set, so the completion still lands. A status that
    /// closed the row would have traded a stuck prompt for a lost outcome.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_receiving_row_goes_transferring_and_still_settles_received_after() {
        let (mgr, chat, dir) = test_manager_full("recv-transferring", 0);
        let peer = DeviceId::from("pb-bob");
        let r = peerbeam_chat::FileRef::new("report.pdf", 4096).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_in(&peer, &r))
            .expect("seed the offered row");

        let active = mgr
            .register_vacant(&r.id, "receiving", "bob", &peer.0, "report.pdf", None)
            .expect("id vacant");

        // `handle_incoming`, immediately after the approval gate clears.
        mgr.chat_settle(&active, ChatStatus::Transferring, None);
        let row = chat.get(&peer, &r.id).expect("get").expect("row");
        assert_eq!(
            row.status,
            ChatStatus::Transferring,
            "the row must stop claiming it is awaiting a decision"
        );

        // …and the completion is still able to write to it.
        mgr.record(&active, true, "transfer_completed", json!({}));
        let row = chat.get(&peer, &r.id).expect("get").expect("row");
        assert_eq!(row.status, ChatStatus::Received);
        let expected = dir.path().join("report.pdf").to_string_lossy().into_owned();
        assert_eq!(
            row.file.expect("file meta").local_path,
            Some(expected),
            "going Transferring first must not close the row to its own completion"
        );
    }

    /// **The mismatch.** The row's name and size come from the peer's
    /// CHAT-channel `FileRef`; the bytes come from a TRANSFER stream whose own
    /// `TransferMeta` decides what is written. They are correlated by id alone
    /// and nothing ever checked one against the other, so a paired peer could
    /// leave a row permanently reading `holiday.jpg · 180 KB · Received` whose
    /// tap-to-open handed the OS `invoice-2026.pdf.exe`.
    ///
    /// Both write points are exercised: the pre-approval reconcile against the
    /// peeked preview (the moment the user actually decides) and the
    /// settle-time write of what genuinely landed.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_receiving_row_is_relabelled_with_what_the_transfer_actually_lands() {
        let (mgr, chat, dir) = test_manager_full("recv-mismatch", 0);
        let peer = DeviceId::from("pb-bob");

        // What the peer put in the conversation.
        let offered = peerbeam_chat::FileRef::new("holiday.jpg", 184_320).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_in(&peer, &offered))
            .expect("seed the offered row");
        let name_of = |chat: &ChatStore| {
            chat.get(&peer, &offered.id)
                .expect("get")
                .expect("row")
                .file
                .expect("file meta")
        };
        assert_eq!(name_of(&chat).name, "holiday.jpg", "the claim, as offered");

        // What the transfer says is arriving (the peeked `TransferMeta`).
        let active = mgr
            .register_vacant(
                &offered.id,
                "receiving",
                "bob",
                &peer.0,
                "invoice-2026.pdf.exe",
                None,
            )
            .expect("id vacant");
        mgr.chat_set_landing(&active, "invoice-2026.pdf.exe", 4_096);

        let meta = name_of(&chat);
        assert_eq!(
            meta.name, "invoice-2026.pdf.exe",
            "the user must approve the name that will land, not the one advertised"
        );
        assert_eq!(meta.size, 4_096);
        assert_eq!(
            chat.get(&peer, &offered.id)
                .expect("get")
                .expect("row")
                .status,
            ChatStatus::PendingApproval,
            "correcting the metadata must not itself decide the approval"
        );

        // The receive completes: 4096 bytes of it.
        active.stats.lock().unwrap().update(4_096, 4_096);
        *active.file.lock().unwrap() = "invoice-2026.pdf.exe".into();
        mgr.record(&active, true, "transfer_completed", json!({}));

        let row = chat.get(&peer, &offered.id).expect("get").expect("row");
        assert_eq!(row.status, ChatStatus::Received);
        let meta = row.file.expect("file meta");
        assert_eq!(meta.name, "invoice-2026.pdf.exe");
        assert_eq!(meta.size, 4_096);
        assert_eq!(
            meta.local_path,
            Some(
                dir.path()
                    .join("invoice-2026.pdf.exe")
                    .to_string_lossy()
                    .into_owned()
            ),
            "the label and the open target must name the same file"
        );
    }

    /// The pre-approval reconcile is best-effort: `peek_incoming_meta` is
    /// explicitly fail-soft, and a slow, closed or undecodable first frame
    /// yields an empty preview — which `chat_set_landing` correctly declines to
    /// write, since a blanked row would be worse than a wrong one. The
    /// settle-time write is what covers that case, and this is the test that
    /// makes it load-bearing rather than merely belt-and-braces: nothing has
    /// corrected the row before the completion here.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_row_whose_peek_learned_nothing_is_still_corrected_when_it_lands() {
        let (mgr, chat, _dir) = test_manager_full("recv-blind-peek", 0);
        let peer = DeviceId::from("pb-bob");
        let offered = peerbeam_chat::FileRef::new("holiday.jpg", 184_320).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_in(&peer, &offered))
            .expect("seed");

        // `handle_incoming` with an empty preview: a locally minted id would
        // normally follow, but a peer can also supply a valid id and a first
        // frame the peek cannot decode. Either way, nothing is learned.
        let active = mgr
            .register_vacant(&offered.id, "receiving", "bob", &peer.0, "(incoming)", None)
            .expect("id vacant");
        mgr.chat_set_landing(&active, "", 0);
        assert_eq!(
            chat.get(&peer, &offered.id)
                .expect("get")
                .expect("row")
                .file
                .expect("file meta")
                .name,
            "holiday.jpg",
            "an empty preview must never blank the row"
        );

        // The receive completes, and only now is the real name known.
        active.stats.lock().unwrap().update(4_096, 4_096);
        *active.file.lock().unwrap() = "invoice-2026.pdf.exe".into();
        mgr.record(&active, true, "transfer_completed", json!({}));

        let meta = chat
            .get(&peer, &offered.id)
            .expect("get")
            .expect("row")
            .file
            .expect("file meta");
        assert_eq!(
            meta.name, "invoice-2026.pdf.exe",
            "the settled row must name what landed even when the peek learned nothing"
        );
        assert_eq!(meta.size, 4_096);
    }

    /// The landing write is a write like any other, so it carries the same
    /// guard: it must not relabel a text row, an already-settled row, or one
    /// belonging to the other direction. Without this a peer-supplied transfer
    /// id would be a rename primitive aimed at any row in the conversation.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_set_landing_is_silent_for_a_row_that_is_not_the_transfers_own() {
        let (mgr, chat, _dir) = test_manager_full("landing-guard", 0);
        let peer = DeviceId::from("pb-bob");

        let text = peerbeam_chat::ChatMessage::new("hello there").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &text))
            .expect("seed");
        mgr.chat_set_landing(
            &active_for(&mgr, &text.id, "receiving", &peer.0),
            "evil.exe",
            1,
        );
        let row = chat.get(&peer, &text.id).expect("get").expect("row");
        assert_eq!(row.kind, ChatKind::Text);
        assert_eq!(row.body, "hello there");
        assert!(row.file.is_none(), "no file metadata conjured onto text");

        // Our own outbound file share, reached for by a *receiving* transfer.
        let mine = peerbeam_chat::FileRef::new("mine.pdf", 10).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &peer,
            &mine,
            file_meta(&mine),
            ChatStatus::Transferring,
        ))
        .expect("seed");
        mgr.chat_set_landing(
            &active_for(&mgr, &mine.id, "receiving", &peer.0),
            "evil.exe",
            1,
        );
        assert_eq!(
            chat.get(&peer, &mine.id)
                .expect("get")
                .expect("row")
                .file
                .expect("file meta")
                .name,
            "mine.pdf",
            "a receive must not rename our own outbound row"
        );
    }

    /// The event vocabulary must be the record's own serde spelling: a surface
    /// applies an event's `status` straight onto a row it read from
    /// `pb_chat_history`, so a second spelling would make the row disagree with
    /// itself.
    #[test]
    fn chat_status_str_matches_the_records_serialized_form() {
        for s in [
            ChatStatus::Pending,
            ChatStatus::Sent,
            ChatStatus::Received,
            ChatStatus::PendingApproval,
            ChatStatus::Transferring,
            ChatStatus::Declined,
            ChatStatus::Failed,
            ChatStatus::Interrupted,
            ChatStatus::Staging,
        ] {
            let serialized = serde_json::to_value(s).expect("status serializes");
            assert_eq!(
                serialized.as_str(),
                Some(chat_status_str(s)),
                "event spelling drifted from the record's for {s:?}"
            );
        }
    }

    /// `chat_send_file` must refuse before it persists anything: a bad path
    /// leaves no row, so a caller never sees a `Transferring` row for a file
    /// that was never staged.
    #[tokio::test]
    async fn chat_send_file_refuses_a_bad_path_without_persisting_a_row() {
        let (mgr, chat, dir) = test_manager_full("bad-path", 0);
        let mgr = Arc::new(mgr);
        let peer = json!({
            "id": "pb-bob", "name": "bob", "addresses": ["127.0.0.1"], "port": 1,
        });

        let missing = dir.path().join("does-not-exist.bin");
        let err = mgr
            .chat_send_file(&json!({ "peer": peer, "path": missing.to_string_lossy() }))
            .expect_err("a missing path must be refused");
        assert_eq!(err.0.as_str(), Code::InvalidArgument.as_str());
        assert!(err.1.contains("cannot read"), "unexpected error: {}", err.1);

        let folder = dir.path().join("a-folder");
        std::fs::create_dir_all(&folder).expect("mkdir");
        let err = mgr
            .chat_send_file(&json!({ "peer": peer, "path": folder.to_string_lossy() }))
            .expect_err("a directory must be refused");
        assert!(
            err.1.contains("folders aren't supported"),
            "unexpected error: {}",
            err.1
        );

        assert!(
            chat.history(&DeviceId::from("pb-bob"))
                .expect("history")
                .is_empty(),
            "a refused share must persist nothing"
        );
        assert_eq!(mgr.active_len(), 0, "and register no transfer");
    }

    /// The thread-open reconcile. A file row is only ever moved off
    /// `Transferring`/`PendingApproval` by a live transfer event, and transfer
    /// ids are process-scoped with nothing replaying them — so a row that
    /// survived a crash in either state would spin forever, and an inbound one
    /// would keep offering an Accept button whose transfer no longer exists.
    ///
    /// Startup reconciliation cannot reach these: it can only enumerate peers
    /// with queued *text*, and a file-only thread has none. This is the entry
    /// point that settles them, and it must leave everything else alone.
    #[tokio::test]
    async fn chat_reconcile_settles_only_the_rows_nothing_will_ever_finish() {
        let (mgr, chat, _dir) = test_manager_full("reconcile", 0);
        let peer = DeviceId::from("pb-bob");

        // (+) Our own send, left mid-flight by a restart.
        let stranded = peerbeam_chat::FileRef::new("a.bin", 3).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &peer,
            &stranded,
            file_meta(&stranded),
            ChatStatus::Transferring,
        ))
        .expect("seed");
        // (+) An offer whose approval prompt died with the process.
        let offered = peerbeam_chat::FileRef::new("b.bin", 4).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_in(&peer, &offered))
            .expect("seed");
        // (-) A settled file row.
        let done = peerbeam_chat::FileRef::new("c.bin", 5).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &peer,
            &done,
            file_meta(&done),
            ChatStatus::Sent,
        ))
        .expect("seed");
        // (-) Ordinary text.
        let text = peerbeam_chat::ChatMessage::new("hello there").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &text))
            .expect("seed");

        let out = mgr
            .chat_reconcile(&json!({ "peer_id": peer.0 }))
            .expect("reconcile");
        assert_eq!(out["changed"], 2);

        let status_of = |id: &str| {
            chat.get(&peer, id)
                .expect("read")
                .expect("row present")
                .status
        };
        assert_eq!(status_of(&stranded.id), ChatStatus::Interrupted);
        assert_eq!(status_of(&offered.id), ChatStatus::Interrupted);
        assert_eq!(
            status_of(&done.id),
            ChatStatus::Sent,
            "settled rows are final"
        );
        assert_eq!(
            status_of(&text.id),
            ChatStatus::Sent,
            "text is never a transfer's business"
        );

        // Idempotent: a second pass has nothing left to settle.
        let again = mgr
            .chat_reconcile(&json!({ "peer_id": peer.0 }))
            .expect("reconcile");
        assert_eq!(again["changed"], 0);
    }

    /// The reason this is not `reconcile_peer` and not reconcile-on-read: a
    /// thread can be opened while a share to that peer is genuinely in flight
    /// (attach a file, navigate away, come back). Marking that row
    /// `Interrupted` would settle a transfer that is actively moving bytes,
    /// and — because a settled row is deliberately no longer writable — its
    /// real completion would then be dropped on the floor.
    #[tokio::test]
    async fn chat_reconcile_leaves_a_row_whose_transfer_is_live_alone() {
        let (mgr, chat, _dir) = test_manager_full("reconcile-live", 0);
        let peer = DeviceId::from("pb-bob");
        let live = peerbeam_chat::FileRef::new("a.bin", 3).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &peer,
            &live,
            file_meta(&live),
            ChatStatus::Transferring,
        ))
        .expect("seed");
        // Its transfer is registered right now.
        let _active = active_for(&mgr, &live.id, "sending", &peer.0);

        let out = mgr
            .chat_reconcile(&json!({ "peer_id": peer.0 }))
            .expect("reconcile");

        assert_eq!(out["changed"], 0);
        assert_eq!(
            chat.get(&peer, &live.id)
                .expect("read")
                .expect("row present")
                .status,
            ChatStatus::Transferring,
        );
    }

    /// **The dial window, closed structurally.** In 2a `chat_send_file` wrote
    /// the row `Transferring` before touching the network, so the seconds spent
    /// staging and dialing were seconds in which this reconcile saw a
    /// `Transferring` row with nothing in `active` and wrote `Interrupted` — a
    /// status outside the writable set, so the real `Sent` was afterwards
    /// dropped and a file that transferred perfectly read "Interrupted" forever
    /// while the receiver's row said `Received`. Backing out of a thread and
    /// re-entering was enough to fire it, and the UI reconciles on every open.
    /// 2a closed it by claiming the id in `active` synchronously.
    ///
    /// 2b removes the window instead of guarding it. A queued file's row is
    /// `Staging` and then `Pending`, and **neither is a status this reconcile
    /// looks at**, so nothing can settle it while it waits — through a stage
    /// that may run for minutes on a multi-GB file, a dial, a peer that is
    /// simply offline, and any number of retries. The row only enters
    /// `Transferring` in `run_queued_file`, immediately *after* the id is
    /// claimed.
    #[tokio::test]
    async fn a_reconcile_inside_the_stage_and_dial_window_leaves_the_row_alone() {
        let (mgr, chat, dir) = test_manager_full("dial-window", 0);
        let mgr = Arc::new(mgr);
        let src = dir.path().join("report.pdf");
        std::fs::write(&src, vec![7u8; 4096]).expect("write");

        // Port 1 on loopback: nothing is listening, so the spawned dial cannot
        // succeed — the row stays in the queued state for as long as that
        // attempt takes, which is exactly the window under test.
        let out = mgr
            .chat_send_file(&json!({
                "peer": {
                    "id": "pb-bob", "name": "bob", "addresses": ["127.0.0.1"], "port": 1,
                },
                "path": src.to_string_lossy(),
            }))
            .expect("staged");
        let id = out["id"].as_str().expect("id").to_string();
        let peer = DeviceId::from("pb-bob");

        // The row is durable and visible the instant the call returns, before
        // any copying or dialing has happened.
        let before = chat.get(&peer, &id).expect("get").expect("row").status;
        assert!(
            matches!(before, ChatStatus::Staging | ChatStatus::Pending),
            "a share that has not been offered to anyone must read Staging or \
             Pending, got {before:?}"
        );

        // The user backs out of the thread and comes straight back in.
        let out = mgr
            .chat_reconcile(&json!({ "peer_id": "pb-bob" }))
            .expect("reconcile");
        assert_eq!(out["changed"], 0, "a queued share must not be reconciled");
        let after = chat.get(&peer, &id).expect("get").expect("row").status;
        assert!(
            matches!(after, ChatStatus::Staging | ChatStatus::Pending),
            "a queued share must not be marked Interrupted — that status is \
             outside the writable set, so its real outcome would be dropped; \
             got {after:?}"
        );
    }

    /// A queued file with its staged blob on disk, ready for
    /// `settle_queued_file` to decide about. Returns `(peer, id, blob_path)`.
    fn queued_file(
        mgr: &Manager,
        chat: &ChatStore,
        dir: &tempfile::TempDir,
    ) -> (DeviceId, String, String) {
        let peer = DeviceId::from("pb-bob");
        let r = peerbeam_chat::FileRef::new("a.bin", 4096).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &peer,
            &r,
            peerbeam_chat::FileMeta::new(&r.name, r.size, Some("/home/me/a.bin".into())),
            ChatStatus::Staging,
        ))
        .expect("seed the row");
        let blobs = dir.path().join("outbox-blobs");
        std::fs::create_dir_all(&blobs).expect("mkdir");
        let blob = blobs.join(&r.id);
        std::fs::write(&blob, vec![7u8; 4096]).expect("write blob");
        let staged = peerbeam_chat::StagedFile {
            name: "a.bin".into(),
            size: 4096,
            staged_path: blob.to_string_lossy().into_owned(),
        };
        chat.enqueue_file(&peer, &r, &staged).expect("queue it");
        let _ = mgr; // the manager owns the staging store these paths live under
        (peer, r.id, staged.staged_path)
    }

    /// A leg that failed with nothing moved and no local fault — what an offer
    /// refused (or unanswered) at the peer's approval gate looks like from the
    /// sending side, and the only shape the backstop counts.
    fn gate_refusal() -> LegOutcome {
        LegOutcome::Failed {
            bytes_moved: 0,
            local_fault: false,
        }
    }

    /// **The backstop's counting rule, in one test.** What may be counted
    /// against a queued file is narrow, and getting it wrong in either
    /// direction is a real harm: too loose and a flapping link drops a file
    /// nobody ever declined; too tight and a peer that keeps refusing is
    /// re-prompted forever.
    ///
    /// Two failures that are NOT refusals, and neither may be counted:
    ///
    /// * bytes demonstrably moved, so the receiver had already accepted and
    ///   this is a mid-stream fault;
    /// * the failure was our own storage. `send_file` opens the source only
    ///   *after* the receiver's `Control::ResumeAck`, so a missing or
    ///   unreadable staged blob presents as zero bytes moved even though the
    ///   peer accepted — counting it would spend a refusal credit on our disk
    ///   and, at the third, tell the user their peer refused a file it never
    ///   saw a byte of.
    #[tokio::test]
    async fn a_mid_stream_or_local_failure_never_counts_against_the_backstop() {
        for leg in [
            LegOutcome::Failed {
                bytes_moved: 1,
                local_fault: false,
            },
            LegOutcome::Failed {
                bytes_moved: 0,
                local_fault: true,
            },
        ] {
            let (mgr, chat, dir) = test_manager_full("backstop-bytes", 0);
            let (peer, id, blob) = queued_file(&mgr, &chat, &dir);

            // Ten of them. None may be counted, and the file must still be
            // queued with its bytes intact.
            for _ in 0..10 {
                mgr.settle_queued_file(&peer, &id, &blob, leg);
            }
            let queued = chat.outbox_for(&peer).expect("outbox");
            assert_eq!(queued.len(), 1, "{leg:?} must not dequeue");
            assert_eq!(
                queued[0].offers_refused, 0,
                "{leg:?} is not the peer refusing anything"
            );
            assert!(
                std::path::Path::new(&blob).exists(),
                "{leg:?} must leave the staged bytes for the retry"
            );
        }
    }

    /// The other half: three offers that died at the approval gate are counted,
    /// and the third is terminal — dequeued, blob deleted, row `Failed` with a
    /// reason that names the backstop rather than a network error.
    #[tokio::test]
    #[serial_test::serial]
    async fn three_gate_refusals_are_terminal_and_evict_the_blob() {
        let (mgr, chat, dir) = test_manager_full("backstop-count", 0);
        let (peer, id, blob) = queued_file(&mgr, &chat, &dir);
        BRIDGE_EVENTS.lock().unwrap().clear();
        crate::events::set_callback(Some(collect_bridge));

        for expected in 1..=2 {
            mgr.settle_queued_file(&peer, &id, &blob, gate_refusal());
            let queued = chat.outbox_for(&peer).expect("outbox");
            assert_eq!(queued.len(), 1, "still queued after {expected} refusal(s)");
            assert_eq!(queued[0].offers_refused, expected);
            assert!(std::path::Path::new(&blob).exists());
        }

        mgr.settle_queued_file(&peer, &id, &blob, gate_refusal());
        assert!(
            chat.outbox_for(&peer).expect("outbox").is_empty(),
            "the third refusal is terminal"
        );
        assert!(
            !std::path::Path::new(&blob).exists(),
            "a file the backstop gave up on must not keep its bytes on disk"
        );
        assert_eq!(
            chat.get(&peer, &id).expect("get").expect("row").status,
            ChatStatus::Failed
        );
        crate::events::set_callback(None);
        let events = BRIDGE_EVENTS.lock().unwrap().clone();
        let reason = events
            .iter()
            .filter(|e| e["type"] == "chat_status" && e["message_id"] == id)
            .filter_map(|e| e["error"].as_str().map(String::from))
            .next_back()
            .expect("the backstop must say why it gave up");
        assert!(
            reason.contains("could not be delivered") && reason.contains("3 attempts"),
            "the reason must name the backstop: {reason}"
        );
        assert!(
            !reason.contains("did not accept"),
            "and must not assert a refusal we cannot know: a link that dropped \
             after the receiver's ack looks identical from here — {reason}"
        );
    }

    /// When a queued file's row refuses to re-open, `run_queued_file` stops the
    /// attempt — and must then decide whether to keep the entry for a later
    /// drain or let go of it. This is that decision, which is the only part of
    /// the bail-out that is not obvious.
    ///
    /// Only two answers justify discarding a user's queued file: the row is
    /// **final** (`Sent`/`Declined` — no future drain can re-open it either), or
    /// there is **no row at all** (nothing left to settle). Everything else must
    /// keep it, including a stale `Transferring` a restart's reconcile has not
    /// reached yet, and — most importantly — a store that cannot be read, where
    /// discarding would destroy a queue entry over a transient error.
    #[tokio::test]
    async fn only_a_final_or_missing_row_lets_go_of_a_queued_file() {
        let (mgr, chat, _dir) = test_manager_full("deliverable", 0);
        let peer = DeviceId::from("pb-bob");
        let seed = |status: ChatStatus| {
            let r = peerbeam_chat::FileRef::new("a.bin", 1).expect("file ref");
            chat.append(&peerbeam_chat::ChatRecord::file_out(
                &peer,
                &r,
                file_meta(&r),
                status,
            ))
            .expect("seed");
            r.id
        };

        for keep in [
            ChatStatus::Pending,
            ChatStatus::Staging,
            ChatStatus::Failed,
            ChatStatus::Interrupted,
            ChatStatus::Transferring,
        ] {
            let id = seed(keep);
            assert!(
                mgr.row_may_still_deliver(&peer, &id),
                "{keep:?} may still deliver — its entry must be kept"
            );
        }
        for done in [ChatStatus::Sent, ChatStatus::Declined] {
            let id = seed(done);
            assert!(
                !mgr.row_may_still_deliver(&peer, &id),
                "{done:?} is final — nothing will ever deliver this entry"
            );
        }
        assert!(
            !mgr.row_may_still_deliver(&peer, "no-such-row"),
            "a queue entry pointing at nothing can never be settled"
        );
    }

    /// **A connection failure never costs a refusal credit — proven by driving
    /// real, completed drain attempts.**
    ///
    /// This is the guarantee the whole backstop is shaped around: nobody saw
    /// the offer, nobody was prompted, and keep-forever is the promise text
    /// already makes. Counting attempts instead of refusals would burn the
    /// budget during a flapping link and drop a file nobody ever declined.
    ///
    /// It is asserted here rather than end-to-end because the end-to-end shape
    /// cannot assert it honestly: a dial at a dead port does not return for
    /// `CONNECT_TIMEOUT` (8 s), so a test that queues a file for an absent peer
    /// and checks the row a moment later is asserting a state that had no
    /// opportunity to change — it passes just as happily with the guarantee
    /// deleted. So this drives `chat_flush_peer` itself, `MAX_OFFERS_REFUSED +
    /// 2` times, **awaiting each one to completion**, and only then reads the
    /// counter.
    ///
    /// The address is `0.0.0.0`, which quinn rejects synchronously
    /// (`InvalidRemoteAddress`) rather than waiting out a timeout: the branch
    /// under test is `chat_flush_peer`'s dial-failure early return, and what it
    /// is failing on is immaterial to that branch.
    #[tokio::test]
    async fn a_connection_failure_never_counts_against_the_backstop() {
        let (mgr, chat, dir) = test_manager_full("no-route", 0);
        let mgr = Arc::new(mgr);
        let (peer, id, blob) = queued_file(&mgr, &chat, &dir);
        let device = Device {
            id: peer.clone(),
            name: "bob".into(),
            device_type: DeviceType::Desktop,
            platform: peerbeam_platform::current(),
            addresses: vec!["0.0.0.0".into()],
            port: 9,
            last_seen: chrono::Utc::now(),
        };

        for attempt in 1..=(MAX_OFFERS_REFUSED + 2) {
            let flushed = mgr.chat_flush_peer(device.clone()).await;
            assert!(
                flushed.is_empty(),
                "attempt {attempt} must deliver nothing — there is no peer"
            );
        }

        let queued = chat.outbox_for(&peer).expect("outbox");
        assert_eq!(
            queued.len(),
            1,
            "a peer we could never reach must never go terminal, however many \
             attempts elapse"
        );
        assert_eq!(
            queued[0].offers_refused, 0,
            "nobody saw the offer, so nothing may be counted against it"
        );
        assert!(
            std::path::Path::new(&blob).exists(),
            "and its staged bytes must still be waiting"
        );
        assert_eq!(
            chat.get(&peer, &id).expect("get").expect("row").status,
            ChatStatus::Pending,
            "the row still reads Queued, not Failed"
        );
    }

    /// Delivery and an explicit decline are both terminal, and both must let go
    /// of the bytes: a queue that keeps a delivered file's blob is a disk leak
    /// with no upper bound.
    #[tokio::test]
    async fn delivery_and_a_decline_both_dequeue_and_evict() {
        for (leg, decline) in [
            (LegOutcome::Delivered, false),
            (LegOutcome::Cancelled, false),
            (gate_refusal(), true),
        ] {
            let (mgr, chat, dir) = test_manager_full("terminal", 0);
            let (peer, id, blob) = queued_file(&mgr, &chat, &dir);
            if decline {
                // What a `FileDecline` arriving mid-flight leaves behind: the
                // handler settled our row, and the leg then failed.
                chat.set_status(&peer, &id, ChatStatus::Declined)
                    .expect("settle declined");
            }
            mgr.settle_queued_file(&peer, &id, &blob, leg);
            assert!(
                chat.outbox_for(&peer).expect("outbox").is_empty(),
                "{leg:?} (declined={decline}) must dequeue"
            );
            assert!(
                !std::path::Path::new(&blob).exists(),
                "{leg:?} (declined={decline}) must delete the staged blob"
            );
        }
    }

    #[tokio::test]
    async fn chat_reconcile_requires_a_peer_id() {
        let mgr = test_manager("reconcile-args");
        let err = mgr
            .chat_reconcile(&json!({}))
            .expect_err("peer_id is required");
        assert_eq!(err.0.as_str(), Code::InvalidArgument.as_str());
    }

    #[tokio::test]
    async fn set_identity_name_changes_identity() {
        let mgr = test_manager("Original Name");
        assert_eq!(mgr.identity().name, "Original Name");
        // device_id/keypair stay stable across a rename.
        let before = mgr.identity();

        mgr.set_identity_name("Renamed Device".to_string());

        let after = mgr.identity();
        assert_eq!(after.name, "Renamed Device");
        assert_eq!(after.device_id, before.device_id);
        assert_eq!(after.keypair.public.0, before.keypair.public.0);
    }

    // ── wait_for_accept: the ghost-transfer leak fix ─────────────
    //
    // `handle_incoming` registers the transfer (counted in `active`) *before*
    // the user decides. These tests exercise `wait_for_accept` directly —
    // the extracted decision-wait — without a real QUIC handshake, proving
    // the pending entry never outlives the decision on every exit path:
    // explicit accept, explicit reject, and (the actual bug) an unanswered
    // prompt timing out.

    /// Pin a device the way `authenticate()`'s TOFU step would, so
    /// `trust.approve` (called only on accept) has a pinned record to flip.
    fn pin(trust: &FsTrust, device: &DeviceId) {
        trust
            .record(peerbeam_domain::entity::TrustRecord {
                device: device.clone(),
                fingerprint: "test-fingerprint".into(),
                name: "peer".into(),
                trusted_at: chrono::Utc::now(),
                approved: false,
            })
            .expect("pin device");
    }

    /// Poll `pred` until it's true, yielding between attempts so other tasks
    /// on the current-thread test runtime get to run. Bounded so a broken
    /// precondition fails fast instead of hanging the test.
    async fn wait_until(mut pred: impl FnMut() -> bool) {
        for _ in 0..10_000 {
            if pred() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("condition not met in time");
    }

    #[tokio::test]
    async fn wait_for_accept_accepted_on_explicit_accept_and_leaves_trust_unapproved() {
        let mgr = Arc::new(test_manager("Device"));
        let peer_id = DeviceId::from("peer-accept");
        pin(&mgr.trust, &peer_id);
        let id = "tx-test-accept".to_string();

        let (mgr2, id2, peer2) = (mgr.clone(), id.clone(), peer_id.clone());
        let waiter = tokio::spawn(async move { mgr2.wait_for_accept(&id2, &peer2).await });

        wait_until(|| mgr.pending.lock().unwrap().contains_key(&id)).await;
        mgr.accept(&id).expect("accept should find the pending id");

        assert_eq!(
            waiter.await.expect("task join"),
            AcceptOutcome::Accepted,
            "explicit accept -> Accepted"
        );
        assert!(
            !mgr.pending.lock().unwrap().contains_key(&id),
            "pending entry must be removed after the decision"
        );
        assert!(
            !mgr.trust.lookup(&peer_id).unwrap().unwrap().approved,
            "a one-time accept must never approve the device for auto-accept"
        );
    }

    #[tokio::test]
    async fn wait_for_accept_accepted_on_accept_trust_and_approves_trust() {
        let mgr = Arc::new(test_manager("Device"));
        let peer_id = DeviceId::from("peer-accept-trust");
        pin(&mgr.trust, &peer_id);
        let id = "tx-test-accept-trust".to_string();

        let (mgr2, id2, peer2) = (mgr.clone(), id.clone(), peer_id.clone());
        let waiter = tokio::spawn(async move { mgr2.wait_for_accept(&id2, &peer2).await });

        wait_until(|| mgr.pending.lock().unwrap().contains_key(&id)).await;
        mgr.accept_trust(&id)
            .expect("accept_trust should find the pending id");

        assert_eq!(
            waiter.await.expect("task join"),
            AcceptOutcome::Accepted,
            "explicit accept-and-trust -> Accepted"
        );
        assert!(
            !mgr.pending.lock().unwrap().contains_key(&id),
            "pending entry must be removed after the decision"
        );
        assert!(
            mgr.trust.lookup(&peer_id).unwrap().unwrap().approved,
            "accept-and-trust records approval for future auto-accept"
        );
    }

    #[tokio::test]
    async fn wait_for_accept_rejected_on_explicit_reject_and_leaves_trust_unapproved() {
        let mgr = Arc::new(test_manager("Device"));
        let peer_id = DeviceId::from("peer-reject");
        pin(&mgr.trust, &peer_id);
        let id = "tx-test-reject".to_string();

        let (mgr2, id2, peer2) = (mgr.clone(), id.clone(), peer_id.clone());
        let waiter = tokio::spawn(async move { mgr2.wait_for_accept(&id2, &peer2).await });

        wait_until(|| mgr.pending.lock().unwrap().contains_key(&id)).await;
        mgr.reject(&id).expect("reject should find the pending id");

        assert_eq!(
            waiter.await.expect("task join"),
            AcceptOutcome::Rejected,
            "explicit reject -> Rejected, the one outcome we may report to the peer"
        );
        assert!(!mgr.pending.lock().unwrap().contains_key(&id));
        assert!(
            !mgr.trust.lookup(&peer_id).unwrap().unwrap().approved,
            "a decline must never approve the device"
        );
    }

    /// The bug this whole fix is for: nobody ever answers (dead connection,
    /// ignored prompt). Without the timeout this hangs forever with the
    /// entry still in `pending` and the transfer still counted as active —
    /// this test uses a paused virtual clock so it doesn't actually sleep
    /// 180s to prove that no longer happens.
    #[tokio::test(start_paused = true)]
    async fn wait_for_accept_times_out_when_unanswered_and_cleans_up() {
        let mgr = Arc::new(test_manager("Device"));
        let peer_id = DeviceId::from("peer-timeout");
        pin(&mgr.trust, &peer_id);
        let id = "tx-test-timeout".to_string();

        let (mgr2, id2, peer2) = (mgr.clone(), id.clone(), peer_id.clone());
        let waiter = tokio::spawn(async move { mgr2.wait_for_accept(&id2, &peer2).await });

        wait_until(|| mgr.pending.lock().unwrap().contains_key(&id)).await;

        // Nobody calls accept()/reject(); fast-forward the virtual clock
        // past the bound instead of actually waiting.
        tokio::time::advance(ACCEPT_TIMEOUT + Duration::from_millis(1)).await;

        assert_eq!(
            waiter.await.expect("task join"),
            AcceptOutcome::Unanswered,
            "an unanswered prompt must resolve to Unanswered — not hang, and \
             emphatically not read as the user having declined"
        );
        assert!(
            !mgr.pending.lock().unwrap().contains_key(&id),
            "the pending entry must not linger after a timeout"
        );
        assert!(
            !mgr.trust.lookup(&peer_id).unwrap().unwrap().approved,
            "a timeout must never approve the device"
        );
    }

    // ── should_send_decline: the I9 enforcement point ────────────
    //
    // `handle_incoming` decides here whether a refusal goes on the wire. The
    // decision is a pure function of data already in hand, so it is tested
    // directly — no QUIC, no handshake, no live session. Each of the three
    // legs gets its own test, because a refactor that drops any one of them
    // must fail something: the capability leg previously had NO coverage at
    // all (it could be replaced with `|| true` and the whole FFI suite still
    // passed), which is exactly the hole these close.

    /// A peer that negotiated the bit, as `CapabilitySet::intersect` would
    /// leave it.
    fn negotiated_with_decline() -> CapabilitySet {
        CapabilitySet::new().with(Capability::with_features(
            ChannelType::CHAT,
            CHAT_FEAT_FILEREF | CHAT_FEAT_FILEDECLINE,
        ))
    }

    /// What a 2a-era peer leaves after intersection: chat file sharing, but no
    /// decline signalling.
    fn negotiated_without_decline() -> CapabilitySet {
        CapabilitySet::new().with(Capability::with_features(
            ChannelType::CHAT,
            CHAT_FEAT_FILEREF,
        ))
    }

    /// A real on-disk `ChatStore` holding one inbound chat file row, the way a
    /// peer's `FileRef` would have left it — plus that row's peer, its id, and
    /// the tempdir backing the store.
    ///
    /// Built directly rather than via `test_manager_full`, which stands up a
    /// `QuicTransport` these tests have no use for (and which needs a tokio
    /// runtime). The whole point of `should_send_decline` is that the decision
    /// needs no session, so its tests should not need one either.
    fn store_with_chat_file_row() -> (ChatStore, DeviceId, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let enc = Arc::new(AeadCrypto::new());
        let key = peerbeam_crypto::derive_subkey(&[9u8; 32], b"peerbeam-appstore-v1");
        let appstore: Arc<dyn peerbeam_domain::port::AppStore> = Arc::new(
            peerbeam_appstore_fs::FsAppStore::open(dir.path().join("appstore"), key, enc),
        );
        let chat = ChatStore::new(appstore);
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_in(&peer, &r))
            .expect("seed row");
        let id = r.id.clone();
        (chat, peer, id, dir)
    }

    /// Leg 1 — only an EXPLICIT refusal is reportable. A prompt that timed out
    /// looks identical to the transfer (nothing moves) but must put nothing on
    /// the wire: a user who stepped away for three minutes would otherwise
    /// lose the file *and* have the sender's history assert they refused it,
    /// with no way back.
    #[test]
    fn a_decline_is_sent_only_for_an_explicit_rejection() {
        let (chat, peer, id, _dir) = store_with_chat_file_row();
        let caps = negotiated_with_decline();

        assert!(
            should_send_decline(AcceptOutcome::Rejected, &caps, &chat, &peer, &id),
            "the user said no — the sender must be told"
        );
        assert!(
            !should_send_decline(AcceptOutcome::Unanswered, &caps, &chat, &peer, &id),
            "a 180s timeout is not a decision and must fall through to the \
             sender's bounded retry backstop"
        );
        assert!(
            !should_send_decline(AcceptOutcome::Accepted, &caps, &chat, &peer, &id),
            "an accepted transfer obviously sends no decline"
        );
    }

    /// Leg 2 — I9 / MESSAGE_REGISTRY §7: capability-advertised, not assumed. A
    /// 2a-era peer ANDs the bit away and must never receive MessageType 3,
    /// even though it would skip the OPTIONAL frame harmlessly.
    #[test]
    fn a_decline_is_never_sent_to_a_peer_that_did_not_negotiate_the_bit() {
        let (chat, peer, id, _dir) = store_with_chat_file_row();

        assert!(
            !should_send_decline(
                AcceptOutcome::Rejected,
                &negotiated_without_decline(),
                &chat,
                &peer,
                &id
            ),
            "a peer that predates the feature must not be sent one"
        );
        assert!(
            !should_send_decline(
                AcceptOutcome::Rejected,
                &CapabilitySet::new(),
                &chat,
                &peer,
                &id
            ),
            "a peer with no CHAT capability at all is unsupported, not a panic"
        );
        // Same refusal, same store, same peer — only the negotiated set
        // differs, so this pins the capability leg specifically.
        assert!(should_send_decline(
            AcceptOutcome::Rejected,
            &negotiated_with_decline(),
            &chat,
            &peer,
            &id
        ));
    }

    /// Leg 3 — an ordinary (non-chat) transfer has no row in the conversation
    /// and must put nothing extra on the wire. That is the overwhelming
    /// majority of refusals.
    #[test]
    fn a_decline_is_never_sent_for_a_transfer_with_no_chat_row() {
        let (chat, peer, id, _dir) = store_with_chat_file_row();
        let caps = negotiated_with_decline();

        assert!(
            !should_send_decline(
                AcceptOutcome::Rejected,
                &caps,
                &chat,
                &peer,
                "tx-ordinary-transfer"
            ),
            "an ordinary transfer has no row in the conversation and must put \
             nothing extra on the wire"
        );
        assert!(
            !should_send_decline(
                AcceptOutcome::Rejected,
                &caps,
                &chat,
                &DeviceId::from("pb-someone-else"),
                &id
            ),
            "the row belongs to another conversation — the id alone is not it"
        );
        // Positive control: same refusal, same caps, the row's real peer + id.
        assert!(should_send_decline(
            AcceptOutcome::Rejected,
            &caps,
            &chat,
            &peer,
            &id
        ));
    }

    // ── BUG 1: daemon_running must reset when serve() exits on its own ──

    #[tokio::test]
    async fn serve_resets_daemon_running_on_bind_failure() {
        let mgr = Arc::new(test_manager("Device"));
        // Occupy a UDP port so the QUIC endpoint bind inside `serve()` fails.
        let sock = std::net::UdpSocket::bind("0.0.0.0:0").expect("bind probe socket");
        let port = sock.local_addr().unwrap().port();

        // Simulate what `start_daemon()` sets before spawning `serve()`, so
        // this test can call `serve()` directly and observe the reset.
        mgr.daemon_running.store(true, Ordering::SeqCst);
        *mgr.daemon_task.lock().unwrap() = None;

        mgr.clone().serve(port).await;

        assert!(
            !mgr.daemon_running.load(Ordering::SeqCst),
            "serve() must reset daemon_running when it exits on a bind \
             failure, or start_daemon() can never restart it"
        );
        assert!(
            mgr.daemon_task.lock().unwrap().is_none(),
            "the stale task handle must be cleared too"
        );
        drop(sock);
    }

    #[tokio::test]
    async fn start_daemon_can_restart_after_a_bind_failure_kills_it() {
        let sock = std::net::UdpSocket::bind("0.0.0.0:0").expect("bind probe socket");
        let port = sock.local_addr().unwrap().port();
        let mgr = Arc::new(test_manager_with_port("Device", port));

        // First start: the port is occupied by `sock`, so the spawned
        // `serve()` task fails to bind and exits almost immediately.
        mgr.start_daemon()
            .expect("start_daemon should accept the request");
        wait_until(|| !mgr.daemon_running.load(Ordering::SeqCst)).await;

        // Before the fix, `daemon_running` would still read `true` here,
        // permanently locking `start_daemon()` out of ever retrying.
        assert!(!mgr.daemon_status()["running"].as_bool().unwrap());

        // Free the port and restart: this must actually spawn a fresh
        // `serve()`, not be swallowed as an "already running" no-op.
        drop(sock);
        let res = mgr.start_daemon().expect("restart should succeed");
        assert!(
            res.get("already_running").is_none(),
            "must be a genuine (re)start, not a dedup no-op: {res}"
        );
        wait_until(|| mgr.daemon_running.load(Ordering::SeqCst)).await;

        let _ = mgr.stop_daemon();
    }

    // ── a stale terminal claim must not tear out its successor ──────────
    //
    // `register_vacant` stops two live transfers holding one id at the same
    // instant. It says nothing about the id being used AGAIN LATER — which a
    // wire-supplied id makes ordinary: `cancel()` frees the slot at once, but
    // a receive task keeps running (only the send paths populate
    // `Active::task`, so nothing aborts it) and unwinds seconds later through
    // `session.close()`. A second transfer can register under the same id in
    // that window, and a retried chat file re-uses its `FileRef` id by design.
    //
    // These tests pin that the late unwind no-ops instead of removing,
    // stamping, and announcing the *successor*. Each asserts `!Arc::ptr_eq`
    // first so it cannot pass vacuously by both handles being the same entry.

    /// Build the exact hijack window: transfer 1 registered under `id`, the
    /// user cancels it (slot freed, task still running), transfer 2 claims the
    /// now-vacant id. Returns both handles, distinctness already asserted.
    fn contested(mgr: &Manager, id: &str) -> (Arc<Active>, Arc<Active>) {
        let first = mgr
            .register_vacant(id, "receiving", "mallory", "pb-mallory", "one.bin", None)
            .expect("transfer 1 claims a vacant id");
        mgr.cancel(id).expect("the user cancels transfer 1");
        let second = mgr
            .register_vacant(id, "receiving", "victim", "pb-victim", "two.bin", None)
            .expect("the id is vacant while transfer 1 is still unwinding");
        assert!(
            !Arc::ptr_eq(&first, &second),
            "the test needs two genuinely distinct transfers, or it proves nothing"
        );
        (first, second)
    }

    /// The reported hijack: transfer 1's late `finish_cancelled` must not
    /// remove transfer 2, stamp it "cancelled", or announce it. Pre-fix this
    /// removed transfer 2 outright — it vanished from the UI on a
    /// `transfer_cancelled` it never earned and could no longer be cancelled,
    /// while still writing to disk.
    #[tokio::test]
    async fn a_late_finish_cancelled_does_not_tear_out_a_reregistered_transfer() {
        let mgr = test_manager("Device");
        let id = "peer-chosen-id";
        let (first, second) = contested(&mgr, id);

        mgr.finish_cancelled(&first); // transfer 1's unwind, arriving late

        let still = mgr
            .get_active(id)
            .expect("transfer 2 must still be registered");
        assert!(
            Arc::ptr_eq(&still, &second),
            "the registry must still hold TRANSFER 2"
        );
        assert_ne!(
            *still.status.lock().unwrap(),
            "cancelled",
            "transfer 2 must not be stamped cancelled by transfer 1's unwind"
        );
        // The user can still stop it...
        assert!(
            mgr.cancel(id).is_ok(),
            "transfer 2 must remain cancellable, not \"no active transfer\""
        );
    }

    /// ...and if it is left alone, its own completion still claims: it emits
    /// `transfer_completed` and writes its history row. (`record` emits and
    /// records under the same claim, so the history row is the observable
    /// proof the event fired — the event sink itself is process-global and
    /// cannot be captured from a non-serial test.)
    #[tokio::test]
    async fn a_reregistered_transfer_still_completes_and_records_its_own_history() {
        let mgr = test_manager("Device");
        let id = "peer-chosen-id-2";
        let (first, second) = contested(&mgr, id);

        mgr.finish_cancelled(&first);
        mgr.record(&second, true, "transfer_completed", json!({}));

        let hist = mgr.history.lock().unwrap();
        assert_eq!(hist.len(), 1, "exactly transfer 2's completion is recorded");
        assert_eq!(hist[0]["id"], id);
        assert_eq!(
            hist[0]["file"], "two.bin",
            "the recorded row must be TRANSFER 2's, not transfer 1's"
        );
        assert_eq!(hist[0]["peer_id"], "pb-victim");
        assert_eq!(hist[0]["success"], true);
    }

    /// The same protection on the failure path: a late `finish_failed` must
    /// not remove the successor or write a failure row against it.
    #[tokio::test]
    async fn a_late_finish_failed_does_not_tear_out_a_reregistered_transfer() {
        let mgr = test_manager("Device");
        let id = "peer-chosen-id-3";
        let (first, second) = contested(&mgr, id);

        mgr.finish_failed(&first, (Code::Connection, "link dropped".into()));

        let still = mgr.get_active(id).expect("transfer 2 still registered");
        assert!(Arc::ptr_eq(&still, &second));
        assert!(
            mgr.history.lock().unwrap().is_empty(),
            "no failure row may be written against a transfer that did not fail"
        );
    }

    /// And on the success path: a late `record` must not claim the successor
    /// (which would emit a completion for a transfer that has not finished).
    #[tokio::test]
    async fn a_late_record_does_not_tear_out_a_reregistered_transfer() {
        let mgr = test_manager("Device");
        let id = "peer-chosen-id-4";
        let (first, second) = contested(&mgr, id);

        mgr.record(&first, true, "transfer_completed", json!({}));

        let still = mgr.get_active(id).expect("transfer 2 still registered");
        assert!(Arc::ptr_eq(&still, &second));
        assert!(
            mgr.history.lock().unwrap().is_empty(),
            "a stale completion must not record history against the successor"
        );
    }

    /// The primitive every terminal site now shares — including the inline
    /// decline branch in `handle_incoming`, whose whole removal step is this
    /// call. Claiming is by `Arc` identity, never by id string.
    #[tokio::test]
    async fn claim_removes_only_the_exact_transfer_it_was_given() {
        let mgr = test_manager("Device");
        let id = "peer-chosen-id-5";
        let (first, second) = contested(&mgr, id);

        // A stale handle claims nothing, and leaves the registry untouched.
        assert!(
            mgr.claim(&first).is_none(),
            "a transfer no longer registered must not claim the entry that replaced it"
        );
        assert!(Arc::ptr_eq(&mgr.get_active(id).unwrap(), &second));

        // The live handle claims exactly once; the second attempt is a no-op.
        let got = mgr.claim(&second).expect("the live transfer claims itself");
        assert!(Arc::ptr_eq(&got, &second));
        assert!(mgr.claim(&second).is_none(), "claiming twice must not work");
        assert!(mgr.active.lock().unwrap().is_empty());
    }

    // ── BUG 2: exactly one terminal event/history entry per transfer ────
    //
    // `cancel()` and the terminal paths (`record`/`finish_failed`/
    // `finish_cancelled`) both claim a transfer by removing it from
    // `active` — whichever removes it first is the sole emitter. These
    // tests don't need real concurrency to prove the invariant: calling
    // both paths in sequence for the same id proves the second one is a
    // documented no-op regardless of which order they land in.

    #[tokio::test]
    async fn cancel_then_finish_failed_only_the_remover_acts() {
        let mgr = test_manager("Device");
        let id = "tx-race-cancel-first";
        let a = mgr
            .register_vacant(id, "sending", "peer", "pb-peer", "file.bin", None)
            .expect("freshly registered id is vacant");

        mgr.cancel(id)
            .expect("cancel finds the freshly-registered transfer");
        assert!(mgr.active.lock().unwrap().get(id).is_none());

        // The task's own unwind races in *after* cancel already claimed
        // (removed) the entry — this must be a no-op: no second terminal
        // event/history entry for an id the UI was already told is gone.
        mgr.finish_failed(&a, (Code::Connection, "link dropped".into()));

        assert!(
            mgr.history.lock().unwrap().is_empty(),
            "a transfer already claimed by cancel() must not also record a \
             failure to history"
        );
    }

    #[tokio::test]
    async fn finish_failed_then_cancel_only_the_remover_acts() {
        let mgr = test_manager("Device");
        let id = "tx-race-finish-first";
        let a = mgr
            .register_vacant(id, "sending", "peer", "pb-peer", "file.bin", None)
            .expect("freshly registered id is vacant");

        mgr.finish_failed(&a, (Code::Connection, "link dropped".into()));
        assert_eq!(
            mgr.history.lock().unwrap().len(),
            1,
            "the winner records history"
        );

        // `cancel()` racing in after the entry is already gone must not
        // succeed, and must not touch history again.
        let res = mgr.cancel(id);
        assert!(res.is_err(), "cancel() must find nothing left to cancel");
        assert_eq!(
            mgr.history.lock().unwrap().len(),
            1,
            "a transfer already claimed by finish_failed() must not be \
             recorded twice"
        );
    }

    #[tokio::test]
    async fn record_then_cancel_only_the_remover_acts() {
        let mgr = test_manager("Device");
        let id = "tx-race-record-first";
        let a = mgr
            .register_vacant(id, "sending", "peer", "pb-peer", "file.bin", None)
            .expect("freshly registered id is vacant");

        mgr.record(&a, true, "transfer_completed", json!({}));
        assert_eq!(mgr.history.lock().unwrap().len(), 1);

        let res = mgr.cancel(id);
        assert!(res.is_err());
        assert_eq!(
            mgr.history.lock().unwrap().len(),
            1,
            "a transfer already claimed by record() must not be cancelled \
             (or recorded) again"
        );
    }

    #[tokio::test]
    async fn cancel_is_not_idempotent_a_second_cancel_errs() {
        let mgr = test_manager("Device");
        let id = "tx-double-cancel";
        mgr.register_vacant(id, "sending", "peer", "pb-peer", "file.bin", None)
            .expect("freshly registered id is vacant");

        mgr.cancel(id).expect("first cancel succeeds");
        let second = mgr.cancel(id);
        assert!(
            second.is_err(),
            "a second cancel on an already-cancelled id must not re-fire \
             the terminal event"
        );
    }

    // ── peer-supplied transfer ids may not collide with the registry ────
    //
    // An incoming transfer now registers under the id the SENDER put on the
    // wire, and a caller may pin one too. That makes the registry keyspace
    // reachable from outside this process for the first time, so these tests
    // pin the two properties that keep it safe: an occupied id is never
    // overwritten, and a freshly minted id never lands on one a peer has
    // already squatted.

    /// The hijack: a peer names an id that is already in the registry. It must
    /// be refused, and the incumbent must be left exactly as it was — if the
    /// entry were replaced, the displaced transfer would keep running with no
    /// registry entry, its terminal event would fire against the intruder's
    /// state, and a user `cancel` would hit the wrong transfer.
    #[tokio::test]
    async fn register_vacant_refuses_to_overwrite_a_claimed_id() {
        let mgr = test_manager("Device");
        let id = "tx-contested";
        let incumbent = mgr
            .register_vacant(id, "sending", "alice", "pb-alice", "mine.bin", None)
            .expect("first claim wins");

        let intruder =
            mgr.register_vacant(id, "receiving", "mallory", "pb-mallory", "theirs.bin", None);

        assert!(intruder.is_none(), "a claimed id must not be re-registered");
        let held = mgr.get_active(id).expect("incumbent still registered");
        assert!(
            Arc::ptr_eq(&held, &incumbent),
            "the registry must still hold the ORIGINAL entry, not a replacement"
        );
        assert_eq!(*held.file.lock().unwrap(), "mine.bin");
        assert_eq!(held.peer_id, "pb-alice");
    }

    /// The pre-emption: a peer squats an id our own counter has not reached
    /// yet, waiting for a local transfer to collide with it. Minting must skip
    /// straight past it instead of overwriting.
    #[tokio::test]
    async fn register_fresh_skips_an_id_a_peer_squatted_in_advance() {
        let mgr = test_manager("Device");
        // Exactly what `next_id()` will produce first for this process.
        let squatted = format!("tx-{}-0", std::process::id());
        let squatter = mgr
            .register_vacant(
                &squatted,
                "receiving",
                "mallory",
                "pb-mallory",
                "bait.bin",
                None,
            )
            .expect("the squat itself succeeds");

        let (id, _active) = mgr.register_fresh("sending", "alice", "pb-alice", "real.bin", None);

        assert_ne!(id, squatted, "minting must not land on the squatted id");
        let still_there = mgr.get_active(&squatted).expect("squatter untouched");
        assert!(
            Arc::ptr_eq(&still_there, &squatter),
            "the squatted entry must survive intact, not be silently replaced"
        );
        assert_eq!(mgr.active.lock().unwrap().len(), 2, "two distinct entries");
    }

    /// A caller pinning an id gets that id or an error — never a different one
    /// silently substituted, which would break the very correlation it asked
    /// for. (`send` validates every path before registering, so the refusal
    /// also leaves nothing half-queued.)
    #[tokio::test]
    async fn send_refuses_a_transfer_id_already_in_use() {
        let mgr = Arc::new(test_manager("Device"));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        std::fs::write(&file, b"payload").unwrap();
        let id = "shared-id";
        mgr.register_vacant(id, "sending", "alice", "pb-alice", "other.bin", None)
            .expect("occupy the id");

        let err = mgr
            .send(&json!({
                "peer": { "id": "pb-alice", "name": "alice", "addresses": ["127.0.0.1"], "port": 1234 },
                "paths": [file.to_string_lossy()],
                "transfer_id": id,
            }))
            .expect_err("a taken id must be refused");

        assert_eq!(err.0.as_str(), Code::InvalidArgument.as_str());
        assert!(err.1.contains("already in use"), "{}", err.1);
        assert_eq!(
            mgr.active.lock().unwrap().len(),
            1,
            "nothing extra was queued by the refused call"
        );
    }

    /// One pinned id cannot name several transfers.
    #[tokio::test]
    async fn send_refuses_a_transfer_id_with_multiple_paths() {
        let mgr = Arc::new(test_manager("Device"));
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();

        let err = mgr
            .send(&json!({
                "peer": { "id": "pb-alice", "name": "alice", "addresses": ["127.0.0.1"], "port": 1234 },
                "paths": [a.to_string_lossy(), b.to_string_lossy()],
                "transfer_id": "one-id",
            }))
            .expect_err("one id cannot cover two paths");
        assert_eq!(err.0.as_str(), Code::InvalidArgument.as_str());
        assert!(mgr.active.lock().unwrap().is_empty(), "nothing was queued");
    }

    /// Without a `transfer_id` the request behaves exactly as before: an id is
    /// minted per path.
    #[tokio::test]
    async fn send_without_a_transfer_id_still_mints_one_per_path() {
        let mgr = Arc::new(test_manager("Device"));
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        std::fs::write(&a, b"a").unwrap();

        let res = mgr
            .send(&json!({
                "peer": { "id": "pb-alice", "name": "alice", "addresses": ["127.0.0.1"], "port": 1234 },
                "paths": [a.to_string_lossy()],
            }))
            .expect("plain send still works");
        let ids = res["ids"].as_array().unwrap();
        assert_eq!(ids.len(), 1);
        assert!(ids[0].as_str().unwrap().starts_with("tx-"));
        // Stop the spawned send task from outliving the test's temp dir.
        let _ = mgr.cancel(ids[0].as_str().unwrap());
    }

    #[test]
    fn transfer_id_validation_accepts_real_ids_and_rejects_hostile_ones() {
        // What actually occurs in production.
        assert!(is_valid_transfer_id("tx-12345-0"));
        assert!(is_valid_transfer_id("1785559080834abcdef0123456789"));
        assert!(is_valid_transfer_id("late-send"));
        assert!(is_valid_transfer_id("a.b_c-1"));
        // Empty, over-long, path-ish, whitespace, control characters, and
        // anything else a peer might hope a consumer mishandles.
        assert!(!is_valid_transfer_id(""));
        assert!(!is_valid_transfer_id(&"x".repeat(MAX_TRANSFER_ID + 1)));
        assert!(is_valid_transfer_id(&"x".repeat(MAX_TRANSFER_ID)));
        assert!(!is_valid_transfer_id("."));
        assert!(!is_valid_transfer_id(".."));
        assert!(!is_valid_transfer_id("../../etc/passwd"));
        assert!(!is_valid_transfer_id("/abs"));
        assert!(!is_valid_transfer_id("a b"));
        assert!(!is_valid_transfer_id("a\nb"));
        assert!(!is_valid_transfer_id("a\0b"));
        assert!(!is_valid_transfer_id("naïve"));
    }

    #[test]
    fn valid_transfer_id_rejects_non_strings() {
        assert!(valid_transfer_id(&json!(42)).is_err());
        assert!(valid_transfer_id(&json!(null)).is_err());
        assert!(valid_transfer_id(&json!("ok-1")).is_ok());
    }

    // ── BUG 3: resume must reset the rate baseline, not progress ─────────

    #[test]
    fn mark_resumed_resets_rate_baseline_but_not_progress() {
        let mut s = Stats::new();
        s.update(1_000_000, 10_000_000);
        std::thread::sleep(Duration::from_millis(60));
        s.update(2_000_000, 10_000_000);
        assert!(s.current_speed > 0.0, "sanity: a rate was established");
        assert_eq!(s.last_bytes, 2_000_000);

        // A long pause elapses with no update() calls (a paused transfer
        // stops reading/writing, so nothing calls update() while paused) —
        // `last_t` goes stale relative to "now".
        std::thread::sleep(Duration::from_millis(150));

        s.mark_resumed();

        // Progress itself is untouched.
        assert_eq!(s.transferred, 2_000_000);
        assert_eq!(s.total, 10_000_000);
        // The rate baseline is fresh: `last_t` re-anchored to resume time
        // (not left dated to before the pause), `last_bytes` matches current
        // progress, and the stale EMA/ETA are cleared rather than leaking a
        // pre-pause value into the next `dto()`.
        assert!(
            s.last_t.elapsed() < Duration::from_millis(50),
            "last_t must be re-anchored to resume time, not left stale"
        );
        assert_eq!(s.last_bytes, 2_000_000);
        assert_eq!(s.current_speed, 0.0);
        assert_eq!(s.eta_secs, None);
    }

    #[test]
    fn resume_avoids_the_bogus_speed_a_missing_reset_would_produce() {
        // Two transfers frozen at the same "just paused mid-transfer" state
        // (2,000,000 / 10,000,000 bytes, no rate established yet — e.g. the
        // first chunk after a pause) built directly rather than via
        // `update()`, so the comparison isolates exactly the `last_t`/
        // `last_bytes` baseline `mark_resumed()` touches, with no EMA
        // blending against an unrelated pre-pause rate to muddy the result.
        let paused = || {
            let mut s = Stats::new();
            s.transferred = 2_000_000;
            s.total = 10_000_000;
            s.last_bytes = 2_000_000;
            s.last_t = Instant::now();
            s
        };
        let mut fixed = paused();
        let mut unfixed = paused();

        // A long pause elapses with no update() calls, as happens while
        // genuinely paused.
        std::thread::sleep(Duration::from_millis(300));
        fixed.mark_resumed(); // the fix under test: re-anchors last_t/last_bytes
                              // `unfixed` intentionally does nothing here.

        std::thread::sleep(Duration::from_millis(60));
        fixed.update(2_060_000, 10_000_000);
        unfixed.update(2_060_000, 10_000_000);

        // Same 60,000 bytes moved in the same ~60ms window post-resume, but
        // `unfixed`'s `dt` spans the full ~360ms pause too, so the same
        // bytes look like they trickled in ~6x slower — exactly the "bogus
        // near-zero speed after resume" bug.
        assert!(
            fixed.current_speed > unfixed.current_speed * 3.0,
            "fixed={} unfixed={}: without the resume reset, current_speed \
             is computed across the pause gap and reads far too low",
            fixed.current_speed,
            unfixed.current_speed
        );
    }

    // ── BUG 4: average_speed must exclude the pre-transfer approval wait ─

    #[test]
    fn average_speed_excludes_the_pre_transfer_wait() {
        let mut s = Stats::new();
        // Registration-time idle wait (e.g. the up-to-180s accept/reject
        // prompt): real time passes with nothing transferred yet.
        std::thread::sleep(Duration::from_millis(150));
        s.update(0, 10_000_000); // still nothing moved — average_started stays None
                                 // Bytes start moving now: this call sets average_started, but its
                                 // own elapsed-since-start is ~0 by construction, so it doesn't yet
                                 // show a meaningful rate.
        s.update(1_000_000, 10_000_000);
        std::thread::sleep(Duration::from_millis(60));
        s.update(7_000_000, 10_000_000);

        // If average_speed were (wrongly) measured since registration,
        // elapsed would be ~210ms giving 7,000,000/0.21 ≈ 33 MB/s. Measured
        // correctly from the first byte (~60ms), it's ≈ 116 MB/s. Assert we
        // land comfortably above the registration-baselined figure.
        let wrong_if_from_registration = 7_000_000.0 / 0.21;
        assert!(
            s.average_speed > wrong_if_from_registration * 2.0,
            "average_speed {} still looks baselined at registration, not \
             at the first byte",
            s.average_speed
        );
    }

    // ── cooperative pause: drive()'s back-channel wiring ─────────────────
    //
    // `stream::receive_file`/`folder::receive_folder` only know about
    // `Progress` (see their module docs on `signal_pause_edge`); the actual
    // raw-`u64` back-channel sentinel translation happens entirely inside
    // `drive()`. These tests exercise that translation directly with fake
    // `ProgressSink`/`ProgressSource` implementations backed by plain mpsc
    // channels, so no real QUIC link is needed.

    /// Fake `ProgressSink` (receiver side): every reported value is mirrored
    /// onto a plain channel the test can drain.
    struct ChanSink {
        tx: mpsc::UnboundedSender<u64>,
    }

    #[async_trait::async_trait]
    impl peerbeam_domain::port::ProgressSink for ChanSink {
        async fn report(&mut self, received: u64) -> DResult<()> {
            self.tx
                .send(received)
                .map_err(|_| peerbeam_domain::error::DomainError::Connection("closed".into()))
        }
    }

    /// Fake `ProgressSource` (sender side): yields whatever the test pushes
    /// onto a plain channel, `None` once it's dropped — mirroring the real
    /// QUIC uni-stream closing.
    struct ChanSource {
        rx: mpsc::UnboundedReceiver<u64>,
    }

    #[async_trait::async_trait]
    impl peerbeam_domain::port::ProgressSource for ChanSource {
        async fn recv(&mut self) -> DResult<Option<u64>> {
            Ok(self.rx.recv().await)
        }
    }

    fn test_progress(status: TransferStatus, transferred: u64, total: u64) -> Progress {
        Progress {
            transfer: TransferId::from("t-coop-pause"),
            direction: Direction::Receiving,
            status,
            total_bytes: total,
            transferred_bytes: transferred,
            speed_bps: 0.0,
            current_file: Some("f.bin".into()),
            files_completed: 0,
            files_total: 1,
            eta_secs: None,
        }
    }

    /// A receive loop's own pause edge (see `stream::receive_file`) reaches
    /// `drive()` as a `Progress{status: Paused}`/`{status: Transferring}`
    /// pair on the `ptx` channel `run` is given. The pump must translate
    /// that into `BACK_PAUSE`/`BACK_RESUME` on the peer-facing sink
    /// immediately — not throttled like ordinary byte progress — so a
    /// receiver-initiated pause reaches the sender promptly.
    #[tokio::test]
    async fn drive_translates_receiver_pause_progress_into_back_channel_sentinels() {
        let (sink_tx, mut sink_rx) = mpsc::unbounded_channel::<u64>();
        let ctrl = TransferControl::new();
        let stats = Arc::new(Mutex::new(Stats::new()));
        let file = Arc::new(Mutex::new(String::new()));
        let progress_out: Option<Box<dyn peerbeam_domain::port::ProgressSink>> =
            Some(Box::new(ChanSink { tx: sink_tx }));

        let outcome = drive(
            "t-coop-pause".into(),
            stats,
            file,
            ctrl,
            |ptx| async move {
                let _ = ptx.send(test_progress(TransferStatus::Transferring, 10, 100));
                let _ = ptx.send(test_progress(TransferStatus::Paused, 10, 100));
                // A little real time so this isn't misread as the same
                // instant as the surrounding messages.
                tokio::time::sleep(Duration::from_millis(10)).await;
                let _ = ptx.send(test_progress(TransferStatus::Transferring, 10, 100));
                let _ = ptx.send(test_progress(TransferStatus::Completed, 100, 100));
                Ok(TransferOutcome::Completed)
            },
            progress_out,
            None,
        )
        .await;
        assert_eq!(outcome.unwrap(), TransferOutcome::Completed);

        let mut mirrored = Vec::new();
        while let Ok(v) = sink_rx.try_recv() {
            mirrored.push(v);
        }
        let pause_at = mirrored.iter().position(|&v| v == BACK_PAUSE);
        let resume_at = mirrored.iter().position(|&v| v == BACK_RESUME);
        assert!(
            pause_at.is_some(),
            "expected BACK_PAUSE on the back-channel: {mirrored:?}"
        );
        assert!(
            resume_at.is_some(),
            "expected BACK_RESUME on the back-channel: {mirrored:?}"
        );
        assert!(
            pause_at.unwrap() < resume_at.unwrap(),
            "pause must precede resume: {mirrored:?}"
        );
    }

    /// A redundant `Progress{status: Paused}` (the loop-freedom guarantee:
    /// the same status repeated) must not re-signal — only the edge does.
    #[tokio::test]
    async fn drive_does_not_resignal_a_repeated_paused_status() {
        let (sink_tx, mut sink_rx) = mpsc::unbounded_channel::<u64>();
        let ctrl = TransferControl::new();
        let stats = Arc::new(Mutex::new(Stats::new()));
        let file = Arc::new(Mutex::new(String::new()));
        let progress_out: Option<Box<dyn peerbeam_domain::port::ProgressSink>> =
            Some(Box::new(ChanSink { tx: sink_tx }));

        let outcome = drive(
            "t-coop-pause-repeat".into(),
            stats,
            file,
            ctrl,
            |ptx| async move {
                let _ = ptx.send(test_progress(TransferStatus::Paused, 10, 100));
                let _ = ptx.send(test_progress(TransferStatus::Paused, 10, 100));
                let _ = ptx.send(test_progress(TransferStatus::Paused, 10, 100));
                Ok(TransferOutcome::Completed)
            },
            progress_out,
            None,
        )
        .await;
        assert_eq!(outcome.unwrap(), TransferOutcome::Completed);

        let mut mirrored = Vec::new();
        while let Ok(v) = sink_rx.try_recv() {
            mirrored.push(v);
        }
        assert_eq!(
            mirrored.iter().filter(|&&v| v == BACK_PAUSE).count(),
            1,
            "three repeated Paused statuses must send exactly one BACK_PAUSE, not one per message: {mirrored:?}"
        );
    }

    /// The sender's half: a `BACK_PAUSE`/`BACK_RESUME` sentinel arriving on
    /// the peer-facing source (the receiver's back-channel signal, read by
    /// `in_task`) must pause/resume `ctrl` — which is what actually stops
    /// the send loop, since it was handed a clone of this same control.
    #[tokio::test]
    async fn drive_pauses_and_resumes_ctrl_from_back_channel_sentinels() {
        let (src_tx, src_rx) = mpsc::unbounded_channel::<u64>();
        let ctrl = TransferControl::new();
        let ctrl_check = ctrl.clone();
        let stats = Arc::new(Mutex::new(Stats::new()));
        let file = Arc::new(Mutex::new(String::new()));
        let progress_in: Option<Box<dyn peerbeam_domain::port::ProgressSource>> =
            Some(Box::new(ChanSource { rx: src_rx }));

        let handle = tokio::spawn(drive(
            "t-coop-pause-sender".into(),
            stats,
            file,
            ctrl,
            |_ptx| async move {
                // Give the in_task time to process the sentinels below
                // before the (fake) send "completes".
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TransferOutcome::Completed)
            },
            None,
            progress_in,
        ));

        src_tx.send(BACK_PAUSE).unwrap();
        wait_until(|| ctrl_check.is_paused()).await;

        // A real byte count breaks in_task out of its first-report wait
        // (mirrors a genuine peer that both pauses and reports progress).
        src_tx.send(500).unwrap();
        src_tx.send(BACK_RESUME).unwrap();
        wait_until(|| !ctrl_check.is_paused()).await;

        drop(src_tx); // let in_task's steady-state loop see the channel close
        let outcome = handle.await.unwrap();
        assert_eq!(outcome.unwrap(), TransferOutcome::Completed);
    }

    /// No infinite frame loop: a bounded pause→resume→pause→resume cycle on
    /// the receiver's `Progress` stream must produce exactly one sentinel
    /// per edge (four sentinels for two full cycles), never more.
    #[tokio::test]
    async fn drive_bounds_sentinels_to_one_per_edge_across_multiple_cycles() {
        let (sink_tx, mut sink_rx) = mpsc::unbounded_channel::<u64>();
        let ctrl = TransferControl::new();
        let stats = Arc::new(Mutex::new(Stats::new()));
        let file = Arc::new(Mutex::new(String::new()));
        let progress_out: Option<Box<dyn peerbeam_domain::port::ProgressSink>> =
            Some(Box::new(ChanSink { tx: sink_tx }));

        let outcome = drive(
            "t-coop-pause-bounded".into(),
            stats,
            file,
            ctrl,
            |ptx| async move {
                for _ in 0..2 {
                    let _ = ptx.send(test_progress(TransferStatus::Paused, 10, 100));
                    let _ = ptx.send(test_progress(TransferStatus::Transferring, 10, 100));
                }
                Ok(TransferOutcome::Completed)
            },
            progress_out,
            None,
        )
        .await;
        assert_eq!(outcome.unwrap(), TransferOutcome::Completed);

        let mut mirrored = Vec::new();
        while let Ok(v) = sink_rx.try_recv() {
            mirrored.push(v);
        }
        assert_eq!(
            mirrored.iter().filter(|&&v| v == BACK_PAUSE).count(),
            2,
            "two pause edges must send exactly two BACK_PAUSE sentinels: {mirrored:?}"
        );
        assert_eq!(
            mirrored.iter().filter(|&&v| v == BACK_RESUME).count(),
            2,
            "two resume edges must send exactly two BACK_RESUME sentinels: {mirrored:?}"
        );
    }
}
