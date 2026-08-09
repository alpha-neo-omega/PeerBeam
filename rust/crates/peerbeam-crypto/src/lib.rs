//! Cryptographic adapter implementing [`EncryptionProvider`].
//!
//! - **Key agreement** — X25519 ECDH. Session keys are *directional*: which
//!   of the two derived keys is "send" vs "recv" is decided by comparing the
//!   two public keys, so both peers agree without a negotiated role.
//! - **Sealing** — AES-256-GCM. Output is `nonce (12) || ciphertext+tag`; the
//!   GCM tag authenticates the data (tamper detection). The caller supplies
//!   the nonce and must never reuse one under the same key.
//! - **Fingerprint** — SHA-256 of the public key, hex, for out-of-band /
//!   trust-on-first-use verification.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce as GcmNonce};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{
    EncryptionProvider, Fingerprint, KeyPair, Nonce, PublicKey, SecretKey, SessionKeys,
};

/// X25519 + AES-256-GCM implementation of [`EncryptionProvider`].
#[derive(Debug, Default, Clone)]
pub struct AeadCrypto;

impl AeadCrypto {
    /// Create the crypto provider.
    pub fn new() -> Self {
        Self
    }
}

impl EncryptionProvider for AeadCrypto {
    fn generate_keypair(&self) -> KeyPair {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = XPublicKey::from(&secret);
        KeyPair {
            public: PublicKey(public.to_bytes()),
            secret: SecretKey(secret.to_bytes()),
        }
    }

    fn key_exchange(&self, ours: &SecretKey, theirs: &PublicKey) -> Result<SessionKeys> {
        let secret = StaticSecret::from(ours.0);
        let our_public = XPublicKey::from(&secret).to_bytes();
        let shared_secret = secret.diffie_hellman(&XPublicKey::from(theirs.0));
        // A peer presenting a low-order public key (e.g. all-zero) forces the
        // ECDH result to a publicly-known constant regardless of our private
        // key, letting anyone who observes the (cleartext) handshake nonces
        // recompute the session keys. Reject non-contributory results instead
        // of feeding them to the KDF.
        if !shared_secret.was_contributory() {
            return Err(DomainError::Encryption(
                "key exchange produced a non-contributory (low-order) shared secret".into(),
            ));
        }
        let shared = shared_secret.to_bytes();

        let lo = kdf(&shared, b"peerbeam-key-lo");
        let hi = kdf(&shared, b"peerbeam-key-hi");
        // Deterministic direction: the peer with the smaller public key sends
        // with `lo`; the other sends with `hi`. Both compute both keys.
        let (send, recv) = if our_public < theirs.0 {
            (lo, hi)
        } else {
            (hi, lo)
        };
        Ok(SessionKeys { send, recv })
    }

    fn seal(&self, key: &[u8; 32], nonce: &Nonce, plaintext: &[u8]) -> Result<Vec<u8>> {
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| DomainError::Encryption(format!("cipher init: {e}")))?;
        let ciphertext = cipher
            .encrypt(GcmNonce::from_slice(&nonce.0), plaintext)
            .map_err(|_| DomainError::Encryption("seal failed".into()))?;
        let mut out = Vec::with_capacity(12 + ciphertext.len());
        out.extend_from_slice(&nonce.0);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    fn open(&self, key: &[u8; 32], ciphertext: &[u8]) -> Result<Vec<u8>> {
        if ciphertext.len() < 12 {
            return Err(DomainError::Encryption("ciphertext too short".into()));
        }
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| DomainError::Encryption(format!("cipher init: {e}")))?;
        let nonce = GcmNonce::from_slice(&ciphertext[..12]);
        cipher
            .decrypt(nonce, &ciphertext[12..])
            .map_err(|_| DomainError::Encryption("open failed (bad tag or key)".into()))
    }

    fn fingerprint(&self, public: &PublicKey) -> Fingerprint {
        let mut hasher = Sha256::new();
        hasher.update(public.0);
        Fingerprint(to_hex(&hasher.finalize()))
    }

    fn pairing_code(&self, a: &PublicKey, b: &PublicKey) -> String {
        // Canonical order so both peers hash identical bytes regardless of who
        // is "a" or "b".
        let (lo, hi) = if a.0 <= b.0 {
            (&a.0, &b.0)
        } else {
            (&b.0, &a.0)
        };
        let mut hasher = Sha256::new();
        hasher.update(b"peerbeam-pairing-v1");
        hasher.update(lo);
        hasher.update(hi);
        let digest = hasher.finalize();
        // 128 bits -> uppercase hex -> eight groups of four.
        let hex = to_hex_upper(&digest[..16]);
        let mut out = String::with_capacity(39);
        for (i, ch) in hex.chars().enumerate() {
            if i != 0 && i % 4 == 0 {
                out.push(' ');
            }
            out.push(ch);
        }
        out
    }
}

fn kdf(shared: &[u8], label: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(shared);
    hasher.update(label);
    hasher.finalize().into()
}

