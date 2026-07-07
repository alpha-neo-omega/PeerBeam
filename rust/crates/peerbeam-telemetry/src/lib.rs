//! Telemetry: structured logging initialization.
//!
//! A frontend (CLI, daemon, or the FFI layer) calls [`init`] once at
//! startup. The engine and libraries only emit `tracing` spans/events;
//! they never configure a subscriber themselves.

use peerbeam_config::LogConfig;
use tracing_subscriber::EnvFilter;

/// Initialize the global logging subscriber from configuration.
///
/// Idempotent and safe to call more than once: a second call is a no-op
/// because the global subscriber can only be set once. Honours the
/// `RUST_LOG` environment variable when set, otherwise falls back to the
/// filter in [`LogConfig`].
pub fn init(config: &LogConfig) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&config.filter))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(config.show_target);

    // `try_init` returns Err if a subscriber is already installed; that is
    // an expected, harmless condition for a shared library entry point.
    let _ = if config.json {
        builder.json().try_init()
    } else {
        builder.try_init()
    };
}
