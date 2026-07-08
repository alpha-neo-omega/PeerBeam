//! Business entities — the nouns of the domain.
//!
//! Plain data with no behaviour that depends on IO. Serializable so they
//! can cross the FFI/IPC boundary and be persisted without translation.

mod clipboard;
mod device;
mod managed_device;
mod route;
mod transfer;
mod trust;

pub use clipboard::{ClipboardItem, ClipboardKind};
pub use device::{Device, DeviceType, Platform};
pub use managed_device::{DeviceCapabilities, ManagedDevice};
pub use route::{Route, RouteHealth, RouteKind};
pub use transfer::{Direction, FileEntry, Progress, TransferSession, TransferStatus};
pub use trust::TrustRecord;
