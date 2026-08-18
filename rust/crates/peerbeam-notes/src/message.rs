//! The wire message carried on the Notes channel.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use peerbeam_domain::session::{ChannelId, MessageFlags, MessageType, SessionFrame};

use crate::note::{Note, NoteError};

/// MessageType id for a batch of notes within the Notes channel namespace.
pub const MSG_NOTE_BATCH: u16 = 1;

/// Maximum encoded size of one batch frame, in bytes.
///
/// A device's whole note set can exceed any single frame, so a sync is sent as
/// a sequence of batches rather than one message. 256 KiB is large enough that
/// an ordinary set is one frame and small enough that a peer never has to
/// buffer something surprising on our say-so.
pub const MAX_BATCH_BYTES: usize = 256 * 1024;

/// Maximum notes in one batch, independent of size.
///
/// A second bound because the byte bound alone would let one frame carry tens
/// of thousands of tiny notes, and every one of them costs a store write on the
/// receiver.
pub const MAX_BATCH_NOTES: usize = 256;

/// A batch of notes offered to a peer.
///
/// **Tombstones are included.** A deletion is a fact about the note set exactly
/// as an edit is, and a batch that carried only live notes could never tell a
/// peer that something was deleted — it would simply look like an older set,
/// and the peer would offer the note back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NoteBatch {
    pub notes: Vec<Note>,
    /// Whether more batches follow in this exchange.
    ///
    /// The receiver answers with its own set only after the last one, so a sync
    /// is exactly two passes and cannot ping-pong: a batch sent *as* a reply
    /// carries [`reply`](Self::reply), and a reply is never answered.
    #[serde(default)]
    pub more: bool,
    /// Whether this batch is the answer to someone else's. Never answered.
    #[serde(default)]
    pub reply: bool,
}

impl NoteBatch {
    #[must_use]
    pub fn new(notes: Vec<Note>, more: bool, reply: bool) -> NoteBatch {
        NoteBatch { notes, more, reply }
    }

