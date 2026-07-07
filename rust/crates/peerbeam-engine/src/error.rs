//! Engine-level errors.

use thiserror::Error;

/// Errors that can occur while building or driving the engine.
#[derive(Debug, Error)]
pub enum EngineError {
    /// A required provider was not registered before `build`.
    #[error("missing required provider: {0}")]
    MissingProvider(&'static str),

    /// A domain-level failure bubbled up.
    #[error(transparent)]
    Domain(#[from] peerbeam_domain::DomainError),

    /// A configuration failure.
    #[error(transparent)]
    Config(#[from] peerbeam_config::ConfigError),
}
