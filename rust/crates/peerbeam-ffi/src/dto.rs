//! Stable wire DTOs for the FFI boundary.
//!
//! These are deliberately *separate* from the domain entities: the JSON shape
//! that crosses to Dart is a versioned contract and must not change just
//! because an internal struct does. Mappers translate domain → DTO here.

use serde::Serialize;
use serde_json::{json, Value};

use peerbeam_domain::entity::ManagedDevice;
use peerbeam_domain::event::DeviceChange;

/// A device as the UI sees it.
#[derive(Serialize)]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub platform: String,
    pub addresses: Vec<String>,
    pub port: u16,
    pub online: bool,
    pub latency_ms: Option<u32>,
    pub reachable_lan: bool,
    pub reachable_remote: bool,
}

impl From<&ManagedDevice> for DeviceDto {
    fn from(m: &ManagedDevice) -> Self {
        DeviceDto {
            id: m.device.id.0.clone(),
            name: m.device.name.clone(),
            kind: device_kind(&m.device.device_type),
            platform: m.device.platform.as_str().to_string(),
            addresses: m.device.addresses.clone(),
            port: m.device.port,
            online: m.online,
            latency_ms: m.latency_ms,
            reachable_lan: m.capabilities.reachable_lan,
            reachable_remote: m.capabilities.reachable_remote,
        }
    }
}

fn device_kind(t: &peerbeam_domain::entity::DeviceType) -> String {
    use peerbeam_domain::entity::DeviceType::*;
    match t {
        Desktop => "desktop",
        Laptop => "laptop",
        Phone => "phone",
        Tablet => "tablet",
        Server => "server",
        WebBrowser => "web",
    }
    .to_string()
}

/// Serialize a device-change into an event DTO (`{type, …}`).
pub fn device_event(change: &DeviceChange) -> Value {
    match change {
        DeviceChange::Added(d) => json!({ "type": "device_added", "device": DeviceDto::from(d) }),
        DeviceChange::Updated(d) => {
            json!({ "type": "device_updated", "device": DeviceDto::from(d) })
        }
        DeviceChange::StatusChanged { id, online } => {
            json!({ "type": "status_changed", "id": id.0, "online": online })
        }
        DeviceChange::LatencyChanged { id, latency_ms } => {
            json!({ "type": "latency_changed", "id": id.0, "latency_ms": latency_ms })
        }
        DeviceChange::Removed(id) => json!({ "type": "device_removed", "id": id.0 }),
    }
}