    /// The Notes MessageType (`NoteBatch` = 1).
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_NOTE_BATCH)
    }

    /// Split `notes` into batches that each satisfy both bounds.
    ///
    /// Returns at least one batch — an empty one when there is nothing to send,
    /// because "I have no notes" is itself an answer a peer needs: without it
    /// the exchange would hang waiting for a set that is never coming.
    pub fn split(notes: Vec<Note>, reply: bool) -> Vec<NoteBatch> {
        let mut out: Vec<NoteBatch> = Vec::new();
        let mut current: Vec<Note> = Vec::new();
        let mut bytes = 0usize;
        for n in notes {
            // Approximate the encoded cost rather than serialising twice. The
            // slack is deliberate: overshooting the frame limit is a protocol
            // error, undershooting is a second frame.
            let cost = n.id.len() + n.title.len() + n.body.len() + n.updated_at.len() + 64;
            if !current.is_empty()
                && (current.len() >= MAX_BATCH_NOTES || bytes + cost > MAX_BATCH_BYTES)
            {
                out.push(NoteBatch::new(std::mem::take(&mut current), true, reply));
                bytes = 0;
            }
            bytes += cost;
            current.push(n);
        }
        out.push(NoteBatch::new(current, false, reply));
        out
    }

    /// Encode as a Notes-channel frame.
    ///
    /// OPTIONAL, like every message added after the session's first release: a
    /// peer that does not implement notes skips it rather than failing the
    /// channel.
    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, NoteError> {
        let payload = serde_json::to_vec(self)
            .map(Bytes::from)
            .map_err(|e| NoteError::Serialization(e.to_string()))?;
        if payload.len() > MAX_BATCH_BYTES {
            return Err(NoteError::Serialization(format!(
                "batch of {} bytes exceeds {MAX_BATCH_BYTES}",
                payload.len()
            )));
        }
        Ok(SessionFrame::new(
            channel,
            Self::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            payload,
        ))
    }

    /// Decode from a Notes-channel frame, enforcing both bounds on the way in.
    ///
    /// A peer's claim about how much it is sending is not trusted: the bounds
    /// are re-checked here, because every note in a batch costs the receiver a
    /// store write.
    pub fn from_frame(frame: &SessionFrame) -> Result<NoteBatch, NoteError> {
        if frame.message_type.get() != MSG_NOTE_BATCH {
            return Err(NoteError::Serialization(format!(
                "unexpected notes message type {}",
                frame.message_type.get()
            )));
        }
        if frame.payload.len() > MAX_BATCH_BYTES {
            return Err(NoteError::Serialization(format!(
                "batch of {} bytes exceeds {MAX_BATCH_BYTES}",
                frame.payload.len()
            )));
        }
        let batch: NoteBatch = serde_json::from_slice(&frame.payload)
            .map_err(|e| NoteError::Serialization(e.to_string()))?;
        if batch.notes.len() > MAX_BATCH_NOTES {
            return Err(NoteError::Serialization(format!(
                "batch of {} notes exceeds {MAX_BATCH_NOTES}",
                batch.notes.len()
            )));
        }
        for n in &batch.notes {
            if n.id.is_empty() || n.id.len() > crate::note::MAX_ID {
                return Err(NoteError::BadId(format!("length {}", n.id.len())));
            }
            if n.title.len() > crate::note::MAX_TITLE {
                return Err(NoteError::TitleTooLarge { len: n.title.len() });
            }
            if n.body.len() > crate::note::MAX_BODY {
                return Err(NoteError::BodyTooLarge { len: n.body.len() });
            }
        }
        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::session::ChannelId;

    fn note(id: &str, body: &str, deleted: bool) -> Note {
        Note {
            id: id.to_string(),
            title: String::new(),
            body: body.to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            deleted,
        }
    }

    #[test]
    fn a_batch_round_trips_and_ships_optional() {
        let b = NoteBatch::new(vec![note("n1", "hello", false)], false, false);
        let frame = b.to_frame(ChannelId::new(1)).unwrap();
        assert_eq!(frame.message_type.get(), MSG_NOTE_BATCH);
        assert!(
            frame.flags.contains(MessageFlags::OPTIONAL),
            "a peer without notes must skip, not fail the channel"
        );
        assert_eq!(NoteBatch::from_frame(&frame).unwrap(), b);
    }

    #[test]
    fn a_batch_carries_tombstones() {
        // Without them a peer could never learn that something was deleted: the
        // set would just look older, and the peer would offer the note back.
        let b = NoteBatch::new(vec![note("n1", "", true)], false, false);
        let back = NoteBatch::from_frame(&b.to_frame(ChannelId::new(1)).unwrap()).unwrap();
        assert!(back.notes[0].deleted);
    }

    #[test]
    fn splitting_keeps_tombstones_because_that_is_how_a_deletion_travels() {
        // The path a sender actually takes. Asserting this on `NoteBatch::new`
        // proves nothing: nothing filters there. `split` is where a well-meaning
        // "only send live notes" would go, and it would mean deletions never
        // propagate while every test still passed.
        let batches = NoteBatch::split(
            vec![note("live", "here", false), note("gone", "", true)],
            false,
        );
        let all: Vec<&Note> = batches.iter().flat_map(|b| b.notes.iter()).collect();
        assert_eq!(all.len(), 2, "a tombstone was dropped on the way out");
        assert!(all.iter().any(|n| n.deleted && n.id == "gone"));
    }

    #[test]
    fn splitting_respects_the_count_bound_and_flags_all_but_the_last() {
        let notes: Vec<Note> = (0..MAX_BATCH_NOTES + 5)
            .map(|i| note(&format!("n{i}"), "x", false))
            .collect();
        let batches = NoteBatch::split(notes, false);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].notes.len(), MAX_BATCH_NOTES);
        assert!(batches[0].more, "an earlier batch must say more follows");
        assert!(!batches[1].more, "the last batch must not");
    }

    #[test]
    fn splitting_nothing_still_produces_one_empty_batch() {
        // "I have no notes" is an answer the peer needs. Sending nothing would
        // leave it waiting for a set that never comes.
        let batches = NoteBatch::split(Vec::new(), true);
        assert_eq!(batches.len(), 1);
        assert!(batches[0].notes.is_empty());
        assert!(!batches[0].more);
        assert!(batches[0].reply);
    }

    #[test]
    fn an_oversized_batch_is_refused_on_the_way_in() {
        // A peer's claim about how much it is sending is not trusted: every
        // note in a batch costs the receiver a store write.
        let notes: Vec<Note> = (0..MAX_BATCH_NOTES + 1)
            .map(|i| note(&format!("n{i}"), "x", false))
            .collect();
        let payload = serde_json::to_vec(&NoteBatch::new(notes, false, false)).unwrap();
        let frame = SessionFrame::new(
            ChannelId::new(1),
            NoteBatch::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from(payload),
        );
        assert!(NoteBatch::from_frame(&frame).is_err());
    }

    #[test]
    fn a_note_that_breaks_a_bound_is_refused_on_the_way_in() {
        let mut n = note("n1", "x", false);
        n.body = "x".repeat(crate::note::MAX_BODY + 1);
        let payload = serde_json::to_vec(&NoteBatch::new(vec![n], false, false)).unwrap();
        let frame = SessionFrame::new(
            ChannelId::new(1),
            NoteBatch::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from(payload),
        );
        assert!(matches!(
            NoteBatch::from_frame(&frame),
            Err(NoteError::BodyTooLarge { .. })
        ));
    }

    #[test]
    fn another_message_type_does_not_decode_as_a_batch() {
        let frame = SessionFrame::new(
            ChannelId::new(1),
            MessageType::new(99),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(b"{}"),
        );
        assert!(NoteBatch::from_frame(&frame).is_err());
    }
}
