//! Typed CLI errors mapped to stable exit codes so scripts can branch.

use std::fmt;

pub type CliResult = Result<(), CliError>;

#[derive(Debug)]
pub enum CliError {
    /// Bad usage / arguments (also clap's own code).
    Usage(String),
    /// A requested peer/device/file was not found.
    NotFound(String),
    /// Connection failure.
    Connection(String),
    /// Integrity/verification failure.
    Integrity(String),
    /// Cancelled by the user.
    Cancelled,
    /// No daemon is running to service the request (reserved for the daemon
    /// commands once implemented).
    #[allow(dead_code)]
    DaemonUnavailable,
    /// Feature not available yet (pending the transport bridge).
    Unavailable(String),
    /// Anything else.
    Other(String),
}

impl CliError {
    pub fn code(&self) -> i32 {
        match self {
            CliError::Other(_) => 1,
            CliError::Usage(_) => 2,
            CliError::NotFound(_) => 3,
            CliError::Connection(_) => 4,
            CliError::Integrity(_) => 5,
            CliError::Cancelled => 6,
            CliError::DaemonUnavailable => 7,
            CliError::Unavailable(_) => 8,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Usage(m) => write!(f, "{m}"),
            CliError::NotFound(m) => write!(f, "not found: {m}"),
            CliError::Connection(m) => write!(f, "connection: {m}"),
            CliError::Integrity(m) => write!(f, "integrity: {m}"),
            CliError::Cancelled => write!(f, "cancelled"),
            CliError::DaemonUnavailable => write!(f, "no PeerBeam daemon is running"),
            CliError::Unavailable(m) => write!(f, "unavailable: {m}"),
            CliError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl From<anyhow::Error> for CliError {
    fn from(e: anyhow::Error) -> Self {
        CliError::Other(e.to_string())
    }
}

impl From<peerbeam_domain::DomainError> for CliError {
    fn from(e: peerbeam_domain::DomainError) -> Self {
        use peerbeam_domain::DomainError as D;
        match e {
            D::NotFound(m) => CliError::NotFound(m),
            D::Connection(m) | D::Transfer(m) => CliError::Connection(m),
            D::Integrity(m) => CliError::Integrity(m),
            D::Cancelled => CliError::Cancelled,
            other => CliError::Other(other.to_string()),
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError::Other(e.to_string())
    }
}

impl From<peerbeam_engine::EngineError> for CliError {
    fn from(e: peerbeam_engine::EngineError) -> Self {
        use peerbeam_engine::EngineError as E;
        match e {
            E::Domain(d) => d.into(),
            other => CliError::Other(other.to_string()),
        }
    }
}
