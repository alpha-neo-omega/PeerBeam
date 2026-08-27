//! Resolving what a reply is answering, for rendering.
//!
//! A reply carries [`ChatRecord::in_reply_to`] — the id of the message it
//! answers, inside the same conversation. Turning that id into something a
//! surface can draw is decided **here**, once, rather than three times in three
//! frontends: the CLI's transcript, the app's quote bubble and any future web
//! view must agree about what an unresolvable parent looks like, or the same
//! message reads as two different things depending on where it is read.
//!
//! # A parent goes missing constantly, and that is not an error
//!
//! Deletion is local and unilateral in this crate — `delete_messages`,
//! `delete_conversation`, and now a [disappearing-message
//! window](crate::Retention) — and none of them ask what points at what. So the
//! ordinary lifetime of a reply includes the period after its parent is gone,
//! and on a thread with a short window that period is *most* of it. An orphaned
//! reply is a normal render state, not an exception, and everything below is
//! shaped by that.
//!
//! # What an orphan must look like, and why
//!
//! The reply is **always shown**, and only the quotation is lost:
//! [`ReplyContext::Orphaned`]. Two alternatives were rejected.
//!
//! *Hiding the reply* would delete one message because a different message was
//! deleted. The user wrote it, it is theirs, and nothing about the parent's
//! window says anything about it. It would also make a short window
//! catastrophic — a thread of replies would erase itself in cascade.
//!
//! *Silently dropping the marker* — rendering the reply as an ordinary message
//! — is worse than either, because it does not look like anything went wrong.
//! A reply's meaning is not in its own text: **"sure, go ahead"** answering
//! *"shall I delete the backups?"* and answering *"can I borrow a pen?"* are
//! the same seven characters. Showing it bare invites the reader to supply the
//! wrong question, and the reader has no way to know they are doing it. So the
//! marker survives its parent: the message keeps saying it was an answer, and
//! says that the question is no longer here.
//!
//! # The parent is not kept alive by being quoted
//!
//! Nothing here touches retention. A quoted message disappears on its own
//! schedule and takes its text with it — the reply does not pin it, and does
//! not keep a copy. The alternative (snapshot the quoted text into the reply,
//! as some clients do) would make a window unenforceable: the text the user
//! asked to have deleted would survive inside every message that ever answered
//! it, for as long as the newest of those lives, which a chain of replies
//! extends without limit.
//!
//! [`ChatRecord::in_reply_to`]: crate::ChatRecord::in_reply_to

use std::collections::HashMap;

use crate::record::{ChatRecord, Direction, Kind};

/// The longest quoted preview, in **characters** (not bytes).
///
/// A quote bubble is one line of context above a message, not a second copy of
/// it. Long enough to recognise which message is meant, short enough that a
/// 16 KiB body cannot be re-rendered above every answer to it.
pub const PREVIEW_CHARS: usize = 80;

/// The message a reply answers, as far as this device can still tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyParent {
    /// The parent's id — its key in this conversation.
    pub id: String,
    /// Which side sent the parent, so a surface can label the quote.
    pub direction: Direction,
    /// Text or file, so a surface can quote a file row as a file rather than as
    /// an empty message: a `Kind::File` row's `body` is empty by construction
    /// and its `preview` is the file name instead.
    pub kind: Kind,
    /// A leading excerpt of what is **stored**, at most [`PREVIEW_CHARS`]
    /// characters: the body for a text row, the file name for a file row.
    ///
    /// Cut on character boundaries and with nothing added — no ellipsis, no
    /// markers — the same rule `SearchHit::snippet` states and for the same
    /// reason: the moment something is added it stops being what is stored.
    /// A surface that wants to mark a truncation can compare the lengths.
    pub preview: String,
}

/// What one row is answering.
///
/// One value with three cases rather than an `Option` plus a flag, because a
/// surface would otherwise have to combine "is `in_reply_to` set" with "did the
/// lookup find anything", and the case that gets forgotten when a caller
/// combines them by hand is exactly [`Orphaned`](Self::Orphaned) — the one that
/// only shows up once a window has closed or a user has deleted something,
/// which is to say not while anybody is writing the surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyContext {
    /// Not a reply: render the message on its own.
    NotAReply,
    /// A reply whose parent is still here: render the quote.
    Quoting(ReplyParent),
    /// A reply whose parent this device no longer has — deleted, or its window
    /// closed. Render the message **with** an "original message unavailable"
    /// marker, never as an ordinary message; see the module doc.
    Orphaned {
        /// The id it still names. Kept so a surface can report it and a log can
        /// be followed, not because anything can be done with it.
        id: String,
    },
}

