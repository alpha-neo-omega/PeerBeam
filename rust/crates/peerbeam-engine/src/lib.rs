//! PeerBeam engine — the composition root.
//!
//! This is the single place where configuration, providers, and the async
//! runtime meet. Frontends (Flutter FFI, CLI, daemon) construct an
//! [`Engine`] via [`EngineBuilder`], register the providers they want, and
//! interact only through the resulting handle and its event stream. They
//! never see tokio, sockets, or a concrete provider.
//!
//! Discovery is wired through the [`DeviceManager`], which the [`Engine`]
//! owns; transfer use-cases attach on top of the same seam.

mod builder;
mod device_manager;
mod engine;
mod error;
mod route_classifier;
mod route_manager;

pub use builder::EngineBuilder;
pub use device_manager::DeviceManager;
pub use engine::Engine;
pub use error::EngineError;
pub use route_classifier::{AddressClassifier, RouteClassifier};
pub use route_manager::{RouteLinkFactory, RouteManager};

// Re-export the pieces frontends need so they depend on one crate.
pub use peerbeam_app::ProviderRegistry;
pub use peerbeam_config::EngineConfig;
pub use peerbeam_domain::entity::{DeviceCapabilities, ManagedDevice};
pub use peerbeam_domain::event::{DeviceChange, DomainEvent};
