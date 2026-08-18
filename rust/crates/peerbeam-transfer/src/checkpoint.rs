//! Checkpoint policy: what a checkpoint *means* once the run that wrote it is
//! gone.
//!
//! [`recover`](crate::recover) writes checkpoints and clears them; this module
//! answers the three questions a surface has to ask about one it found on disk
//! after a restart, and answers them in one place so the FFI and the CLI
//! cannot answer them differently:
//!
//! 1. **May this resume at all?** — [`check_resume`]. A checkpoint is a claim
//!    about a *specific* transfer of a *specific* file with a *specific* peer.
//!    Resuming appends to a partial file, so a checkpoint matched against the
//!    wrong file would corrupt it silently and the end-of-transfer checksum
//!    would be the only thing left to notice. Every field the identity rests
//!    on is compared before a byte moves.
//! 2. **Was it consented to?** — the same function, via
//!    [`TransferSession::accepted`]. An inbound transfer the user accepted
//!    resumes without a second prompt; one that was never accepted must not
//!    become resumable into an accepted one. A crash is not an approval (I6).
//! 3. **When is it garbage?** — [`is_expired`] and [`partial_file`]. A
//!    checkpoint nobody will ever resume, and the partial bytes it holds open,
//!    must eventually be reclaimed rather than accumulate forever.
//!
//! Nothing here does IO or decides *how* to resume: it is pure policy over an
//! already-loaded [`TransferSession`], which is what makes every rule below a
//! unit test rather than an end-to-end one.

use chrono::{DateTime, Duration as ChronoDuration, Utc};

use peerbeam_domain::entity::{Direction, TransferSession};
use peerbeam_domain::id::DeviceId;

use crate::stream::part_path;

/// How long a checkpoint survives without being resumed before it is
/// reclaimed, along with any partial file it holds.
///
/// A checkpoint is not immortal. It pins disk — its own record, and far more
/// importantly the `.part` file whose bytes are the entire point of keeping it
/// — and a transfer nobody resumed in two weeks is one nobody is going to.
/// Fourteen days is long enough to cover a laptop left shut over a holiday,
/// which is the case that makes an aggressive expiry infuriating, and short
/// enough that an abandoned 40 GB transfer does not sit on the disk for a
/// year.
///
/// Age is measured from [`TransferSession::started_at`], which a resume
/// refreshes by starting a new session — so a transfer that is being retried
/// repeatedly never expires out from under the user.
pub const CHECKPOINT_MAX_AGE_DAYS: i64 = 14;

/// What a resume must agree with before a single byte is appended.
///
/// Deliberately built from what the *current* attempt says — the authenticated
/// peer, the name on the wire, the size on the wire — never from the
/// checkpoint itself, so the comparison below is a check rather than a
/// tautology.
#[derive(Debug, Clone)]
pub struct ResumeClaim<'a> {
    /// The peer this attempt is with, as authenticated — never a name a peer
    /// presented for itself.
    pub peer: &'a DeviceId,
    /// The file name this attempt carries, already sanitized to the single
    /// path component that will actually be written.
    pub name: &'a str,
    /// The total size this attempt declares.
    pub total_bytes: u64,
    /// The direction this attempt runs in.
    pub direction: Direction,
}

/// Why a resume was refused. Each variant is a distinct thing to tell a user,
/// and — more to the point — a distinct thing to assert in a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeRefusal {
    /// The checkpoint records the other direction. A send may not be resumed
    /// as a receive, or the reverse.
    Direction,
    /// A different peer. The checkpoint's partial bytes belong to a transfer
    /// with someone else.
    Peer,
    /// A different file name, so a different destination and a different
    /// `.part`.
    Name,
    /// The same name but a different total size — the file changed, or this is
    /// simply not the same file.
    Size,
    /// The checkpoint has no file recorded at all, so there is nothing to
    /// bind to. Refused rather than resumed on trust.
    Unbound,
    /// The user never accepted this inbound transfer. It may not be resumed,
    /// and it may not be auto-accepted: an interruption must not launder an
    /// unanswered prompt into consent (I6).
    NotAccepted,
}

