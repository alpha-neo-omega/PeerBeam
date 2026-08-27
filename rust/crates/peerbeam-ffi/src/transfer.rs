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
    ChatError, ChatStore, Direction as ChatDirection, FileRef, PendingFile, SearchHit,
    StagingLimits, StagingStore, Status as ChatStatus, DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT,
};
// Only referenced by the guard test's assertions below; the guard's own
// kind check now lives solely in `ChatRecord::is_settleable_file_row`.
#[cfg(test)]
use peerbeam_chat::Kind as ChatKind;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::entity::{
    Device, DeviceType, Direction, FileEntry, Permission, Progress, TransferSession, TransferStatus,
};
use peerbeam_domain::error::Result as DResult;
use peerbeam_domain::id::{DeviceId, TransferId};
use peerbeam_domain::port::{ReliabilityStore, TrustStore};
use peerbeam_domain::session::CapabilitySet;
use peerbeam_engine::RouteManager;
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    peek_incoming_meta, receive_on_channel, send_file_on_session_recover, send_folder_on_session,
    ChannelReceived, FolderSendRequest, Identity, SendRequest, TransferControl, TransferOutcome,
    BACK_PAUSE, BACK_RESUME,
};
use peerbeam_transfer_quic::{QuicChannels, QuicTransport};
use peerbeam_trust_fs::FsTrust;

use crate::error::{from_domain, Code};
use crate::events;
use crate::resume::CheckpointWriter;

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

pub(crate) struct Active {
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
    pub(crate) task: Mutex<Option<JoinHandle<()>>>,
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

/// Outcome of the optional first-contact pairing check, as applied to an
/// **accept**.
///
/// The deliberate twin of the CLI's `PairingGate` (`peerbeam-cli`'s
/// `commands::pairing_gate`), which this must not drift from: the same three
/// inputs decide the same three outcomes, so a device that would be let
/// through at a shell is let through in the app and vice versa. The CLI asks
/// its question on stdin and answers it in the same breath; the FFI cannot —
/// the surface holding the prompt is a separate process — so here the answer
/// arrives as the `confirmed` flag on the accept call itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairingGate {
    /// Not first contact, or the toggle is off — accept without blocking.
    /// This is every transfer on a default install (the toggle ships off).
    Proceed,
    /// First contact + toggle on + the caller confirmed the codes match.
    Confirmed,
    /// First contact + toggle on + no explicit confirmation — refuse the
    /// accept. The transfer stays pending, so this is recoverable: the user
    /// can compare the codes and accept again. It is *not* a decline, and it
    /// must never be turned into one silently.
    Blocked,
}

