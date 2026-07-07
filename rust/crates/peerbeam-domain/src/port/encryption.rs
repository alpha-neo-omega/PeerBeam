//! Encryption port: identity keys, key exchange, and AEAD sealing.

use crate::error::Result;

/// A 32-byte X25519 public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicKey(pub [u8; 32]);

/// A 32-byte X25519 secret key.
#[derive(Clone)]
pub struct SecretKey(pub [u8; 32]);

/// An X25519 keypair.
#[derive(Clone)]
pub struct KeyPair {
    /// Public half, shared with peers.
    pub public: PublicKey,
    /// Secret half, never leaves the device.
    pub secret: SecretKey,
}

/// Directional session keys derived from a shared secret.
#[derive(Clone)]
pub struct SessionKeys {
    /// Key for data this device sends.
    pub send: [u8; 32],
    /// Key for data this device receives.
    pub recv: [u8; 32],
}

/// A 12-byte AEAD nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nonce(pub [u8; 12]);

/// A human-comparable fingerprint of a public key, shown in the UI for
/// out-of-band verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint(pub String);

/// Cryptographic operations backing secure transfers.
///
/// Kept synchronous: these are CPU-bound and callers offload to a blocking
/// pool if needed. Never returns a provider-specific error type.
pub trait EncryptionProvider: Send + Sync {
    /// Generate a fresh keypair.
    fn generate_keypair(&self) -> KeyPair;

    /// Derive directional session keys via X25519 Diffie-Hellman.
    fn key_exchange(&self, ours: &SecretKey, theirs: &PublicKey) -> Result<SessionKeys>;

    /// Seal (encrypt + authenticate) `plaintext` under `key` and `nonce`.
    fn seal(&self, key: &[u8; 32], nonce: &Nonce, plaintext: &[u8]) -> Result<Vec<u8>>;

    /// Open (verify + decrypt) `ciphertext` under `key`.
    fn open(&self, key: &[u8; 32], ciphertext: &[u8]) -> Result<Vec<u8>>;

    /// Compute a user-verifiable fingerprint of a public key.
    fn fingerprint(&self, public: &PublicKey) -> Fingerprint;
}