impl ResumeRefusal {
    /// A short, user-facing reason.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            ResumeRefusal::Direction => "the checkpoint is for a transfer in the other direction",
            ResumeRefusal::Peer => "the checkpoint belongs to a transfer with a different device",
            ResumeRefusal::Name => "the checkpoint is for a different file",
            ResumeRefusal::Size => "the file's size has changed since the transfer was interrupted",
            ResumeRefusal::Unbound => "the checkpoint records no file to resume",
            ResumeRefusal::NotAccepted => "this transfer was never accepted, so it cannot resume",
        }
    }
}

/// Whether `checkpoint` may be resumed by the attempt `claim` describes.
///
/// **The fields checked are the transfer's identity:** direction, peer id,
/// file name, total size — and the consent flag. Every one of them is a way
/// two different transfers could otherwise be confused for each other, and the
/// cost of confusing them is appending one file's bytes to another's.
///
/// The size is compared against [`TransferSession::total_bytes`] *and* the
/// recorded file's own size, which the writer keeps equal; a checkpoint whose
/// two sizes disagree is refused rather than resolved in favour of either.
///
/// Note what is deliberately **not** compared: `transferred_bytes`. The real
/// resume offset is negotiated by the protocol from the bytes actually on
/// disk, never from this record — a checkpoint that has drifted behind the
/// partial file costs nothing, and trusting it over the disk would be how a
/// resume skips bytes it never sent.
pub fn check_resume(
    checkpoint: &TransferSession,
    claim: &ResumeClaim<'_>,
) -> Result<(), ResumeRefusal> {
    if checkpoint.direction != claim.direction {
        return Err(ResumeRefusal::Direction);
    }
    // Consent before identity: an inbound transfer nobody accepted must be
    // refused whether or not it also happens to match.
    if !checkpoint.accepted {
        return Err(ResumeRefusal::NotAccepted);
    }
    if checkpoint.peer != *claim.peer {
        return Err(ResumeRefusal::Peer);
    }
    let Some(file) = checkpoint.files.first() else {
        return Err(ResumeRefusal::Unbound);
    };
    if file.name != claim.name {
        return Err(ResumeRefusal::Name);
    }
    if file.size != claim.total_bytes || checkpoint.total_bytes != claim.total_bytes {
        return Err(ResumeRefusal::Size);
    }
    Ok(())
}

/// The partial file a checkpoint is holding open, if it has one.
///
/// Only a receiving checkpoint has one: a send reads its source and writes
/// nothing, so discarding it reclaims a record and never touches the user's
/// file. A receive writes to `<destination>.part`, which is the file the
/// resume offset is actually measured from — and the file that must go when
/// the checkpoint does, or a discarded transfer would silently seed the next
/// attempt at the same name with bytes from the one before it.
#[must_use]
pub fn partial_file(checkpoint: &TransferSession) -> Option<String> {
    if checkpoint.direction != Direction::Receiving {
        return None;
    }
    let file = checkpoint.files.first()?;
    let dest = file.path.to_str()?;
    if dest.is_empty() {
        return None;
    }
    Some(part_path(dest))
}

/// Whether `checkpoint` is old enough to reclaim, as of `now`.
#[must_use]
pub fn is_expired(checkpoint: &TransferSession, now: DateTime<Utc>) -> bool {
    is_expired_after(checkpoint, now, CHECKPOINT_MAX_AGE_DAYS)
}

