//! Pure parsing of `tailscale status --json` and snapshot diffing.
//!
//! No IO here — the raw JSON string comes from a [`crate::source::StatusSource`].
//! Keeping this pure makes the peer mapping and the Found/Updated/Lost
//! diffing fully unit-testable without Tailscale installed.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde::Deserialize;

use peerbeam_domain::entity::{Device, DeviceType, Platform};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::DiscoveryEvent;

/// Top-level shape of `tailscale status --json` (only fields we use).
///
/// `Self` is intentionally omitted: the `Peer` map contains only *other*
/// nodes, so self-filtering is inherent.
#[derive(Debug, Deserialize)]
struct StatusJson {
    #[serde(rename = "Peer", default)]
    peer: HashMap<String, PeerJson>,
}

/// A single peer node in the tailnet.
#[derive(Debug, Deserialize)]
struct PeerJson {
    #[serde(rename = "ID", default)]
    id: String,
    #[serde(rename = "HostName", default)]
    host_name: String,
    /// MagicDNS name, e.g. `laptop.tailnet-1234.ts.net.` (trailing dot).
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "OS", default)]
    os: String,
    /// Tailnet IPs (100.64.0.0/10 v4 and fd7a:… v6).
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
    #[serde(rename = "Online", default)]
    online: bool,
}

/// Parse a status JSON document into discoverable [`Device`]s.
///
/// - Offline peers are skipped unless `include_offline`.
/// - Addresses are the tailnet IPs plus the MagicDNS name (trailing dot
///   stripped) so a peer is reachable by IP or name.
/// - `peer_port` stamps the assumed transfer port (Tailscale status does not
///   know the app's port; the transfer handshake resolves the real one).
pub(crate) fn parse_status(
    json: &str,
    include_offline: bool,
    peer_port: u16,
) -> Result<Vec<Device>> {
    let status: StatusJson = serde_json::from_str(json)
        .map_err(|e| DomainError::Discovery(format!("tailscale status parse: {e}")))?;

    let now = Utc::now();
    let mut devices = Vec::new();

    for peer in status.peer.values() {
        if !include_offline && !peer.online {
            continue;
        }

        let magic = peer.dns_name.trim_end_matches('.').to_string();

        let mut addresses = peer.tailscale_ips.clone();
        if !magic.is_empty() && !addresses.contains(&magic) {
            addresses.push(magic.clone());
        }
        if addresses.is_empty() {
            continue; // nothing reachable
        }

        let name = if !peer.host_name.is_empty() {
            peer.host_name.clone()
        } else {
            magic.clone()
        };

        // Prefer Tailscale's stable node ID; fall back to the MagicDNS name.
        let raw_id = if !peer.id.is_empty() {
            &peer.id
        } else {
            &magic
        };
        if raw_id.is_empty() {
            continue;
        }

        devices.push(Device {
            id: DeviceId::from(format!("ts:{raw_id}")),
            name,
            device_type: device_type_for_os(&peer.os),
            platform: platform_for_os(&peer.os),
            addresses,
            port: peer_port,
            last_seen: now,
        });
    }

    Ok(devices)
}

fn platform_for_os(os: &str) -> Platform {
    match os.to_ascii_lowercase().as_str() {
        "windows" => Platform::Windows,
        "macos" => Platform::MacOS,
        "ios" => Platform::IOS,
        "android" => Platform::Android,
        _ => Platform::Linux,
    }
}

fn device_type_for_os(os: &str) -> DeviceType {
    match os.to_ascii_lowercase().as_str() {
        "ios" | "android" => DeviceType::Phone,
        _ => DeviceType::Desktop,
    }
}

/// Diffs successive full snapshots into incremental discovery events.
///
/// Tailscale has no push API, so the provider polls `status` and this differ
/// turns each snapshot into Found (new), Updated (identity changed), and Lost
/// (absent from the new snapshot) events.
#[derive(Default)]
pub(crate) struct SnapshotDiffer {
    known: HashMap<DeviceId, Device>,
}

