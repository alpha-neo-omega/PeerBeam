//! Version and capability negotiation — pure logic, no IO.
//!
//! The session layer exchanges each side's [`Version`] and [`CapabilitySet`] and
//! reduces them to a shared agreement with the functions here. Behaviour is
//! chosen by negotiated *capability*, never by sniffing a version number, so a
//! peer missing a capability simply never uses it.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::ids::ChannelType;

/// Protocol version: `major.minor`. Peers must share a `major`; a higher `minor`
/// is backward-compatible (additive only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    /// Framing/semantics contract. Must match between peers.
    pub major: u16,
    /// Additive extensions. Any minor talks to any minor of the same major.
    pub minor: u16,
}

impl Version {
    /// The protocol version this build speaks.
    pub const CURRENT: Version = Version { major: 1, minor: 0 };

    /// Construct a version.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Version { major, minor }
    }

    /// Whether two versions can interoperate (share a major).
    #[must_use]
    pub fn is_compatible_with(&self, other: &Version) -> bool {
        self.major == other.major
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The outcome of negotiating two versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionNegotiation {
    /// A compatible version was agreed (highest common minor).
    Agreed(Version),
    /// No common major — the peers cannot interoperate.
    Incompatible {
        /// This side's version.
        local: Version,
        /// The peer's version.
        peer: Version,
    },
}

/// Negotiate a shared version: the common major at the lower of the two minors,
/// or [`Incompatible`](VersionNegotiation::Incompatible) if the majors differ.
#[must_use]
pub fn negotiate_version(local: Version, peer: Version) -> VersionNegotiation {
    if local.major == peer.major {
        VersionNegotiation::Agreed(Version {
            major: local.major,
            minor: local.minor.min(peer.minor),
        })
    } else {
        VersionNegotiation::Incompatible { local, peer }
    }
}

/// Feature bit on the CHAT capability: this peer understands the `FileRef`
/// message (chat MessageType 2) and can correlate it with a transfer.
///
/// `Capability.features` is already on the wire and `CapabilitySet::intersect`
/// already ANDs the bits, so advertising this is not a wire change: a peer from
/// before this feature advertises `features: 0`, the intersection clears the
/// bit, and a sender simply never offers it a `FileRef`.
pub const CHAT_FEAT_FILEREF: u32 = 1 << 0;

/// Feature bit on the CHAT capability: this peer understands and handles the
/// `FileDecline` message (chat MessageType 3) — telling it "the file you offered
/// under this id was turned down" will mean something to it.
///
/// **A receive capability, like [`CHAT_FEAT_FILEREF`], and read as one**: the
/// party that consults it is the one *sending* a `FileDecline`, which asks
/// whether the PEER negotiated the bit before putting the message on the wire
/// (`peerbeam_ffi::transfer::should_send_decline`). It therefore says nothing
/// about whether the advertiser ever sends a decline of its own — a build with
/// no decline action to offer its user still advertises this truthfully, because
/// what it asserts is comprehension, not behaviour. Every capability bit in this
/// module reads that way; see `docs/MESSAGE_REGISTRY.md`.
///
/// Without it a sender cannot tell "you declined" from "the network dropped",
/// so a refused file would be re-offered forever and re-prompt its receiver
/// every time. A peer that does not advertise this is handled by the sender's
/// bounded backstop instead.
///
/// Like [`CHAT_FEAT_FILEREF`], advertising it is not a wire change: it rides
/// the `Capability.features` bitset that is already on the wire, and
/// [`CapabilitySet::intersect`] ANDs it away against a peer that predates it.
pub const CHAT_FEAT_FILEDECLINE: u32 = 1 << 1;

/// Feature bit on the CHAT capability: this peer understands the `Reaction`
/// message (chat MessageType 4) — an emoji attached to one of its messages will
/// mean something to it.
///
/// **A receive capability, read like [`CHAT_FEAT_FILEREF`]**: the party that
/// consults it is the one *sending* a reaction, asking whether the peer
/// negotiated the bit before putting the message on the wire. It asserts
/// comprehension, not behaviour — a build with no way to react still advertises
/// it truthfully if it can display one it is sent.
///
/// Without it a reaction sent to an older peer is simply dropped on arrival
/// (the frame is OPTIONAL), which costs the reaction and nothing else. The bit
/// exists so a sender can decline to offer the gesture at all rather than let a
/// user believe it landed.
///
/// Advertising it is not a wire change: it rides the `Capability.features`
/// bitset already on the wire, and [`CapabilitySet::intersect`] ANDs it away
/// against a peer that predates it.
pub const CHAT_FEAT_REACTION: u32 = 1 << 2;

/// Feature bit on the CHAT capability: this peer understands the `Receipt`
/// message (chat MessageType 5) — telling it "I have read your messages up to
/// here" will mean something.
///
/// **A receive capability, read like [`CHAT_FEAT_FILEREF`]**: it asserts
/// comprehension, not behaviour. A build advertises it if it can *apply* a
/// receipt, regardless of whether its user has opted into *sending* one —
/// those are different questions, and conflating them would make a privacy
/// setting visible on the wire. Whether this device sends receipts is
/// `DeviceConfig::share_read_receipts`, which is nobody else's business.
///
/// Advertising it is not a wire change: it rides the `Capability.features`
/// bitset already on the wire, and [`CapabilitySet::intersect`] ANDs it away
/// against a peer that predates it.
pub const CHAT_FEAT_RECEIPT: u32 = 1 << 3;

/// Feature bit on the TRANSFER capability: this peer answers a completed
/// **folder** transfer with a `Received` acknowledgement.
///
/// Comprehension, not consent — like every bit here. What it buys is the one
/// guarantee a folder send never had: the single-file path ends with
/// `Complete { checksum }` and blocks on the receiver's verdict, so a sender
/// that returns `Completed` knows the bytes landed. The folder path had no
/// acknowledgement at all, so it reported success once the last frame reached
/// the stream's send buffer — and the session then closed the shared QUIC
/// connection, which quinn documents as licence for the peer to discard stream
/// data it has not yet handed to the application. A folder could therefore be
/// reported sent while its tail was thrown away.
///
/// Both sides read the **negotiated** (intersected) set, so a peer that
/// predates this bit ANDs it away and both ends behave exactly as they did
/// before: no acknowledgement is sent, none is expected, and nothing on the
/// wire changes. That is what keeps this additive rather than a break — a
/// receiver must never send a frame an older sender would fail to parse, and
/// `FolderMessage` is externally tagged, so an unknown variant is an error
/// rather than something to skip.
pub const TRANSFER_FEAT_FOLDER_ACK: u32 = 1 << 0;

/// Feature bit on the NOTES capability: this peer understands the `NoteBatch`
/// message (notes MessageType 1) — sending it notes will mean something.
///
/// Read like every other feature bit: it asserts comprehension, not consent.
/// Whether notes are actually exchanged with a given device is
/// `Permission::Notes` in the trust store, which is the user's decision and is
/// checked on both sides of every exchange.
pub const NOTES_FEAT_SYNC: u32 = 1 << 0;

/// Feature bit on the BROWSE capability: this peer understands `ListRequest`
/// and answers with `ListResponse`.
///
/// Comprehension, not consent. Whether a device answers is `Permission::Browse`
/// **and** whether it has actually shared any folder — with no shares
/// configured, a permitted peer still sees nothing, which is the default state.
pub const BROWSE_FEAT_LIST: u32 = 1 << 0;

/// Feature bit on the SYNC capability: this peer answers `ManifestRequest` with
/// a `Manifest`, and honours `FileRequest` by sending the file over Transfer.
///
/// Comprehension, not consent. Whether it answers at all is
/// `Permission::Browse` for the manifest — the same "may you see what exists"
/// question browsing asks — and `Permission::Files` before any bytes move,
/// because being allowed to see a name is not being allowed to receive the
/// file.
pub const SYNC_FEAT_MANIFEST: u32 = 1 << 0;

/// Feature bit on the CLIPBOARD capability: this peer understands the `Clip`
/// message (clipboard MessageType 1) — a synced clipboard sent to it will mean
/// something.
///
/// **A receive capability, read exactly like the bits above**: the party that
/// consults it is the one *sending* a `Clip`, asking whether the PEER
/// negotiated the bit before putting one on the wire. It asserts comprehension,
/// not behaviour — and here that separation is not a nicety but a platform
/// fact. Android 10+ forbids reading the clipboard from the background, so a
/// phone can never auto-*send*; it advertises this bit truthfully all the same,
/// because it applies an incoming clip perfectly well. Desktop sends, every
/// platform receives.
///
/// It rides the `Capability.features` bitset already on the wire, so
/// advertising it is not a wire change: a peer that predates Clipboard never
/// advertises the CLIPBOARD channel at all, `CapabilitySet::intersect` drops
/// it, and no `Clip` is ever sent to it. A peer that advertises CLIPBOARD with
/// `features: 0` has the bit ANDed away and is likewise sent nothing — it
/// simply does not take part in sync, which is not an error.
///
/// As with `PRESENCE_FEAT_STATUS`, the feature bit is the *third* of three
/// independent gates on a send; the other two — the opt-in setting and the
/// trusted-only rule — are local privacy decisions and are not negotiable by a
/// peer. See `peerbeam_clipboard::may_share_clip`.
pub const CLIPBOARD_FEAT_CLIP: u32 = 1 << 0;

/// Feature bit on the PRESENCE capability: this peer understands the `Status`
/// message (presence MessageType 1) — device status heartbeats sent to it will
/// mean something.
///
/// **A receive capability, read exactly like the two CHAT bits above**: the
/// party that consults it is the one *sending* a `Status`, asking whether the
/// PEER negotiated the bit before putting one on the wire. It asserts
/// comprehension, not behaviour — a device with the opt-in setting off still
/// advertises this truthfully, because it can display an incoming status
/// perfectly well while sending none of its own. That separation is the whole
/// point: receiving is unconditional, sending is gated.
///
/// It rides the `Capability.features` bitset already on the wire, so
/// advertising it is not a wire change: a peer that predates Presence never
/// advertises the PRESENCE channel at all, `CapabilitySet::intersect` drops it,
/// and no `Status` is ever sent to it. A peer that advertises PRESENCE with
/// `features: 0` has the bit ANDed away and is likewise sent nothing — it shows
/// as "status not shared" rather than as an error.
///
/// Note the feature bit is the *third* of three independent gates on a send;
/// the other two — the opt-in setting and the trusted-only rule — are local
/// privacy decisions and are not negotiable by a peer. See
/// `peerbeam_presence::may_share_status`.
pub const PRESENCE_FEAT_STATUS: u32 = 1 << 0;

/// Feature bit on the PRESENCE capability: this peer understands the `Ring`
/// message (presence MessageType 2) — asking it to make itself findable will
/// mean something.
///
/// Comprehension, not consent, like every feature bit. Whether a device will
/// actually ring is `Permission::Presence` in the trust store, checked on both
/// sides.
pub const PRESENCE_FEAT_RING: u32 = 1 << 1;

/// Feature bit on the PIPE capability: this peer understands an inbound
/// **byte stream** on the Pipe channel (`0x0107`) — opening one and writing
/// chunks at it will mean something.
///
/// **A receive capability, read exactly like the bits above**: the party that
/// consults it is the one about to *open* a pipe, asking whether the PEER
/// negotiated the bit before it starts reading stdin. It asserts comprehension,
/// not behaviour, and here that split is unusually wide — every PeerBeam build
/// advertises it, including the Flutter GUI, which understands the channel
/// perfectly well and then refuses every pipe offered to it because a GUI has
/// no stdout to write bytes to. Advertising uniformly is deliberate: a peer's
/// behaviour must not depend on which of PeerBeam's two frontends it reached,
/// which is exactly the bug 2a shipped with `CHAT_FEAT_FILEREF`.
///
/// So this bit answers **"can these bytes be framed and understood?"**, never
/// **"will they be accepted?"**. Acceptance is a separate, local decision made
/// per inbound pipe by `peerbeam_transfer::may_accept_pipe`, and a peer has no
/// say in it. A sender that sees the bit and is then refused has learned only
/// that the receiver is not running `peerbeam pipe --listen`.
///
/// It rides the `Capability.features` bitset already on the wire, so
/// advertising it is not a wire change: a peer that predates Pipe never
/// advertises the PIPE channel at all, `CapabilitySet::intersect` drops it, and
/// `pipe --to` refuses up front — before it reads a byte of stdin — rather than
/// opening a channel that peer would reject.
pub const PIPE_FEAT_STREAM: u32 = 1 << 0;

/// One advertised capability: a channel type the peer supports, with a bitset of
/// optional per-capability feature flags (`0` = base capability, no extras).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    /// The channel type supported.
    pub channel: ChannelType,
    /// Optional feature flags for this capability (additive; unknown bits are
    /// ignored by a peer that does not understand them).
    pub features: u32,
}

