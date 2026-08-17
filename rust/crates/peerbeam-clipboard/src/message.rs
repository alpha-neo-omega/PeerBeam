//! The wire clip message carried on the Clipboard channel.

use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use peerbeam_domain::session::{ChannelId, MessageFlags, MessageType, SessionError, SessionFrame};

/// MessageType id for a clip within the Clipboard channel namespace
/// (`docs/MESSAGE_REGISTRY.md` §4, Clipboard `Clip = 1`).
pub const MSG_CLIP: u16 = 1;

/// Maximum clip size (UTF-8 bytes).
///
/// A **frozen wire constant**, on the same terms as `peerbeam_chat::MAX_BODY`:
/// raising it later is a breaking change for any peer still on the old cap —
/// that peer's decoder refuses the over-cap frame as
/// [`ClipboardError::TooLarge`] and, per registry §6, closes the Clipboard
/// channel — so it requires a new feature bit, not a silent bump.
///
/// 64 KiB, four times chat's 16 KiB, because the two are used differently. A
/// chat body is something a person typed; a clipboard routinely holds a whole
/// file's worth of code, and 64 KiB is roughly a thousand lines at 64 columns —
/// comfortably past the snippet, config, URL and token cases this feature
/// exists for.
///
/// It is bounded at all because **nothing here is a deliberate send**. A user
/// presses no button: the watcher pushes whatever was copied, to every trusted
/// device, automatically. Without a cap an accidental `Ctrl+A Ctrl+C` in a
/// 200 MB log replicates across the fleet with nobody having asked for it. The
/// cap is the difference between "a clipboard, synced" and "an unattended file
/// transfer with no consent step".
pub const MAX_CLIP: usize = 65_536;

/// Errors from encoding/decoding/validating a clip.
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    /// The clip is **skipped**, never truncated — see [`Clip::validate`].
    #[error("clip too large: {len} bytes (max {MAX_CLIP})")]
    TooLarge { len: usize },
    /// Includes a payload that is not valid UTF-8: `serde_json::from_slice`
    /// refuses it outright rather than lossily replacing the bad bytes, which
    /// is the behaviour this type wants and a test pins.
    #[error("clip serialization: {0}")]
    Serialization(String),
    #[error("unexpected clipboard message type {0}")]
    WrongType(u16),
    /// An empty clip is refused — see [`Clip::validate`].
    #[error("clip is empty")]
    Empty,
}

impl From<ClipboardError> for SessionError {
    fn from(e: ClipboardError) -> Self {
        SessionError::FrameDecode(e.to_string())
    }
}

/// One clipboard payload, as it travels on the wire.
///
/// The sender identity is NOT carried here — it is the authenticated session
/// peer, exactly as for a chat message or a status.
///
/// Nothing here is ever written to disk. A synced clipboard is live state, and
/// persisting it would turn an opt-in convenience into a durable log of
/// everything the user ever copied — passwords included, since (see the crate
/// docs) there is no way to tell which those were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clip {
    /// UTF-8 text. The only kind carried today.
    ///
    /// Images and files are **not** in scope and must not be smuggled through
    /// this field: a receiver writes it to the system clipboard as plain text
    /// and nothing else, so a base64 blob here would arrive as a screenful of
    /// base64, not as a picture. Adding a kind means a new MessageType and a
    /// new feature bit.
    pub text: String,
    /// RFC3339, the sender's clock.
    ///
    /// Used for display and ordering only — never trusted as absolute, since
    /// peer clocks are not synchronised. Nothing in the receive path branches
    /// on it; a clip is applied because it arrived, not because of when it
    /// claims to have been sent.
    pub sent_at: String,
}

impl Clip {
    /// Create a clip from local clipboard text, minting the timestamp.
    /// Rejects an over-cap or empty payload.
    pub fn new(text: &str) -> Result<Clip, ClipboardError> {
        let clip = Clip {
            text: text.to_string(),
            sent_at: Utc::now().to_rfc3339(),
        };
        clip.validate()?;
        Ok(clip)
    }