impl SnapshotDiffer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Fold a full snapshot into events describing the change since the last.
    pub(crate) fn diff(&mut self, current: Vec<Device>) -> Vec<DiscoveryEvent> {
        let mut events = Vec::new();
        let mut seen: HashSet<DeviceId> = HashSet::new();

        for device in current {
            seen.insert(device.id.clone());
            match self.known.get(&device.id) {
                None => {
                    self.known.insert(device.id.clone(), device.clone());
                    events.push(DiscoveryEvent::Found(device));
                }
                Some(prev) if !prev.same_identity(&device) => {
                    self.known.insert(device.id.clone(), device.clone());
                    events.push(DiscoveryEvent::Updated(device));
                }
                Some(_) => {} // unchanged → silent
            }
        }

        let lost: Vec<DeviceId> = self
            .known
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect();
        for id in lost {
            self.known.remove(&id);
            events.push(DiscoveryEvent::Lost(id));
        }

        events
    }

    /// Remove and return every known device id (used on stop).
    pub(crate) fn drain_ids(&mut self) -> Vec<DeviceId> {
        let ids: Vec<DeviceId> = self.known.keys().cloned().collect();
        self.known.clear();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "Peer": {
            "nodekey:aaa": {
                "ID": "n1",
                "HostName": "alice-laptop",
                "DNSName": "alice-laptop.tail1234.ts.net.",
                "OS": "linux",
                "TailscaleIPs": ["100.101.102.103", "fd7a:115c:a1e0::1"],
                "Online": true
            },
            "nodekey:bbb": {
                "ID": "n2",
                "HostName": "bob-phone",
                "DNSName": "bob-phone.tail1234.ts.net.",
                "OS": "iOS",
                "TailscaleIPs": ["100.64.0.7"],
                "Online": false
            }
        }
    }"#;

    #[test]
    fn parses_online_peer_with_magicdns_and_ips() {
        let devices = parse_status(SAMPLE, false, 4200).unwrap();
        assert_eq!(devices.len(), 1, "offline peer excluded");
        let d = &devices[0];
        assert_eq!(d.id, DeviceId::from("ts:n1"));
        assert_eq!(d.name, "alice-laptop");
        assert_eq!(d.platform, Platform::Linux);
        assert_eq!(d.port, 4200);
        assert!(d.addresses.contains(&"100.101.102.103".to_string()));
        assert!(d.addresses.contains(&"fd7a:115c:a1e0::1".to_string()));
        // MagicDNS name included, trailing dot stripped.
        assert!(d
            .addresses
            .contains(&"alice-laptop.tail1234.ts.net".to_string()));
    }

    #[test]
    fn include_offline_returns_all() {
        let devices = parse_status(SAMPLE, true, 0).unwrap();
        assert_eq!(devices.len(), 2);
        let phone = devices
            .iter()
            .find(|d| d.id == DeviceId::from("ts:n2"))
            .unwrap();
        assert_eq!(phone.platform, Platform::IOS);
        assert_eq!(phone.device_type, DeviceType::Phone);
    }

    #[test]
    fn empty_status_yields_no_devices() {
        assert!(parse_status(r#"{"Peer":{}}"#, false, 0).unwrap().is_empty());
        assert!(parse_status(r#"{}"#, false, 0).unwrap().is_empty());
    }

    #[test]
    fn invalid_json_errors() {
        assert!(parse_status("not json", false, 0).is_err());
    }

    #[test]
    fn os_mapping() {
        assert_eq!(platform_for_os("windows"), Platform::Windows);
        assert_eq!(platform_for_os("macOS"), Platform::MacOS);
        assert_eq!(platform_for_os("android"), Platform::Android);
        assert_eq!(platform_for_os("freebsd"), Platform::Linux);
        assert_eq!(device_type_for_os("android"), DeviceType::Phone);
        assert_eq!(device_type_for_os("linux"), DeviceType::Desktop);
    }

    fn device(id: &str, name: &str) -> Device {
        Device {
            id: DeviceId::from(id),
            name: name.to_string(),
            device_type: DeviceType::Desktop,
            platform: Platform::Linux,
            addresses: vec!["100.64.0.1".to_string()],
            port: 0,
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn differ_emits_found_then_updated_then_lost() {
        let mut differ = SnapshotDiffer::new();

        let e1 = differ.diff(vec![device("ts:a", "A"), device("ts:b", "B")]);
        assert_eq!(e1.len(), 2);
        assert!(e1.iter().all(|e| matches!(e, DiscoveryEvent::Found(_))));

        // Same snapshot → nothing.
        let e2 = differ.diff(vec![device("ts:a", "A"), device("ts:b", "B")]);
        assert!(e2.is_empty());

        // A renamed → Updated; B gone → Lost.
        let e3 = differ.diff(vec![device("ts:a", "A-renamed")]);
        assert_eq!(e3.len(), 2);
        assert!(e3.iter().any(|e| matches!(e, DiscoveryEvent::Updated(_))));
        assert!(e3
            .iter()
            .any(|e| matches!(e, DiscoveryEvent::Lost(id) if *id == DeviceId::from("ts:b"))));
    }

    #[test]
    fn drain_ids_clears() {
        let mut differ = SnapshotDiffer::new();
        differ.diff(vec![device("ts:a", "A")]);
        assert_eq!(differ.drain_ids(), vec![DeviceId::from("ts:a")]);
        assert!(differ.drain_ids().is_empty());
    }
}
