//! Searching this device's **own** stored chat history.
//!
//! Local only, by construction. It reads the same conversation namespaces
//! [`ChatStore::history`] reads and nothing else: no channel is opened, no peer
//! is dialled, nothing is asked of anybody. A thread for a peer that has gone
//! offline — or that no longer exists at all — is searchable exactly like one
//! whose device is sitting on the same desk, and a conversation the user
//! deleted is not searchable at all, because the rows are simply gone.
//!
//! # Why this lives in the engine
//!
//! The alternative — a filter in the surface — means loading every message of
//! every conversation across the FFI to answer one query. That is the wrong
//! shape at any size worth searching, and it would have to be written again for
//! each of the three surfaces.
//!
//! # What is matched
//!
//! A **case-insensitive substring** of a message's text body, or of a file
//! message's **name**. Deliberately not a regex: a user-supplied pattern is an
//! unbounded amount of work over an unbounded amount of history, and nothing
//! here needs one.
//!
//! A file's `local_path` is **never** searched. It is this device's filesystem
//! layout, not conversation content, and matching on it would surface a thread
//! because of where a file happens to sit on disk — `/home/alice/…` would
//! return every file anyone ever sent.

use std::cmp::Reverse;

use peerbeam_domain::id::DeviceId;

use crate::message::ChatError;
use crate::record::{ChatRecord, Direction, Kind};
use crate::store::ChatStore;

/// How many hits a surface asks for when it does not say.
pub const DEFAULT_SEARCH_LIMIT: usize = 50;

/// The most a surface may ask for in one call.
///
/// Not a limit on how much history is searched — every conversation is walked
/// whatever this is — but on how much comes back, so one call cannot build an
/// arbitrarily large response. A user who genuinely needs more than this is
/// asking the wrong question of a search box; the honest answer is
/// [`SearchResults::truncated`] and a narrower query.
pub const MAX_SEARCH_LIMIT: usize = 500;

/// The longest snippet, in **characters** (not bytes).
const SNIPPET_CHARS: usize = 120;

/// How much of the stored text precedes the match in a snippet, in characters.
/// Enough to read the match in context; short enough that the match itself is
/// always inside the window.
const SNIPPET_LEAD: usize = 24;

/// One message that matched, and enough to navigate to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The conversation the row was read from — see [`ChatStore::search`] for
    /// why this is the namespace's peer and not the record's own field.
    pub peer_id: String,
    /// The row's id, which is also its key in that conversation.
    pub message_id: String,
    pub timestamp: String,
    pub direction: Direction,
    pub kind: Kind,
    /// A **substring of the stored text** that matched — the body for a text
    /// row, the file name for a file row. Never re-rendered, re-wrapped,
    /// highlighted or elided: a surface that wants to mark the match can find
    /// it in here itself, and a snippet that had been reformatted would no
    /// longer be evidence of what is actually stored.
    pub snippet: String,
}

impl SearchHit {
    /// The total order hits are returned in: **newest first**, ties broken by
    /// peer id and then message id, both ascending.
    ///
    /// The tiebreak is not decoration. Timestamps collide readily — a burst of
    /// queued messages flushed together, an inbound row stamped by a peer whose
    /// clock is coarse — and an arbitrary order among them makes both paging
    /// and tests flaky. `(peer_id, message_id)` is unique (a message id is its
    /// key within one conversation's namespace), so this is a *total* order:
    /// two runs over the same stored rows return the same sequence.
    fn order(&self) -> (Reverse<&str>, &str, &str) {
        (
            Reverse(self.timestamp.as_str()),
            self.peer_id.as_str(),
            self.message_id.as_str(),
        )
    }
}

/// What a search found — and, crucially, whether that was all there was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    /// Newest first; see [`SearchHit::order`].
    pub hits: Vec<SearchHit>,
    /// **There were more matches than the limit allowed.** A caller must say
    /// so. A bounded search that silently returns its first `n` reads as "that
    /// is all there is", which — for a search over the user's own history — is
    /// a wrong answer rather than a partial one: the message they are looking
    /// for exists, they were told it does not, and nothing on screen suggests
    /// asking differently.
    pub truncated: bool,
}

