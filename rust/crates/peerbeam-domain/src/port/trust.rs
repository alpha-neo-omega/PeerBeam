//! Trust port: persisted device trust (TOFU).

use crate::entity::TrustRecord;
use crate::error::Result;
use crate::id::DeviceId;

/// Stores and queries trusted-device records.
pub trait TrustStore: Send + Sync {
    /// Record (or update) trust for a device.
    fn record(&self, record: TrustRecord) -> Result<()>;

    /// Look up the trust record for a device, if any.
    fn lookup(&self, device: &DeviceId) -> Result<Option<TrustRecord>>;

    /// Convenience predicate: is this device currently trusted?
    fn is_trusted(&self, device: &DeviceId) -> bool;
}