/// [`is_expired`] with an explicit age, so a test does not have to fabricate a
/// two-week-old clock.
#[must_use]
pub fn is_expired_after(checkpoint: &TransferSession, now: DateTime<Utc>, days: i64) -> bool {
    now.signed_duration_since(checkpoint.started_at) > ChronoDuration::days(days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::entity::{FileEntry, TransferStatus};
    use peerbeam_domain::id::TransferId;
    use std::path::PathBuf;

    fn checkpoint(direction: Direction, accepted: bool) -> TransferSession {
        TransferSession {
            id: TransferId::from("t1"),
            peer: DeviceId::from("peer-a"),
            direction,
            status: TransferStatus::Transferring,
            files: vec![FileEntry {
                path: PathBuf::from("/save/movie.mkv"),
                name: "movie.mkv".into(),
                size: 1000,
                mime_type: String::new(),
                checksum: None,
            }],
            total_bytes: 1000,
            transferred_bytes: 400,
            started_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            completed_at: None,
            is_resume: false,
            accepted,
        }
    }

    fn claim<'a>(peer: &'a DeviceId, name: &'a str, size: u64) -> ResumeClaim<'a> {
        ResumeClaim {
            peer,
            name,
            total_bytes: size,
            direction: Direction::Receiving,
        }
    }

    #[test]
    fn a_matching_claim_resumes() {
        let peer = DeviceId::from("peer-a");
        assert_eq!(
            check_resume(
                &checkpoint(Direction::Receiving, true),
                &claim(&peer, "movie.mkv", 1000)
            ),
            Ok(())
        );
    }

    #[test]
    fn a_different_peer_is_refused() {
        let other = DeviceId::from("peer-b");
        assert_eq!(
            check_resume(
                &checkpoint(Direction::Receiving, true),
                &claim(&other, "movie.mkv", 1000)
            ),
            Err(ResumeRefusal::Peer)
        );
    }

    #[test]
    fn a_different_file_name_is_refused() {
        let peer = DeviceId::from("peer-a");
        assert_eq!(
            check_resume(
                &checkpoint(Direction::Receiving, true),
                &claim(&peer, "other.mkv", 1000)
            ),
            Err(ResumeRefusal::Name)
        );
    }

    #[test]
    fn a_different_total_size_is_refused() {
        let peer = DeviceId::from("peer-a");
        assert_eq!(
            check_resume(
                &checkpoint(Direction::Receiving, true),
                &claim(&peer, "movie.mkv", 999)
            ),
            Err(ResumeRefusal::Size)
        );
    }

    #[test]
    fn a_checkpoint_whose_two_sizes_disagree_is_refused() {
        // `total_bytes` and the file entry's own size are both written by us
        // and must agree; a record where they do not is not resolved in
        // favour of either, it is refused.
        let peer = DeviceId::from("peer-a");
        let mut cp = checkpoint(Direction::Receiving, true);
        cp.total_bytes = 2000;
        assert_eq!(
            check_resume(&cp, &claim(&peer, "movie.mkv", 1000)),
            Err(ResumeRefusal::Size)
        );
    }

    #[test]
    fn the_other_direction_is_refused() {
        let peer = DeviceId::from("peer-a");
        assert_eq!(
            check_resume(
                &checkpoint(Direction::Sending, true),
                &claim(&peer, "movie.mkv", 1000)
            ),
            Err(ResumeRefusal::Direction)
        );
    }

    #[test]
    fn a_checkpoint_with_no_file_has_nothing_to_bind_to() {
        let peer = DeviceId::from("peer-a");
        let mut cp = checkpoint(Direction::Receiving, true);
        cp.files.clear();
        assert_eq!(
            check_resume(&cp, &claim(&peer, "movie.mkv", 1000)),
            Err(ResumeRefusal::Unbound)
        );
    }

    #[test]
    fn a_never_accepted_inbound_checkpoint_is_refused_even_when_everything_matches() {
        // The consent gate, standing alone: identical peer, name and size —
        // the only difference is that nobody ever said yes.
        let peer = DeviceId::from("peer-a");
        assert_eq!(
            check_resume(
                &checkpoint(Direction::Receiving, false),
                &claim(&peer, "movie.mkv", 1000)
            ),
            Err(ResumeRefusal::NotAccepted)
        );
    }

    #[test]
    fn only_a_receive_holds_a_partial_file() {
        assert_eq!(
            partial_file(&checkpoint(Direction::Receiving, true)).as_deref(),
            Some("/save/movie.mkv.part")
        );
        assert_eq!(partial_file(&checkpoint(Direction::Sending, true)), None);
    }

    #[test]
    fn a_receive_with_no_recorded_destination_holds_no_partial_file() {
        let mut cp = checkpoint(Direction::Receiving, true);
        cp.files[0].path = PathBuf::new();
        assert_eq!(partial_file(&cp), None);
    }

    #[test]
    fn a_checkpoint_expires_only_once_it_is_past_the_age() {
        let cp = checkpoint(Direction::Receiving, true);
        let start = cp.started_at;
        assert!(!is_expired(&cp, start));
        assert!(!is_expired(
            &cp,
            start + ChronoDuration::days(CHECKPOINT_MAX_AGE_DAYS)
        ));
        assert!(is_expired(
            &cp,
            start + ChronoDuration::days(CHECKPOINT_MAX_AGE_DAYS) + ChronoDuration::seconds(1)
        ));
    }
}
