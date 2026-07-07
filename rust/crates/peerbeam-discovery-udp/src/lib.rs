//! LAN device discovery over UDP broadcast.
//!
//! An adapter implementing [`peerbeam_domain::port::DiscoveryProvider`]. It
//! finds peers on the same broadcast domain (Wi-Fi, Ethernet, USB tether)
//! with zero configuration.
//!
//! # How it works
//!
//! A single UDP socket, bound with address/port reuse so multiple instances
//! (and this device's own echo) coexist, both sends and receives on a
//! well-known port (default `49500`):
//!
//! - **Advertise** — periodically broadcast a small JSON [`Announce`] with
//!   our identity and transfer port. See [`proto`].
//! - **Scan** — on start, broadcast a [`Query`] so existing peers announce
//!   *immediately* (fast discovery, no waiting for the next interval), then
//!   listen for their announcements.
//! - **Automatic refresh** — every interval we re-announce; a reaper expires
//!   peers not heard from within `peer_ttl` and emits `Lost`. Liveness is
//!   therefore self-healing without any manual refresh.
//!
//! # Cross-platform
//!
//! `SO_REUSEADDR` is set everywhere; `SO_REUSEPORT` additionally on Unix.
//! Broadcast is enabled on the socket. No platform-specific code paths in
//! the discovery logic itself.
//!
//! # Security note
//!
//! A peer's address is taken from the UDP source, never the self-reported
//! field, so the advertisement cannot redirect a connection elsewhere.
//! Discovery only *finds* devices; authentication and trust happen later in
//! the transfer handshake.

mod peers;
mod proto;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use futures::StreamExt;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::BroadcastStream;

use peerbeam_domain::entity::Device;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::{DeviceId, ProviderId};
use peerbeam_domain::port::{DiscoveryCaps, DiscoveryEvent, DiscoveryProvider};

use crate::peers::PeerTable;
use crate::proto::{wire_to_device, Wire, WireKind};

/// The default well-known PeerBeam discovery port.
pub const DEFAULT_DISCOVERY_PORT: u16 = 49500;

/// Capacity of the internal event broadcast channel.
const EVENT_CHANNEL_CAPACITY: usize = 128;

/// Maximum expected datagram size.
const RECV_BUFFER: usize = 2048;

/// Tunable behaviour of [`UdpDiscovery`].
#[derive(Debug, Clone)]
pub struct Config {
    /// Port to bind and broadcast on. `0` binds an OS-assigned port (used
    /// in tests); production uses [`DEFAULT_DISCOVERY_PORT`].
    pub port: u16,
    /// Destination address for broadcasts.
    pub broadcast_addr: Ipv4Addr,
    /// How often to re-announce and run the liveness reaper.
    pub interval: Duration,
    /// How long a peer may go unheard before it is considered lost.
    pub peer_ttl: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: DEFAULT_DISCOVERY_PORT,
            broadcast_addr: Ipv4Addr::BROADCAST,
            interval: Duration::from_secs(2),
            peer_ttl: Duration::from_secs(6),
        }
    }
}

struct Inner {
    config: Config,
    device_id: DeviceId,
    self_device: RwLock<Option<Device>>,
    peers: Mutex<PeerTable>,
    events_tx: broadcast::Sender<DiscoveryEvent>,
    socket: Mutex<Option<Arc<UdpSocket>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    advertising: AtomicBool,
    scanning: AtomicBool,
}

/// LAN discovery provider using UDP broadcast.
pub struct UdpDiscovery {
    inner: Arc<Inner>,
}

impl UdpDiscovery {
    /// Create a provider for the device identified by `device_id`, using
    /// default [`Config`]. The `device_id` is used to filter out this
    /// device's own broadcast echo.
    pub fn new(device_id: DeviceId) -> Self {
        Self::with_config(device_id, Config::default())
    }

