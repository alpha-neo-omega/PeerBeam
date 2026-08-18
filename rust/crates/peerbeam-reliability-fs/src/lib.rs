//! Filesystem [`ReliabilityStore`].
//!
//! Computes SHA-256 checksums and persists per-transfer checkpoints as JSON
//! files (`<dir>/<transfer_id>.json`). Persistence is what lets a transfer
//! survive a process restart: on relaunch the checkpoint says which transfer
//! was in flight and how far it got, so it can be resumed rather than
//! restarted.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use peerbeam_domain::entity::TransferSession;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::TransferId;
use peerbeam_domain::port::ReliabilityStore;

/// A [`ReliabilityStore`] backed by a directory of JSON checkpoints.
#[derive(Debug, Clone)]
pub struct FsReliability {
    dir: PathBuf,
}

impl FsReliability {
    /// Create a store rooted at `dir` (created on first write).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path_for(&self, transfer: &TransferId) -> PathBuf {
        self.dir.join(format!("{}.json", transfer.as_str()))
    }
}

impl ReliabilityStore for FsReliability {
    fn checksum(&self, data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        to_hex(&hasher.finalize())
    }

    fn save_checkpoint(&self, session: &TransferSession) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| DomainError::Storage(format!("checkpoint dir: {e}")))?;
        let json = serde_json::to_vec_pretty(session)
            .map_err(|e| DomainError::Storage(format!("serialize checkpoint: {e}")))?;
        let path = self.path_for(&session.id);
        // Atomic write: uniquely-named temp + rename, so a crash mid-write can't
        // corrupt an existing checkpoint (which would forfeit resume), and two
        // writers to the same checkpoint can't rename the same temp away.
        let tmp = unique_tmp(&path);
        std::fs::write(&tmp, json)
            .map_err(|e| DomainError::Storage(format!("write checkpoint: {e}")))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            DomainError::Storage(format!("commit checkpoint: {e}"))
        })
    }

    fn load_checkpoint(&self, transfer: &TransferId) -> Result<Option<TransferSession>> {
        let path = self.path_for(transfer);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let session = serde_json::from_slice(&bytes)
                    .map_err(|e| DomainError::Storage(format!("parse checkpoint: {e}")))?;
                Ok(Some(session))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(DomainError::Storage(format!("read checkpoint: {e}"))),
        }
    }

    fn list_checkpoints(&self) -> Result<Vec<TransferSession>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            // No directory yet simply means no checkpoint has ever been
            // written — an empty list, not a failure.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(DomainError::Storage(format!("list checkpoints: {e}"))),
        };
        let mut out: Vec<TransferSession> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            // `.json` only: `save_checkpoint`'s uniquely-named `.tmp` files may
            // be mid-write, and a half-written temp is not a transfer.
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            // A file that cannot be read or parsed is skipped rather than
            // failing the whole listing — see the port's contract.
            match std::fs::read(&path).ok().and_then(|bytes| {
                serde_json::from_slice::<TransferSession>(&bytes)
                    .map_err(|e| tracing::warn!(path = %path.display(), error = %e, "unreadable checkpoint skipped"))
                    .ok()
            }) {
                Some(session) => out.push(session),
                None => continue,
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        Ok(out)
    }

    fn resumable_offset(&self, transfer: &TransferId) -> Result<u64> {
        Ok(self
            .load_checkpoint(transfer)?
            .map(|s| s.transferred_bytes)
            .unwrap_or(0))
    }

    fn clear_checkpoint(&self, transfer: &TransferId) -> Result<()> {
        let path = self.path_for(transfer);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DomainError::Storage(format!("clear checkpoint: {e}"))),
        }
    }
}

/// A temp path next to `path`, unique per process and per call.
fn unique_tmp(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{}.{}.tmp", std::process::id(), n));
    PathBuf::from(s)
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::entity::{Direction, TransferSession, TransferStatus};
    use peerbeam_domain::id::DeviceId;

    fn session(id: &str, transferred: u64) -> TransferSession {
        TransferSession {
            id: TransferId::from(id),
            peer: DeviceId::from("peer"),
            direction: Direction::Sending,
            status: TransferStatus::Transferring,
            files: vec![],
            total_bytes: 1000,
            transferred_bytes: transferred,
            started_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            completed_at: None,
            is_resume: false,
            accepted: true,
        }
    }

    #[test]
    fn checksum_matches_known_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsReliability::new(dir.path());
        // SHA-256("abc")
        assert_eq!(
            store.checksum(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn save_load_resume_clear_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsReliability::new(dir.path());
        let id = TransferId::from("t1");

        assert!(store.load_checkpoint(&id).unwrap().is_none());
        assert_eq!(store.resumable_offset(&id).unwrap(), 0);

        store.save_checkpoint(&session("t1", 512)).unwrap();
        let loaded = store.load_checkpoint(&id).unwrap().unwrap();
        assert_eq!(loaded.transferred_bytes, 512);
        assert_eq!(store.resumable_offset(&id).unwrap(), 512);

        store.clear_checkpoint(&id).unwrap();
        assert!(store.load_checkpoint(&id).unwrap().is_none());
        // Clearing a missing checkpoint is a no-op.
        store.clear_checkpoint(&id).unwrap();
    }

    #[test]
    fn listing_returns_every_checkpoint_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsReliability::new(dir.path());
        assert!(store.list_checkpoints().unwrap().is_empty());

        let mut old = session("older", 10);
        old.started_at = chrono::DateTime::from_timestamp(1_600_000_000, 0).unwrap();
        store.save_checkpoint(&old).unwrap();
        store.save_checkpoint(&session("newer", 20)).unwrap();

        let all = store.list_checkpoints().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id.as_str(), "newer");
        assert_eq!(all[1].id.as_str(), "older");
    }

    #[test]
    fn one_corrupt_checkpoint_does_not_cost_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsReliability::new(dir.path());
        store.save_checkpoint(&session("good", 42)).unwrap();
        std::fs::write(dir.path().join("broken.json"), b"{ not json").unwrap();
        // A stray temp from an interrupted write is not a transfer either.
        std::fs::write(dir.path().join("good.json.99.0.tmp"), b"{}").unwrap();

        let all = store.list_checkpoints().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id.as_str(), "good");
    }

    #[test]
    fn a_checkpoint_missing_the_consent_field_reads_as_not_accepted() {
        // Fail closed: a checkpoint written before `accepted` existed, or one
        // truncated/edited, must never be read as consent the user gave.
        let dir = tempfile::tempdir().unwrap();
        let store = FsReliability::new(dir.path());
        let mut json = serde_json::to_value(session("legacy", 5)).unwrap();
        json.as_object_mut().unwrap().remove("accepted");
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("legacy.json"),
            serde_json::to_vec(&json).unwrap(),
        )
        .unwrap();

        let loaded = store
            .load_checkpoint(&TransferId::from("legacy"))
            .unwrap()
            .unwrap();
        assert!(!loaded.accepted);
    }

    #[test]
    fn survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = FsReliability::new(dir.path());
            store.save_checkpoint(&session("persist", 900)).unwrap();
        }
        // A fresh store (as if after a restart) still sees the checkpoint.
        let store = FsReliability::new(dir.path());
        assert_eq!(
            store
                .resumable_offset(&TransferId::from("persist"))
                .unwrap(),
            900
        );
    }
}
