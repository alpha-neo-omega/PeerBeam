//! Transfer orchestration behind the FFI. Wraps the production transfer engine
//! (RouteManager + authenticate + SecureLink + send/receive) into an
//! id-addressed, event-driven manager: multiple simultaneous transfers, each a
//! background task, controlled by id, reporting progress/stats/history as
//! events. No file bytes cross FFI — only paths in, metadata/progress out.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::{
    Device, DeviceType, Direction, Progress, TransferSession, TransferStatus,
};
use peerbeam_domain::error::Result as DResult;
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{FrameKind, Link, TrustStore};
use peerbeam_engine::RouteManager;
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    authenticate, receive_file, receive_folder, send_file, send_folder, FolderSendRequest,
    Identity, PeekLink, SecureLink, SendRequest, TransferControl, TransferOutcome, BACK_PAUSE,
    BACK_RESUME,
};
use peerbeam_transfer_quic::QuicTransport;
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

// ── manager ─────────────────────────────────────────────────────

pub struct Manager {
    rm: Arc<RouteManager>,
    quic: Arc<QuicTransport>,
    enc: Arc<AeadCrypto>,
    trust: Arc<FsTrust>,
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
    pub fn send(self: &Arc<Self>, req: &Value) -> Op {
        let device = device_from(req.get("peer"))?;
        let paths = req
            .get("paths")
            .and_then(|p| p.as_array())
            .ok_or((Code::InvalidArgument, "paths[] required".into()))?;

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
            let id = self.next_id();
            let active = self.register(&id, "sending", &device.name, &name, Some(path.clone()));
            events::transfer(
                &id,
                "transfer_queued",
                json!({ "peer": device.name, "file": name }),
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
        let id = self.next_id();
        let active = self.register(&id, "sending", &device.name, &name, Some(path.clone()));
        events::transfer(
            &id,
            "transfer_queued",
            json!({ "peer": device.name, "folder": name }),
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

    fn register(
        &self,
        id: &str,
        direction: &'static str,
        peer: &str,
        file: &str,
        path: Option<String>,
    ) -> Arc<Active> {
        let active = Arc::new(Active {
            id: id.to_string(),
            direction,
            peer: peer.to_string(),
            ctrl: TransferControl::new(),
            stats: Arc::new(Mutex::new(Stats::new())),
            file: Arc::new(Mutex::new(file.to_string())),
            status: Mutex::new("queued".to_string()),
            path: Mutex::new(path),
            task: Mutex::new(None),
        });
        self.active
            .lock()
            .unwrap()
            .insert(id.to_string(), active.clone());
        active
    }

    /// Connect to the peer, retrying transient connection failures with a
    /// short backoff (Wi-Fi blips, a receiver mid-restart). Emits
    /// `transfer_retrying` per attempt; cancellation stops the retries.
    async fn connect_with_retry(
        &self,
        id: &str,
        active: &Active,
        device: &Device,
        session: &TransferSession,
    ) -> Result<Box<dyn Link>, (Code, String)> {
        const BACKOFF: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];
        let mut attempt = 0;
        loop {
            match self.rm.connect(device, session).await {
                Ok(link) => return Ok(link),
                Err(e) => {
                    let mapped = from_domain(e);
                    let transient = matches!(mapped.0, Code::Connection);
                    if !transient || attempt >= BACKOFF.len() || active.ctrl.is_cancelled() {
                        return Err(mapped);
                    }
                    let delay = BACKOFF[attempt];
                    attempt += 1;
                    *active.status.lock().unwrap() = "retrying".into();
                    events::transfer(
                        id,
                        "transfer_retrying",
                        json!({ "attempt": attempt, "delay_ms": delay.as_millis() as u64 }),
                    );
                    tokio::time::sleep(delay).await;
                    if active.ctrl.is_cancelled() {
                        return Err(mapped);
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
        let session = self.session(&id, device.id.clone(), size);

        let mut link = match self
            .connect_with_retry(&id, &active, &device, &session)
            .await
        {
            Ok(l) => l,
            Err(e) => return self.finish_failed(&id, e),
        };
        let sess = match authenticate(
            &mut *link,
            &self.identity(),
            self.enc.as_ref(),
            self.trust.as_ref(),
        )
        .await
        {
            Ok(s) => s,
            Err(e) => return self.finish_failed(&id, from_domain(e)),
        };
        events::transfer(
            &id,
            "transfer_started",
            json!({ "peer": device.name, "file": name }),
        );
        *active.status.lock().unwrap() = "transferring".into();

        // Read the peer's live progress back-channel (receiver-confirmed bytes)
        // so the bar reflects the receiver, not just bytes handed to transport.
        let peer_progress = link.progress_source();
        let mut secure = SecureLink::new(&mut *link, self.enc.as_ref(), sess);
        let req = SendRequest {
            transfer_id: id.clone(),
            name,
            path,
            size,
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
                let r = send_file(&mut secure, &storage, req, &ctrl, &ptx, 3).await;
                drop(ptx);
                r
            },
            None,
            peer_progress,
        )
        .await;
        self.finish(&id, outcome);
    }

    async fn run_send_folder(
        self: Arc<Self>,
        id: String,
        active: Arc<Active>,
        device: Device,
        path: String,
    ) {
        *active.status.lock().unwrap() = "connecting".into();
        let session = self.session(&id, device.id.clone(), 0);
        let mut link = match self
            .connect_with_retry(&id, &active, &device, &session)
            .await
        {
            Ok(l) => l,
            Err(e) => return self.finish_failed(&id, e),
        };
        let sess = match authenticate(
            &mut *link,
            &self.identity(),
            self.enc.as_ref(),
            self.trust.as_ref(),
        )
        .await
        {
            Ok(s) => s,
            Err(e) => return self.finish_failed(&id, from_domain(e)),
        };
        events::transfer(&id, "transfer_started", json!({ "peer": device.name }));
        *active.status.lock().unwrap() = "transferring".into();

        let peer_progress = link.progress_source();
        let mut secure = SecureLink::new(&mut *link, self.enc.as_ref(), sess);
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
                let r = send_folder(&mut secure, &storage, req, &ctrl, &ptx, 3).await;
                drop(ptx);
                r
            },
            None,
            peer_progress,
        )
        .await;
        self.finish(&id, outcome);
    }

    fn finish(&self, id: &str, outcome: DResult<TransferOutcome>) {
        match outcome {
            Ok(TransferOutcome::Completed) => {
                self.record(id, true, "transfer_completed", json!({}));
            }
            Ok(TransferOutcome::Cancelled) => self.finish_cancelled(id),
            Err(e) => self.finish_failed(id, from_domain(e)),
        }
    }

    /// The task observed its own cancellation (it noticed `ctrl` between
    /// chunks) and unwound with `TransferOutcome::Cancelled` — most often
    /// because `cancel()` already removed the entry and emitted
    /// `transfer_cancelled` synchronously, racing ahead of the task's own
    /// unwind. `remove` is the atomic claim: exactly one of {`cancel()`,
    /// this} ever gets `Some` back for a given id, so exactly one of them
    /// emits the terminal event. No history entry for a user cancel.
    fn finish_cancelled(&self, id: &str) {
        let Some(a) = self.active.lock().unwrap().remove(id) else {
            return;
        };
        *a.status.lock().unwrap() = "cancelled".into();
        events::transfer(id, "transfer_cancelled", json!({}));
    }

    fn finish_failed(&self, id: &str, (code, msg): (Code, String)) {
        // Claim the entry atomically: only whoever successfully removes it
        // emits the terminal event. A concurrent `cancel()` may have already
        // claimed (and removed) this id — in which case there is nothing
        // left to fail here.
        let Some(a) = self.active.lock().unwrap().remove(id) else {
            return;
        };
        *a.status.lock().unwrap() = "failed".into();
        events::transfer(
            id,
            "transfer_failed",
            json!({ "error": { "code": code.as_str(), "message": msg } }),
        );
        self.record_history(id, &a, false);
    }

    /// Success path: emit completed + append history.
    fn record(&self, id: &str, success: bool, event: &str, extra: Value) {
        // Same atomic-claim rationale as `finish_failed`: a concurrent
        // `cancel()` may have already removed this id, in which case there
        // is nothing left to record.
        let Some(a) = self.active.lock().unwrap().remove(id) else {
            return;
        };
        *a.status.lock().unwrap() = "completed".into();
        let file = a.file.lock().unwrap().clone();
        let path = a.path.lock().unwrap().clone().unwrap_or_else(|| {
            std::path::Path::new(&self.save_dir())
                .join(&file)
                .to_string_lossy()
                .into_owned()
        });
        let stats = a.stats.lock().unwrap().dto();
        let mut payload = json!({ "stats": stats, "file": file, "path": path });
        if let Value::Object(m) = &mut payload {
            if let Value::Object(e) = extra {
                m.extend(e);
            }
        }
        events::transfer(id, event, payload);
        self.record_history(id, &a, success);
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
        events::transfer(id, "transfer_paused", json!({}));
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
        events::transfer(id, "transfer_resumed", json!({}));
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
        events::transfer(id, "transfer_cancelled", json!({}));
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
        let (_local, mut incoming) = match self.quic.serve_addr_on(bind).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "receive server failed to bind");
                self.mark_daemon_stopped();
                return;
            }
        };
        while let Some(item) = incoming.next().await {
            match item {
                Ok(link) => {
                    let mgr = self.clone();
                    crate::runtime::spawn(async move { mgr.handle_incoming(link).await });
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
    async fn wait_for_accept(&self, id: &str, peer_id: &DeviceId) -> bool {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.to_string(), tx);
        let accepted = match tokio::time::timeout(ACCEPT_TIMEOUT, rx).await {
            Ok(Ok(AcceptDecision::AcceptOnce)) => true,
            Ok(Ok(AcceptDecision::AcceptAndTrust)) => {
                // Explicit accept-and-trust: this device is now approved for
                // auto-accept on future connections. Never set on a plain
                // accept, a decline, a dropped sender, or a timeout.
                let _ = self.trust.approve(peer_id);
                true
            }
            _ => false,
        };
        self.pending.lock().unwrap().remove(id);
        accepted
    }

    async fn handle_incoming(self: Arc<Self>, mut link: Box<dyn Link>) {
        let sess = match authenticate(
            &mut *link,
            &self.identity(),
            self.enc.as_ref(),
            self.trust.as_ref(),
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "incoming auth failed");
                return;
            }
        };
        let id = self.next_id();
        // Prefer the peer's human name from the handshake; fall back to the raw
        // device id only when the peer presented no name.
        let peer = {
            let n = sess.peer_name.trim();
            if n.is_empty() {
                sess.peer_id.0.clone()
            } else {
                n.to_string()
            }
        };
        let active = self.register(&id, "receiving", &peer, "(incoming)", None);
        events::transfer(
            &id,
            "transfer_queued",
            json!({ "peer": peer, "incoming": true }),
        );

        // Approval: auto-accept only peers explicitly approved by the user on
        // a prior transfer, else wait for a decision. A pinned key alone
        // (TOFU trust, MITM protection) is not consent to auto-accept — that
        // requires the user to have accepted at least once before.
        // Read the flag fresh so a live toggle applies without a restart.
        let auto = self.auto_accept.load(Ordering::SeqCst);
        let approved = self
            .trust
            .lookup(&sess.peer_id)
            .ok()
            .flatten()
            .map(|r| r.approved)
            .unwrap_or(false);
        let accepted = if auto && approved {
            true
        } else {
            self.wait_for_accept(&id, &sess.peer_id).await
        };
        if !accepted {
            events::transfer(&id, "transfer_cancelled", json!({ "reason": "rejected" }));
            self.active.lock().unwrap().remove(&id);
            let _ = link.close().await;
            return;
        }

        events::transfer(&id, "transfer_started", json!({ "peer": peer }));
        *active.status.lock().unwrap() = "transferring".into();
        // Report our received-byte progress back to the sender (best-effort).
        let peer_progress = link.progress_sink();
        let mut secure = SecureLink::new(&mut *link, self.enc.as_ref(), sess);

        // Peek the first frame to dispatch file vs folder receive (no engine
        // change — a PeekLink replays the frame the real receiver expects).
        let first = match secure.recv_frame().await {
            Ok(Some(f)) => f,
            Ok(None) => {
                return self.finish_failed(&id, (Code::Connection, "closed before data".into()))
            }
            Err(e) => return self.finish_failed(&id, from_domain(e)),
        };
        let is_folder = first.kind == FrameKind::Control;
        let save_dir = self.save_dir();
        if is_folder {
            // A folder lands as many files; default to the save dir until the
            // real root is known (set below once the receive completes).
            *active.path.lock().unwrap() = Some(save_dir.clone());
        }
        let storage = self.storage();
        let ctrl = active.ctrl.clone();
        // Filled in by the folder branch with the sanitized root name
        // `receive_folder` actually wrote under `save_dir`, so history/"open"
        // can point at the folder itself instead of its parent.
        let folder_root = Arc::new(std::sync::Mutex::new(None::<String>));

        let dest_dir = save_dir.clone();
        let folder_root_cell = folder_root.clone();
        let outcome = drive(
            id.clone(),
            active.stats.clone(),
            active.file.clone(),
            active.ctrl.clone(),
            |ptx| async move {
                let mut peek = PeekLink::new(first, &mut secure);
                let r = if is_folder {
                    receive_folder(&mut peek, &storage, &dest_dir, &ctrl, &ptx)
                        .await
                        .map(|r| {
                            *folder_root_cell.lock().unwrap() = Some(r.root);
                            r.outcome
                        })
                } else {
                    receive_file(&mut peek, &storage, &dest_dir, &ctrl, &ptx)
                        .await
                        .map(|r| r.outcome)
                };
                drop(ptx);
                r
            },
            peer_progress,
            None,
        )
        .await;
        if is_folder && matches!(outcome, Ok(TransferOutcome::Completed)) {
            if let Some(root) = folder_root.lock().unwrap().clone() {
                *active.path.lock().unwrap() =
                    Some(format!("{}/{}", save_dir.trim_end_matches('/'), root));
            }
        }
        self.finish(&id, outcome);
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
        Manager::new(
            rm,
            quic,
            enc,
            trust,
            identity,
            dir.path().to_string_lossy().into_owned(),
            false,
            1024,
            daemon_port,
            None,
        )
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
    async fn wait_for_accept_true_on_explicit_accept_and_leaves_trust_unapproved() {
        let mgr = Arc::new(test_manager("Device"));
        let peer_id = DeviceId::from("peer-accept");
        pin(&mgr.trust, &peer_id);
        let id = "tx-test-accept".to_string();

        let (mgr2, id2, peer2) = (mgr.clone(), id.clone(), peer_id.clone());
        let waiter = tokio::spawn(async move { mgr2.wait_for_accept(&id2, &peer2).await });

        wait_until(|| mgr.pending.lock().unwrap().contains_key(&id)).await;
        mgr.accept(&id).expect("accept should find the pending id");

        assert!(waiter.await.expect("task join"), "explicit accept -> true");
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
    async fn wait_for_accept_true_on_accept_trust_and_approves_trust() {
        let mgr = Arc::new(test_manager("Device"));
        let peer_id = DeviceId::from("peer-accept-trust");
        pin(&mgr.trust, &peer_id);
        let id = "tx-test-accept-trust".to_string();

        let (mgr2, id2, peer2) = (mgr.clone(), id.clone(), peer_id.clone());
        let waiter = tokio::spawn(async move { mgr2.wait_for_accept(&id2, &peer2).await });

        wait_until(|| mgr.pending.lock().unwrap().contains_key(&id)).await;
        mgr.accept_trust(&id)
            .expect("accept_trust should find the pending id");

        assert!(
            waiter.await.expect("task join"),
            "explicit accept-and-trust -> true"
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
    async fn wait_for_accept_false_on_explicit_reject_and_leaves_trust_unapproved() {
        let mgr = Arc::new(test_manager("Device"));
        let peer_id = DeviceId::from("peer-reject");
        pin(&mgr.trust, &peer_id);
        let id = "tx-test-reject".to_string();

        let (mgr2, id2, peer2) = (mgr.clone(), id.clone(), peer_id.clone());
        let waiter = tokio::spawn(async move { mgr2.wait_for_accept(&id2, &peer2).await });

        wait_until(|| mgr.pending.lock().unwrap().contains_key(&id)).await;
        mgr.reject(&id).expect("reject should find the pending id");

        assert!(
            !waiter.await.expect("task join"),
            "explicit reject -> false"
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

        assert!(
            !waiter.await.expect("task join"),
            "an unanswered prompt must resolve to false, not hang forever"
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
        mgr.register(id, "sending", "peer", "file.bin", None);

        mgr.cancel(id)
            .expect("cancel finds the freshly-registered transfer");
        assert!(mgr.active.lock().unwrap().get(id).is_none());

        // The task's own unwind races in *after* cancel already claimed
        // (removed) the entry — this must be a no-op: no second terminal
        // event/history entry for an id the UI was already told is gone.
        mgr.finish_failed(id, (Code::Connection, "link dropped".into()));

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
        mgr.register(id, "sending", "peer", "file.bin", None);

        mgr.finish_failed(id, (Code::Connection, "link dropped".into()));
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
        mgr.register(id, "sending", "peer", "file.bin", None);

        mgr.record(id, true, "transfer_completed", json!({}));
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
        mgr.register(id, "sending", "peer", "file.bin", None);

        mgr.cancel(id).expect("first cancel succeeds");
        let second = mgr.cancel(id);
        assert!(
            second.is_err(),
            "a second cancel on an already-cancelled id must not re-fire \
             the terminal event"
        );
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
