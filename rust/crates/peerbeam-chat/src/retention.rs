//! Disappearing messages: a per-conversation window after which this device's
//! copy of a message stops being readable and is deleted.
//!
//! # Two mechanisms, two different promises
//!
//! **Read-time filtering** ([`ChatStore::history`], [`ChatStore::get`],
//! [`ChatStore::outbox_pending`]) decides what is *shown* and what still
//! *leaves this machine*. It is the guarantee, because it holds whether or not
//! anything has run: a window that closed one second ago closes the next read,
//! on a device that has been asleep for a month, with no background task alive
//! anywhere. `peerbeam_domain::entity::TrustRecord::expires_at` chose exactly
//! this shape for trust expiry and its doc carries the long form — *"a sweep
//! that has not happened yet is a device still trusted after its window
//! closed, and the interval between sweeps is exactly the interval an attacker
//! wants"*. A message still readable after its window closed is the same
//! defect wearing different clothes.
//!
//! **Pruning** ([`ChatStore::prune`], and [`prune_conversation`] which also
//! collects the bytes) decides what is *kept*. Filtering alone would be a lie
//! about disk: the row would still be there, in an encrypted store whose key
//! sits on the same machine, for anybody who later gets both. So the two run
//! together and neither is optional — filtering is what makes the promise
//! true on time, pruning is what makes it true about bytes.
//!
//! # What this device can and cannot promise
//!
//! It can promise this: **a message is readable on this device for at most the
//! window, and is then deleted from it.** That is measurable and this device
//! keeps it alone.
//!
//! It cannot promise anything about the peer's copy, and this module
//! deliberately does not try. The window is **local**: no frame announces it,
//! nothing asks the peer to delete, and a peer running any build at all keeps
//! whatever it kept. A "delete on both sides" message would be a request the
//! other end is free to ignore, log, or have already backed up — a promise
//! this project cannot keep, and one that would be worse than saying nothing
//! because a user would believe it. `docs/SECURITY.md` states the same limit
//! for every other local delete in this crate: *"local only … the peer keeps
//! its own copy. This is 'forget this thread here', never 'unsend'."*
//!
//! It also does not delete **received files** off the user's disk. A file the
//! user accepted is saved where they chose and is theirs; the conversation row
//! that describes it disappears, the file does not. Deleting a user's files
//! from Downloads because a chat window closed would be data loss wearing a
//! privacy feature's clothes.
//!
//! # Off by default, and off for everything that already exists
//!
//! A conversation with no policy record has no window, and that is every
//! conversation on the machine the moment this ships. Nothing about an upgrade
//! may start deleting a user's history — the same rule
//! `TrustRecord::expires_at` follows for the same reason, and the reason every
//! fail-safe below is contained: **none of them can fire in a conversation
//! whose window is off.**
//!
//! [`ChatStore::history`]: crate::ChatStore::history
//! [`ChatStore::get`]: crate::ChatStore::get
//! [`ChatStore::outbox_pending`]: crate::ChatStore::outbox_pending
//! [`ChatStore::prune`]: crate::ChatStore::prune

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use peerbeam_domain::id::DeviceId;

use crate::message::ChatError;
use crate::staging::StagingStore;
use crate::store::ChatStore;

/// The AppStore namespace holding one [`Retention`] per peer, keyed by device
/// id.
///
/// A `.` as the fifth character, like [`OUTBOX_NS`], and for the reason that
/// constant's doc spells out at length: `ChatStore::namespace` always produces
/// `chat-<peer_id>` with a dash, and device ids are peer-supplied over the
/// wire, so a peer that claims the id `retention` must not land its
/// conversation on top of this. `chat-retention` and `chat.retention` are
/// different namespaces; `chat-retention` and `chat-retention` would not be.
///
/// [`OUTBOX_NS`]: crate::OUTBOX_NS
pub const RETENTION_NS: &str = "chat.retention";

