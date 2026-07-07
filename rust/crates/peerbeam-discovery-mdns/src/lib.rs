//! mDNS / DNS-SD device discovery.
//!
//! An adapter implementing [`peerbeam_domain::port::DiscoveryProvider`] on
//! top of [`mdns-sd`]. Advertises this device as a `_peerbeam._tcp.local.`
//! service and browses for peers advertising the same service.
//!
//! Complements [`peerbeam-discovery-udp`](https://docs.rs): both run at once
//! and the engine merges their results (see `peerbeam_app::merge_discovery`),
//! so a device seen by mDNS *and* UDP appears once. mDNS is often more
//! reliable on managed Wi-Fi where UDP broadcast is filtered.
//!
//! # Design
//!
//! - **Advertise** — register a service with TXT records carrying identity
//!   (`id`, `name`, `device_type`, `platform`, `version`); addresses are
//!   auto-detected across interfaces via `enable_addr_auto`.
//! - **Scan** — browse the service type; on `ServiceResolved` emit `Found`,
//!   on `ServiceRemoved` emit `Lost`. A fullname→id map lets removals map
//!   back to the right device.
//! - **Self-filter** — ignore any resolved service whose TXT `id` is ours.
//!
//! Addresses come from mDNS's resolved records; identity comes from TXT.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use futures::StreamExt;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::BroadcastStream;

use peerbeam_domain::entity::{Device, DeviceType, Platform};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::{DeviceId, ProviderId};
use peerbeam_domain::port::{DiscoveryCaps, DiscoveryEvent, DiscoveryProvider};

/// The DNS-SD service type PeerBeam advertises and browses.
pub const SERVICE_TYPE: &str = "_peerbeam._tcp.local.";

/// Capacity of the internal event broadcast channel.
const EVENT_CHANNEL_CAPACITY: usize = 128;

struct Inner {
    daemon: ServiceDaemon,
    device_id: DeviceId,
    events_tx: broadcast::Sender<DiscoveryEvent>,
    /// Maps a resolved service fullname to its device id, so `ServiceRemoved`
    /// (which only carries the fullname) can emit the right `Lost`.
    fullnames: Mutex<HashMap<String, DeviceId>>,
    /// Our own registered service fullname, for unregister on stop.
    registered: Mutex<Option<String>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    advertising: AtomicBool,
    scanning: AtomicBool,
}

/// mDNS discovery provider.
pub struct MdnsDiscovery {
    inner: Arc<Inner>,
}

impl MdnsDiscovery {
    /// Create a provider for the device identified by `device_id`.
    ///
    /// Fails with [`DomainError::Discovery`] if the mDNS daemon cannot start
    /// (e.g. no multicast support); callers may then run with UDP discovery
    /// only.
    pub fn new(device_id: DeviceId) -> Result<Self> {
        let daemon = ServiceDaemon::new()
            .map_err(|e| DomainError::Discovery(format!("mdns daemon: {e}")))?;
        let (events_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Ok(Self {
            inner: Arc::new(Inner {
                daemon,
                device_id,
                events_tx,
                fullnames: Mutex::new(HashMap::new()),
                registered: Mutex::new(None),
                tasks: Mutex::new(Vec::new()),
                advertising: AtomicBool::new(false),
                scanning: AtomicBool::new(false),
            }),
        })
    }

    /// Short, DNS-safe instance label derived from the device id.
    fn instance_name(&self) -> String {
        let id = self.inner.device_id.as_str();
        let short = if id.len() > 8 { &id[..8] } else { id };
        format!("PeerBeam-{short}")
    }
}

#[async_trait]
impl DiscoveryProvider for MdnsDiscovery {
    fn id(&self) -> ProviderId {
        ProviderId::from("mdns")
    }

    fn capabilities(&self) -> DiscoveryCaps {
        DiscoveryCaps {
            can_advertise: true,
            can_scan: true,
            // mDNS is link-local; cross-subnet reach is other providers' job.
            crosses_subnet: false,
            requires_tailscale: false,
        }
    }

    async fn advertise(&self, me: &Device) -> Result<()> {
        if self.inner.advertising.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let instance = self.instance_name();
        let host = format!("{instance}.local.");
        let mut props: HashMap<String, String> = HashMap::new();
        props.insert("id".into(), me.id.to_string());
        props.insert("name".into(), me.name.clone());
        props.insert(
            "device_type".into(),
            encode_device_type(me.device_type).into(),
        );
        props.insert("platform".into(), me.platform.as_str().into());
        props.insert("version".into(), env!("CARGO_PKG_VERSION").into());

        let service = ServiceInfo::new(SERVICE_TYPE, &instance, &host, "", me.port, props)
            .map_err(|e| DomainError::Discovery(format!("mdns service info: {e}")))?
            .enable_addr_auto();

        let fullname = service.get_fullname().to_string();
        self.inner
            .daemon
            .register(service)
            .map_err(|e| DomainError::Discovery(format!("mdns register: {e}")))?;
        *self.inner.registered.lock().unwrap() = Some(fullname);

        tracing::info!(provider = "mdns", instance = %instance, "advertising started");
        Ok(())
    }

    async fn scan(&self) -> Result<()> {
        if self.inner.scanning.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let receiver = self
            .inner
            .daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| DomainError::Discovery(format!("mdns browse: {e}")))?;