impl ChatStore {
    /// Search every conversation this device holds for `query`, returning at
    /// most `limit` hits, newest first.
    ///
    /// `query` is matched **case-insensitively** as a plain **substring** of a
    /// message's text body or a file message's name. Leading and trailing
    /// whitespace is trimmed (a search box collects it by accident); whitespace
    /// *inside* the query is significant. A query that is empty or nothing but
    /// whitespace finds **nothing** — returning everything would turn an
    /// unfinished keystroke into a full history dump.
    ///
    /// # Case-insensitivity
    ///
    /// Both sides are folded with Unicode's per-character lowercase mapping
    /// ([`char::to_lowercase`]), so `Ünïcödé`, `ÜNÏCÖDÉ` and `ünïcödé` are one
    /// query, as are `Привет` and `ПРИВЕТ`. This is lowercase *mapping*, not
    /// full Unicode case folding, which Rust's standard library does not offer
    /// and which no dependency here would earn: German `ß` therefore does not
    /// match `SS`, and Greek final `ς` does not match medial `σ`. The
    /// alternative — matching bytes — would leave every non-English
    /// conversation searchable only in the case it was typed.
    ///
    /// # Which peer a hit belongs to
    ///
    /// The **namespace the row was read from**, never the `peer_id` field
    /// inside the record. They agree for everything this crate writes, but only
    /// the namespace decides which thread the message is actually *in*, and
    /// that is what a surface opens when the user taps the hit. A hit with the
    /// right message and the wrong peer sends the user to a conversation the
    /// message is not in.
    ///
    /// # Boundedness, and what is *not* bounded
    ///
    /// `limit` bounds the *result*, not the work: every conversation is walked
    /// either way, because the newest matches cannot be known without looking
    /// at all of them. Memory is bounded — at most `limit` hits are ever held —
    /// and [`SearchResults::truncated`] reports when that mattered. A `limit`
    /// of 0 is honest rather than an error: no hits, and `truncated` set if
    /// there was anything to cut.
    ///
    /// Cost is one namespace scan plus one full history read per conversation,
    /// the same shape `Manager::chat_conversations` already pays, and a folded
    /// scan of each candidate body (at most [`crate::MAX_BODY`] bytes) and file
    /// name. Nothing is cached: a search runs against what is on disk now.
    ///
    /// # Failure
    ///
    /// A row this build cannot decode is **skipped**, because
    /// [`ChatStore::history`] skips it — one row from a newer schema must not
    /// make a conversation unsearchable, let alone abort a search across all of
    /// them. A genuine store failure reading a conversation is *not* skipped:
    /// it is returned. Carrying on would quietly drop that thread's matches and
    /// report the rest as though they were everything, which is the same lie
    /// `truncated` exists to prevent — only with nothing at all to hint at it.
    pub fn search(&self, query: &str, limit: usize) -> Result<SearchResults, ChatError> {
        let needle = fold(query.trim());
        if needle.is_empty() {
            return Ok(SearchResults::default());
        }
        let mut top = TopHits::new(limit);
        for peer in self.conversations()? {
            for rec in self.history(&peer)? {
                if let Some(hit) = hit(&peer, &rec, &needle) {
                    top.push(hit);
                }
            }
        }
        Ok(top.into_results())
    }
}

/// The hit for `rec`, or `None` if it does not match.
///
/// One hit per message, never one per matching field: a message is a single
/// thing to navigate to, and a row whose body and file name both matched is
/// still one message.
fn hit(peer: &DeviceId, rec: &ChatRecord, needle: &str) -> Option<SearchHit> {
    // Body first, then the file's NAME — and nothing else. `file.local_path` is
    // deliberately absent: see the module doc.
    let snippet = snippet_of(&rec.body, needle).or_else(|| {
        rec.file
            .as_ref()
            .and_then(|file| snippet_of(&file.name, needle))
    })?;
    Some(SearchHit {
        // The namespace's peer, not `rec.peer_id`; see `ChatStore::search`.
        peer_id: peer.0.clone(),
        message_id: rec.id.clone(),
        timestamp: rec.timestamp.clone(),
        direction: rec.direction,
        kind: rec.kind,
        snippet,
    })
}