    /// Create a provider with a custom [`Config`].
    pub fn with_config(device_id: DeviceId, config: Config) -> Self {
        let (events_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                config,
                device_id,
                self_device: RwLock::new(None),
                peers: Mutex::new(PeerTable::new()),
                events_tx,
                socket: Mutex::new(None),
                tasks: Mutex::new(Vec::new()),
                advertising: AtomicBool::new(false),
                scanning: AtomicBool::new(false),
            }),
        }
    }

    /// The actual bound port, once a socket exists. Useful in tests when
    /// [`Config::port`] is `0`.
    pub fn bound_port(&self) -> Option<u16> {
        self.inner
            .socket
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.local_addr().ok())
            .map(|a| a.port())
    }

    /// Bind the shared socket and start the receive loop once. Idempotent:
    /// subsequent calls return the existing socket.
    fn ensure_socket(&self) -> Result<Arc<UdpSocket>> {
        let mut guard = self.inner.socket.lock().unwrap();
        if let Some(sock) = guard.as_ref() {
            return Ok(sock.clone());
        }
        let sock = Arc::new(build_socket(self.inner.config.port)?);
        let handle = tokio::spawn(recv_loop(self.inner.clone(), sock.clone()));
        self.inner.tasks.lock().unwrap().push(handle);
        *guard = Some(sock.clone());
        Ok(sock)
    }
}

#[async_trait]
impl DiscoveryProvider for UdpDiscovery {
    fn id(&self) -> ProviderId {
        ProviderId::from(format!("udp:{}", self.inner.config.port))
    }

    fn capabilities(&self) -> DiscoveryCaps {
        DiscoveryCaps {
            can_advertise: true,
            can_scan: true,
            // Broadcast does not cross subnets/NAT; other providers handle that.
            crosses_subnet: false,
            requires_tailscale: false,
        }
    }

    async fn advertise(&self, me: &Device) -> Result<()> {
        if self.inner.advertising.swap(true, Ordering::SeqCst) {
            return Ok(()); // already advertising
        }
        *self.inner.self_device.write().unwrap() = Some(me.clone());
        let sock = self.ensure_socket()?;
        let handle = tokio::spawn(advertise_loop(self.inner.clone(), sock));
        self.inner.tasks.lock().unwrap().push(handle);
        tracing::info!(provider = %self.id(), "advertising started");
        Ok(())
    }

    async fn scan(&self) -> Result<()> {
        if self.inner.scanning.swap(true, Ordering::SeqCst) {
            return Ok(()); // already scanning
        }
        let sock = self.ensure_socket()?;

        // Fast discovery: prompt existing peers to announce right now.
        let query = Wire::query(&self.inner.device_id).encode();
        let target = SocketAddr::from((self.inner.config.broadcast_addr, self.inner.config.port));
        let _ = sock.send_to(&query, target).await;

        let handle = tokio::spawn(reaper_loop(self.inner.clone()));
        self.inner.tasks.lock().unwrap().push(handle);
        tracing::info!(provider = %self.id(), "scanning started");
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.inner.advertising.store(false, Ordering::SeqCst);
        self.inner.scanning.store(false, Ordering::SeqCst);

        for handle in self.inner.tasks.lock().unwrap().drain(..) {
            handle.abort();
        }
        *self.inner.socket.lock().unwrap() = None;

        // Emit Lost for everyone we knew, then clear.
        let lost = self.inner.peers.lock().unwrap().drain_ids();
        for id in lost {
            let _ = self.inner.events_tx.send(DiscoveryEvent::Lost(id));
        }
        tracing::info!(provider = %self.id(), "discovery stopped");
        Ok(())
    }

    fn events(&self) -> BoxStream<'static, DiscoveryEvent> {
        // Drop lag errors: a slow consumer simply misses intermediate
        // events; the periodic re-announce and reaper keep state correct.
        BroadcastStream::new(self.inner.events_tx.subscribe())
            .filter_map(|res| async move { res.ok() })
            .boxed()
    }
}