impl ReplyContext {
    /// Whether this row is a reply at all, resolvable or not.
    #[must_use]
    pub fn is_reply(&self) -> bool {
        !matches!(self, ReplyContext::NotAReply)
    }
}

/// Resolve every row's reply context against the rows themselves, index for
/// index: `out[i]` describes `rows[i]`.
///
/// # Pure, and resolved against what is *shown*
///
/// It takes no store and does no IO. Two consequences, both deliberate.
///
/// First, "the parent is gone" is defined as **not among these rows**, which is
/// the only definition that cannot drift from what the user is looking at.
/// `ChatStore::history` already filters out rows whose window has closed, so
/// resolving against its output means a message that has disappeared is
/// unresolvable *by construction* — there is no second code path that could
/// forget the window and quote a message the user was told is gone. A resolver
/// that went back to the store for each parent could do exactly that, and would
/// also turn rendering one screen into one store read per reply.
///
/// Second, it is a pure function of its inputs, so the behaviour every surface
/// depends on is asserted with hand-built rows and no store, no clock and no
/// sleeping.
///
/// # Scope and cost
///
/// `rows` must be **one conversation's** rows — what `ChatStore::history`
/// returns. A reply names a message in its own thread; resolving across threads
/// would let an id collision quote a stranger's message into this one.
///
/// One pass to index and one to resolve, so it is linear rather than the
/// quadratic scan a per-row lookup would be on a long thread. The index holds
/// borrowed ids and nothing is cloned but the previews actually produced.
#[must_use]
pub fn resolve_replies(rows: &[ChatRecord]) -> Vec<ReplyContext> {
    let by_id: HashMap<&str, &ChatRecord> = rows.iter().map(|rec| (rec.id.as_str(), rec)).collect();
    rows.iter()
        .map(|rec| match rec.in_reply_to.as_deref() {
            None => ReplyContext::NotAReply,
            Some(parent_id) => match by_id.get(parent_id) {
                Some(parent) => ReplyContext::Quoting(ReplyParent {
                    id: parent.id.clone(),
                    direction: parent.direction,
                    kind: parent.kind,
                    preview: preview_of(parent),
                }),
                None => ReplyContext::Orphaned {
                    id: parent_id.to_string(),
                },
            },
        })
        .collect()
}

