//! The wire status message carried on the Presence channel.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use peerbeam_domain::session::{ChannelId, MessageFlags, MessageType, SessionError, SessionFrame};

/// MessageType id for a device status within the Presence channel namespace
/// (`docs/MESSAGE_REGISTRY.md` §4, Presence `Status = 1`).
pub const MSG_STATUS: u16 = 1;

/// MessageType id for a ring request within the Presence channel namespace.
pub const MSG_RING: u16 = 2;

/// How long a ring may ask a device to make itself findable, in seconds.
///
/// A bound, not a preference: without one a peer could ask a phone to sound
/// indefinitely, which is a nuisance a user has to physically chase down.
pub const MAX_RING_SECONDS: u16 = 60;

/// The complete vocabulary of the [`Status::network`] field.
///
/// A **frozen wire constant**: this is a closed set, and a value outside it is
/// dropped to `None` on receipt rather than being rendered. Adding a word to it
/// is therefore not free — an older peer would silently render the new word as
/// "unknown" — so it requires a feature bit, not an edit here. The words are
/// deliberately about *how this device reaches the network*, not about which
/// peer it is talking to.
pub const NETWORK_KINDS: [&str; 5] = ["lan", "wifi", "ethernet", "tailscale", "unknown"];

/// The largest meaningful `battery_percent`. A reading above this is a broken
/// collector on the sender, not a very full battery.
pub const MAX_BATTERY_PERCENT: u8 = 100;

/// The longest `app_version` a peer may have rendered.
///
/// A ceiling, not a grammar. Unlike [`NETWORK_KINDS`] there is no closed set to
/// check a version against — `0.9.0` and `1.2.3-rc.1+sha.5114f85` are both
/// honest — but a string that reaches a dashboard chip verbatim needs *some*
/// bound, and 40 characters is more than any version a build could truthfully
/// carry.
pub const MAX_APP_VERSION_LEN: usize = 40;

/// "Make yourself findable" — the message behind *find my device*.
///
/// Carries nothing but a duration, because there is nothing else to say: the
/// receiving device decides *how* to be findable (a sound, a notification, a
/// flashing window), and a sender that dictated the method would be making a
/// decision about hardware it cannot see.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ring {
    /// How long to keep signalling, in seconds. Clamped on receipt.
    pub seconds: u16,
    /// RFC 3339 send time, for logging and for a receiver that wants to ignore
    /// a request that sat in a queue.
    pub timestamp: String,
}

impl Ring {
    /// A ring for `seconds`, clamped to [`MAX_RING_SECONDS`].
    #[must_use]
    pub fn new(seconds: u16) -> Ring {
        Ring {
            seconds: seconds.clamp(1, MAX_RING_SECONDS),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// The Presence MessageType (`Ring` = 2).
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_RING)
    }

    /// Encode as a Presence-channel frame. OPTIONAL: a peer that predates
    /// ringing skips it rather than failing the channel, so asking an older
    /// device to ring costs the request and nothing else.
    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, PresenceError> {
        let payload = serde_json::to_vec(self)
            .map(Bytes::from)
            .map_err(|e| PresenceError::Serialization(e.to_string()))?;
        Ok(SessionFrame::new(
            channel,
            Self::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            payload,
        ))
    }

    /// Decode from a Presence-channel frame, clamping the duration.
    ///
    /// Clamped rather than refused: a peer asking for an hour is being
    /// unreasonable, not hostile, and refusing outright would leave a user
    /// standing next to a silent phone. One minute is the most any device is
    /// asked to make noise for.
    pub fn from_frame(frame: &SessionFrame) -> Result<Ring, PresenceError> {
        if frame.message_type.get() != MSG_RING {
            return Err(PresenceError::WrongType(frame.message_type.get()));
        }
        let mut r: Ring = serde_json::from_slice(&frame.payload)
            .map_err(|e| PresenceError::Serialization(e.to_string()))?;
        r.seconds = r.seconds.clamp(1, MAX_RING_SECONDS);
        Ok(r)
    }
}