/// A window of `field` around the first case-insensitive occurrence of the
/// already-folded `needle`, or `None` when there is none.
///
/// The window is a plain substring: [`SNIPPET_LEAD`] characters of lead-in
/// where there is that much, then at most [`SNIPPET_CHARS`] characters in all.
/// Cut on character boundaries, so no multi-byte character is ever halved, and
/// with nothing added — no ellipsis, no markers — because the moment something
/// is added it stops being what is stored.
fn snippet_of(field: &str, needle: &str) -> Option<String> {
    let at = find_folded(field, needle)?;
    let start = field[..at]
        .char_indices()
        .rev()
        .take(SNIPPET_LEAD)
        .last()
        .map_or(at, |(i, _)| i);
    let end = field[start..]
        .char_indices()
        .nth(SNIPPET_CHARS)
        .map_or(field.len(), |(i, _)| start + i);
    Some(field[start..end].to_string())
}

/// The byte offset in `haystack` at which a case-insensitive occurrence of the
/// already-folded `needle` begins.
///
/// Walks `haystack`'s own character boundaries and folds as it compares, rather
/// than folding the whole string and searching that. Two reasons: it allocates
/// nothing per record, and the offset it returns is an offset into the
/// **original**, which is what a snippet has to be cut from. Folding first
/// would hand back an offset into the folded copy, and the two do not line up
/// — a character whose lowercase form is longer than itself (`İ` → `i̇`) shifts
/// everything after it.
///
/// Naive, so O(n·m) in the worst case. Both are small and bounded (a body is at
/// most [`crate::MAX_BODY`], a name [`crate::MAX_NAME`]), a mismatch is
/// detected on the first character in ordinary text, and this is a
/// user-initiated read of local storage rather than anything on a hot path.
fn find_folded(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack
        .char_indices()
        .map(|(i, _)| i)
        .find(|&i| folded_starts_with(&haystack[i..], needle))
}

/// Whether `hay`, folded, begins with the already-folded `needle`.
fn folded_starts_with(hay: &str, needle: &str) -> bool {
    let mut want = needle.chars();
    for c in hay.chars() {
        for lc in c.to_lowercase() {
            // The needle ran out mid-character: everything it asked for has
            // already matched.
            let Some(w) = want.next() else { return true };
            if w != lc {
                return false;
            }
        }
    }
    want.next().is_none()
}

/// Unicode per-character lowercase mapping, applied identically to both sides.
/// See [`ChatStore::search`] for what this is and is not.
fn fold(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).collect()
}

/// The `limit` best hits seen so far, held in order, plus whether anything had
/// to be dropped to stay inside it.
///
/// A bounded top-N rather than "collect everything, sort, truncate": the result
/// is identical — the newest `limit` matches in the whole store — but at most
/// `limit` hits are ever held, so a query matching a hundred thousand rows
/// costs the same memory as one matching ten. It is also the only place
/// `truncated` can be set honestly, because it is the only place that knows a
/// hit was thrown away.
struct TopHits {
    hits: Vec<SearchHit>,
    limit: usize,
    truncated: bool,
}

impl TopHits {
    fn new(limit: usize) -> TopHits {
        TopHits {
            // Capped: `limit` is a caller's number and may be enormous, and an
            // allocation that large before a single match is found would be a
            // denial of service written into the search itself.
            hits: Vec::with_capacity(limit.min(64)),
            limit,
            truncated: false,
        }
    }