/// The longest window this type will accept: a hundred years.
///
/// Not a taste judgement about how long a conversation should live — it is the
/// point past which a window stops meaning anything. "Delete after a century"
/// is *keep forever* said confusingly, and [`Retention::OFF`] is how keep
/// forever is said clearly. The bound also keeps every subtraction in
/// [`Retention::cutoff`] far inside `chrono`'s range, so a legal window can
/// never silently become "never expires" through arithmetic that saturated.
pub const MAX_TTL_SECS: u64 = 100 * 365 * 24 * 60 * 60;

/// One conversation's disappearing-message window.
///
/// # Why the policy is live rather than stamped onto each record
///
/// The alternative — compute a deadline when a message is written and store it
/// on the row, as `TrustRecord::expires_at` does — was rejected for one
/// concrete reason: **tightening the window is the privacy-urgent direction,
/// and stamping makes it do nothing.** A user who changes a thread from a week
/// to ten minutes is telling this device to get rid of what is there now.
/// Stamped deadlines would leave every existing row on its old, longer clock,
/// so the thread would hold three different answers to a question the user
/// thinks they have given one answer to, and the current setting would explain
/// none of them. Reading the window live means one setting, one answer, applied
/// to every row at once, with no migration and nothing to keep in step.
///
/// A trust record is the other way round — a deadline the user attached to *one
/// grant*, not a standing rule over a collection — which is why the two
/// deliberately differ here while sharing everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Retention {
    /// How long a message stays readable on this device, in seconds.
    ///
    /// `None` — the default, and what every conversation predating this feature
    /// loads as — is history kept until something deletes it.
    ///
    /// `#[serde(default)]` so a policy blob written by an older or newer build
    /// still decodes, and `skip_serializing_if` so [`Retention::OFF`] is `{}`
    /// on disk rather than a null nobody needs. Absent is **not** the
    /// fail-closed direction here and must never be made one: reading a missing
    /// window as "zero" would delete every conversation on the machine the
    /// moment the user upgraded, which is precisely the trap
    /// `TrustRecord::expires_at` documents on its own `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
}

impl Retention {
    /// Keep history until something deletes it — the default, and what an
    /// absent policy record means.
    pub const OFF: Retention = Retention { ttl_secs: None };

    /// A window of `secs` seconds.
    ///
    /// Rejects `0`, which has no honest reading: as "delete immediately" it
    /// makes the conversation unusable, and as "off" it silently discards an
    /// instruction the user gave. [`OFF`](Self::OFF) is how off is said.
    /// Rejects anything above [`MAX_TTL_SECS`] for the reason on that constant.
    pub fn for_secs(secs: u64) -> Result<Retention, ChatError> {
        if secs == 0 || secs > MAX_TTL_SECS {
            return Err(ChatError::BadRetention(format!(
                "window {secs}s is outside 1..={MAX_TTL_SECS} (use Retention::OFF to keep history)"
            )));
        }
        Ok(Retention {
            ttl_secs: Some(secs),
        })
    }

    /// Whether this conversation keeps its history.
    #[must_use]
    pub fn is_off(&self) -> bool {
        self.ttl_secs.is_none()
    }

    /// The instant at which the window opens on to the past: anything this
    /// device stored at or before it has disappeared as of `now`.
    ///
    /// `None` when the window is off — there is no such instant — which is what
    /// makes every fail-safe in [`closed_on`](Self::closed_on) unreachable for
    /// a conversation the user has not put a clock on.
    ///
    /// `now` is a parameter rather than a clock read, the same shape as
    /// `TrustRecord::has_expired` and `peerbeam_transfer::is_expired`, so the
    /// boundary is asserted exactly instead of by a test that sleeps and hopes.
    /// The layer that *is* asking about the present reads the clock: see
    /// [`ChatStore::history`](crate::ChatStore::history).
    #[must_use]
    pub fn cutoff(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let secs = self.ttl_secs?;
        // Both fallbacks are unreachable for a window `for_secs` accepted; they
        // are here so a policy blob hand-edited to an absurd number degrades to
        // "keeps history" rather than to a deadline nobody can predict.
        let ttl = Duration::try_seconds(i64::try_from(secs).ok()?)?;
        now.checked_sub_signed(ttl)
    }