    /// The clipboard MessageType (`Clip` = 1).
    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_CLIP)
    }

    /// Encode as a Clipboard-channel [`SessionFrame`] on `channel`.
    ///
    /// Sent `OPTIONAL` so a peer that does not implement the type skips the
    /// message instead of failing the channel (MESSAGE_REGISTRY.md §6/§7).
    ///
    /// Validation runs on the way out as well as the way in, per §7, so no
    /// PeerBeam build can emit a clip its own peers would refuse.
    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, ClipboardError> {
        self.validate()?;
        let payload = serde_json::to_vec(self)
            .map(Bytes::from)
            .map_err(|e| ClipboardError::Serialization(e.to_string()))?;
        Ok(SessionFrame::new(
            channel,
            Self::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            payload,
        ))
    }

    /// Decode from a Clipboard-channel frame, validating peer-supplied input.
    ///
    /// A non-UTF-8 payload never reaches [`validate`](Self::validate): JSON is
    /// UTF-8 by definition and `from_slice` refuses invalid bytes rather than
    /// replacing them, so the message is rejected instead of arriving mangled.
    pub fn from_frame(frame: &SessionFrame) -> Result<Clip, ClipboardError> {
        if frame.message_type.get() != MSG_CLIP {
            return Err(ClipboardError::WrongType(frame.message_type.get()));
        }
        let clip: Clip = serde_json::from_slice(&frame.payload)
            .map_err(|e| ClipboardError::Serialization(e.to_string()))?;
        clip.validate()?;
        Ok(clip)
    }

    /// The one validation, shared by encode and decode so they cannot drift.
    ///
    /// Two rules, and both **reject rather than repair**:
    ///
    /// * **Over-cap is skipped, never truncated.** A truncated clipboard is the
    ///   worst possible outcome: the user believes they copied a whole file and
    ///   pastes 64 KiB of it somewhere that mattered, with nothing to indicate
    ///   the tail is missing. Silent corruption of what the user thinks they
    ///   hold is worse than not syncing at all, so an over-cap clip is dropped
    ///   and the user is told.
    /// * **Empty is refused.** An empty clip carries no content and applying
    ///   one would *erase* every trusted device's clipboard — a destructive act
    ///   with no user intent behind it. A momentarily-empty read (a just-booted
    ///   session, an app clearing its own selection) must never propagate.
    fn validate(&self) -> Result<(), ClipboardError> {
        if self.text.is_empty() {
            return Err(ClipboardError::Empty);
        }
        if self.text.len() > MAX_CLIP {
            return Err(ClipboardError::TooLarge {
                len: self.text.len(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(text: &str) -> Clip {
        Clip {
            text: text.to_string(),
            sent_at: "2026-08-17T10:00:00Z".into(),
        }
    }

    #[test]
    fn a_clip_round_trips() {
        let c = clip("hello from the other side");
        let frame = c.to_frame(ChannelId::new(4)).unwrap();
        assert_eq!(frame.message_type.get(), MSG_CLIP);
        assert_eq!(Clip::from_frame(&frame).unwrap(), c);
    }

    /// Clipboard text is arbitrary: newlines, tabs, emoji, NUL, RTL marks and
    /// anything that looks like markup. All of it must survive **byte for
    /// byte** — a clipboard that quietly normalises what you copied is broken,
    /// and the markup cases are also the ones a hostile peer would try, so
    /// pinning verbatim delivery is what lets the receiver commit to treating
    /// the text as text and never as anything else.
    #[test]
    fn arbitrary_text_survives_verbatim() {
        for text in [
            "line one\nline two\r\n\ttabbed",
            "🌐 emoji and ünïcödé",
            "<script>alert(1)</script>",
            "**markdown** `code` [link](http://x)",
            "trailing whitespace   ",
            "   leading whitespace",
            "nul\u{0}inside",
            "\u{202e}rtl override",
            "{\"looks\":\"like json\"}",
            " ", // a single space is real content, not emptiness
        ] {
            let frame = clip(text).to_frame(ChannelId::new(1)).unwrap();
            assert_eq!(
                Clip::from_frame(&frame).unwrap().text,
                text,
                "clipboard text was altered in transit: {text:?}"
            );
        }
    }

    /// The cap is a frozen wire constant (docs/MESSAGE_REGISTRY.md §4,
    /// Clipboard `Clip`). Changing it breaks every peer on the old value, so it
    /// is pinned here exactly as `peerbeam_chat::MAX_BODY` is.
    #[test]
    fn the_clip_cap_is_sixty_four_kib() {
        assert_eq!(MAX_CLIP, 65_536);
    }

    /// **Over-cap is refused, not truncated** — on both sides. The truncation
    /// assertion is the load-bearing half: a build that "fixed" this by
    /// clamping would still return `Ok`, and only the length check would catch
    /// it.
    #[test]
    fn an_over_cap_clip_is_refused_and_never_truncated() {
        let big = "x".repeat(MAX_CLIP + 1);

        // Encode side.
        assert!(matches!(
            Clip::new(&big),
            Err(ClipboardError::TooLarge { len }) if len == MAX_CLIP + 1
        ));
        assert!(matches!(
            clip(&big).to_frame(ChannelId::new(1)),
            Err(ClipboardError::TooLarge { .. })
        ));

        // Decode side: a peer that ignores the cap is refused here.
        let frame = SessionFrame::new(
            ChannelId::new(1),
            Clip::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from(
                serde_json::to_vec(&serde_json::json!({
                    "text": big,
                    "sent_at": "2026-08-17T10:00:00Z",
                }))
                .unwrap(),
            ),
        );
        match Clip::from_frame(&frame) {
            Err(ClipboardError::TooLarge { len }) => assert_eq!(len, MAX_CLIP + 1),
            Ok(c) => panic!(
                "an over-cap clip was accepted at {} bytes — truncating a \
                 clipboard silently corrupts what the user thinks they copied",
                c.text.len()
            ),
            Err(e) => panic!("wrong error: {e:?}"),
        }
    }

    /// The boundary itself is valid: exactly `MAX_CLIP` bytes is a legal clip,
    /// so the refusal above is a cap and not an off-by-one.
    #[test]
    fn a_clip_of_exactly_the_cap_is_accepted() {
        let exact = "y".repeat(MAX_CLIP);
        let c = Clip::new(&exact).expect("the boundary is inclusive");
        let frame = c.to_frame(ChannelId::new(1)).unwrap();
        assert_eq!(Clip::from_frame(&frame).unwrap().text.len(), MAX_CLIP);
    }

    /// The cap counts **bytes, not characters** — the same unit the wire uses.
    /// A multi-byte string that fits in `MAX_CLIP` chars but not `MAX_CLIP`
    /// bytes must be refused, or a peer whose decoder counts bytes would reject
    /// what this one emitted.
    #[test]
    fn the_cap_counts_bytes_not_characters() {
        let multibyte = "é".repeat(MAX_CLIP); // 2 bytes each
        assert_eq!(multibyte.chars().count(), MAX_CLIP);
        assert!(matches!(
            Clip::new(&multibyte),
            Err(ClipboardError::TooLarge { len }) if len == MAX_CLIP * 2
        ));
    }

    /// An empty clip is refused on both sides: applying one would erase the
    /// peer's clipboard, and nobody asked for that.
    #[test]
    fn an_empty_clip_is_refused_rather_than_erasing_the_peers_clipboard() {
        assert!(matches!(Clip::new(""), Err(ClipboardError::Empty)));
        assert!(matches!(
            clip("").to_frame(ChannelId::new(1)),
            Err(ClipboardError::Empty)
        ));
        let frame = SessionFrame::new(
            ChannelId::new(1),
            Clip::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(br#"{"text":"","sent_at":"t"}"#),
        );
        assert!(matches!(
            Clip::from_frame(&frame),
            Err(ClipboardError::Empty)
        ));
    }

    /// A payload that is not valid UTF-8 is **rejected**, not lossily decoded.
    /// A clipboard that pastes `\u{FFFD}` where a byte used to be has corrupted
    /// the thing it was asked to carry.
    #[test]
    fn a_non_utf8_payload_is_rejected_rather_than_replaced() {
        // Valid JSON shape, invalid UTF-8 inside the string.
        let mut bad = br#"{"text":"ab"#.to_vec();
        bad.push(0xFF);
        bad.extend_from_slice(br#"","sent_at":"t"}"#);
        let frame = SessionFrame::new(
            ChannelId::new(1),
            Clip::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from(bad),
        );
        match Clip::from_frame(&frame) {
            Err(ClipboardError::Serialization(_)) => {}
            Ok(c) => panic!("invalid UTF-8 decoded to {:?} instead of failing", c.text),
            Err(e) => panic!("wrong error: {e:?}"),
        }
    }

    /// An additive type ships OPTIONAL so an older peer skips it instead of
    /// failing the channel (registry §7).
    #[test]
    fn a_clip_ships_optional_and_end_of_message() {
        let frame = clip("hi").to_frame(ChannelId::new(1)).unwrap();
        assert!(frame.flags.is_optional(), "additive types ship OPTIONAL");
        assert!(frame.flags.contains(MessageFlags::END_OF_MESSAGE));
    }

    #[test]
    fn from_frame_rejects_the_wrong_message_type() {
        let mut frame = clip("hi").to_frame(ChannelId::new(1)).unwrap();
        frame.message_type = MessageType::new(2);
        assert!(matches!(
            Clip::from_frame(&frame),
            Err(ClipboardError::WrongType(2))
        ));
    }

    #[test]
    fn from_frame_rejects_malformed_json() {
        let frame = SessionFrame::new(
            ChannelId::new(1),
            Clip::message_type(),
            MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
            Bytes::from_static(b"not json"),
        );
        assert!(matches!(
            Clip::from_frame(&frame),
            Err(ClipboardError::Serialization(_))
        ));
    }

    /// The wire shape is exactly these two keys. Every copy the user makes is
    /// pushed automatically, so a field joining this struct quietly is a field
    /// leaving this machine on every `Ctrl+C` — pinning the key set is what
    /// makes adding one impossible to do by accident. In particular there is no
    /// source application, no window title and no device-local path here: what
    /// was copied is the payload, where it came from is not.
    #[test]
    fn the_clip_wire_shape_is_exactly_its_two_fields() {
        let frame = clip("hi").to_frame(ChannelId::new(1)).unwrap();
        let json = String::from_utf8(frame.payload.to_vec()).unwrap();
        let object: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut keys: Vec<String> = object
            .as_object()
            .expect("a Clip frame is a JSON object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["sent_at".to_string(), "text".to_string()],
            "the Clip wire shape gained or lost a field: {json}"
        );
    }

    /// `new` mints an RFC3339 timestamp the peer can parse. It is display-only,
    /// but an unparseable one would still be a bug on the wire.
    #[test]
    fn new_mints_a_parseable_rfc3339_timestamp() {
        let c = Clip::new("hi").unwrap();
        assert!(
            chrono::DateTime::parse_from_rfc3339(&c.sent_at).is_ok(),
            "not RFC3339: {}",
            c.sent_at
        );
    }
}
