//! PeerBeam engine — the composition root.
//!
//! This is the single place where configuration, providers, and the async
//! runtime meet. Frontends (Flutter FFI, CLI, daemon) construct an
//! [`Engine`] via [`EngineBuilder`], register the providers they want, and
//! interact only through the resulting handle and its event stream. They
//! never see tokio, sockets, or a concrete provider.
//!
//! No transfer/discovery behaviour lives here yet — this is the wiring
//! seam. Use-cases from `peerbeam-app` attach on top of it.

mod builder;
mod engine;
mod error;

pub use builder::EngineBuilder;
pub use engine::Engine;
pub use error::EngineError;

// Re-export the pieces frontends need so they depend on one crate.
pub use peerbeam_app::ProviderRegistry;
pub use peerbeam_config::EngineConfig;
pub use peerbeam_domain::event::DomainEvent;