    /// Whether a row whose age is measured from `basis` has disappeared as of
    /// `now`.
    ///
    /// Gone **at** the deadline, not a tick after it: a ten-minute window on a
    /// message stored at 10:00 is over at 10:10:00 exactly. That is the same
    /// "invalid at or after" boundary `TrustRecord::has_expired` and a resume
    /// token already follow, and having a third rule would be a third thing to
    /// remember.
    ///
    /// # `None` is treated as disappeared, and why that is safe here
    ///
    /// `basis` is `None` when this device cannot establish how old a row is —
    /// a row written before [`ChatRecord::stored_at`] existed whose `timestamp`
    /// is not RFC 3339. That is reachable: an inbound row copies the *peer's*
    /// timestamp string verbatim and nothing on the wire validates its shape,
    /// so a peer that sent junk in an older build left rows this device cannot
    /// date. Keeping them would hand any peer a way to make its messages
    /// outlive the user's window forever, by sending a timestamp nobody can
    /// parse.
    ///
    /// This is the opposite direction from `TrustRecord::expires_at`'s absent
    /// deadline, and deliberately: there, "absent" would have revoked every
    /// device on the machine at upgrade. Here nothing is reachable at all
    /// unless the user has set a window on this one conversation, and inside a
    /// conversation whose whole point is that things do not last, an age that
    /// cannot be proved to be inside the window is not inside it.
    ///
    /// [`ChatRecord::stored_at`]: crate::ChatRecord::stored_at
    #[must_use]
    pub fn closed_on(&self, basis: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        let Some(cutoff) = self.cutoff(now) else {
            return false;
        };
        // An explicit `match` rather than `Option::is_none_or` so it compiles
        // on the workspace MSRV (1.80), as `peerbeam_transfer::pipe::gate`
        // already does — and the two cases are the two paragraphs above.
        match basis {
            Some(stored) => stored <= cutoff,
            None => true,
        }
    }

    /// Serialize for the AppStore.
    pub(crate) fn encode(&self) -> Result<Vec<u8>, ChatError> {
        serde_json::to_vec(self).map_err(|e| ChatError::Serialization(e.to_string()))
    }

    /// Deserialize from AppStore bytes.
    ///
    /// A blob that will not decode is an **error**, never a quiet
    /// [`OFF`](Self::OFF). The two are not interchangeable: reporting "no
    /// window" for a window we failed to read would answer a privacy question
    /// with the least private answer available, and would do it silently, on
    /// every read, forever. A caller that gets this back has a store problem to
    /// fix, not a policy.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Retention, ChatError> {
        serde_json::from_slice(bytes).map_err(|e| ChatError::Serialization(e.to_string()))
    }
}

/// What a prune removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pruned {
    /// Conversation rows deleted from local history.
    pub records: usize,
    /// Queued outbound entries deleted, each one a message that will now never
    /// be sent. A queued message is content sitting on this device exactly like
    /// a delivered one, and its window applies to it in the same way — see
    /// [`ChatStore::prune`](crate::ChatStore::prune) for why not sending it is
    /// the point rather than a side effect.
    pub queued: usize,
    /// Staged blobs those entries owned: full copies of files the user had
    /// queued to send, which nothing owns any more.
    ///
    /// Returned rather than deleted because [`ChatStore`] has no storage
    /// provider and must not grow one to delete a file. [`prune_conversation`]
    /// is the call that finishes the job; a caller that prunes through
    /// [`ChatStore::prune`](crate::ChatStore::prune) directly must hand these
    /// to [`StagingStore::remove`] itself, or the bytes survive until the next
    /// [`StagingStore::sweep`].
    pub staged: Vec<String>,
}

impl Pruned {
    /// Fold another conversation's result into this one.
    pub fn absorb(&mut self, other: Pruned) {
        self.records += other.records;
        self.queued += other.queued;
        self.staged.extend(other.staged);
    }

    /// Whether anything at all was removed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records == 0 && self.queued == 0 && self.staged.is_empty()
    }
}

