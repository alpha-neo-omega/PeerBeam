//! The single typed error surface for the domain and every port.

use thiserror::Error;

/// All errors that domain operations and port implementations may return.
///
/// Concrete adapters map their own failures (mDNS, QUIC, aes-gcm, io, …)
/// into these variants so the application layer never depends on a
/// provider-specific error type.
#[derive(Error, Debug)]
pub enum DomainError {
    /// Device discovery failed.
    #[error("discovery: {0}")]
    Discovery(String),

    /// A transfer failed at the protocol/session level.
    #[error("transfer: {0}")]
    Transfer(String),

    /// Establishing or maintaining a connection failed.
    #[error("connection: {0}")]
    Connection(String),

    /// Route candidates could not be produced or probed.
    #[error("route: {0}")]
    Route(String),

    /// Key exchange, sealing, or opening failed.
    #[error("encryption: {0}")]
    Encryption(String),

    /// Compression or decompression failed.
    #[error("compression: {0}")]
    Compression(String),

    /// A checksum/integrity check failed.
    #[error("integrity: {0}")]
    Integrity(String),

    /// A storage (filesystem/persistence) operation failed.
    #[error("storage: {0}")]
    Storage(String),

    /// The requested entity does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// The operation is not supported by the available providers.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// Configuration was invalid.
    #[error("config: {0}")]
    Config(String),

    /// The operation was cancelled by the user or a peer.
    #[error("cancelled")]
    Cancelled,
}

/// Convenience result alias used throughout the domain and its ports.
pub type Result<T> = std::result::Result<T, DomainError>;
