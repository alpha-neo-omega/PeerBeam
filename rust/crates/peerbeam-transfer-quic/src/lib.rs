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

mod channels;
mod link;
mod tls;

pub use channels::QuicChannels;
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
    // Every PeerSession channel is one bidi stream (control + up to
    // DEFAULT_CHANNEL_LIMIT=256 data channels). quinn defaults to 100, which is
    // below the app-level channel limit and would make the pump block on
    // `open_bi` back-pressure once ~100 channels are open. Advertise headroom
    // above 256 so the application guard stays authoritative, not the transport.
    tc.max_concurrent_bidi_streams(512u32.into());
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
    /// IPv4 client endpoint. Always present.
    client: quinn::Endpoint,
    /// IPv6 client endpoint, when the host can bind one. See [`Self::new`] for
    /// why this is a second socket rather than one dual-stack socket.
    client_v6: Option<quinn::Endpoint>,
}

impl QuicTransport {
    /// Create a transport that can dial both address families.
    ///
    /// **Two endpoints, not one.** A QUIC endpoint can only dial a peer whose
    /// address family matches its own socket, and this used to bind `0.0.0.0`
    /// alone — so every IPv6 address was undialable, on every platform. That is
    /// not an edge case here: a Tailscale peer advertises a tailnet IPv6
    /// (`fd7a:…`) alongside its IPv4, and a MagicDNS name can resolve to the
    /// IPv6 first, so the one form the tailnet always has was the one form this
    /// could never reach.
    ///
    /// The alternative — a single dual-stack `[::]` socket relying on
    /// IPv4-mapped addresses — is not used deliberately: whether a mapped
    /// address works depends on the host's `IPV6_V6ONLY` default, which differs
    /// across the platforms this ships on. Two explicit sockets behave the same
    /// everywhere.
    ///
    /// The IPv6 endpoint is **best-effort**: a host with IPv6 disabled cannot
    /// bind it, and that must not stop the app from starting. Its absence means
    /// an IPv6 dial fails with a reason rather than being attempted.
    pub fn new() -> Result<Self> {
        let mut transport = Self::bound("0.0.0.0:0".parse().expect("valid addr"))?;
        transport.client_v6 = match "[::]:0".parse().map(quinn::Endpoint::client) {
            Ok(Ok(mut endpoint)) => {
                let mut config = tls::client_config()?;
                config.transport_config(transport_config());
                endpoint.set_default_client_config(config);
                tracing::debug!("quic IPv6 client endpoint ready");
                Some(endpoint)
            }
            _ => {
                // Common and not an error: IPv6 disabled on the host.
                tracing::info!(
                    "no IPv6 client endpoint — IPv6 peers, including tailnet \
                     fd7a: addresses, cannot be dialled from this machine"
                );
                None
            }
        };
        Ok(transport)
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
            client_v6: None,
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
        let (endpoint, local) = server_endpoint(addr)?;
        tracing::info!(%local, "quic serving");
        let stream = accept_loop(endpoint, accept_link);
        Ok((local, stream))
    }

    /// The endpoint whose socket family matches `addr`, if this transport has
    /// one.
    ///
    /// Chosen by each endpoint's **actual bound address**, not by assuming
    /// `client` is IPv4. `bound()` takes whatever the caller gives it — the
    /// network tests bind it to `[::]` — so an assumption here silently broke
    /// IPv6 for every transport not built by `new()`.
    fn endpoint_for(&self, addr: SocketAddr) -> Option<&quinn::Endpoint> {
        let same_family = |endpoint: &quinn::Endpoint| {
            endpoint
                .local_addr()
                .map(|local| local.is_ipv4() == addr.is_ipv4())
                .unwrap_or(false)
        };
        if same_family(&self.client) {
            return Some(&self.client);
        }
        self.client_v6.as_ref().filter(|e| same_family(e))
    }

