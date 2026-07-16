//! QUIC [`TransferProvider`] for PeerBeam.
//!
//! Turns the abstract transfer [`Link`] into a real network connection using
//! [quinn](https://docs.rs/quinn). It plugs into the existing transfer engine
//! unchanged: `send_file`/`receive_file`/`send_folder` already operate on
//! `&mut dyn Link`, so a [`QuicLink`] is a drop-in transport.
//!
//! - **Encryption** is provided by QUIC's mandatory TLS (see [`tls`]).
//! - **Identity/trust** is *not* — it is layered on top by
//!   `peerbeam-transfer`'s `authenticate` + `SecureLink`. QUIC here is an
//!   encrypted-but-unauthenticated pipe by design (zero-config, no PKI).
//!
//! ```no_run
//! # async fn ex() -> peerbeam_domain::error::Result<()> {
//! use peerbeam_transfer_quic::QuicTransport;
//! use peerbeam_domain::port::{Bind, TransferProvider};
//!
//! let quic = QuicTransport::new()?;
//! let mut incoming = quic.serve(Bind { port: 0 }).await?; // receiver
//! // ... meanwhile a sender calls quic.dial(route, session).await? ...
//! # Ok(()) }
//! ```

mod link;
mod tls;

pub use link::QuicLink;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;

use peerbeam_domain::entity::{Route, TransferSession};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::ProviderId;
use peerbeam_domain::port::{Bind, Link, Protocol, TransferProvider};

/// Server name presented to QUIC (ignored by the accept-any verifier, but the
/// TLS layer requires one).
const SERVER_NAME: &str = "peerbeam";

fn conn_err(e: impl std::fmt::Display) -> DomainError {
    DomainError::Connection(format!("quic: {e}"))
}

/// How long an outbound handshake may take before the dial fails. Long enough
/// for a slow Tailscale DERP round-trip, short enough that a dead peer fails
/// while the user is still watching.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// Shared QUIC transport tuning: a keep-alive so idle connections (e.g. a
/// paused transfer) stay up, and a generous idle timeout before giving up.
fn transport_config() -> Arc<quinn::TransportConfig> {
    let mut tc = quinn::TransportConfig::default();
    tc.keep_alive_interval(Some(Duration::from_secs(5)));
    // Allow the progress back-channel's dedicated uni-stream (receiver→sender).
    tc.max_concurrent_uni_streams(4u8.into());
    tc.max_idle_timeout(Some(
        Duration::from_secs(30)
            .try_into()
            .expect("valid idle timeout"),
    ));
    Arc::new(tc)
}

/// A QUIC transport that can dial peers and accept inbound connections.
///
/// One client [`quinn::Endpoint`] is held for outbound `dial`s; each `serve`
/// call binds its own server endpoint (kept alive by the returned stream).
pub struct QuicTransport {
    id: ProviderId,
    client: quinn::Endpoint,
}

impl QuicTransport {
    /// Create a transport with an IPv4 client endpoint on an ephemeral port.
    pub fn new() -> Result<Self> {
        Self::bound("0.0.0.0:0".parse().expect("valid addr"))
    }

    /// Create a transport whose client endpoint is bound to `bind`. Use an
    /// IPv6 wildcard (`[::]:0`) to dial IPv6 peers, or a specific interface
    /// address to pin outbound traffic to one NIC.
    pub fn bound(bind: SocketAddr) -> Result<Self> {
        let mut client = quinn::Endpoint::client(bind).map_err(conn_err)?;
        let mut client_config = tls::client_config()?;
        client_config.transport_config(transport_config());
        client.set_default_client_config(client_config);
        tracing::debug!(local = %client.local_addr().map(|a| a.to_string()).unwrap_or_default(), "quic client endpoint ready");
        Ok(Self {
            id: ProviderId::from("quic"),
            client,
        })
    }

    /// Like [`TransferProvider::serve`], but also returns the actual bound
    /// local address. Needed when binding to port 0 (OS-assigned) — e.g. tests
    /// and the benchmark — where the caller must learn the chosen port to dial.
    /// Binds the IPv4 wildcard; use [`serve_addr_on`](Self::serve_addr_on) for
    /// IPv6 or a specific interface.
    pub async fn serve_addr(
        &self,
        bind: Bind,
    ) -> Result<(SocketAddr, BoxStream<'static, Result<Box<dyn Link>>>)> {
        let addr: SocketAddr = format!("0.0.0.0:{}", bind.port)
            .parse()
            .expect("valid addr");
        self.serve_addr_on(addr).await
    }

