//! Interrupted transfers: the surface over the engine's checkpoints.
//!
//! A transfer that ends because the link dropped or the app was closed leaves
//! a checkpoint behind ([`peerbeam_transfer::send_file_on_session_recover`]
//! writes it; the receive path in [`crate::transfer`] writes its own). This
//! module is everything a frontend does with one afterwards: list them, resume
//! one, discard one, and — at startup — throw away the ones nobody will ever
//! come back to.
//!
//! It lives beside [`crate::transfer`] rather than inside it because that file
//! is already the largest in the crate (I8), and because these are genuinely a
//! different lifecycle: an `Active` is a transfer this process is running,
//! while a checkpoint is a claim about one that some process, possibly a
//! previous one, was running.
//!
//! **What is resumable, and what is not.** A *send* can be restarted from
//! here: this side dials, and the receiver's on-disk bytes negotiate the
//! offset. A *receive* cannot — the receiver has no way to ask a peer to start
//! sending again without inventing a wire message, and resume is deliberately
//! the existing protocol's own mechanism. So an interrupted receive keeps its
//! partial file, its progress and — critically — its consent, and resumes the
//! moment the sender offers it again; what the user can do with it here is
//! discard it. That asymmetry is reported honestly to the surface
//! (`resumable`) rather than papered over with a Resume button that does
//! nothing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use peerbeam_domain::entity::{Device, Direction, TransferSession};
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::ReliabilityStore;
use peerbeam_transfer::{check_resume, is_expired, partial_file, ResumeClaim, ResumeRefusal};

use crate::error::Code;
use crate::events;
use crate::transfer::{Manager, Op};

/// Minimum spacing between checkpoint writes during a transfer.
///
/// Deliberately far coarser than [`crate::transfer`]'s progress interval: a
/// checkpoint is a small file rewritten atomically (temp + rename), and the
/// only thing a tighter interval buys is a couple of seconds less re-sent data
/// after a crash — which the protocol re-negotiates from the receiver's disk
/// anyway. What a tighter interval *costs* is a rename per progress tick for
/// the whole life of every transfer.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(2);

/// Keeps a running transfer's checkpoint roughly current.
///
/// The checkpoint written before a transfer starts records zero bytes. Left
/// alone it says zero forever, so an interrupted transfer would come back
/// after a restart claiming no progress at all — the bar starts from nothing
/// and the user has no reason to believe resuming is worth anything. This
/// writes the real count through, at [`CHECKPOINT_INTERVAL`].
///
/// Failures are logged and swallowed: a checkpoint that cannot be updated
/// costs some resume precision, and stopping a transfer that is otherwise
/// working perfectly to complain about it would cost the whole file.
pub struct CheckpointWriter {
    store: Arc<dyn ReliabilityStore>,
    session: TransferSession,
    last: Option<Instant>,
}

impl CheckpointWriter {
    /// A writer for `session`'s checkpoint. Does not itself write: the initial
    /// record is written by whoever starts the transfer, before the first byte
    /// moves.
    pub fn new(store: Arc<dyn ReliabilityStore>, session: TransferSession) -> Self {
        CheckpointWriter {
            store,
            session,
            last: None,
        }
    }

    /// Note that `transferred` bytes have moved, persisting at most once per
    /// [`CHECKPOINT_INTERVAL`].
    pub fn record(&mut self, transferred: u64) {
        if self.last.is_some_and(|t| t.elapsed() < CHECKPOINT_INTERVAL) {
            return;
        }
        self.last = Some(Instant::now());
        self.session.transferred_bytes = transferred;
        if let Err(e) = self.store.save_checkpoint(&self.session) {
            tracing::debug!(error = %e, transfer_id = %self.session.id.as_str(), "checkpoint not updated");
        }
    }
}

/// The DTO one interrupted transfer presents to a frontend.
///
/// Shaped like [`crate::transfer`]'s active-transfer DTO — same `id`,
/// `direction`, `peer_id`, `file`, `stats` keys — so a surface can put the two
/// lists in one view without a second parser. `status` is always
/// `"interrupted"`, which is a status no *active* transfer ever has, so the
/// two can never be confused for each other.
fn dto(cp: &TransferSession) -> Value {
    let file = cp.files.first();
    json!({
        "id": cp.id.as_str(),
        "direction": match cp.direction {
            Direction::Sending => "sending",
            Direction::Receiving => "receiving",
        },
        // The peer's *name* is not in the checkpoint on purpose: a name is
        // neither unique nor stable, and after a restart there is nothing to
        // resolve it against until discovery finds the device again. The
        // surface joins on the id, exactly as it does for every terminal
        // transfer event.
        "peer_id": cp.peer.0,
        "file": file.map(|f| f.name.clone()).unwrap_or_default(),
        "path": file.and_then(|f| f.path.to_str()).unwrap_or_default(),
        "status": "interrupted",
        "started_at": cp.started_at.to_rfc3339(),
        "is_resume": cp.is_resume,
        // Only a send can be restarted from this side — see the module doc.
        "resumable": cp.direction == Direction::Sending,
        "stats": {
            "transferred_bytes": cp.transferred_bytes,
            "total_bytes": cp.total_bytes,
            "current_speed": 0.0,
            "average_speed": 0.0,
            "eta_secs": Value::Null,
        },
    })
}