/// Derive a 32-byte subkey from high-entropy input key material `ikm` and a
/// domain-separation `label`. Used to derive an at-rest data key (e.g. the
/// AppStore key) from the device identity's X25519 secret. The secret is already
/// 32 bytes of high-entropy material, so `SHA-256(ikm ‖ label)` is sufficient;
/// `label` domain-separates it from any other use of the same secret.
#[must_use]
pub fn derive_subkey(ikm: &[u8], label: &[u8]) -> [u8; 32] {
    kdf(ikm, label)
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn to_hex_upper(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02X}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_exchange_agrees_and_is_directional() {
        let c = AeadCrypto::new();
        let a = c.generate_keypair();
        let b = c.generate_keypair();

        let a_keys = c.key_exchange(&a.secret, &b.public).unwrap();
        let b_keys = c.key_exchange(&b.secret, &a.public).unwrap();

        // A's send == B's recv, and vice-versa (directional agreement).
        assert_eq!(a_keys.send, b_keys.recv);
        assert_eq!(a_keys.recv, b_keys.send);
        // Directions differ.
        assert_ne!(a_keys.send, a_keys.recv);
    }

    /// A peer presenting a low-order X25519 public key (the all-zero point is
    /// the simplest of the small-order points) forces the shared secret to a
    /// publicly-known constant. `key_exchange` must reject it rather than
    /// silently deriving session keys anyone could recompute.
    #[test]
    fn key_exchange_rejects_low_order_peer_public_key() {
        let c = AeadCrypto::new();
        let ours = c.generate_keypair();
        let low_order_peer = PublicKey([0u8; 32]);

        let result = c.key_exchange(&ours.secret, &low_order_peer);
        assert!(
            matches!(result, Err(DomainError::Encryption(_))),
            "all-zero peer pubkey must be rejected"
        );
    }

    #[test]
    fn seal_open_roundtrip() {
        let c = AeadCrypto::new();
        let key = [7u8; 32];
        let nonce = Nonce([1u8; 12]);
        let ct = c.seal(&key, &nonce, b"secret payload").unwrap();
        assert_ne!(&ct[12..], b"secret payload");
        assert_eq!(c.open(&key, &ct).unwrap(), b"secret payload");
    }

    #[test]
    fn open_rejects_tamper_and_wrong_key() {
        let c = AeadCrypto::new();
        let key = [7u8; 32];
        let nonce = Nonce([2u8; 12]);
        let mut ct = c.seal(&key, &nonce, b"data").unwrap();

        // Flip a ciphertext byte → GCM tag fails.
        let last = ct.len() - 1;
        ct[last] ^= 0x01;
        assert!(c.open(&key, &ct).is_err());

        // Wrong key → fails.
        let ct2 = c.seal(&key, &nonce, b"data").unwrap();
        assert!(c.open(&[8u8; 32], &ct2).is_err());
    }

    #[test]
    fn open_rejects_short_input() {
        let c = AeadCrypto::new();
        assert!(c.open(&[0u8; 32], &[1, 2, 3]).is_err());
    }

    #[test]
    fn fingerprint_is_stable_and_distinct() {
        let c = AeadCrypto::new();
        let a = c.generate_keypair();
        let b = c.generate_keypair();
        assert_eq!(c.fingerprint(&a.public), c.fingerprint(&a.public));
        assert_ne!(c.fingerprint(&a.public), c.fingerprint(&b.public));
        assert_eq!(c.fingerprint(&a.public).0.len(), 64);
    }

    #[test]
    fn derive_subkey_is_stable_distinct_and_domain_separated() {
        let ikm_a = [7u8; 32];
        let ikm_b = [8u8; 32];
        // Stable: same inputs -> same key.
        assert_eq!(
            derive_subkey(&ikm_a, b"peerbeam-appstore-v1"),
            derive_subkey(&ikm_a, b"peerbeam-appstore-v1"),
        );
        // Distinct ikm -> distinct key.
        assert_ne!(
            derive_subkey(&ikm_a, b"peerbeam-appstore-v1"),
            derive_subkey(&ikm_b, b"peerbeam-appstore-v1"),
        );
        // Domain separation: same ikm, different label -> distinct key.
        assert_ne!(
            derive_subkey(&ikm_a, b"peerbeam-appstore-v1"),
            derive_subkey(&ikm_a, b"peerbeam-other-v1"),
        );
    }

    #[test]
    fn pairing_code_is_stable_order_independent_and_distinct() {
        let c = AeadCrypto::new();
        let a = c.generate_keypair();
        let b = c.generate_keypair();

        // Stable: same pair -> same code.
        assert_eq!(
            c.pairing_code(&a.public, &b.public),
            c.pairing_code(&a.public, &b.public),
        );
        // Order-independent: canonical ordering means swap yields the same code.
        assert_eq!(
            c.pairing_code(&a.public, &b.public),
            c.pairing_code(&b.public, &a.public),
        );
        // Distinct: a different second key yields a different code.
        let d = c.generate_keypair();
        assert_ne!(
            c.pairing_code(&a.public, &b.public),
            c.pairing_code(&a.public, &d.public),
        );
    }

    #[test]
    fn pairing_code_format_is_eight_groups_of_four_uppercase_hex() {
        let c = AeadCrypto::new();
        let a = c.generate_keypair();
        let b = c.generate_keypair();
        let code = c.pairing_code(&a.public, &b.public);

        assert_eq!(code.len(), 39, "32 hex + 7 spaces");
        let groups: Vec<&str> = code.split(' ').collect();
        assert_eq!(groups.len(), 8);
        for g in groups {
            assert_eq!(g.len(), 4);
            assert!(
                g.chars()
                    .all(|ch| ch.is_ascii_digit() || ('A'..='F').contains(&ch)),
                "uppercase hex only, got {g}"
            );
        }
    }

    /// The check must actually detect a substituted key: a MITM sits between A
    /// and B with its own key M, so A sees (A,M) and B sees (M,B). Those codes
    /// must differ, or the two users would see matching codes and be fooled.
    #[test]
    fn pairing_code_detects_a_mitm_substitution() {
        let c = AeadCrypto::new();
        let a = c.generate_keypair();
        let b = c.generate_keypair();
        let m = c.generate_keypair();

        let a_sees = c.pairing_code(&a.public, &m.public);
        let b_sees = c.pairing_code(&m.public, &b.public);
        assert_ne!(a_sees, b_sees);
    }
}
