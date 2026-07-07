//! Transfer session entities.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::id::{DeviceId, TransferId};

/// The direction of a transfer relative to this device.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Direction {
    Sending,
    Receiving,
}

/// Lifecycle status of a transfer session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransferStatus {
    Pending,
    Connecting,
    Transferring,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// A single file participating in a transfer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    /// Local source path (sender) or intended destination (receiver).
    pub path: PathBuf,
    /// Base file name shown to the user and written to disk.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// MIME type, used for compression heuristics.
    pub mime_type: String,
    /// Whole-file checksum, if known.
    pub checksum: Option<String>,
}

/// A complete transfer session record — the unit of resume and history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferSession {
    /// Unique id of this session.
    pub id: TransferId,
    /// The other party in the transfer.
    pub peer: DeviceId,
    /// Direction relative to this device.
    pub direction: Direction,
    /// Current status.
    pub status: TransferStatus,
    /// Files in the session.
    pub files: Vec<FileEntry>,
    /// Total bytes across all files.
    pub total_bytes: u64,
    /// Bytes transferred so far (drives resume).
    pub transferred_bytes: u64,
    /// When the session started.
    pub started_at: DateTime<Utc>,
    /// When the session finished, if it has.
    pub completed_at: Option<DateTime<Utc>>,
    /// Whether this session resumed a prior interrupted one.
    pub is_resume: bool,
}

/// A progress snapshot emitted during an active transfer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Progress {
    /// The transfer this snapshot belongs to.
    pub transfer: TransferId,
    /// Direction relative to this device.
    pub direction: Direction,
    /// Current status.
    pub status: TransferStatus,
    /// Total bytes to transfer.
    pub total_bytes: u64,
    /// Bytes transferred so far.
    pub transferred_bytes: u64,
    /// Instantaneous throughput in bytes/second.
    pub speed_bps: f64,
    /// Name of the file currently in flight, if any.
    pub current_file: Option<String>,
    /// Number of files fully completed.
    pub files_completed: u32,
    /// Total number of files.
    pub files_total: u32,
    /// Estimated seconds remaining, if computable.
    pub eta_secs: Option<f64>,
}