/// Decide whether an accept may proceed at first contact.
///
/// `confirmed` is the caller's explicit "I compared the two codes and they
/// match". Anything that is not an explicit `true` blocks, which is the same
/// safe default the CLI's gate applies to a missing answer: a surface that has
/// no way to ask (a script driving the FFI, a UI that has not been taught the
/// prompt) confirms nothing and therefore gets nothing through.
///
/// Pure and total, so the policy is testable without a session, a socket or a
/// settings file — the wiring around it is thin on purpose.
pub(crate) fn pairing_gate(first_contact: bool, require: bool, confirmed: bool) -> PairingGate {
    if !first_contact || !require {
        return PairingGate::Proceed;
    }
    if confirmed {
        PairingGate::Confirmed
    } else {
        PairingGate::Blocked
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

/// What the trust store says about an inbound transfer from `peer`, decided
/// **before** anyone is asked anything.
///
/// Three outcomes rather than a bool, because the store genuinely has three
/// things to say and collapsing any two of them loses a real behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileAdmission {
    /// The user approved this device and then took its `files` permission
    /// away. Refuse without prompting: they have already answered.
    Refused,
    /// Ask the user, exactly as this build always has.
    Prompt,
    /// Accept without asking.
    AutoAccept,
}

/// The trust half of the inbound-transfer decision.
///
/// Extracted from `handle_incoming` as a pure function of data already in hand
/// — no session, no network — so every leg is unit-testable and a refactor
/// cannot delete one silently. It mirrors [`should_send_decline`] and the four
/// `may_*` gates: the predicate is the tested unit, the call site is the thin
/// part.
///
/// The legs, in order:
///
/// 1. **Approved, but `files` revoked → [`Refused`].** The user said "this
///    device may not send me files". Prompting anyway would ask them to
///    re-decide something they already decided, on the schedule of whoever is
///    sending — which is how a permission becomes a nuisance rather than a
///    setting. This also beats a resume: an interrupted transfer that was
///    accepted before the permission was taken away does not get to finish
///    (revoking applies to the *next* operation, and this is one).
/// 2. **`auto_accept` and the device may [`Permission::Files`] →
///    [`AutoAccept`].** Formerly `auto && record.approved`;
///    [`TrustStore::may`] implies that approval, so this is strictly narrower
///    than what it replaces and no device that auto-accepted before stops
///    doing so unless the permission was deliberately removed.
///
///    `auto_accept` is now **either** the global setting **or** this device's
///    own `auto_accept` bit — "stop asking me about this one" — and the two are
///    deliberately an `or`: the per-device answer exists precisely so the
///    global one does not have to be turned on for everybody. Both are still
///    `&& may_files`, so neither can admit a byte the `files` permission would
///    refuse. That conjunction is the whole safety argument and must not be
///    loosened: this is a setting about *asking*, never about *allowing*.
/// 3. **Everything else → [`Prompt`].** In particular a merely *pinned* peer —
///    the state the TOFU handshake leaves every stranger in — is prompted
///    exactly as it always was. Permissions narrow a standing the user granted;
///    they never create one, and they must not turn first contact into a silent
///    refusal.
///
/// [`Refused`]: FileAdmission::Refused
/// [`AutoAccept`]: FileAdmission::AutoAccept
/// [`Prompt`]: FileAdmission::Prompt
/// [`TrustStore::may`]: peerbeam_domain::port::TrustStore::may
pub(crate) fn admit_transfer(
    auto_accept: bool,
    trust: &dyn TrustStore,
    peer: &DeviceId,
) -> FileAdmission {
    let may_files = trust.may(peer, Permission::Files);
    if trust.is_approved(peer) && !may_files {
        return FileAdmission::Refused;
    }
    if auto_accept && may_files {
        return FileAdmission::AutoAccept;
    }
    FileAdmission::Prompt
}

/// [`admit_transfer`], with the per-device *stop asking about this one* bit
/// folded into the global setting.
///
/// Kept as a separate function so [`admit_transfer`]'s existing tests keep
/// asserting the gate itself, and so the `or` between the two settings is one
/// line that can be pointed at rather than a condition spread across callers.
pub(crate) fn admit_transfer_for(
    global_auto_accept: bool,
    per_device_auto_accept: bool,
    trust: &dyn TrustStore,
    peer: &DeviceId,
) -> FileAdmission {
    admit_transfer(global_auto_accept || per_device_auto_accept, trust, peer)
}

/// The `files` permission on the **outbound** path, as an [`Op`]-shaped refusal.
///
/// The mirror of [`admit_transfer`]'s first leg, and deliberately the same
/// shape: a device the user narrowed is refused, a device they never decided
/// about is not. Sending to a merely-pinned peer must keep working — that is
/// the ordinary "spot a device, send it a file" flow, and gating it on `may`
/// (which implies approval) would break the app's primary purpose for every
/// device the user has not explicitly accepted.
///
/// Without this, revoking `files` stopped that device's files arriving but left
/// this device happily sending to it, which is not what "Send and receive files
/// with this device" says on the switch.
fn permit_send_files(trust: &dyn TrustStore, peer: &DeviceId) -> Result<(), (Code, String)> {
    if trust.is_approved(peer) && !trust.may(peer, Permission::Files) {
        return Err((
            Code::PermissionDenied,
            format!(
                "this device may not exchange files with {} — its Files permission was turned off",
                peer.0
            ),
        ));
    }
    Ok(())
}

/// The `chat` permission, as an [`Op`]-shaped refusal.
///
/// Thin on purpose: [`peerbeam_chat::may_exchange_chat`] is the decision and
/// the tested unit; this only turns a `false` into the message a user reads.
/// Both chat entry points call it so a refusal is worded identically whether
/// the user typed a message or attached a file.
fn permit_chat(trust: &dyn TrustStore, peer: &DeviceId) -> Result<(), (Code, String)> {
    if peerbeam_chat::may_exchange_chat(trust, peer) {
        return Ok(());
    }
    Err((
        Code::PermissionDenied,
        format!(
            "messages to {} are not permitted: this device's `chat` permission \
             was revoked. Restore it in Trusted Devices, or run \
             `peerbeam trust permit {} chat`",
            peer.0, peer.0
        ),
    ))
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
    /// Notes, in the same encrypted AppStore under their own namespace.
    notes: peerbeam_notes::NoteStore,
    /// Named local sets of trusted peers. Purely a label — see `peerbeam-spaces`.
    spaces: peerbeam_spaces::SpaceStore,
    /// Groups: rosters every member holds, and the invitations waiting for an
    /// answer. Unlike a Space this **is** on the wire, which is the whole trade
    /// — see `peerbeam-groups` and amendment A2.
    groups: peerbeam_groups::GroupStore,
    /// Hardware addresses this device remembers, so a sleeping machine of the
    /// user's own can be started. Local network only — see `peerbeam-wake`.
    wake: peerbeam_wake::WakeStore,
    /// Clipboard history — empty and unwritten unless the user turned it on.
    clip_history: peerbeam_clipboard::ClipHistory,
    /// The encrypted store every capability shares, kept so folder sync can
    /// open its own index namespace without a second key derivation.
    appstore: Arc<dyn peerbeam_domain::port::AppStore>,
    /// Folders being watched, keyed by path-and-destination, each holding the
    /// flag its thread checks to know when to stop.
    watches: Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicBool>>>,
    /// A program to run after a received file lands, or empty for none.
    /// Behind a lock so a settings change reaches the next file rather than the
    /// next restart.
    receive_hook: RwLock<String>,
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
    /// Staging copies running right now, keyed by `(peer_id, message_id)` — the
    /// handle [`Manager::chat_cancel`] needs to stop an 8 GiB copy that is
    /// already under way. Without it a cancel could only ever dequeue, leaving
    /// the copy to run to completion and re-queue the file behind it.
    ///
    /// Keyed by the pair, not by the message id alone, so a request naming
    /// peer B can never reach a stage belonging to peer A even if it guesses
    /// A's id. `TransferControl` is `Arc`-backed and `Clone`, so the copy task
    /// and this map hold the same flag.
    chat_staging: Mutex<HashMap<(String, String), TransferControl>>,
    /// Every peer's last shared status — live, in memory, never persisted (I4).
    ///
    /// One registry for the whole process, shared (`Clone` is a shared handle)
    /// with each session's `PresenceHandler`, so `pb_presence_json` reads what
    /// every session wrote regardless of which side dialed.
    presence: peerbeam_presence::PresenceRegistry,
    identity: Identity,
    /// The presented name, split out from `identity` so a live rename
    /// (`set_identity_name`) reaches in-flight/future handshakes without a
    /// restart. `identity.name` itself is left stale; always read the name
    /// through [`Self::identity`].
    identity_name: RwLock<String>,
    /// Received-files directory. Interior-mutable so a live settings change
    /// (`set_save_dir`) reaches in-flight/future receives without a restart.
    save_dir: RwLock<String>,
    /// Ordered auto-save rules — **where** an accepted item lands, never
    /// whether it is accepted. Interior-mutable for the same reason
    /// `save_dir` is: editing the list in Settings must apply to the next
    /// receive, not the next launch.
    save_rules: RwLock<Vec<peerbeam_config::SaveRule>>,
    /// Approval policy. Interior-mutable so toggling auto-accept applies live.
    auto_accept: AtomicBool,
    /// Outbound ceiling in bytes per second, `0` unlimited.
    ///
    /// Held on the manager rather than on each transfer because it is a
    /// *device* setting: a person who turns it down means "this machine should
    /// stop saturating my link", not "this one transfer should". Every new
    /// transfer's control is seeded from it, and every running one is updated
    /// when it changes.
    send_limit: AtomicU64,
    /// The optional first-contact pairing check
    /// (`device.require_pairing_confirmation`). Interior-mutable for the same
    /// reason `auto_accept` is: turning it on in Settings must protect the very
    /// next connection, not the next launch.
    ///
    /// **Off by default**, and that default is the whole compatibility story —
    /// off means every accept behaves exactly as it did before this gate
    /// existed.
    require_pairing_confirmation: AtomicBool,
    chunk_size: u32,
    daemon_port: u16,
    active: Mutex<HashMap<String, Arc<Active>>>,
    pending: Mutex<HashMap<String, oneshot::Sender<AcceptDecision>>>,
    /// Transfers whose session pinned its peer **during this very handshake**,
    /// mapped to that peer — i.e. the ones for which this is genuinely first
    /// contact. Populated when the transfer is queued, removed when its
    /// decision resolves.
    ///
    /// One record, read by both halves of this feature (the accept gate and
    /// the refusal un-pin), so the two can never disagree about which
    /// transfers are first contact. It is deliberately *not* re-derived from
    /// the trust store at decision time: by then the peer is pinned either way
    /// — the handshake pinned it — and a lookup could no longer tell a peer
    /// this session pinned from one the user approved last week. That
    /// distinction is exactly what must not be lost, because it is what keeps
    /// a refusal from un-pinning a long-trusted device.
    first_contact: Mutex<HashMap<String, DeviceId>>,
    history: Mutex<Vec<Value>>,
    /// Where history persists across restarts (None = in-memory only, tests).
    history_path: Option<std::path::PathBuf>,
    /// Checkpoints for transfers that were interrupted rather than finished.
    ///
    /// The engine's own store (I2 — there is one recovery implementation and
    /// this is the port it writes through). It is what makes a transfer
    /// killed by a dropped link or a closed app something the user can come
    /// back to instead of something that is simply gone.
    reliability: Arc<dyn ReliabilityStore>,
    counter: AtomicU64,
    daemon_task: Mutex<Option<JoinHandle<()>>>,
    daemon_running: AtomicBool,
}

pub(crate) type Op = Result<Value, (Code, String)>;

/// A space refusal, mapped to the FFI's code space.
///
/// Validation problems are the caller's to fix and say so; a storage failure is
/// ours. Collapsing them all to `Internal` would tell a user typing a duplicate
/// name that the app broke.
fn group_err(e: peerbeam_groups::GroupError) -> (Code, String) {
    use peerbeam_groups::GroupError as E;
    match e {
        // No `NotFound` in this enum: a named group that is not here is an
        // argument that does not identify anything, which is what
        // `InvalidArgument` already means to every surface reading it.
        E::UnknownGroup { .. } => (Code::InvalidArgument, e.to_string()),
        E::Unreadable { .. } | E::Unwritable { .. } => (Code::Storage, e.to_string()),
        _ => (Code::InvalidArgument, e.to_string()),
    }
}

fn space_err(e: peerbeam_spaces::SpaceError) -> (Code, String) {
    use peerbeam_spaces::SpaceError as E;
    match e {
        E::Storage(_) | E::TrustUnreadable { .. } => (Code::Storage, e.to_string()),
        // No NotFound in this code space; a missing space is the caller
        // naming one that is not there, which is an argument problem.
        E::NotFound(_) => (Code::InvalidArgument, e.to_string()),
        _ => (Code::InvalidArgument, e.to_string()),
    }
}

impl Manager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rm: Arc<RouteManager>,
        quic: Arc<QuicTransport>,
        enc: Arc<AeadCrypto>,
        trust: Arc<FsTrust>,
        chat: ChatStore,
        notes: peerbeam_notes::NoteStore,
        clip_history: peerbeam_clipboard::ClipHistory,
        appstore: Arc<dyn peerbeam_domain::port::AppStore>,
        receive_hook: String,
        staging: Arc<StagingStore>,
        staging_limits: StagingLimits,
        identity: Identity,
        save_dir: String,
        save_rules: Vec<peerbeam_config::SaveRule>,
        auto_accept: bool,
        require_pairing_confirmation: bool,
        chunk_size: u32,
        daemon_port: u16,
        history_path: Option<std::path::PathBuf>,
        reliability: Arc<dyn ReliabilityStore>,
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
            // Built before `trust` and `appstore` are moved into the struct.
            spaces: peerbeam_spaces::SpaceStore::new(appstore.clone(), trust.clone()),
            groups: peerbeam_groups::GroupStore::new(
                appstore.clone(),
                trust.clone(),
                identity.device_id.clone(),
            ),
            wake: peerbeam_wake::WakeStore::new(appstore.clone()),
            trust,
            chat,
            notes,
            clip_history,
            appstore,
            watches: Mutex::new(std::collections::HashMap::new()),
            receive_hook: RwLock::new(receive_hook),
            staging,
            staging_limits,
            chat_file_in_flight: Mutex::new(HashSet::new()),
            chat_staging: Mutex::new(HashMap::new()),
            presence: peerbeam_presence::PresenceRegistry::new(),
            identity,
            identity_name,
            save_dir: RwLock::new(save_dir),
            save_rules: RwLock::new(save_rules),
            auto_accept: AtomicBool::new(auto_accept),
            // Unlimited here; `runtime::init` applies the configured ceiling
            // through `set_send_limit` immediately after construction, the same
            // way the other live settings are applied. Threading it through
            // this already-long signature would buy nothing.
            send_limit: AtomicU64::new(0),
            require_pairing_confirmation: AtomicBool::new(require_pairing_confirmation),
            chunk_size: chunk_size.max(1),
            daemon_port,
            active: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            first_contact: Mutex::new(HashMap::new()),
            history: Mutex::new(history),
            history_path,
            reliability,
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

    /// The auto-save rules to consult for the next received item.
    ///
    /// **The one gate.** `crate::rules::SUPPORTED` is checked here rather than
    /// at each write, so there is no arrangement of settings files, hot
    /// restarts or hand-edited documents under which a platform that cannot
    /// honour a destination ends up trying to. On Android this is always
    /// empty, which means "the save directory", which is what SAF gave us.
    fn save_rules(&self) -> Vec<peerbeam_config::SaveRule> {
        if !crate::rules::SUPPORTED {
            return Vec::new();
        }
        self.save_rules.read().unwrap().clone()
    }

    /// Apply a new rule list live (persisted settings change; no restart).
    pub fn set_save_rules(&self, rules: Vec<peerbeam_config::SaveRule>) {
        *self.save_rules.write().unwrap() = rules;
    }

    /// Apply the auto-accept policy live (persisted settings change; no restart).
    pub fn set_auto_accept(&self, v: bool) {
        self.auto_accept.store(v, Ordering::SeqCst);
    }

    /// Apply the outbound speed ceiling live, to new **and running** transfers.
    ///
    /// Reaching the running ones is the point. Someone moves this slider
    /// because something is saturating their link *now*; a limit that applied
    /// only to the next transfer would arrive after the problem had passed.
    pub fn set_send_limit(&self, bytes_per_sec: u64) {
        self.send_limit.store(bytes_per_sec, Ordering::SeqCst);
        for active in self.active.lock().unwrap().values() {
            active.ctrl.set_rate_limit(bytes_per_sec);
        }
    }

    /// Turn the optional first-contact pairing check on or off, live.
    pub fn set_require_pairing_confirmation(&self, v: bool) {
        self.require_pairing_confirmation.store(v, Ordering::SeqCst);
    }

    /// Whether the first-contact pairing check is currently on.
    #[must_use]
    pub fn require_pairing_confirmation(&self) -> bool {
        self.require_pairing_confirmation.load(Ordering::SeqCst)
    }

    /// Record that `id`'s session pinned `peer` during its own handshake.
    fn mark_first_contact(&self, id: &str, peer: &DeviceId) {
        self.first_contact
            .lock()
            .unwrap()
            .insert(id.to_string(), peer.clone());
    }

    /// Whether `id` is still an open first-contact decision.
    fn is_first_contact(&self, id: &str) -> bool {
        self.first_contact.lock().unwrap().contains_key(id)
    }

    /// Take `id`'s first-contact record, if it has one. Removing on the way out
    /// is what keeps the map bounded by *open* decisions rather than by every
    /// transfer this process has ever seen.
    fn take_first_contact(&self, id: &str) -> Option<DeviceId> {
        self.first_contact.lock().unwrap().remove(id)
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
    /// Whether `peer` may ask this device to ring.
    ///
    /// [`Permission::Presence`], for the reason `presence::ring_sink`
    /// documents: a device already allowed to see this machine's status is one
    /// the user has decided may locate it.
    #[must_use]
    pub fn may_ring(&self, peer: &DeviceId) -> bool {
        use peerbeam_domain::port::TrustStore;
        self.trust
            .may(peer, peerbeam_domain::entity::Permission::Presence)
    }

    /// The display name this device has recorded for `peer`, if any.
    ///
    /// Sent with a ring so the alert can name who is looking. An unattributed
    /// noise from a pocket is alarming and gives the user nothing to act on,
    /// and by the time a ring is authorised the trust record — which carries
    /// the name — has already been read.
    #[must_use]
    pub fn peer_name(&self, peer: &DeviceId) -> Option<String> {
        use peerbeam_domain::port::TrustStore;
        self.trust
            .lookup(peer)
            .ok()
            .flatten()
            .map(|r| r.name)
            .filter(|n| !n.is_empty())
    }

    /// Clipboard history. Reading it is always allowed; **writing happens only
    /// when the opt-in is on**, which `clipboard::sink` decides.
    #[must_use]
    pub fn clip_history(&self) -> peerbeam_clipboard::ClipHistory {
        self.clip_history.clone()
    }

    /// The note store, for the session wiring that serves the Notes channel.
    #[must_use]
    pub fn notes_store(&self) -> peerbeam_notes::NoteStore {
        self.notes.clone()
    }

    /// The trust store, for the same wiring — the notes permission is read from
    /// it per batch.
    /// Whether a hardware address is recorded for `device`, so it can be woken.
    ///
    /// Used to exempt it from device pruning: waking a machine needs it listed,
    /// and a machine worth waking is asleep. A read failure answers `false` —
    /// the exemption is a convenience, and a store that cannot be read should
    /// not pin every device in the list for ever.
    #[must_use]
    pub fn has_wake_address(&self, device: &DeviceId) -> bool {
        self.wake.lookup(device).ok().flatten().is_some()
    }

    #[must_use]
    pub fn trust_store(&self) -> Arc<FsTrust> {
        self.trust.clone()
    }

    fn chat_wiring(&self) -> crate::session_exec::ChatWiring {
        crate::session_exec::ChatWiring {
            store: self.chat.clone(),
            sink: Arc::new(|rec| crate::events::chat(&rec)),
        }
    }

    /// The presence wiring every session registers, dial or accept — for
    /// exactly the reason `chat_wiring` above must be registered on both sides.
    /// A session without a `PresenceHandler` does not refuse an inbound Status;
    /// the channel dispatch loop drops it silently, so the peer believes it
    /// shared and this device shows nothing.
    ///
    /// Wiring it does **not** mean this device shares anything. The heartbeat
    /// task re-checks `may_share_status` on every beat, and with the opt-in
    /// setting off (the default) it opens no channel and sends no frame.
    fn presence_wiring(&self) -> crate::presence::PresenceWiring {
        crate::presence::PresenceWiring {
            registry: self.presence.clone(),
            save_dir: self.save_dir(),
        }
    }

    /// The live presence snapshot for `pb_presence_json`.
    pub fn presence_snapshot(&self) -> Op {
        crate::presence::snapshot(&self.presence, &self.save_dir())
    }

    /// Push the clipboard to every named peer: `{text, peers:[…]}` → `{queued}`.
    ///
    /// Called by the desktop watcher when the user copies something. The two
    /// decisions that can be made **without touching the network** are made
    /// here, synchronously, and both refuse before anything is dialed:
    ///
    /// * **The opt-in.** With the setting off this returns `queued: 0` having
    ///   done nothing at all — no dial, no handshake, no packet. "Off" must be
    ///   observably silent, not merely undelivered.
    /// * **The cap.** An over-cap clip is refused *here* rather than after N
    ///   dials, because `ClipboardSender::send` would refuse it against every
    ///   peer anyway. It is an error, not a silent skip, so the surface can
    ///   tell the user their copy was too large instead of leaving them to
    ///   wonder why one machine never got it. It is never truncated.
    ///
    /// The third gate — trust — and the fourth — the peer's negotiated
    /// capability — are per-peer and cannot be decided until a session exists,
    /// so they stay where they belong: in `may_share_clip`, consulted by
    /// `ClipboardSender::send` on the far side of the dial. This method must
    /// never pre-empt them; a peer named here that turns out to be untrusted is
    /// simply sent nothing.
    ///
    /// Delivery runs in the background, one task per peer, and this returns as
    /// soon as they are spawned — a clipboard watcher must not block on a
    /// handshake, and one unreachable device must not delay the rest. A push
    /// that fails is dropped rather than queued: the clipboard is live state,
    /// and delivering what the user copied ten minutes ago on top of what they
    /// copied since would be worse than not delivering it.
    pub fn clipboard_sync(self: &Arc<Self>, req: &Value) -> Op {
        let text = req
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "text required".into()))?;
        // Validated against the same constant the wire encoder uses, so this
        // check and `Clip::new`'s can never disagree about what fits.
        if text.is_empty() {
            return Err((Code::InvalidArgument, "clipboard is empty".into()));
        }
        if text.len() > peerbeam_clipboard::MAX_CLIP {
            return Err((
                Code::InvalidArgument,
                format!(
                    "clipboard too large to sync: {} bytes (max {})",
                    text.len(),
                    peerbeam_clipboard::MAX_CLIP
                ),
            ));
        }
        // **Recorded here too, not only on the way in.** Clipboard history was
        // written from one place — a clip *arriving* from a peer — so the log
        // held only other people's copies, and what this device sent was absent
        // from its own history. Recorded before the sync gate, because a clip
        // this device put out is a clip this device had whether or not it was
        // allowed to leave.
        if crate::clipboard::history_enabled() {
            let _ = self.clip_history.record(text, None);
        }
        if !crate::clipboard::sync_enabled() {
            return Ok(json!({ "queued": 0, "sync": false }));
        }
        let peers: Vec<Device> = req
            .get("peers")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|d| device_from(Some(d)).ok()).collect())
            .unwrap_or_default();

        let mut queued = 0usize;
        for device in peers {
            let me = self.clone();
            let text = text.to_string();
            crate::runtime::spawn(async move {
                me.clipboard_push_peer(device, &text).await;
            });
            queued += 1;
        }
        Ok(json!({ "queued": queued, "sync": true }))
    }

    /// Dial one peer and offer it the clip. Best-effort and silent on failure:
    /// an unreachable device simply does not get this clip, and nothing is
    /// retried or stored.
    async fn clipboard_push_peer(self: &Arc<Self>, device: Device, text: &str) {
        let meta = self.session(&format!("clip-{}", device.id.0), device.id.clone(), 0);
        let Ok(session) = crate::session_exec::dial(
            &self.quic,
            &self.rm,
            &device,
            &meta,
            self.identity(),
            self.enc.clone(),
            self.trust.clone(),
            // Same rationale as every other dial site: this session must be
            // able to receive what the peer pushes back over it, not just
            // carry ours out.
            Some(self.chat_wiring()),
            Some(self.presence_wiring()),
        )
        .await
        else {
            return; // unreachable; the clipboard is live state, so nothing queues
        };
        // The **authenticated** peer, not the pre-dial `device.id`: the gate
        // asks the trust store about who actually answered, so a device
        // impersonating a trusted id in discovery is refused here.
        let mut sender = peerbeam_clipboard::ClipboardSender::new(
            session.handle.clone(),
            session.peer_device.clone(),
            session.capabilities.clone(),
            self.trust.clone(),
            Arc::new(crate::clipboard::sync_enabled),
        );
        let _ = sender.send(text).await;
        session.close().await;
    }

    /// Drop a peer's shared status. Called when its trust is revoked: a device
    /// the user no longer trusts must leave the dashboard immediately rather
    /// than linger until restart.
    pub fn presence_forget(&self, peer: &DeviceId) {
        self.presence.forget(peer);
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
        let task = self.daemon_task.lock().unwrap().take();
        if let Some(handle) = task {
            handle.abort();
            // **Waited on, not just asked to stop.** `abort` schedules
            // cancellation; the task keeps running until it next yields, and it
            // holds the QUIC endpoint while it does. So this used to return with
            // the port still bound and the daemon still alive — and when the task
            // finally unwound, `serve`'s own exit path emitted `daemon_stopped`
            // into whatever was listening by then. In the test suite that meant a
            // later test's event buffer; for a host that stops and restarts the
            // engine it means the rebind can race a listener that has not let go.
            match tokio::runtime::Handle::try_current() {
                // Already inside a multi-threaded runtime (the case
                // `runtime::shutdown` documents): `block_on` would panic, so hand
                // the blocking to this worker.
                //
                // A *current-thread* runtime is deliberately left alone: it has
                // no other worker to hand the blocking to, so waiting here would
                // deadlock the very executor that has to poll the task to
                // completion. The abort still stands, making that the one path
                // that returns before the task has finished — reachable only
                // from a test driving this inside `#[tokio::test]`, never from a
                // host, which calls in from a plain thread and takes the branch
                // below.
                Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                    let _ = tokio::task::block_in_place(|| h.block_on(handle));
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = crate::runtime::rt().block_on(handle);
                }
            }
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
        *self.daemon_task.lock().unwrap() = None;
        // Announced on the transition only. Both callers can reach this for the
        // same stop — `stop_daemon` explicitly, and `serve` as it exits — so
        // emitting unconditionally sent `daemon_stopped` twice for one daemon,
        // the second one late enough to land after the next engine had started.
        if self.daemon_running.swap(false, Ordering::SeqCst) {
            daemon_event("daemon_stopped", self.daemon_port);
        }
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

    /// The checkpoint store. The single handle every interrupted-transfer
    /// operation goes through — see [`crate::resume`].
    pub(crate) fn checkpoints(&self) -> &dyn ReliabilityStore {
        self.reliability.as_ref()
    }

    /// Whether a transfer is registered and running under `id` right now.
    /// A checkpoint's id and a live transfer's id are the same keyspace, so
    /// anything that acts on a checkpoint has to be able to ask.
    pub(crate) fn is_active(&self, id: &str) -> bool {
        self.active.lock().unwrap().contains_key(id)
    }

    /// Transfer-session metadata used to dial (routing/telemetry only).
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
            accepted: true,
        }
    }

    /// The record that survives this transfer being interrupted.
    ///
    /// Unlike [`session`](Self::session) — which only ever describes a dial to
    /// the route manager — this one is written to disk and read back by a
    /// process that knows nothing else about the transfer, so every field a
    /// resume has to check is filled in: the peer, the file's name, its
    /// destination (a receive) or source (a send), and its size, in both the
    /// per-file entry and `total_bytes` because `check_resume` compares them
    /// against each other.
    ///
    /// `accepted` is the caller's assertion that the local user consented, and
    /// is the only place that assertion is made. Every send passes `true` (the
    /// user started it); the receive path passes `true` only on the far side of
    /// the approval gate.
    ///
    /// `is_resume` is not a parameter because it is not a caller's opinion: a
    /// checkpoint already sitting under this id *is* a prior interrupted run,
    /// and there is nothing else it could be. `started_at` restarts here too,
    /// so a transfer someone keeps retrying never ages into expiry.
    #[allow(clippy::too_many_arguments)]
    fn checkpoint(
        &self,
        id: &str,
        peer: DeviceId,
        direction: Direction,
        path: &str,
        name: &str,
        size: u64,
        accepted: bool,
    ) -> TransferSession {
        let is_resume = self
            .reliability
            .load_checkpoint(&TransferId::from(id))
            .ok()
            .flatten()
            .is_some();
        TransferSession {
            id: TransferId::from(id),
            peer,
            direction,
            status: TransferStatus::Transferring,
            files: vec![FileEntry {
                path: std::path::PathBuf::from(path),
                name: name.to_string(),
                size,
                mime_type: String::new(),
                checksum: None,
            }],
            total_bytes: size,
            transferred_bytes: 0,
            started_at: chrono::Utc::now(),
            completed_at: None,
            is_resume,
            accepted,
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
        permit_send_files(self.trust.as_ref(), &device.id)?;
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
        permit_send_files(self.trust.as_ref(), &device.id)?;
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
    pub(crate) fn register_vacant(
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
            ctrl: {
                // Seeded, not left unlimited: a transfer starting while a limit
                // is in force must respect it from its first chunk.
                let c = TransferControl::new();
                c.set_rate_limit(self.send_limit.load(Ordering::SeqCst));
                c
            },
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
                Some(self.presence_wiring()),
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

    pub(crate) async fn run_send(
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
        // The record that outlives an interruption. Built before the transfer
        // rather than after it fails, because the case it exists for — the
        // process being killed — never reaches an "after".
        let checkpoint = self.checkpoint(
            id,
            device.id.clone(),
            Direction::Sending,
            &path,
            &name,
            size,
            // A send is the local user's own action; there is no prompt and
            // nothing to withhold.
            true,
        );
        let req = SendRequest {
            transfer_id: id.to_string(),
            name,
            path,
            size,
            chunk_size: self.chunk_size,
        };
        let storage = self.storage();
        let ctrl = active.ctrl.clone();
        let reliability = self.reliability.clone();
        let writer = CheckpointWriter::new(self.reliability.clone(), checkpoint.clone());
        let outcome = drive(
            id.to_string(),
            active.stats.clone(),
            active.file.clone(),
            active.ctrl.clone(),
            |ptx| async move {
                let r = send_file_on_session_recover(
                    &handle,
                    &storage,
                    reliability.as_ref(),
                    req,
                    checkpoint,
                    &ctrl,
                    &ptx,
                    SEND_RECOVER_ATTEMPTS,
                    3,
                )
                .await;
                drop(ptx);
                r
            },
            None,
            None,
            Some(writer),
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
        // The terminal event above says the transfer is over; this says what
        // it left behind. Emitted only when a checkpoint actually survived, so
        // a completed or cancelled transfer says nothing extra.
        self.announce_if_interrupted(id);
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

        let expect_ack = crate::session_exec::caps_support_folder_ack(&session.capabilities);
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
                let r = send_folder_on_session(&handle, &storage, req, &ctrl, &ptx, 3, expect_ack)
                    .await;
                drop(ptx);
                r
            },
            None,
            None,
            // A folder is a manifest plus N files, not one resumable stream —
            // a checkpoint naming a single file/size would be a lie about what
            // is on disk, and `check_resume` would have nothing true to bind
            // to. Folder resume is Phase C's folder-sync work, not this.
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
        let peer = DeviceId::from(a.peer_id.clone());
        // **A missing row is not a reason to stop.** This used to begin with
        // `if !is_settleable_chat_row(a) { return }`, and that helper answers
        // `false` when the row simply is not there yet — which is exactly the
        // case worth handling. The peer's claim rides CHAT and what actually
        // lands rides TRANSFER; nothing orders them, and when the transfer wins
        // the race the row has not been written yet. The guard turned that into
        // a silent no-op, so the approval prompt kept the sender's chosen name.
        //
        // `set_file_row_landing` distinguishes the three cases properly: it
        // parks the landing when the row is absent (applied by `append` when
        // the row appears), declines when the row exists but is not a
        // settleable file row, and writes otherwise. A row that exists and is
        // not settleable is still skipped — just by the store, which can tell
        // the difference, rather than by a guard that cannot.
        if self.chat.get(&peer, &a.id).is_ok_and(|r| {
            r.is_some_and(|rec| {
                !rec.is_settleable_file_row(if a.direction == "sending" {
                    ChatDirection::Out
                } else {
                    ChatDirection::In
                })
            })
        }) {
            return;
        }
        let expected = if a.direction == "sending" {
            ChatDirection::Out
        } else {
            ChatDirection::In
        };
        // Logged either way, and deliberately without the name: this reconcile
        // is what stops a peer's claim standing unchallenged at the approval
        // prompt, so "it did not apply" is a security-relevant outcome and
        // silence about it is how a platform-specific failure hides. The name
        // itself is not logged — a filename can be sensitive, and the ids are
        // enough to follow what happened.
        match self
            .chat
            .set_file_row_landing(&peer, &a.id, expected, name, size)
        {
            // `info`, not `debug`: the default filter is `peerbeam=info`, so a
            // debug line never reaches the log buffer and "applied" becomes
            // indistinguishable from "never ran" when reading a report.
            Ok(peerbeam_chat::Landing::Applied) => {
                tracing::info!(transfer_id = %a.id, peer = %peer.0, "chat row landing applied")
            }
            // Parked is a success: the row has not arrived yet and `append`
            // will apply this when it does. Warning here would fire on the
            // ordinary side of a race and train a reader to ignore the line.
            Ok(peerbeam_chat::Landing::Parked) => tracing::debug!(
                transfer_id = %a.id,
                peer = %peer.0,
                "chat row landing parked until its row arrives"
            ),
            Ok(peerbeam_chat::Landing::Declined) => tracing::warn!(
                transfer_id = %a.id,
                peer = %peer.0,
                direction = ?expected,
                name_empty = name.is_empty(),
                "chat row landing declined — the row will keep the peer's claim"
            ),
            Err(e) => {
                tracing::warn!(error = %e, transfer_id = %a.id, peer = %peer.0, "chat row landing not persisted");
            }
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
        // The transfer is over. If it never had a chat row, any landing parked
        // for it is waiting for something that will not arrive — most transfers
        // are not chat files, so leaving them would grow a record per transfer
        // forever. A row that *does* exist has already consumed it.
        if self
            .chat
            .get(&DeviceId::from(a.peer_id.clone()), id)
            .ok()
            .flatten()
            .is_none()
        {
            self.chat.drop_pending_landing(&a.peer_id, id);
        }
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
        // The receive hook, if the user configured one. **Received files only,
        // and only successful ones**: a hook that fired on this device's own
        // sends would run on files that never arrived from anywhere, and one
        // that fired on a failure would be handed a partial or absent path.
        //
        // Read from live config each time, so turning the hook off stops the
        // next file rather than the next restart.
        if success && a.direction == "receive" {
            let hook = self.receive_hook.read().unwrap().clone();
            if let (Some(p), Some(peer)) = (
                entry.get("path").and_then(Value::as_str),
                entry.get("peer_id").and_then(Value::as_str),
            ) {
                // Fire and forget: the child is dropped deliberately, since
                // nothing about the completed transfer depends on it.
                let _ = crate::hook::run(&hook, p, peer);
            }
        }
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

    /// Apply the optional first-contact pairing check to an accept.
    ///
    /// Runs **before** the pending decision is taken, so a blocked accept
    /// leaves the transfer exactly as it found it: still pending, still
    /// answerable. The user compares the two codes and accepts again — being
    /// asked to verify must never cost them the file.
    fn check_pairing(&self, id: &str, confirmed: bool) -> Result<(), (Code, String)> {
        match pairing_gate(
            self.is_first_contact(id),
            self.require_pairing_confirmation(),
            confirmed,
        ) {
            PairingGate::Proceed | PairingGate::Confirmed => Ok(()),
            PairingGate::Blocked => Err((
                Code::InvalidArgument,
                format!(
                    "transfer {id} is from a device seen for the first time; \
                     confirm the pairing code matches the other device before accepting"
                ),
            )),
        }
    }

    /// Accept an incoming transfer this one time only. Does not trust the
    /// sending device — the next incoming transfer from it still needs a
    /// decision. See [`accept_trust`](Self::accept_trust) to also trust it.
    ///
    /// `confirmed` answers the first-contact pairing check: it means "the user
    /// compared this session's pairing code against the other device's screen
    /// and they match". It is consulted **only** when the peer was pinned by
    /// this very handshake and the check is switched on; on a default install
    /// it is dead weight and every accept proceeds as it always has. It is
    /// never inferred — a caller that does not pass it has confirmed nothing.
    pub fn accept(&self, id: &str, confirmed: bool) -> Op {
        self.check_pairing(id, confirmed)?;
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
    ///
    /// `confirmed` carries the same meaning as on [`accept`](Self::accept), and
    /// is gated identically. If anything this path needs it more: it is the one
    /// call that grants a device standing auto-accept, so letting it skip the
    /// check while the weaker accept honoured it would leave the gate guarding
    /// only the lesser act.
    pub fn accept_trust(&self, id: &str, confirmed: bool) -> Op {
        self.check_pairing(id, confirmed)?;
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

    /// Refuse an incoming transfer.
    ///
    /// At **first contact** this is also the app's `PairingGate::Revoke`: the
    /// peer was pinned by the handshake that offered this file, the user has
    /// just said no to it, and the pin goes with it. Refusing a device the user
    /// has met before does not touch its pin — only the peer *this session
    /// pinned* is un-pinned, which is why the decision is read from
    /// [`first_contact`](Self::first_contact) and not from the trust store (by
    /// now the store cannot tell the two apart).
    pub fn reject(&self, id: &str) -> Op {
        // Answer the decision first, so the refusal is in effect no matter what
        // the un-pin below does. Nothing lands on disk either way.
        match self.pending.lock().unwrap().remove(id) {
            // Same rationale as `accept`: a failed send means the timeout
            // already declined this transfer out from under us.
            Some(tx) => {
                if tx.send(AcceptDecision::Reject).is_err() {
                    return Err((Code::InvalidArgument, format!("no pending transfer {id}")));
                }
            }
            None => return Err((Code::InvalidArgument, format!("no pending transfer {id}"))),
        }
        let Some(peer) = self.take_first_contact(id) else {
            return Ok(json!({ "rejected": true }));
        };
        // A security-relevant un-pin, and the one place in this feature where a
        // swallowed error would be worse than an outage. Reporting success on a
        // failed removal would leave the peer trusted on disk while the app
        // told the user it had been un-pinned — and the *next* connection from
        // it would then not be first contact at all: no `newly_trusted`, no
        // pairing code shown, no gate, silently. The user would never be asked
        // to verify the device they had just refused as a suspected MITM.
        //
        // So the result is checked and the failure is reported, on the same
        // channel every other transfer failure uses. The refusal itself already
        // stands (above), so this cannot fall through to receiving data; what
        // the error buys is that the user finds out the pin is still there and
        // can remove it themselves in Trusted Devices.
        match self.trust.remove(&peer) {
            Ok(_) => Ok(json!({ "rejected": true, "unpinned": true })),
            Err(e) => Err((
                Code::Storage,
                format!(
                    "transfer {id} was declined, but this device could NOT be un-pinned ({e}); \
                     remove it in Trusted Devices — until then it will not be treated as new"
                ),
            )),
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

    /// One chronological view of what this device has done: `{limit?}` →
    /// `{events:[…]}`, newest first.
    ///
    /// **A read across stores this device already keeps** — transfer history,
    /// every conversation, and clipboard history when it is on. Nothing new is
    /// recorded to build it: a timeline that needed its own log would be a
    /// second copy of the same facts, free to disagree with them, and a
    /// durable record of activity the user never separately agreed to.
    ///
    /// Clipboard entries appear only when clipboard history is on, because
    /// otherwise there is nothing to read; they carry **no text**, only that a
    /// clip happened and who it came from. A timeline is for recognising when
    /// something occurred, and putting clip contents in a scrollable activity
    /// feed would undo the care taken to bound and abbreviate them elsewhere.
    pub fn timeline(&self, req: &Value) -> Op {
        const DEFAULT_LIMIT: usize = 200;
        const MAX_LIMIT: usize = 1000;
        let limit = match req.get("limit") {
            None | Some(Value::Null) => DEFAULT_LIMIT,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=MAX_LIMIT as u64).contains(n))
                .ok_or((
                    Code::InvalidArgument,
                    format!("limit must be an integer between 1 and {MAX_LIMIT}"),
                ))? as usize,
        };

        let mut events: Vec<Value> = Vec::new();

        // Transfers.
        for item in self.history.lock().unwrap().iter() {
            let at = item.get("at").and_then(Value::as_str).unwrap_or_default();
            events.push(json!({
                "kind": "transfer",
                "at": at,
                "peer": item.get("peer").and_then(Value::as_str).unwrap_or_default(),
                "detail": item.get("file_name").and_then(Value::as_str).unwrap_or_default(),
                "ok": item.get("success").and_then(Value::as_bool).unwrap_or(false),
            }));
        }

        // Conversations. A message's own body is not carried: the timeline says
        // that a conversation happened, and the conversation itself is one tap
        // away with far better tools for reading it.
        if let Ok(peers) = self.chat.conversations() {
            for peer in peers {
                if let Ok(history) = self.chat.history(&peer) {
                    for rec in history {
                        events.push(json!({
                            "kind": "chat",
                            "at": rec.timestamp,
                            "peer": peer.0,
                            "detail": match rec.kind {
                                peerbeam_chat::Kind::File => rec
                                    .file
                                    .as_ref()
                                    .map_or_else(String::new, |f| f.name.clone()),
                                _ => String::new(),
                            },
                            "ok": true,
                        }));
                    }
                }
            }
        }

        // Clipboard, only when the user opted into remembering it at all.
        if crate::clipboard::history_enabled() {
            if let Ok(entries) = self.clip_history.list() {
                for e in entries {
                    events.push(json!({
                        "kind": "clipboard",
                        "at": e.at,
                        "peer": e.from.unwrap_or_default(),
                        "detail": "",
                        "ok": true,
                    }));
                }
            }
        }

        // Newest first, with the id as a stable tiebreak so two events in the
        // same second do not swap places between calls.
        events.sort_by(|a, b| {
            let (x, y) = (
                a.get("at").and_then(Value::as_str).unwrap_or_default(),
                b.get("at").and_then(Value::as_str).unwrap_or_default(),
            );
            y.cmp(x).then_with(|| {
                let (ka, kb) = (
                    a.get("kind").and_then(Value::as_str).unwrap_or_default(),
                    b.get("kind").and_then(Value::as_str).unwrap_or_default(),
                );
                ka.cmp(kb)
            })
        });
        let truncated = events.len() > limit;
        events.truncate(limit);
        Ok(json!({ "events": events, "truncated": truncated, "limit": limit }))
    }

    /// Pinned devices, newest first.
    ///
    /// `approved` is the difference between "this key was pinned when the
    /// device connected" and "the user chose this device". Every never-seen
    /// peer is pinned by the authenticated handshake so a later key change is
    /// detectable, so this list contains strangers as well as the user's own
    /// machines — and only the approved ones may be sent presence, clipboard
    /// contents, or an accepted pipe. A surface that renders the two alike
    /// tells the user a stranger is trusted.
    ///
    /// `permissions` is an explicit **array of names**, never a bitmask and
    /// never inferred from `approved`: it is what the device may actually do,
    /// and a surface renders one toggle per entry. It is emitted for pinned
    /// devices too (where it is typically empty), so a surface never has to
    /// guess what an absent key means.
    ///
    /// `approved` is the **effective** answer, not the stored bit: a device
    /// whose time-limited approval has run out reports `false` here, with
    /// `expired: true` and `expires_at` saying why. Reporting the stored bit
    /// would leave a surface showing a device as trusted after its window shut,
    /// which is the one thing time-limited trust exists to prevent — and the
    /// permissions array has always been the effective set, so the two would
    /// have disagreed on the same row.
    ///
    /// One clock read for the whole list, so a long listing cannot straddle a
    /// deadline and report two devices as of two different instants.
    pub fn trust_list(&self) -> Op {
        let now = chrono::Utc::now();
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
                    "approved": r.is_approved_at(now),
                    "expires_at": r.expires_at.map(|at| at.to_rfc3339()),
                    "expired": r.approved && r.has_expired(now),
                    "permissions": r
                        .effective_permissions_at(now)
                        .granted()
                        .into_iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>(),
                    // The *effective* answer, like `approved` and `permissions`
                    // above it: a device whose window has closed is not
                    // auto-accepting, whatever its stored bit says, and a
                    // surface must not draw a toggle claiming otherwise.
                    "auto_accept": r.auto_accept && r.is_approved_at(now),
                })
            })
            .collect();
        Ok(json!({ "devices": devices }))
    }

    /// Stop asking about one device's files, or start asking again:
    /// `{id, auto_accept}` → `{changed}`.
    ///
    /// The global *auto-accept trusted devices* setting is all-or-nothing, so
    /// silencing one device the user syncs with constantly meant silencing every
    /// approved device. This is the same answer given per device.
    ///
    /// **A prompt setting, not a permission.** It is consulted only after
    /// `may(Files)` has already said yes, so it can never admit a transfer the
    /// permission would refuse — setting it on a device that may not send files
    /// is inert. `changed: false` also covers a device that is not pinned.
    pub fn trust_set_auto_accept(&self, req: &Value) -> Op {
        let id = req
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "id required".into()))?;
        let auto_accept = req
            .get("auto_accept")
            .and_then(|v| v.as_bool())
            .ok_or((Code::InvalidArgument, "auto_accept required".into()))?;
        let changed = self
            .trust
            .set_auto_accept(&DeviceId::from(id), auto_accept)
            .map_err(from_domain)?;
        if changed {
            events::event(&json!({ "type": "trust_changed", "timestamp": timestamp() }));
        }
        Ok(json!({ "changed": changed }))
    }

    /// Grant or withhold one permission for one pinned device:
    /// `{id, permission, granted}` → `{changed}`.
    ///
    /// Names, not indices, so the call means the same thing across an engine
    /// upgrade that adds a permission. An unknown name is an
    /// [`Code::InvalidArgument`] rather than a silent no-op — a surface asking
    /// for something this engine cannot enforce must be told, not humoured.
    ///
    /// `changed: false` means the store already read that way (or the device is
    /// not pinned); it is not an error, so a UI that re-asserts a toggle is
    /// idempotent.
    ///
    /// Emits `trust_changed`, which is what makes the Trusted Devices list —
    /// and any other open surface — re-read without polling.
    /// Approve a pinned device directly — `{id, share?}` → `{approved, pinned}`.
    ///
    /// The GUI could previously approve a device **only** by answering a
    /// transfer from it: `pb_transfer_accept_trust` takes a transfer id, and
    /// nothing else wrote approval. A device seen once and never sent a file
    /// therefore sat in the trusted list forever reading "Accept a transfer
    /// from it to approve" — the CLI's `trust approve` had no counterpart here,
    /// which is invariant I7 in reverse: the engine's capability existed and
    /// one frontend could not reach it.
    ///
    /// `share: false` is the *trust it, share nothing* case. See
    /// [`FsTrust::approve_with`] for why the permission set is written only on
    /// the transition to approved.
    ///
    /// `pinned: false` reports a device this machine holds no key for, rather
    /// than reporting a success the store does not reflect.
    pub fn trust_approve(&self, req: &Value) -> Op {
        let id = req
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "id required".into()))?;
        // Defaults to the historical grant, so a caller that omits the field
        // gets exactly what `trust approve` has always done.
        let share = req.get("share").and_then(|v| v.as_bool()).unwrap_or(true);
        let grant = if share {
            peerbeam_domain::entity::PermissionSet::granted_on_approval()
        } else {
            peerbeam_domain::entity::PermissionSet::none()
        };
        let device = DeviceId::from(id);
        let pinned = self
            .trust
            .approve_with(&device, true, None, grant)
            .map_err(from_domain)?;
        if pinned {
            events::event(&json!({ "type": "trust_changed", "timestamp": timestamp() }));
        }
        Ok(json!({ "approved": pinned, "pinned": pinned }))
    }

    pub fn trust_set_permission(&self, req: &Value) -> Op {
        let id = req
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "id required".into()))?;
        let name = req
            .get("permission")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "permission required".into()))?;
        let granted = req
            .get("granted")
            .and_then(|v| v.as_bool())
            .ok_or((Code::InvalidArgument, "granted required".into()))?;
        let permission = Permission::parse(name).ok_or((
            Code::InvalidArgument,
            format!(
                "unknown permission `{name}` — this engine knows {}",
                Permission::ALL.map(|p| p.as_str()).join(", ")
            ),
        ))?;
        let device = DeviceId::from(id);
        let changed = self
            .trust
            .set_permission(&device, permission, granted)
            .map_err(from_domain)?;
        // Presence is displayed from a live registry, so a revoked `presence`
        // permission must also drop what this device already holds — otherwise
        // the dashboard keeps showing a status for a peer that may no longer
        // send one. The send side needs nothing: every gate re-reads the store.
        if !granted && permission == Permission::Presence {
            self.presence_forget(&device);
        }
        events::event(&json!({ "type": "trust_changed", "timestamp": timestamp() }));
        Ok(json!({ "changed": changed }))
    }

    /// Revoke a pinned device; its next connection needs fresh approval.
    pub fn trust_remove(&self, req: &Value) -> Op {
        let id = req
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "id required".into()))?;
        let device = DeviceId::from(id);
        let removed = self.trust.remove(&device).map_err(from_domain)?;
        // Revoking trust removes the device from the presence dashboard now,
        // not at the next restart. The send side is already handled — the
        // heartbeat re-reads the trust store every beat — but a status this
        // device *already* holds would otherwise keep being displayed for a
        // peer the user just said they do not trust.
        self.presence_forget(&device);
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
        // `in_reply_to` is optional and carries only the answered message's id.
        // Nothing quotes text into the new body: a snapshot would survive the
        // message it quoted, which is exactly how a retention window gets
        // defeated by replying to something.
        let in_reply_to = req.get("in_reply_to").and_then(|v| v.as_str());
        let msg = peerbeam_chat::ChatMessage::replying(text, in_reply_to)
            .map_err(|e| (Code::InvalidArgument, e.to_string()))?;
        // Refuse **before** persisting: a message that may never be sent has no
        // business sitting in the thread looking Pending. The gate is
        // `may_exchange_chat`, the same predicate the drain asks, so the two can
        // never disagree about whether this peer is reachable.
        permit_chat(self.trust.as_ref(), &device.id)?;
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
        // A file shared *in chat* is a chat operation: it puts a `FileRef` row
        // in the conversation, and the bytes only follow because of it. So it
        // is refused for the same reason and by the same predicate as text.
        permit_chat(self.trust.as_ref(), &device.id)?;
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
    ///
    /// The copy is **cancellable throughout**: its `TransferControl` is
    /// published to [`chat_staging`](Self#structfield.chat_staging) before the
    /// first byte and retired after the last, so
    /// [`chat_cancel`](Self::chat_cancel) can stop a multi-GB stage inside one
    /// 64 KiB buffer instead of watching it run to completion.
    async fn run_chat_file_send(self: Arc<Self>, device: Device, path: String, file_ref: FileRef) {
        let peer_id = device.id.0.clone();
        let id = file_ref.id.clone();
        let total = file_ref.size;
        let mut file_ref = file_ref;
        let ctrl = TransferControl::new();
        self.register_stage(&peer_id, &id, ctrl.clone());
        let (ptx, mut prx) = mpsc::unbounded_channel::<u64>();

        // Drain the progress channel alongside the copy rather than after it:
        // one report per 64 KiB is ~262k reports for a 16 GiB file, and an
        // unbounded channel nobody reads would hold every one of them. What
        // reaches the bridge is a throttled fraction of that — see
        // `StagingThrottle`, which is what stops a determinate bar from
        // drowning the event channel.
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
        let progress_peer = peer_id.clone();
        let progress_id = id.clone();
        let pump = async move {
            let mut throttle = StagingThrottle::new();
            while let Some(done) = prx.recv().await {
                if throttle.due(done, total) {
                    events::chat_staging(&progress_peer, &progress_id, done, total);
                }
            }
        };
        let (staged, ()) = tokio::join!(stage, pump);
        // Retire the stage and read its cancel flag under one lock, so a cancel
        // is either seen here or lands while the copy is still running — never
        // lost between the two.
        let cancelled = self.finish_stage(&peer_id, &id);

        let staged = match staged {
            Ok(Some(s)) => s,
            // The user deleted this conversation while the copy was running,
            // and it finished into a thread that no longer exists. `stage_file_
            // send` has already deleted the blob and queued nothing, so the
            // delete is honoured completely: no bytes left over, and no offer
            // put in front of the peer for a file this device will not send.
            // Nothing is emitted either — there is no row for an event to
            // describe, and inventing a status for one would put the deleted
            // thread back on the surface that just removed it.
            Ok(None) => {
                tracing::info!(
                    peer_id = %peer_id,
                    message_id = %id,
                    "chat file dropped: its conversation was deleted while it was being staged"
                );
                return;
            }
            Err(e) => {
                // A cancelled copy already deleted its own partial blob and
                // nothing was queued, so there is nothing left to release —
                // only a row to settle, and it says what the user did rather
                // than quoting a plumbing error at them.
                let reason = if cancelled {
                    "cancelled".to_string()
                } else {
                    e.to_string()
                };
                return self.fail_chat_file(&peer_id, &id, &reason);
            }
        };

        // The race the flag exists for: a cancel that arrived while the copy
        // was finishing. `stage_file_send` had already queued the entry by
        // then, so honouring the cancel means letting go of both here — the
        // cancel path could not, because neither existed when it ran.
        if cancelled {
            self.drop_queued_file(&id, &staged.staged_path);
            return self.fail_chat_file(&peer_id, &id, "cancelled");
        }

        // Queued. Say so — a surface showing a determinate staging bar has no
        // other way to learn the copy finished, and would sit at 99% until
        // something re-read history.
        events::chat_status(&peer_id, &id, chat_status_str(ChatStatus::Pending));

        // The online case is this queue draining without delay — and if the
        // peer is unreachable the file simply waits, exactly like text.
        let _ = self.chat_flush_peer(device).await;
    }

    /// Publish the control that stops an in-flight staging copy, so a cancel
    /// can reach it. Paired with [`finish_stage`](Self::finish_stage), which
    /// must run on every exit from the copy.
    fn register_stage(&self, peer_id: &str, id: &str, ctrl: TransferControl) {
        self.chat_staging
            .lock()
            .unwrap()
            .insert((peer_id.to_string(), id.to_string()), ctrl);
    }

    /// Retire a finished staging copy, reporting whether it was cancelled.
    ///
    /// The removal and the flag read happen under **one** lock acquisition, and
    /// [`cancel_stage`](Self::cancel_stage) sets the flag under the same lock.
    /// That is what makes the two total: a cancel either sets the flag before
    /// this reads it (so the caller cleans up) or finds the entry already gone
    /// (so it falls through to the queue, where the entry it needs now is).
    /// There is no interleaving in which the cancel is silently dropped.
    fn finish_stage(&self, peer_id: &str, id: &str) -> bool {
        self.chat_staging
            .lock()
            .unwrap()
            .remove(&(peer_id.to_string(), id.to_string()))
            .is_some_and(|ctrl| ctrl.is_cancelled())
    }

    /// Stop a staging copy that is running right now, if there is one for this
    /// exact `(peer, message)`. Returns whether one was found.
    ///
    /// The copy checks the flag between 64 KiB buffers and unlinks its own
    /// partial blob on the way out (`StagingStore::stage`), so an 8 GiB stage
    /// stops promptly and leaves nothing behind.
    fn cancel_stage(&self, peer_id: &str, id: &str) -> bool {
        match self
            .chat_staging
            .lock()
            .unwrap()
            .get(&(peer_id.to_string(), id.to_string()))
        {
            Some(ctrl) => {
                ctrl.cancel();
                true
            }
            None => false,
        }
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

        // Still wanted? The queue was read before this session was dialed, and
        // dialing plus the handshake is exactly when a user watching a row that
        // reads "Queued" reaches for Cancel. Re-reading here means a cancelled
        // file is never put in front of the peer at all — without it, cancel
        // would be honoured in every state except the seconds in which the file
        // is actually being offered.
        if !self.still_queued(&peer, &id) {
            session.close().await;
            self.release_file_slot(&peer.0);
            tracing::info!(message_id = %id, "queued file not offered: no longer queued");
            return;
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

    /// Whether `id` is still in `peer`'s queue.
    ///
    /// Read immediately before a queued file is offered, to catch a cancel that
    /// landed after the drain chose this file. An unreadable outbox answers
    /// **yes**, exactly like [`row_may_still_deliver`](Self::row_may_still_deliver):
    /// a transient read error must cost one drain tick, never a file the user
    /// is still waiting to send.
    fn still_queued(&self, peer: &DeviceId, id: &str) -> bool {
        match self.chat.outbox_for(peer) {
            Ok(entries) => entries.iter().any(|e| e.message_id == id),
            Err(e) => {
                tracing::warn!(error = %e, message_id = %id, "outbox unreadable; assuming queued");
                true
            }
        }
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
            Some(self.presence_wiring()),
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
        // The permission is asked here, of the **authenticated** peer, and on
        // every drain — so revoking `chat` for a device stops the very next
        // message rather than the next reconnect, and a message queued before
        // the revocation is not delivered after it. Nothing is discarded: the
        // outbox keeps it, exactly as it does for an unreachable peer, and it
        // flows again if the permission is restored.
        if !peerbeam_chat::may_exchange_chat(self.trust.as_ref(), &peer) {
            tracing::info!(
                peer_id = %peer.0,
                "not flushing chat: this device's `chat` permission was revoked"
            );
            session.close().await;
            return Vec::new();
        }
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

    /// Search this device's stored chat history:
    /// `{query, limit?}` → `{hits:[…], truncated, limit}`.
    ///
    /// **A pure local read.** It walks the conversation namespaces already on
    /// disk ([`ChatStore::search`]) and does nothing else: no channel is
    /// opened, no peer is dialled, nothing goes on the wire and nothing a peer
    /// could observe happens. A thread whose device is long gone is searchable;
    /// one the user deleted is not, because its rows are gone.
    ///
    /// `query` is a **case-insensitive substring** of a message's text body or
    /// a file message's name — never of a file's `local_path`, which is this
    /// device's filesystem layout rather than conversation content. It is not a
    /// regex. A missing or non-string `query` is `invalid_argument`; an *empty*
    /// or whitespace-only one is not — it finds nothing and says so, which is
    /// what a search box that has just been cleared should get rather than an
    /// error to render.
    ///
    /// `limit` is optional and defaults to [`DEFAULT_SEARCH_LIMIT`]. When given
    /// it must be an integer in `1..=MAX_SEARCH_LIMIT`; anything else is
    /// `invalid_argument` naming the bound. Deliberately not clamped: a caller
    /// asking for 10 000 has misunderstood something, and silently answering a
    /// different question than the one asked is how a surface comes to believe
    /// it is showing everything.
    ///
    /// **`truncated` is the field that matters.** It says there were more
    /// matches than `limit` allowed. A surface must show it: a bounded search
    /// that silently returns its first `n` reads as "that is all there is",
    /// which for a search over the user's own history is a wrong answer rather
    /// than a partial one. `limit` is echoed back so a surface can say how many
    /// it is showing without having to know whether it passed one.
    ///
    /// Hits are newest first, tie-broken by peer id then message id (see
    /// [`SearchHit`]), so paging and tests are stable. Each carries the
    /// conversation it was read from, so tapping it opens the right thread.
    ///
    /// [`ChatStore::search`]: peerbeam_chat::ChatStore::search
    /// Ask a device to make itself findable: `{peer, seconds?}` → `{sent}`.
    ///
    /// The other half of *find my device*. `sent: false` means the peer could
    /// not be reached or runs a build without ringing — whether it actually
    /// makes a sound is its own decision, and this device never learns it.
    ///
    /// **Not gated on the presence sharing opt-in.** That setting governs what
    /// this device reveals about itself; ringing asks something of the other
    /// device and reveals nothing here, so someone who shares no status can
    /// still find their own phone.
    pub fn presence_ring(self: &Arc<Self>, req: &Value) -> Op {
        let device = device_from(req.get("peer"))?;
        let seconds = req
            .get("seconds")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(15)
            .min(u64::from(peerbeam_presence::MAX_RING_SECONDS)) as u16;
        let me = self.clone();
        // Bounded for the reason `browse` is: this is a synchronous FFI call
        // and the app makes it from the isolate that draws. Ring is the sharpest
        // case of all — a person taps it *because* they cannot find the device,
        // which is exactly when the dial goes slowly, so the unbounded version
        // froze the interface precisely when it was being used.
        let sent = crate::runtime::block_on(async move {
            tokio::time::timeout(RING_BUDGET, me.deliver_ring(device, seconds))
                .await
                .unwrap_or(false)
        });
        Ok(json!({ "sent": sent }))
    }

    /// Dial and ring. Never fails the caller; every negative answer is
    /// "not sent".
    async fn deliver_ring(self: &Arc<Self>, device: Device, seconds: u16) -> bool {
        let meta = self.session(&format!("ring-{}", device.id.0), device.id.clone(), 0);
        let Ok(session) = crate::session_exec::dial(
            &self.quic,
            &self.rm,
            &device,
            &meta,
            self.identity(),
            self.enc.clone(),
            self.trust.clone(),
            Some(self.chat_wiring()),
            Some(self.presence_wiring()),
        )
        .await
        else {
            return false;
        };
        if !crate::session_exec::caps_support_ring(&session.capabilities) {
            return false;
        }
        // Built with this device's real trust and sharing settings even though
        // `ring` consults neither: the sender owns the channel bookkeeping both
        // paths share, and handing it a fake gate would leave a `beat` added
        // here later silently ungated.
        let mut sender = peerbeam_presence::PresenceSender::new(
            session.handle.clone(),
            session.peer_device.clone(),
            session.capabilities.clone(),
            self.trust.clone(),
            Arc::new(crate::presence::sharing_enabled),
            Arc::new(peerbeam_presence::Status::default),
        );
        sender.ring(seconds).await.is_ok()
    }

    /// Sync a folder with a peer, in both directions:
    /// `{peer, path, into}` → `{fetching, pushing, deleted, conflicts:[…]}`.
    ///
    /// **Bidirectional, with conflicts kept rather than resolved.** Each side
    /// carries per-file version vectors, so this can tell "their copy is newer"
    /// from "we both changed it" — the distinction a modification time cannot
    /// make, and the one that decides whether an edit survives.
    ///
    /// * Only they changed it → fetch.
    /// * Only we changed it → push.
    /// * They deleted something we had not touched since → delete ours.
    /// * **Both changed it** → their copy arrives as
    ///   `name.sync-conflict-<peer>.ext` and **ours is left untouched**. No
    ///   automatic rule can pick correctly, and every one of them loses
    ///   somebody's work.
    ///
    /// The local folder is rescanned first, so a file edited in an editor
    /// counts as this device's edit — otherwise the next sync would quietly
    /// overwrite it with the peer's older copy.
    pub fn sync_pull(self: &Arc<Self>, req: &Value) -> Op {
        let device = device_from(req.get("peer"))?;
        let path = req
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "path required".into()))?
            .to_string();
        let into = std::path::PathBuf::from(
            req.get("into")
                .and_then(|v| v.as_str())
                .ok_or((Code::InvalidArgument, "into required".into()))?,
        );
        if !into.is_dir() {
            return Err((
                Code::InvalidArgument,
                format!("{} is not a directory", into.display()),
            ));
        }

        // **One deadline for the whole operation.** Each network wait inside had
        // a timeout of its own and none bounded the total, so a folder with many
        // files against a peer that stalls part-way could hold this call — and
        // therefore the isolate that draws — for far longer than any single
        // timeout suggests.
        //
        // It does **not** bound the rescan below: that is synchronous CPU and
        // disk work, and a timeout cannot interrupt it. A first sync of a very
        // large folder still hashes every byte before this returns. Bounding the
        // waits is what can be done here; the fix for the rest is to stop
        // calling blocking FFI from the drawing isolate.
        let deadline = tokio::time::Instant::now() + SYNC_BUDGET;
        let left = move || deadline.saturating_duration_since(tokio::time::Instant::now());

        // Rescan before anything else: an edit made outside PeerBeam is exactly
        // as real as one received, and a sync that ignored it would overwrite
        // the user's work with a peer's older copy.
        let index = self.sync_index();
        index
            .rescan(&path, &into)
            .map_err(|e| (Code::Internal, e.to_string()))?;
        let local = index
            .load(&path)
            .map_err(|e| (Code::Internal, e.to_string()))?;

        let me = self.clone();
        let manifest_path = path.clone();
        let manifest = crate::runtime::block_on(async move {
            tokio::time::timeout(left(), me.fetch_manifest(device.clone(), manifest_path))
                .await
                .ok()
                .flatten()
        });
        let Some(manifest) = manifest else {
            return Err((
                Code::Connection,
                format!("could not get a manifest for {path}"),
            ));
        };

        let remote: Vec<peerbeam_sync::RemoteFile> = manifest
            .files
            .iter()
            .map(|f| peerbeam_sync::RemoteFile {
                path: f.path.clone(),
                size: f.size,
                version: f.version.clone(),
                content: f.content.clone(),
                deleted: f.deleted,
            })
            .collect();
        let peer_name = req
            .get("peer")
            .and_then(|p| p.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("peer");
        let actions = peerbeam_sync::reconcile(&local, &remote, peer_name);

        let remote_versions: std::collections::BTreeMap<String, peerbeam_sync::VersionVector> =
            remote
                .iter()
                .map(|f| (f.path.clone(), f.version.clone()))
                .collect();
        let outcome = peerbeam_sync::apply_local(&index, &path, &into, &actions, &remote_versions)
            .map_err(|e| (Code::Internal, e.to_string()))?;

        // Ask for everything the plan wants from the peer. A conflict is
        // fetched too — under its conflict name — because keeping both copies
        // means actually having both.
        let device = device_from(req.get("peer"))?;
        // **The remote name and the local name are two different things, and a
        // conflict is the case where they differ.**
        //
        // This used to collapse `Conflict { path, keep_as }` to `path` and pass
        // that one string as both, so the peer's version was written straight
        // over the user's file — while the surface reported that both copies
        // had been kept, naming a `.sync-conflict-` file that was never
        // created. The user's edit was gone and nothing said so. The CLI has
        // always carried the pair (`browse.rs`); this is the same shape.
        let wanted: Vec<(String, String)> = actions
            .iter()
            .filter_map(|a| match a {
                peerbeam_sync::Action::Fetch { path } => Some((path.clone(), path.clone())),
                peerbeam_sync::Action::Conflict { path, keep_as } => {
                    Some((path.clone(), keep_as.clone()))
                }
                _ => None,
            })
            .collect();
        let me = self.clone();
        let request_path = path.clone();
        let into_for_fetch = into.clone();
        crate::runtime::block_on(async move {
            // Whatever arrived before the deadline is kept: files land as they
            // are received, so giving up here leaves a partially-synced folder
            // rather than undoing anything. The next sync picks up the rest.
            let _ = tokio::time::timeout(
                left(),
                me.request_files(device, request_path, into_for_fetch, wanted),
            )
            .await;
        });

        Ok(json!({
            "fetching": outcome.fetching,
            "renamed": outcome.renamed,
            "pushing": outcome.pushing,
            "deleted": outcome.deleted,
            "conflicts": outcome.conflicts,
            "truncated": manifest.truncated,
        }))
    }

    /// Keep a folder in sync until told to stop:
    /// `{peer, path, into, interval?}` → `{watching:true}`.
    ///
    /// **A thread, not a task.** [`sync_pull`](Self::sync_pull) blocks on the
    /// runtime internally, and blocking on a runtime from inside it deadlocks —
    /// so the loop gets its own thread and the async work stays where it
    /// already worked.
    ///
    /// A file is only synced once it has stopped changing, so saving a large
    /// file part-way through a poll neither syncs a half-written copy nor
    /// raises the version vector once per observation and manufactures a
    /// conflict out of an ordinary save.
    pub fn sync_watch(self: &Arc<Self>, req: &Value) -> Op {
        let path = req
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "path required".into()))?
            .to_string();
        let into = req
            .get("into")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "into required".into()))?
            .to_string();
        if !std::path::Path::new(&into).is_dir() {
            return Err((Code::InvalidArgument, format!("{into} is not a directory")));
        }
        // Clamped: a one-second poll would re-hash the folder continuously and
        // cost more than the change it is trying to notice.
        let interval = req
            .get("interval")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .max(5);
        let key = watch_key(&path, &into);

        let mut watches = self.watches.lock().unwrap();
        if watches.contains_key(&key) {
            // Already watching. Succeeding is right: the state the caller asked
            // for is the state that holds, and a second toggle should not error.
            return Ok(json!({ "watching": true, "already": true }));
        }
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        watches.insert(key, stop.clone());
        drop(watches);

        let me = self.clone();
        let request = req.clone();
        let root = std::path::PathBuf::from(&into);
        std::thread::spawn(move || {
            let mut settling = peerbeam_sync::Settling::new();
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let settled = settling.observe(&observe_folder(&root));
                if settled.is_empty() && settling.unsettled() > 0 {
                    // Something is still being written. Waiting is the point.
                    std::thread::sleep(std::time::Duration::from_secs(interval));
                    continue;
                }
                // A failed pass must not end the watch: the peer may simply be
                // asleep, and a watch that quits on the first unreachable
                // device is one nobody can leave running.
                if let Err((_, e)) = me.sync_pull(&request) {
                    tracing::debug!(error = %e, "watched sync pass failed; will retry");
                }
                std::thread::sleep(std::time::Duration::from_secs(interval));
            }
        });
        Ok(json!({ "watching": true }))
    }

    /// Stop watching a folder: `{path, into}` → `{watching:false}`.
    pub fn sync_unwatch(self: &Arc<Self>, req: &Value) -> Op {
        let path = req
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "path required".into()))?;
        let into = req
            .get("into")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "into required".into()))?;
        let existed = self
            .watches
            .lock()
            .unwrap()
            .remove(&watch_key(path, into))
            .map(|stop| stop.store(true, std::sync::atomic::Ordering::Relaxed))
            .is_some();
        Ok(json!({ "watching": false, "was_watching": existed }))
    }

    /// Which folders are being watched: `{watching:[{path,into}]}`.
    pub fn sync_watches(self: &Arc<Self>) -> Op {
        let watches = self.watches.lock().unwrap();
        let list: Vec<Value> = watches
            .keys()
            .filter_map(|k| k.split_once('\u{1}'))
            .map(|(p, i)| json!({ "path": p, "into": i }))
            .collect();
        Ok(json!({ "watching": list }))
    }

    /// The per-device sync index, keyed by this device's own id — the counter
    /// its own edits raise.
    fn sync_index(&self) -> peerbeam_sync::SyncIndex {
        peerbeam_sync::SyncIndex::new(self.appstore.clone(), &self.identity().device_id.0)
    }

    /// Dial and fetch a manifest. `None` when the peer cannot be reached or
    /// does not implement folder sync.
    async fn fetch_manifest(
        self: &Arc<Self>,
        device: Device,
        path: String,
    ) -> Option<peerbeam_sync::Manifest> {
        let session = self.sync_session(&device).await?;
        crate::session_exec::request_manifest(&session, &path).await
    }

    /// Ask the peer for each wanted file. Best effort: the bytes arrive as
    /// ordinary transfers, so nothing here waits for them.
    async fn request_files(
        self: &Arc<Self>,
        device: Device,
        folder: String,
        into: std::path::PathBuf,
        // `(remote path, where to write it)`. The two differ for a conflict:
        // the peer's copy lands beside the user's under its conflict name,
        // never on top of it.
        wanted: Vec<(String, String)>,
    ) {
        let Some(session) = self.sync_session(&device).await else {
            return;
        };
        let index = self.sync_index();
        for (rel, write_to) in wanted {
            let remote = format!("{folder}/{rel}");
            // Delta first; the whole file only when it cannot be used. Every
            // reason to give up — no chunk map, a chunk that never arrived,
            // bytes that did not verify — is a reason to send the file the slow
            // way, never a reason to stop the sync.
            if fetch_by_delta(&session, &index, &folder, &into, &remote, &write_to)
                .await
                .is_none()
            {
                // The whole-file path writes under the name the *sender*
                // states, so a conflict cannot use it: it would land on the
                // user's file again. Skipping loses the peer's copy of a
                // conflicting file, which is recoverable by syncing again;
                // overwriting is not.
                if write_to == rel {
                    crate::session_exec::request_file(&session, &remote).await;
                } else {
                    tracing::warn!(
                        path = %rel,
                        keep_as = %write_to,
                        "conflict copy not fetched: delta failed and a whole-file \
                         request would overwrite the local file"
                    );
                }
            }
        }
    }

    /// A session for folder sync, or `None` if the peer cannot take one.
    async fn sync_session(
        self: &Arc<Self>,
        device: &Device,
    ) -> Option<crate::session_exec::Session> {
        let meta = self.session(&format!("sync-{}", device.id.0), device.id.clone(), 0);
        let session = crate::session_exec::dial(
            &self.quic,
            &self.rm,
            device,
            &meta,
            self.identity(),
            self.enc.clone(),
            self.trust.clone(),
            Some(self.chat_wiring()),
            Some(self.presence_wiring()),
        )
        .await
        .ok()?;
        crate::session_exec::caps_support_sync(&session.capabilities).then_some(session)
    }

    /// Ask a device what is in one of its shared folders:
    /// `{peer, path?}` → `{path, entries:[…], truncated, denied}`.
    ///
    /// `path` is **share-relative** — `photos/2026`, never an absolute path.
    /// Empty asks what the device shares at all.
    ///
    /// An empty answer with `denied: true` means the device is not showing
    /// this: it may not have granted us `browse`, may share nothing, or the
    /// path may not exist. **Those are deliberately indistinguishable** — a
    /// caller able to tell them apart could map a filesystem it may not see,
    /// one refused request at a time.
    pub fn browse_list(self: &Arc<Self>, req: &Value) -> Op {
        let device = device_from(req.get("peer"))?;
        let path = req
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if path.len() > peerbeam_browse::MAX_PATH {
            return Err((
                Code::InvalidArgument,
                format!("path must be at most {} bytes", peerbeam_browse::MAX_PATH),
            ));
        }
        let me = self.clone();
        // **Bounded, because this call blocks whoever made it.** Every `pb_*`
        // function is synchronous, and the Flutter SDK calls this one straight
        // from the UI isolate, so the whole wait is frozen frames. `dial` tries
        // each of a device's routes in turn with its own connect timeout, so a
        // peer that is merely asleep — the ordinary case for "browse a device
        // that is not there" — could hold the interface for the sum of them.
        //
        // The cap does not make this asynchronous and must not be mistaken for
        // a fix: the real one is to stop calling blocking FFI from the isolate
        // that draws. It bounds the damage to a wait a person can sit through,
        // and turns "the app hung" into the honest "could not ask the device",
        // which is the same answer they would have got at the end anyway.
        let answer = crate::runtime::block_on(async move {
            tokio::time::timeout(BROWSE_BUDGET, me.ask_browse(device, path.clone()))
                .await
                .ok()
                .flatten()
        });
        match answer {
            Some(r) => Ok(crate::browse::response_dto(&r)),
            None => Err(crate::browse::unreachable(
                req.get("path").and_then(|v| v.as_str()).unwrap_or(""),
            )),
        }
    }

    /// Dial, ask, and wait for the one answer. `None` when the device cannot be
    /// reached or does not implement browsing.
    async fn ask_browse(
        self: &Arc<Self>,
        device: Device,
        path: String,
    ) -> Option<peerbeam_browse::ListResponse> {
        let meta = self.session(&format!("browse-{}", device.id.0), device.id.clone(), 0);
        let session = crate::session_exec::dial(
            &self.quic,
            &self.rm,
            &device,
            &meta,
            self.identity(),
            self.enc.clone(),
            self.trust.clone(),
            Some(self.chat_wiring()),
            Some(self.presence_wiring()),
        )
        .await
        .ok()?;
        if !crate::session_exec::caps_support_browse(&session.capabilities) {
            return None;
        }
        crate::session_exec::request_listing(&session, &path).await
    }

    /// Sync notes with a peer: `{peer}` → `{sent}`.
    ///
    /// Sends this device's whole note set — tombstones included, because a
    /// deletion is a fact about the set — and the peer answers with its own,
    /// which the handler merges. Two passes, then done.
    ///
    /// `sent: false` is a normal answer, not a failure: the user may not have
    /// granted this device the `notes` permission, the peer may be unreachable,
    /// or its build may predate notes entirely.
    ///
    /// The permission is checked here *and* on the way in on both sides. This
    /// check is about what leaves; the handler's is about what may be written.
    pub fn notes_sync(self: &Arc<Self>, req: &Value) -> Op {
        let device = device_from(req.get("peer"))?;
        if !peerbeam_notes::may_sync_notes(self.trust.as_ref(), &device.id) {
            return Ok(json!({ "sent": false }));
        }
        let mine = self
            .notes
            .all()
            .map_err(|e| (Code::Internal, e.to_string()))?;
        let batches = peerbeam_notes::NoteBatch::split(mine, false);
        let me = self.clone();
        let sent = crate::runtime::block_on(async move { me.deliver_notes(device, batches).await });
        Ok(json!({ "sent": sent }))
    }

    /// Dial the peer and push a note exchange. Never fails the caller: every
    /// negative answer is "not sent".
    async fn deliver_notes(
        self: &Arc<Self>,
        device: Device,
        batches: Vec<peerbeam_notes::NoteBatch>,
    ) -> bool {
        let meta = self.session(&format!("notes-{}", device.id.0), device.id.clone(), 0);
        let Ok(session) = crate::session_exec::dial(
            &self.quic,
            &self.rm,
            &device,
            &meta,
            self.identity(),
            self.enc.clone(),
            self.trust.clone(),
            Some(self.chat_wiring()),
            Some(self.presence_wiring()),
        )
        .await
        else {
            return false;
        };
        if !crate::session_exec::caps_support_notes(&session.capabilities) {
            return false;
        }
        // Re-asked of the **authenticated** identity, which is the device the
        // permission is actually about: the pre-dial id is whatever the caller
        // named, and for an address-dialled peer it is a placeholder.
        if !peerbeam_notes::may_sync_notes(self.trust.as_ref(), &session.peer_device) {
            return false;
        }
        crate::session_exec::send_note_batches(&session.handle, &batches)
            .await
            .is_ok()
    }

    /// Every live note, newest edit first: `{}` → `{notes: [...]}`.
    ///
    /// Tombstones are not included. They exist so a deletion can reach a peer,
    /// not to be read back as notes.
    /// A required string field, or a refusal naming which one is missing.
    fn str_field(req: &Value, key: &str) -> Result<String, (Code, String)> {
        req.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .ok_or((Code::InvalidArgument, format!("{key} required")))
    }

    /// `{"device","mac"}` → `{"mac"}`. Records where to send a wake packet.
    ///
    /// Grants nothing and tells the device nothing; it is a note this machine
    /// keeps so it can start one of the user's own machines.
    pub fn wake_set(&self, req: &Value) -> Op {
        let device = DeviceId::from(Self::str_field(req, "device")?);
        let raw = Self::str_field(req, "mac")?;
        let mac: peerbeam_wake::MacAddress = raw
            .parse()
            .map_err(|e: peerbeam_wake::MacError| (Code::InvalidArgument, e.to_string()))?;
        self.wake
            .remember(&device, mac, chrono::Utc::now())
            .map_err(|e| (Code::Storage, e.to_string()))?;
        Ok(json!({ "mac": mac.to_string() }))
    }

    /// `{"device"}` → `{"mac": string|null}` — the address recorded for a
    /// device, if any.
    ///
    /// **A write-only setting is one nobody keeps using.** The address could be
    /// stored and sent with, and never read back, so every surface that wanted
    /// to show it had to ask the user to type it again — and a MAC typed from
    /// memory is a MAC typed wrong. Null means none is recorded, which is the
    /// difference between "wake is ready" and "wake needs setting up".
    pub fn wake_get(&self, req: &Value) -> Op {
        let device = DeviceId::from(Self::str_field(req, "device")?);
        let mac = self
            .wake
            .lookup(&device)
            .map_err(|e| (Code::Storage, e.to_string()))?
            .map(|record| record.mac.to_string());
        Ok(json!({ "mac": mac }))
    }

    /// `{"device"}` → `{"forgotten":bool}`.
    pub fn wake_forget(&self, req: &Value) -> Op {
        let device = DeviceId::from(Self::str_field(req, "device")?);
        Ok(json!({
            "forgotten": self
                .wake
                .forget(&device)
                .map_err(|e| (Code::Storage, e.to_string()))?
        }))
    }

    /// `{"device"}` → `{"mac", "sent_to":[…]}`.
    ///
    /// **Reports what was sent, never that the device woke.** Wake-on-LAN has
    /// no reply, so a surface that showed "woken" would be inventing a fact.
    /// The confirmation is the device appearing in discovery.
    pub fn wake_send(&self, req: &Value) -> Op {
        let device = DeviceId::from(Self::str_field(req, "device")?);
        let socket = peerbeam_wake::UdpBroadcast::bind()
            .map_err(|e| (Code::Connection, format!("broadcast socket: {e}")))?;
        let attempt = peerbeam_wake::wake_device(
            &self.wake,
            self.trust.as_ref(),
            &socket,
            &device,
            std::net::Ipv4Addr::BROADCAST,
        )
        .map_err(|e| match e {
            peerbeam_wake::WakeError::Storage(_) => (Code::Storage, e.to_string()),
            peerbeam_wake::WakeError::Send(_) => (Code::Connection, e.to_string()),
            _ => (Code::InvalidArgument, e.to_string()),
        })?;
        Ok(json!({
            "mac": attempt.mac.to_string(),
            "sent_to": attempt.sent_to.iter().map(ToString::to_string).collect::<Vec<_>>(),
        }))
    }

    /// `{"peer_id"}` → `{"seconds"|null}` — this conversation's window.
    pub fn chat_retention_get(&self, req: &Value) -> Op {
        let peer = DeviceId::from(Self::str_field(req, "peer_id")?);
        let r = self
            .chat
            .retention(&peer)
            .map_err(|e| (Code::Storage, e.to_string()))?;
        Ok(json!({ "seconds": r.ttl_secs }))
    }

    /// `{"peer_id","seconds"?}` → `{"seconds"|null}`. Omit `seconds` to turn it
    /// off.
    ///
    /// The window is **local**: nothing is sent to the peer, and no surface may
    /// suggest the peer's copy is affected.
    pub fn chat_retention_set(&self, req: &Value) -> Op {
        let peer = DeviceId::from(Self::str_field(req, "peer_id")?);
        let next = match req.get("seconds").and_then(Value::as_u64) {
            None => peerbeam_chat::Retention::OFF,
            Some(secs) => peerbeam_chat::Retention::for_secs(secs)
                .map_err(|e| (Code::InvalidArgument, e.to_string()))?,
        };
        self.chat
            .set_retention(&peer, next)
            .map_err(|e| (Code::Storage, e.to_string()))?;
        Ok(json!({ "seconds": next.ttl_secs }))
    }

    /// `{"peer_id"?}` → `{"messages","queued"}`. Deletes what has aged out.
    pub fn chat_prune(&self, req: &Value) -> Op {
        let now = chrono::Utc::now();
        let pruned = match req.get("peer_id").and_then(Value::as_str) {
            Some(p) => self
                .chat
                .prune(&DeviceId::from(p.to_string()), now)
                .map_err(|e| (Code::Storage, e.to_string()))?,
            None => peerbeam_chat::prune_all_conversations(&self.chat, &self.staging, now)
                .map_err(|e| (Code::Storage, e.to_string()))?,
        };
        Ok(json!({ "messages": pruned.records, "queued": pruned.queued }))
    }

    /// `{}` → `{"spaces":[SpaceView…]}`.
    ///
    /// A Space is a label this device keeps over peers it already trusts. It is
    /// never sent anywhere and no peer learns it exists, which is why there is
    /// no "join" and no member list on the wire.
    pub fn spaces_list(&self, _req: &Value) -> Op {
        Ok(json!({ "spaces": self.spaces.list().map_err(space_err)? }))
    }

    /// `{"name"}` → `{"space":SpaceView}`.
    pub fn spaces_create(&self, req: &Value) -> Op {
        let name = Self::str_field(req, "name")?;
        Ok(json!({ "space": self.spaces.create(&name).map_err(space_err)? }))
    }

    /// `{"id","name"}` → `{"space":SpaceView}`.
    pub fn spaces_rename(&self, req: &Value) -> Op {
        let id = Self::str_field(req, "id")?;
        let name = Self::str_field(req, "name")?;
        Ok(json!({ "space": self.spaces.rename(&id, &name).map_err(space_err)? }))
    }

    /// `{"id"}` → `{"deleted":bool}`.
    pub fn spaces_delete(&self, req: &Value) -> Op {
        let id = Self::str_field(req, "id")?;
        Ok(json!({ "deleted": self.spaces.delete(&id).map_err(space_err)? }))
    }

    /// `{"id","device"}` → `{"added":bool}`.
    pub fn spaces_add_member(&self, req: &Value) -> Op {
        let id = Self::str_field(req, "id")?;
        let device = DeviceId::from(Self::str_field(req, "device")?);
        Ok(json!({ "added": self.spaces.add_member(&id, &device).map_err(space_err)? }))
    }

    /// `{"id","device"}` → `{"removed":bool}`.
    pub fn spaces_remove_member(&self, req: &Value) -> Op {
        let id = Self::str_field(req, "id")?;
        let device = DeviceId::from(Self::str_field(req, "device")?);
        Ok(json!({ "removed": self.spaces.remove_member(&id, &device).map_err(space_err)? }))
    }

    /// `{}` → `{"groups":[…],"invites":[…]}`.
    ///
    /// Both in one call because a surface showing groups must show pending
    /// invitations beside them — an invitation is the only way into a group,
    /// and one that arrived while the app was closed would otherwise be
    /// invisible until something else happened to fetch it.
    ///
    /// Each group carries `reachable` and `unreachable` so a surface can name
    /// the members it cannot message rather than quietly showing a shorter
    /// list (A2, condition 3).
    pub fn groups_list(&self, _req: &Value) -> Op {
        let groups = self.groups.list().map_err(group_err)?;
        let mut out = Vec::with_capacity(groups.len());
        for g in &groups {
            let (live, stale) = self.groups.reachable(&g.id).map_err(group_err)?;
            out.push(json!({
                "id": g.id,
                "name": g.name,
                "members": g.members.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
                "reachable": live.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
                "unreachable": stale.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
            }));
        }
        let invites: Vec<Value> = self
            .groups
            .invites()
            .map_err(group_err)?
            .iter()
            .map(|i| {
                json!({
                    "group": i.group,
                    "name": i.name,
                    "from": i.from.0,
                    "members": i.members.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
                    "at": i.at,
                })
            })
            .collect();
        Ok(json!({ "groups": out, "invites": invites }))
    }

    /// `{"name"}` → `{"group":{…}}`.
    ///
    /// Holds only this device. Members are added by inviting them and join when
    /// **they** accept — a create that took a member list would enrol other
    /// people's devices in something they never agreed to (A2, condition 4).
    pub fn groups_create(&self, req: &Value) -> Op {
        let name = Self::str_field(req, "name")?;
        let g = self.groups.create(&name).map_err(group_err)?;
        Ok(json!({ "group": { "id": g.id, "name": g.name } }))
    }

    /// `{"id","name"}` → `{"group":{…}}`. Local only; names are not shared.
    pub fn groups_rename(&self, req: &Value) -> Op {
        let id = Self::str_field(req, "id")?;
        let name = Self::str_field(req, "name")?;
        let g = self.groups.rename(&id, &name).map_err(group_err)?;
        Ok(json!({ "group": { "id": g.id, "name": g.name } }))
    }

    /// `{"group"}` → `{"declined":bool}` — turn an invitation down.
    ///
    /// Local and silent: the inviter is **not** told. Telling them would say
    /// "this device saw your invitation and refused", which is a fact about the
    /// user that nobody asked to publish; ignoring an offer is allowed to look
    /// exactly like never having seen it.
    pub fn groups_decline(&self, req: &Value) -> Op {
        let group = Self::str_field(req, "group")?;
        Ok(json!({ "declined": self.groups.forget_invite(&group).map_err(group_err)? }))
    }

    /// The `peers` array a group verb is given: how to reach each member.
    ///
    /// The engine holds a roster of **ids**; routes come from discovery, which
    /// the app already has. Passing them in keeps one device list rather than
    /// two that can disagree, and matches how every other peer-addressed call
    /// here works.
    ///
    /// Absent or empty is not an error — it means "nobody reachable right now",
    /// and the members simply find out at next contact. A group that refused to
    /// be left because nobody was online would trap the user in it.
    fn peers_field(req: &Value) -> Result<Vec<Device>, (Code, String)> {
        let Some(list) = req.get("peers").and_then(|v| v.as_array()) else {
            return Ok(Vec::new());
        };
        // One bad entry is skipped rather than failing the verb: the others are
        // still worth telling, and an app that sent a malformed peer should not
        // be able to stop somebody leaving a group.
        Ok(list
            .iter()
            .filter_map(|p| device_from(Some(p)).ok())
            .collect())
    }

    /// Dial one member and push a single membership frame.
    ///
    /// **Background, never blocking.** Every `pb_*` call is synchronous and the
    /// app makes them from the isolate that draws, so a verb that dialled
    /// inline would freeze the interface for as long as the peer took to
    /// answer — the failure mode `BROWSE_BUDGET` and `RING_BUDGET` exist to
    /// bound. These spawn and report through an event instead, so the UI stays
    /// alive and learns what happened when it happens.
    fn group_control(
        self: &Arc<Self>,
        device: Device,
        group: String,
        kind: &'static str,
        message_type: peerbeam_domain::session::MessageType,
        payload: Vec<u8>,
    ) {
        let me = self.clone();
        crate::runtime::spawn(async move {
            let meta = me.session(&format!("group-{}", device.id.0), device.id.clone(), 0);
            let outcome = match crate::session_exec::dial(
                &me.quic,
                &me.rm,
                &device,
                &meta,
                me.identity(),
                me.enc.clone(),
                me.trust.clone(),
                None,
                None,
            )
            .await
            {
                Ok(session) => {
                    // Asked of the identity that actually answered, not the id
                    // dialled: a route can resolve to a different device, and
                    // the permission is about whoever picked up.
                    let peer = DeviceId::from(session.peer_id.clone());
                    if peerbeam_chat::may_exchange_chat(me.trust.as_ref(), &peer) {
                        peerbeam_chat::send_foreign(&session.handle, message_type, payload)
                            .await
                            .map_err(|e| e.to_string())
                    } else {
                        Err("this device may not exchange messages".to_string())
                    }
                }
                Err((_, why)) => Err(why),
            };
            events::event(&json!({
                "type": "group_control",
                "kind": kind,
                "group": group,
                "device": device.id.0,
                "ok": outcome.is_ok(),
                "error": outcome.err(),
                "timestamp": timestamp(),
            }));
        });
    }

    /// `{"id","peer"}` → `{"queued":true}` — offer a device a place.
    ///
    /// An **offer**, not an enrolment: nothing changes on their device until
    /// their own user accepts (A2, condition 4). The roster travels with it, so
    /// the invitee learns who is already in the group — and everyone in it
    /// learns them if they accept. A surface must say so before sending.
    pub fn groups_invite(self: &Arc<Self>, req: &Value) -> Op {
        let id = Self::str_field(req, "id")?;
        let device = device_from(req.get("peer"))?;
        let group = self.groups.get(&id).map_err(group_err)?;
        let invite = peerbeam_groups::GroupInvite {
            group: group.id.clone(),
            name: group.name.clone(),
            members: group.members.clone(),
        };
        let payload = serde_json::to_vec(&invite).map_err(|e| (Code::Internal, e.to_string()))?;
        self.group_control(
            device,
            group.id,
            "invite",
            peerbeam_groups::GroupInvite::message_type(),
            payload,
        );
        Ok(json!({ "queued": true }))
    }

    /// `{"group"}` → `{"group":{…}}` — accept an invitation.
    ///
    /// The roster is adopted **before** anyone is told, so a join whose
    /// announcements half-fail leaves this device in the group it agreed to
    /// join rather than in nothing. Members who did not hear find out at next
    /// contact; there is nobody to ask for the truth, which is the point.
    pub fn groups_accept(self: &Arc<Self>, req: &Value) -> Op {
        let group_id = Self::str_field(req, "group")?;
        let pending = self
            .groups
            .invite(&group_id)
            .map_err(group_err)?
            .ok_or((Code::InvalidArgument, "no such invitation".into()))?;
        let joined = self
            .groups
            .adopt(&pending.group, &pending.name, &pending.members)
            .map_err(group_err)?;
        self.groups
            .forget_invite(&pending.group)
            .map_err(group_err)?;

        let announce = peerbeam_groups::GroupJoined {
            group: joined.id.clone(),
        };
        let payload = serde_json::to_vec(&announce).map_err(|e| (Code::Internal, e.to_string()))?;
        // **The caller supplies how to reach each member.** The engine holds a
        // roster of ids, not routes: routes come from discovery, which the app
        // already has in front of it. Matching the ids here rather than
        // re-resolving them keeps one device list rather than two that can
        // disagree — and a member the app cannot see is simply not told yet,
        // which is the same "they find out at next contact" this design accepts
        // everywhere else.
        for device in Self::peers_field(req)?
            .into_iter()
            .filter(|d| pending.members.contains(&d.id))
        {
            self.group_control(
                device,
                joined.id.clone(),
                "joined",
                peerbeam_groups::GroupJoined::message_type(),
                payload.clone(),
            );
        }
        events::event(&json!({ "type": "groups_changed", "timestamp": timestamp() }));
        Ok(json!({ "group": { "id": joined.id, "name": joined.name } }))
    }

    /// `{"id"}` → `{"left":true}` — tell the members, then forget it here.
    ///
    /// Forgotten whether or not anyone heard: leaving is a decision about this
    /// device. A member that missed the message may keep sending, and the only
    /// thing that refuses it is withholding `chat` from that device — a
    /// decision about the device rather than about a label.
    pub fn groups_leave(self: &Arc<Self>, req: &Value) -> Op {
        let id = Self::str_field(req, "id")?;
        let group = self.groups.get(&id).map_err(group_err)?;
        let msg = peerbeam_groups::GroupLeft {
            group: group.id.clone(),
        };
        let payload = serde_json::to_vec(&msg).map_err(|e| (Code::Internal, e.to_string()))?;
        let (live, _stale) = self.groups.reachable(&group.id).map_err(group_err)?;
        for device in Self::peers_field(req)?
            .into_iter()
            .filter(|d| live.contains(&d.id))
        {
            self.group_control(
                device,
                group.id.clone(),
                "left",
                peerbeam_groups::GroupLeft::message_type(),
                payload.clone(),
            );
        }
        self.groups.forget(&group.id).map_err(group_err)?;
        events::event(&json!({ "type": "groups_changed", "timestamp": timestamp() }));
        Ok(json!({ "left": true }))
    }

    /// `{"id","text"}` → `{"id":…,"sent":N,"skipped":[…]}` — message everyone.
    ///
    /// **N ordinary sends, one id.** Each copy is enqueued in the member's own
    /// outbox, so an unreachable member's copy is delivered later by the same
    /// drain a one-to-one message uses — because that is exactly what it is.
    /// The message id is minted once and shared, which is what lets a
    /// transcript show it once instead of once per member.
    ///
    /// Members this device may not message are **named** in `skipped`, never
    /// silently dropped (A2, condition 3).
    pub fn groups_send(self: &Arc<Self>, req: &Value) -> Op {
        let id = Self::str_field(req, "id")?;
        let text = Self::str_field(req, "text")?;
        let group = self.groups.get(&id).map_err(group_err)?;
        let (live, stale) = self.groups.reachable(&group.id).map_err(group_err)?;
        if live.is_empty() {
            return Err((
                Code::PermissionDenied,
                format!("{} has nobody this device may message", group.name),
            ));
        }
        let mut msg = peerbeam_chat::ChatMessage::new(&text)
            .map_err(|e| (Code::InvalidArgument, e.to_string()))?;
        msg.group = Some(group.id.clone());

        // **Enqueued and left to the drain.** Every copy goes into the
        // member's own outbox, and the periodic flush already delivers those
        // the moment a peer is online — the same machinery a one-to-one message
        // uses, because that is exactly what each copy is. Dialling here would
        // duplicate it and would need routes the engine does not hold.
        for member in &live {
            self.chat
                .enqueue(member, &msg)
                .map_err(|e| (Code::Internal, e.to_string()))?;
        }
        Ok(json!({
            "id": msg.id,
            "sent": live.len(),
            "skipped": stale.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
        }))
    }

    /// `{"group"}` → `{"messages":[…]}` — a group's transcript.
    ///
    /// Gathered across the members it was sent to, because that is where the
    /// copies are: a group message is N one-to-one sends, so there is no group
    /// namespace to read. An outgoing message appears **once** despite having
    /// one row per recipient — they share an id.
    pub fn groups_history(&self, req: &Value) -> Op {
        let group = Self::str_field(req, "group")?;
        let rows = self
            .chat
            .group_history(&group)
            .map_err(|e| (Code::Storage, e.to_string()))?;
        Ok(json!({ "messages": rows.iter().map(crate::events::record_dto).collect::<Vec<_>>() }))
    }

    /// `{"device","mine":bool}` → `{"changed":bool}`.
    ///
    /// A label, not a grant: marking a device as mine widens no permission and
    /// tells that device nothing. See `TrustRecord::mine`.
    pub fn trust_set_mine(&self, req: &Value) -> Op {
        let device = DeviceId::from(Self::str_field(req, "device")?);
        let mine = req.get("mine").and_then(Value::as_bool).ok_or((
            Code::InvalidArgument,
            "expected \"mine\": true|false".to_string(),
        ))?;
        let changed = self
            .trust
            .set_mine(&device, mine)
            .map_err(|e| (Code::Internal, e.to_string()))?;
        Ok(json!({ "changed": changed }))
    }

    /// `{}` → `{"devices":[…]}` — the devices the user marked as their own.
    pub fn trust_my_devices(&self, _req: &Value) -> Op {
        let devices = self
            .trust
            .my_devices()
            .map_err(|e| (Code::Internal, e.to_string()))?;
        Ok(json!({ "devices": devices }))
    }

    pub fn notes_list(&self, _req: &Value) -> Op {
        let notes = self
            .notes
            .list()
            .map_err(|e| (Code::Internal, e.to_string()))?;
        Ok(json!({ "notes": notes }))
    }

    /// Create a note: `{title?, body}` → `{id}`.
    pub fn notes_create(&self, req: &Value) -> Op {
        let title = req.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let body = req
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "body required".into()))?;
        let note = peerbeam_notes::Note::new(title, body)
            .map_err(|e| (Code::InvalidArgument, e.to_string()))?;
        self.notes
            .put(&note)
            .map_err(|e| (Code::Internal, e.to_string()))?;
        Ok(json!({ "id": note.id }))
    }

    /// Replace a note's content: `{id, title?, body}` → `{updated}`.
    ///
    /// `updated: false` means there is no such note, or it has been deleted —
    /// editing a tombstone would resurrect it without anyone asking.
    pub fn notes_edit(&self, req: &Value) -> Op {
        let id = req
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "id required".into()))?;
        let title = req.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let body = req
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "body required".into()))?;
        let updated = self
            .notes
            .edit(id, title, body)
            .map_err(|e| (Code::InvalidArgument, e.to_string()))?;
        Ok(json!({ "updated": updated }))
    }

    /// Delete a note: `{id}` → `{deleted}`.
    ///
    /// Leaves a tombstone so the deletion can reach a peer. `deleted: false`
    /// means there was nothing to delete, or it was already deleted — a repeat
    /// must not re-stamp the tombstone and win a conflict it should have lost.
    pub fn notes_delete(&self, req: &Value) -> Op {
        let id = req
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "id required".into()))?;
        let deleted = self
            .notes
            .delete(id)
            .map_err(|e| (Code::Internal, e.to_string()))?;
        Ok(json!({ "deleted": deleted }))
    }

    /// Tell a peer we have read its messages: `{peer, read_through}` →
    /// `{sent}`.
    ///
    /// Sends nothing at all unless the user has opted in
    /// (`share_read_receipts`, default off). A read receipt discloses when
    /// *you* looked, which is a fact about your attention rather than about the
    /// message, so the default is silence and `sent: false` is the ordinary
    /// answer rather than a failure.
    ///
    /// One watermark rather than one message per receipt: ids are
    /// time-ordered, so a single id names the prefix of the conversation that
    /// has been read. That makes the message idempotent and monotonic, and one
    /// thread-read costs one frame.
    ///
    /// Nothing is written locally. Whether *we* have read a peer's messages is
    /// the surface's own business — persisting it here would invent a second
    /// read-state that no wire message maintains.
    pub fn chat_mark_read(self: &Arc<Self>, req: &Value) -> Op {
        let device = device_from(req.get("peer"))?;
        let read_through = req
            .get("read_through")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "read_through required".into()))?;
        if read_through.is_empty() || read_through.len() > peerbeam_chat::MAX_ID {
            return Err((
                Code::InvalidArgument,
                format!("read_through must be 1..={} bytes", peerbeam_chat::MAX_ID),
            ));
        }
        if !crate::chat_receipts::sending_enabled() {
            return Ok(json!({ "sent": false }));
        }
        permit_chat(self.trust.as_ref(), &device.id)?;

        let r = peerbeam_chat::Receipt::read_through(read_through);
        let me = self.clone();
        let sent = crate::runtime::block_on(async move { me.deliver_receipt(device, r).await });
        Ok(json!({ "sent": sent }))
    }

    /// Put one read receipt on the wire if the peer is reachable and
    /// understands them. Never fails the caller.
    async fn deliver_receipt(self: &Arc<Self>, device: Device, r: peerbeam_chat::Receipt) -> bool {
        let meta = self.session(&format!("receipt-{}", device.id.0), device.id.clone(), 0);
        let Ok(session) = crate::session_exec::dial(
            &self.quic,
            &self.rm,
            &device,
            &meta,
            self.identity(),
            self.enc.clone(),
            self.trust.clone(),
            Some(self.chat_wiring()),
            Some(self.presence_wiring()),
        )
        .await
        else {
            return false;
        };
        if !crate::session_exec::caps_support_receipt(&session.capabilities) {
            return false;
        }
        peerbeam_chat::send_receipt(&session.handle, &r)
            .await
            .is_ok()
    }

    /// React to one message: `{peer, id, emoji, remove?}` →
    /// `{applied, delivered}`.
    ///
    /// Applies to **our own** history first and unconditionally — it is our
    /// record of our own gesture, and a peer being unreachable is no reason for
    /// this device to forget that its user reacted. `applied` is false only
    /// when nothing changed: no such message in that conversation, or the
    /// reaction was already in the requested state.
    ///
    /// Delivery is best-effort and reported separately rather than folded into
    /// success. `delivered` is false when the peer could not be reached, or
    /// when it never negotiated [`CHAT_FEAT_REACTION`] — an older build would
    /// drop the OPTIONAL frame in silence, and telling the caller "sent" would
    /// be a claim about a screen where nothing appeared.
    ///
    /// Reactions are deliberately **not** queued in the outbox the way text and
    /// files are. A gesture that arrives long after its conversation moved on
    /// is noise, and the outbox's terminal-state handling is the most defect-
    /// prone part of this crate; a reaction that missed its moment is better
    /// lost than resurrected.
    pub fn chat_react(self: &Arc<Self>, req: &Value) -> Op {
        let device = device_from(req.get("peer"))?;
        let id = req
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "id required".into()))?;
        let emoji = req
            .get("emoji")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "emoji required".into()))?;
        if emoji.is_empty() || emoji.len() > peerbeam_chat::MAX_REACTION {
            return Err((
                Code::InvalidArgument,
                format!("emoji must be 1..={} bytes", peerbeam_chat::MAX_REACTION),
            ));
        }
        let remove = req
            .get("remove")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        // Same gate as sending a message: reacting is chatting.
        permit_chat(self.trust.as_ref(), &device.id)?;

        let applied = self
            .chat
            .apply_reaction(&device.id, id, emoji, peerbeam_chat::Direction::Out, remove)
            .map_err(|e| (Code::Internal, e.to_string()))?;

        let r = if remove {
            peerbeam_chat::Reaction::remove(id, emoji)
        } else {
            peerbeam_chat::Reaction::add(id, emoji)
        };
        let me = self.clone();
        let delivered =
            crate::runtime::block_on(async move { me.deliver_reaction(device, r).await });
        Ok(json!({ "applied": applied, "delivered": delivered }))
    }

    /// Put one reaction on the wire if the peer is reachable and understands
    /// them. Never fails the caller: every negative answer is simply "not
    /// delivered".
    async fn deliver_reaction(
        self: &Arc<Self>,
        device: Device,
        r: peerbeam_chat::Reaction,
    ) -> bool {
        let meta = self.session(&format!("react-{}", device.id.0), device.id.clone(), 0);
        let Ok(session) = crate::session_exec::dial(
            &self.quic,
            &self.rm,
            &device,
            &meta,
            self.identity(),
            self.enc.clone(),
            self.trust.clone(),
            Some(self.chat_wiring()),
            Some(self.presence_wiring()),
        )
        .await
        else {
            return false;
        };
        if !crate::session_exec::caps_support_reaction(&session.capabilities) {
            return false;
        }
        peerbeam_chat::send_reaction(&session.handle, &r)
            .await
            .is_ok()
    }

    pub fn chat_search(&self, req: &Value) -> Op {
        let query = req
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or((Code::InvalidArgument, "query required".into()))?;
        let limit = match req.get("limit") {
            None | Some(Value::Null) => DEFAULT_SEARCH_LIMIT,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=MAX_SEARCH_LIMIT as u64).contains(n))
                .ok_or((
                    Code::InvalidArgument,
                    format!("limit must be an integer between 1 and {MAX_SEARCH_LIMIT}"),
                ))? as usize,
        };
        let found = self
            .chat
            .search(query, limit)
            .map_err(|e| (Code::Internal, e.to_string()))?;
        let hits: Vec<Value> = found.hits.iter().map(search_hit_dto).collect();
        Ok(json!({
            "hits": hits,
            "truncated": found.truncated,
            "limit": limit,
        }))
    }

    /// Every conversation this device holds:
    /// `{peers:[{peer_id, last_timestamp, unread_hint}]}`. Takes no arguments.
    ///
    /// Derived from the namespaces that actually exist
    /// ([`ChatStore::conversations`]), not from a separate index: an index can
    /// drift, and the failure this exists to prevent is precisely a thread
    /// nothing can name — a peer discovery cannot see right now has no entry in
    /// the device list, so before this there was no way to *open* the
    /// conversation you already had with it.
    ///
    /// **`unread_hint` is the number of inbound file offers still awaiting your
    /// decision** in that thread (`direction: in`, `status:
    /// "pendingapproval"`) — rows with a live Accept button on them.
    ///
    /// It is deliberately **not** "messages you have not read", and no such
    /// number is reported, because PeerBeam cannot compute one honestly: there
    /// are no read receipts, and nothing records when a thread was last opened,
    /// so any "N unread" would be a guess dressed as a fact. The usual
    /// stand-in — counting what arrived since your last reply — is exactly that
    /// guess: a user who read every message and simply did not answer would be
    /// badged as having ignored them. What is reported instead is a fact the
    /// store already holds and a surface can act on: this thread is waiting on
    /// you for `n` decisions. A thread with unread *text* therefore reads 0,
    /// which is the honest answer to a question this product cannot yet ask.
    ///
    /// `last_timestamp` is the newest row's timestamp, or `null` for a thread
    /// whose rows this build cannot read. Sorted newest-first (ties broken by
    /// peer id, so the order is stable), which is what a conversation list
    /// wants — but note that an inbound row's timestamp came off the peer's own
    /// clock, so this is best-effort recency and not a trusted ordering.
    ///
    /// Cost: one namespace scan plus one full history read per conversation.
    /// [`AppStore`](peerbeam_domain::port::AppStore) has no "last key" call, so
    /// there is no cheaper way to learn a thread's newest row today.
    ///
    /// [`ChatStore::conversations`]: peerbeam_chat::ChatStore::conversations
    pub fn chat_conversations(&self, _req: &Value) -> Op {
        let peers = self
            .chat
            .conversations()
            .map_err(|e| (Code::Internal, e.to_string()))?;
        let mut rows: Vec<(String, Option<String>, usize)> = Vec::with_capacity(peers.len());
        for peer in peers {
            // A thread whose records cannot be read still exists and must still
            // be listed — dropping it would hide the very conversation this
            // call was added to make reachable. It just has nothing to say
            // about itself.
            let history = self.chat.history(&peer).unwrap_or_else(|e| {
                tracing::warn!(error = %e, peer_id = %peer.0, "conversation summary unreadable");
                Vec::new()
            });
            let last = history.last().map(|rec| rec.timestamp.clone());
            let awaiting = history
                .iter()
                .filter(|rec| {
                    rec.direction == ChatDirection::In && rec.status == ChatStatus::PendingApproval
                })
                .count();
            rows.push((peer.0, last, awaiting));
        }
        // Newest first. `None` sorts below every `Some`, so an unreadable
        // thread lands at the bottom rather than the top.
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let peers: Vec<Value> = rows
            .into_iter()
            .map(|(peer_id, last_timestamp, unread_hint)| {
                json!({
                    "peer_id": peer_id,
                    "last_timestamp": last_timestamp,
                    "unread_hint": unread_hint,
                })
            })
            .collect();
        Ok(json!({ "peers": peers }))
    }

    /// Delete this device's copy of one conversation:
    /// `{peer_id}` → `{removed, kept}`.
    ///
    /// **Local only.** Nothing goes on the wire and the peer keeps its own
    /// copy — this is "forget this thread here", never "unsend".
    ///
    /// `removed` is how many records were deleted; `kept` is how many were
    /// deliberately left behind because they still back a **queued** outbound
    /// message. Both are counted, not estimated, so a surface can tell the user
    /// what actually happened rather than what it hoped would.
    ///
    /// The keep set is the whole point, and
    /// [`ChatStore::delete_conversation`] carries the long form: clearing the
    /// namespace outright would destroy the queue, because the drain reads a
    /// **missing** record as "nothing will ever settle this"
    /// ([`row_may_still_deliver`](Self::row_may_still_deliver)) and releases the
    /// entry along with its staged bytes. So a queued file's row stays, its
    /// entry stays, its blob stays — and the next drain delivers it exactly as
    /// if the user had never touched the thread.
    ///
    /// No event is emitted: nothing else in this process holds conversation
    /// rows, and the surface that asked already knows. It refreshes its own
    /// list from [`chat_conversations`](Self::chat_conversations), where a
    /// thread that kept a queued record is still listed — correctly, and
    /// immediately, rather than reappearing later out of nowhere.
    ///
    /// **A refusal carries its own code.** When [`ChatStore::delete_conversation`]
    /// refuses because the shared outbox holds an entry it cannot decode, that
    /// reaches the caller as [`Code::QueueUnreadable`], never
    /// [`Code::Internal`] — the two are told apart on the
    /// [`ChatError`] variant, not by matching the message text. The
    /// distinction is not cosmetic: a plain store failure may well clear on
    /// its own retry, but this one won't — the offending entry might not even
    /// belong to the conversation being deleted, since the outbox is shared
    /// across every peer. Any other failure (a real store I/O error) still
    /// reports `Internal`, unchanged.
    ///
    /// [`ChatStore::delete_conversation`]: peerbeam_chat::ChatStore::delete_conversation
    pub fn chat_delete(&self, req: &Value) -> Op {
        // Held to exactly the rule `chat_cancel` uses — the increment's other
        // destructive call — rather than a second, parallel one: non-empty, and
        // required. A peer id is not a path or a registry key here (it names a
        // namespace the store itself builds), so `is_valid_transfer_id`'s
        // charset rule is not the applicable guard; what matters is that an
        // absent or empty id can never be read as "some conversation".
        let peer_id = req
            .get("peer_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or((Code::InvalidArgument, "peer_id required".into()))?
            .to_string();
        let peer = DeviceId::from(peer_id);
        let removed = self.chat.delete_conversation(&peer).map_err(|e| {
            // `QueueUnreadable` is the one refusal a retry cannot clear by
            // itself, so it earns its own code; everything else (a genuine
            // store failure) keeps reporting `Internal`, exactly as before.
            let code = match &e {
                ChatError::QueueUnreadable(_) => Code::QueueUnreadable,
                _ => Code::Internal,
            };
            (code, e.to_string())
        })?;
        // Counted from what is actually on disk *after* the delete — every row
        // still present is one the delete chose to keep — rather than from the
        // rule that chose them. A number the user is shown ("1 queued file will
        // still be sent") should be an observation, not a restatement of the
        // intent.
        //
        // Counted by STORED KEY, never by a successful decode. `delete_
        // conversation` keeps rows by key, so a kept row written by a newer
        // schema is real and present; a count that only saw rows this build can
        // read would report `kept: 0` while the thread stayed listed with a row
        // in it the user can neither see nor remove. It is also what lets a
        // file still being staged — kept, but with no outbox entry naming it
        // yet — be counted at all.
        let kept = self.chat.record_count(&peer).unwrap_or_default();
        Ok(json!({ "removed": removed, "kept": kept }))
    }

    /// Delete some of one conversation's messages:
    /// `{peer_id, message_ids:[…]}` → `{removed, kept:[…]}`.
    ///
    /// **Local only**, exactly like [`chat_delete`](Self::chat_delete): nothing
    /// goes on the wire and the peer keeps its own copy — this is "forget these
    /// messages here", never "unsend".
    ///
    /// `removed` is how many rows were deleted; `kept` names the ids that were
    /// asked for and deliberately left behind, because a **queued** outbound
    /// send still depends on them. That list is the point of the call answering
    /// at all: the user pointed at particular bubbles, so the surface can say
    /// which ones it could not take and why, rather than reporting the request
    /// back as though it had been carried out.
    ///
    /// [`ChatStore::delete_messages`] shares its keep rule with
    /// [`ChatStore::delete_conversation`] — the same one implementation, not a
    /// second copy of it — and that rule's doc carries the long form of why a
    /// row backing a queued send must survive: the drain reads a **missing**
    /// record as "nothing will ever settle this"
    /// ([`row_may_still_deliver`](Self::row_may_still_deliver)) and releases the
    /// entry along with the staged bytes it owns.
    ///
    /// No event is emitted, for the same reason as `chat_delete`: nothing else
    /// in this process holds conversation rows, and the surface that asked
    /// already knows. It re-reads the thread it is looking at.
    ///
    /// **A refusal carries its own code**, again exactly as `chat_delete`:
    /// [`Code::QueueUnreadable`] when the shared outbox holds an entry that will
    /// not decode, told apart on the [`ChatError`] variant rather than by
    /// matching message text, and [`Code::Internal`] for any genuine store
    /// failure.
    ///
    /// [`ChatStore::delete_messages`]: peerbeam_chat::ChatStore::delete_messages
    /// [`ChatStore::delete_conversation`]: peerbeam_chat::ChatStore::delete_conversation
    pub fn chat_delete_messages(&self, req: &Value) -> Op {
        // The same peer_id rule as `chat_delete` — non-empty and required — so
        // the two destructive chat calls cannot disagree about what names a
        // conversation.
        let peer_id = req
            .get("peer_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or((Code::InvalidArgument, "peer_id required".into()))?
            .to_string();
        // Required, and every element a non-empty string. A malformed element
        // silently dropped would make `removed`/`kept` a report about a request
        // the caller never made; a missing field entirely would delete nothing
        // and say so, which reads exactly like a selection that was already
        // gone. An EMPTY array is not a validation error, though: it asks for
        // nothing, deletes nothing, and answers `{removed: 0, kept: []}` —
        // which is what a surface whose selection emptied itself between the
        // render and the tap should get, rather than a failure to explain.
        //
        // "Not a validation error" is deliberately not "always `Ok`", and the
        // doc says so because the code means it. `ChatStore::delete_messages`
        // establishes the keep rule before it looks at a single id, so an empty
        // request against an outbox this build cannot read completely still
        // refuses with `QueueUnreadable`. That refusal is about the store's
        // state and not about the size of the request, and short-circuiting an
        // empty list past the one rule both deletes share is precisely the kind
        // of divergence that rule exists as a type to prevent.
        let ids = req
            .get("message_ids")
            .and_then(|v| v.as_array())
            .ok_or((Code::InvalidArgument, "message_ids required".into()))?;
        let mut message_ids = Vec::with_capacity(ids.len());
        for id in ids {
            let id = id
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or((
                    Code::InvalidArgument,
                    "message_ids must be non-empty strings".into(),
                ))?
                .to_string();
            message_ids.push(id);
        }
        let peer = DeviceId::from(peer_id);
        let (removed, kept) = self
            .chat
            .delete_messages(&peer, &message_ids)
            .map_err(|e| {
                // `QueueUnreadable` is the one refusal a retry cannot clear by
                // itself, so it earns its own code; everything else (a genuine
                // store failure) reports `Internal`.
                let code = match &e {
                    ChatError::QueueUnreadable(_) => Code::QueueUnreadable,
                    _ => Code::Internal,
                };
                (code, e.to_string())
            })?;
        Ok(json!({ "removed": removed, "kept": kept }))
    }

    /// Call off a file we are sharing: `{peer_id, message_id}` → `{cancelled}`.
    ///
    /// Stops the copy if one is running, stops the transfer if the bytes are
    /// moving, takes the entry out of the queue, deletes the staged blob, and
    /// settles the row `Failed`/`cancelled`. Safe in every state a share can be
    /// in — mid-stage, queued and never offered, in flight, or already settled —
    /// and `cancelled` says which way it went, so a surface never has to
    /// special-case an id it was too late for.
    ///
    /// **This deletes bytes on the strength of two caller-supplied strings**, so
    /// both are treated as hostile:
    ///
    /// * `message_id` goes through the same [`is_valid_transfer_id`] rule as
    ///   every other id crossing this boundary — a chat file's message id *is*
    ///   its transfer id — which is what keeps `..`, separators and NUL out of a
    ///   value that names a blob;
    /// * the row is fetched from **`peer_id`'s own namespace** and must pass
    ///   [`ChatRecord::is_cancellable_outgoing_file`]: our own outgoing file
    ///   share, not yet settled. Naming another conversation's message finds
    ///   nothing under that peer, a text row is refused, and an inbound offer is
    ///   refused — that one stays the approval gate's business (I6), which this
    ///   must never become a second, unprompted path into;
    /// * the path unlinked is the one read back off the **queue entry**, which
    ///   `StagingStore` itself produced under its own blob root, and that entry
    ///   is found via [`ChatStore::outbox_for`], which filters by peer. No path
    ///   is ever built from caller input.
    ///
    /// Failing any of those is a clean `{cancelled: false}` — not an error a
    /// surface has to handle — except a malformed `message_id`, which is a
    /// caller bug and says so.
    ///
    /// [`ChatRecord::is_cancellable_outgoing_file`]: peerbeam_chat::ChatRecord::is_cancellable_outgoing_file
    /// [`ChatStore::outbox_for`]: peerbeam_chat::ChatStore::outbox_for
    pub fn chat_cancel(&self, req: &Value) -> Op {
        let peer_id = req
            .get("peer_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or((Code::InvalidArgument, "peer_id required".into()))?
            .to_string();
        let id = valid_message_id(req.get("message_id"))?;
        let peer = DeviceId::from(peer_id.clone());

        // The authorization, in one place: this peer's own namespace, our own
        // outgoing file share, not already settled.
        let row = self
            .chat
            .get(&peer, &id)
            .map_err(|e| (Code::Internal, e.to_string()))?;
        if !row.is_some_and(|rec| rec.is_cancellable_outgoing_file()) {
            return Ok(json!({ "cancelled": false }));
        }

        // 1. A copy running right now. Stopping it lands inside one 64 KiB
        //    buffer and `StagingStore::stage` unlinks its own partial blob, so
        //    an 8 GiB stage leaves nothing behind. The copy's own task settles
        //    the row (and handles the race where it finished first), which is
        //    why nothing below re-settles when this fires.
        let stopped_stage = self.cancel_stage(&peer_id, &id);

        // 2. A transfer moving the bytes right now — but only ours, and only to
        //    this peer. `active` is keyed by an id space peers also write into
        //    (an incoming transfer registers under the id on its first frame),
        //    so the entry is matched on direction and peer before it is touched,
        //    never on the id alone.
        let ours_is_live = self
            .active
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|a| a.direction == "sending" && a.peer_id == peer_id);
        let stopped_transfer = ours_is_live && self.cancel(&id).is_ok();

        // 3. The queue entry and the bytes it owns. `outbox_for` filters by
        //    peer, so an entry found here belongs to this conversation by
        //    construction, and its `staged_path` is a string `StagingStore`
        //    wrote — never one derived from the request.
        let queued = self
            .chat
            .outbox_for(&peer)
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.message_id == id);
        let dequeued = match queued {
            Some(entry) => {
                match entry.file.as_ref() {
                    Some(staged) => self.drop_queued_file(&id, &staged.staged_path),
                    // A file entry with no blob owns no bytes; dequeue it alone.
                    None => {
                        if let Err(e) = self.chat.outbox_remove(&id) {
                            tracing::warn!(error = %e, message_id = %id, "cancelled entry not dequeued");
                        }
                    }
                }
                true
            }
            None => false,
        };

        // 4. The row. Skipped for a cancelled stage — that copy settles its own
        //    row moments from now, with this same status and reason.
        let settled = !stopped_stage && self.settle_cancelled(&peer, &peer_id, &id);
        Ok(json!({ "cancelled": stopped_stage || stopped_transfer || dequeued || settled }))
    }

    /// Move a cancelled share's row to `Failed`/`cancelled` — but only if the
    /// row *still* authorizes it. Returns whether this call changed anything, so
    /// a cancel that genuinely only tidied a stranded row still reports
    /// honestly, and one that found the row already settled does not emit a
    /// second, identical event.
    ///
    /// **This re-reads the row and re-applies
    /// [`ChatRecord::is_cancellable_outgoing_file`] — the same single rule
    /// `chat_cancel` authorized with, not a weaker one.** The two reads are far
    /// apart: between them `chat_cancel` takes a lock, cancels a live transfer,
    /// and runs a whole [`ChatStore::outbox_for`] (a `list` plus an AEAD decrypt
    /// per record). Another task's writer can land inside that window — the
    /// transfer completing writes `Sent` (`chat_settle` via `finish`), an
    /// arriving `FileDecline` writes `Declined` — and both are states the rule
    /// calls final. Settling on the *earlier* read would then overwrite a
    /// delivered file with `Failed`/"cancelled" and emit a `chat_status_detail`
    /// saying so: the sender's history would permanently claim a file the
    /// receiver holds was cancelled, and a peer's refusal would be relabelled as
    /// our own cancellation. The row that gets written is the row that was
    /// checked.
    ///
    /// `Failed` is excluded on top of the shared rule (which permits it, since a
    /// failed row may still have a queue entry a later drain would retry): the
    /// live transfer's own cancel path (`Manager::cancel` → `chat_settle`) may
    /// have landed it moments ago, and a second identical write is not something
    /// this call did.
    ///
    /// [`ChatRecord::is_cancellable_outgoing_file`]: peerbeam_chat::ChatRecord::is_cancellable_outgoing_file
    /// [`ChatStore::outbox_for`]: peerbeam_chat::ChatStore::outbox_for
    fn settle_cancelled(&self, peer: &DeviceId, peer_id: &str, id: &str) -> bool {
        match self.chat.get(peer, id) {
            Ok(Some(rec))
                if rec.is_cancellable_outgoing_file() && rec.status != ChatStatus::Failed =>
            {
                self.fail_chat_file(peer_id, id, "cancelled");
                true
            }
            _ => false,
        }
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
                //
                // **The transfer proceeds either way; the approval is what can
                // fail.** The user pressed a button meaning two things — take
                // this file, and remember this device — and only the second
                // needs the store. Refusing the transfer because the store
                // could not be written would punish them for a disk problem
                // they did not cause; saying nothing meant the next connection
                // asked again with no hint why, which reads as the button not
                // working.
                if let Err(e) = self.trust.approve(peer_id) {
                    tracing::warn!(
                        error = %e,
                        peer = %peer_id.0,
                        "accepted the transfer but could not record the approval — \
                         this device will be asked about again"
                    );
                }
                AcceptOutcome::Accepted
            }
            // The user's own refusal — `reject()`, or `cancel()` firing the
            // pending sender.
            Ok(Ok(AcceptDecision::Reject)) => AcceptOutcome::Rejected,
            // `Ok(Err(_))`: the sending half dropped without ever deciding.
            // `Err(_)`: ACCEPT_TIMEOUT elapsed with the prompt unanswered.
            // Neither is the user saying no.
            Ok(Err(_)) | Err(_) => {
                // ...but while the pairing check is on, an unanswered first
                // contact must not quietly *consume* the one chance to verify
                // this device. The handshake pinned it; leaving that pin means
                // the next connection is not `newly_trusted`, so no code is
                // shown and the gate never fires again — the peer is silently
                // accepted as known, having never been checked by anyone. That
                // is the same trap `reject`'s comment describes, reached by
                // nobody being at the machine rather than by a swallowed error.
                //
                // So the pin is given back and the device stays genuinely new.
                // Only while the check is required: with it off nothing was
                // going to be verified anyway, and un-pinning would be churn
                // that changes the default behaviour for no gain.
                if self.require_pairing_confirmation() {
                    if let Some(peer) = self.take_first_contact(id) {
                        if let Err(e) = self.trust.remove(&peer) {
                            tracing::warn!(
                                transfer = %id,
                                error = %e,
                                "an unanswered first contact could not be un-pinned; \
                                 it will not be treated as new next time"
                            );
                        }
                    }
                }
                AcceptOutcome::Unanswered
            }
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
            &self.rm,
            self.identity(),
            self.enc.clone(),
            self.trust.clone(),
            Some(self.chat_wiring()),
            Some(self.presence_wiring()),
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

        // Notes sync on connect, for the same reason chat flushes here: a
        // device that has just connected is reachable *now*, and that is the
        // only moment either side can be sure of. Nothing schedules a sync
        // otherwise, so without this notes would only ever move when someone
        // asked for it by hand.
        //
        // Gated on the permission and on the negotiated capability, and asked
        // of the **authenticated** peer. An unpermitted or older peer costs one
        // pair of lookups and nothing on the wire.
        //
        // A batch that arrives twice merges to no-ops — every note either loses
        // its conflict or is already identical — so an exchange racing an
        // explicit `notes_sync` is wasteful, never wrong.
        if peerbeam_notes::may_sync_notes(self.trust.as_ref(), &session.peer_device)
            && crate::session_exec::caps_support_notes(&session.capabilities)
        {
            match self.notes.all() {
                Ok(mine) => {
                    let batches = peerbeam_notes::NoteBatch::split(mine, false);
                    if let Err((_, e)) =
                        crate::session_exec::send_note_batches(&session.handle, &batches).await
                    {
                        tracing::debug!(error = %e, peer = %session.peer_device.0,
                            "notes sync-on-connect not delivered");
                    }
                }
                Err(e) => tracing::debug!(error = %e, "notes unreadable for sync-on-connect"),
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

        // **An inbound pipe is refused here, in the handler — deliberately not
        // by omitting the advertisement.**
        //
        // This build advertises `PIPE_FEAT_STREAM` exactly as the CLI does
        // (`session_exec::advertised_caps`), because a peer must not behave
        // differently depending on which of PeerBeam's two frontends it
        // reached — the bug 2a shipped with `CHAT_FEAT_FILEREF`. The bit
        // asserts *comprehension*, and this build genuinely comprehends the
        // channel; what it has no way to honour is the destination. A pipe
        // writes raw bytes to a shell's stdout, and a GUI has no stdout: there
        // is nothing here for the bytes to be, and no `peerbeam pipe --listen`
        // for the user to have started, which is the only consent that admits
        // one. So the refusal belongs at the point of acceptance, where the
        // reason is knowable, rather than hidden in a silently different
        // advertisement.
        //
        // It goes through the same `accept_pipe` funnel every other refusing
        // process uses, with `listening: false` and an `out` that discards, so
        // the decision stays in one place and even a broken gate would have
        // nowhere to put the bytes.
        if incoming_ch.channel_type == peerbeam_domain::session::ChannelType::PIPE {
            let consent = peerbeam_transfer::PipeConsent {
                listening: false,
                trust: self.trust.as_ref(),
                only_from: None,
                negotiated: &session.capabilities,
            };
            let mut nowhere = futures::io::sink();
            let outcome = peerbeam_transfer::accept_pipe(
                incoming_ch,
                &session.handle,
                &session.peer_device,
                &consent,
                &mut nowhere,
            )
            .await;
            match outcome {
                Ok(_) => {
                    tracing::error!("a pipe was accepted by the GUI, which must never accept one")
                }
                Err(e) => tracing::info!(error = %e, "refused an inbound pipe"),
            }
            session.close().await;
            return;
        }

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
        // Record first contact before any decision can be taken, so the accept
        // gate and the refusal un-pin both see it. `newly_trusted` is the only
        // moment this is knowable: the handshake has already pinned the peer,
        // so from here on the trust store reads the same for a stranger met
        // seconds ago and a device approved last week.
        if session.newly_trusted {
            self.mark_first_contact(&id, &session.peer_device);
        }

        // Approval: auto-accept only peers explicitly approved by the user on
        // a prior transfer, else wait for a decision. A pinned key alone
        // (TOFU trust, MITM protection) is not consent to auto-accept — that
        // requires the user to have accepted at least once before.
        // Read the flag fresh so a live toggle applies without a restart.
        //
        // Which of the three ways this can go is decided by `admit_transfer`,
        // where each leg is named and unit-tested — including the one this
        // increment adds: a device the user approved and then denied `files`
        // is refused outright rather than prompted about.
        // Both read fresh, so a live toggle of either applies without a
        // restart — the per-device bit comes off the trust store, which every
        // gate already re-reads per operation.
        let admission = admit_transfer_for(
            self.auto_accept.load(Ordering::SeqCst),
            self.trust.auto_accepts(&session.peer_device),
            self.trust.as_ref(),
            &session.peer_device,
        );
        // A transfer the user already accepted, interrupted and now offered
        // again, resumes without a second prompt — being asked twice for one
        // file because the Wi-Fi dropped is exactly the friction resume exists
        // to remove.
        //
        // It grants nothing by itself: `resumes_accepted_receive` requires a
        // stored checkpoint whose `accepted` is true AND whose peer, file name
        // and total size match what this connection is actually offering. A
        // transfer that was declined, timed out, or never answered has no
        // checkpoint at all and lands in the ordinary gate below — an
        // interruption must not turn an unanswered prompt into a yes (I6).
        let resuming =
            self.resumes_accepted_receive(&id, &session.peer_device, &preview.name, preview.size);
        let outcome = match admission {
            // A revoked `files` permission beats a resume: the user's decision
            // is newer than the checkpoint.
            FileAdmission::Refused => AcceptOutcome::Rejected,
            FileAdmission::AutoAccept => AcceptOutcome::Accepted,
            FileAdmission::Prompt if resuming => AcceptOutcome::Accepted,
            FileAdmission::Prompt => self.wait_for_accept(&id, &session.peer_device).await,
        };
        // The decision is settled, so this transfer is no longer an open
        // first-contact question — drop the record however it resolved. A
        // decline already consumed it in `reject`, which is the only path that
        // un-pins; this is the cleanup for every other ending (accepted,
        // cancelled, or nobody answered), and it deliberately un-pins nothing.
        // An unanswered prompt is not the user refusing, and this project does
        // not convert absence into a decision.
        self.take_first_contact(&id);
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

        // **Where this item lands.** The one call site: consulted here, after
        // every decision about *whether* to accept has been made above, and
        // immediately before the bytes are written (I6 — nothing in the
        // matcher can accept anything, and nothing above it reads a rule).
        //
        // Its three inputs are the authenticated `peer_device` id — never the
        // name the peer presented, which it chose — the *sanitized* name
        // (`preview.name` came through `sanitize_file_name`) and the size. A
        // peek that learned nothing gives an empty name and a zero size, which
        // simply matches fewer rules; a catch-all still applies and the save
        // directory is still the answer when nothing matches.
        //
        // A **resume** skips the matcher entirely and goes back to the
        // directory its own checkpoint records. The partial bytes are at
        // `<that directory>/<name>.part`, and the receive engine looks for
        // them relative to the directory it is handed — so re-deriving the
        // destination here would send a resumed file wherever the rules and
        // save directory point *now*, find no partial file, restart from zero,
        // and strand the first half in the old directory. Nothing about
        // consent moves: this only ever runs for a transfer already accepted
        // above, and it can only ever choose a directory this device itself
        // chose earlier.
        let resumed_dir = resuming
            .then(|| {
                self.resume_destination(&id, &session.peer_device, &preview.name, preview.size)
            })
            .flatten();
        let resolved = match resumed_dir {
            Some(dir) => peerbeam_config::rules::Destination {
                directory: dir,
                fallback: None,
            },
            None => peerbeam_config::rules::destination(
                &self.save_rules(),
                &self.save_dir(),
                &session.peer_device.0,
                &preview.name,
                preview.size,
            ),
        };
        // A destination that could not be used must be *said*. The file is
        // safe — it is going to the save directory — but a user who wrote a
        // rule believes the sort happened, and a file quietly landing
        // elsewhere is worse than no rules at all. It rides the transfer
        // event stream, the same channel every other thing that goes wrong
        // with this transfer uses, keyed to this transfer's own id.
        if let Some(fb) = &resolved.fallback {
            events::transfer(
                &id,
                "transfer_save_fallback",
                json!({
                    "peer_id": session.peer_device.0,
                    "rule_directory": fb.rule_directory,
                    "directory": resolved.directory,
                    "reason": fb.reason,
                }),
            );
            tracing::warn!(
                transfer_id = %id,
                rule_directory = %fb.rule_directory,
                reason = %fb.reason,
                "auto-save rule destination unusable; saving to the save directory"
            );
        }
        let save_dir = resolved.directory;
        // The record that survives an interruption, written **here** and
        // nowhere earlier: every line above this one is part of deciding
        // whether these bytes may land at all, and a checkpoint written before
        // that decision would be a consent record for a decision nobody made.
        // `accepted: true` is therefore only ever asserted on this side of the
        // gate.
        //
        // A folder gets none: it is a manifest plus N files, so a checkpoint
        // naming one file and one size would be a claim `check_resume` could
        // not honestly bind to. `preview.is_folder` is peer-supplied, but the
        // failure mode of a lie is only a missing (or an unbindable, therefore
        // refused) checkpoint — never a wrong one.
        let checkpoint = (!preview.is_folder).then(|| {
            self.checkpoint(
                &id,
                session.peer_device.clone(),
                Direction::Receiving,
                // `local_path`, not a glued `/`: this string is what the app
                // shows and taps to open, so it must use this machine's
                // separator. On Windows the glued form produced
                // `C:\Users\…\recv/file.exe` — which opens, but is not a
                // path any renderer or split can handle.
                &peerbeam_domain::local_path(
                    std::path::Path::new(save_dir.as_str()),
                    &preview.name,
                )
                .to_string_lossy(),
                &preview.name,
                preview.size,
                true,
            )
        });
        if let Some(cp) = &checkpoint {
            if let Err(e) = self.reliability.save_checkpoint(cp) {
                // Not fatal: the transfer still works, it just will not be
                // resumable if it dies. Losing the file to a refusal to record
                // it would be the worse trade.
                tracing::warn!(error = %e, transfer_id = %id, "receive checkpoint not written");
            }
        }
        let writer = checkpoint
            .clone()
            .map(|cp| crate::resume::CheckpointWriter::new(self.reliability.clone(), cp));
        let storage = self.storage();
        let ctrl = active.ctrl.clone();
        // Filled in by the folder branch with the sanitized root name
        // `receive_folder` actually wrote under `save_dir`, so history/"open"
        // can point at the folder itself instead of its parent.
        let folder_root = Arc::new(std::sync::Mutex::new(None::<String>));
        let dest_dir = save_dir.clone();
        let folder_root_cell = folder_root.clone();
        let ack = crate::session_exec::caps_support_folder_ack(&session.capabilities);
        let handle = session.handle.clone();
        let outcome = drive(
            id.clone(),
            active.stats.clone(),
            active.file.clone(),
            active.ctrl.clone(),
            |ptx| async move {
                let r =
                    receive_on_channel(incoming_ch, &handle, &storage, &dest_dir, &ctrl, &ptx, ack)
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
            writer,
        )
        .await;
        // What the checkpoint is worth now that this leg has ended.
        //
        // Completed: nothing left to resume, drop it.
        //
        // Cancelled **by this side** — `Manager::cancel` sets our own `ctrl`,
        // and nothing else does — is the user refusing the file, so the
        // partial bytes go with the record: they are not a head start on
        // anything, and leaving them would let a transfer the user threw away
        // seed the next one of the same name.
        //
        // Cancelled by the **peer** is not a refusal at all, it is the sender
        // walking away mid-stream — which is exactly what an interrupted
        // transfer looks like from here. Keep both, the same as an error: the
        // engine itself keeps the `.part` on a cancel for precisely this
        // reason, and deleting it would throw away the thing resume exists
        // for.
        if let Some(cp) = &checkpoint {
            match &outcome {
                Ok(TransferOutcome::Completed) => {
                    if let Err(e) = self.reliability.clear_checkpoint(&cp.id) {
                        tracing::warn!(error = %e, transfer_id = %id, "checkpoint not cleared");
                    }
                }
                Ok(TransferOutcome::Cancelled) if active.ctrl.is_cancelled() => {
                    self.discard_checkpoint(cp);
                }
                Ok(TransferOutcome::Cancelled) => tracing::info!(
                    transfer_id = %id,
                    "sender stopped mid-transfer; checkpoint kept for resume"
                ),
                Err(e) => tracing::info!(
                    error = %e,
                    transfer_id = %id,
                    "receive interrupted; checkpoint kept for resume"
                ),
            }
        }
        if matches!(outcome, Ok(TransferOutcome::Completed)) {
            // **Where it actually landed**, recorded for both shapes now
            // rather than only for folders. `record`/`record_history` fall
            // back to `self.save_dir()` when this is unset — which was exactly
            // right while there was only one destination, and would now name
            // the wrong directory for a rule-routed file in history, in the
            // chat row's "Open", and in the completion event. A folder still
            // points at the folder itself rather than its parent.
            let landed = folder_root
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| active.file.lock().unwrap().clone());
            if !landed.is_empty() {
                *active.path.lock().unwrap() = Some(
                    peerbeam_domain::local_path(std::path::Path::new(save_dir.as_str()), &landed)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        session.close().await;
        self.finish(&active, outcome);
        self.announce_if_interrupted(&id);
    }
}

/// How long an incoming transfer waits for the user to accept/reject before
/// it's treated as abandoned. Without this bound, a connection that dies (or
/// a prompt nobody answers) parks the handler on the approval channel
/// forever: the transfer stays in `active` — counted by the UI/notification —
/// with no terminal event ever emitted. Long enough that a human answering a
/// prompt is never rushed; short enough that ghosts don't accumulate.
/// How long a `browse` may hold its caller before it gives up.
///
/// This is a **user-interface** budget, not a network one. `pb_browse_list` is
/// synchronous and the app calls it from the isolate that draws, so every
/// second here is a second of frozen frames; the number is therefore chosen for
/// how long a person will sit still, not for how long a slow link might need.
///
/// Ten seconds is comfortably above a working dial over Tailscale or a VPN and
/// well below the sum of per-route connect timeouts a sleeping peer would
/// otherwise cost. `browse_budget_is_short_enough_to_sit_through` pins the
/// upper bound so a later change to the routing cannot quietly restore the
/// freeze.
const BROWSE_BUDGET: Duration = Duration::from_secs(10);

/// How long `ring` may hold its caller before giving up.
///
/// A UI budget, like [`BROWSE_BUDGET`], and shorter: ringing is a "make a noise
/// now" gesture, so a person who has waited eight seconds has already learned
/// what the answer is going to be. `sent: false` is an ordinary answer here —
/// the command has never promised the device made a sound, only that a request
/// was sent — so giving up early costs a truthful report of nothing.
const RING_BUDGET: Duration = Duration::from_secs(8);

/// How long the whole of `sync` may hold its caller.
///
/// Bigger than the others because a sync legitimately moves files, and cutting
/// a working transfer off at ten seconds would break the feature to protect the
/// interface. It exists because the operation had **no** ceiling at all: two
/// dials, a manifest wait, then a wait per file and per chunk batch, each with
/// its own timeout and none bounding the total — a large folder against a peer
/// that stalls mid-way could hold the isolate for an hour, which Android kills
/// as an ANR long before the user gets an error.
///
/// The real fix is to stop calling blocking FFI from the drawing isolate; this
/// bounds the damage until that exists.
const SYNC_BUDGET: Duration = Duration::from_secs(300);

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

/// How many times a send re-opens a transfer channel on the same session
/// before giving up.
///
/// Small on purpose. The session-level reconnect that matters — a new dial
/// over whatever route is now best — is `open_send_retry`'s, one layer up;
/// this only covers a channel that failed while the session itself survived,
/// which is a narrow window. Two attempts turn a single transient channel
/// error into a resumed transfer; more would just delay reporting a link that
/// is genuinely gone.
const SEND_RECOVER_ATTEMPTS: u32 = 2;

/// Minimum spacing between emitted progress updates (~20/s) — keeps small-chunk
/// progress smooth without flooding the event bridge.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

/// Minimum spacing between emitted **staging** progress events (~4/s).
///
/// Lower than [`PROGRESS_INTERVAL`] on purpose: a staging copy is local disk
/// work behind a determinate bar, not a live link whose speed and ETA the user
/// is watching tick, so four updates a second is already smoother than anyone
/// can read.
const STAGING_PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Minimum share of the file that must be copied between two emitted staging
/// events, as a divisor: `total / 100` is one percent.
const STAGING_PROGRESS_STEP_DIVISOR: u64 = 100;

/// Decides which of a staging copy's byte reports actually reach the event
/// bridge.
///
/// `StagingStore` reports every 64 KiB. That is right for the copy — it is how
/// cancellation lands within one buffer — and catastrophic for the bridge:
/// **~262,000 reports for a 16 GiB file**, each one a JSON string allocated,
/// copied across the FFI boundary and posted to the Dart isolate's port. An
/// unthrottled bridge would spend more time announcing the copy than doing it,
/// and would bury every other event (a `chat_received`, a `transfer_progress`)
/// behind a queue of stale byte counts.
///
/// The policy is **both** limits at once, because either alone fails a real
/// case:
///
/// * **time only** (`>= 250 ms` apart) bounds the rate but not the total — a
///   copy that runs for an hour still emits ~14,000 events;
/// * **percentage only** (`>= 1%` apart) bounds the total at ~100 but not the
///   spacing — a fast local copy of a small file emits its whole hundred in a
///   few milliseconds, which is exactly the flood being prevented.
///
/// So an update is emitted only when it is at least 250 ms *and* at least 1%
/// past the last one — at most ~100 events for a whole copy, never more than
/// four a second — with two deliberate exceptions that cost one event each:
/// the **first** report (so a bar appears immediately rather than up to a
/// second late) and the **first** report that reaches `total` (so the bar
/// finishes rather than stopping at 99%). Only the first such report: a source
/// still being appended to keeps reporting past `total`, and treating every
/// one of those as "final" would restore the flood at the worst moment.
struct StagingThrottle {
    /// Elapsed-time floor. A field rather than a constant so a test can set it
    /// to zero and exercise the percentage leg in isolation.
    interval: Duration,
    /// When the last event was emitted; `None` until the first report.
    last: Option<Instant>,
    /// The `done` the last emitted event carried.
    last_done: u64,
    /// Whether a report has already reached `total`.
    reached_total: bool,
}

impl StagingThrottle {
    fn new() -> StagingThrottle {
        StagingThrottle {
            interval: STAGING_PROGRESS_INTERVAL,
            last: None,
            last_done: 0,
            reached_total: false,
        }
    }

    /// Whether this report should be emitted — and, when it should, record it
    /// as the new baseline.
    fn due(&mut self, done: u64, total: u64) -> bool {
        let first = self.last.is_none();
        let finishing = !self.reached_total && total > 0 && done >= total;
        let spaced = self.last.is_some_and(|t| t.elapsed() >= self.interval);
        let stepped = done.saturating_sub(self.last_done) >= total / STAGING_PROGRESS_STEP_DIVISOR;
        if !(first || finishing || (spaced && stepped)) {
            return false;
        }
        if finishing {
            self.reached_total = true;
        }
        self.last = Some(Instant::now());
        self.last_done = done;
        true
    }
}

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
///
/// `checkpoint`, when present, is kept roughly current from the same progress
/// stream: the checkpoint written before the transfer starts says zero bytes
/// forever otherwise, and a resumable transfer whose recorded progress is
/// always zero is one the user is told nothing useful about after a restart.
#[allow(clippy::too_many_arguments)]
async fn drive<F, Fut>(
    id: String,
    stats: Arc<Mutex<Stats>>,
    file: Arc<Mutex<String>>,
    ctrl: TransferControl,
    run: F,
    progress_out: Option<Box<dyn peerbeam_domain::port::ProgressSink>>,
    progress_in: Option<Box<dyn peerbeam_domain::port::ProgressSource>>,
    checkpoint: Option<crate::resume::CheckpointWriter>,
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
    // Captured before the writer moves into the pump below: after the join
    // there is no writer left to ask, and this is the only place that knows the
    // last checkpoint write is past.
    let clearer = checkpoint
        .as_ref()
        .map(crate::resume::CheckpointWriter::clearer);

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
        let mut checkpoint = checkpoint;
        while let Some(p) = prx.recv().await {
            if let Some(f) = &p.current_file {
                *file.lock().unwrap() = f.clone();
            }

            // Before any of the throttling/suppression below, which exists for
            // the *event bridge* and would otherwise silently decide how much
            // progress survives a crash. The writer does its own, far coarser
            // throttling.
            if let Some(w) = checkpoint.as_mut() {
                w.record(p.transferred_bytes);
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

    // **The final word on the checkpoint.** A completed send clears its own
    // checkpoint from inside the transfer task, while the pump is still
    // draining a backlog of `Progress` on an unbounded channel — and every one
    // of those can write the checkpoint back. Deleting again here, with the
    // pump joined and no writer left alive, is what makes the deletion stick.
    // Without it a finished transfer intermittently stays on disk as an
    // interrupted one, and the user is offered a resume for a file they
    // already have.
    if matches!(r, Ok(TransferOutcome::Completed)) {
        if let Some(c) = clearer {
            c.clear();
        }
    }
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

/// The JSON projection of one [`SearchHit`], for `pb_chat_search`.
///
/// Deliberately **not** `events::record_dto`: a hit is not a record. It carries
/// the snippet rather than the whole body (the point of a bounded search), it
/// has no `status` and no `file` object, and — most importantly — its `peer_id`
/// is the conversation the row was read from rather than a field copied out of
/// the row. Reusing the record projection would mean either shipping every
/// matched message in full or quietly filling the missing fields in with
/// plausible values.
///
/// `direction` and `kind` serialize exactly as they do on a history row, so a
/// surface applies one vocabulary to both.
fn search_hit_dto(hit: &SearchHit) -> Value {
    json!({
        "peer_id": hit.peer_id,
        "message_id": hit.message_id,
        "timestamp": hit.timestamp,
        "direction": hit.direction,
        "kind": hit.kind,
        "snippet": hit.snippet,
    })
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

/// Read a caller-supplied chat `message_id`, held to **exactly** the
/// [`valid_transfer_id`] rule — the same function, only the field name in the
/// error differs.
///
/// Not a second, parallel validator, deliberately. A chat file's message id
/// *is* its transfer id (one identity, two channels), and it is also the name
/// of its staged blob on disk: `StagingStore::blob_path` refuses anything that
/// is not a bare file name, and this rule is what guarantees nothing that could
/// fail there — a separator, `..`, a NUL — ever reaches it. Two rules that were
/// meant to agree would eventually not.
fn valid_message_id(v: Option<&Value>) -> Result<String, (Code, String)> {
    valid_transfer_id(v.unwrap_or(&Value::Null)).map_err(|(code, _)| {
        (
            code,
            format!("message_id must be 1-{MAX_TRANSFER_ID} chars of [A-Za-z0-9._-]"),
        )
    })
}

/// Build a target `Device` from a `peer` JSON object.
pub(crate) fn device_from(peer: Option<&Value>) -> Result<Device, (Code, String)> {
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

/// Fetch one file by chunks, writing it into `into`. Returns bytes reused, or
/// `None` if delta transfer could not be used.
///
/// Free function rather than a method: it needs no `Manager` state, and keeping
/// it out of the impl makes it obvious that a delta fetch touches nothing but
/// the session, the index and the destination.
async fn fetch_by_delta(
    session: &crate::session_exec::Session,
    index: &peerbeam_sync::SyncIndex,
    folder: &str,
    into: &std::path::Path,
    remote_path: &str,
    write_to: &str,
) -> Option<u64> {
    let answer = crate::session_exec::request_chunk_map(session, remote_path).await?;
    if answer.denied || answer.chunks.is_empty() {
        return None;
    }
    let map = peerbeam_sync::ChunkMap {
        path: remote_path.to_string(),
        chunks: answer.chunks,
    };

    // **Refused before it is fetched, not after.** `reassemble` enforces this
    // same ceiling, but it runs once every chunk is already downloaded and
    // resident — so the guard protected nothing on this path. The declared size
    // is the peer's figure, and on a 32-bit ABI an over-large one aborts the
    // process outright (allocation failure is `handle_alloc_error`, not an
    // `Err`), which is why the whole-file fallback below could never be reached.
    // Answering `None` here hands this file to that fallback, which streams.
    if !peerbeam_sync::fits_in_memory(&map) {
        tracing::warn!(
            path = remote_path,
            declared = map.total_bytes(),
            ceiling = peerbeam_sync::MAX_REASSEMBLE,
            "delta refused: the peer's chunk map is larger than reassembly allows"
        );
        return None;
    }

    let have = index.chunks().have(folder);
    let need = peerbeam_sync::plan_delta(&map, &have);
    let fetched = if need.fetch.is_empty() {
        std::collections::HashMap::new()
    } else {
        crate::session_exec::request_chunks(session, remote_path, &need.fetch).await
    };

    let local = index.load(folder).ok()?;
    let rebuilt = peerbeam_sync::reassemble(&map, |h| {
        fetched
            .get(h)
            .cloned()
            .or_else(|| index.chunks().read(folder, into, &local, h))
    })?;

    // **`local_path`, not `join`.** `write_to` is a path from the peer's
    // manifest: `reconcile` emits a `Fetch` for any remote path absent from the
    // local index, and nothing between `Manifest::from_frame` and here checks
    // its shape — the manifest decoder bounds length and count, not content. So
    // `into.join("../../../../.bashrc")` resolves outside the sync root and this
    // `fs::write` would put peer-chosen bytes there, with the app's privileges.
    // The reassembly hash proves nothing about that: the peer supplied the map
    // it is checked against.
    //
    // `local_path` keeps only real components, so a hostile path lands inside
    // the root or nowhere.
    let dest = peerbeam_domain::local_path(into, write_to);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    std::fs::write(&dest, &rebuilt).ok()?;
    Some(need.reuse_bytes)
}

/// A stable key for one watched (share path → local directory) pair.
///
/// Joined with a control character rather than a slash or a dash: both appear
/// in real paths, and a separator that can occur in the values it separates is
/// a collision waiting to happen.
fn watch_key(path: &str, into: &str) -> String {
    format!("{path}\u{1}{into}")
}

/// Every file under `root`, as the settling rule wants to see it.
fn observe_folder(root: &std::path::Path) -> Vec<(String, peerbeam_sync::Observed)> {
    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        out: &mut Vec<(String, peerbeam_sync::Observed)>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            // Symlinks are skipped for the reason the index skips them:
            // following one can leave the folder or loop.
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                walk(root, &path, out);
            } else if meta.is_file() {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push((
                        peerbeam_domain::wire_path(rel),
                        peerbeam_sync::Observed {
                            size: meta.len(),
                            modified: meta
                                .modified()
                                .ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map_or(0, |d| d.as_secs() as i64),
                        },
                    ));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
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

    const SEND_LIMIT_KB: u64 = 1024;

    #[tokio::test]
    async fn a_manager_starts_unlimited() {
        // A limit nobody set is a slow transfer nobody can explain. Asserted
        // through a transfer's control rather than a getter on the manager:
        // what matters is that nothing throttles, not what a field holds.
        let mgr = test_manager("limit-default");
        let (_id, active) = mgr.register_fresh("sending", "bob", "pb-bob", "big.bin", None);
        assert_eq!(active.ctrl.rate_limit(), 0);
    }

    /// **The property that makes the setting useful.** Someone moves this
    /// slider because a transfer is saturating their link *now*; a limit that
    /// only applied to the next transfer would arrive after the problem had
    /// passed.
    #[tokio::test]
    async fn lowering_the_limit_reaches_a_transfer_already_running() {
        let mgr = test_manager("limit-live");
        let (_id, active) = mgr.register_fresh("sending", "bob", "pb-bob", "big.bin", None);
        assert_eq!(active.ctrl.rate_limit(), 0, "starts unlimited");

        mgr.set_send_limit(50 * SEND_LIMIT_KB);
        assert_eq!(
            active.ctrl.rate_limit(),
            50 * SEND_LIMIT_KB,
            "the running transfer never heard about the new limit"
        );
    }

    /// A transfer that starts while a limit is in force must respect it from
    /// its first chunk, not from whenever the setting next changes.
    #[tokio::test]
    async fn a_transfer_started_under_a_limit_is_born_limited() {
        let mgr = test_manager("limit-seeded");
        mgr.set_send_limit(100 * SEND_LIMIT_KB);
        let (_id, active) = mgr.register_fresh("sending", "bob", "pb-bob", "big.bin", None);
        assert_eq!(active.ctrl.rate_limit(), 100 * SEND_LIMIT_KB);
    }

    #[tokio::test]
    async fn clearing_the_limit_reaches_running_transfers_too() {
        let mgr = test_manager("limit-clear");
        mgr.set_send_limit(10 * SEND_LIMIT_KB);
        let (_id, active) = mgr.register_fresh("sending", "bob", "pb-bob", "big.bin", None);
        mgr.set_send_limit(0);
        assert_eq!(
            active.ctrl.rate_limit(),
            0,
            "a transfer stayed throttled after the limit was removed"
        );
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
        let (mgr, chat, _raw, dir) = test_manager_parts(name, daemon_port);
        (mgr, chat, dir)
    }

    /// [`test_manager_full`] plus the raw [`AppStore`] behind that `ChatStore`,
    /// so a test can write a value no `ChatStore` method can produce — a
    /// conversation row this build cannot decode in particular, which is what a
    /// row written by a newer schema looks like from here.
    ///
    /// [`AppStore`]: peerbeam_domain::port::AppStore
    fn test_manager_parts(
        name: &str,
        daemon_port: u16,
    ) -> (
        Manager,
        ChatStore,
        Arc<dyn peerbeam_domain::port::AppStore>,
        tempfile::TempDir,
    ) {
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
        let chat = ChatStore::new(appstore.clone());
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
            peerbeam_notes::NoteStore::new(appstore.clone()),
            peerbeam_clipboard::ClipHistory::new(appstore.clone()),
            appstore.clone(),
            // No hook in tests: running a program per received file would make
            // the suite depend on this machine's configuration.
            String::new(),
            staging,
            StagingLimits {
                max_bytes: u64::MAX,
                min_free_bytes: 0,
            },
            identity,
            dir.path().to_string_lossy().into_owned(),
            // No rules: these tests are about the transfer → chat bridge, and
            // the save destination is not the variable under study.
            Vec::new(),
            // auto_accept off.
            false,
            // require_pairing_confirmation off — the shipped default, so every
            // existing test keeps exercising the unchanged accept path. The
            // pairing-gate tests turn it on explicitly.
            false,
            1024,
            daemon_port,
            None,
            Arc::new(peerbeam_reliability_fs::FsReliability::new(
                dir.path().join("checkpoints"),
            )),
        );
        (mgr, chat, appstore, dir)
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
    /// **The transfer can arrive before the conversation row.** The peer's
    /// claim rides CHAT and what actually lands rides TRANSFER; nothing orders
    /// them, and under load the transfer wins often enough to matter.
    ///
    /// This used to be a silent no-op: `chat_set_landing` began by asking
    /// whether the row was settleable, that check answered "no" for a row that
    /// did not exist yet, and it returned without recording anything. The row
    /// then appeared carrying the sender's chosen name, and the user approved
    /// against a claim nothing had checked.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_landing_that_beats_its_chat_row_is_applied_when_the_row_arrives() {
        let (mgr, chat, _dir) = test_manager_full("recv-landing-first", 0);
        let peer = DeviceId::from("pb-bob");
        let offered = peerbeam_chat::FileRef::new("holiday.jpg", 184_320).expect("file ref");

        // TRANSFER first — no row exists yet.
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
        assert!(
            chat.get(&peer, &offered.id).expect("get").is_none(),
            "no row should exist yet"
        );

        // CHAT second — the row arrives, and must carry what lands.
        chat.append(&peerbeam_chat::ChatRecord::file_in(&peer, &offered))
            .expect("row arrives late");

        let meta = chat
            .get(&peer, &offered.id)
            .expect("get")
            .expect("row")
            .file
            .expect("file meta");
        assert_eq!(
            meta.name, "invoice-2026.pdf.exe",
            "the peer's claim outranked what actually lands"
        );
        assert_eq!(meta.size, 4_096, "and its size");
    }

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
    /// Startup reconciliation now enumerates every conversation, so it does
    /// reach a file-only thread — but it only runs at startup. This is the
    /// entry point for a row a *running* process stranded, and it must leave
    /// everything else alone.
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
        assert!(
            chat.enqueue_file(&peer, &r, &staged).expect("queue it"),
            "the row seeded above is there, so it queues"
        );
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
                mine: false,
                auto_accept: false,
                device: device.clone(),
                fingerprint: "test-fingerprint".into(),
                name: "peer".into(),
                trusted_at: chrono::Utc::now(),
                approved: false,
                permissions: peerbeam_domain::entity::PermissionSet::none(),
                expires_at: None,
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
        mgr.accept(&id, false)
            .expect("accept should find the pending id");

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
        mgr.accept_trust(&id, false)
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

    // ── the first-contact pairing gate ───────────────────────────
    //
    // Two halves, tested apart: `pairing_gate` decides *whether an accept may
    // proceed*, and `reject` decides *what a refusal costs the peer*. Both act
    // only on a peer this session pinned, which is the fact `first_contact`
    // records — the trust store cannot supply it, because by decision time a
    // stranger and a device approved last week are both simply "pinned".

    /// The policy itself, exhaustively. Eight rows, no session, no socket, no
    /// settings file — a change in what this gate lets through has to break
    /// here first.
    #[test]
    fn pairing_gate_only_lets_first_contact_through_on_an_explicit_confirmation() {
        // Not first contact: the check never applies, whatever the toggle says.
        // This is every transfer from a device the user has met before.
        for require in [false, true] {
            for confirmed in [false, true] {
                assert_eq!(
                    pairing_gate(false, require, confirmed),
                    PairingGate::Proceed,
                    "a known device is never gated (require={require}, confirmed={confirmed})"
                );
            }
        }
        // First contact with the check off — the shipped default, and exactly
        // the behaviour that existed before this gate.
        assert_eq!(pairing_gate(true, false, false), PairingGate::Proceed);
        assert_eq!(pairing_gate(true, false, true), PairingGate::Proceed);
        // First contact with the check on: an explicit yes, and nothing else.
        assert_eq!(
            pairing_gate(true, true, false),
            PairingGate::Blocked,
            "no confirmation is not a confirmation — it must never default to yes"
        );
        assert_eq!(pairing_gate(true, true, true), PairingGate::Confirmed);
    }

    /// Park a decision for `id` the way `handle_incoming` does, so `accept`/
    /// `reject` have something real to answer. Returns the waiter.
    fn park_decision(
        mgr: &Arc<Manager>,
        id: &str,
        peer: &DeviceId,
    ) -> tokio::task::JoinHandle<AcceptOutcome> {
        let (m, i, p) = (mgr.clone(), id.to_string(), peer.clone());
        tokio::spawn(async move { m.wait_for_accept(&i, &p).await })
    }

    #[tokio::test]
    async fn accept_at_first_contact_is_refused_until_the_codes_are_confirmed() {
        let mgr = Arc::new(test_manager("Device"));
        let peer = DeviceId::from("peer-stranger");
        pin(&mgr.trust, &peer);
        let id = "tx-first-contact";
        mgr.set_require_pairing_confirmation(true);
        mgr.mark_first_contact(id, &peer);

        let waiter = park_decision(&mgr, id, &peer);
        wait_until(|| mgr.pending.lock().unwrap().contains_key(id)).await;

        let blocked = mgr.accept(id, false);
        assert!(
            blocked.is_err(),
            "an unconfirmed accept at first contact must be refused"
        );
        assert!(
            mgr.pending.lock().unwrap().contains_key(id),
            "a blocked accept must leave the transfer PENDING — being asked to \
             verify a device must not cost the user the file"
        );

        // The same call, now carrying the user's explicit confirmation.
        mgr.accept(id, true)
            .expect("a confirmed accept goes through");
        assert_eq!(waiter.await.expect("task join"), AcceptOutcome::Accepted);
    }

    /// The stronger of the two accepts — the one that grants standing
    /// auto-accept — is gated identically. A gate on the weaker act only would
    /// be no gate at all.
    #[tokio::test]
    async fn accept_trust_at_first_contact_is_refused_until_the_codes_are_confirmed() {
        let mgr = Arc::new(test_manager("Device"));
        let peer = DeviceId::from("peer-stranger-trust");
        pin(&mgr.trust, &peer);
        let id = "tx-first-contact-trust";
        mgr.set_require_pairing_confirmation(true);
        mgr.mark_first_contact(id, &peer);

        let waiter = park_decision(&mgr, id, &peer);
        wait_until(|| mgr.pending.lock().unwrap().contains_key(id)).await;

        assert!(
            mgr.accept_trust(id, false).is_err(),
            "an unconfirmed accept-and-trust at first contact must be refused"
        );
        assert!(
            !mgr.trust.lookup(&peer).unwrap().unwrap().approved,
            "a blocked accept-and-trust must not have approved the device"
        );

        mgr.accept_trust(id, true)
            .expect("a confirmed accept-and-trust goes through");
        assert_eq!(waiter.await.expect("task join"), AcceptOutcome::Accepted);
    }

    /// With the toggle off — the default every install ships with — a
    /// first-contact transfer accepts exactly as it always has, confirmation
    /// flag or no confirmation flag.
    #[tokio::test]
    async fn accept_at_first_contact_is_unchanged_while_the_check_is_off() {
        let mgr = Arc::new(test_manager("Device"));
        let peer = DeviceId::from("peer-default-install");
        pin(&mgr.trust, &peer);
        let id = "tx-check-off";
        mgr.mark_first_contact(id, &peer);

        let waiter = park_decision(&mgr, id, &peer);
        wait_until(|| mgr.pending.lock().unwrap().contains_key(id)).await;

        mgr.accept(id, false)
            .expect("with the check off, an unconfirmed accept is just an accept");
        assert_eq!(waiter.await.expect("task join"), AcceptOutcome::Accepted);
    }

    /// The check is *first contact* only. A device the user has met before is
    /// never re-gated, however loudly the toggle is set.
    #[tokio::test]
    async fn accept_from_a_known_device_is_never_gated() {
        let mgr = Arc::new(test_manager("Device"));
        let peer = DeviceId::from("peer-known");
        pin(&mgr.trust, &peer);
        let id = "tx-known-device";
        mgr.set_require_pairing_confirmation(true);
        // Deliberately NOT marked first contact: this session did not pin it.

        let waiter = park_decision(&mgr, id, &peer);
        wait_until(|| mgr.pending.lock().unwrap().contains_key(id)).await;

        mgr.accept(id, false)
            .expect("a device the user has met before needs no pairing confirmation");
        assert_eq!(waiter.await.expect("task join"), AcceptOutcome::Accepted);
    }

    /// The app's `PairingGate::Revoke`: refusing a device this session pinned
    /// takes the pin with it.
    #[tokio::test]
    async fn refusing_a_first_contact_un_pins_the_peer() {
        let mgr = Arc::new(test_manager("Device"));
        let peer = DeviceId::from("peer-refused");
        pin(&mgr.trust, &peer);
        let id = "tx-refuse-first-contact";
        mgr.mark_first_contact(id, &peer);

        let waiter = park_decision(&mgr, id, &peer);
        wait_until(|| mgr.pending.lock().unwrap().contains_key(id)).await;

        let out = mgr.reject(id).expect("reject should find the pending id");
        assert_eq!(out["unpinned"], json!(true));
        assert_eq!(waiter.await.expect("task join"), AcceptOutcome::Rejected);
        assert!(
            mgr.trust.lookup(&peer).unwrap().is_none(),
            "a refused first contact must leave NO pin behind — otherwise the \
             next connection is not 'new', skips the gate, and the user is \
             never again asked to verify the device they just refused"
        );
    }

    /// The exclusion that keeps the un-pin honest. Declining a file from a
    /// device the user approved long ago is an ordinary "no thanks"; it must
    /// not quietly revoke a standing trust relationship.
    #[tokio::test]
    async fn refusing_a_previously_known_device_leaves_its_pin_alone() {
        let mgr = Arc::new(test_manager("Device"));
        let peer = DeviceId::from("peer-old-friend");
        pin(&mgr.trust, &peer);
        let id = "tx-refuse-known";
        // No `mark_first_contact`: this session did not pin this device.

        let waiter = park_decision(&mgr, id, &peer);
        wait_until(|| mgr.pending.lock().unwrap().contains_key(id)).await;

        let out = mgr.reject(id).expect("reject should find the pending id");
        assert!(
            out.get("unpinned").is_none(),
            "nothing was un-pinned, so nothing may claim to have been"
        );
        assert_eq!(waiter.await.expect("task join"), AcceptOutcome::Rejected);
        assert!(
            mgr.trust.lookup(&peer).unwrap().is_some(),
            "declining one file must never revoke a device the user already trusts"
        );
    }

    /// The trap the CLI's un-pin site documents, guarded here too: a removal
    /// that failed must never be reported as a removal that worked. If it
    /// were, the peer would stay trusted on disk while the app said otherwise
    /// — and the next connection from it would not be first contact, so no
    /// code, no gate, no second chance to catch the MITM.
    #[tokio::test]
    async fn a_failed_un_pin_is_reported_as_a_failure_not_as_success() {
        let (mgr, _chat, dir) = test_manager_full("Device", 0);
        let mgr = Arc::new(mgr);
        let peer = DeviceId::from("peer-unremovable");
        pin(&mgr.trust, &peer);
        let id = "tx-unpin-fails";
        mgr.mark_first_contact(id, &peer);

        let waiter = park_decision(&mgr, id, &peer);
        wait_until(|| mgr.pending.lock().unwrap().contains_key(id)).await;

        // Break the store's ability to write itself back: the file the trust
        // store persists through becomes a directory, so the read-merge-write
        // cycle inside `remove` cannot complete.
        let trust_path = dir.path().join("trust.json");
        std::fs::remove_file(&trust_path).expect("remove trust file");
        std::fs::create_dir(&trust_path).expect("put a directory in its place");

        let err = mgr
            .reject(id)
            .expect_err("a failed un-pin must not report success");
        assert!(
            matches!(err.0, Code::Storage),
            "a storage failure, reported"
        );
        assert!(
            err.1.contains("could NOT be un-pinned"),
            "the message must say the pin is still there: {}",
            err.1
        );
        assert_eq!(
            waiter.await.expect("task join"),
            AcceptOutcome::Rejected,
            "the refusal itself still stands — a failed un-pin must never fall \
             through to receiving the file"
        );
    }

    /// **With the pairing check off**, an unanswered prompt is not a refusal.
    /// `AcceptOutcome` exists precisely so this project never converts absence
    /// into the user's decision. The peer stays pinned by ordinary TOFU; it is
    /// still not `approved`, so it gains nothing (I6), and nothing was going to
    /// be verified anyway — so there is no verification opportunity to give
    /// back. The companion test below covers the case where there is.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_first_contact_prompt_un_pins_nothing_while_the_check_is_off() {
        let mgr = Arc::new(test_manager("Device"));
        let peer = DeviceId::from("peer-nobody-home");
        pin(&mgr.trust, &peer);
        let id = "tx-unanswered-first-contact";
        mgr.mark_first_contact(id, &peer);

        let waiter = park_decision(&mgr, id, &peer);
        wait_until(|| mgr.pending.lock().unwrap().contains_key(id)).await;
        tokio::time::advance(ACCEPT_TIMEOUT + Duration::from_millis(1)).await;

        assert_eq!(waiter.await.expect("task join"), AcceptOutcome::Unanswered);
        let record = mgr.trust.lookup(&peer).unwrap();
        assert!(record.is_some(), "a timeout is not the user saying no");
        assert!(
            !record.unwrap().approved,
            "and it grants the device nothing either"
        );
    }

    /// **With the pairing check on**, an unanswered first contact gives the pin
    /// back, and this is the case that matters.
    ///
    /// The handshake pinned the peer. If that pin survives an unanswered
    /// prompt, the *next* connection is not `newly_trusted` — no code is shown,
    /// the gate does not fire, and the device is treated as known having been
    /// checked by nobody. A stranger connecting while the machine is unattended
    /// would consume the single verification opportunity by doing nothing at
    /// all. That is the same trap `reject`'s comment describes, reached by
    /// absence instead of by a swallowed error.
    ///
    /// This is not absence being read as a decision: the transfer is still
    /// `Unanswered` and nothing is refused to the peer. Only the pin is
    /// released, so the device stays genuinely new until somebody looks at it.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_first_contact_gives_the_pin_back_while_the_check_is_on() {
        let mgr = Arc::new(test_manager("Device"));
        mgr.set_require_pairing_confirmation(true);
        let peer = DeviceId::from("peer-nobody-home");
        pin(&mgr.trust, &peer);
        let id = "tx-unanswered-gated-first-contact";
        mgr.mark_first_contact(id, &peer);

        let waiter = park_decision(&mgr, id, &peer);
        wait_until(|| mgr.pending.lock().unwrap().contains_key(id)).await;
        tokio::time::advance(ACCEPT_TIMEOUT + Duration::from_millis(1)).await;

        assert_eq!(
            waiter.await.expect("task join"),
            AcceptOutcome::Unanswered,
            "still not a refusal — only the pin is released"
        );
        assert!(
            mgr.trust.lookup(&peer).unwrap().is_none(),
            "the pin must be given back, so the next connection is first \
             contact again and the code is shown"
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

    // ── permit_send_files: the `files` permission on the SEND path ─────────
    //
    // `admit_transfer` covers what arrives. These cover what leaves, which the
    // switch's own wording ("Send and receive files with this device") promises
    // and which nothing enforced until now.

    /// The flow the app exists for: spot a device, send it a file. It has never
    /// required approval and must not start — `may` implies approval, so gating
    /// on it alone would refuse every device the user has not explicitly
    /// accepted.
    #[test]
    fn sending_to_a_merely_pinned_device_is_still_permitted() {
        let trust = AdmitTrust {
            pinned: true,
            approved: false,
            permissions: peerbeam_domain::entity::PermissionSet::none(),
        };
        assert!(permit_send_files(&trust, &admit_peer()).is_ok());
    }

    /// An approved device the user narrowed is refused in *both* directions.
    #[test]
    fn sending_to_an_approved_device_without_files_is_refused() {
        let trust = admit_approved_without(Permission::Files);
        let err = permit_send_files(&trust, &admit_peer()).expect_err("must refuse");
        assert!(
            matches!(err.0, Code::PermissionDenied),
            "a narrowed device is a permission refusal, not a generic failure"
        );
    }

    /// ...and revoking `files` does not quietly take anything else with it.
    #[test]
    fn revoking_another_permission_leaves_sending_alone() {
        for other in [
            Permission::Chat,
            Permission::Clipboard,
            Permission::Presence,
            Permission::Pipe,
        ] {
            let trust = admit_approved_without(other);
            assert!(
                permit_send_files(&trust, &admit_peer()).is_ok(),
                "revoking {other:?} must not stop file sends"
            );
        }
    }

    // ── admit_transfer: the `files` permission on the accept path ──────────

    /// A trust store in whichever of the four states matters here.
    struct AdmitTrust {
        pinned: bool,
        approved: bool,
        permissions: peerbeam_domain::entity::PermissionSet,
    }

    impl TrustStore for AdmitTrust {
        fn record(
            &self,
            _r: peerbeam_domain::entity::TrustRecord,
        ) -> peerbeam_domain::error::Result<()> {
            Ok(())
        }
        fn lookup(
            &self,
            d: &DeviceId,
        ) -> peerbeam_domain::error::Result<Option<peerbeam_domain::entity::TrustRecord>> {
            if !self.pinned {
                return Ok(None);
            }
            Ok(Some(peerbeam_domain::entity::TrustRecord {
                mine: false,
                auto_accept: false,
                device: d.clone(),
                fingerprint: "ff".into(),
                name: "Peer".into(),
                trusted_at: chrono::Utc::now(),
                approved: self.approved,
                permissions: self.permissions,
                expires_at: None,
            }))
        }
        fn is_trusted(&self, _d: &DeviceId) -> bool {
            self.pinned
        }
    }

    fn admit_approved() -> AdmitTrust {
        AdmitTrust {
            pinned: true,
            approved: true,
            permissions: peerbeam_domain::entity::PermissionSet::granted_on_approval(),
        }
    }

    fn admit_approved_without(p: Permission) -> AdmitTrust {
        AdmitTrust {
            permissions: peerbeam_domain::entity::PermissionSet::granted_on_approval()
                .set(p, false),
            ..admit_approved()
        }
    }

    fn admit_peer() -> DeviceId {
        DeviceId::from("pb-bob")
    }

    /// Leg 2, and the compatibility statement for this whole change: an
    /// approved device with its permissions intact auto-accepts exactly as it
    /// did before permissions existed.
    #[test]
    fn an_approved_device_still_auto_accepts_when_the_setting_is_on() {
        assert_eq!(
            admit_transfer(true, &admit_approved(), &admit_peer()),
            FileAdmission::AutoAccept
        );
        assert_eq!(
            admit_transfer(false, &admit_approved(), &admit_peer()),
            FileAdmission::Prompt,
            "with auto-accept off it is prompted, exactly as before"
        );
    }

    /// **The permission gate.** Revoking `files` refuses the transfer outright
    /// — it does not fall back to a prompt. The user already answered this
    /// question, and re-asking it on the sender's schedule is how a setting
    /// turns into a nuisance. Deleting the `Refused` leg must make this fail.
    #[test]
    fn revoking_files_refuses_an_inbound_transfer_without_prompting() {
        let trust = admit_approved_without(Permission::Files);
        assert_eq!(
            admit_transfer(true, &trust, &admit_peer()),
            FileAdmission::Refused
        );
        assert_eq!(
            admit_transfer(false, &trust, &admit_peer()),
            FileAdmission::Refused,
            "the auto-accept setting has no bearing on a revoked permission"
        );
    }

    /// **The permissions are separate bits, not an alias for `approved`.**
    /// Revoking any *other* permission leaves transfers exactly as they were.
    #[test]
    fn revoking_a_different_permission_leaves_transfers_working() {
        for other in Permission::ALL
            .into_iter()
            .filter(|p| *p != Permission::Files)
        {
            let trust = admit_approved_without(other);
            assert_eq!(
                admit_transfer(true, &trust, &admit_peer()),
                FileAdmission::AutoAccept,
                "revoking {other} must not affect transfers"
            );
        }
    }

    /// The per-device bit stops the prompt for **that** device and no other.
    /// That is the whole reason it exists: the global setting is all-or-nothing,
    /// so silencing one chatty phone used to mean silencing every device.
    #[test]
    fn per_device_auto_accept_silences_one_device_with_the_global_setting_off() {
        assert_eq!(
            admit_transfer_for(false, true, &admit_approved(), &admit_peer()),
            FileAdmission::AutoAccept,
            "the per-device answer must work without the global one"
        );
        assert_eq!(
            admit_transfer_for(false, false, &admit_approved(), &admit_peer()),
            FileAdmission::Prompt,
            "and a device it was not set on still asks"
        );
    }

    /// **The safety property.** Auto-accept decides whether the user is *asked*,
    /// never whether the device is *allowed*. Neither setting — nor both
    /// together — may admit a byte the `files` permission would refuse.
    #[test]
    fn per_device_auto_accept_cannot_admit_what_files_would_refuse() {
        let narrowed = AdmitTrust {
            permissions: peerbeam_domain::entity::PermissionSet::granted_on_approval()
                .set(Permission::Files, false),
            ..admit_approved()
        };
        for (global, per_device) in [(false, true), (true, true), (true, false)] {
            assert_eq!(
                admit_transfer_for(global, per_device, &narrowed, &admit_peer()),
                FileAdmission::Refused,
                "global={global} per_device={per_device} admitted a device \
                 whose `files` permission was revoked"
            );
        }

        // And on a device nobody approved, the per-device bit is inert: an
        // unapproved record may nothing whatever its other bits say, so this
        // is first contact and is asked about.
        let pinned = AdmitTrust {
            approved: false,
            permissions: peerbeam_domain::entity::PermissionSet::none(),
            ..admit_approved()
        };
        assert_eq!(
            admit_transfer_for(false, true, &pinned, &admit_peer()),
            FileAdmission::Prompt,
            "auto-accept on an unapproved device must not skip the prompt"
        );
    }

    /// The browse budget is a UI number: it must stay short enough that a
    /// person will sit through it, because `pb_browse_list` blocks the isolate
    /// that draws for its whole duration.
    ///
    /// Pinned as a bound rather than an equality so the value can be tuned, but
    /// not quietly raised back to the sum of per-route connect timeouts that
    /// froze the app before it existed.
    #[test]
    fn browse_budget_is_short_enough_to_sit_through() {
        assert!(
            BROWSE_BUDGET <= Duration::from_secs(15),
            "a synchronous call that blocks the UI for {BROWSE_BUDGET:?} reads \
             as a hang, not as a wait"
        );
        assert!(
            BROWSE_BUDGET >= Duration::from_secs(5),
            "too short to complete a working dial over a VPN or Tailscale"
        );
    }

    /// **The backward-compatibility leg.** A merely pinned peer — every
    /// stranger the TOFU handshake has ever recorded — is prompted, never
    /// silently refused. Permissions narrow a standing the user granted; they
    /// must not turn first contact into a refusal nobody sees.
    #[test]
    fn a_merely_pinned_peer_is_still_prompted_not_refused() {
        let pinned = AdmitTrust {
            approved: false,
            permissions: peerbeam_domain::entity::PermissionSet::none(),
            ..admit_approved()
        };
        assert!(pinned.is_trusted(&admit_peer()), "the handshake pinned it");
        assert_eq!(
            admit_transfer(true, &pinned, &admit_peer()),
            FileAdmission::Prompt
        );

        let unknown = AdmitTrust {
            pinned: false,
            ..pinned
        };
        assert_eq!(
            admit_transfer(true, &unknown, &admit_peer()),
            FileAdmission::Prompt,
            "a device with no record at all is first contact, and is asked about"
        );
    }

    /// Approval alone no longer auto-accepts: the permission has to be there
    /// too. A record with an empty set (which only an explicit revoke-all
    /// produces) is refused rather than auto-accepted.
    #[test]
    fn approval_without_the_files_permission_is_not_auto_accept() {
        let trust = AdmitTrust {
            permissions: peerbeam_domain::entity::PermissionSet::none(),
            ..admit_approved()
        };
        assert_eq!(
            admit_transfer(true, &trust, &admit_peer()),
            FileAdmission::Refused
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
        let paused_at = Instant::now();
        std::thread::sleep(Duration::from_millis(300));
        fixed.mark_resumed(); // the fix under test: re-anchors last_t/last_bytes
                              // `unfixed` intentionally does nothing here.
        let pause = paused_at.elapsed().as_secs_f64();

        let resumed_at = Instant::now();
        std::thread::sleep(Duration::from_millis(60));
        fixed.update(2_060_000, 10_000_000);
        unfixed.update(2_060_000, 10_000_000);
        let window = resumed_at.elapsed().as_secs_f64();

        // Same 60,000 bytes moved in the same ~60ms window post-resume, but
        // `unfixed`'s `dt` spans the full ~360ms pause too, so the same
        // bytes look like they trickled in ~6x slower — exactly the "bogus
        // near-zero speed after resume" bug.
        // **Derived from the sleeps that actually happened**, not the ones asked
        // for. `unfixed` spans the pause as well as the window, so the ratio it
        // should lose by is `(pause + window) / window` — a figure that depends
        // entirely on how long the two sleeps really took. Hardcoding 3x
        // assumed a 300ms pause and a 60ms window; on a loaded runner the
        // window stretches, the true ratio falls, and the test failed for a
        // reason having nothing to do with the resume anchor. 60% of the
        // measured ratio still fails outright if `mark_resumed` does nothing,
        // which is the property under test.
        let expected_ratio = (pause + window) / window;
        assert!(
            fixed.current_speed > unfixed.current_speed * (expected_ratio * 0.6).max(1.2),
            "fixed={} unfixed={} (pause {pause:.3}s, window {window:.3}s, \
             expected ratio {expected_ratio:.2}): without the resume reset, \
             current_speed is computed across the pause gap and reads far too low",
            fixed.current_speed,
            unfixed.current_speed
        );
    }

    // ── BUG 4: average_speed must exclude the pre-transfer approval wait ─

    #[test]
    fn average_speed_excludes_the_pre_transfer_wait() {
        let mut s = Stats::new();
        let registered = Instant::now();
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

        // **Measured, not assumed.** An earlier version hardcoded the elapsed
        // time as 210 ms — the two sleeps added up — and asserted a rate above
        // the figure that implied. On a loaded runner a 60 ms sleep is not
        // 60 ms, so the arithmetic stopped describing the run and the test
        // failed for reasons unrelated to what it checks. Timing the window
        // with the same clock the code uses holds at any speed.
        let since_registration = registered.elapsed().as_secs_f64();
        let baselined_at_registration = 7_000_000.0 / since_registration;
        assert!(
            s.average_speed > baselined_at_registration * 1.2,
            "average_speed {} is no better than the registration-baselined \
             {baselined_at_registration}, so it still counts the idle wait",
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
            None,
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

    // ── staging progress: the throttle ──────────────────────────

    /// The number this exists for. A 16 GiB file reports every 64 KiB — 262,144
    /// times — and every one of those would otherwise be a JSON string built,
    /// copied across the FFI boundary and posted to the Dart isolate. The
    /// throttle must cut that to something a bar can actually use.
    #[test]
    fn staging_throttle_cuts_a_16_gib_copy_from_262144_reports_to_about_a_hundred() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let total = 16 * GIB;
        let chunk = 64 * 1024;
        let reports = total / chunk;
        assert_eq!(reports, 262_144, "the flood this throttle exists for");

        // Time is not the binding limit here — a copy this size runs for
        // minutes — so hold the interval at zero and measure the percentage
        // leg on its own.
        let mut throttle = StagingThrottle::new();
        throttle.interval = Duration::ZERO;
        let emitted = (1..=reports)
            .filter(|n| throttle.due(n * chunk, total))
            .count();
        assert!(
            (100..=102).contains(&emitted),
            "16 GiB must emit ~100 events, not {emitted}"
        );
    }

    /// A fast local copy is the case the percentage limit alone gets wrong: 100
    /// events inside a few milliseconds is still a flood. The time floor is what
    /// stops it, and the first report is deliberately exempt so a bar appears at
    /// once rather than up to a quarter-second late.
    #[test]
    fn staging_throttle_holds_a_fast_copy_to_its_first_report() {
        let total = 100 * 1024 * 1024;
        let chunk = 64 * 1024;
        let mut throttle = StagingThrottle::new();
        let emitted = (1..=(total / chunk))
            .filter(|n| throttle.due(n * chunk, total))
            .count();
        assert_eq!(
            emitted, 2,
            "a copy that finishes inside one interval emits its first report and \
             its last, and nothing in between"
        );
    }

    /// The completing report always goes out, so the bar reaches 100% instead of
    /// stopping wherever the throttle last let something through — but only the
    /// *first* one to reach `total`. A source being appended to while we copy
    /// (a log, a running download) keeps reporting past its own size, and
    /// treating each of those as "final" would restore the flood at the worst
    /// possible moment.
    #[test]
    fn staging_throttle_emits_the_completing_report_once_and_not_the_overrun() {
        let mut throttle = StagingThrottle::new();
        throttle.interval = Duration::from_secs(3600); // only first/finishing can fire
        assert!(throttle.due(64, 640), "the first report always goes out");
        assert!(!throttle.due(128, 640), "throttled");
        assert!(throttle.due(640, 640), "the completing report goes out");
        for overrun in [704, 768, 832] {
            assert!(
                !throttle.due(overrun, 640),
                "a growing source must not re-fire 'finished' at {overrun}"
            );
        }
    }

    /// An empty file has no fraction to report. It must not divide by zero and
    /// must not claim to have finished something it never started.
    #[test]
    fn staging_throttle_survives_a_zero_byte_total() {
        let mut throttle = StagingThrottle::new();
        throttle.interval = Duration::from_secs(3600);
        assert!(throttle.due(0, 0), "the first report still goes out");
        assert!(!throttle.due(0, 0));
    }

    // ── cancelling a share ──────────────────────────────────────

    /// Seed an outgoing file row plus a queued outbox entry whose staged blob
    /// really exists on disk, exactly as a completed stage would leave them.
    /// Returns the message id and the blob's path.
    fn seed_queued_file(
        chat: &ChatStore,
        blob_root: &std::path::Path,
        peer: &DeviceId,
        name: &str,
    ) -> (String, std::path::PathBuf) {
        let r = peerbeam_chat::FileRef::new(name, 4).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            peer,
            &r,
            file_meta(&r),
            ChatStatus::Staging,
        ))
        .expect("seed the row");
        std::fs::create_dir_all(blob_root).expect("blob root");
        let blob = blob_root.join(&r.id);
        std::fs::write(&blob, b"bytes").expect("seed the blob");
        assert!(
            chat.enqueue_file(
                peer,
                &r,
                &peerbeam_chat::StagedFile {
                    name: name.to_string(),
                    size: 5,
                    staged_path: blob.to_string_lossy().into_owned(),
                },
            )
            .expect("queue it"),
            "the row seeded above is there, so it queues"
        );
        (r.id, blob)
    }

    fn cancel(mgr: &Manager, peer_id: &str, message_id: &str) -> Op {
        mgr.chat_cancel(&json!({ "peer_id": peer_id, "message_id": message_id }))
    }

    /// The ordinary case: a file queued for a peer that is not there. Cancel
    /// takes the entry out of the queue, deletes the bytes it owned, and settles
    /// the row `failed`/`cancelled`.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_cancel_dequeues_a_queued_file_deletes_its_blob_and_fails_the_row() {
        let (mgr, chat, dir) = test_manager_full("canceller", 0);
        let peer = DeviceId::from("pb-bob");
        let blobs = dir.path().join("outbox-blobs");
        let (id, blob) = seed_queued_file(&chat, &blobs, &peer, "holiday.mp4");

        let out = cancel(&mgr, &peer.0, &id).expect("cancel");
        assert_eq!(out["cancelled"], true);
        assert!(
            chat.outbox_for(&peer).unwrap().is_empty(),
            "the entry must leave the queue"
        );
        assert!(!blob.exists(), "and its staged bytes must be deleted");
        let row = chat.get(&peer, &id).unwrap().expect("the row survives");
        assert_eq!(row.status, ChatStatus::Failed);
        assert_eq!(row.kind, peerbeam_chat::Kind::File, "still a file row");
    }

    /// **What stops a cancel reaching outside its own conversation.** Two peers
    /// each have a file queued under their own id. Cancelling one must leave the
    /// other's entry *and* its bytes untouched — including when the caller pairs
    /// a real message id with the wrong peer, which is the shape an attempt to
    /// reach across threads would actually take.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_cancel_cannot_reach_another_conversations_entry_or_blob() {
        let (mgr, chat, dir) = test_manager_full("canceller", 0);
        let blobs = dir.path().join("outbox-blobs");
        let alice = DeviceId::from("pb-alice");
        let bob = DeviceId::from("pb-bob");
        let (alice_id, alice_blob) = seed_queued_file(&chat, &blobs, &alice, "alice.bin");
        let (bob_id, bob_blob) = seed_queued_file(&chat, &blobs, &bob, "bob.bin");

        // Alice's id, claimed as Bob's message. There is no such row in Bob's
        // namespace, so the guard never gets as far as the queue.
        let out = cancel(&mgr, &bob.0, &alice_id).expect("cancel");
        assert_eq!(
            out["cancelled"], false,
            "no row for that id under this peer"
        );
        assert!(alice_blob.exists(), "alice's bytes are untouched");
        assert_eq!(chat.outbox_for(&alice).unwrap().len(), 1);
        assert_eq!(
            chat.get(&alice, &alice_id).unwrap().unwrap().status,
            ChatStatus::Pending,
            "and alice's row is not settled by bob's cancel"
        );

        // The honest cancel still works, and still only touches its own thread.
        assert_eq!(cancel(&mgr, &bob.0, &bob_id).unwrap()["cancelled"], true);
        assert!(!bob_blob.exists());
        assert!(alice_blob.exists(), "alice is still untouched afterwards");
        assert_eq!(chat.outbox_for(&alice).unwrap().len(), 1);
    }

    /// The other three rows a caller could name, all refused without a write: a
    /// text message (nothing staged), the peer's own offer to *us* (that is the
    /// approval gate's business, I6), and a share that already completed.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_cancel_refuses_text_inbound_and_settled_rows() {
        let (mgr, chat, _dir) = test_manager_full("canceller", 0);
        let peer = DeviceId::from("pb-bob");

        let text = peerbeam_chat::ChatMessage::new("hello").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &text))
            .expect("seed text");
        let offered = peerbeam_chat::FileRef::new("theirs.bin", 9).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_in(&peer, &offered))
            .expect("seed the inbound offer");
        let done = peerbeam_chat::FileRef::new("done.bin", 9).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &peer,
            &done,
            file_meta(&done),
            ChatStatus::Sent,
        ))
        .expect("seed a delivered share");

        for (id, why) in [
            (&text.id, "a text row has nothing to cancel"),
            (
                &offered.id,
                "an inbound offer is refused at the gate, not here",
            ),
            (&done.id, "a delivered file cannot be un-sent"),
        ] {
            let out = cancel(&mgr, &peer.0, id).expect("cancel");
            assert_eq!(out["cancelled"], false, "{why}");
        }
        // Not one of them was written to.
        assert_eq!(
            chat.get(&peer, &text.id).unwrap().unwrap().status,
            ChatStatus::Sent
        );
        assert_eq!(
            chat.get(&peer, &offered.id).unwrap().unwrap().status,
            ChatStatus::PendingApproval
        );
        assert_eq!(
            chat.get(&peer, &done.id).unwrap().unwrap().status,
            ChatStatus::Sent
        );
    }

    /// An id for a message that does not exist is a clean no-op, not an error a
    /// surface has to special-case — but a *malformed* id is a caller bug and
    /// says so, because that string would otherwise name a file.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_cancel_is_a_no_op_for_an_unknown_id_and_an_error_for_a_malformed_one() {
        let (mgr, _chat, _dir) = test_manager_full("canceller", 0);
        assert_eq!(
            cancel(&mgr, "pb-bob", "1785559080834abcdef0123456789").unwrap()["cancelled"],
            false
        );
        for hostile in ["../../../etc/passwd", "..", "a/b", "a\\b", ""] {
            let (code, _) = cancel(&mgr, "pb-bob", hostile).expect_err("must be refused");
            assert!(
                matches!(code, Code::InvalidArgument),
                "{hostile:?} must be refused as an argument, not acted on"
            );
        }
        let (code, _) = mgr
            .chat_cancel(&json!({ "message_id": "1785559080834abcdef0123456789" }))
            .expect_err("peer_id is required");
        assert!(matches!(code, Code::InvalidArgument));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn notes_sync_sends_nothing_to_a_device_that_was_never_granted_notes() {
        // The default state of the feature. `Notes` is slot 5, assigned after
        // `granted_on_approval` was frozen, so even an approved device does not
        // have it until someone says so — and `sent: false` is the ordinary
        // answer rather than an error.
        let (mgr, _chat, _dir) = test_manager_full("syncer", 0);
        let mgr = Arc::new(mgr);
        let out = mgr
            .notes_sync(&json!({ "peer": { "id": "pb-bob", "name": "Bob",
                                           "addresses": ["127.0.0.1"], "port": 49600 } }))
            .expect("a peer without the permission is not an error");
        assert_eq!(out["sent"], false);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn the_timeline_merges_stores_newest_first_and_carries_no_content() {
        let (mgr, chat, _dir) = test_manager_full("timeliner", 0);
        let peer = DeviceId::from("pb-bob");
        let msg = peerbeam_chat::ChatMessage::new("something private").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &msg))
            .expect("append");

        let out = mgr.timeline(&json!({})).expect("timeline");
        let events = out["events"].as_array().expect("array");
        assert!(
            !events.is_empty(),
            "a conversation produced no timeline entry"
        );

        // The timeline says *that* something happened. A message body in a
        // scrollable activity feed is a second, worse copy of the conversation.
        let dumped = serde_json::to_string(&out).expect("json");
        assert!(
            !dumped.contains("something private"),
            "the timeline carried a message body"
        );

        // Newest first.
        let times: Vec<&str> = events.iter().filter_map(|e| e["at"].as_str()).collect();
        let mut sorted = times.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(times, sorted, "the timeline was not newest-first");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn the_timeline_bounds_itself_and_says_when_it_truncated() {
        let (mgr, chat, _dir) = test_manager_full("timeliner2", 0);
        let peer = DeviceId::from("pb-bob");
        for i in 0..5 {
            let m = peerbeam_chat::ChatMessage::new(&format!("m{i}")).expect("message");
            chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
                .expect("append");
        }

        let out = mgr.timeline(&json!({ "limit": 2 })).expect("timeline");
        assert_eq!(out["events"].as_array().expect("array").len(), 2);
        assert_eq!(out["truncated"], true, "truncation was not reported");
        assert_eq!(out["limit"], 2);

        let (code, _) = mgr
            .timeline(&json!({ "limit": 0 }))
            .expect_err("a zero limit is refused");
        assert!(matches!(code, Code::InvalidArgument));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn notes_round_trip_through_create_edit_list_and_delete() {
        let (mgr, _chat, _dir) = test_manager_full("noter", 0);

        let id = mgr
            .notes_create(&json!({ "title": "Shopping", "body": "milk" }))
            .expect("create")["id"]
            .as_str()
            .expect("id")
            .to_string();

        let listed = mgr.notes_list(&json!({})).expect("list");
        assert_eq!(listed["notes"].as_array().expect("array").len(), 1);
        assert_eq!(listed["notes"][0]["body"], "milk");

        assert_eq!(
            mgr.notes_edit(&json!({ "id": id, "title": "Shopping", "body": "milk, bread" }))
                .expect("edit")["updated"],
            true
        );
        assert_eq!(
            mgr.notes_list(&json!({})).expect("list")["notes"][0]["body"],
            "milk, bread"
        );

        assert_eq!(
            mgr.notes_delete(&json!({ "id": id })).expect("delete")["deleted"],
            true
        );
        assert!(
            mgr.notes_list(&json!({})).expect("list")["notes"]
                .as_array()
                .expect("array")
                .is_empty(),
            "a deleted note is still listed"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_deleted_note_cannot_be_edited_or_deleted_again() {
        // Both would resurrect or re-stamp a tombstone, and a re-stamped
        // tombstone wins conflicts it should lose.
        let (mgr, _chat, _dir) = test_manager_full("noter2", 0);
        let id = mgr
            .notes_create(&json!({ "body": "temporary" }))
            .expect("create")["id"]
            .as_str()
            .expect("id")
            .to_string();
        mgr.notes_delete(&json!({ "id": id })).expect("delete");

        assert_eq!(
            mgr.notes_edit(&json!({ "id": id, "body": "back" }))
                .expect("edit")["updated"],
            false
        );
        assert_eq!(
            mgr.notes_delete(&json!({ "id": id })).expect("delete")["deleted"],
            false
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn notes_refuse_what_the_store_refuses() {
        let (mgr, _chat, _dir) = test_manager_full("noter3", 0);
        let (code, _) = mgr
            .notes_create(&json!({ "body": "x".repeat(peerbeam_notes::MAX_BODY + 1) }))
            .expect_err("an oversized body is refused");
        assert!(matches!(code, Code::InvalidArgument));

        let (code, _) = mgr
            .notes_create(&json!({ "title": "t" }))
            .expect_err("a note with no body is refused");
        assert!(matches!(code, Code::InvalidArgument));
    }

    /// **Cancelling mid-stage cancels the copy.** The control the staging copy
    /// is holding is reached through the `(peer, message)` registry, so an 8 GiB
    /// stage stops inside one 64 KiB buffer rather than running to completion.
    ///
    /// The row is deliberately left alone here: the copy's own task settles it
    /// moments later, with this same status and reason, and racing it would emit
    /// the event twice.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_cancel_stops_a_staging_copy_that_is_still_running() {
        let (mgr, chat, _dir) = test_manager_full("canceller", 0);
        let peer = DeviceId::from("pb-bob");
        let r = peerbeam_chat::FileRef::new("huge.iso", 8 * 1024 * 1024 * 1024).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &peer,
            &r,
            file_meta(&r),
            ChatStatus::Staging,
        ))
        .expect("seed the staging row");
        let ctrl = TransferControl::new();
        mgr.register_stage(&peer.0, &r.id, ctrl.clone());

        assert_eq!(cancel(&mgr, &peer.0, &r.id).unwrap()["cancelled"], true);
        assert!(ctrl.is_cancelled(), "the copy must be told to stop");
        assert_eq!(
            chat.get(&peer, &r.id).unwrap().unwrap().status,
            ChatStatus::Staging,
            "the copy's own task settles this row; cancel must not race it"
        );

        // …and the same control, retired by the copy, reports the cancellation
        // to it. This handshake is what closes the race where the copy finishes
        // a moment before the cancel lands.
        assert!(mgr.finish_stage(&peer.0, &r.id));
        assert!(
            !mgr.cancel_stage(&peer.0, &r.id),
            "a retired stage is no longer cancellable"
        );
    }

    /// A stage belonging to one peer must not be reachable through another
    /// peer's request, even when the caller has the right message id — which is
    /// why the registry is keyed by the pair.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_stage_is_only_cancellable_through_its_own_peer() {
        let (mgr, _chat, _dir) = test_manager_full("canceller", 0);
        let ctrl = TransferControl::new();
        mgr.register_stage("pb-alice", "m-1", ctrl.clone());
        assert!(!mgr.cancel_stage("pb-bob", "m-1"));
        assert!(!ctrl.is_cancelled());
        assert!(mgr.cancel_stage("pb-alice", "m-1"));
        assert!(ctrl.is_cancelled());
    }

    /// A row stranded `Staging` by a crash — its copy died with the process, so
    /// nothing will ever settle it — is still cancellable, and the call reports
    /// honestly that it did something. Calling again reports that there was
    /// nothing left to do.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_cancel_settles_a_stranded_row_once_and_then_reports_nothing_to_do() {
        let (mgr, chat, _dir) = test_manager_full("canceller", 0);
        let peer = DeviceId::from("pb-bob");
        let r = peerbeam_chat::FileRef::new("stranded.bin", 7).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &peer,
            &r,
            file_meta(&r),
            ChatStatus::Staging,
        ))
        .expect("seed the stranded row");

        assert_eq!(cancel(&mgr, &peer.0, &r.id).unwrap()["cancelled"], true);
        assert_eq!(
            chat.get(&peer, &r.id).unwrap().unwrap().status,
            ChatStatus::Failed
        );
        assert_eq!(
            cancel(&mgr, &peer.0, &r.id).unwrap()["cancelled"],
            false,
            "a second cancel stopped nothing and must not pretend otherwise"
        );
    }

    /// **A cancel that loses the race must not rewrite history.** `chat_cancel`
    /// authorizes on one read of the row and settles on a second, and between
    /// them it takes a lock, cancels a live transfer and decrypts the whole
    /// outbox. Another task's writer lands in that window: the transfer
    /// completing writes `Sent`, an arriving `FileDecline` writes `Declined`.
    ///
    /// The user taps Cancel as the progress bar completes. If the settle step
    /// trusted the *first* read, it would overwrite the delivered row with
    /// `Failed`/"cancelled" and answer `{cancelled: true}` — the sender's
    /// history would forever claim a file the receiver actually holds was
    /// cancelled, and a peer's refusal would be relabelled as our cancellation.
    ///
    /// So this drives `settle_cancelled` directly: that is the second read, and
    /// reaching it through `chat_cancel` would need the row to change mid-call,
    /// which no single-threaded test can stage. The public entry point is
    /// checked too — its own gate must refuse both rows just as flatly.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_cancel_that_lost_the_race_never_overwrites_a_settled_row() {
        let (mgr, chat, _dir) = test_manager_full("canceller", 0);
        let peer = DeviceId::from("pb-bob");

        // Exactly the two states the shared rule calls final, and exactly the
        // two another task can land while a cancel is in flight.
        let mut seeded = Vec::new();
        for (name, status, why) in [
            (
                "delivered.mp4",
                ChatStatus::Sent,
                "a delivered file must not be relabelled cancelled",
            ),
            (
                "refused.mp4",
                ChatStatus::Declined,
                "a peer's refusal must not be relabelled as our cancellation",
            ),
        ] {
            let r = peerbeam_chat::FileRef::new(name, 4).expect("file ref");
            chat.append(&peerbeam_chat::ChatRecord::file_out(
                &peer,
                &r,
                file_meta(&r),
                status,
            ))
            .expect("seed the settled row");
            seeded.push((r.id, status, why));
        }

        for (id, status, why) in &seeded {
            assert!(
                !mgr.settle_cancelled(&peer, &peer.0, id),
                "settle_cancelled must report it changed nothing: {why}"
            );
            assert_eq!(
                chat.get(&peer, id)
                    .unwrap()
                    .expect("the row survives")
                    .status,
                *status,
                "{why}"
            );
            assert_eq!(
                cancel(&mgr, &peer.0, id).unwrap()["cancelled"],
                false,
                "and the whole cancel path agrees: {why}"
            );
            assert_eq!(
                chat.get(&peer, id)
                    .unwrap()
                    .expect("the row survives")
                    .status,
                *status,
                "still untouched after the public call: {why}"
            );
        }
    }

    // ── the conversation list ───────────────────────────────────

    /// A thread whose only row is a file must be listed — that is the whole
    /// point: a peer discovery cannot see has no entry in the device list, so
    /// without this there is no way to open the conversation you already have
    /// with it. Newest thread first, and `unread_hint` counts only what the
    /// thread is genuinely waiting on the user for.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_conversations_lists_a_file_only_thread_newest_first() {
        let (mgr, chat, _dir) = test_manager_full("lister", 0);
        let quiet = DeviceId::from("pb-quiet");
        let busy = DeviceId::from("pb-busy");

        // A thread with nothing but an outgoing file share.
        let file = peerbeam_chat::FileRef::new("only.bin", 3).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &quiet,
            &file,
            file_meta(&file),
            ChatStatus::Pending,
        ))
        .expect("seed the file-only thread");
        // A newer thread holding one text message and two offers awaiting us.
        for name in ["one.bin", "two.bin"] {
            let offered = peerbeam_chat::FileRef::new(name, 3).expect("file ref");
            chat.append(&peerbeam_chat::ChatRecord::file_in(&busy, &offered))
                .expect("seed an inbound offer");
        }
        let text = peerbeam_chat::ChatMessage::new("hi").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::received(&busy, &text))
            .expect("seed the text");

        let out = mgr.chat_conversations(&json!({})).expect("conversations");
        let peers = out["peers"].as_array().expect("peers array").clone();
        assert_eq!(peers.len(), 2, "both threads: {peers:?}");
        assert_eq!(peers[0]["peer_id"], "pb-busy", "newest first");
        assert_eq!(peers[1]["peer_id"], "pb-quiet");
        assert!(peers[1]["last_timestamp"].is_string());

        // Only the rows genuinely awaiting a decision are counted — not the
        // text (which nothing can tell us was read) and not our own outgoing
        // file.
        assert_eq!(peers[0]["unread_hint"], 2);
        assert_eq!(peers[1]["unread_hint"], 0);
    }

    // ── searching stored history ────────────────────────────────

    /// The call in one pass: a text body and a file name both match, across two
    /// conversations, each hit attributed to the thread it actually lives in
    /// and newest first. A `local_path` is not searched.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_search_finds_text_and_file_names_across_conversations() {
        let (mgr, chat, _dir) = test_manager_full("searcher", 0);
        let alice = DeviceId::from("pb-alice");
        let bob = DeviceId::from("pb-bob");

        let msg = peerbeam_chat::ChatMessage::new("the quarterly invoice").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&alice, &msg))
            .expect("seed alice");
        let file = peerbeam_chat::FileRef::new("invoice-2026.pdf", 9).expect("file ref");
        let mut meta = file_meta(&file);
        // The one field a search must never look at.
        meta.local_path = Some("/home/someone/Downloads/never-searched/x.bin".into());
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &bob,
            &file,
            meta,
            ChatStatus::Sent,
        ))
        .expect("seed bob");

        let out = mgr
            .chat_search(&json!({ "query": "INVOICE" }))
            .expect("search");
        let hits = out["hits"].as_array().expect("hits array").clone();
        assert_eq!(hits.len(), 2, "{hits:?}");
        let peers: Vec<&str> = hits
            .iter()
            .map(|h| h["peer_id"].as_str().unwrap())
            .collect();
        assert!(
            peers.contains(&"pb-alice") && peers.contains(&"pb-bob"),
            "each thread is represented, and by its own id: {hits:?}"
        );
        // Newest first. Both rows were stamped by the clock rather than by the
        // test, so this asserts the ordering property rather than a fixed
        // sequence (the deterministic tie-break is pinned in `peerbeam-chat`).
        let stamps: Vec<&str> = hits
            .iter()
            .map(|h| h["timestamp"].as_str().unwrap())
            .collect();
        assert!(stamps[0] >= stamps[1], "not newest-first: {stamps:?}");
        assert_eq!(out["truncated"], false);
        assert_eq!(out["limit"], DEFAULT_SEARCH_LIMIT as u64);

        // Every hit carries what a surface needs to navigate to it.
        for hit in &hits {
            assert!(hit["message_id"].as_str().is_some_and(|s| !s.is_empty()));
            assert!(hit["timestamp"].as_str().is_some_and(|s| !s.is_empty()));
            assert!(matches!(hit["direction"].as_str(), Some("out")));
            assert!(hit["snippet"]
                .as_str()
                .is_some_and(|s| s.to_lowercase().contains("invoice")));
        }

        // The path is not conversation content and is not matched.
        let none = mgr
            .chat_search(&json!({ "query": "never-searched" }))
            .expect("search");
        assert!(none["hits"].as_array().expect("hits").is_empty());
    }

    /// The field the whole call exists to get right: a surface must be told
    /// there was more, and must be told how many it is looking at.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_search_reports_truncation_and_echoes_the_limit() {
        let (mgr, chat, _dir) = test_manager_full("searcher", 0);
        let peer = DeviceId::from("pb-bob");
        for _ in 0..6 {
            let m = peerbeam_chat::ChatMessage::new("invoice").expect("message");
            chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
                .expect("seed");
        }

        let cut = mgr
            .chat_search(&json!({ "query": "invoice", "limit": 2 }))
            .expect("search");
        assert_eq!(cut["hits"].as_array().expect("hits").len(), 2);
        assert_eq!(cut["truncated"], true);
        assert_eq!(cut["limit"], 2);

        let whole = mgr
            .chat_search(&json!({ "query": "invoice", "limit": 6 }))
            .expect("search");
        assert_eq!(whole["hits"].as_array().expect("hits").len(), 6);
        assert_eq!(
            whole["truncated"], false,
            "an exact fit is complete, not truncated"
        );
    }

    /// An empty query is a cleared search box, not a mistake — and must not be
    /// answered with the entire history either.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_search_validates_its_arguments() {
        let (mgr, chat, _dir) = test_manager_full("searcher", 0);
        let peer = DeviceId::from("pb-bob");
        let m = peerbeam_chat::ChatMessage::new("invoice").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
            .expect("seed");

        // Missing or wrongly-typed `query`: a caller error.
        for bad in [json!({}), json!({ "query": 7 }), json!({ "query": null })] {
            let err = mgr.chat_search(&bad).expect_err("query required");
            assert!(matches!(err.0, Code::InvalidArgument), "{bad}: {err:?}");
        }
        // Empty or whitespace-only: not an error, and not everything.
        for empty in ["", "   ", "\t\n"] {
            let out = mgr
                .chat_search(&json!({ "query": empty }))
                .expect("empty query is answerable");
            assert!(
                out["hits"].as_array().expect("hits").is_empty(),
                "{empty:?}"
            );
            assert_eq!(out["truncated"], false);
        }
        // A limit outside the contract is refused rather than clamped: a
        // surface silently answered a different question would believe it is
        // showing everything.
        for bad in [
            json!(0),
            json!(-1),
            json!(1.5),
            json!("50"),
            json!(MAX_SEARCH_LIMIT as u64 + 1),
        ] {
            let err = mgr
                .chat_search(&json!({ "query": "invoice", "limit": bad }))
                .expect_err("limit out of range");
            assert!(matches!(err.0, Code::InvalidArgument), "{bad}: {err:?}");
        }
        // Absent or explicitly null takes the default.
        for ok in [
            json!({ "query": "invoice" }),
            json!({ "query": "invoice", "limit": null }),
        ] {
            let out = mgr.chat_search(&ok).expect("search");
            assert_eq!(out["limit"], DEFAULT_SEARCH_LIMIT as u64);
            assert_eq!(out["hits"].as_array().expect("hits").len(), 1);
        }
    }

    /// One row this build cannot decode must not take its conversation — or
    /// the search across every other conversation — down with it.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_search_skips_an_undecodable_row_and_keeps_going() {
        let (mgr, chat, raw, _dir) = test_manager_parts("searcher", 0);
        let peer = DeviceId::from("pb-bob");
        let other = DeviceId::from("pb-carol");
        let m = peerbeam_chat::ChatMessage::new("readable invoice").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
            .expect("seed");
        raw.put("chat-pb-bob", "zzz-newer-schema", b"{\"kind\":")
            .expect("seed an unreadable row");
        let n = peerbeam_chat::ChatMessage::new("another invoice").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&other, &n))
            .expect("seed");

        let out = mgr
            .chat_search(&json!({ "query": "invoice" }))
            .expect("search survives an unreadable row");
        let hits = out["hits"].as_array().expect("hits");
        assert_eq!(hits.len(), 2, "{hits:?}");
    }

    /// Deleted history is gone, and search must not resurrect it from
    /// anywhere — including the shared outbox, where a queued message's body
    /// also lives.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_search_cannot_reach_a_deleted_conversation() {
        let (mgr, chat, _dir) = test_manager_full("searcher", 0);
        let peer = DeviceId::from("pb-bob");
        let m = peerbeam_chat::ChatMessage::new("deleted invoice").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
            .expect("seed");
        assert_eq!(
            mgr.chat_search(&json!({ "query": "invoice" }))
                .expect("search")["hits"]
                .as_array()
                .expect("hits")
                .len(),
            1
        );

        mgr.chat_delete(&json!({ "peer_id": "pb-bob" }))
            .expect("delete");

        let out = mgr
            .chat_search(&json!({ "query": "invoice" }))
            .expect("search");
        assert!(out["hits"].as_array().expect("hits").is_empty());
    }

    /// The outbox lives in the same AppStore as the conversations and must never
    /// appear as one — its namespace is `chat.outbox`, a dot, precisely so a
    /// `chat-` prefix scan cannot pick it up.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_conversations_never_lists_the_outbox_as_a_peer() {
        let (mgr, chat, dir) = test_manager_full("lister", 0);
        let peer = DeviceId::from("pb-bob");
        seed_queued_file(&chat, &dir.path().join("outbox-blobs"), &peer, "queued.bin");

        let out = mgr.chat_conversations(&json!({})).expect("conversations");
        let peers = out["peers"].as_array().expect("peers array").clone();
        assert_eq!(peers.len(), 1, "{peers:?}");
        assert_eq!(peers[0]["peer_id"], "pb-bob");
    }

    // ── deleting a conversation ─────────────────────────────────

    /// **Deleting a thread must not disarm the queue.** This is the whole trap:
    /// the drain re-opens the record a queue entry is named after, and reads a
    /// *missing* record as "nothing will ever settle this" — releasing the
    /// entry and deleting the staged bytes. So the delete keeps that record,
    /// and this asserts the consequence in the drain's own terms rather than
    /// only in the store's: [`Manager::row_may_still_deliver`] — the exact
    /// predicate `run_queued_file` consults before letting go — still answers
    /// yes afterwards.
    ///
    /// It also proves the delete is not simply a no-op: the settled history
    /// around the queued file really is gone.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_keeps_a_queued_file_deliverable_and_removes_the_rest() {
        let (mgr, chat, dir) = test_manager_full("deleter", 0);
        let peer = DeviceId::from("pb-bob");

        for body in ["settled one", "settled two"] {
            let m = peerbeam_chat::ChatMessage::new(body).expect("message");
            chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
                .expect("seed history");
        }
        let (queued_id, blob) = seed_queued_file(
            &chat,
            &dir.path().join("outbox-blobs"),
            &peer,
            "waiting.mkv",
        );

        let out = mgr
            .chat_delete(&json!({ "peer_id": "pb-bob" }))
            .expect("delete");
        assert_eq!(out["removed"], 2, "the settled text, and only that");
        assert_eq!(out["kept"], 1, "the record backing the queued file");

        // The drain's own decision, unchanged by the delete.
        assert!(
            mgr.row_may_still_deliver(&peer, &queued_id),
            "the delete must leave the queued file deliverable — a missing row \
             here is what makes `run_queued_file` throw the bytes away"
        );
        assert_eq!(
            chat.outbox_for(&peer).expect("outbox").len(),
            1,
            "the entry is still queued"
        );
        assert!(blob.exists(), "and its staged bytes are still on disk");

        // Not a no-op: the rest of the thread is gone.
        let left = chat.history(&peer).expect("history");
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(left[0].id, queued_id);
    }

    /// With nothing queued the thread goes completely, and stops being listed —
    /// which is what makes the row disappear from a surface rather than come
    /// back empty.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_with_nothing_queued_removes_the_whole_thread() {
        let (mgr, chat, _dir) = test_manager_full("deleter", 0);
        let peer = DeviceId::from("pb-bob");
        for body in ["one", "two", "three"] {
            let m = peerbeam_chat::ChatMessage::new(body).expect("message");
            chat.append(&peerbeam_chat::ChatRecord::received(&peer, &m))
                .expect("seed history");
        }

        let out = mgr
            .chat_delete(&json!({ "peer_id": "pb-bob" }))
            .expect("delete");
        assert_eq!(out["removed"], 3);
        assert_eq!(out["kept"], 0, "nothing was queued, so nothing is kept");

        let listed = mgr.chat_conversations(&json!({})).expect("conversations");
        assert!(
            listed["peers"].as_array().expect("peers").is_empty(),
            "a deleted thread must not still be listed: {listed}"
        );

        // Deleting again is honest about having found nothing, rather than an
        // error a surface that raced itself has to handle.
        let again = mgr
            .chat_delete(&json!({ "peer_id": "pb-bob" }))
            .expect("delete");
        assert_eq!(again["removed"], 0);
        assert_eq!(again["kept"], 0);
    }

    /// **A file still being copied into the outbox is kept, and reported.**
    ///
    /// `chat_send_file` writes the row `Staging` and returns immediately; the
    /// copy runs in a spawned task for as long as the file is big. Nothing
    /// reaches the outbox until it finishes, so for that whole window an
    /// outbox-only keep set does not name the row — and a delete landing there
    /// removed it, leaving the finished copy to queue an entry with no record
    /// behind it. `run_queued_file` offers that to the peer and only then
    /// throws the entry and the bytes away.
    ///
    /// Driven by seeding the row rather than by racing a real copy: the whole
    /// window is "however long the copy takes", so a test that tried to land a
    /// delete inside it would either be slow and disk-hungry or would silently
    /// stop testing anything the day the copy got faster. What is asserted
    /// here is the FFI's own decision at the seam — kept, and counted.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_keeps_and_reports_a_file_that_is_still_staging() {
        let (mgr, chat, _dir) = test_manager_full("deleter", 0);
        let peer = DeviceId::from("pb-bob");

        let m = peerbeam_chat::ChatMessage::new("settled").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
            .expect("seed history");
        // Exactly what `begin_file_send` leaves behind before the copy starts.
        let r = peerbeam_chat::FileRef::new("holiday.mp4", 8_000_000_000).expect("file ref");
        chat.append(&peerbeam_chat::ChatRecord::file_out(
            &peer,
            &r,
            peerbeam_chat::FileMeta::new(&r.name, r.size, Some("/home/me/holiday.mp4".into())),
            ChatStatus::Staging,
        ))
        .expect("seed the staging row");
        assert!(
            chat.outbox_for(&peer).expect("outbox").is_empty(),
            "nothing is queued until the copy finishes — that IS the window"
        );

        let out = mgr
            .chat_delete(&json!({ "peer_id": "pb-bob" }))
            .expect("delete");
        assert_eq!(out["removed"], 1, "the settled text, and only that: {out}");
        assert_eq!(
            out["kept"], 1,
            "a file the user attached seconds ago is 'still waiting to be sent': {out}"
        );

        let left = chat.history(&peer).expect("history");
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(left[0].id, r.id);
        assert_eq!(left[0].status, ChatStatus::Staging);
        // The thread stays listed, so the kept row is visible immediately
        // rather than reappearing when the copy lands.
        let listed = mgr.chat_conversations(&json!({})).expect("conversations");
        assert_eq!(listed["peers"].as_array().expect("peers").len(), 1);
    }

    /// **`kept` counts what survived, including a row this build cannot read.**
    ///
    /// `delete_conversation` keeps rows by stored key, decodable or not, so a
    /// kept row written by a newer schema is really there. Counting it through
    /// a decode instead dropped it: the user was told "Deleted 1 message" with
    /// no mention of anything kept, while the thread stayed listed with a row
    /// in it they can neither see nor remove.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_counts_a_kept_row_it_cannot_decode() {
        let (mgr, chat, raw, dir) = test_manager_parts("deleter", 0);
        let peer = DeviceId::from("pb-bob");

        // One removable settled message, and one queued file whose row is then
        // overwritten with something only a newer build could read.
        let m = peerbeam_chat::ChatMessage::new("settled").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
            .expect("seed history");
        let (queued_id, _blob) = seed_queued_file(
            &chat,
            &dir.path().join("outbox-blobs"),
            &peer,
            "waiting.mkv",
        );
        raw.put(
            &peerbeam_chat::namespace(&peer),
            &queued_id,
            b"{\"from\":\"the-future\"}",
        )
        .expect("make the kept row undecodable");
        assert_eq!(
            chat.history(&peer).expect("history").len(),
            1,
            "the undecodable row is invisible to `history` — that is the trap"
        );

        let out = mgr
            .chat_delete(&json!({ "peer_id": "pb-bob" }))
            .expect("delete");
        assert_eq!(out["removed"], 1, "the settled text: {out}");
        assert_eq!(
            out["kept"], 1,
            "the row backing the queued file was kept and must be reported: {out}"
        );
        assert!(
            mgr.chat
                .contains(&peer, &queued_id)
                .expect("the kept row is still on disk"),
            "and it really is still there — the count is an observation"
        );
    }

    /// **A queued decline must not keep the inbound row it names.**
    ///
    /// Its `message_id` is the sender's `FileRef` id, so in our namespace it
    /// names the row we *refused*. Keeping it reported "1 queued message was
    /// kept and will still be sent" about a file the user turned down, and left
    /// the thread listed and undeletable until that peer returned.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_does_not_keep_a_row_for_a_queued_decline() {
        let (mgr, chat, _dir) = test_manager_full("deleter", 0);
        let peer = DeviceId::from("pb-bob");

        let theirs = peerbeam_chat::FileRef::new("theirs.iso", 900).expect("file ref");
        let mut declined = peerbeam_chat::ChatRecord::file_in(&peer, &theirs);
        declined.status = ChatStatus::Declined;
        chat.append(&declined).expect("seed the declined row");
        assert!(chat
            .enqueue_decline(&peer, &peerbeam_chat::FileDecline::new(&theirs.id))
            .expect("queue the decline"));

        let out = mgr
            .chat_delete(&json!({ "peer_id": "pb-bob" }))
            .expect("delete");
        assert_eq!(out["removed"], 1, "{out}");
        assert_eq!(
            out["kept"], 0,
            "a refused file is not something the user is still sending: {out}"
        );

        let listed = mgr.chat_conversations(&json!({})).expect("conversations");
        assert!(
            listed["peers"].as_array().expect("peers").is_empty(),
            "the thread is gone rather than waiting on a peer that may never \
             return: {listed}"
        );
        // The decline itself is untouched and still goes out when he does.
        let queued = chat.outbox_for(&peer).expect("outbox");
        assert_eq!(queued.len(), 1, "{queued:?}");
        assert_eq!(queued[0].message_id, theirs.id);
    }

    /// An absent or empty `peer_id` is a caller bug, not "some conversation" —
    /// held to the same rule as `chat_cancel`, the increment's other
    /// destructive call.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_requires_a_non_empty_peer_id() {
        let (mgr, chat, _dir) = test_manager_full("deleter", 0);
        let peer = DeviceId::from("pb-bob");
        let m = peerbeam_chat::ChatMessage::new("still here").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
            .expect("seed history");

        for bad in [json!({}), json!({ "peer_id": "" }), json!({ "peer_id": 7 })] {
            // A `let`-else rather than `expect_err`, whose message is a `&str`:
            // `expect_err("…: {bad}")` printed the braces literally, so a
            // regression named none of the cases it was walking through.
            let Err(err) = mgr.chat_delete(&bad) else {
                panic!("peer_id is required, but {bad} was accepted");
            };
            assert_eq!(err.0.as_str(), Code::InvalidArgument.as_str(), "{bad}");
        }
        assert_eq!(
            chat.history(&peer).expect("history").len(),
            1,
            "and nothing was deleted on the way to refusing"
        );
    }

    /// **The refusal for an unreadable outbox entry carries its own code.**
    ///
    /// `delete_conversation` refuses whenever the shared outbox holds an
    /// entry it cannot decode (see its doc for why guessing here is worse
    /// than refusing). Before this, that refusal reached the caller as bare
    /// `Internal` — indistinguishable from any other unexpected failure —
    /// which left the only user-visible message "Something went wrong.
    /// Please try again", advice that can never be followed to a fix, since
    /// retrying never touches the offending entry.
    ///
    /// The corrupted entry deliberately belongs to a DIFFERENT peer than the
    /// one being deleted: the outbox is shared across every conversation, so
    /// this is the actual failure mode — one unrelated peer's unreadable
    /// entry blocks every other conversation's delete, not just its own.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_reports_queue_unreadable_for_an_undecodable_outbox_entry() {
        let (mgr, chat, raw, dir) = test_manager_parts("deleter", 0);
        let peer = DeviceId::from("pb-bob");

        let m = peerbeam_chat::ChatMessage::new("settled").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
            .expect("seed history");
        seed_queued_file(
            &chat,
            &dir.path().join("outbox-blobs"),
            &peer,
            "waiting.mkv",
        );

        // An entirely unrelated peer's outbox entry, corrupted — the shape a
        // newer schema takes to this build.
        let other = peerbeam_chat::FileRef::new("unrelated.bin", 4).expect("file ref");
        raw.put(
            peerbeam_chat::OUTBOX_NS,
            &other.id,
            b"{\"from\":\"the-future\"}",
        )
        .expect("corrupt an unrelated peer's outbox entry");

        let err = mgr
            .chat_delete(&json!({ "peer_id": "pb-bob" }))
            .expect_err("an unreadable outbox entry anywhere must refuse the delete");
        assert_eq!(err.0.as_str(), Code::QueueUnreadable.as_str());

        // Nothing was deleted on the way to refusing.
        assert_eq!(
            chat.history(&peer).expect("history").len(),
            2,
            "the settled text and the queued file's row are both untouched"
        );
    }

    /// **A genuine store failure still reports `Internal`.** The previous
    /// test proves the new code fires for an unreadable outbox entry; this
    /// one proves it does NOT fire for just any failure — a real store error
    /// must still surface as `Internal`, so the two are told apart by the
    /// actual `ChatError` variant `delete_conversation` returns, never by the
    /// shape of whatever went wrong.
    ///
    /// `FsAppStore` validates a namespace's characters before ever touching
    /// disk, so a `peer_id` that cannot form a valid namespace (`/` is not in
    /// `[A-Za-z0-9._-]`) deterministically fails the conversation-namespace
    /// list call inside `delete_conversation` — a different failure than an
    /// unreadable outbox entry, and one no amount of outbox decoding could
    /// have avoided. Nothing needs to be queued for this: the failure happens
    /// on the conversation's OWN namespace, after the (empty, and therefore
    /// trivially readable) outbox keep set has already been established.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_still_reports_internal_for_a_genuine_store_failure() {
        let (mgr, _chat, _dir) = test_manager_full("deleter", 0);

        let err = mgr
            .chat_delete(&json!({ "peer_id": "pb/bob" }))
            .expect_err("an invalid conversation namespace must fail the delete");
        assert_eq!(err.0.as_str(), Code::Internal.as_str());
    }

    // ── deleting selected messages ──────────────────────────────

    fn delete_messages(mgr: &Manager, peer_id: &str, ids: &[&str]) -> Op {
        mgr.chat_delete_messages(&json!({ "peer_id": peer_id, "message_ids": ids }))
    }

    /// The JSON in and out, and the one thing that makes the `kept` list worth
    /// returning: a selection that mixes settled history with still-queued
    /// files removes the first, keeps the rest, and **names** them — while the
    /// drain's own predicate still says each queued file can be delivered.
    ///
    /// TWO queued files, deliberately. Against a one-element `kept` a bug that
    /// reported only the first id is invisible, and the surface built on it
    /// would tell the user "1 kept" about two files it could not delete.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_messages_removes_the_settled_and_names_what_it_kept() {
        let (mgr, chat, dir) = test_manager_full("deleter", 0);
        let peer = DeviceId::from("pb-bob");

        let mut settled = Vec::new();
        for body in ["one", "two"] {
            let m = peerbeam_chat::ChatMessage::new(body).expect("message");
            chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
                .expect("seed history");
            settled.push(m.id);
        }
        let blob_root = dir.path().join("outbox-blobs");
        let (first_id, first_blob) = seed_queued_file(&chat, &blob_root, &peer, "waiting.mkv");
        let (second_id, second_blob) =
            seed_queued_file(&chat, &blob_root, &peer, "also-waiting.iso");

        let out =
            delete_messages(&mgr, "pb-bob", &[&settled[0], &first_id, &second_id]).expect("delete");
        assert_eq!(
            out["removed"], 1,
            "the settled message, and only that: {out}"
        );
        assert_eq!(
            out["kept"],
            json!([first_id, second_id]),
            "EVERY kept id is named, not counted and not just the first: {out}"
        );

        // The drain's own decision, unchanged by the delete.
        for (id, blob) in [(&first_id, &first_blob), (&second_id, &second_blob)] {
            assert!(
                mgr.row_may_still_deliver(&peer, id),
                "a missing row here is what makes `run_queued_file` throw the bytes away"
            );
            assert!(blob.exists(), "and the staged bytes are still on disk");
        }

        // Not a no-op, and not over-broad: the unselected message stays.
        let left: Vec<String> = chat
            .history(&peer)
            .expect("history")
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(left.len(), 3, "{left:?}");
        assert!(left.contains(&settled[1]), "the unselected row survives");
        assert!(left.contains(&first_id));
        assert!(left.contains(&second_id));
    }

    /// An id in no thread at all is neither removed nor kept — a surface must
    /// not be told a message it can no longer see is still on its way out.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_messages_ignores_an_unknown_id() {
        let (mgr, chat, _dir) = test_manager_full("deleter", 0);
        let peer = DeviceId::from("pb-bob");
        let m = peerbeam_chat::ChatMessage::new("here").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
            .expect("seed history");

        let out = delete_messages(&mgr, "pb-bob", &[&m.id, "0000000000000-nope"]).expect("delete");
        assert_eq!(out["removed"], 1, "{out}");
        assert_eq!(out["kept"], json!([]), "{out}");
        assert!(chat.history(&peer).expect("history").is_empty());
    }

    /// Both arguments are required, and `message_ids` must be an array of
    /// non-empty strings. An empty array is deliberately NOT a validation
    /// error: it asks for nothing and gets nothing, which is what a surface
    /// whose selection emptied itself between the render and the tap should be
    /// handed.
    ///
    /// Not a validation error is not the same as always `Ok`, and the case
    /// below is asserted from the healthy store this test builds. The keep rule
    /// is established before a single id is looked at, so an empty request
    /// against an outbox that cannot be read completely still refuses with
    /// `QueueUnreadable` — see `chat_delete_messages`'s own comment on why that
    /// is left alone rather than special-cased.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_messages_validates_its_arguments() {
        let (mgr, chat, _dir) = test_manager_full("deleter", 0);
        let peer = DeviceId::from("pb-bob");
        let m = peerbeam_chat::ChatMessage::new("still here").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
            .expect("seed history");

        for bad in [
            json!({ "message_ids": ["x"] }),
            json!({ "peer_id": "", "message_ids": ["x"] }),
            json!({ "peer_id": "pb-bob" }),
            json!({ "peer_id": "pb-bob", "message_ids": "x" }),
            json!({ "peer_id": "pb-bob", "message_ids": [7] }),
            json!({ "peer_id": "pb-bob", "message_ids": [""] }),
        ] {
            // A `let`-else rather than `expect_err`, whose message is a `&str`:
            // `expect_err("…: {bad}")` printed the braces literally, so a
            // regression named none of the six cases it was walking through.
            let Err(err) = mgr.chat_delete_messages(&bad) else {
                panic!("must be refused, but {bad} was accepted");
            };
            assert_eq!(err.0.as_str(), Code::InvalidArgument.as_str(), "{bad}");
        }
        assert_eq!(
            chat.history(&peer).expect("history").len(),
            1,
            "and nothing was deleted on the way to refusing"
        );

        let empty = mgr
            .chat_delete_messages(&json!({ "peer_id": "pb-bob", "message_ids": [] }))
            .expect("an empty selection is a no-op, not a failure");
        assert_eq!(empty["removed"], 0);
        assert_eq!(empty["kept"], json!([]));
        assert_eq!(chat.history(&peer).expect("history").len(), 1);
    }

    /// **The refusal for an unreadable outbox entry carries its own code**, the
    /// same way `chat_delete`'s does — and for the same reason: retrying will
    /// not clear it, since the offending entry need not even belong to the
    /// conversation being deleted. The corrupted entry here belongs to a
    /// different peer, which is the actual failure mode.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_messages_reports_queue_unreadable_for_an_undecodable_entry() {
        let (mgr, chat, raw, dir) = test_manager_parts("deleter", 0);
        let peer = DeviceId::from("pb-bob");

        let m = peerbeam_chat::ChatMessage::new("settled").expect("message");
        chat.append(&peerbeam_chat::ChatRecord::sent(&peer, &m))
            .expect("seed history");
        seed_queued_file(
            &chat,
            &dir.path().join("outbox-blobs"),
            &peer,
            "waiting.mkv",
        );

        let other = peerbeam_chat::FileRef::new("unrelated.bin", 4).expect("file ref");
        raw.put(
            peerbeam_chat::OUTBOX_NS,
            &other.id,
            b"{\"from\":\"the-future\"}",
        )
        .expect("corrupt an unrelated peer's outbox entry");

        let err = delete_messages(&mgr, "pb-bob", &[&m.id])
            .expect_err("an unreadable outbox entry anywhere must refuse the delete");
        assert_eq!(err.0.as_str(), Code::QueueUnreadable.as_str());
        assert_eq!(
            chat.history(&peer).expect("history").len(),
            2,
            "and nothing was deleted on the way to refusing"
        );
    }

    /// A genuine store failure still reports `Internal`, so the two are told
    /// apart by the `ChatError` variant rather than by whatever went wrong.
    #[tokio::test]
    #[serial_test::serial]
    async fn chat_delete_messages_still_reports_internal_for_a_genuine_store_failure() {
        let (mgr, _chat, _dir) = test_manager_full("deleter", 0);
        let err = delete_messages(&mgr, "pb/bob", &["anything"])
            .expect_err("an invalid conversation namespace must fail the delete");
        assert_eq!(err.0.as_str(), Code::Internal.as_str());
    }

    // ── auto-save rules: the manager's copy of the list ──────────

    fn catch_all(dir: &str) -> peerbeam_config::SaveRule {
        peerbeam_config::SaveRule {
            directory: dir.to_string(),
            ..peerbeam_config::SaveRule::default()
        }
    }

    /// **The "nothing changed" default.** A manager built with no rules
    /// resolves every item to the save directory — the behaviour that shipped
    /// before this feature existed.
    #[tokio::test]
    #[serial_test::serial]
    async fn with_no_rules_everything_resolves_to_the_save_directory() {
        let (mgr, _chat, dir) = test_manager_full("receiver", 0);
        assert!(mgr.save_rules().is_empty());

        let resolved = peerbeam_config::rules::destination(
            &mgr.save_rules(),
            &mgr.save_dir(),
            "pb-alice000001",
            "report.pdf",
            10,
        );
        assert_eq!(resolved.directory, dir.path().to_string_lossy());
        assert!(resolved.fallback.is_none());
    }

    /// A rule list applied live (a Settings edit) is what the *next* receive
    /// resolves against — not the list the engine started with. The whole
    /// point of holding it behind a lock rather than reading it once at init.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_live_rule_edit_applies_to_the_next_item() {
        let (mgr, _chat, dir) = test_manager_full("receiver", 0);
        let sorted = dir.path().join("sorted");

        mgr.set_save_rules(vec![catch_all(&sorted.to_string_lossy())]);
        assert_eq!(mgr.save_rules().len(), 1, "rules apply without a restart");

        let resolved = peerbeam_config::rules::destination(
            &mgr.save_rules(),
            &mgr.save_dir(),
            "pb-alice000001",
            "report.pdf",
            10,
        );
        assert_eq!(resolved.directory, sorted.to_string_lossy());

        // …and clearing the list puts the save directory back in charge.
        mgr.set_save_rules(Vec::new());
        let resolved = peerbeam_config::rules::destination(
            &mgr.save_rules(),
            &mgr.save_dir(),
            "pb-alice000001",
            "report.pdf",
            10,
        );
        assert_eq!(resolved.directory, dir.path().to_string_lossy());
    }

    /// **The platform gate is on the read, not on each write.** However a rule
    /// list got into the manager — init, a live settings edit, a hand-edited
    /// document — a build that cannot honour a destination must consult none of
    /// them. On desktop this build can, so the same call returns the list.
    #[tokio::test]
    #[serial_test::serial]
    async fn the_platform_gate_is_checked_where_the_rules_are_read() {
        let (mgr, _chat, dir) = test_manager_full("receiver", 0);
        mgr.set_save_rules(vec![catch_all(
            &dir.path().join("sorted").to_string_lossy(),
        )]);

        if crate::rules::SUPPORTED {
            assert_eq!(mgr.save_rules().len(), 1);
        } else {
            assert!(
                mgr.save_rules().is_empty(),
                "a platform that cannot write an absolute path must consult no rules"
            );
        }
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;

    #[test]
    fn a_watch_key_cannot_be_forged_by_a_path_containing_the_separator() {
        // Joined on a control character precisely because slashes and dashes
        // occur in real paths: a separator that can appear in the values it
        // separates is a collision waiting to happen.
        assert_ne!(watch_key("a", "b/c"), watch_key("a/b", "c"));
        assert_ne!(watch_key("a", "b-c"), watch_key("a-b", "c"));
        assert_eq!(watch_key("a", "b"), watch_key("a", "b"));
    }

    #[test]
    fn observing_a_folder_reports_files_by_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"one").unwrap();
        std::fs::write(dir.path().join("sub").join("b.txt"), b"two").unwrap();

        let mut seen: Vec<String> = observe_folder(dir.path())
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        seen.sort();
        assert_eq!(seen, vec!["a.txt", "sub/b.txt"]);
    }

    #[test]
    fn an_observation_carries_the_size_the_settling_rule_compares() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"12345").unwrap();
        let seen = observe_folder(dir.path());
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1.size, 5);
    }

    /// A growing file must never settle, which is what stops a watch from
    /// syncing a half-written copy. Checked here against the real observer
    /// rather than only against hand-made fixtures.
    #[test]
    fn a_file_still_being_written_does_not_settle_through_the_real_observer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("growing.bin");
        let mut settling = peerbeam_sync::Settling::new();

        for n in [10usize, 200, 4000] {
            std::fs::write(&path, vec![0u8; n]).unwrap();
            assert!(
                settling.observe(&observe_folder(dir.path())).is_empty(),
                "a file still growing settled at {n} bytes"
            );
        }
        // The loop's last pass already recorded the file at 4000 bytes, so the
        // next identical observation is its *second* consecutive hold and it
        // settles there. Getting this off by one is easy — the count is of
        // consecutive matching observations, not of calls after the writing
        // stops.
        assert_eq!(
            settling.observe(&observe_folder(dir.path())),
            vec!["growing.bin"]
        );
    }
}