        let inner = self.inner.clone();
        let handle = tokio::spawn(async move {
            while let Ok(event) = receiver.recv_async().await {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        if let Some(device) = parse_service(&info) {
                            if device.id == inner.device_id {
                                continue; // self-filter
                            }
                            inner
                                .fullnames
                                .lock()
                                .unwrap()
                                .insert(info.get_fullname().to_string(), device.id.clone());
                            let _ = inner.events_tx.send(DiscoveryEvent::Found(device));
                        }
                    }
                    ServiceEvent::ServiceRemoved(_ty, fullname) => {
                        let id = inner.fullnames.lock().unwrap().remove(&fullname);
                        if let Some(id) = id {
                            let _ = inner.events_tx.send(DiscoveryEvent::Lost(id));
                        }
                    }
                    _ => {}
                }
            }
        });
        self.inner.tasks.lock().unwrap().push(handle);

        tracing::info!(provider = "mdns", "scanning started");
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.inner.advertising.store(false, Ordering::SeqCst);
        self.inner.scanning.store(false, Ordering::SeqCst);

        if let Some(fullname) = self.inner.registered.lock().unwrap().take() {
            let _ = self.inner.daemon.unregister(&fullname);
        }
        let _ = self.inner.daemon.stop_browse(SERVICE_TYPE);

        for handle in self.inner.tasks.lock().unwrap().drain(..) {
            handle.abort();
        }

        let lost: Vec<DeviceId> = self
            .inner
            .fullnames
            .lock()
            .unwrap()
            .drain()
            .map(|(_, id)| id)
            .collect();
        for id in lost {
            let _ = self.inner.events_tx.send(DiscoveryEvent::Lost(id));
        }

        tracing::info!(provider = "mdns", "discovery stopped");
        Ok(())
    }

    fn events(&self) -> BoxStream<'static, DiscoveryEvent> {
        BroadcastStream::new(self.inner.events_tx.subscribe())
            .filter_map(|res| async move { res.ok() })
            .boxed()
    }
}

impl Drop for MdnsDiscovery {
    fn drop(&mut self) {
        let _ = self.inner.daemon.shutdown();
    }
}

/// Parse a resolved mDNS service into a [`Device`]. Returns `None` if it
/// lacks an `id` TXT record or resolved to no addresses.
fn parse_service(info: &ServiceInfo) -> Option<Device> {
    let id = info.get_property_val_str("id").unwrap_or("");
    if id.is_empty() {
        return None;
    }
    let addresses: Vec<String> = info
        .get_addresses()
        .iter()
        .map(|ip| ip.to_string())
        .collect();
    if addresses.is_empty() {
        return None;
    }
    Some(Device {
        id: DeviceId::from(id),
        name: info
            .get_property_val_str("name")
            .unwrap_or("Unknown Device")
            .to_string(),
        device_type: decode_device_type(info.get_property_val_str("device_type").unwrap_or("")),
        platform: decode_platform(info.get_property_val_str("platform").unwrap_or("")),
        addresses,
        port: info.get_port(),
        last_seen: Utc::now(),
    })
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

    fn service_with(props: &[(&str, &str)], addr: &str, port: u16) -> ServiceInfo {
        let map: HashMap<String, String> = props
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ServiceInfo::new(
            SERVICE_TYPE,
            "PeerBeam-test",
            "PeerBeam-test.local.",
            addr,
            port,
            map,
        )
        .expect("valid service info")
    }

    #[test]
    fn parse_full_service() {
        let info = service_with(
            &[
                ("id", "dev-1"),
                ("name", "Alice"),
                ("device_type", "Phone"),
                ("platform", "android"),
            ],
            "192.168.1.5",
            4200,
        );
        let device = parse_service(&info).expect("should parse");
        assert_eq!(device.id, DeviceId::from("dev-1"));
        assert_eq!(device.name, "Alice");
        assert_eq!(device.device_type, DeviceType::Phone);
        assert_eq!(device.platform, Platform::Android);
        assert_eq!(device.port, 4200);
        assert!(device.addresses.contains(&"192.168.1.5".to_string()));
    }

    #[test]
    fn parse_without_id_is_none() {
        let info = service_with(&[("name", "NoId")], "192.168.1.5", 4200);
        assert!(parse_service(&info).is_none());
    }

    #[test]
    fn parse_missing_name_defaults() {
        let info = service_with(&[("id", "dev-2")], "10.0.0.7", 9000);
        let device = parse_service(&info).expect("should parse");
        assert_eq!(device.name, "Unknown Device");
        assert_eq!(device.device_type, DeviceType::Desktop);
        assert_eq!(device.platform, Platform::Linux);
    }

    #[test]
    fn device_type_and_platform_roundtrip() {
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
    fn capabilities_and_id() {
        // Skip if the mDNS daemon cannot start in this environment.
        let Ok(provider) = MdnsDiscovery::new(DeviceId::from("abcd1234efgh")) else {
            return;
        };
        assert_eq!(provider.id(), ProviderId::from("mdns"));
        assert_eq!(provider.instance_name(), "PeerBeam-abcd1234");
        let caps = provider.capabilities();
        assert!(caps.can_advertise && caps.can_scan);
    }
}
