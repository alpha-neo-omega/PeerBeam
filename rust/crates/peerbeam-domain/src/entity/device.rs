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
