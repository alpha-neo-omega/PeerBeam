//! PeerBeam domain layer.
//!
//! The innermost, dependency-free core of the architecture. It defines:
//!
//! - [`entity`] — the business objects (devices, transfers, routes, trust).
//! - [`port`]   — the interfaces (traits) that outer layers implement.
//! - [`event`]  — the events the engine emits to any frontend.
//! - [`error`]  — the single typed error surface.
//!
//! This crate depends on no runtime, no sockets, no Flutter, and no
//! concrete provider. Everything above it depends inward on these types;
//! nothing here depends outward. This is the dependency sink.

pub mod entity;
pub mod error;
pub mod event;
pub mod id;
pub mod port;

pub use error::{DomainError, Result};
