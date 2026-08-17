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
    ///
    /// **This means "we have pinned this device's key", not "the user chose
    /// this device".** A never-seen peer is recorded during the authenticated
    /// handshake — that pin is what makes a later key change detectable — and
    /// it is written with `approved: false`. So this answers `true` for any
    /// device that has ever completed a handshake, including a stranger on the
    /// LAN who connected once and was never accepted.
    ///
    /// Use it for MITM questions ("is this the key I saw before?"). For "may
    /// this device receive something of mine?", use [`is_approved`].
    ///
    /// [`is_approved`]: TrustStore::is_approved
    fn is_trusted(&self, device: &DeviceId) -> bool;

    /// Did the **user** deliberately grant this device standing?
    ///
    /// True only for a device the user explicitly accepted-and-trusted, which
    /// is the act that sets [`TrustRecord::approved`]. This is the predicate
    /// for any feature that sends something outward on the user's behalf
    /// without asking again — presence status, clipboard contents, an accepted
    /// pipe — because each of those is only defensible as "my own devices",
    /// and a key pinned by the handshake is not that.
    ///
    /// Fails **closed**: a store that cannot answer is not permission.
    fn is_approved(&self, device: &DeviceId) -> bool {
        matches!(self.lookup(device), Ok(Some(r)) if r.approved)
    }
}
