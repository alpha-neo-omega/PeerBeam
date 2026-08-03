//! Session resume tokens (M6): a single-use, integrity-protected credential that
//! lets a reconnecting peer re-attach to an existing authenticated session
//! **without repeating the handshake**, while still proving it is the same peer.
//!
//! ## What a token proves
//!
//! A [`ResumeToken`] is a MAC (HMAC-SHA256) over an immutable binding, keyed by
//! the session's **resume key** ([`SessionCrypto::resume_key`]) — which is derived
//! from the authenticated master secret and never leaves memory. Only a peer that
//! completed the original handshake holds that key, so a valid MAC is proof of the
//! authenticated identity. The token binds:
//!
//! - **SessionId** — which session to re-attach to,
//! - **peer identity** ([`DeviceId`]) — who the token is for,
//! - **protocol version** — the negotiated wire version (I9),
//! - **epoch** — the reconnect generation (single-use; see below),
//! - **created / expires timestamps** — a short validity window.
//!
//! ## Security properties (all enforced by [`ResumeToken::verify`])
//!
//! - **Tamper** → the MAC covers every field; any change fails verification.
//! - **Wrong peer / wrong version / wrong session** → binding mismatch, rejected.
//! - **Expired** → outside `[created, expires]`, rejected.
//! - **Replay** → each token authorises exactly one epoch; the accepter tracks the
//!   highest epoch it has consumed and rejects any token whose epoch is not
//!   strictly greater, so a captured token cannot be reused.
//!
//! A rejected token never resumes: the caller fails closed to a fresh session
//! (I11). The resume key itself is never transmitted — only a MAC over public
//! binding fields is.
//!
//! [`SessionCrypto::resume_key`]: super::crypto::SessionCrypto::resume_key

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{SessionError, SessionId, Version};

type HmacSha256 = Hmac<Sha256>;

/// The immutable binding a resume token authenticates. Both the minting peer and
/// the validating peer build this from their own view of the session; the MAC ties
/// them together.
///
/// The two device ids are bound as an **unordered pair** (canonically sorted
/// before hashing), so each side — which sees the ids in mirrored `local`/`peer`
/// roles — produces the same binding, while a token for any other pair of devices
/// fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeBinding {
    /// The session being resumed.
    pub session_id: SessionId,
    /// This side's own device id.
    pub local: DeviceId,
    /// The peer's device id.
    pub peer: DeviceId,
    /// The negotiated protocol version (I9).
    pub version: Version,
}

/// The two device ids, canonically ordered so both endpoints agree.
fn ordered_pair(a: &DeviceId, b: &DeviceId) -> (String, String) {
    let (x, y) = (a.as_str().to_string(), b.as_str().to_string());
    if x <= y {
        (x, y)
    } else {
        (y, x)
    }
}

/// A single-use, MAC-protected session resume credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeToken {
    /// The session to re-attach to.
    pub session_id: SessionId,
    /// The minting side's device id (one half of the bound pair).
    pub local: DeviceId,
    /// The counterpart device id (the other half of the bound pair).
    pub peer: DeviceId,
    /// The negotiated protocol version.
    pub version: Version,
    /// Reconnect generation this token authorises (strictly increasing; single-use).
    pub epoch: u64,
    /// Creation time (unix milliseconds).
    pub created_at_ms: u64,
    /// Expiry time (unix milliseconds); the token is invalid at or after this.
    pub expires_at_ms: u64,
    /// HMAC-SHA256 over the binding + epoch + timestamps, keyed by the resume key.
    pub mac: [u8; 32],
}

/// Canonical, serialization-independent MAC input: fixed field order and widths so
/// the tag is stable regardless of how the token is encoded on the wire.
#[allow(clippy::too_many_arguments)]
fn mac_input(
    session_id: &SessionId,
    local: &DeviceId,
    peer: &DeviceId,
    version: Version,
    epoch: u64,
    created_at_ms: u64,
    expires_at_ms: u64,
) -> Vec<u8> {
    // Canonical, role-independent pair so both endpoints hash the same bytes.
    let (lo, hi) = ordered_pair(local, peer);
    let lo = lo.as_bytes();
    let hi = hi.as_bytes();
    let mut v = Vec::with_capacity(16 + 8 + lo.len() + hi.len() + 4 + 8 + 8 + 8);
    v.extend_from_slice(session_id.as_bytes());
    v.extend_from_slice(&(lo.len() as u32).to_be_bytes());
    v.extend_from_slice(lo);
    v.extend_from_slice(&(hi.len() as u32).to_be_bytes());
    v.extend_from_slice(hi);
    v.extend_from_slice(&version.major.to_be_bytes());
    v.extend_from_slice(&version.minor.to_be_bytes());
    v.extend_from_slice(&epoch.to_be_bytes());
    v.extend_from_slice(&created_at_ms.to_be_bytes());
    v.extend_from_slice(&expires_at_ms.to_be_bytes());
    v
}

