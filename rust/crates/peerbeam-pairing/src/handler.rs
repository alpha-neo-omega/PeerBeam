//! The pairing channel handler.
//!
//! Routes [`PairingMsg`] to whoever is running the pairing flow. Holds no PIN
//! and makes no trust decision: it moves messages, and the decision stays with
//! the code that owns the attempt budget.

use std::sync::Arc;

use async_trait::async_trait;
use peerbeam_domain::session::{ChannelType, MessageHandler, SessionError, SessionFrame};

use crate::wire::PairingMsg;

/// Somewhere to deliver a pairing message.
pub type PairingSink = Arc<dyn Fn(PairingMsg) + Send + Sync>;

/// Routes pairing messages to whoever is running the flow.
///
/// Holds no PIN and makes no trust decision: it moves messages. The decision —
/// and the attempt budget that makes six digits safe — stays with the code that
/// owns the [`Pairing`](crate::Pairing).
pub struct PairingHandler {
    incoming: PairingSink,
}

impl PairingHandler {
    #[must_use]
    pub fn new(incoming: PairingSink) -> Arc<PairingHandler> {
        Arc::new(PairingHandler { incoming })
    }
}

#[async_trait]
impl MessageHandler for PairingHandler {
    fn channel_type(&self) -> ChannelType {
        ChannelType::PAIRING
    }

    async fn handle(&self, frame: SessionFrame) -> Result<(), SessionError> {
        // Undecodable frames are dropped, not errors: an unknown OPTIONAL type
        // is skipped per MESSAGE_REGISTRY.md §6, and failing the session over a
        // malformed pairing frame would let anyone on the path kill a
        // connection by sending junk on this channel.
        if let Some(msg) = decode(&frame.payload) {
            (self.incoming)(msg);
        }
        Ok(())
    }
}

/// Decode a pairing payload, or `None` if it is not one.
///
/// **Rejects rather than guesses.** A malformed pairing message during the one
/// exchange that authenticates first contact is exactly when a permissive
/// parser is most expensive.
#[must_use]
pub fn decode(payload: &[u8]) -> Option<PairingMsg> {
    serde_json::from_slice(payload).ok()
}

/// Encode a pairing message.
#[must_use]
pub fn encode(msg: &PairingMsg) -> Vec<u8> {
    serde_json::to_vec(msg).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pairing_message_survives_the_round_trip() {
        let m = PairingMsg::Prove {
            proof: vec![1, 2, 3],
        };
        assert_eq!(decode(&encode(&m)), Some(m));
    }

    #[test]
    fn junk_is_rejected_rather_than_guessed_at() {
        assert!(decode(b"").is_none());
        assert!(decode(b"{}").is_none());
        assert!(decode(b"not json").is_none());
        assert!(decode(br#"{"Prove":{}}"#).is_none());
    }
}
