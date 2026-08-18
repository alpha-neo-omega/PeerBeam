//! Notes on disk, in the encrypted [`AppStore`].

use std::sync::Arc;

use peerbeam_domain::port::AppStore;

use crate::note::{Note, NoteError};
#[cfg(test)]
use crate::note::{MAX_BODY, MAX_TITLE};

/// The single namespace every note lives in.
///
/// One namespace, not one per peer: a note belongs to the person, not to a
/// conversation. Sync copies the same set to whichever devices have been
/// granted the notes permission, so partitioning by peer would mean storing the
/// same note once per device and having to reconcile the copies with each other.
pub const NS: &str = "notes";

/// Notes, persisted and encrypted at rest by the [`AppStore`] beneath.
#[derive(Clone)]
pub struct NoteStore {
    store: Arc<dyn AppStore>,
}

impl NoteStore {
    #[must_use]
    pub fn new(store: Arc<dyn AppStore>) -> Self {
        NoteStore { store }
    }

    /// Write a note, replacing any note with the same id.
    pub fn put(&self, note: &Note) -> Result<(), NoteError> {
        let bytes =
            serde_json::to_vec(note).map_err(|e| NoteError::Serialization(e.to_string()))?;
        self.store
            .put(NS, &note.id, &bytes)
            .map_err(|e| NoteError::Storage(e.to_string()))
    }

    /// One note by id, tombstones included — a caller reconciling with a peer
    /// needs to see a deletion, and [`list`](Self::list) deliberately hides it.
    pub fn get(&self, id: &str) -> Result<Option<Note>, NoteError> {
        let Some(bytes) = self
            .store
            .get(NS, id)
            .map_err(|e| NoteError::Storage(e.to_string()))?
        else {
            return Ok(None);
        };
        match serde_json::from_slice(&bytes) {
            Ok(n) => Ok(Some(n)),
            // A row this build cannot read is skipped rather than fatal, the
            // same containment `ChatStore::history` applies: one unreadable
            // note must not make every note unreachable.
            Err(_) => Ok(None),
        }
    }

    /// Every live note, newest edit first. Tombstones are **not** included:
    /// they exist to be sent to peers, not to be read as notes.
    pub fn list(&self) -> Result<Vec<Note>, NoteError> {
        let mut notes = self.all()?;
        notes.retain(|n| !n.deleted);
        notes.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
        Ok(notes)
    }

