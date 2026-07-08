//! Application layer.
//!
//! Holds the dependency-injection container ([`ProviderRegistry`]) and,
//! over time, the use-cases that orchestrate the domain ports. It depends
//! *only* on `peerbeam-domain` traits — never on a concrete provider,
//! never on a runtime, never on a frontend. Use-cases are written against
//! the registry, so they are unit-testable with mock providers.

pub mod device_store;
pub mod discovery;
pub mod registry;

pub use device_store::DeviceStore;
pub use discovery::{merge_discovery, DiscoveryRegistry};
pub use registry::ProviderRegistry;