impl Capability {
    /// A base capability with no extra features.
    #[must_use]
    pub const fn new(channel: ChannelType) -> Self {
        Capability {
            channel,
            features: 0,
        }
    }

    /// A capability with feature flags.
    #[must_use]
    pub const fn with_features(channel: ChannelType, features: u32) -> Self {
        Capability { channel, features }
    }
}

/// A set of advertised capabilities, kept sorted by channel type and unique.
///
/// Stored as a vector (not a map) so it serializes to a JSON array with valid
/// keys, and so ordering is canonical for reproducible negotiation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CapabilitySet {
    caps: Vec<Capability>,
}

impl<'de> Deserialize<'de> for CapabilitySet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Normalize on decode: `supports`/`features`/`intersect` binary-search the
        // vector, which is only valid when it is sorted and unique. A peer from a
        // different/older/re-implemented build may serialize capabilities in any
        // order, so rebuild through `insert` to re-establish the invariant rather
        // than trusting the wire order. Mirror the derived `Serialize` shape
        // (`{ "caps": [...] }`) so the wire format is unchanged.
        #[derive(Deserialize)]
        struct Raw {
            caps: Vec<Capability>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let mut set = CapabilitySet::new();
        for cap in raw.caps {
            set.insert(cap);
        }
        Ok(set)
    }
}