    /// Serve on an explicit bind address (IPv4 or IPv6, specific interface or
    /// wildcard), returning the bound local address and the inbound stream.
    pub async fn serve_addr_on(
        &self,
        addr: SocketAddr,
    ) -> Result<(SocketAddr, BoxStream<'static, Result<Box<dyn Link>>>)> {
        let mut server_config = tls::server_config()?;
        server_config.transport = transport_config();
        let endpoint = quinn::Endpoint::server(server_config, addr).map_err(conn_err)?;
        let local = endpoint.local_addr().map_err(conn_err)?;
        tracing::info!(%local, "quic serving");

        let stream = futures::stream::unfold(endpoint, |ep| async move {
            loop {
                match ep.accept().await {
                    Some(incoming) => match accept_link(incoming).await {
                        Ok(link) => return Some((Ok(link), ep)),
                        Err(e) => {
                            // A single bad connection must not stop the server.
                            tracing::warn!(error = %e, "quic inbound connection rejected");
                            continue;
                        }
                    },
                    None => return None, // endpoint closed
                }
            }
        });
        Ok((local, Box::pin(stream)))
    }
}

#[async_trait]
impl TransferProvider for QuicTransport {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn protocol(&self) -> Protocol {
        Protocol::Quic
    }

    async fn dial(&self, route: &Route, session: &TransferSession) -> Result<Box<dyn Link>> {
        let addr = resolve_addr(&route.address, route.port).await?;
        tracing::info!(peer = %session.peer.0, %addr, kind = ?route.kind, "quic dial");

        // Bound the handshake: an unreachable peer must fail fast (the user is
        // watching), not after the 30s idle timeout.
        let connecting = self.client.connect(addr, SERVER_NAME).map_err(conn_err)?;
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
            .await
            .map_err(|_| conn_err("connect timed out — peer unreachable"))?
            .map_err(conn_err)?;
        // Client opens the bidirectional stream; it materialises on the server
        // once the first frame (transfer Meta) is written by the engine.
        let (send, recv) = conn.open_bi().await.map_err(conn_err)?;
        tracing::debug!(%addr, "quic link established (outbound)");
        Ok(Box::new(QuicLink::new(conn, send, recv)))
    }

    async fn serve(&self, bind: Bind) -> Result<BoxStream<'static, Result<Box<dyn Link>>>> {
        let (_addr, stream) = self.serve_addr(bind).await?;
        Ok(stream)
    }
}

/// Resolve a route target (IPv4/IPv6 literal or hostname) + port to a socket
/// address. Handles IPv6 bracketing correctly (unlike naive `host:port`).
///
/// IP literals short-circuit synchronously. Hostnames (e.g. a Tailscale
/// MagicDNS name) are resolved via `tokio::net::lookup_host`, which runs the
/// blocking `getaddrinfo` call on a blocking-pool thread instead of the async
/// worker thread, and the whole resolution is bounded by [`CONNECT_TIMEOUT`]
/// so a slow/unreachable resolver can't stall the dial (or the runtime).
async fn resolve_addr(host: &str, port: u16) -> Result<SocketAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    let lookup = tokio::net::lookup_host((host, port));
    let mut addrs = tokio::time::timeout(CONNECT_TIMEOUT, lookup)
        .await
        .map_err(|_| DomainError::Connection(format!("resolve {host}: timed out")))?
        .map_err(|e| DomainError::Connection(format!("resolve {host}: {e}")))?;
    addrs
        .next()
        .ok_or_else(|| DomainError::Connection(format!("no address for {host}")))
}

/// Accept one inbound connection and its first bidirectional stream.
async fn accept_link(incoming: quinn::Incoming) -> Result<Box<dyn Link>> {
    let conn = incoming.await.map_err(conn_err)?;
    let remote = conn.remote_address();
    let (send, recv) = conn.accept_bi().await.map_err(conn_err)?;
    tracing::debug!(%remote, "quic link established (inbound)");
    Ok(Box::new(QuicLink::new(conn, send, recv)))
}

/// Build a [`Route`] for dialing a plain `address:port` over QUIC.
pub fn direct_route(address: impl Into<String>, port: u16) -> Route {
    use peerbeam_domain::entity::RouteKind;
    Route {
        kind: RouteKind::DirectInternet,
        address: address.into(),
        port,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IP literals must short-circuit without touching the resolver.
    #[tokio::test]
    async fn resolve_addr_ip_literal_is_immediate() {
        let addr = resolve_addr("127.0.0.1", 9000).await.unwrap();
        assert_eq!(addr, "127.0.0.1:9000".parse::<SocketAddr>().unwrap());

        let addr = resolve_addr("::1", 9000).await.unwrap();
        assert_eq!(addr, "[::1]:9000".parse::<SocketAddr>().unwrap());
    }

    /// A hostname is resolved via the async, non-blocking resolver (this must
    /// not deadlock or block the single-threaded test runtime — the old
    /// synchronous `to_socket_addrs()` call ran directly on the async task).
    #[tokio::test]
    async fn resolve_addr_hostname_resolves_via_async_lookup() {
        let addr = resolve_addr("localhost", 9000).await.unwrap();
        assert!(addr.ip().is_loopback());
        assert_eq!(addr.port(), 9000);
    }
}
