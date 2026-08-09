//! Load the device's persistent identity, generating it once on first run.

use peerbeam_domain::entity::{device_id_from_fingerprint, StoredIdentity};
use peerbeam_domain::error::Result;
use peerbeam_domain::port::{EncryptionProvider, IdentityStore, KeyPair};

use crate::auth::Identity;

/// Return this device's stable [`Identity`]: load it from `store`, or — on first
/// run — generate a keypair, derive the device id from its fingerprint, persist
/// it, and return it. `name` is the human-facing device name (from config), not
/// part of the stored identity.
pub fn load_or_generate(
    store: &dyn IdentityStore,
    enc: &dyn EncryptionProvider,
    name: String,
) -> Result<Identity> {
    if let Some(stored) = store.load()? {
        return Ok(Identity {
            device_id: stored.device_id,
            name,
            keypair: KeyPair {
                public: stored.public,
                secret: stored.secret,
            },
        });
    }
    let keypair = enc.generate_keypair();
    let device_id = device_id_from_fingerprint(&enc.fingerprint(&keypair.public));
    store.save(&StoredIdentity {
        device_id: device_id.clone(),
        public: keypair.public,
        secret: keypair.secret.clone(),
    })?;
    Ok(Identity {
        device_id,
        name,
        keypair,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_crypto::AeadCrypto;
    use peerbeam_identity_fs::FsIdentity;

    #[test]
    fn generates_once_then_loads_the_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        let enc = AeadCrypto::new();

        let first = load_or_generate(&FsIdentity::open(&path), &enc, "dev".into()).expect("first");
        let second =
            load_or_generate(&FsIdentity::open(&path), &enc, "dev".into()).expect("second");

        assert_eq!(first.device_id.0, second.device_id.0, "stable device id");
        assert_eq!(
            first.keypair.public.0, second.keypair.public.0,
            "stable public key"
        );
        assert_eq!(first.keypair.secret.0, second.keypair.secret.0);
        assert!(first.device_id.0.starts_with("pb-"));
    }
}