impl CapabilitySet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        CapabilitySet { caps: Vec::new() }
    }

    /// Insert or replace a capability, preserving sorted-unique order.
    pub fn insert(&mut self, cap: Capability) {
        match self.caps.binary_search_by_key(&cap.channel, |c| c.channel) {
            Ok(idx) => self.caps[idx] = cap,
            Err(idx) => self.caps.insert(idx, cap),
        }
    }

    /// Builder-style insert.
    #[must_use]
    pub fn with(mut self, cap: Capability) -> Self {
        self.insert(cap);
        self
    }

    /// Whether `channel` is supported.
    #[must_use]
    pub fn supports(&self, channel: ChannelType) -> bool {
        self.features(channel).is_some()
    }

    /// The feature flags advertised for `channel`, if supported.
    #[must_use]
    pub fn features(&self, channel: ChannelType) -> Option<u32> {
        self.caps
            .binary_search_by_key(&channel, |c| c.channel)
            .ok()
            .map(|idx| self.caps[idx].features)
    }

    /// The agreed capabilities: channels supported by *both* sides, with feature
    /// flags reduced to the bits both advertise (`self.features & other.features`).
    #[must_use]
    pub fn intersect(&self, other: &CapabilitySet) -> CapabilitySet {
        let mut out = CapabilitySet::new();
        for cap in &self.caps {
            if let Some(their_features) = other.features(cap.channel) {
                out.insert(Capability::with_features(
                    cap.channel,
                    cap.features & their_features,
                ));
            }
        }
        out
    }

    /// Number of capabilities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.caps.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.caps.is_empty()
    }

    /// Iterate the capabilities in canonical (channel-sorted) order.
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.caps.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatible_versions_agree_on_lower_minor() {
        let a = Version::new(1, 5);
        let b = Version::new(1, 2);
        assert_eq!(
            negotiate_version(a, b),
            VersionNegotiation::Agreed(Version::new(1, 2))
        );
        assert_eq!(
            negotiate_version(b, a),
            VersionNegotiation::Agreed(Version::new(1, 2))
        );
    }

    #[test]
    fn incompatible_majors_do_not_agree() {
        let a = Version::new(1, 0);
        let b = Version::new(2, 0);
        assert_eq!(
            negotiate_version(a, b),
            VersionNegotiation::Incompatible { local: a, peer: b }
        );
        assert!(!a.is_compatible_with(&b));
        assert!(a.is_compatible_with(&Version::new(1, 9)));
    }

    #[test]
    fn capability_set_is_sorted_and_unique() {
        let mut set = CapabilitySet::new();
        set.insert(Capability::new(ChannelType::TRANSFER));
        set.insert(Capability::new(ChannelType::CONTROL));
        set.insert(Capability::with_features(ChannelType::CONTROL, 0b10)); // replace
        let channels: Vec<u16> = set.iter().map(|c| c.channel.get()).collect();
        assert_eq!(channels, vec![0x0000, 0x0100]);
        assert_eq!(set.features(ChannelType::CONTROL), Some(0b10));
        assert!(set.supports(ChannelType::TRANSFER));
    }

    #[test]
    fn deserialize_normalizes_unsorted_wire_order() {
        // A peer that serializes capabilities in non-ascending channel order must
        // still negotiate correctly: deserialize re-sorts so binary_search works.
        let json = r#"{"caps":[{"channel":256,"features":0},{"channel":0,"features":3}]}"#;
        let set: CapabilitySet = serde_json::from_str(json).expect("deserialize");
        let channels: Vec<u16> = set.iter().map(|c| c.channel.get()).collect();
        assert_eq!(channels, vec![0x0000, 0x0100], "re-sorted ascending");
        assert!(
            set.supports(ChannelType::TRANSFER),
            "0x0100 found after re-sort"
        );
        assert_eq!(set.features(ChannelType::CONTROL), Some(3));

        // Round-trip: the custom Deserialize must accept exactly what Serialize
        // emits (the wire shape used by SessionHello), or handshakes break.
        let original = CapabilitySet::new()
            .with(Capability::with_features(ChannelType::CONTROL, 3))
            .with(Capability::new(ChannelType::TRANSFER));
        let wire = serde_json::to_string(&original).expect("serialize");
        let back: CapabilitySet = serde_json::from_str(&wire).expect("round-trip deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn intersect_keeps_common_channels_and_ands_features() {
        let a = CapabilitySet::new()
            .with(Capability::with_features(ChannelType::CONTROL, 0b111))
            .with(Capability::new(ChannelType::TRANSFER));
        let b = CapabilitySet::new()
            .with(Capability::with_features(ChannelType::CONTROL, 0b101))
            .with(Capability::new(ChannelType::new(0x0101))); // chat, not in a

        let common = a.intersect(&b);
        assert_eq!(common.len(), 1);
        assert_eq!(common.features(ChannelType::CONTROL), Some(0b101));
        assert!(!common.supports(ChannelType::TRANSFER));
    }

    /// The five feature bits assigned so far live in four *separate*
    /// namespaces (`Capability.features` is per-channel), so CHAT, CLIPBOARD,
    /// PRESENCE and PIPE all having a bit 0 is correct and must not be
    /// "fixed". What would be a real bug is reading one channel's bits off
    /// another's capability, so this pins that `features()` is keyed by
    /// channel.
    #[test]
    fn feature_bits_are_scoped_to_their_own_channel() {
        let set = CapabilitySet::new()
            .with(Capability::with_features(
                ChannelType::CHAT,
                CHAT_FEAT_FILEREF,
            ))
            .with(Capability::new(ChannelType::CLIPBOARD)) // CLIPBOARD, no features
            .with(Capability::new(ChannelType::PRESENCE)) // PRESENCE, no features
            .with(Capability::new(ChannelType::PIPE)); // PIPE, no features
        assert_eq!(set.features(ChannelType::CHAT), Some(CHAT_FEAT_FILEREF));
        assert_eq!(
            set.features(ChannelType::PRESENCE),
            Some(0),
            "PRESENCE must not inherit CHAT's bits despite the same bit index"
        );
        assert_eq!(
            set.features(ChannelType::CLIPBOARD),
            Some(0),
            "CLIPBOARD must not inherit CHAT's bits despite the same bit index"
        );
        assert_eq!(
            set.features(ChannelType::PIPE),
            Some(0),
            "PIPE must not inherit CHAT's bits despite the same bit index"
        );
        // All four bit-0s are the same value, which is the point: they are
        // only ever meaningful alongside the channel they were read from.
        assert_eq!(CHAT_FEAT_FILEREF, CLIPBOARD_FEAT_CLIP);
        assert_eq!(CLIPBOARD_FEAT_CLIP, PRESENCE_FEAT_STATUS);
        assert_eq!(PRESENCE_FEAT_STATUS, PIPE_FEAT_STREAM);
    }

    #[test]
    fn unknown_capability_is_simply_absent() {
        let set = CapabilitySet::new().with(Capability::new(ChannelType::CONTROL));
        assert!(!set.supports(ChannelType::new(0xABCD)));
        assert_eq!(set.features(ChannelType::new(0xABCD)), None);
    }
}
