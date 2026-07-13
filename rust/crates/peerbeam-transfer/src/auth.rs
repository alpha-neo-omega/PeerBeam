//! Mutual authentication handshake, run once at the start of a connection.
//!
//! An authenticated X25519 key agreement with trust-on-first-use:
//!
//! ```text
//! A→B: Hello{ device_id, name, pubkey_A, nonce_A }
//! B→A: Hello{ device_id, name, pubkey_B, nonce_B }
//! A→B: Confirm{ HMAC(send_key, transcript) }
//! B→A: Confirm{ HMAC(send_key, transcript) }
//! ```
//!
//! Both sides compute the same ECDH shared secret and derive **directional**
//! session keys. The `Confirm` MAC is *key confirmation*: verifying the
//! peer's MAC (with our receive key) proves the peer derived the same secret,
//! i.e. it holds the private key matching the public key it presented — this
//! is the mutual-authentication step. The transcript binds both public keys
//! and both fresh nonces, so a replayed handshake yields different keys.
//!
//! **TOFU trust**: the peer's public-key fingerprint is pinned on first
//! contact via the [`TrustStore`]; on later connections a changed fingerprint
//! (new device reusing the id, or a MITM) is rejected.

use chrono::Utc;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use peerbeam_domain::entity::TrustRecord;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{
    EncryptionProvider, Frame, FrameKind, KeyPair, Link, PublicKey, TrustStore,
};

type HmacSha256 = Hmac<Sha256>;

/// This device's stable identity for authentication.
#[derive(Clone)]
pub struct Identity {
    /// Stable device id (also used as the trust-store key).
    pub device_id: DeviceId,
    /// Human-friendly name presented to peers.
    pub name: String,
    /// Long-term X25519 identity keypair.
    pub keypair: KeyPair,
}

/// The authenticated session produced by a successful handshake. Consumed by
/// [`crate::SecureLink`] to seal/open data frames.
#[derive(Clone)]
pub struct Session {
    pub(crate) send_key: [u8; 32],
    pub(crate) recv_key: [u8; 32],
    pub(crate) send_prefix: [u8; 4],
    pub(crate) recv_prefix: [u8; 4],
    /// The authenticated peer's device id.
    pub peer_id: DeviceId,
    /// Whether the peer was newly pinned (true) or already trusted (false).
    pub newly_trusted: bool,
}

#[derive(Serialize, Deserialize)]
enum AuthMsg {
    Hello {
        device_id: String,
        name: String,
        pubkey: [u8; 32],
        nonce: [u8; 16],
    },
    Confirm {
        tag: Vec<u8>,
    },
}

fn auth_frame(msg: &AuthMsg) -> Frame {
    Frame {
        kind: FrameKind::Handshake,
        payload: bytes::Bytes::from(serde_json::to_vec(msg).expect("AuthMsg serializable")),
    }
}

async fn recv_auth(link: &mut dyn Link) -> Result<AuthMsg> {
    loop {
        match link.recv_frame().await? {
            Some(frame) if frame.kind == FrameKind::Handshake => {
                return serde_json::from_slice(&frame.payload)
                    .map_err(|e| DomainError::Encryption(format!("bad auth message: {e}")));
            }
            Some(_) => continue,
            None => {
                return Err(DomainError::Encryption(
                    "link closed during handshake".into(),
                ))
            }
        }
    }
}

/// Perform the mutual-authentication handshake over `link`, returning an
/// authenticated [`Session`]. Symmetric: both peers call this.
pub async fn authenticate(
    link: &mut dyn Link,
    identity: &Identity,
    enc: &dyn EncryptionProvider,
    trust: &dyn TrustStore,
) -> Result<Session> {
    let mut our_nonce = [0u8; 16];
    OsRng.fill_bytes(&mut our_nonce);
    let our_pub = identity.keypair.public.0;

    link.send_frame(auth_frame(&AuthMsg::Hello {
        device_id: identity.device_id.0.clone(),
        name: identity.name.clone(),
        pubkey: our_pub,
        nonce: our_nonce,
    }))
    .await?;

    let (peer_device, peer_name, peer_pub, peer_nonce) = match recv_auth(link).await? {
        AuthMsg::Hello {
            device_id,
            name,
            pubkey,
            nonce,
        } => (device_id, name, pubkey, nonce),
        _ => return Err(DomainError::Encryption("expected Hello".into())),
    };

    let keys = enc.key_exchange(&identity.keypair.secret, &PublicKey(peer_pub))?;
    let transcript = transcript(&our_pub, &our_nonce, &peer_pub, &peer_nonce);

    // Key confirmation.
    let our_tag = hmac(&keys.send, &transcript);
    link.send_frame(auth_frame(&AuthMsg::Confirm { tag: our_tag }))
        .await?;

    let peer_tag = match recv_auth(link).await? {
        AuthMsg::Confirm { tag } => tag,
        _ => return Err(DomainError::Encryption("expected Confirm".into())),
    };
    let expected = hmac(&keys.recv, &transcript);
    if !constant_time_eq(&peer_tag, &expected) {
        return Err(DomainError::Encryption(
            "authentication failed: key confirmation mismatch".into(),
        ));
    }

    // Trust-on-first-use.
    let peer_id = DeviceId::from(peer_device);
    let fingerprint = enc.fingerprint(&PublicKey(peer_pub)).0;
    let newly_trusted = match trust.lookup(&peer_id)? {
        Some(rec) if rec.fingerprint != fingerprint => {
            return Err(DomainError::Encryption(format!(
                "peer {} key changed since it was trusted (possible MITM)",
                peer_id.0
            )));
        }
        Some(_) => false,
        None => {
            trust.record(TrustRecord {
                device: peer_id.clone(),
                fingerprint,
                name: peer_name,
                trusted_at: Utc::now(),
            })?;
            true
        }
    };

    // Derive fresh data keys + nonce prefixes bound to this handshake.
    let send_key = kdf(&keys.send, &transcript);
    let recv_key = kdf(&keys.recv, &transcript);
    Ok(Session {
        send_prefix: prefix(&kdf(&send_key, b"peerbeam-nonce")),
        recv_prefix: prefix(&kdf(&recv_key, b"peerbeam-nonce")),
        send_key,
        recv_key,
        peer_id,
        newly_trusted,
    })
}

/// Canonical transcript: order the two (pubkey, nonce) pairs by public key so
/// both peers hash identical bytes regardless of who spoke first.
fn transcript(
    a_pub: &[u8; 32],
    a_nonce: &[u8; 16],
    b_pub: &[u8; 32],
    b_nonce: &[u8; 16],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 * (32 + 16));
    let (first, first_n, second, second_n) = if a_pub <= b_pub {
        (a_pub, a_nonce, b_pub, b_nonce)
    } else {
        (b_pub, b_nonce, a_pub, a_nonce)
    };
    out.extend_from_slice(first);
    out.extend_from_slice(first_n);
    out.extend_from_slice(second);
    out.extend_from_slice(second_n);
    out
}

fn hmac(key: &[u8; 32], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn kdf(key: &[u8], label: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(key);
    hasher.update(label);
    hasher.finalize().into()
}

fn prefix(bytes: &[u8; 32]) -> [u8; 4] {
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

/// Constant-time comparison to avoid MAC-verification timing leaks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