/// Refuse with the reason the policy gave, as an argument error — every one of
/// them is a statement about what the caller asked for, not a fault.
fn refuse(id: &str, why: ResumeRefusal) -> (Code, String) {
    tracing::warn!(transfer_id = %id, reason = why.message(), "resume refused");
    (
        Code::InvalidArgument,
        format!("cannot resume {id}: {}", why.message()),
    )
}

impl Manager {
    /// Every interrupted transfer, newest first.
    pub fn interrupted_list(&self) -> Op {
        let list: Vec<Value> = self
            .checkpoints()
            .list_checkpoints()
            .map_err(crate::error::from_domain)?
            .iter()
            .map(dto)
            .collect();
        Ok(json!({ "transfers": list }))
    }

    /// Whether an inbound transfer arriving now is the continuation of one the
    /// user has **already** accepted — and may therefore skip the prompt.
    ///
    /// This is the one place consent survives a restart, and it is the one
    /// place it could be laundered, so it grants nothing on its own: it hands
    /// the stored checkpoint and the claim the peer is making right now to
    /// [`check_resume`], which refuses on any of direction, consent, peer id,
    /// file name or total size. A peer that re-offers a transfer this user
    /// declined, ignored, or never saw gets a prompt; a peer that re-offers a
    /// *different* file under a remembered id gets a prompt; a peer that
    /// re-offers the same file at a different size gets a prompt.
    pub fn resumes_accepted_receive(
        &self,
        id: &str,
        peer: &DeviceId,
        name: &str,
        size: u64,
    ) -> bool {
        let Some(cp) = self.load_checkpoint(id) else {
            return false;
        };
        let claim = ResumeClaim {
            peer,
            name,
            total_bytes: size,
            direction: Direction::Receiving,
        };
        match check_resume(&cp, &claim) {
            Ok(()) => {
                tracing::info!(
                    transfer_id = %id,
                    peer = %peer.0,
                    "inbound transfer resumes an already-accepted one; not re-prompting"
                );
                true
            }
            Err(why) => {
                // Not an error: this is simply not a resume, so the ordinary
                // approval gate runs. Logged because "why was I asked again?"
                // is a real question a user will have.
                tracing::debug!(
                    transfer_id = %id,
                    peer = %peer.0,
                    reason = why.message(),
                    "inbound transfer is not a resume of an accepted one; prompting"
                );
                false
            }
        }
    }

    /// The directory a resumed receive must land in, if this is one.
    ///
    /// **This is what makes a resumed receive actually resume.** The partial
    /// bytes are at `<recorded destination>.part`, and the receive engine
    /// derives that path from the directory it is given. Re-deriving the
    /// directory from the current save directory and rules — rather than from
    /// the checkpoint — would send a resumed file to wherever those point
    /// *now*, find no partial file there, and restart the transfer from zero,
    /// leaving the first half orphaned in the old directory. A user who
    /// changed their save folder while a transfer was interrupted is not an
    /// exotic case; it is the ordinary one.
    pub fn resume_destination(
        &self,
        id: &str,
        peer: &DeviceId,
        name: &str,
        size: u64,
    ) -> Option<String> {
        let cp = self.load_checkpoint(id)?;
        let claim = ResumeClaim {
            peer,
            name,
            total_bytes: size,
            direction: Direction::Receiving,
        };
        check_resume(&cp, &claim).ok()?;
        let dest = cp.files.first()?.path.clone();
        let dir = dest.parent()?.to_str()?.to_string();
        (!dir.is_empty()).then_some(dir)
    }

