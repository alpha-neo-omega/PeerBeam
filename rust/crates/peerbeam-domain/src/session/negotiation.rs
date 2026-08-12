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

    #[test]
    fn unknown_capability_is_simply_absent() {
        let set = CapabilitySet::new().with(Capability::new(ChannelType::CONTROL));
        assert!(!set.supports(ChannelType::new(0xABCD)));
        assert_eq!(set.features(ChannelType::new(0xABCD)), None);
    }
}
