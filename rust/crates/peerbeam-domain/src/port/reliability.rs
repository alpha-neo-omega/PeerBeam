//! Reliability port: integrity and resume.

use crate::entity::TransferSession;
use crate::error::Result;
use crate::id::TransferId;

/// Checksums payloads and persists checkpoints so interrupted transfers
/// can resume from the last confirmed offset.
pub trait ReliabilityStore: Send + Sync {
    /// Compute a hex checksum of a buffer.
    fn checksum(&self, data: &[u8]) -> String;

    /// Persist a checkpoint for a session.
    fn save_checkpoint(&self, session: &TransferSession) -> Result<()>;

    /// Load a session checkpoint, if one exists.
    fn load_checkpoint(&self, transfer: &TransferId) -> Result<Option<TransferSession>>;

    /// Every checkpoint currently held, newest first.
    ///
    /// Loading one checkpoint requires already knowing its id, which is
    /// exactly what a process that has just restarted does not have: the ids
    /// died with the run that minted them. This is what lets a surface
    /// rebuild the list of interrupted transfers at startup instead of
    /// silently dropping them.
    ///
    /// An entry that cannot be read or parsed is **skipped**, not an error —
    /// one corrupt file must not cost the user every other resumable
    /// transfer.
    fn list_checkpoints(&self) -> Result<Vec<TransferSession>>;

    /// Byte offset a transfer can safely resume from.
    fn resumable_offset(&self, transfer: &TransferId) -> Result<u64>;

    /// Delete a checkpoint (called once a transfer completes successfully).
    fn clear_checkpoint(&self, transfer: &TransferId) -> Result<()>;
}