    /// Connect to `route`, returning the raw QUIC connection (bounded handshake).
    async fn connect(&self, route: &Route, session: &TransferSession) -> Result<quinn::Connection> {
        // EVERY resolved address, not just the first.
        //
        // `resolve_addrs` used to be `resolve_addr` and returned
        // `addrs.next()` — one address, whichever the resolver happened to put
        // first, with no regard for family. A MagicDNS name whose AAAA record
        // sorts first therefore produced an IPv6 address and nothing else was
        // ever tried, even though the same name also resolves to a reachable
        // tailnet IPv4.
        let addrs = resolve_addrs(&route.address, route.port).await?;
        let mut last: Option<DomainError> = None;
        for addr in addrs {
            let endpoint = match self.endpoint_for(addr) {
                Some(endpoint) => endpoint,
                None => {
                    last = Some(DomainError::Connection(format!(
                        "{addr} is IPv{} and this transport has no socket of \
                         that family",
                        if addr.is_ipv4() { "4" } else { "6" }
                    )));
                    continue;
                }
            };
            tracing::info!(peer = %session.peer.0, %addr, kind = ?route.kind, "quic dial");
            let attempt = async {
                let connecting = endpoint.connect(addr, SERVER_NAME).map_err(conn_err)?;
                tokio::time::timeout(CONNECT_TIMEOUT, connecting)
                    .await
                    .map_err(|_| conn_err("connect timed out — peer unreachable"))?
                    .map_err(conn_err)
            };
            match attempt.await {
                Ok(conn) => return Ok(conn),
                Err(e) => {
                    tracing::debug!(%addr, error = %e, "quic dial failed, trying the next address");
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| {
            DomainError::Connection(format!("no usable address for {}", route.address))
        }))
    }

    /// Dial `route` and present the connection as a multi-channel transport
    /// (each channel is a QUIC bidirectional stream).
    pub async fn dial_channels(
        &self,
        route: &Route,
        session: &TransferSession,
    ) -> Result<QuicChannels> {
        Ok(QuicChannels::new(self.connect(route, session).await?))
    }

    /// Serve on `addr`, yielding each inbound connection as a multi-channel
    /// transport, plus the bound local address.
    pub async fn serve_channels_on(
        &self,
        addr: SocketAddr,
    ) -> Result<(SocketAddr, BoxStream<'static, Result<QuicChannels>>)> {
        let (endpoint, local) = server_endpoint(addr)?;
        tracing::info!(%local, "quic serving (channels)");
        let stream = accept_loop(endpoint, |incoming| async move {
            Ok(QuicChannels::new(incoming.await.map_err(conn_err)?))
        });
        Ok((local, stream))
    }
}

/// Build a server endpoint bound to `addr`, returning it and its local address.
fn server_endpoint(addr: SocketAddr) -> Result<(quinn::Endpoint, SocketAddr)> {
    let mut server_config = tls::server_config()?;
    server_config.transport = transport_config();
    let endpoint = quinn::Endpoint::server(server_config, addr).map_err(conn_err)?;
    let local = endpoint.local_addr().map_err(conn_err)?;
    Ok((endpoint, local))
}

/// Drive an accept loop that runs each inbound `handshake` on its **own** task,
/// yielding the results as a stream.
///
/// `quinn::Endpoint::accept()` only dequeues a connection; the handshake happens
/// in the `handshake` future. Awaiting that inline (as a plain `unfold` did)
/// serialises acceptance — one slow or deliberately-stalled peer would block
/// accepting every other connection (head-of-line DoS). Spawning per connection
/// keeps acceptance flowing. The driver stops when the endpoint closes or when
/// the consumer drops the returned stream (`tx.closed()`), which drops the
/// endpoint. A failed handshake is logged and skipped, never fatal to the server.
fn accept_loop<T, F, Fut>(endpoint: quinn::Endpoint, handshake: F) -> BoxStream<'static, Result<T>>
where
    T: Send + 'static,
    F: Fn(quinn::Incoming) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<T>> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<T>>(64);
    let handshake = Arc::new(handshake);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tx.closed() => break, // consumer dropped the stream
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else { break }; // endpoint closed
                    let tx = tx.clone();
                    let handshake = handshake.clone();
                    tokio::spawn(async move {
                        match handshake(incoming).await {
                            Ok(item) => {
                                let _ = tx.send(Ok(item)).await;
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "quic inbound connection rejected");
                            }
                        }
                    });
                }
            }
        }
    });
    Box::pin(futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }))
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
        // Bound the handshake: an unreachable peer must fail fast (the user is
        // watching), not after the 30s idle timeout.
        let conn = self.connect(route, session).await?;
        // Client opens the bidirectional stream; it materialises on the server
        // once the first frame (transfer Meta) is written by the engine.
        let (send, recv) = conn.open_bi().await.map_err(conn_err)?;
        tracing::debug!(remote = %conn.remote_address(), "quic link established (outbound)");
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
async fn resolve_addrs(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let lookup = tokio::net::lookup_host((host, port));
    let addrs = tokio::time::timeout(CONNECT_TIMEOUT, lookup)
        .await
        .map_err(|_| DomainError::Connection(format!("resolve {host}: timed out")))?
        .map_err(|e| DomainError::Connection(format!("resolve {host}: {e}")))?;
    // IPv4 first. Not a preference for IPv4 as such: an IPv6 dial needs the
    // optional v6 endpoint, so trying the family that always has a socket first
    // means the common case connects on the first attempt instead of after a
    // failure. Order within each family is the resolver's.
    let (mut v4, v6): (Vec<_>, Vec<_>) = addrs.partition(|a| a.is_ipv4());
    v4.extend(v6);
    if v4.is_empty() {
        return Err(DomainError::Connection(format!("no address for {host}")));
    }
    Ok(v4)
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
    async fn resolve_addrs_ip_literal_is_immediate() {
        let addrs = resolve_addrs("127.0.0.1", 9000).await.unwrap();
        assert_eq!(addrs, vec!["127.0.0.1:9000".parse::<SocketAddr>().unwrap()]);

        let addrs = resolve_addrs("::1", 9000).await.unwrap();
        assert_eq!(addrs, vec!["[::1]:9000".parse::<SocketAddr>().unwrap()]);
    }

    /// A hostname is resolved via the async, non-blocking resolver (this must
    /// not deadlock or block the single-threaded test runtime — the old
    /// synchronous `to_socket_addrs()` call ran directly on the async task).
    #[tokio::test]
    async fn resolve_addrs_hostname_resolves_via_async_lookup() {
        let addrs = resolve_addrs("localhost", 9000).await.unwrap();
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.ip().is_loopback()));
        assert!(addrs.iter().all(|a| a.port() == 9000));
    }

    /// **Every address, not just the first.** This returned `addrs.next()`, so
    /// a name whose AAAA record sorts first was dialled as IPv6 and nothing
    /// else was attempted — even when the same name also resolves to a
    /// reachable IPv4. `localhost` resolves to both families on a normal host,
    /// which is what makes it a usable probe for the ordering rule.
    #[tokio::test]
    async fn resolve_addrs_puts_ipv4_first_and_keeps_the_rest() {
        let addrs = resolve_addrs("localhost", 9000).await.unwrap();
        // Whatever the resolver returned, no IPv6 may precede an IPv4: the v6
        // endpoint is optional, so trying v4 first is what makes the common
        // case connect without a failed attempt.
        let first_v6 = addrs.iter().position(|a| a.is_ipv6());
        let last_v4 = addrs.iter().rposition(|a| a.is_ipv4());
        if let (Some(v6), Some(v4)) = (first_v6, last_v4) {
            assert!(v4 < v6, "IPv6 sorted before IPv4 in {addrs:?}");
        }
    }
}
