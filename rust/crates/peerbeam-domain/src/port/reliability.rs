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

    /// Byte offset a transfer can safely resume from.
    fn resumable_offset(&self, transfer: &TransferId) -> Result<u64>;
}
