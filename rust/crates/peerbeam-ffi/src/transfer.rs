//! Transfer orchestration behind the FFI. Wraps the production transfer engine
//! (RouteManager + authenticate + SecureLink + send/receive) into an
//! id-addressed, event-driven manager: multiple simultaneous transfers, each a
//! background task, controlled by id, reporting progress/stats/history as
//! events. No file bytes cross FFI — only paths in, metadata/progress out.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    ) -> Self {
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
            history: Mutex::new(Vec::new()),
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

        let mut ids = Vec::new();
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
            let id = self.next_id();
            let active = self.register(&id, "sending", &device.name, &name);
            events::transfer(
                &id,
                "transfer_queued",
                json!({ "peer": device.name, "file": name }),
            );
            ids.push(id.clone());

            let mgr = self.clone();
            let device = device.clone();
            let path = path.to_string();
            crate::runtime::spawn(async move {
                mgr.run_send(id, active, device, path, name, size).await;
            });
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
        let active = self.register(&id, "sending", &device.name, &name);
        events::transfer(
            &id,
            "transfer_queued",
            json!({ "peer": device.name, "folder": name }),
        );

        let mgr = self.clone();
        let id2 = id.clone();
        crate::runtime::spawn(async move {
            mgr.run_send_folder(id2, active, device, path).await;
        });
        Ok(json!({ "id": id }))
    }

    fn register(&self, id: &str, direction: &'static str, peer: &str, file: &str) -> Arc<Active> {
        let active = Arc::new(Active {
            id: id.to_string(),
            direction,
            peer: peer.to_string(),
            ctrl: TransferControl::new(),
            stats: Arc::new(Mutex::new(Stats::new())),
            file: Arc::new(Mutex::new(file.to_string())),
            status: Mutex::new("queued".to_string()),
        });
        self.active
            .lock()
            .unwrap()
            .insert(id.to_string(), active.clone());
        active
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

        let mut link = match self.rm.connect(&device, &session).await {
            Ok(l) => l,
            Err(e) => return self.finish_failed(&id, from_domain(e)),
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
        let mut link = match self.rm.connect(&device, &session).await {
            Ok(l) => l,
            Err(e) => return self.finish_failed(&id, from_domain(e)),
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
        let stats = self
            .active
            .lock()
            .unwrap()
            .get(id)
            .map(|a| a.stats.lock().unwrap().dto())
            .unwrap_or(json!({}));
        let mut payload = json!({ "stats": stats });
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
            json!({
                "id": id,
                "direction": a.direction,
                "peer": a.peer,
                "file": *a.file.lock().unwrap(),
                "bytes": a.stats.lock().unwrap().transferred,
                "success": success,
                "at": timestamp(),
            })
        };
        self.history.lock().unwrap().push(entry);
        events::event(&json!({ "type": "history_updated", "timestamp": timestamp() }));
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
        Ok(json!({ "cancelling": true }))
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
        let active = self.register(&id, "receiving", &peer, "(incoming)");
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
        )
        .await;
        self.finish(&id, outcome);
    }
}

/// Run a transfer future while pumping its progress channel into stats +
/// ordered `transfer_progress` events (one per update, per transfer).
async fn drive<F, Fut>(
    id: String,
    stats: Arc<Mutex<Stats>>,
    file: Arc<Mutex<String>>,
    run: F,
) -> DResult<TransferOutcome>
where
    F: FnOnce(mpsc::UnboundedSender<Progress>) -> Fut,
    Fut: std::future::Future<Output = DResult<TransferOutcome>>,
{
    let (ptx, mut prx) = mpsc::unbounded_channel::<Progress>();
    let pump = async move {
        while let Some(p) = prx.recv().await {
            let dto = {
                let mut s = stats.lock().unwrap();
                s.update(p.transferred_bytes, p.total_bytes);
                s.dto()
            };
            if let Some(f) = &p.current_file {
                *file.lock().unwrap() = f.clone();
            }
            events::transfer(
                &id,
                "transfer_progress",
                json!({ "stats": dto, "file": p.current_file }),
            );
        }
    };
    let work = run(ptx);
    let (r, _) = tokio::join!(work, pump);
    r
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
