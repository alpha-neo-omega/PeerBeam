//! Filesystem [`ReliabilityStore`].
//!
//! Computes SHA-256 checksums and persists per-transfer checkpoints as JSON
//! files (`<dir>/<transfer_id>.json`). Persistence is what lets a transfer
//! survive a process restart: on relaunch the checkpoint says which transfer
//! was in flight and how far it got, so it can be resumed rather than
//! restarted.

use std::path::PathBuf;

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
        std::fs::write(&path, json)
            .map_err(|e| DomainError::Storage(format!("write checkpoint: {e}")))
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
