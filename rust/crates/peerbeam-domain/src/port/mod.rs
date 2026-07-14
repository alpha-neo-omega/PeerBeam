//! Ports — the interfaces outer layers implement.
//!
//! Each port is a trait. Infrastructure crates provide concrete adapters
//! (plugins); the application layer depends only on these traits, never on
//! a concrete implementation. This is the seam that makes the engine
//! plugin-driven and testable with mocks.

mod clipboard;
mod compression;
mod discovery;
mod encryption;
mod notification;
mod reliability;
mod route;
mod storage;
mod transfer;
mod trust;

pub use clipboard::ClipboardProvider;
pub use compression::CompressionProvider;
pub use discovery::{DiscoveryCaps, DiscoveryEvent, DiscoveryProvider};
pub use encryption::{
    EncryptionProvider, Fingerprint, KeyPair, Nonce, PublicKey, SecretKey, SessionKeys,
};
pub use notification::{Notice, NotificationSink};
pub use reliability::ReliabilityStore;
pub use route::RouteProvider;
pub use storage::StorageProvider;
pub use transfer::{
    Bind, Frame, FrameKind, Link, ProgressSink, ProgressSource, Protocol, TransferProvider,
};
pub use trust::TrustStore;