/// Errors from encoding/decoding/validating a status message.
#[derive(Debug, thiserror::Error)]
pub enum PresenceError {
    #[error("presence serialization: {0}")]
    Serialization(String),
    #[error("unexpected presence message type {0}")]
    WrongType(u16),
    /// The whole message is discarded, deliberately — see [`Status::from_frame`].
    #[error("battery_percent out of range: {0} (max {MAX_BATTERY_PERCENT})")]
    BatteryOutOfRange(u8),
    /// Raised on **encode only**. On decode an unknown network word is dropped
    /// to `None` instead; the asymmetry is explained on [`Status::from_frame`].
    #[error("unknown network kind: {0:?}")]
    UnknownNetwork(String),
    /// Raised on **encode only**, for the same reason as [`Self::UnknownNetwork`]:
    /// on decode an implausible version is dropped to `None`.
    #[error("implausible app_version: {0:?}")]
    ImplausibleAppVersion(String),
}

impl From<PresenceError> for SessionError {
    fn from(e: PresenceError) -> Self {
        SessionError::FrameDecode(e.to_string())
    }
}

/// One device's self-reported status, as it travels on the wire.
///
/// The sender identity is NOT carried here — it is the authenticated session
/// peer, exactly as for a chat message. Every field but [`sent_at`](Self::sent_at)
/// is optional, and **absent means "this device cannot determine it"**, never
/// zero: a desktop with no battery omits `battery_percent` rather than
/// reporting `0`, and a surface must render the difference.
///
/// Nothing here is ever written to disk. Presence is live state: a restart
/// starts empty rather than showing stale numbers as current (I4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Status {
    /// 0-100. Absent when the device has no battery or cannot read one.
    #[serde(default)]
    pub battery_percent: Option<u8>,
    /// Whether the battery is charging right now. Absent when unknown, which is
    /// independent of `battery_percent` being absent.
    #[serde(default)]
    pub charging: Option<bool>,
    /// Free bytes on the volume holding the receive directory.
    #[serde(default)]
    pub storage_free_bytes: Option<u64>,
    /// How this device currently reaches the network, in its own words: one of
    /// [`NETWORK_KINDS`]. Anything else is dropped to `None` on receipt.
    #[serde(default)]
    pub network: Option<String>,
    /// The sender's own app version, so a dashboard can show version skew.
    #[serde(default)]
    pub app_version: Option<String>,
    /// RFC3339, the sender's clock.
    ///
    /// Display relative to *our* receipt time, never as absolute truth — peer
    /// clocks are not synchronised, and a peer with a wrong clock would
    /// otherwise render as "last seen in 3 hours". `PeerStatus::received_at`
    /// holds the number a surface should actually count from.
    pub sent_at: String,
}

/// Whether `network` is one of the words the wire vocabulary defines.
#[must_use]
pub fn is_known_network(network: &str) -> bool {
    NETWORK_KINDS.contains(&network)
}

/// Whether `app_version` is shaped like a version a build could actually have.
///
/// [`is_known_network`]'s counterpart for the field that has no closed set:
/// non-empty, within [`MAX_APP_VERSION_LEN`], and built only from the
/// characters versions are written with. The character rule is the load-bearing
/// half — it is what keeps markup, newlines, control characters and
/// bidirectional overrides out of a string the dashboard prints beside a
/// device's own name, where a peer could otherwise forge a whole status line.
#[must_use]
pub fn is_plausible_app_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= MAX_APP_VERSION_LEN
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
}