    /// Every note including tombstones, in no particular order. The set a sync
    /// exchange works from.
    pub fn all(&self) -> Result<Vec<Note>, NoteError> {
        let rows = self
            .store
            .list(NS)
            .map_err(|e| NoteError::Storage(e.to_string()))?;
        // An undecodable row is skipped, never fatal.
        Ok(rows
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_slice::<Note>(&v).ok())
            .collect())
    }

    /// Replace a note's content, keeping its id and stamping it now.
    ///
    /// Returns `false` when there is no such note, or when it has been deleted:
    /// editing a tombstone would resurrect it without the user asking, and a
    /// deletion that a peer already saw would then have to be undone there too.
    pub fn edit(&self, id: &str, title: &str, body: &str) -> Result<bool, NoteError> {
        let Some(existing) = self.get(id)? else {
            return Ok(false);
        };
        if existing.deleted {
            return Ok(false);
        }
        self.put(&Note::at(id.to_string(), title, body)?)?;
        Ok(true)
    }

    /// Delete a note by leaving a tombstone in its place.
    ///
    /// The row stays so the deletion can reach a peer that has not seen it. Its
    /// text does not: a tombstone keeps the id and the time and nothing else,
    /// because retaining what the user deleted — purely to help a sync
    /// algorithm — is the wrong trade.
    ///
    /// Returns whether anything changed. Deleting an already-deleted note is a
    /// no-op rather than a fresh tombstone, so a repeat cannot bump the
    /// timestamp and win a conflict it should have lost.
    pub fn delete(&self, id: &str) -> Result<bool, NoteError> {
        let Some(existing) = self.get(id)? else {
            return Ok(false);
        };
        if existing.deleted {
            return Ok(false);
        }
        self.put(&existing.tombstone())?;
        Ok(true)
    }

    /// Take a note from a peer, keeping whichever version wins.
    ///
    /// Returns whether local storage changed, so a caller can avoid emitting an
    /// event for a no-op. A note we have never seen is always taken — including
    /// a tombstone, which is how a deletion made elsewhere reaches this device.
    pub fn merge(&self, incoming: &Note) -> Result<bool, NoteError> {
        match self.get(&incoming.id)? {
            None => {
                self.put(incoming)?;
                Ok(true)
            }
            Some(mine) => {
                if Note::wins(&mine, incoming) == incoming && mine != *incoming {
                    self.put(incoming)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::Note;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::port::EncryptionProvider;

    fn new_store() -> (NoteStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[7u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        (NoteStore::new(app), dir)
    }

    /// A note stamped at `at`, for tests that need to control the clock the
    /// resolver reads.
    fn at(id: &str, body: &str, at: &str, deleted: bool) -> Note {
        Note {
            id: id.to_string(),
            title: String::new(),
            body: body.to_string(),
            updated_at: at.to_string(),
            deleted,
        }
    }

    #[test]
    fn a_note_survives_a_reopen() {
        let (ns, dir) = new_store();
        let n = Note::new("Title", "body").unwrap();
        ns.put(&n).unwrap();
        drop(ns);

        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[7u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        let reopened = NoteStore::new(app);
        assert_eq!(reopened.list().unwrap().len(), 1);
        assert_eq!(reopened.list().unwrap()[0].body, "body");
    }

    #[test]
    fn deleting_leaves_a_tombstone_that_list_hides_and_sync_can_see() {
        // The whole reason a delete is not a removal: a row that simply
        // vanished is indistinguishable from one a peer has not seen, and the
        // next exchange would resurrect it.
        let (ns, _dir) = new_store();
        let n = Note::new("t", "secret").unwrap();
        ns.put(&n).unwrap();

        assert!(ns.delete(&n.id).unwrap());
        assert!(
            ns.list().unwrap().is_empty(),
            "a deleted note is not listed"
        );

        let stone = ns.get(&n.id).unwrap().expect("the tombstone remains");
        assert!(stone.deleted);
        assert_eq!(stone.body, "", "a tombstone must not keep what was deleted");
        assert_eq!(stone.title, "");
    }

    #[test]
    fn deleting_twice_does_not_restamp_the_tombstone() {
        // A repeat must not bump the timestamp: that would let an idle device
        // win a conflict it should have lost.
        let (ns, _dir) = new_store();
        let n = Note::new("t", "b").unwrap();
        ns.put(&n).unwrap();
        assert!(ns.delete(&n.id).unwrap());
        let first = ns.get(&n.id).unwrap().unwrap().updated_at;

        assert!(
            !ns.delete(&n.id).unwrap(),
            "a second delete changed something"
        );
        assert_eq!(ns.get(&n.id).unwrap().unwrap().updated_at, first);
    }

    #[test]
    fn editing_a_deleted_note_does_not_resurrect_it() {
        let (ns, _dir) = new_store();
        let n = Note::new("t", "b").unwrap();
        ns.put(&n).unwrap();
        ns.delete(&n.id).unwrap();

        assert!(!ns.edit(&n.id, "t", "back again").unwrap());
        assert!(ns.get(&n.id).unwrap().unwrap().deleted);
        assert!(ns.list().unwrap().is_empty());
    }

    #[test]
    fn a_later_edit_wins_and_an_earlier_one_is_ignored() {
        let (ns, _dir) = new_store();
        ns.put(&at("n1", "mine", "2026-01-01T00:00:05Z", false))
            .unwrap();

        assert!(
            !ns.merge(&at("n1", "stale", "2026-01-01T00:00:01Z", false))
                .unwrap(),
            "an older edit overwrote a newer one"
        );
        assert_eq!(ns.get("n1").unwrap().unwrap().body, "mine");

        assert!(ns
            .merge(&at("n1", "theirs", "2026-01-01T00:00:09Z", false))
            .unwrap());
        assert_eq!(ns.get("n1").unwrap().unwrap().body, "theirs");
    }

    #[test]
    fn a_tie_is_broken_toward_deletion() {
        // Two devices, same second, one deleted and one edited. Resolving
        // toward the edit would bring back a note the user deleted, which is
        // the more surprising of the two outcomes.
        let (ns, _dir) = new_store();
        ns.put(&at("n1", "edited", "2026-01-01T00:00:05Z", false))
            .unwrap();

        assert!(ns
            .merge(&at("n1", "", "2026-01-01T00:00:05Z", true))
            .unwrap());
        assert!(ns.get("n1").unwrap().unwrap().deleted);
    }

    #[test]
    fn a_note_deleted_here_is_not_resurrected_by_a_same_second_edit() {
        // The direction that actually bites. With local = deleted and incoming
        // = an edit at the same instant, a tie resolved toward the edit brings
        // back a note the user deleted. Testing only the mirror case (local
        // edit, incoming deletion) proves nothing: there the deletion wins
        // either way, because it is the incoming side.
        let (ns, _dir) = new_store();
        ns.put(&at("n1", "", "2026-01-01T00:00:05Z", true)).unwrap();

        assert!(
            !ns.merge(&at("n1", "back again", "2026-01-01T00:00:05Z", false))
                .unwrap(),
            "a same-second edit overwrote a deletion"
        );
        let stored = ns.get("n1").unwrap().unwrap();
        assert!(stored.deleted, "the note came back from the dead");
        assert_eq!(stored.body, "");
    }

    #[test]
    fn a_deletion_from_a_peer_arrives_even_for_a_note_we_never_had() {
        // Otherwise a device that was offline for the whole life of a note
        // would take the tombstone as an unknown id and drop it, then offer the
        // note back to everyone else.
        let (ns, _dir) = new_store();
        assert!(ns
            .merge(&at("n1", "", "2026-01-01T00:00:00Z", true))
            .unwrap());
        assert!(ns.get("n1").unwrap().unwrap().deleted);
        assert!(ns.list().unwrap().is_empty());
    }

    #[test]
    fn merging_an_identical_note_reports_no_change() {
        let (ns, _dir) = new_store();
        let n = at("n1", "same", "2026-01-01T00:00:05Z", false);
        ns.put(&n).unwrap();
        assert!(!ns.merge(&n).unwrap(), "an identical note reported a write");
    }

    #[test]
    fn an_undecodable_row_is_skipped_rather_than_hiding_every_note() {
        let (ns, dir) = new_store();
        ns.put(&Note::new("t", "readable").unwrap()).unwrap();

        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[7u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        app.put(NS, "junk", b"not json at all").unwrap();

        let listed = NoteStore::new(app).list().unwrap();
        assert_eq!(listed.len(), 1, "one bad row hid the readable notes");
        assert_eq!(listed[0].body, "readable");
    }

    #[test]
    fn oversized_content_is_refused_rather_than_truncated() {
        assert!(matches!(
            Note::new("t", &"x".repeat(MAX_BODY + 1)),
            Err(NoteError::BodyTooLarge { .. })
        ));
        assert!(matches!(
            Note::new(&"t".repeat(MAX_TITLE + 1), "b"),
            Err(NoteError::TitleTooLarge { .. })
        ));
    }

    #[test]
    fn a_tombstone_is_visible_to_a_second_store_over_the_same_directory() {
        // The CLI opens a fresh store per command, so a deletion written by one
        // instance must be seen by the next. If it were not, `notes edit` would
        // happily resurrect a note the previous command deleted.
        let (ns, dir) = new_store();
        let n = Note::new("t", "b").unwrap();
        ns.put(&n).unwrap();
        assert!(ns.delete(&n.id).unwrap());

        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[7u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        let second = NoteStore::new(app);
        assert!(
            second
                .get(&n.id)
                .unwrap()
                .expect("the row is there")
                .deleted,
            "a second store did not see the tombstone"
        );
        assert!(!second.edit(&n.id, "t", "back").unwrap());
    }
}
