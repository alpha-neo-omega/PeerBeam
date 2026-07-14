//! Transfer orchestration behind the FFI. Wraps the production transfer engine
//! (RouteManager + authenticate + SecureLink + send/receive) into an
//! id-addressed, event-driven manager: multiple simultaneous transfers, each a
//! background task, controlled by id, reporting progress/stats/history as
//! events. No file bytes cross FFI — only paths in, metadata/progress out.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::{
    Device, DeviceType, Direction, Progress, TransferSession, TransferStatus,
};
use peerbeam_domain::error::Result as DResult;
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{Frame, FrameKind, Link};
use peerbeam_engine::RouteManager;
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    authenticate, receive_file, receive_folder, send_file, send_folder, FolderSendRequest,
    Identity, SecureLink, SendRequest, TransferControl, TransferOutcome,
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
    started: Instant,
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
            started: now,
            last_t: now,
            last_bytes: 0,
        }
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
        let elapsed = now.duration_since(self.started).as_secs_f64();
        self.average_speed = if elapsed > 0.0 {
            transferred as f64 / elapsed
        } else {
            0.0
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

// ── manager ─────────────────────────────────────────────────────

pub struct Manager {
    rm: Arc<RouteManager>,
    quic: Arc<QuicTransport>,
    enc: Arc<AeadCrypto>,
    trust: Arc<FsTrust>,
    identity: Identity,
    save_dir: String,
    auto_accept: bool,
    chunk_size: u32,
    daemon_port: u16,
    active: Mutex<HashMap<String, Arc<Active>>>,
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
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
        Manager {
            rm,
            quic,
            enc,
            trust,
            identity,
            save_dir,
            auto_accept,
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
        self.daemon_running.store(false, Ordering::SeqCst);
        daemon_event("daemon_stopped", self.daemon_port);
        Ok(json!({ "running": false }))
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
            &self.identity,
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
            &self.identity,
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
            Ok(TransferOutcome::Cancelled) => {
                self.set_status(id, "cancelled");
                events::transfer(id, "transfer_cancelled", json!({}));
                self.active.lock().unwrap().remove(id);
            }
            Err(e) => self.finish_failed(id, from_domain(e)),
        }
    }

    fn finish_failed(&self, id: &str, (code, msg): (Code, String)) {
        self.set_status(id, "failed");
        events::transfer(
            id,
            "transfer_failed",
            json!({ "error": { "code": code.as_str(), "message": msg } }),
        );
        self.record_history(id, false);
        self.active.lock().unwrap().remove(id);
    }

    /// Success path: emit completed + append history.
    fn record(&self, id: &str, success: bool, event: &str, extra: Value) {
        self.set_status(id, "completed");
        let (stats, file, path) = {
            let active = self.active.lock().unwrap();
            match active.get(id) {
                Some(a) => {
                    let file = a.file.lock().unwrap().clone();
                    let path = a.path.lock().unwrap().clone().unwrap_or_else(|| {
                        std::path::Path::new(&self.save_dir)
                            .join(&file)
                            .to_string_lossy()
                            .into_owned()
                    });
                    (a.stats.lock().unwrap().dto(), file, path)
                }
                None => (json!({}), String::new(), String::new()),
            }
        };
        let mut payload = json!({ "stats": stats, "file": file, "path": path });
        if let Value::Object(m) = &mut payload {
            if let Value::Object(e) = extra {
                m.extend(e);
            }
        }
        events::transfer(id, event, payload);
        self.record_history(id, success);
        self.active.lock().unwrap().remove(id);
    }

    fn record_history(&self, id: &str, success: bool) {
        let entry = {
            let active = self.active.lock().unwrap();
            let Some(a) = active.get(id) else { return };
            let file = a.file.lock().unwrap().clone();
            // Local path of the item: explicit when known (sends, folder
            // receives); otherwise a received file's final location under the
            // save directory.
            let path = a.path.lock().unwrap().clone().unwrap_or_else(|| {
                std::path::Path::new(&self.save_dir)
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

    fn set_status(&self, id: &str, status: &str) {
        if let Some(a) = self.active.lock().unwrap().get(id) {
            *a.status.lock().unwrap() = status.to_string();
        }
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
        *a.status.lock().unwrap() = "transferring".into();
        events::transfer(id, "transfer_resumed", json!({}));
        Ok(json!({ "resumed": true }))
    }

    pub fn cancel(&self, id: &str) -> Op {
        let a = self.get_active(id)?;
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
            let _ = tx.send(false);
        }
        // An aborted task won't run finish(); do the cleanup + notify here.
        *a.status.lock().unwrap() = "cancelled".into();
        events::transfer(id, "transfer_cancelled", json!({}));
        self.active.lock().unwrap().remove(id);
        Ok(json!({ "cancelled": true }))
    }

    pub fn accept(&self, id: &str) -> Op {
        match self.pending.lock().unwrap().remove(id) {
            Some(tx) => {
                let _ = tx.send(true);
                Ok(json!({ "accepted": true }))
            }
            None => Err((Code::InvalidArgument, format!("no pending transfer {id}"))),
        }
    }

    pub fn reject(&self, id: &str) -> Op {
        match self.pending.lock().unwrap().remove(id) {
            Some(tx) => {
                let _ = tx.send(false);
                Ok(json!({ "rejected": true }))
            }
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
    pub async fn serve(self: Arc<Self>, port: u16) {
        let bind = format!("0.0.0.0:{port}").parse().expect("valid bind");
        let (_local, mut incoming) = match self.quic.serve_addr_on(bind).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "receive server failed to bind");
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
    }

    async fn handle_incoming(self: Arc<Self>, mut link: Box<dyn Link>) {
        let sess = match authenticate(
            &mut *link,
            &self.identity,
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
        let peer = sess.peer_id.0.clone();
        let active = self.register(&id, "receiving", &peer, "(incoming)", None);
        events::transfer(
            &id,
            "transfer_queued",
            json!({ "peer": peer, "incoming": true }),
        );

        // Approval: auto-accept already-trusted peers when configured, else wait.
        let accepted = if self.auto_accept && !sess.newly_trusted {
            true
        } else {
            let (tx, rx) = oneshot::channel();
            self.pending.lock().unwrap().insert(id.clone(), tx);
            rx.await.unwrap_or(false)
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
        if is_folder {
            // A folder lands as many files; point history at the save dir.
            *active.path.lock().unwrap() = Some(self.save_dir.clone());
        }
        let save_dir = self.save_dir.clone();
        let storage = self.storage();
        let ctrl = active.ctrl.clone();

        let outcome = drive(
            id.clone(),
            active.stats.clone(),
            active.file.clone(),
            |ptx| async move {
                let mut peek = PeekLink {
                    first: Some(first),
                    inner: &mut secure,
                };
                let r = if is_folder {
                    receive_folder(&mut peek, &storage, &save_dir, &ctrl, &ptx)
                        .await
                        .map(|r| r.outcome)
                } else {
                    receive_file(&mut peek, &storage, &save_dir, &ctrl, &ptx)
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
        self.finish(&id, outcome);
    }
}

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
async fn drive<F, Fut>(
    id: String,
    stats: Arc<Mutex<Stats>>,
    file: Arc<Mutex<String>>,
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
        let mut last = u64::MAX;
        while let Some(bytes) = out_rx.recv().await {
            if bytes == last {
                continue;
            }
            last = bytes;
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
        // First report has a grace window; if it never comes, leave the bar to
        // the bytes-sent fallback in the pump.
        let mut latest = match tokio::time::timeout(PEER_PROGRESS_GRACE, source.recv()).await {
            Ok(Ok(Some(first))) => {
                in_driving.store(true, Ordering::SeqCst);
                emit_peer(&in_id, &in_stats, first);
                first
            }
            // No peer report within the grace: let the pump fall back to
            // bytes-sent from here on.
            _ => {
                in_fell_back.store(true, Ordering::SeqCst);
                return;
            }
        };
        // Emit on each report (up to ~20/s), and at least once a second as a
        // heartbeat so speed/ETA keep ticking and the bar never looks frozen on
        // a slow/stalled link.
        let mut beat = tokio::time::interval(Duration::from_secs(1));
        beat.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                r = source.recv() => match r {
                    Ok(Some(bytes)) => {
                        latest = bytes;
                        emit_peer(&in_id, &in_stats, latest);
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
        while let Some(p) = prx.recv().await {
            if let Some(f) = &p.current_file {
                *file.lock().unwrap() = f.clone();
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

/// A link that replays one already-read frame before delegating — lets the FFI
/// peek the first frame (to choose file vs folder receive) without the engine
/// knowing.
struct PeekLink<'a> {
    first: Option<Frame>,
    inner: &'a mut dyn Link,
}

#[async_trait]
impl Link for PeekLink<'_> {
    async fn send_frame(&mut self, frame: Frame) -> DResult<()> {
        self.inner.send_frame(frame).await
    }
    async fn recv_frame(&mut self) -> DResult<Option<Frame>> {
        if let Some(f) = self.first.take() {
            Ok(Some(f))
        } else {
            self.inner.recv_frame().await
        }
    }
    async fn close(&mut self) -> DResult<()> {
        self.inner.close().await
    }
}

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
