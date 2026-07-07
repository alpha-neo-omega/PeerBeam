//! Discovery wire protocol.
//!
//! A single small JSON datagram, versioned so future changes stay
//! backward-compatible. Two kinds:
//!
//! - `Announce` — "I am here", carrying the sender's identity and transfer
//!   port. Broadcast periodically and in response to a `Query`.
//! - `Query` — "who is here?", prompting peers to `Announce` immediately so
//!   a newly-started device sees others without waiting a full interval.
//!
//! The sender's IP is taken from the datagram source address, never
//! self-reported, so a spoofed address field cannot redirect connections.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use peerbeam_domain::entity::{Device, DeviceType, Platform};
use peerbeam_domain::id::DeviceId;

/// Current wire protocol version. Datagrams with a different version are
/// ignored so old and new builds never misinterpret each other.
pub(crate) const PROTOCOL_VERSION: u8 = 1;

/// The kind of discovery datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum WireKind {
    /// Announce presence.
    Announce,
    /// Ask peers to announce.
    Query,
}

/// A discovery datagram on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Wire {
    /// Protocol version.
    pub v: u8,
    /// Datagram kind.
    pub kind: WireKind,
    /// Sender device id.
    pub id: String,
    /// Sender display name.
    pub name: String,
    /// Sender device type (e.g. "Phone").
    pub device_type: String,
    /// Sender platform (e.g. "android").
    pub platform: String,
    /// Sender transfer-server port.
    pub port: u16,
}

impl Wire {
    /// Build an `Announce` datagram for our device.
    pub(crate) fn announce(device: &Device) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            kind: WireKind::Announce,
            id: device.id.to_string(),
            name: device.name.clone(),
            device_type: encode_device_type(device.device_type).to_string(),
            platform: device.platform.as_str().to_string(),
            port: device.port,
        }
    }

    /// Build a `Query` datagram carrying only our id (so peers can filter
    /// our own announcement echo).
    pub(crate) fn query(id: &DeviceId) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            kind: WireKind::Query,
            id: id.to_string(),
            name: String::new(),
            device_type: "Desktop".to_string(),
            platform: "linux".to_string(),
            port: 0,
        }
    }

    /// Serialize to bytes for sending.
    pub(crate) fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Parse bytes into a datagram, rejecting malformed or wrong-version
    /// payloads (returns `None` rather than erroring — bad packets on a
    /// shared port are expected and simply ignored).
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let wire: Wire = serde_json::from_slice(bytes).ok()?;
        (wire.v == PROTOCOL_VERSION).then_some(wire)
    }
}

/// Convert a received datagram into a [`Device`], using the observed source
/// IP as the peer's address.
pub(crate) fn wire_to_device(wire: &Wire, src_ip: String, now: DateTime<Utc>) -> Device {
    Device {
        id: DeviceId::from(wire.id.clone()),
        name: wire.name.clone(),
        device_type: decode_device_type(&wire.device_type),
        platform: decode_platform(&wire.platform),
        addresses: vec![src_ip],
        port: wire.port,
        last_seen: now,
    }
}

fn encode_device_type(dt: DeviceType) -> &'static str {
    match dt {
        DeviceType::Desktop => "Desktop",
        DeviceType::Laptop => "Laptop",
        DeviceType::Phone => "Phone",
        DeviceType::Tablet => "Tablet",
        DeviceType::Server => "Server",
        DeviceType::WebBrowser => "WebBrowser",
    }
}

fn decode_device_type(s: &str) -> DeviceType {
    match s {
        "Laptop" => DeviceType::Laptop,
        "Phone" => DeviceType::Phone,
        "Tablet" => DeviceType::Tablet,
        "Server" => DeviceType::Server,
        "WebBrowser" => DeviceType::WebBrowser,
        _ => DeviceType::Desktop,
    }
}

fn decode_platform(s: &str) -> Platform {
    match s {
        "windows" => Platform::Windows,
        "macos" => Platform::MacOS,
        "android" => Platform::Android,
        "ios" => Platform::IOS,
        "web" => Platform::Web,
        _ => Platform::Linux,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_device() -> Device {
        Device {
            id: DeviceId::from("dev-1"),
            name: "Alice".to_string(),
            device_type: DeviceType::Phone,
            platform: Platform::Android,
            addresses: vec!["192.168.1.5".to_string()],
            port: 4200,
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn announce_roundtrips() {
        let wire = Wire::announce(&sample_device());
        let bytes = wire.encode();
        let decoded = Wire::decode(&bytes).expect("valid");
        assert_eq!(decoded.kind, WireKind::Announce);
        assert_eq!(decoded.id, "dev-1");
        assert_eq!(decoded.name, "Alice");
        assert_eq!(decoded.device_type, "Phone");
        assert_eq!(decoded.platform, "android");
        assert_eq!(decoded.port, 4200);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(Wire::decode(b"not json at all").is_none());
        assert!(Wire::decode(b"").is_none());
    }

    #[test]
    fn decode_rejects_wrong_version() {
        let mut wire = Wire::announce(&sample_device());
        wire.v = 99;
        assert!(Wire::decode(&wire.encode()).is_none());
    }

    #[test]
    fn wire_to_device_uses_source_ip_not_reported() {
        let wire = Wire::announce(&sample_device());
        let now = Utc::now();
        let dev = wire_to_device(&wire, "10.0.0.9".to_string(), now);
        assert_eq!(dev.id, DeviceId::from("dev-1"));
        assert_eq!(dev.addresses, vec!["10.0.0.9".to_string()]);
        assert_eq!(dev.device_type, DeviceType::Phone);
        assert_eq!(dev.platform, Platform::Android);
        assert_eq!(dev.port, 4200);
        assert_eq!(dev.last_seen, now);
    }

    #[test]
    fn device_type_and_platform_roundtrip_all_variants() {
        for dt in [
            DeviceType::Desktop,
            DeviceType::Laptop,
            DeviceType::Phone,
            DeviceType::Tablet,
            DeviceType::Server,
            DeviceType::WebBrowser,
        ] {
            assert_eq!(decode_device_type(encode_device_type(dt)), dt);
        }
        for p in [
            Platform::Windows,
            Platform::MacOS,
            Platform::Linux,
            Platform::Android,
            Platform::IOS,
            Platform::Web,
        ] {
            assert_eq!(decode_platform(p.as_str()), p);
        }
    }

    #[test]
    fn unknown_type_and_platform_fall_back() {
        assert_eq!(decode_device_type("Toaster"), DeviceType::Desktop);
        assert_eq!(decode_platform("plan9"), Platform::Linux);
    }

    #[test]
    fn same_identity_detects_changes() {
        let a = sample_device();
        let mut b = a.clone();
        b.last_seen = Utc::now(); // timestamp change is ignored
        assert!(a.same_identity(&b));

        b.name = "Alice-renamed".to_string();
        assert!(!a.same_identity(&b));
    }
}