    /// Restart an interrupted **send** from its checkpoint.
    ///
    /// Distinct from [`Manager::resume`], which un-pauses a transfer this
    /// process is still running. The two share a verb and nothing else: that
    /// one needs a live `Active` and fails without one, this one needs a
    /// checkpoint and a peer to dial.
    ///
    /// Verifies the binding before anything is queued, so a checkpoint whose
    /// source file has been moved, replaced or resized is refused here rather
    /// than discovered half-way through a transfer that will fail its
    /// checksum.
    pub fn resume_interrupted(self: &Arc<Self>, req: &Value) -> Op {
        let id = req
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "id required".into()))?;
        let cp = self.load_checkpoint(id).ok_or((
            Code::InvalidArgument,
            format!("no interrupted transfer {id}"),
        ))?;

        // An inbound transfer cannot be pulled: resume is the transfer
        // protocol's own mechanism, and that protocol is sender-driven. Its
        // consent and its partial bytes are kept, so it continues the moment
        // the sender offers it again — which is what `resumes_accepted_receive`
        // is for. Reported as unsupported rather than attempted, so a surface
        // never shows a Resume that silently does nothing.
        if cp.direction == Direction::Receiving {
            return Err((
                Code::Unsupported,
                format!("{id} is an incoming transfer; it resumes when the sender offers it again"),
            ));
        }

        let file = cp
            .files
            .first()
            .ok_or_else(|| refuse(id, ResumeRefusal::Unbound))?;
        let path = file
            .path
            .to_str()
            .ok_or_else(|| refuse(id, ResumeRefusal::Unbound))?
            .to_string();

        // Bind against what is on disk *now*, not against what the checkpoint
        // remembers — the whole point is that time has passed. A source that
        // has been deleted, replaced by a different file of a different size,
        // or truncated cannot be resumed into: the receiver's partial bytes
        // came from the old contents, and appending the new ones would build a
        // file that never existed anywhere.
        let on_disk = std::fs::metadata(&path)
            .map_err(|e| (Code::Storage, format!("cannot resume {id}: {path}: {e}")))?;
        let claim = ResumeClaim {
            peer: &cp.peer,
            name: &file.name,
            total_bytes: on_disk.len(),
            direction: Direction::Sending,
        };
        check_resume(&cp, &claim).map_err(|why| refuse(id, why))?;

        let name = file.name.clone();
        let size = file.size;
        let device = self.peer_device(id, &cp.peer, req.get("peer"))?;

        let active = self
            .register_vacant(
                id,
                "sending",
                &device.name,
                &device.id.0,
                &name,
                Some(path.clone()),
            )
            .ok_or((
                Code::InvalidArgument,
                format!("transfer {id} is already running"),
            ))?;

        events::transfer(
            id,
            "transfer_queued",
            json!({
                "peer": device.name,
                "peer_id": device.id.0,
                "file": name,
                "size": size,
                "resumed": true,
            }),
        );

        let mgr = self.clone();
        let owned = id.to_string();
        let active_handle = active.clone();
        let h = crate::runtime::spawn_handle(async move {
            mgr.run_send(owned, active, device, path, name, size).await;
        });
        *active_handle.task.lock().unwrap() = Some(h);
        Ok(json!({ "id": id, "resumed": true }))
    }

    /// Forget an interrupted transfer: its checkpoint and the partial bytes it
    /// was holding open.
    ///
    /// Without this an interrupted transfer is undismissable clutter — it
    /// cannot complete, nothing will ever clear it, and it sits in the list
    /// forever with a half-written `.part` beside it. The partial file goes
    /// too, deliberately: leaving it would let it seed the *next* transfer of
    /// the same name with a prefix from a transfer the user threw away, which
    /// is the silent corruption the binding check exists to prevent.
    ///
    /// Refuses to touch a transfer that is currently running under the same
    /// id: discarding a live transfer's checkpoint out from under it would
    /// leave it unable to resume when it is the one thing that still could.
    pub fn discard_interrupted(&self, id: &str) -> Op {
        let cp = self.load_checkpoint(id).ok_or((
            Code::InvalidArgument,
            format!("no interrupted transfer {id}"),
        ))?;
        if self.is_active(id) {
            return Err((
                Code::InvalidArgument,
                format!("{id} is running; cancel it instead"),
            ));
        }
        let partial = self.reclaim(&cp);
        self.checkpoints()
            .clear_checkpoint(&cp.id)
            .map_err(crate::error::from_domain)?;
        events::transfer(id, "transfer_discarded", json!({ "peer_id": cp.peer.0 }));
        Ok(json!({ "discarded": true, "partial_removed": partial }))
    }

    /// Drop every checkpoint that has aged out, and the partial bytes each was
    /// holding.
    ///
    /// Runs at startup, which is the only moment at which nothing is running
    /// and every checkpoint on disk is therefore genuinely stale rather than
    /// merely in progress. Returns how many were reclaimed.
    ///
    /// A sweep failure is never fatal: leaking a checkpoint is recoverable,
    /// and refusing to start because a directory could not be listed is not.
    pub fn sweep_checkpoints(&self) -> usize {
        let all = match self.checkpoints().list_checkpoints() {
            Ok(all) => all,
            Err(e) => {
                tracing::warn!(error = %e, "checkpoint sweep skipped");
                return 0;
            }
        };
        let now = chrono::Utc::now();
        let mut swept = 0;
        for cp in all.iter().filter(|cp| is_expired(cp, now)) {
            self.reclaim(cp);
            match self.checkpoints().clear_checkpoint(&cp.id) {
                Ok(()) => swept += 1,
                Err(e) => {
                    tracing::warn!(error = %e, transfer_id = %cp.id.as_str(), "expired checkpoint not removed")
                }
            }
        }
        if swept > 0 {
            tracing::info!(count = swept, "removed expired transfer checkpoints");
        }
        swept
    }

    /// Announce the checkpoints that outlived the run that made them, so a
    /// surface already listening for transfer events learns about them the
    /// same way it learns about everything else.
    pub fn announce_interrupted(&self) {
        let Ok(all) = self.checkpoints().list_checkpoints() else {
            return;
        };
        for cp in &all {
            events::transfer(cp.id.as_str(), "transfer_interrupted", dto(cp));
        }
    }

    /// Announce one transfer as interrupted, if it left a checkpoint.
    ///
    /// Called after a transfer settles. The terminal event a surface already
    /// gets (`transfer_failed`, `transfer_cancelled`) says the transfer is
    /// over, and every surface responds by dropping the row — which is right
    /// when nothing survived and wrong when something did. This is the second
    /// half of the sentence: the transfer is over *and* here is what it left.
    /// Emitted after the terminal event, so a surface applies them in order.
    pub(crate) fn announce_if_interrupted(&self, id: &str) {
        if let Some(cp) = self.load_checkpoint(id) {
            events::transfer(id, "transfer_interrupted", dto(&cp));
        }
    }

    /// Throw a checkpoint away along with the partial bytes it was holding.
    ///
    /// The one place the two are dropped together, so a caller cannot clear
    /// the record and leave the `.part` — which would let a transfer the user
    /// cancelled silently seed the next one of the same name.
    pub(crate) fn discard_checkpoint(&self, cp: &TransferSession) {
        self.reclaim(cp);
        if let Err(e) = self.checkpoints().clear_checkpoint(&cp.id) {
            tracing::warn!(error = %e, transfer_id = %cp.id.as_str(), "checkpoint not cleared");
        }
    }

    /// Delete the partial file a checkpoint was holding, if it has one.
    /// Reports whether anything was removed.
    fn reclaim(&self, cp: &TransferSession) -> bool {
        let Some(part) = partial_file(cp) else {
            return false;
        };
        match std::fs::remove_file(&part) {
            Ok(()) => {
                tracing::info!(path = %part, "removed partial file");
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                tracing::warn!(error = %e, path = %part, "partial file not removed");
                false
            }
        }
    }

    /// The checkpoint under `id`, if there is a readable one.
    fn load_checkpoint(&self, id: &str) -> Option<TransferSession> {
        self.checkpoints()
            .load_checkpoint(&TransferId::from(id))
            .ok()
            .flatten()
    }

    /// Resolve a checkpointed peer id to a device that can actually be dialed.
    ///
    /// A checkpoint stores the peer's **id** and nothing else about how to
    /// reach it, deliberately: addresses are exactly what changes while a
    /// transfer sits interrupted, and a stored one would be the stale half of
    /// every resume. A resume therefore goes wherever the peer is *now* — a
    /// different Wi-Fi network, a Tailscale address instead of a LAN one — and
    /// fails honestly as "not reachable" when the peer is simply not there.
    /// The addresses come either from the caller — a surface that already has
    /// the device in front of the user, exactly as `pb_transfer_send` does —
    /// or from live discovery when it does not. Either way the caller only
    /// gets to say **how to reach** the peer, never **which** peer: a supplied
    /// device whose id is not the checkpoint's is refused outright, so a
    /// resume can never be redirected at a device the interrupted transfer had
    /// nothing to do with.
    fn peer_device(
        &self,
        id: &str,
        peer: &DeviceId,
        supplied: Option<&Value>,
    ) -> Result<Device, (Code, String)> {
        if let Some(v) = supplied {
            if !v.is_null() {
                let device = crate::transfer::device_from(Some(v))?;
                if device.id != *peer {
                    return Err(refuse(id, ResumeRefusal::Peer));
                }
                return Ok(device);
            }
        }
        crate::runtime::find_device(peer).ok_or((
            Code::Connection,
            format!("{} is not reachable right now", peer.0),
        ))
    }
}
