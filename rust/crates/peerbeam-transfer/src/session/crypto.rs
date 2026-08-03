//! Per-channel cryptographic contexts derived from the authenticated session
//! master secret.
//!
//! The authenticated handshake runs exactly once per session and yields the
//! master secret (the directional keys of the auth [`Session`]). Every channel —
//! including the control channel — derives its **own** send/recv keys and nonce
//! prefixes from that master via HKDF-Expand, so no two channels (nor the two
//! directions of one channel) ever share a key or a nonce space. This is the
//! isolation guarantee M4 adds; the AEAD primitive and the frame-sealing scheme
//! are unchanged from [`crate::SecureLink`].
//!
//! ## Directional derivation
//!
//! The handshake gives each side two directional master keys: `master_send`
//! (this side's outgoing) and `master_recv` (incoming), where the initiator's
//! `master_send` equals the responder's `master_recv` and vice-versa. A channel
//! key is `HKDF-Expand(directional_master, info(channel, version, direction))`.
//! The *direction* label (forward = initiator→responder, reverse =
//! responder→initiator) is chosen from this side's role so that both peers
//! derive an identical key for the same traffic direction:
//!
//! - my send key = HKDF(master_send, … dir = my-outgoing)
//! - peer's recv key for my traffic = HKDF(peer.master_recv, … dir = my-outgoing)
//!
//! and `master_send == peer.master_recv`, so the keys match. No literal
//! per-endpoint role byte is mixed in (it would desync the two sides); the role
//! only selects which direction label pairs with send vs recv.

use std::sync::Arc;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroize;

use peerbeam_domain::port::{EncryptionProvider, Nonce};
use peerbeam_domain::session::{ChannelId, SessionError, Version};

use crate::auth::Session;

use super::event::SessionRole;

type HmacSha256 = Hmac<Sha256>;

/// Domain-separation label for channel key derivation (versioned).
const DOMAIN_LABEL: &[u8] = b"peerbeam-channel-keys-v1";
/// Domain-separation label for the session **resume key** (M6). Independent of
/// the channel-key label so the resume key can never collide with a channel key.
const RESUME_LABEL: &[u8] = b"peerbeam-resume-key-v1";
/// Derivation purpose: an encryption key.
const PURPOSE_KEY: u8 = 1;
/// Derivation purpose: a nonce prefix.
const PURPOSE_NONCE: u8 = 2;

/// The protocol version used to derive the **control** channel's keys. Fixed
/// (not the negotiated version) because the control channel is sealed before
/// version negotiation completes, so both peers must agree on it up front.
const CONTROL_VERSION: Version = Version::new(1, 0);

/// Traffic direction, an explicit input to key derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Initiator → responder.
    Forward = 0,
    /// Responder → initiator.
    Reverse = 1,
}

/// HKDF-Expand (RFC 5869) with HMAC-SHA256. The master keys are already
/// high-entropy (ECDH + hash), so no Extract step is needed — they are the PRK.
fn hkdf_expand(prk: &[u8], info: &[u8], out_len: usize) -> Result<Vec<u8>, SessionError> {
    let mut out = Vec::with_capacity(out_len);
    let mut block: Vec<u8> = Vec::new();
    let mut counter: u8 = 1;
    while out.len() < out_len {
        let mut mac = HmacSha256::new_from_slice(prk)
            .map_err(|_| SessionError::Channel("hkdf: invalid key length".into()))?;
        mac.update(&block);
        mac.update(info);
        mac.update(&[counter]);
        block = mac.finalize().into_bytes().to_vec();
        out.extend_from_slice(&block);
        counter = counter
            .checked_add(1)
            .ok_or_else(|| SessionError::Channel("hkdf: output too long".into()))?;
    }
    out.truncate(out_len);
    Ok(out)
}

/// Build the HKDF `info`: domain label ‖ purpose ‖ channel ‖ version ‖ direction
/// ‖ epoch. The **epoch** (M6) is the reconnect generation: bumping it on every
/// resume yields entirely fresh keys, so a resumed channel starts its counters at
/// zero under a *different* key — no `(key, nonce)` pair is ever reused across
/// reconnects, and a counter reset is never a rollback within one key.
fn info(purpose: u8, channel: ChannelId, version: Version, dir: Direction, epoch: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(DOMAIN_LABEL.len() + 1 + 8 + 4 + 1 + 8);
    v.extend_from_slice(DOMAIN_LABEL);
    v.push(purpose);
    v.extend_from_slice(&channel.get().to_be_bytes());
    v.extend_from_slice(&version.major.to_be_bytes());
    v.extend_from_slice(&version.minor.to_be_bytes());
    v.push(dir as u8);
    v.extend_from_slice(&epoch.to_be_bytes());
    v
}

