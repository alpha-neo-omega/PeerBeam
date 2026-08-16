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
    /// The file is being copied into the outbox's own storage. Nothing has
    /// been queued or offered yet, so nothing can settle it.
    Staging,
}

/// What a record holds — or, in the outbox, what a queued entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A text/markdown message (every record written before file-in-chat).
    #[default]
    Text,
    /// A shared file; see `ChatRecord::file`.
    File,
    /// **Outbox only:** "tell this peer I turned their file down", queued
    /// because the sender dropped before the live `FileDecline` could reach it.
    ///
    /// Never the kind of a persisted [`ChatRecord`]: a decline is a *status
    /// change* on a row that already exists (ours reads `Declined` the moment
    /// the user refuses, whether or not the signal ever lands), not a row of its
    /// own. Nothing in this crate constructs a record with it, and
    /// [`ChatRecord::is_settleable_file_row`] requires `File`, so a record that
    /// somehow carried it would be inert rather than dangerous.
    Decline,
}

/// Whether `c` must never reach a *rendered* file name.
///
/// A chat file row prints its name directly above an Accept button, so the
/// glyphs the user reads are the entire basis of that decision. Two families of
/// character break the correspondence between what is read and what is there:
///
/// * **C0/C1 controls** (`U+0000`–`U+001F`, `U+007F`–`U+009F`, i.e. Unicode
///   category `Cc`) — a newline lets a name paint extra lines into the bubble,
///   and a terminal surface (`peerbeam chat history`) would act on an escape
///   sequence outright;
/// * **bidi overrides and isolates** (`U+200E`, `U+200F`, `U+202A`–`U+202E`,
///   `U+2066`–`U+2069`) — the classic homograph: `photo\u{202E}gnp.exe` renders
///   as `photoexe.png`, an executable that reads as an image.
fn is_display_hostile(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// The render-safe form of a peer- or user-supplied file name: every
/// [display-hostile](is_display_hostile) character replaced by `U+FFFD`.
///
/// This is a **display** policy and nothing more. What lands on disk is decided
/// solely by `peerbeam_transfer`'s `sanitize_file_name`, which remains the one
/// authority for that and is untouched by this; and this is deliberately *not*
/// a second, stricter `validate_name` — refusing the frame would give chat a
/// name policy plain transfers do not have, and a file a transfer would accept
/// would become unreceivable merely because it was offered in a conversation.
///
/// Substitution rather than deletion, because deleting is the attack: dropping
/// the override from `photo\u{202E}gnp.exe` yields `photognp.exe`, which hides
/// that anything was ever there. `U+FFFD` leaves visible evidence.
///
/// Returns the input unchanged — allocation aside — for every name that holds
/// none of those characters, which is every ordinary file.
#[must_use]
pub fn display_name(raw: &str) -> String {
    raw.chars()
        .map(|c| if is_display_hostile(c) { '\u{FFFD}' } else { c })
        .collect()
}

/// Record-side file metadata. NEVER serialized to a frame — `local_path` is the
/// owner's private filesystem layout (the wire type is `FileRef`).
///
/// Build it with [`FileMeta::new`] rather than a struct literal: `name` is what
/// every surface renders next to an approval control, so it must go through
/// [`display_name`] first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    pub name: String,
    pub size: u64,
    /// Where the file lives on THIS device: the source path on the sender, the
    /// saved path on the receiver. `None` until a receive completes.
    #[serde(default)]
    pub local_path: Option<String>,
}

