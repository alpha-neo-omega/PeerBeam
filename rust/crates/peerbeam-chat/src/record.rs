//! The persisted chat record (distinct from the wire `ChatMessage`).

use serde::{Deserialize, Serialize};

use peerbeam_domain::id::DeviceId;

use crate::message::{ChatError, ChatMessage, FileRef};

/// Whether a record was sent by us or received from the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Out,
    In,
}

/// Delivery status. In 1a only `Sent`/`Received` occur; `Pending` is reserved
/// for the offline outbox (increment 1b).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Sent,
    Received,
    /// A file offered to us, awaiting the user's accept/decline.
    PendingApproval,
    /// A file whose bytes are moving.
    Transferring,
    /// The peer declined the file.
    Declined,
    /// The transfer failed.
    Failed,
    /// Left mid-flight by a crash/restart; no event will ever complete it.
    Interrupted,
}

/// What a record holds: a text body, or a reference to a shared file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A text/markdown message (every record written before file-in-chat).
    #[default]
    Text,
    /// A shared file; see `ChatRecord::file`.
    File,
}

/// Record-side file metadata. NEVER serialized to a frame — `local_path` is the
/// owner's private filesystem layout (the wire type is `FileRef`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    pub name: String,
    pub size: u64,
    /// Where the file lives on THIS device: the source path on the sender, the
    /// saved path on the receiver. `None` until a receive completes.
    #[serde(default)]
    pub local_path: Option<String>,
}

/// A chat message persisted in one conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRecord {
    pub id: String,
    pub peer_id: String,
    pub direction: Direction,
    pub timestamp: String,
    pub body: String,
    pub status: Status,
    /// Text (default, so legacy records decode) or File.
    #[serde(default)]
    pub kind: Kind,
    /// Present only when `kind == Kind::File`.
    #[serde(default)]
    pub file: Option<FileMeta>,
}

impl ChatRecord {
    /// A record for a message we sent to `peer` (status `Sent`).
    #[must_use]
    pub fn sent(peer: &DeviceId, msg: &ChatMessage) -> ChatRecord {
        ChatRecord {
            id: msg.id.clone(),
            peer_id: peer.0.clone(),
            direction: Direction::Out,
            timestamp: msg.timestamp.clone(),
            body: msg.body.clone(),
            status: Status::Sent,
            kind: Kind::Text,
            file: None,
        }
    }

    /// A record for a message received from `peer` (status `Received`).
    #[must_use]
    pub fn received(peer: &DeviceId, msg: &ChatMessage) -> ChatRecord {
        ChatRecord {
            id: msg.id.clone(),
            peer_id: peer.0.clone(),
            direction: Direction::In,
            timestamp: msg.timestamp.clone(),
            body: msg.body.clone(),
            status: Status::Received,
            kind: Kind::Text,
            file: None,
        }
    }

    /// An outgoing record with an explicit status (used for the offline outbox:
    /// `Pending` on enqueue, `Sent` once flushed).
    #[must_use]
    pub fn out(peer: &DeviceId, msg: &ChatMessage, status: Status) -> ChatRecord {
        ChatRecord {
            id: msg.id.clone(),
            peer_id: peer.0.clone(),
            direction: Direction::Out,
            timestamp: msg.timestamp.clone(),
            body: msg.body.clone(),
            status,
            kind: Kind::Text,
            file: None,
        }
    }

    /// A record for a file we are sending to `peer`.
    #[must_use]
    pub fn file_out(peer: &DeviceId, r: &FileRef, meta: FileMeta, status: Status) -> ChatRecord {
        ChatRecord {
            id: r.id.clone(),
            peer_id: peer.0.clone(),
            direction: Direction::Out,
            timestamp: r.timestamp.clone(),
            body: String::new(),
            status,
            kind: Kind::File,
            file: Some(meta),
        }
    }

    /// A record for a file `peer` is offering us (awaiting approval).
    #[must_use]
    pub fn file_in(peer: &DeviceId, r: &FileRef) -> ChatRecord {
        ChatRecord {
            id: r.id.clone(),
            peer_id: peer.0.clone(),
            direction: Direction::In,
            timestamp: r.timestamp.clone(),
            body: String::new(),
            status: Status::PendingApproval,
            kind: Kind::File,
            file: Some(FileMeta {
                name: r.name.clone(),
                size: r.size,
                local_path: None,
            }),
        }
    }

    /// Serialize to opaque bytes for the AppStore.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        // Infallible in practice (plain struct of owned strings); fall back to an
        // empty vec rather than panicking, and let the caller's put persist it.
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from AppStore bytes.
    pub fn decode(bytes: &[u8]) -> Result<ChatRecord, ChatError> {
        serde_json::from_slice(bytes).map_err(|e| ChatError::Serialization(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChatMessage;
    use peerbeam_domain::id::DeviceId;

    #[test]
    fn record_encode_decode_roundtrip() {
        let peer = DeviceId::from("pb-bob");
        let m = ChatMessage::new("hi").unwrap();
        let rec = ChatRecord::sent(&peer, &m);
        let back = ChatRecord::decode(&rec.encode()).unwrap();
        assert_eq!(back.id, m.id);
        assert_eq!(back.peer_id, "pb-bob");
        assert_eq!(back.direction, Direction::Out);
        assert_eq!(back.status, Status::Sent);
        assert_eq!(back.body, "hi");
    }

    #[test]
    fn received_sets_in_and_received() {
        let rec = ChatRecord::received(&DeviceId::from("pb-a"), &ChatMessage::new("x").unwrap());
        assert_eq!(rec.direction, Direction::In);
        assert_eq!(rec.status, Status::Received);
    }

    /// A record persisted by 1a/1b (no `kind`, no `file`) must still decode.
    #[test]
    fn legacy_record_json_decodes_as_text() {
        let legacy = br#"{"id":"1","peer_id":"pb-a","direction":"out",
            "timestamp":"t","body":"hello","status":"sent"}"#;
        let rec = ChatRecord::decode(legacy).unwrap();
        assert_eq!(rec.kind, Kind::Text);
        assert!(rec.file.is_none());
        assert_eq!(rec.body, "hello");
    }

    #[test]
    fn file_record_carries_its_meta_and_roundtrips() {
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("a.bin", 7).unwrap();
        let meta = FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: Some("/tmp/a.bin".into()),
        };
        let rec = ChatRecord::file_out(&peer, &r, meta.clone(), Status::Transferring);
        assert_eq!(rec.kind, Kind::File);
        assert_eq!(rec.direction, Direction::Out);
        assert_eq!(rec.id, r.id);
        let back = ChatRecord::decode(&rec.encode()).unwrap();
        assert_eq!(back, rec);
        assert_eq!(back.file.unwrap().local_path.as_deref(), Some("/tmp/a.bin"));
    }
}