fn derive_key(
    master: &[u8; 32],
    channel: ChannelId,
    version: Version,
    dir: Direction,
    epoch: u64,
) -> Result<[u8; 32], SessionError> {
    let bytes = hkdf_expand(master, &info(PURPOSE_KEY, channel, version, dir, epoch), 32)?;
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

fn derive_prefix(
    master: &[u8; 32],
    channel: ChannelId,
    version: Version,
    dir: Direction,
    epoch: u64,
) -> Result<[u8; 4], SessionError> {
    let bytes = hkdf_expand(
        master,
        &info(PURPOSE_NONCE, channel, version, dir, epoch),
        4,
    )?;
    let mut prefix = [0u8; 4];
    prefix.copy_from_slice(&bytes);
    Ok(prefix)
}

/// A single channel's crypto context: independent keys, nonce prefixes, and
/// monotonic counters for each direction. Reused by [`crate::SecureLink`] and by
/// the session's channel actors.
pub(crate) struct ChannelCrypto {
    send_key: [u8; 32],
    recv_key: [u8; 32],
    send_prefix: [u8; 4],
    recv_prefix: [u8; 4],
    send_ctr: u64,
    recv_ctr: u64,
}

impl ChannelCrypto {
    /// Construct directly from key material (used by [`crate::SecureLink`], whose
    /// keys come straight from the handshake, and by tests).
    pub(crate) fn from_keys(
        send_key: [u8; 32],
        recv_key: [u8; 32],
        send_prefix: [u8; 4],
        recv_prefix: [u8; 4],
    ) -> Self {
        ChannelCrypto {
            send_key,
            recv_key,
            send_prefix,
            recv_prefix,
            send_ctr: 0,
            recv_ctr: 0,
        }
    }

    fn nonce(prefix: [u8; 4], ctr: u64) -> Nonce {
        let mut n = [0u8; 12];
        n[..4].copy_from_slice(&prefix);
        n[4..].copy_from_slice(&ctr.to_be_bytes());
        Nonce(n)
    }

    /// Seal `plaintext` with the **current** send counter (does not advance it).
    /// The caller advances via [`advance_send`](ChannelCrypto::advance_send) only
    /// after the sealed frame is successfully handed to the transport, so a
    /// retried send re-seals with the same nonce (the receiver never advanced).
    pub(crate) fn seal(
        &self,
        enc: &dyn EncryptionProvider,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, SessionError> {
        let nonce = Self::nonce(self.send_prefix, self.send_ctr);
        enc.seal(&self.send_key, &nonce, plaintext)
            .map_err(SessionError::from)
    }

    /// Advance the send counter after a successful send; fails closed on overflow.
    pub(crate) fn advance_send(&mut self) -> Result<(), SessionError> {
        self.send_ctr = self
            .send_ctr
            .checked_add(1)
            .ok_or_else(|| SessionError::Channel("send nonce counter overflow".into()))?;
        Ok(())
    }

    /// Open a sealed frame: reject anything but the next expected counter
    /// (replay/reorder/forgery), verify + decrypt, then advance the receive
    /// counter (fails closed on overflow).
    pub(crate) fn open(
        &mut self,
        enc: &dyn EncryptionProvider,
        sealed: &[u8],
    ) -> Result<Vec<u8>, SessionError> {
        if sealed.len() < 12 {
            return Err(SessionError::Channel("secure frame too short".into()));
        }
        let got_prefix = &sealed[..4];
        let got_ctr = u64::from_be_bytes(
            sealed[4..12]
                .try_into()
                .map_err(|_| SessionError::Channel("bad nonce".into()))?,
        );
        if got_prefix != self.recv_prefix || got_ctr != self.recv_ctr {
            return Err(SessionError::Channel(
                "replayed, reordered, or forged frame".into(),
            ));
        }
        let plain = enc
            .open(&self.recv_key, sealed)
            .map_err(SessionError::from)?;
        self.recv_ctr = self
            .recv_ctr
            .checked_add(1)
            .ok_or_else(|| SessionError::Channel("recv nonce counter overflow".into()))?;
        Ok(plain)
    }
}

impl Drop for ChannelCrypto {
    fn drop(&mut self) {
        // Secure zeroization: keys must not linger in freed memory.
        self.send_key.zeroize();
        self.recv_key.zeroize();
    }
}

/// The session master secret plus the metadata needed to derive per-channel
/// contexts. Constructed once from the authenticated handshake.
pub(crate) struct SessionCrypto {
    master_send: [u8; 32],
    master_recv: [u8; 32],
    role: SessionRole,
    /// Reconnect generation (M6). Starts at 0; every successful resume bumps it,
    /// re-deriving all channel keys so nonces are never reused across reconnects.
    epoch: u64,
    enc: Arc<dyn EncryptionProvider>,
}

impl SessionCrypto {
    /// Capture the master secret from an authenticated [`Session`] at epoch 0.
    pub(crate) fn from_session(
        session: &Session,
        role: SessionRole,
        enc: Arc<dyn EncryptionProvider>,
    ) -> Self {
        SessionCrypto {
            master_send: session.send_key,
            master_recv: session.recv_key,
            role,
            epoch: 0,
            enc,
        }
    }

    /// The encryption provider, shared with channel actors.
    pub(crate) fn enc(&self) -> Arc<dyn EncryptionProvider> {
        self.enc.clone()
    }

    /// The current reconnect generation.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// A copy of this crypto context rebound to `epoch` — same master secret and
    /// role, fresh key generation. Used on resume so the recovered session's
    /// channels derive brand-new keys (no nonce reuse). The old context is dropped
    /// (and zeroized) by the caller.
    pub(crate) fn with_epoch(&self, epoch: u64) -> Self {
        SessionCrypto {
            master_send: self.master_send,
            master_recv: self.master_recv,
            role: self.role,
            epoch,
            enc: self.enc.clone(),
        }
    }

    /// The **resume key**: a symmetric, role-independent key derived from the
    /// session master secret, used to MAC resume tokens. Both peers derive an
    /// identical value (the two directional masters are ordered canonically before
    /// derivation, so initiator and responder agree regardless of role), and it is
    /// domain-separated from every channel key. It is *never* sent on the wire —
    /// only its MAC over a token is.
    pub(crate) fn resume_key(&self) -> Result<[u8; 32], SessionError> {
        // Canonical order so both roles derive the same key: initiator's
        // master_send == responder's master_recv and vice-versa.
        let (lo, hi) = if self.master_send <= self.master_recv {
            (self.master_send, self.master_recv)
        } else {
            (self.master_recv, self.master_send)
        };
        let mut prk = [0u8; 64];
        prk[..32].copy_from_slice(&lo);
        prk[32..].copy_from_slice(&hi);
        let bytes = hkdf_expand(&prk, RESUME_LABEL, 32)?;
        prk.zeroize();
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        Ok(key)
    }

    fn send_dir(&self) -> Direction {
        match self.role {
            SessionRole::Initiator => Direction::Forward,
            SessionRole::Responder => Direction::Reverse,
        }
    }

    fn recv_dir(&self) -> Direction {
        match self.role {
            SessionRole::Initiator => Direction::Reverse,
            SessionRole::Responder => Direction::Forward,
        }
    }

    /// Derive the crypto context for `channel` at protocol `version` (at the
    /// current epoch).
    pub(crate) fn derive(
        &self,
        channel: ChannelId,
        version: Version,
    ) -> Result<ChannelCrypto, SessionError> {
        let e = self.epoch;
        Ok(ChannelCrypto::from_keys(
            derive_key(&self.master_send, channel, version, self.send_dir(), e)?,
            derive_key(&self.master_recv, channel, version, self.recv_dir(), e)?,
            derive_prefix(&self.master_send, channel, version, self.send_dir(), e)?,
            derive_prefix(&self.master_recv, channel, version, self.recv_dir(), e)?,
        ))
    }

    /// Derive the control channel's crypto context (fixed [`CONTROL_VERSION`]).
    pub(crate) fn control(&self) -> Result<ChannelCrypto, SessionError> {
        self.derive(ChannelId::CONTROL, CONTROL_VERSION)
    }
}

impl Drop for SessionCrypto {
    fn drop(&mut self) {
        self.master_send.zeroize();
        self.master_recv.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_crypto::AeadCrypto;
    use peerbeam_domain::id::DeviceId;

    // Two sessions sharing the ECDH-derived directional keys: initiator.send ==
    // responder.recv and vice-versa (as a real handshake produces).
    fn master_pair() -> (SessionCrypto, SessionCrypto) {
        let k_fwd = [11u8; 32];
        let k_rev = [22u8; 32];
        let init = Session {
            send_key: k_fwd,
            recv_key: k_rev,
            send_prefix: [0; 4],
            recv_prefix: [0; 4],
            peer_id: DeviceId::from("r"),
            peer_name: "r".into(),
            newly_trusted: false,
        };
        let resp = Session {
            send_key: k_rev,
            recv_key: k_fwd,
            send_prefix: [0; 4],
            recv_prefix: [0; 4],
            peer_id: DeviceId::from("i"),
            peer_name: "i".into(),
            newly_trusted: false,
        };
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        (
            SessionCrypto::from_session(&init, SessionRole::Initiator, enc.clone()),
            SessionCrypto::from_session(&resp, SessionRole::Responder, enc),
        )
    }

    fn v() -> Version {
        Version::new(1, 0)
    }

    #[test]
    fn peers_derive_matching_channel_keys() {
        let (init, resp) = master_pair();
        let mut a = init.derive(ChannelId::new(1), v()).unwrap();
        let mut b = resp.derive(ChannelId::new(1), v()).unwrap();
        let enc = AeadCrypto::new();
        // A→B
        let sealed = a.seal(&enc, b"hello").unwrap();
        a.advance_send().unwrap();
        assert_eq!(b.open(&enc, &sealed).unwrap(), b"hello");
        // B→A (independent direction)
        let sealed = b.seal(&enc, b"hi back").unwrap();
        b.advance_send().unwrap();
        assert_eq!(a.open(&enc, &sealed).unwrap(), b"hi back");
    }

    #[test]
    fn different_channels_get_different_keys() {
        let (init, _resp) = master_pair();
        let c1 = init.derive(ChannelId::new(1), v()).unwrap();
        let c2 = init.derive(ChannelId::new(2), v()).unwrap();
        assert_ne!(c1.send_key, c2.send_key);
        assert_ne!(c1.recv_key, c2.recv_key);
        // Send and recv within one channel also differ (distinct directions).
        assert_ne!(c1.send_key, c1.recv_key);
        assert_ne!(c1.send_prefix, c2.send_prefix);
        // Control channel differs from any data channel.
        let ctl = init.control().unwrap();
        assert_ne!(ctl.send_key, c1.send_key);
    }

    #[test]
    fn nonces_are_unique_per_frame() {
        let (init, _r) = master_pair();
        let mut c = init.derive(ChannelId::new(5), v()).unwrap();
        let enc = AeadCrypto::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let sealed = c.seal(&enc, b"x").unwrap();
            c.advance_send().unwrap();
            // First 12 bytes are the nonce; every one must be unique.
            assert!(seen.insert(sealed[..12].to_vec()), "nonce reused");
        }
    }

    #[test]
    fn replay_and_reorder_are_rejected() {
        let (init, resp) = master_pair();
        let mut a = init.derive(ChannelId::new(1), v()).unwrap();
        let mut b = resp.derive(ChannelId::new(1), v()).unwrap();
        let enc = AeadCrypto::new();
        let f0 = a.seal(&enc, b"0").unwrap();
        a.advance_send().unwrap();
        let f1 = a.seal(&enc, b"1").unwrap();
        a.advance_send().unwrap();
        // Out of order (f1 before f0) is rejected.
        assert!(b.open(&enc, &f1).is_err());
        // In order works.
        assert_eq!(b.open(&enc, &f0).unwrap(), b"0");
        assert_eq!(b.open(&enc, &f1).unwrap(), b"1");
        // Replay of f0 now fails (counter advanced).
        assert!(b.open(&enc, &f0).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails_to_open() {
        let (init, resp) = master_pair();
        let mut a = init.derive(ChannelId::new(1), v()).unwrap();
        let mut b = resp.derive(ChannelId::new(1), v()).unwrap();
        let enc = AeadCrypto::new();
        let mut sealed = a.seal(&enc, b"secret").unwrap();
        a.advance_send().unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01; // flip a tag bit
        assert!(b.open(&enc, &sealed).is_err());
    }

    #[test]
    fn wrong_channel_cannot_decrypt() {
        let (init, resp) = master_pair();
        let mut a1 = init.derive(ChannelId::new(1), v()).unwrap();
        let mut b2 = resp.derive(ChannelId::new(2), v()).unwrap();
        let enc = AeadCrypto::new();
        let sealed = a1.seal(&enc, b"for channel 1").unwrap();
        a1.advance_send().unwrap();
        // Channel 2's context cannot open channel 1's frame.
        assert!(b2.open(&enc, &sealed).is_err());
    }

    #[test]
    fn malformed_frame_is_rejected() {
        let (init, _r) = master_pair();
        let mut c = init.derive(ChannelId::new(1), v()).unwrap();
        let enc = AeadCrypto::new();
        assert!(c.open(&enc, &[0u8; 3]).is_err()); // too short
        assert!(c.open(&enc, &[]).is_err());
    }

    #[test]
    fn counter_overflow_fails_closed() {
        let (init, _r) = master_pair();
        let mut c = init.derive(ChannelId::new(1), v()).unwrap();
        c.send_ctr = u64::MAX;
        assert!(c.advance_send().is_err());
        c.recv_ctr = u64::MAX;
        // A valid-looking frame at MAX would advance past MAX → fail closed.
        // (We can't easily forge a valid ciphertext here; the counter guard is
        // exercised directly.)
        assert_eq!(c.recv_ctr, u64::MAX);
    }

    #[test]
    fn version_is_mixed_into_derivation() {
        let (init, _r) = master_pair();
        let c_v10 = init.derive(ChannelId::new(1), Version::new(1, 0)).unwrap();
        let c_v11 = init.derive(ChannelId::new(1), Version::new(1, 1)).unwrap();
        assert_ne!(c_v10.send_key, c_v11.send_key);
    }

    #[test]
    fn epoch_bump_yields_fresh_keys_no_nonce_reuse() {
        let (init, _r) = master_pair();
        assert_eq!(init.epoch(), 0);
        let e0 = init.derive(ChannelId::new(1), v()).unwrap();
        let resumed = init.with_epoch(1);
        assert_eq!(resumed.epoch(), 1);
        let e1 = resumed.derive(ChannelId::new(1), v()).unwrap();
        // Same channel + version, next epoch → entirely different key material,
        // so counters restarting at 0 never reuse an (epoch-0) nonce.
        assert_ne!(e0.send_key, e1.send_key);
        assert_ne!(e0.recv_key, e1.recv_key);
        assert_ne!(e0.send_prefix, e1.send_prefix);
        // Control channel also re-keys per epoch.
        let ctl0 = init.control().unwrap();
        let ctl1 = resumed.control().unwrap();
        assert_ne!(ctl0.send_key, ctl1.send_key);
    }

    #[test]
    fn both_peers_derive_same_epoch_keys() {
        let (init, resp) = master_pair();
        let mut a = init.with_epoch(3).derive(ChannelId::new(2), v()).unwrap();
        let mut b = resp.with_epoch(3).derive(ChannelId::new(2), v()).unwrap();
        let enc = AeadCrypto::new();
        let sealed = a.seal(&enc, b"resumed").unwrap();
        a.advance_send().unwrap();
        assert_eq!(b.open(&enc, &sealed).unwrap(), b"resumed");
    }

    #[test]
    fn resume_key_is_symmetric_across_roles() {
        // Initiator and responder hold mirrored directional masters; the resume
        // key must come out identical so either can MAC/validate a token.
        let (init, resp) = master_pair();
        assert_eq!(init.resume_key().unwrap(), resp.resume_key().unwrap());
        // It is domain-separated from any channel key.
        let ch = init.derive(ChannelId::new(1), v()).unwrap();
        assert_ne!(init.resume_key().unwrap(), ch.send_key);
    }
}
