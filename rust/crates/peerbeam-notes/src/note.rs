//! A note, and the bounds that keep one honest.

use chrono::Utc;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// Maximum note body size, in UTF-8 bytes.
///
/// Generous next to any note a person types, and small enough that a peer
/// cannot use the notes channel to move a file. A note is text; anything larger
/// is a transfer, and PeerBeam already has one of those.
pub const MAX_BODY: usize = 65536;
/// Maximum note title size, in UTF-8 bytes.
pub const MAX_TITLE: usize = 256;
/// Maximum note id size, in bytes. Same bound as a chat message id, and for the
/// same reason: an id is echoed into events, storage keys and log lines.
pub const MAX_ID: usize = 128;

/// Errors from validating or storing a note.
#[derive(Debug, thiserror::Error)]
pub enum NoteError {
    #[error("note body too large: {len} bytes (max {MAX_BODY})")]
    BodyTooLarge { len: usize },
    #[error("note title too large: {len} bytes (max {MAX_TITLE})")]
    TitleTooLarge { len: usize },
    #[error("bad note id: {0}")]
    BadId(String),
    #[error("note storage: {0}")]
    Storage(String),
    #[error("note serialization: {0}")]
    Serialization(String),
}

/// One note.
///
/// A note carries its own **tombstone** rather than being removed from storage,
/// because notes are meant to sync: a row that simply vanished would be
/// indistinguishable from one a peer had not seen yet, and the next exchange
/// would resurrect it. `deleted` is the difference between "gone" and "never
/// arrived".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Note {
    pub id: String,
    /// Optional heading. Empty is normal — plenty of notes are a line of text.
    #[serde(default)]
    pub title: String,
    /// The note itself. Empty on a tombstone: a deleted note keeps its id and
    /// its time, and nothing else. Retaining the text of something the user
    /// deleted, purely to help a sync algorithm, would be the wrong trade.
    #[serde(default)]
    pub body: String,
    /// RFC 3339 time of the last edit. **The conflict resolver**: when two
    /// devices edited the same note, the later `updated_at` wins.
    pub updated_at: String,
    /// Whether this note has been deleted.
    #[serde(default)]
    pub deleted: bool,
}

impl Note {
    /// A new note with a freshly minted id.
    pub fn new(title: &str, body: &str) -> Result<Note, NoteError> {
        Self::at(mint_id(), title, body)
    }

    /// A note with a caller-supplied id, validated. Used when an edit or a peer
    /// names an existing note.
    pub fn at(id: String, title: &str, body: &str) -> Result<Note, NoteError> {
        if id.is_empty() || id.len() > MAX_ID {
            return Err(NoteError::BadId(format!("length {}", id.len())));
        }
        if title.len() > MAX_TITLE {
            return Err(NoteError::TitleTooLarge { len: title.len() });
        }
        if body.len() > MAX_BODY {
            return Err(NoteError::BodyTooLarge { len: body.len() });
        }
        Ok(Note {
            id,
            title: title.to_string(),
            body: body.to_string(),
            updated_at: Utc::now().to_rfc3339(),
            deleted: false,
        })
    }

    /// The tombstone for this note: same id, no content, stamped now.
    #[must_use]
    pub fn tombstone(&self) -> Note {
        Note {
            id: self.id.clone(),
            title: String::new(),
            body: String::new(),
            updated_at: Utc::now().to_rfc3339(),
            deleted: true,
        }
    }

    /// Which of two versions of the same note wins.
    ///
    /// **Last writer wins, by `updated_at`, with deletion breaking a tie.** Two
    /// devices that edited while apart cannot both be right, and there is no
    /// third party to ask; the alternative — surfacing every divergence as a
    /// conflict — turns a notepad into a merge tool. A tie broken toward
    /// deletion means a note the user deleted on one device does not come back
    /// because the clocks agreed to the second.
    ///
    /// Comparison is on the RFC 3339 strings, which sort chronologically when
    /// both carry the same offset; both sides are minted by [`Utc::now`], so
    /// they do.
    #[must_use]
    pub fn wins<'a>(a: &'a Note, b: &'a Note) -> &'a Note {
        match a.updated_at.cmp(&b.updated_at) {
            std::cmp::Ordering::Greater => a,
            std::cmp::Ordering::Less => b,
            std::cmp::Ordering::Equal if a.deleted => a,
            std::cmp::Ordering::Equal => b,
        }
    }
}

/// A lexicographically time-ordered id: 13-digit unix-millis + 16 hex.
///
/// The same shape chat ids use, so a note id sorts by creation time and reads
/// the same way in a log.
#[must_use]
pub fn mint_id() -> String {
    let millis = Utc::now().timestamp_millis().max(0) as u64;
    let mut suffix = [0u8; 8];
    OsRng.fill_bytes(&mut suffix);
    format!("{millis:013}{}", hex(&suffix))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