    fn push(&mut self, hit: SearchHit) {
        let at = self.hits.partition_point(|held| held.order() < hit.order());
        // Older than everything already held, and there is no room: dropped,
        // and said so.
        if at >= self.limit {
            self.truncated = true;
            return;
        }
        self.hits.insert(at, hit);
        if self.hits.len() > self.limit {
            self.hits.pop();
            self.truncated = true;
        }
    }

    fn into_results(self) -> SearchResults {
        SearchResults {
            hits: self.hits,
            truncated: self.truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{FileMeta, Status};
    use crate::{ChatMessage, ChatStore};
    use peerbeam_appstore_fs::FsAppStore;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::port::{AppStore, EncryptionProvider};
    use std::sync::Arc;

    /// A fresh store, plus the raw `AppStore` handle so a test can write a
    /// value `ChatStore`'s own encode paths could never produce (an undecodable
    /// row, or a record filed under a namespace that disagrees with its own
    /// `peer_id`).
    fn new_store() -> (ChatStore, Arc<dyn AppStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[7u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> =
            Arc::new(FsAppStore::open(dir.path().join("appstore"), key, enc));
        (ChatStore::new(app.clone()), app, dir)
    }

    fn text(peer: &str, id: &str, ts: &str, body: &str) -> ChatRecord {
        ChatRecord {
            id: id.to_string(),
            peer_id: peer.to_string(),
            direction: Direction::In,
            timestamp: ts.to_string(),
            body: body.to_string(),
            status: Status::Received,
            kind: Kind::Text,
            file: None,
            read_at: None,
            reactions: Vec::new(),
        }
    }

    fn file(peer: &str, id: &str, ts: &str, name: &str, local_path: Option<&str>) -> ChatRecord {
        ChatRecord {
            id: id.to_string(),
            peer_id: peer.to_string(),
            direction: Direction::Out,
            timestamp: ts.to_string(),
            body: String::new(),
            status: Status::Sent,
            kind: Kind::File,
            file: Some(FileMeta {
                name: name.to_string(),
                size: 12,
                local_path: local_path.map(str::to_string),
            }),
            read_at: None,
            reactions: Vec::new(),
        }
    }

    fn ids(r: &SearchResults) -> Vec<&str> {
        r.hits.iter().map(|h| h.message_id.as_str()).collect()
    }

    #[test]
    fn finds_a_match_in_a_text_body() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&text(
            "pb-a",
            "m1",
            "2026-01-01T00:00:00Z",
            "the quarterly report",
        ))
        .unwrap();
        cs.append(&text("pb-a", "m2", "2026-01-01T00:00:01Z", "lunch?"))
            .unwrap();

        let found = cs.search("quarterly", DEFAULT_SEARCH_LIMIT).unwrap();
        assert_eq!(ids(&found), ["m1"]);
        assert_eq!(found.hits[0].peer_id, "pb-a");
        assert_eq!(found.hits[0].kind, Kind::Text);
        assert_eq!(found.hits[0].direction, Direction::In);
        assert_eq!(found.hits[0].timestamp, "2026-01-01T00:00:00Z");
        assert!(!found.truncated);
    }

    #[test]
    fn finds_a_match_in_a_file_name() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&file(
            "pb-a",
            "f1",
            "2026-01-01T00:00:00Z",
            "budget-2026.xlsx",
            None,
        ))
        .unwrap();