impl Status {
    /// The presence MessageType (`Status` = 1).
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_STATUS)
    }

    /// Encode as a Presence-channel [`SessionFrame`] on `channel`.
    ///
    /// Sent `OPTIONAL` so a peer that does not implement the type skips the
    /// message instead of failing the channel (MESSAGE_REGISTRY.md §6/§7).
    ///
    /// Validation runs on the way out as well as the way in, per §7: no
    /// PeerBeam build may emit a status its own peers would refuse or silently
    /// blank. An unknown `network` is an error here (rather than being dropped
    /// as it is on decode) precisely because *we* control what we emit — a word
    /// our own receiver would throw away is a bug in the collector, and it
    /// should fail loudly on the one side that can fix it.
    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, PresenceError> {
        self.validate_range()?;
        if let Some(net) = &self.network {
            if !is_known_network(net) {
                return Err(PresenceError::UnknownNetwork(net.clone()));
            }
        }
        if let Some(version) = &self.app_version {
            if !is_plausible_app_version(version) {
                return Err(PresenceError::ImplausibleAppVersion(version.clone()));
            }
        }
        let payload = serde_json::to_vec(self)
            .map(Bytes::from)
            .map_err(|e| PresenceError::Serialization(e.to_string()))?;
        Ok(SessionFrame::new(
            channel,
            Self::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            payload,
        ))
    }

    /// Decode from a Presence-channel frame, validating peer-supplied input.
    ///
    /// Two different failure modes, deliberately:
    ///
    /// * **`battery_percent > 100` rejects the whole message.** It is not
    ///   clamped: a collector that can report 137% is not a collector whose
    ///   storage or network readings should be believed either, and clamping
    ///   would present an invented number as a measurement. Rejecting keeps the
    ///   *previous* good reading visible, aged, which is honest.
    /// * **An unknown `network` word is dropped to `None`.** It is a
    ///   peer-supplied string that would otherwise reach the UI verbatim, so it
    ///   must never be rendered raw — but throwing away a perfectly good
    ///   battery and storage reading over one cosmetic label would be the wrong
    ///   trade. The field simply reads as unshared.
    /// * **An implausible `app_version` is dropped to `None`.** The same trade
    ///   as `network`, and for the same reason: it is peer text the dashboard
    ///   prints verbatim in a chip, and a 4 KB string or one carrying markup is
    ///   a sender bug or an attack, never a version. Dropped rather than
    ///   truncated, because a truncated version *is* a wrong version, and
    ///   showing one would be the same invention that clamping a 137% battery
    ///   would be.
    pub fn from_frame(frame: &SessionFrame) -> Result<Status, PresenceError> {
        if frame.message_type.get() != MSG_STATUS {
            return Err(PresenceError::WrongType(frame.message_type.get()));
        }
        let mut s: Status = serde_json::from_slice(&frame.payload)
            .map_err(|e| PresenceError::Serialization(e.to_string()))?;
        s.validate_range()?;
        if let Some(net) = &s.network {
            if !is_known_network(net) {
                s.network = None;
            }
        }
        if let Some(version) = &s.app_version {
            if !is_plausible_app_version(version) {
                s.app_version = None;
            }
        }
        Ok(s)
    }

    /// The one range check, shared by encode and decode so they cannot drift.
    fn validate_range(&self) -> Result<(), PresenceError> {
        match self.battery_percent {
            Some(p) if p > MAX_BATTERY_PERCENT => Err(PresenceError::BatteryOutOfRange(p)),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::session::MessageFlags;

    fn full() -> Status {
        Status {
            battery_percent: Some(72),
            charging: Some(true),
            storage_free_bytes: Some(123_456_789),
            network: Some("tailscale".into()),
            app_version: Some("0.4.1".into()),
            sent_at: "2026-08-17T10:00:00Z".into(),
        }
    }

    fn empty() -> Status {
        Status {
            sent_at: "2026-08-17T10:00:00Z".into(),
            ..Status::default()
        }
    }

    /// The brief's first test: every field present survives the frame encoding.
    #[test]
    fn a_fully_populated_status_round_trips() {
        let s = full();
        let frame = s.to_frame(ChannelId::new(4)).unwrap();
        assert_eq!(frame.message_type.get(), MSG_STATUS);
        assert_eq!(Status::from_frame(&frame).unwrap(), s);
    }

    /// And its second: every optional field absent is the *normal* case (a
    /// desktop with no battery), not an error, and must survive too.
    #[test]
    fn a_status_with_every_optional_field_absent_round_trips() {
        let s = empty();
        let frame = s.to_frame(ChannelId::new(4)).unwrap();
        let back = Status::from_frame(&frame).unwrap();
        assert_eq!(back, s);
        assert!(back.battery_percent.is_none());
        assert!(back.charging.is_none());
        assert!(back.storage_free_bytes.is_none());
        assert!(back.network.is_none());
        assert!(back.app_version.is_none());
        assert_eq!(back.sent_at, "2026-08-17T10:00:00Z");
    }

    /// A peer that omits the keys entirely (rather than sending `null`) must
    /// decode — that is what `#[serde(default)]` on every optional field buys,
    /// and it is how a future *smaller* sender will look.
    #[test]
    fn missing_keys_decode_as_absent_rather_than_failing() {
        let frame = SessionFrame::new(
            ChannelId::new(1),
            Status::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(br#"{"sent_at":"2026-08-17T10:00:00Z"}"#),
        );
        assert_eq!(Status::from_frame(&frame).unwrap(), empty());
    }

    /// An additive type ships OPTIONAL so an older peer skips it instead of
    /// failing the channel (registry §7).
    #[test]
    fn a_status_ships_optional_and_end_of_message() {
        let frame = full().to_frame(ChannelId::new(1)).unwrap();
        assert!(frame.flags.is_optional(), "additive types ship OPTIONAL");
        assert!(frame.flags.contains(MessageFlags::END_OF_MESSAGE));
    }

    #[test]
    fn from_frame_rejects_the_wrong_message_type() {
        let mut frame = full().to_frame(ChannelId::new(1)).unwrap();
        frame.message_type = MessageType::new(2);
        assert!(matches!(
            Status::from_frame(&frame),
            Err(PresenceError::WrongType(2))
        ));
    }

    /// `battery_percent: 101` is rejected and the message discarded — not
    /// clamped to 100, which would present an invented number as a measurement.
    #[test]
    fn an_out_of_range_battery_discards_the_whole_message() {
        for bad in [101u16, 137, 255] {
            let frame = SessionFrame::new(
                ChannelId::new(1),
                Status::message_type(),
                MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
                Bytes::from(
                    serde_json::to_vec(&serde_json::json!({
                        "battery_percent": bad,
                        "storage_free_bytes": 999u64,
                        "sent_at": "2026-08-17T10:00:00Z",
                    }))
                    .unwrap(),
                ),
            );
            let err = Status::from_frame(&frame).unwrap_err();
            assert!(
                matches!(err, PresenceError::BatteryOutOfRange(_)),
                "battery {bad} must be rejected, got {err:?}"
            );
        }
        // The boundary itself is valid: 100% is a full battery, not an error.
        let ok = Status {
            battery_percent: Some(100),
            ..empty()
        };
        assert_eq!(
            Status::from_frame(&ok.to_frame(ChannelId::new(1)).unwrap())
                .unwrap()
                .battery_percent,
            Some(100)
        );
    }

    /// A value that cannot even fit `u8` (a peer sending 1000) is refused by
    /// deserialization rather than wrapping to a plausible-looking 232.
    #[test]
    fn a_battery_too_large_for_u8_is_refused_not_wrapped() {
        let frame = SessionFrame::new(
            ChannelId::new(1),
            Status::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(br#"{"battery_percent":1000,"sent_at":"t"}"#),
        );
        assert!(matches!(
            Status::from_frame(&frame),
            Err(PresenceError::Serialization(_))
        ));
    }

    /// The same rule on the way out, so no PeerBeam build can emit a status its
    /// own peers would refuse (registry §7: symmetric on encode and decode).
    #[test]
    fn to_frame_refuses_an_out_of_range_battery() {
        let s = Status {
            battery_percent: Some(101),
            ..empty()
        };
        assert!(matches!(
            s.to_frame(ChannelId::new(1)),
            Err(PresenceError::BatteryOutOfRange(101))
        ));
    }

    /// An unknown `network` string renders as absent, never verbatim. This is
    /// the field a hostile peer would use to push markup or a fake status line
    /// into a dashboard, so it is dropped rather than passed through.
    #[test]
    fn an_unknown_network_word_decodes_as_absent_never_verbatim() {
        for hostile in [
            "ethernet-ish",
            "LAN",
            "<script>alert(1)</script>",
            "",
            "tailscale ",
            "🌐",
            "wifi\u{0}",
        ] {
            let frame = SessionFrame::new(
                ChannelId::new(1),
                Status::message_type(),
                MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
                Bytes::from(
                    serde_json::to_vec(&serde_json::json!({
                        "network": hostile,
                        "battery_percent": 50u8,
                        "sent_at": "2026-08-17T10:00:00Z",
                    }))
                    .unwrap(),
                ),
            );
            let decoded = Status::from_frame(&frame)
                .expect("an unknown network word must not fail the message");
            assert_eq!(
                decoded.network, None,
                "unknown network {hostile:?} reached the surface"
            );
            // The rest of the message survives — dropping one cosmetic label
            // must not cost a real reading.
            assert_eq!(decoded.battery_percent, Some(50));
        }
    }

    /// The guard cannot pass by blanking everything: each known word survives.
    #[test]
    fn every_known_network_word_survives_the_round_trip() {
        for known in NETWORK_KINDS {
            let s = Status {
                network: Some(known.to_string()),
                ..empty()
            };
            let frame = s.to_frame(ChannelId::new(1)).unwrap();
            assert_eq!(
                Status::from_frame(&frame).unwrap().network.as_deref(),
                Some(known),
                "known word {known:?} must survive"
            );
        }
        assert!(is_known_network("lan"));
        assert!(!is_known_network("lan "));
    }

    /// Encode refuses a word decode would blank, so a collector bug fails on
    /// the side that can fix it instead of silently shipping an empty field.
    #[test]
    fn to_frame_refuses_an_unknown_network_word() {
        let s = Status {
            network: Some("carrier-pigeon".into()),
            ..empty()
        };
        assert!(matches!(
            s.to_frame(ChannelId::new(1)),
            Err(PresenceError::UnknownNetwork(_))
        ));
    }

    /// The version chip is the second string a peer controls, and until now the
    /// only unbounded one: `network` was vetted against a closed set while
    /// `app_version` passed through whatever arrived. A dashboard renders it
    /// verbatim beside the device's own name, so a 4 KB string, a newline, or a
    /// tag is dropped exactly the way an unknown network word is.
    #[test]
    fn an_implausible_app_version_decodes_as_absent_never_verbatim() {
        let long = "9".repeat(MAX_APP_VERSION_LEN + 1);
        for hostile in [
            long.as_str(),
            "<script>alert(1)</script>",
            "",
            "0.4.1\nBattery 100%",
            "0.4.1 (patched)",
            "0.4.1\u{0}",
            "\u{202e}1.4.0",
            "🎉",
        ] {
            let frame = SessionFrame::new(
                ChannelId::new(1),
                Status::message_type(),
                MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
                Bytes::from(
                    serde_json::to_vec(&serde_json::json!({
                        "app_version": hostile,
                        "battery_percent": 50u8,
                        "sent_at": "2026-08-17T10:00:00Z",
                    }))
                    .unwrap(),
                ),
            );
            let decoded = Status::from_frame(&frame)
                .expect("an implausible app_version must not fail the message");
            assert_eq!(
                decoded.app_version, None,
                "app_version {hostile:?} reached the surface"
            );
            // Same trade as `network`: one cosmetic label must not cost a real
            // reading.
            assert_eq!(decoded.battery_percent, Some(50));
        }
    }

    /// The guard cannot pass by blanking every version: the shapes a build
    /// actually carries survive, including a full pre-release with build
    /// metadata, and the length limit itself is inclusive.
    #[test]
    fn a_real_version_survives_the_round_trip() {
        let at_limit = format!("1.2.3-rc.1+{}", "a".repeat(MAX_APP_VERSION_LEN - 11));
        assert_eq!(at_limit.len(), MAX_APP_VERSION_LEN);
        for real in [
            "0.9.0",
            "0.4.1",
            "1.2.3-rc.1+sha.5114f85",
            "2026.08.19",
            "0.9.0_nightly",
            at_limit.as_str(),
        ] {
            let s = Status {
                app_version: Some(real.to_string()),
                ..empty()
            };
            let frame = s
                .to_frame(ChannelId::new(1))
                .expect("a real version must encode");
            assert_eq!(
                Status::from_frame(&frame).unwrap().app_version.as_deref(),
                Some(real),
                "real version {real:?} must survive"
            );
        }
        // The version this build ships is the one that matters most: a bound
        // that rejected our own would blank the field on every peer.
        assert!(is_plausible_app_version(env!("CARGO_PKG_VERSION")));
    }

    /// A version too long is dropped whole, never shortened. `0.4.1` truncated
    /// out of `0.4.11` is a *wrong* version presented as a fact — the same
    /// invention that clamping a 137% battery would be.
    #[test]
    fn an_over_long_app_version_is_dropped_not_truncated() {
        let too_long = format!("{}xyz", "1.2.3-".repeat(8));
        assert!(too_long.len() > MAX_APP_VERSION_LEN);
        let frame = SessionFrame::new(
            ChannelId::new(1),
            Status::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from(
                serde_json::to_vec(&serde_json::json!({
                    "app_version": too_long,
                    "sent_at": "2026-08-17T10:00:00Z",
                }))
                .unwrap(),
            ),
        );
        let decoded = Status::from_frame(&frame).unwrap();
        assert_eq!(decoded.app_version, None);
    }

    /// Encode refuses what decode would blank, so a collector shipping a
    /// malformed version fails on the side that can fix it rather than
    /// broadcasting a field every peer silently drops (registry §7).
    #[test]
    fn to_frame_refuses_an_implausible_app_version() {
        for bad in ["", "0.4.1 (dev)", &"9".repeat(MAX_APP_VERSION_LEN + 1)] {
            let s = Status {
                app_version: Some(bad.to_string()),
                ..empty()
            };
            assert!(
                matches!(
                    s.to_frame(ChannelId::new(1)),
                    Err(PresenceError::ImplausibleAppVersion(_))
                ),
                "encode must refuse {bad:?}"
            );
        }
    }

    /// The wire shape is exactly these six keys. A status is broadcast to every
    /// trusted device on a timer, so a field joining it quietly is a field
    /// leaving this machine sixty times an hour — pinning the key set is what
    /// makes that impossible to do by accident.
    #[test]
    fn the_status_wire_shape_is_exactly_its_six_fields() {
        let frame = full().to_frame(ChannelId::new(1)).unwrap();
        let json = String::from_utf8(frame.payload.to_vec()).unwrap();
        let object: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut keys: Vec<String> = object
            .as_object()
            .expect("a Status frame is a JSON object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "app_version".to_string(),
                "battery_percent".to_string(),
                "charging".to_string(),
                "network".to_string(),
                "sent_at".to_string(),
                "storage_free_bytes".to_string(),
            ],
            "the Status wire shape gained or lost a field: {json}"
        );
    }

    /// Free bytes on a volume, and nothing that would identify the volume. A
    /// path here would leak the sender's filesystem layout to every trusted
    /// device on a 60-second timer, which is the same mistake `FileRef` was
    /// built to avoid.
    #[test]
    fn the_wire_shape_carries_no_path() {
        const SECRET: &str = "/home/alice/Private/PeerBeam";
        let s = Status {
            storage_free_bytes: peerbeam_platform::available_bytes(SECRET),
            ..full()
        };
        let frame = s.to_frame(ChannelId::new(1)).unwrap();
        let json = String::from_utf8(frame.payload.to_vec()).unwrap();
        assert!(!json.contains("/home/alice"), "leaked a path: {json}");
        assert!(!json.contains("directory"), "leaked a path key: {json}");
    }
}
