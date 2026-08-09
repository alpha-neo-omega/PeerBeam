//! Identity port: the device's persisted long-term identity (keypair + id).

use crate::entity::StoredIdentity;
use crate::error::Result;

/// Loads and persists this device's stable identity. Implemented by an infra
/// adapter (e.g. a JSON file); the frontends generate-once then reuse it.
pub trait IdentityStore: Send + Sync {
    /// Load the stored identity, or `Ok(None)` if none exists yet (first run).
    /// A present-but-unreadable store is an `Err`, never a silent `None` —
    /// silently regenerating would break every peer's trust pin.
    fn load(&self) -> Result<Option<StoredIdentity>>;

    /// Persist `identity`, replacing any previous one.
    fn save(&self, identity: &StoredIdentity) -> Result<()>;
}