        let found = cs.search("budget", DEFAULT_SEARCH_LIMIT).unwrap();
        assert_eq!(ids(&found), ["f1"]);
        assert_eq!(found.hits[0].kind, Kind::File);
        // The snippet is the stored name, not a re-rendered one.
        assert_eq!(found.hits[0].snippet, "budget-2026.xlsx");
    }

    /// `local_path` is this device's filesystem layout, not conversation
    /// content. Searching it would surface a thread because of where a file
    /// happens to sit on disk — and every file ever received would answer to
    /// the name of the downloads folder.
    #[test]
    fn never_matches_on_a_files_local_path() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&file(
            "pb-a",
            "f1",
            "2026-01-01T00:00:00Z",
            "notes.txt",
            Some("/home/alice/Downloads/secret-project/notes.txt"),
        ))
        .unwrap();

        // Every one of these appears only in the path.
        for query in ["Downloads", "secret-project", "/home/alice", "alice"] {
            let found = cs.search(query, DEFAULT_SEARCH_LIMIT).unwrap();
            assert!(
                found.hits.is_empty(),
                "{query:?} matched a local_path: {:?}",
                found.hits
            );
        }
        // The name itself still matches, so this is an exclusion and not a
        // file row that cannot be searched at all.
        assert_eq!(
            ids(&cs.search("notes", DEFAULT_SEARCH_LIMIT).unwrap()),
            ["f1"]
        );
    }

    #[test]
    fn matching_is_case_insensitive_including_non_ascii() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&text(
            "pb-a",
            "m1",
            "2026-01-01T00:00:00Z",
            "Quarterly REPORT",
        ))
        .unwrap();
        cs.append(&text(
            "pb-a",
            "m2",
            "2026-01-01T00:00:01Z",
            "ПРИВЕТ, как дела",
        ))
        .unwrap();
        cs.append(&text(
            "pb-a",
            "m3",
            "2026-01-01T00:00:02Z",
            "l'École est fermée",
        ))
        .unwrap();
        cs.append(&file(
            "pb-a",
            "f1",
            "2026-01-01T00:00:03Z",
            "RÉSUMÉ-FINAL.PDF",
            None,
        ))
        .unwrap();

        // ASCII, both directions.
        assert_eq!(ids(&cs.search("quarterly", 10).unwrap()), ["m1"]);
        assert_eq!(ids(&cs.search("REPORT", 10).unwrap()), ["m1"]);
        // Cyrillic: stored upper, queried lower, and the reverse.
        assert_eq!(ids(&cs.search("привет", 10).unwrap()), ["m2"]);
        assert_eq!(ids(&cs.search("ДЕЛА", 10).unwrap()), ["m2"]);
        // Latin-1 accents, in a body and in a file name.
        assert_eq!(ids(&cs.search("école", 10).unwrap()), ["m3"]);
        assert_eq!(ids(&cs.search("résumé", 10).unwrap()), ["f1"]);
        assert_eq!(ids(&cs.search("RÉSUMÉ", 10).unwrap()), ["f1"]);
    }

    /// The honest limit of the folding chosen, pinned so it cannot change
    /// silently: this is Unicode lowercase *mapping*, not full case folding.
    /// `ß` does not match `SS`. If a future change adopts real case folding,
    /// this test is the one that must be updated deliberately.
    #[test]
    fn folds_by_lowercase_mapping_not_by_full_case_folding() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&text("pb-a", "m1", "2026-01-01T00:00:00Z", "Straße 7"))
            .unwrap();

        // What lowercase mapping does give us: the capital form matches.
        assert_eq!(ids(&cs.search("STRAßE", 10).unwrap()), ["m1"]);
        assert_eq!(ids(&cs.search("straße", 10).unwrap()), ["m1"]);
        // What it does not: ß is not folded to ss.
        assert!(cs.search("strasse", 10).unwrap().hits.is_empty());
    }

    /// The failure worth pinning is not the count — it is a hit attributed to
    /// the wrong conversation, which sends the user to a thread the message is
    /// not in.
    #[test]
    fn searches_every_conversation_and_attributes_each_hit_to_its_own_peer() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&text(
            "pb-a",
            "m1",
            "2026-01-01T00:00:01Z",
            "shipping the invoice",
        ))
        .unwrap();
        cs.append(&text(
            "pb-b",
            "m2",
            "2026-01-01T00:00:02Z",
            "invoice attached",
        ))
        .unwrap();
        cs.append(&text(
            "pb-c",
            "m3",
            "2026-01-01T00:00:03Z",
            "no mention here",
        ))
        .unwrap();
        cs.append(&file(
            "pb-c",
            "f1",
            "2026-01-01T00:00:04Z",
            "invoice-final.pdf",
            None,
        ))
        .unwrap();

        let found = cs.search("invoice", DEFAULT_SEARCH_LIMIT).unwrap();
        let attributed: Vec<(&str, &str)> = found
            .hits
            .iter()
            .map(|h| (h.peer_id.as_str(), h.message_id.as_str()))
            .collect();
        // Newest first.
        assert_eq!(attributed, [("pb-c", "f1"), ("pb-b", "m2"), ("pb-a", "m1")]);
        assert!(!found.truncated);
    }

    /// The record's own `peer_id` field is not what decides which thread a hit
    /// belongs to — the namespace it was read from is. They agree for
    /// everything this crate writes; only one of them is where the message
    /// actually is.
    #[test]
    fn a_hit_is_attributed_to_the_namespace_it_was_read_from() {
        let (cs, raw, _dir) = new_store();
        let mut rec = text("pb-a", "m1", "2026-01-01T00:00:00Z", "misfiled invoice");
        rec.peer_id = "pb-somebody-else".to_string();
        raw.put("chat-pb-a", "m1", &rec.encode()).unwrap();

        let found = cs.search("invoice", DEFAULT_SEARCH_LIMIT).unwrap();
        assert_eq!(found.hits.len(), 1);
        assert_eq!(found.hits[0].peer_id, "pb-a");
    }

    #[test]
    fn respects_the_limit_and_reports_truncation() {
        let (cs, _raw, _dir) = new_store();
        for i in 0..10 {
            cs.append(&text(
                "pb-a",
                &format!("m{i}"),
                &format!("2026-01-01T00:00:{i:02}Z"),
                "invoice",
            ))
            .unwrap();
        }

        let found = cs.search("invoice", 3).unwrap();
        assert_eq!(found.hits.len(), 3);
        assert!(
            found.truncated,
            "10 matches capped at 3 must report truncation"
        );
        // And the three it kept are the newest three, in order.
        assert_eq!(ids(&found), ["m9", "m8", "m7"]);
    }

    /// Truncation means "there was more", not "a limit was set". A result that
    /// exactly fills the limit is complete, and saying otherwise would send the
    /// user hunting for messages that do not exist.
    #[test]
    fn an_exact_fit_is_not_reported_as_truncated() {
        let (cs, _raw, _dir) = new_store();
        for i in 0..3 {
            cs.append(&text(
                "pb-a",
                &format!("m{i}"),
                &format!("2026-01-01T00:00:{i:02}Z"),
                "invoice",
            ))
            .unwrap();
        }

        let found = cs.search("invoice", 3).unwrap();
        assert_eq!(found.hits.len(), 3);
        assert!(!found.truncated);
    }

    /// A limit of zero is a real answer, not an error: nothing back, and an
    /// honest `truncated` when there was something to cut.
    #[test]
    fn a_zero_limit_returns_nothing_and_admits_it_cut_something() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&text("pb-a", "m1", "2026-01-01T00:00:00Z", "invoice"))
            .unwrap();

        let found = cs.search("invoice", 0).unwrap();
        assert!(found.hits.is_empty());
        assert!(found.truncated);
        // Nothing matched at all: nothing was cut either.
        let none = cs.search("nothing-like-this", 0).unwrap();
        assert!(none.hits.is_empty());
        assert!(!none.truncated);
    }

    #[test]
    fn an_empty_or_whitespace_query_finds_nothing() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&text(
            "pb-a",
            "m1",
            "2026-01-01T00:00:00Z",
            "anything at all",
        ))
        .unwrap();
        cs.append(&file(
            "pb-a",
            "f1",
            "2026-01-01T00:00:01Z",
            "a file.txt",
            None,
        ))
        .unwrap();

        for query in ["", " ", "\t", "\n", "   \t \n ", "\u{00A0}"] {
            let found = cs.search(query, DEFAULT_SEARCH_LIMIT).unwrap();
            assert!(
                found.hits.is_empty() && !found.truncated,
                "{query:?} returned {:?}",
                found.hits
            );
        }
    }

    /// `ChatStore::history` skips a row it cannot decode rather than failing
    /// the namespace (forward compatibility with a newer schema). Search
    /// inherits that: one bad row costs that row, not the conversation, and
    /// certainly not the whole search.
    #[test]
    fn an_undecodable_row_is_skipped_and_the_rest_still_matches() {
        let (cs, raw, _dir) = new_store();
        cs.append(&text("pb-a", "m1", "2026-01-01T00:00:01Z", "first invoice"))
            .unwrap();
        // Not a `ChatRecord` in any version — written straight through the raw
        // handle, as no `ChatStore` path could produce it.
        raw.put("chat-pb-a", "m2", b"{not-json-at-all").unwrap();
        cs.append(&text(
            "pb-a",
            "m3",
            "2026-01-01T00:00:03Z",
            "second invoice",
        ))
        .unwrap();
        cs.append(&text("pb-b", "m4", "2026-01-01T00:00:04Z", "third invoice"))
            .unwrap();

        let found = cs.search("invoice", DEFAULT_SEARCH_LIMIT).unwrap();
        // The bad row is gone; every other row — in this conversation and in
        // the next one — is still found.
        assert_eq!(ids(&found), ["m4", "m3", "m1"]);
    }

    /// Deleted history is gone. Search reads storage and only storage, so
    /// there is nowhere for it to come back from.
    #[test]
    fn a_deleted_conversation_yields_no_hits() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&text(
            "pb-a",
            "m1",
            "2026-01-01T00:00:01Z",
            "invoice for pb-a",
        ))
        .unwrap();
        cs.append(&text(
            "pb-b",
            "m2",
            "2026-01-01T00:00:02Z",
            "invoice for pb-b",
        ))
        .unwrap();
        assert_eq!(cs.search("invoice", 10).unwrap().hits.len(), 2);

        cs.delete_conversation(&DeviceId::from("pb-a")).unwrap();

        let found = cs.search("invoice", 10).unwrap();
        assert_eq!(ids(&found), ["m2"]);
        assert_eq!(found.hits[0].peer_id, "pb-b");
    }

    #[test]
    fn ordering_is_newest_first_and_stable_when_timestamps_tie() {
        let (cs, _raw, _dir) = new_store();
        // Three peers, one shared timestamp, plus one genuinely newer row.
        let tied = "2026-01-01T00:00:00Z";
        cs.append(&text("pb-b", "m1", tied, "invoice b")).unwrap();
        cs.append(&text("pb-a", "m1", tied, "invoice a")).unwrap();
        cs.append(&text("pb-a", "m2", tied, "invoice a2")).unwrap();
        cs.append(&text("pb-c", "m9", "2026-01-02T00:00:00Z", "invoice c"))
            .unwrap();

        let expected = [
            ("pb-c", "m9"), // newest
            ("pb-a", "m1"), // then the tie, by peer id then message id
            ("pb-a", "m2"),
            ("pb-b", "m1"),
        ];
        // Run it repeatedly: the namespace scan underneath makes no ordering
        // promise, so determinism has to come from the sort.
        for _ in 0..5 {
            let found = cs.search("invoice", 10).unwrap();
            let got: Vec<(&str, &str)> = found
                .hits
                .iter()
                .map(|h| (h.peer_id.as_str(), h.message_id.as_str()))
                .collect();
            assert_eq!(got, expected);
        }
    }

    /// The queued copy of a message lives in the shared outbox namespace, which
    /// is not a conversation. Searching it would report the same message twice
    /// — once as a thread row, once as a queue entry — for every message
    /// waiting on an offline peer.
    #[test]
    fn a_queued_message_matches_once_not_twice() {
        let (cs, _raw, _dir) = new_store();
        let peer = DeviceId::from("pb-a");
        cs.enqueue(&peer, &ChatMessage::new("queued invoice").unwrap())
            .unwrap();

        let found = cs.search("invoice", 10).unwrap();
        assert_eq!(found.hits.len(), 1);
        assert_eq!(found.hits[0].peer_id, "pb-a");
    }

    #[test]
    fn the_snippet_is_a_substring_of_the_stored_body_containing_the_match() {
        let (cs, _raw, _dir) = new_store();
        let body = format!("{}NEEDLE{}", "x".repeat(400), "y".repeat(400));
        cs.append(&text("pb-a", "m1", "2026-01-01T00:00:00Z", &body))
            .unwrap();

        let found = cs.search("needle", 10).unwrap();
        let snippet = &found.hits[0].snippet;
        assert!(
            body.contains(snippet.as_str()),
            "snippet is not a substring of the stored body: {snippet:?}"
        );
        assert!(
            snippet.contains("NEEDLE"),
            "snippet lost the match it was cut around: {snippet:?}"
        );
        assert!(snippet.chars().count() <= SNIPPET_CHARS);
        // Not re-rendered: the stored casing survives, and nothing is appended.
        assert!(!snippet.contains('…'));
    }

    /// A short body comes back whole, and a multi-byte character is never cut
    /// in half — the snippet is sliced on character boundaries, so a body of
    /// nothing but wide characters is still valid UTF-8 coming out.
    #[test]
    fn a_snippet_cuts_on_character_boundaries() {
        let (cs, _raw, _dir) = new_store();
        let body = format!("{}Привет{}", "я".repeat(300), "ё".repeat(300));
        cs.append(&text("pb-a", "m1", "2026-01-01T00:00:00Z", &body))
            .unwrap();
        cs.append(&text("pb-a", "m2", "2026-01-01T00:00:01Z", "короткое"))
            .unwrap();

        let long = cs.search("привет", 10).unwrap();
        assert!(body.contains(long.hits[0].snippet.as_str()));
        assert!(long.hits[0].snippet.contains("Привет"));

        let short = cs.search("короткое", 10).unwrap();
        assert_eq!(short.hits[0].snippet, "короткое");
    }

    #[test]
    fn a_multi_word_query_is_a_plain_substring_not_a_word_set() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&text(
            "pb-a",
            "m1",
            "2026-01-01T00:00:00Z",
            "send the quarterly report",
        ))
        .unwrap();

        assert_eq!(ids(&cs.search("quarterly report", 10).unwrap()), ["m1"]);
        // Interior whitespace is significant; the words are not reordered or
        // matched independently.
        assert!(cs.search("report quarterly", 10).unwrap().hits.is_empty());
        // Surrounding whitespace is not.
        assert_eq!(ids(&cs.search("  quarterly  ", 10).unwrap()), ["m1"]);
    }

    /// A regex is not a query language here. `.*` is matched as the three
    /// characters it is, which is the whole point of choosing substring
    /// matching: no user-supplied pattern can cost unbounded work.
    #[test]
    fn a_regex_is_matched_literally() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&text("pb-a", "m1", "2026-01-01T00:00:00Z", "plain text"))
            .unwrap();
        cs.append(&text("pb-a", "m2", "2026-01-01T00:00:01Z", "a .* literal"))
            .unwrap();

        let found = cs.search(".*", 10).unwrap();
        assert_eq!(ids(&found), ["m2"]);
    }

    /// A search asks nothing of anybody: no peer needs to exist, be online, or
    /// ever have been discovered.
    #[test]
    fn a_thread_for_a_peer_that_no_longer_exists_is_still_searchable() {
        let (cs, _raw, _dir) = new_store();
        cs.append(&text(
            "pb-long-gone",
            "m1",
            "2026-01-01T00:00:00Z",
            "the last invoice",
        ))
        .unwrap();

        let found = cs.search("invoice", 10).unwrap();
        assert_eq!(found.hits[0].peer_id, "pb-long-gone");
    }
}