fn compute_mac(resume_key: &[u8; 32], input: &[u8]) -> Result<[u8; 32], SessionError> {
    let mut mac = HmacSha256::new_from_slice(resume_key)
        .map_err(|_| SessionError::ResumeRejected("invalid resume key length".into()))?;
    mac.update(input);
    let tag = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&tag);
    Ok(out)
}

impl ResumeToken {
    /// Mint a token for the next reconnect generation `epoch`, valid for
    /// `ttl_ms` from `now_ms`. `resume_key` comes from
    /// [`SessionCrypto::resume_key`](super::crypto::SessionCrypto::resume_key).
    pub fn mint(
        resume_key: &[u8; 32],
        binding: &ResumeBinding,
        epoch: u64,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<ResumeToken, SessionError> {
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        let mac = compute_mac(
            resume_key,
            &mac_input(
                &binding.session_id,
                &binding.local,
                &binding.peer,
                binding.version,
                epoch,
                now_ms,
                expires_at_ms,
            ),
        )?;
        Ok(ResumeToken {
            session_id: binding.session_id,
            local: binding.local.clone(),
            peer: binding.peer.clone(),
            version: binding.version,
            epoch,
            created_at_ms: now_ms,
            expires_at_ms,
            mac,
        })
    }

    /// Verify this token against the resume key and the accepter's expected
    /// binding, at time `now_ms`, given the highest epoch already consumed
    /// (`consumed_epoch`).
    ///
    /// Order of checks is deliberate: MAC first (authenticity), then binding, then
    /// freshness, then single-use. All failures are typed and fail closed.
    pub fn verify(
        &self,
        resume_key: &[u8; 32],
        expected: &ResumeBinding,
        now_ms: u64,
        consumed_epoch: u64,
    ) -> Result<(), SessionError> {
        // 1. Authenticity: recompute the MAC and compare in constant time.
        let mut mac = HmacSha256::new_from_slice(resume_key)
            .map_err(|_| SessionError::ResumeRejected("invalid resume key length".into()))?;
        mac.update(&mac_input(
            &self.session_id,
            &self.local,
            &self.peer,
            self.version,
            self.epoch,
            self.created_at_ms,
            self.expires_at_ms,
        ));
        mac.verify_slice(&self.mac).map_err(|_| {
            SessionError::ResumeRejected("token MAC mismatch (tampered or forged)".into())
        })?;

        // 2. Binding: the token must be for this session, this device pair, this
        //    version. The device pair is compared unordered.
        if self.session_id != expected.session_id {
            return Err(SessionError::ResumeRejected("session id mismatch".into()));
        }
        if ordered_pair(&self.local, &self.peer) != ordered_pair(&expected.local, &expected.peer) {
            return Err(SessionError::ResumeRejected(
                "peer identity mismatch".into(),
            ));
        }
        if self.version != expected.version {
            return Err(SessionError::ResumeRejected(
                "protocol version mismatch".into(),
            ));
        }

        // 3. Freshness: within its validity window.
        if now_ms >= self.expires_at_ms {
            return Err(SessionError::ResumeExpired);
        }

        // 4. Single-use: the epoch must be strictly greater than any consumed.
        if self.epoch <= consumed_epoch {
            return Err(SessionError::ResumeReplayed);
        }

        Ok(())
    }

    /// Encode for the wire (JSON, matching the control-channel codec).
    pub fn encode(&self) -> Result<Vec<u8>, SessionError> {
        serde_json::to_vec(self).map_err(|e| SessionError::Serialization(e.to_string()))
    }

    /// Decode from the wire.
    pub fn decode(bytes: &[u8]) -> Result<ResumeToken, SessionError> {
        serde_json::from_slice(bytes).map_err(|e| SessionError::Serialization(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7u8; 32];
    const OTHER_KEY: [u8; 32] = [8u8; 32];
    const NOW: u64 = 1_000_000;
    const TTL: u64 = 30_000;

    fn binding() -> ResumeBinding {
        ResumeBinding {
            session_id: SessionId::from_u128(0xABCD),
            local: DeviceId::from("device-me"),
            peer: DeviceId::from("peer-a"),
            version: Version::new(1, 0),
        }
    }

    /// The peer's mirror view: local/peer swapped. Must verify identically.
    fn mirror_binding() -> ResumeBinding {
        ResumeBinding {
            session_id: SessionId::from_u128(0xABCD),
            local: DeviceId::from("peer-a"),
            peer: DeviceId::from("device-me"),
            version: Version::new(1, 0),
        }
    }

    fn mint(epoch: u64) -> ResumeToken {
        ResumeToken::mint(&KEY, &binding(), epoch, NOW, TTL).unwrap()
    }

    #[test]
    fn valid_token_verifies() {
        let t = mint(1);
        assert!(t.verify(&KEY, &binding(), NOW + 1_000, 0).is_ok());
    }

    #[test]
    fn verifies_from_the_peers_mirrored_view() {
        // The token minted by one side verifies against the other side's binding,
        // where local/peer are swapped (unordered pair).
        let t = mint(1);
        assert!(t.verify(&KEY, &mirror_binding(), NOW + 1, 0).is_ok());
    }

    #[test]
    fn roundtrips_through_the_wire() {
        let t = mint(1);
        let bytes = t.encode().unwrap();
        let back = ResumeToken::decode(&bytes).unwrap();
        assert_eq!(t, back);
        assert!(back.verify(&KEY, &binding(), NOW + 1, 0).is_ok());
    }

    #[test]
    fn wrong_resume_key_fails() {
        let t = mint(1);
        assert!(matches!(
            t.verify(&OTHER_KEY, &binding(), NOW + 1, 0),
            Err(SessionError::ResumeRejected(_))
        ));
    }

    #[test]
    fn tampered_field_fails() {
        let mut t = mint(1);
        t.epoch = 999; // MAC no longer covers this value
        assert!(matches!(
            t.verify(&KEY, &binding(), NOW + 1, 0),
            Err(SessionError::ResumeRejected(_))
        ));
    }

    #[test]
    fn tampered_mac_fails() {
        let mut t = mint(1);
        t.mac[0] ^= 0x01;
        assert!(matches!(
            t.verify(&KEY, &binding(), NOW + 1, 0),
            Err(SessionError::ResumeRejected(_))
        ));
    }

    #[test]
    fn wrong_peer_fails() {
        let t = mint(1);
        let mut other = binding();
        other.peer = DeviceId::from("peer-b");
        assert!(matches!(
            t.verify(&KEY, &other, NOW + 1, 0),
            Err(SessionError::ResumeRejected(_))
        ));
    }

    #[test]
    fn wrong_version_fails() {
        let t = mint(1);
        let mut other = binding();
        other.version = Version::new(2, 0);
        assert!(matches!(
            t.verify(&KEY, &other, NOW + 1, 0),
            Err(SessionError::ResumeRejected(_))
        ));
    }

    #[test]
    fn wrong_session_fails() {
        let t = mint(1);
        let mut other = binding();
        other.session_id = SessionId::from_u128(0x1234);
        assert!(matches!(
            t.verify(&KEY, &other, NOW + 1, 0),
            Err(SessionError::ResumeRejected(_))
        ));
    }

    #[test]
    fn expired_token_fails() {
        let t = mint(1);
        assert!(matches!(
            t.verify(&KEY, &binding(), NOW + TTL, 0),
            Err(SessionError::ResumeExpired)
        ));
        // One ms before expiry still valid.
        assert!(t.verify(&KEY, &binding(), NOW + TTL - 1, 0).is_ok());
    }

    #[test]
    fn replayed_epoch_fails() {
        let t = mint(1);
        // Epoch 1 already consumed → a token for epoch 1 is a replay.
        assert!(matches!(
            t.verify(&KEY, &binding(), NOW + 1, 1),
            Err(SessionError::ResumeReplayed)
        ));
        // Same-or-lower epoch also fails.
        assert!(matches!(
            t.verify(&KEY, &binding(), NOW + 1, 5),
            Err(SessionError::ResumeReplayed)
        ));
    }

    #[test]
    fn monotonic_epochs_each_verify_once() {
        // Epoch 1 valid when nothing consumed; epoch 2 valid after 1 consumed.
        assert!(mint(1).verify(&KEY, &binding(), NOW + 1, 0).is_ok());
        assert!(mint(2).verify(&KEY, &binding(), NOW + 1, 1).is_ok());
        // But re-presenting epoch 1 after consuming 1 is a replay.
        assert!(matches!(
            mint(1).verify(&KEY, &binding(), NOW + 1, 1),
            Err(SessionError::ResumeReplayed)
        ));
    }
}
