//! Device / peer entity.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::DeviceId;

/// The category of a device, used for iconography and heuristics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum DeviceType {
    Desktop,
    Laptop,
    Phone,
    Tablet,
    Server,
    WebBrowser,
}

/// The operating-system platform a device runs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Platform {
    Windows,
    MacOS,
    Linux,
    Android,
    IOS,
    Web,
}

impl Platform {
    /// Lowercase wire/string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::Windows => "windows",
            Platform::MacOS => "macos",
            Platform::Linux => "linux",
            Platform::Android => "android",
            Platform::IOS => "ios",
            Platform::Web => "web",
        }
    }
}

/// A device on the network — either this device or a discovered peer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Device {
    /// Stable identifier for this device.
    pub id: DeviceId,
    /// Human-friendly name shown in the UI.
    pub name: String,
    /// Category of the device.
    pub device_type: DeviceType,
    /// Operating-system platform.
    pub platform: Platform,
    /// Reachable addresses (may span interfaces: LAN, Tailscale, …).
    pub addresses: Vec<String>,
    /// Port the device's transfer server listens on (0 if unknown).
    pub port: u16,
    /// When the device was last observed.
    pub last_seen: DateTime<Utc>,
}

impl Device {
    /// Whether this device shares the same observable identity as `other`,
    /// ignoring the volatile `last_seen` timestamp.
    ///
    /// Used to decide whether a re-observation is a meaningful change
    /// (worth a `Updated`/`PeerUpdated` event) or a silent liveness refresh.
    pub fn same_identity(&self, other: &Device) -> bool {
        self.id == other.id
            && self.name == other.name
            && self.platform == other.platform
            && self.device_type == other.device_type
            && self.addresses == other.addresses
            && self.port == other.port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Device {
        Device {
            id: DeviceId::from("dev-1"),
            name: "Laptop".into(),
            device_type: DeviceType::Laptop,
            platform: Platform::Linux,
            addresses: vec!["10.0.0.2".into()],
            port: 49500,
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn same_identity_ignores_last_seen() {
        let a = sample();
        let mut b = a.clone();
        // A liveness refresh only bumps the timestamp — still the same device.
        b.last_seen = a.last_seen + chrono::Duration::seconds(30);
        assert!(a.same_identity(&b));
    }

    #[test]
    fn same_identity_detects_meaningful_changes() {
        let a = sample();
        let mutations: [fn(&mut Device); 6] = [
            |d| d.name = "Renamed".into(),
            |d| d.addresses = vec!["10.0.0.3".into()],
            |d| d.port = 40000,
            |d| d.platform = Platform::Windows,
            |d| d.device_type = DeviceType::Desktop,
            |d| d.id = DeviceId::from("dev-2"),
        ];
        for mutate in mutations {
            let mut b = a.clone();
            mutate(&mut b);
            assert!(
                !a.same_identity(&b),
                "a change to an identity field must be observable"
            );
        }
    }
}