/// The leading [`PREVIEW_CHARS`] characters of what `rec` holds: its body, or —
/// for a file row, whose body is empty by construction — its file name.
///
/// The name comes from [`FileMeta`](crate::FileMeta), which stores it through
/// `display_name`, so a quoted file name is already render-safe. A body is not
/// treated any further here: it is the same text the surface already renders in
/// the bubble below, under whatever policy it applies there, and a second,
/// different policy for the quote would make one message read two ways on one
/// screen.
fn preview_of(rec: &ChatRecord) -> String {
    let field = match (rec.kind, rec.file.as_ref()) {
        (Kind::File, Some(file)) => file.name.as_str(),
        _ => rec.body.as_str(),
    };
    // Cut on a character boundary; `char_indices` gives the byte offset of the
    // (PREVIEW_CHARS+1)th character, or nothing when the field is shorter.
    match field.char_indices().nth(PREVIEW_CHARS) {
        Some((end, _)) => field[..end].to_string(),
        None => field.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{FileMeta, Status};

    fn row(id: &str, body: &str, in_reply_to: Option<&str>) -> ChatRecord {
        ChatRecord {
            id: id.to_string(),
            peer_id: "pb-bob".to_string(),
            direction: Direction::Out,
            timestamp: "2026-01-01T10:00:00Z".to_string(),
            body: body.to_string(),
            status: Status::Sent,
            kind: Kind::Text,
            file: None,
            read_at: None,
            reactions: Vec::new(),
            in_reply_to: in_reply_to.map(str::to_string),
            group: None,
            stored_at: None,
        }
    }

    #[test]
    fn a_reply_quotes_the_row_it_names() {
        let rows = vec![
            row("m1", "shall I delete the backups?", None),
            row("m2", "sure, go ahead", Some("m1")),
        ];
        let ctx = resolve_replies(&rows);
        assert_eq!(ctx.len(), rows.len(), "one context per row, index aligned");
        assert_eq!(ctx[0], ReplyContext::NotAReply);
        match &ctx[1] {
            ReplyContext::Quoting(parent) => {
                assert_eq!(parent.id, "m1");
                assert_eq!(parent.preview, "shall I delete the backups?");
                assert_eq!(parent.kind, Kind::Text);
            }
            other => panic!("expected a resolved quote, got {other:?}"),
        }
    }

    /// **The case this whole module exists for.** The parent is gone — deleted,
    /// or its window closed — and the reply must still render, still saying it
    /// was an answer. Turning `Orphaned` into `NotAReply` would make "sure, go
    /// ahead" read as a standalone statement, and must make this fail.
    #[test]
    fn a_reply_whose_parent_is_gone_still_renders_as_a_reply() {
        let rows = vec![row("m2", "sure, go ahead", Some("m1"))];
        let ctx = resolve_replies(&rows);
        assert_eq!(
            ctx[0],
            ReplyContext::Orphaned {
                id: "m1".to_string()
            },
            "an orphan must keep saying it answered something"
        );
        assert!(ctx[0].is_reply());
    }

    /// A reply is never dropped from the rows because its parent went: the
    /// message is the user's and the parent's deletion said nothing about it.
    #[test]
    fn an_orphaned_reply_is_never_dropped_from_the_rendered_rows() {
        let rows = vec![row("m2", "sure", Some("gone")), row("m3", "and this", None)];
        assert_eq!(resolve_replies(&rows).len(), 2);
    }

    /// Replies to replies do not cascade: losing the head of a chain orphans
    /// exactly one message, not the chain.
    #[test]
    fn losing_the_head_of_a_chain_orphans_only_the_row_that_named_it() {
        let rows = vec![
            row("m2", "answer", Some("m1")),
            row("m3", "answer to the answer", Some("m2")),
        ];
        let ctx = resolve_replies(&rows);
        assert!(matches!(ctx[0], ReplyContext::Orphaned { .. }));
        assert!(matches!(ctx[1], ReplyContext::Quoting(_)));
    }

    /// A file row's body is empty by construction, so quoting it must show the
    /// file name or the quote is a blank bubble.
    #[test]
    fn quoting_a_file_row_previews_its_name_not_its_empty_body() {
        let mut parent = row("m1", "", None);
        parent.kind = Kind::File;
        parent.file = Some(FileMeta::new("quarterly.pdf", 12, None));
        let rows = vec![parent, row("m2", "got it", Some("m1"))];
        match &resolve_replies(&rows)[1] {
            ReplyContext::Quoting(p) => {
                assert_eq!(p.preview, "quarterly.pdf");
                assert_eq!(p.kind, Kind::File);
            }
            other => panic!("expected a quote, got {other:?}"),
        }
    }

    /// Bounded, and cut where a character ends — a body is up to `MAX_BODY`
    /// bytes and a quote is one line of context, not a second copy.
    #[test]
    fn a_long_preview_is_cut_to_the_bound_on_a_character_boundary() {
        let body = "é".repeat(PREVIEW_CHARS * 2);
        let rows = vec![row("m1", &body, None), row("m2", "ok", Some("m1"))];
        match &resolve_replies(&rows)[1] {
            ReplyContext::Quoting(p) => {
                assert_eq!(p.preview.chars().count(), PREVIEW_CHARS);
                // Nothing added: it is a prefix of what is stored, exactly as
                // `SearchHit::snippet` is a substring of it.
                assert!(body.starts_with(&p.preview));
            }
            other => panic!("expected a quote, got {other:?}"),
        }
    }

    #[test]
    fn a_thread_with_no_replies_resolves_to_nothing_at_all() {
        let rows = vec![row("m1", "hello", None), row("m2", "hi", None)];
        assert!(resolve_replies(&rows).iter().all(|c| !c.is_reply()));
        assert!(resolve_replies(&[]).is_empty());
    }
}