/// Build a broadcast-capable, reuse-enabled UDP socket bound to `port`.
fn build_socket(port: u16) -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let map = |e: std::io::Error| DomainError::Discovery(format!("udp socket: {e}"));

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(map)?;
    socket.set_reuse_address(true).map_err(map)?;
    #[cfg(unix)]
    socket.set_reuse_port(true).map_err(map)?;
    socket.set_broadcast(true).map_err(map)?;
    socket.set_nonblocking(true).map_err(map)?;

    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
    socket.bind(&addr.into()).map_err(map)?;

    let std_sock: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_sock).map_err(map)
}

/// Receive loop: parse datagrams, update the peer table, emit events, and
/// answer queries. Runs for the life of the socket.
async fn recv_loop(inner: Arc<Inner>, sock: Arc<UdpSocket>) {
    let mut buf = [0u8; RECV_BUFFER];
    loop {
        let (len, src) = match sock.recv_from(&mut buf).await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::debug!("udp recv error: {e}");
                continue;
            }
        };

        let Some(wire) = Wire::decode(&buf[..len]) else {
            continue;
        };

        // Self-filter: never react to our own broadcast echo.
        if wire.id == inner.device_id.as_str() {
            continue;
        }

        match wire.kind {
            WireKind::Announce => {
                let device = wire_to_device(&wire, src.ip().to_string(), Utc::now());
                let event = inner.peers.lock().unwrap().observe(device, Instant::now());
                if let Some(event) = event {
                    let _ = inner.events_tx.send(event);
                }
            }
            WireKind::Query => {
                // Answer only if we are advertising, replying directly to the
                // asker for a fast, targeted response.
                if inner.advertising.load(Ordering::SeqCst) {
                    let me = inner.self_device.read().unwrap().clone();
                    if let Some(me) = me {
                        let reply = Wire::announce(&me).encode();
                        let _ = sock.send_to(&reply, src).await;
                    }
                }
            }
        }
    }
}

/// Periodically broadcast our announcement.
async fn advertise_loop(inner: Arc<Inner>, sock: Arc<UdpSocket>) {
    let mut ticker = tokio::time::interval(inner.config.interval);
    let target = SocketAddr::from((inner.config.broadcast_addr, inner.config.port));
    loop {
        ticker.tick().await; // first tick fires immediately
        if !inner.advertising.load(Ordering::SeqCst) {
            break;
        }
        let me = inner.self_device.read().unwrap().clone();
        if let Some(me) = me {
            let bytes = Wire::announce(&me).encode();
            let _ = sock.send_to(&bytes, target).await;
        }
    }
}

/// Periodically expire stale peers and emit `Lost`.
async fn reaper_loop(inner: Arc<Inner>) {
    let mut ticker = tokio::time::interval(inner.config.interval);
    loop {
        ticker.tick().await;
        if !inner.scanning.load(Ordering::SeqCst) {
            break;
        }
        let lost = inner
            .peers
            .lock()
            .unwrap()
            .expire(Instant::now(), inner.config.peer_ttl);
        for id in lost {
            let _ = inner.events_tx.send(DiscoveryEvent::Lost(id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.port, DEFAULT_DISCOVERY_PORT);
        assert_eq!(c.broadcast_addr, Ipv4Addr::BROADCAST);
        assert!(c.peer_ttl > c.interval, "ttl must outlast one interval");
    }

    #[test]
    fn capabilities_declare_lan_only() {
        let d = UdpDiscovery::new(DeviceId::from("x"));
        let caps = d.capabilities();
        assert!(caps.can_advertise && caps.can_scan);
        assert!(!caps.crosses_subnet);
        assert!(!caps.requires_tailscale);
    }

    #[test]
    fn id_reflects_port() {
        let d = UdpDiscovery::with_config(
            DeviceId::from("x"),
            Config {
                port: 12345,
                ..Config::default()
            },
        );
        assert_eq!(d.id(), ProviderId::from("udp:12345"));
    }

    #[test]
    fn bound_port_is_none_before_start() {
        let d = UdpDiscovery::new(DeviceId::from("x"));
        assert!(d.bound_port().is_none());
    }
}