impl FileMeta {
    /// Record-side metadata for a shared file, with `name` reduced to its
    /// render-safe form ([`display_name`]).
    #[must_use]
    pub fn new(name: &str, size: u64, local_path: Option<String>) -> FileMeta {
        FileMeta {
            name: display_name(name),
            size,
            local_path,
        }
    }
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
            // The peer's own claim about the file, rendered safely. It is only
            // a *claim*: the bytes ride a separate TRANSFER stream whose
            // `TransferMeta` decides what is actually written, and the two are
            // correlated by id alone. `ChatStore::set_file_row_landing`
            // reconciles this row against that stream.
            file: Some(FileMeta::new(&r.name, r.size, None)),
        }
    }

    /// Whether this record is genuinely an in-flight file-share row for
    /// `expected` direction — the authorization check every surface that
    /// bridges a transfer's terminal outcome onto a chat row (the FFI's
    /// `Manager::chat_settle`, the CLI's receive path) must pass before
    /// writing into the chat store. See [`ChatStore::settle_file_row`] and
    /// [`ChatStore::set_file_row_path`], which are gated on this.
    ///
    /// A transfer id is not proof the row belongs to that transfer: on
    /// receive it is the **peer's own** first-frame field, checked by
    /// `is_valid_transfer_id` for shape only, not ownership; a chat message
    /// id is a wire field the peer has already seen. So a bare key match at
    /// that id is a write primitive aimed at an arbitrary conversation row —
    /// an already-paired peer could open an ordinary, non-chat transfer whose
    /// id happens to equal a message id in *our* thread with them, and an
    /// ungated write would then stamp our own outbound "sent" text as
    /// `Received`, or flip a file we `Declined` back to `Received` while
    /// keeping its old name/size, or relabel an unrelated row with a name and
    /// size of the peer's choosing. All three of the following must hold:
    ///
    /// 1. **`kind == File`** — a text row is never a transfer's business;
    /// 2. **`direction` agrees with `expected`** — `Out` for a send, `In` for
    ///    a receive, so a peer can drive neither our outbound rows nor the
    ///    reverse;
    /// 3. **still in flight** (`Transferring` | `PendingApproval`) — a
    ///    settled row is final, which makes every terminal write once-only
    ///    and rules out reopening an already-declined/received file.
    ///
    /// Both in-flight statuses are writable, in both directions: a *receiving*
    /// row legitimately moves `PendingApproval` → `Transferring` the moment the
    /// bytes are cleared to start (`Manager::handle_incoming`), and must stay
    /// writable afterwards so its landing metadata, its path and its final
    /// `Received` can still be recorded.
    ///
    /// [`ChatStore::settle_file_row`]: crate::ChatStore::settle_file_row
    /// [`ChatStore::set_file_row_path`]: crate::ChatStore::set_file_row_path
    #[must_use]
    pub fn is_settleable_file_row(&self, expected: Direction) -> bool {
        self.kind == Kind::File
            && self.direction == expected
            && matches!(self.status, Status::Transferring | Status::PendingApproval)
    }

    /// Whether this record is one the local user may still call off — the
    /// authorization check every surface exposing "cancel this file" must pass
    /// before deleting a staged blob or dequeueing an outbox entry.
    ///
    /// The sibling of [`is_settleable_file_row`](Self::is_settleable_file_row),
    /// and here for the same reason: cancelling takes a `peer_id` and a
    /// `message_id` from *outside* — the FFI's `pb_chat_cancel` request JSON,
    /// the CLI's `chat cancel <peer> <id>` arguments — and then **deletes
    /// bytes**. A bare key match at those two strings is not authorization: it
    /// would let a caller name any row in any conversation, including a text
    /// message, a file the peer is offering *us*, or a share that already
    /// completed. All three of the following must hold:
    ///
    /// 1. **`kind == File`** — there is nothing to cancel about a text row: no
    ///    staged blob, and its outbox entry is delivered or not;
    /// 2. **`direction == Out`** — only our *own* outgoing share. An inbound
    ///    offer is refused with the approval gate (I6), which this must never
    ///    become a second, unprompted path into;
    /// 3. **not already settled** — `Sent` and `Declined` are final and
    ///    exactly the two states [`ChatStore::reopen_for_retry`] refuses, so a
    ///    cancel could not stop anything that is still going to happen; it
    ///    could only rewrite history about something that already did.
    ///
    /// Everything else an outgoing file row can read — `Staging` (a copy is
    /// running), `Pending` (queued), `Transferring` (bytes moving), `Failed`
    /// and `Interrupted` (both of which may still have a live queue entry a
    /// later drain would retry) — is cancellable, because in every one of them
    /// there is genuinely something left to stop.
    ///
    /// Note what this does **not** decide: which conversation the row was read
    /// from. That is the caller's `peer_id`, and it is load-bearing — the row
    /// must be fetched from `peer`'s own namespace
    /// ([`ChatStore::get`](crate::ChatStore::get)), never scanned for across
    /// conversations, or the direction check would be the only thing standing
    /// between one thread's cancel and another thread's file.
    ///
    /// [`ChatStore::reopen_for_retry`]: crate::ChatStore::reopen_for_retry
    #[must_use]
    pub fn is_cancellable_outgoing_file(&self) -> bool {
        self.kind == Kind::File
            && self.direction == Direction::Out
            && !matches!(self.status, Status::Sent | Status::Declined)
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

    fn file_row(direction: Direction, status: Status) -> ChatRecord {
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("a.bin", 7).unwrap();
        let meta = FileMeta {
            name: r.name.clone(),
            size: r.size,
            local_path: None,
        };
        match direction {
            Direction::Out => ChatRecord::file_out(&peer, &r, meta, status),
            Direction::In => {
                let mut rec = ChatRecord::file_in(&peer, &r);
                rec.status = status;
                rec
            }
        }
    }

    #[test]
    fn is_settleable_file_row_accepts_the_two_legitimate_in_flight_shapes() {
        assert!(
            file_row(Direction::Out, Status::Transferring).is_settleable_file_row(Direction::Out)
        );
        assert!(
            file_row(Direction::In, Status::PendingApproval).is_settleable_file_row(Direction::In)
        );
    }

    #[test]
    fn is_settleable_file_row_rejects_a_text_row() {
        let rec = ChatRecord::sent(&DeviceId::from("pb-bob"), &ChatMessage::new("hi").unwrap());
        assert!(!rec.is_settleable_file_row(Direction::Out));
    }

    #[test]
    fn is_settleable_file_row_rejects_a_direction_mismatch() {
        // Our own outbound row must not be settleable as if it were a receive,
        // and vice versa — this is what stops a peer from driving the wrong
        // side of a conversation.
        assert!(
            !file_row(Direction::Out, Status::Transferring).is_settleable_file_row(Direction::In)
        );
        assert!(!file_row(Direction::In, Status::PendingApproval)
            .is_settleable_file_row(Direction::Out));
    }

    /// The homograph this exists for: a name that *renders* as a `.png` while
    /// ending in `.exe`, shown directly above an Accept button. The override
    /// must be visible in the rendered string, not silently dropped (which
    /// would hide that anything was there).
    #[test]
    fn display_name_defuses_a_bidi_override() {
        let hostile = "photo\u{202E}gnp.exe";
        let shown = display_name(hostile);
        assert!(
            !shown.contains('\u{202E}'),
            "the override must not survive: {shown:?}"
        );
        assert_eq!(shown, "photo\u{FFFD}gnp.exe");
        assert!(
            shown.ends_with(".exe"),
            "the real extension must still read as the last thing in the name"
        );
    }

    #[test]
    fn display_name_defuses_controls_and_isolates_and_leaves_ordinary_names_alone() {
        // C0, DEL, C1, and every bidi mark/override/isolate.
        for hostile in [
            "a\nb.txt",
            "a\rb.txt",
            "a\u{0}b.txt",
            "a\u{1B}b.txt",
            "a\u{7F}b.txt",
            "a\u{9B}b.txt",
            "a\u{200E}b.txt",
            "a\u{200F}b.txt",
            "a\u{202A}b.txt",
            "a\u{202D}b.txt",
            "a\u{202E}b.txt",
            "a\u{2066}b.txt",
            "a\u{2069}b.txt",
        ] {
            assert_eq!(
                display_name(hostile),
                "a\u{FFFD}b.txt",
                "not defused: {hostile:?}"
            );
        }
        // Ordinary names — including non-ASCII and emoji — are untouched.
        for ok in ["report.pdf", "Ünïcödé — file (1).tar.gz", "🎉 party.mov"] {
            assert_eq!(display_name(ok), ok);
        }
    }

    /// The one construction path production code uses must apply the policy;
    /// a row built from a hostile `FileRef` is already safe to render.
    #[test]
    fn file_in_renders_a_hostile_peer_name_safely() {
        let peer = DeviceId::from("pb-bob");
        let mut r = FileRef::new("ok.txt", 1).unwrap();
        // A peer can put this on the wire: `validate_name` (deliberately the
        // same policy transfers use) only rejects paths/lengths.
        r.name = "photo\u{202E}gnp.exe".to_string();
        let rec = ChatRecord::file_in(&peer, &r);
        assert_eq!(rec.file.unwrap().name, "photo\u{FFFD}gnp.exe");
    }

    #[test]
    fn is_settleable_file_row_rejects_an_already_settled_row() {
        for terminal in [
            Status::Pending,
            Status::Sent,
            Status::Received,
            Status::Declined,
            Status::Failed,
            Status::Interrupted,
        ] {
            assert!(
                !file_row(Direction::Out, terminal).is_settleable_file_row(Direction::Out),
                "{terminal:?} must not be re-settleable"
            );
        }
    }

    /// Every state an outgoing file row can sit in while something is still
    /// going to happen to it — including the two (`Failed`, `Interrupted`)
    /// whose queue entry a later drain would retry.
    #[test]
    fn is_cancellable_outgoing_file_accepts_every_state_with_work_left() {
        for status in [
            Status::Staging,
            Status::Pending,
            Status::Transferring,
            Status::Failed,
            Status::Interrupted,
        ] {
            assert!(
                file_row(Direction::Out, status).is_cancellable_outgoing_file(),
                "{status:?} still has something to stop"
            );
        }
    }

    /// The two final states. A cancel here could not stop anything — the file
    /// was delivered or refused — it could only rewrite history about it.
    #[test]
    fn is_cancellable_outgoing_file_rejects_a_settled_row() {
        for status in [Status::Sent, Status::Declined] {
            assert!(
                !file_row(Direction::Out, status).is_cancellable_outgoing_file(),
                "{status:?} is final"
            );
        }
    }

    /// The two shapes that make this a guard rather than a key lookup: a text
    /// row (nothing staged, nothing to delete) and the peer's own offer to us
    /// (refused at the approval gate, never here).
    #[test]
    fn is_cancellable_outgoing_file_rejects_text_and_inbound_rows() {
        let text = ChatRecord::sent(&DeviceId::from("pb-bob"), &ChatMessage::new("hi").unwrap());
        assert!(!text.is_cancellable_outgoing_file());
        for status in [
            Status::PendingApproval,
            Status::Transferring,
            Status::Received,
        ] {
            assert!(
                !file_row(Direction::In, status).is_cancellable_outgoing_file(),
                "an inbound offer in {status:?} is not ours to cancel"
            );
        }
    }

    #[test]
    fn staging_serializes_as_lowercase_and_is_not_settleable() {
        let v = serde_json::to_value(Status::Staging).unwrap();
        assert_eq!(v, serde_json::json!("staging"));
        // Staging is before the queue, let alone before a transfer: no
        // wire-driven settle may touch a row that has not been offered yet.
        let rec = file_row(Direction::Out, Status::Staging);
        assert!(!rec.is_settleable_file_row(Direction::Out));
    }
}