/// Prune one conversation and delete the staged blobs it orphaned — the whole
/// job, in the one call a surface should be reaching for.
///
/// [`ChatStore::prune`] cannot do the second half: it holds an `AppStore` and
/// nothing else, and giving it a storage provider so it could unlink a file
/// would put filesystem knowledge inside the conversation store to serve one
/// call. So the split is deliberate — the store decides *what* is out of its
/// window, this decides *and finishes* it — and this is the entry point named
/// everywhere, so that "prune, then forget the bytes" cannot be half-done by a
/// caller who did not know there was a second half.
///
/// [`ChatStore::prune`]: crate::ChatStore::prune
pub fn prune_conversation(
    store: &ChatStore,
    staging: &StagingStore,
    peer: &DeviceId,
    now: DateTime<Utc>,
) -> Result<Pruned, ChatError> {
    let pruned = store.prune(peer, now)?;
    for path in &pruned.staged {
        staging.remove(path);
    }
    Ok(pruned)
}

/// Prune every conversation on this device, deleting the staged blobs they
/// orphaned.
///
/// What a composition root calls at startup and on whatever tick it already
/// runs: read-time filtering has already made the promise true, and this is
/// what makes it true about disk for a thread nobody has opened.
///
/// A conversation whose window is off costs one policy read and nothing else,
/// so this stays cheap on a machine where the feature is unused — which, being
/// off by default, is every machine until a user asks for it.
pub fn prune_all_conversations(
    store: &ChatStore,
    staging: &StagingStore,
    now: DateTime<Utc>,
) -> Result<Pruned, ChatError> {
    let mut total = Pruned::default();
    for peer in store.prunable_peers()? {
        total.absorb(prune_conversation(store, staging, &peer, now)?);
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-01-01T{hour:02}:{minute:02}:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    /// The default is the only one an upgrade can produce, and it must keep
    /// everything. A `Retention` that defaulted to any window at all would
    /// delete history nobody asked it to.
    #[test]
    fn the_default_window_is_off_and_nothing_ever_disappears_under_it() {
        let r = Retention::default();
        assert_eq!(r, Retention::OFF);
        assert!(r.is_off());
        assert!(r.cutoff(at(12, 0)).is_none());
        // Including the two fail-safes, which must be unreachable while off.
        assert!(!r.closed_on(Some(at(0, 0)), at(23, 0)));
        assert!(
            !r.closed_on(None, at(23, 0)),
            "an undatable row must not disappear from a conversation with no window"
        );
    }

    /// The boundary, asserted exactly rather than by sleeping: a thirty-minute
    /// window on a row stored at 10:00 is over **at** 10:30, not a tick later.
    #[test]
    fn a_window_closes_at_the_deadline_not_after_it() {
        let r = Retention::for_secs(30 * 60).unwrap();
        let stored = at(10, 0);
        assert!(!r.closed_on(Some(stored), at(10, 29)), "inside the window");
        assert!(
            r.closed_on(Some(stored), at(10, 30)),
            "the deadline itself is closed — 'at or after', like trust expiry"
        );
        assert!(r.closed_on(Some(stored), at(10, 31)));
    }

    /// A row this device cannot date, in a conversation the user *has* put a
    /// window on. Keeping it would let a peer that sent an unparseable
    /// timestamp in an older build outlive the window forever.
    #[test]
    fn a_row_whose_age_cannot_be_established_is_treated_as_gone() {
        assert!(Retention::for_secs(60).unwrap().closed_on(None, at(10, 0)));
    }

    #[test]
    fn a_zero_window_is_refused_rather_than_read_as_off_or_as_instant_deletion() {
        assert!(matches!(
            Retention::for_secs(0),
            Err(ChatError::BadRetention(_))
        ));
    }

    #[test]
    fn a_window_longer_than_the_bound_is_refused() {
        assert!(Retention::for_secs(MAX_TTL_SECS).is_ok());
        assert!(matches!(
            Retention::for_secs(MAX_TTL_SECS + 1),
            Err(ChatError::BadRetention(_))
        ));
        assert!(matches!(
            Retention::for_secs(u64::MAX),
            Err(ChatError::BadRetention(_))
        ));
    }

    /// Off must be byte-identical to what a build with no retention field would
    /// have written, so a policy blob is never bigger than the fact it records.
    #[test]
    fn an_off_policy_serializes_to_an_empty_object_and_round_trips() {
        let json = String::from_utf8(Retention::OFF.encode().unwrap()).unwrap();
        assert_eq!(json, "{}");
        assert_eq!(Retention::decode(json.as_bytes()).unwrap(), Retention::OFF);
        // And a blob from a build that never had the field decodes as off.
        assert_eq!(Retention::decode(b"{}").unwrap(), Retention::OFF);
    }

    #[test]
    fn a_window_round_trips_through_the_store_encoding() {
        let r = Retention::for_secs(3600).unwrap();
        let bytes = r.encode().unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("\"ttl_secs\":3600"));
        assert_eq!(Retention::decode(&bytes).unwrap(), r);
    }

    /// An unreadable policy is a store problem, not "no window". Answering a
    /// privacy question with the least private answer available, silently, on
    /// every read, is the failure this refuses.
    #[test]
    fn an_undecodable_policy_is_an_error_rather_than_a_quiet_off() {
        assert!(Retention::decode(b"not json at all").is_err());
    }

    /// The one call a surface should make, and the half `ChatStore::prune`
    /// cannot do: the staged blob a queued file owned is a full plaintext copy
    /// of that file, and leaving it behind is the "disappearing that did not
    /// delete anything" this feature exists to avoid.
    #[test]
    fn prune_conversation_deletes_the_staged_blob_the_store_could_only_report() {
        use crate::record::{ChatRecord, FileMeta, Status};
        use crate::store::StagedFile;
        use crate::{ChatMessage, FileRef};
        use peerbeam_crypto::{derive_subkey, AeadCrypto};
        use peerbeam_domain::port::{AppStore, EncryptionProvider};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[9u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        let store = ChatStore::new(app);
        let staging = StagingStore::new(
            dir.path().join("blobs").to_string_lossy().into_owned(),
            Arc::new(peerbeam_storage_fs::FsStorage::new()),
        );
        let peer = DeviceId::from("pb-bob");

        // A queued file, aged past the window, with a blob really on disk.
        std::fs::create_dir_all(dir.path().join("blobs")).unwrap();
        let blob = dir.path().join("blobs").join("report");
        std::fs::write(&blob, b"the whole file").unwrap();
        let mut r = FileRef::new("report.pdf", 14).unwrap();
        r.timestamp = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        let mut rec = ChatRecord::file_out(
            &peer,
            &r,
            FileMeta::new(&r.name, r.size, None),
            Status::Staging,
        );
        rec.stored_at = Some(Utc::now() - chrono::Duration::hours(2));
        store.append(&rec).unwrap();
        assert!(store
            .enqueue_file(
                &peer,
                &r,
                &StagedFile {
                    name: "report.pdf".into(),
                    size: 14,
                    staged_path: blob.to_string_lossy().into_owned(),
                },
            )
            .unwrap());
        // Something ordinary in another thread, to prove `prune_all` is scoped
        // by each conversation's own window rather than sweeping the machine.
        let other = DeviceId::from("pb-ada");
        store
            .append(&ChatRecord::sent(
                &other,
                &ChatMessage::new("keep me").unwrap(),
            ))
            .unwrap();

        store
            .set_retention(&peer, Retention::for_secs(3600).unwrap())
            .unwrap();
        let pruned = prune_all_conversations(&store, &staging, Utc::now()).unwrap();

        assert_eq!((pruned.records, pruned.queued), (1, 1));
        assert!(
            !blob.exists(),
            "the plaintext copy of the queued file must actually be unlinked"
        );
        assert!(store.history(&peer).unwrap().is_empty());
        assert_eq!(
            store.history(&other).unwrap().len(),
            1,
            "a conversation with no window is untouched"
        );
    }
}
