//! The pairing channel: `0x0109`.
//!
//! Two messages, and a deliberate asymmetry. The side that *shows* the PIN
//! never sends it; the side that *types* it sends only a proof. The PIN itself
//! never crosses the wire, which is what makes the out-of-band step meaningful
//! — a PIN that travelled over the connection it is meant to authenticate
//! would prove nothing about who is on the other end.

use serde::{Deserialize, Serialize};

/// The channel this capability speaks on.
pub const CHANNEL: u16 = 0x0109;

/// `Prove` on the wire.
pub const MSG_PROVE: u16 = 1;
/// `Result` on the wire.
pub const MSG_RESULT: u16 = 2;

/// The wire type for one message.
#[must_use]
pub fn message_type(msg: &PairingMsg) -> u16 {
    match msg {
        PairingMsg::Offer | PairingMsg::Prove { .. } => MSG_PROVE,
        PairingMsg::Result { .. } => MSG_RESULT,
    }
}

/// A message on the pairing channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PairingMsg {
    /// "I am ready to be PIN-paired; show your person the digits." Carries
    /// nothing: the PIN is displayed locally and read aloud, never sent.
    Offer,
    /// A proof of knowing the PIN, over this handshake's transcript.
    Prove {
        #[serde(with = "hex_bytes")]
        proof: Vec<u8>,
    },
    /// The verifier's answer.
    Result {
        verified: bool,
        /// Guesses remaining after this one. `0` means the pairing is dead and
        /// a fresh PIN is required — not that the caller should retry.
        attempts_left: u8,
    },
}

/// Hex rather than a byte array, so a proof reads the same in a log, a JSON
/// bridge and a test fixture. It is a MAC, not a secret, and never a PIN.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        use std::fmt::Write;
        let mut out = String::with_capacity(v.len() * 2);
        for b in v {
            let _ = write!(out, "{b:02x}");
        }
        s.serialize_str(&out)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        // `peerbeam_domain::hex`, not a slice loop: `&s[i..i + 2]` indexes by
        // bytes and panics when a character is wider than one. A peer reaches
        // that with the ASCII JSON escape `"\u20aca"`, and the panic lands
        // inside a channel actor — before this peer is approved.
        peerbeam_domain::hex::decode(&s).ok_or_else(|| serde::de::Error::custom("invalid hex"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A peer could kill the pairing channel with one frame, before being
    /// approved.** The payload below is pure ASCII on the wire — `\u20ac` is a
    /// JSON escape — so nothing upstream can filter it; serde unescapes it to a
    /// three-byte `€`, and the old decoder sliced two bytes into it and
    /// panicked inside the channel actor.
    #[test]
    fn a_multibyte_proof_is_refused_rather_than_panicking() {
        // Written as the escape it is on the wire, so this test carries the
        // same ASCII bytes a hostile peer would send.
        let frame = br#"{"Prove":{"proof":"\u20aca"}}"#;
        let got: Result<PairingMsg, _> = serde_json::from_slice(frame);
        assert!(got.is_err(), "refused, and above all not a panic");
    }

    #[test]
    fn messages_round_trip() {
        for m in [
            PairingMsg::Offer,
            PairingMsg::Prove {
                proof: vec![0, 1, 0xfe, 0xff],
            },
            PairingMsg::Result {
                verified: true,
                attempts_left: 2,
            },
        ] {
            let json = serde_json::to_string(&m).unwrap();
            assert_eq!(serde_json::from_str::<PairingMsg>(&json).unwrap(), m);
        }
    }

    /// **The PIN has no field to travel in.** A PIN sent over the connection it
    /// authenticates proves nothing about who is on the other end, so the wire
    /// format gives it nowhere to go — the asymmetry is structural, not a rule
    /// someone has to remember.
    #[test]
    fn no_message_can_carry_a_pin() {
        let all = [
            serde_json::to_string(&PairingMsg::Offer).unwrap(),
            serde_json::to_string(&PairingMsg::Prove { proof: vec![1, 2] }).unwrap(),
            serde_json::to_string(&PairingMsg::Result {
                verified: false,
                attempts_left: 1,
            })
            .unwrap(),
        ];
        for json in all {
            assert!(
                !json.to_lowercase().contains("pin"),
                "a pairing message has a PIN-shaped field: {json}"
            );
        }
    }

    #[test]
    fn a_malformed_proof_is_rejected_rather_than_truncated() {
        assert!(serde_json::from_str::<PairingMsg>(r#"{"Prove":{"proof":"abc"}}"#).is_err());
        assert!(serde_json::from_str::<PairingMsg>(r#"{"Prove":{"proof":"zz"}}"#).is_err());
    }
}
