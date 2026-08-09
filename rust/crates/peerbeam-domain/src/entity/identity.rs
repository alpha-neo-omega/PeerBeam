//! Persistent device identity: the long-term keypair + the device id derived
//! from its fingerprint. Serialized with 32-byte keys as lowercase hex so the
//! on-disk file is human-inspectable and independent of in-memory layout.

use serde::{Deserialize, Serialize};

use crate::id::DeviceId;
use crate::port::{Fingerprint, PublicKey, SecretKey};

/// The device's stable cryptographic identity, persisted across restarts.
///
/// Not `Debug`/`PartialEq` on purpose: the secret key must not be printed, and
/// secret equality is not needed outside tests (which compare the raw bytes).
#[derive(Clone, Serialize, Deserialize)]
#[serde(into = "IdentityWire", try_from = "IdentityWire")]
pub struct StoredIdentity {
    /// Stable device id, derived from `public`'s fingerprint.
    pub device_id: DeviceId,
    /// Long-term X25519 public key.
    pub public: PublicKey,
    /// Long-term X25519 secret key. Never leaves the device.
    pub secret: SecretKey,
}

/// On-disk form: keys as hex strings.
#[derive(Serialize, Deserialize)]
struct IdentityWire {
    device_id: String,
    public: String,
    secret: String,
}

impl From<StoredIdentity> for IdentityWire {
    fn from(s: StoredIdentity) -> Self {
        IdentityWire {
            device_id: s.device_id.0,
            public: to_hex(&s.public.0),
            secret: to_hex(&s.secret.0),
        }
    }
}

impl TryFrom<IdentityWire> for StoredIdentity {
    type Error = String;
    fn try_from(w: IdentityWire) -> Result<Self, String> {
        Ok(StoredIdentity {
            device_id: DeviceId::from(w.device_id),
            public: PublicKey(from_hex(&w.public)?),
            secret: SecretKey(from_hex(&w.secret)?),
        })
    }
}

/// Derive the stable device id: `"pb-"` + the first 12 hex chars of the public
/// key's fingerprint (itself `SHA-256(public)` as hex). One source of truth —
/// the keypair determines the fingerprint, which determines the id.
#[must_use]
pub fn device_id_from_fingerprint(fp: &Fingerprint) -> DeviceId {
    let short: String = fp.0.chars().take(12).collect();
    DeviceId::from(format!("pb-{short}"))
}

fn to_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn from_hex(s: &str) -> Result<[u8; 32], String> {
    if s.len() != 64 || !s.is_ascii() {
        return Err(format!("expected 64 hex chars, got {}", s.len()));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("bad hex at byte {i}: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(hex: &str) -> Fingerprint {
        Fingerprint(hex.to_string())
    }

    #[test]
    fn device_id_derivation_is_stable_distinct_and_formatted() {
        let a = fp(&"a".repeat(64));
        let b = fp(&"b".repeat(64));
        assert_eq!(
            device_id_from_fingerprint(&a).0,
            device_id_from_fingerprint(&a).0,
            "same fingerprint -> same id"
        );
        assert_ne!(
            device_id_from_fingerprint(&a).0,
            device_id_from_fingerprint(&b).0,
            "different fingerprint -> different id"
        );
        let id = device_id_from_fingerprint(&a).0;
        assert!(id.starts_with("pb-"));
        assert_eq!(id.len(), 3 + 12);
    }

    #[test]
    fn stored_identity_json_round_trips_as_hex() {
        let s = StoredIdentity {
            device_id: DeviceId::from("pb-abcdef012345"),
            public: PublicKey([7u8; 32]),
            secret: SecretKey([9u8; 32]),
        };
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(json.contains(&"07".repeat(32)), "public as hex");
        assert!(json.contains(&"09".repeat(32)), "secret as hex");
        let back: StoredIdentity = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.device_id.0, "pb-abcdef012345");
        assert_eq!(back.public.0, [7u8; 32]);
        assert_eq!(back.secret.0, [9u8; 32]);
    }

    #[test]
    fn bad_hex_is_rejected_not_silently_accepted() {
        let json = r#"{"device_id":"pb-x","public":"zz","secret":"00"}"#;
        assert!(serde_json::from_str::<StoredIdentity>(json).is_err());
    }

    #[test]
    fn sixty_four_byte_multi_byte_utf8_is_rejected_not_panicking() {
        // `public` here is exactly 64 *bytes* (the old guard's unit) but only
        // 63 *chars*, because it contains one 2-byte UTF-8 character ('é')
        // positioned right after the first byte. `from_hex` slices in fixed
        // 2-byte chunks (`s[0..2]`, `s[2..4]`, ...); chunk `s[0..2]` here cuts
        // through the middle of 'é' (its bytes occupy offsets 1..3), which is
        // not a char boundary. Slicing a `&str` at a non-char-boundary
        // panics, so a byte-length-only guard is not enough — the guard must
        // also reject non-ASCII input before any slicing happens.
        let public = format!("a\u{e9}{}", "a".repeat(61));
        assert_eq!(public.len(), 64, "fixture must stay exactly 64 bytes");
        assert_ne!(
            public.chars().count(),
            64,
            "fixture must be non-ASCII (fewer chars than bytes)"
        );
        let json = format!(
            r#"{{"device_id":"pb-x","public":"{public}","secret":"{}"}}"#,
            "0".repeat(64)
        );
        assert!(
            serde_json::from_str::<StoredIdentity>(&json).is_err(),
            "must decode-error, not panic, on multi-byte UTF-8 padded to 64 bytes"
        );
    }
}
