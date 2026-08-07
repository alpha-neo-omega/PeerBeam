//! Session reconnect + resume driver (M6).
//!
//! When an `Active` session loses its transport, its **identity is not lost**: the
//! authenticated master secret, [`SessionId`], negotiated version, capabilities,
//! and channel registry are captured in a [`PreservedSession`]. A
//! [`RecoveryManager`] then re-establishes a transport (via a [`TransportFactory`],
//! e.g. `RouteManager` re-selecting a route) and **resumes** the session with a
//! single-use [`ResumeToken`](super::resume::ResumeToken) — no repeated handshake,
//! same authenticated identity, fresh crypto epoch (so no nonce is ever reused).
//!
//! This is pure infrastructure: it is capability-agnostic. Message channels are
//! re-attached automatically; a stream capability (e.g. transfer) observes the
//! recovery and re-opens its own channel, resuming its payload from its own
//! checkpoint (the transfer engine already does this via the `ReliabilityStore`
//! and [`crate::recover`], which this layer deliberately does not duplicate).
//!
//! On success the session continues at [`SessionState::Active`]; if attempts are
//! exhausted or a token is refused, it fails closed to
//! [`SessionState::Closed`](peerbeam_domain::session::SessionState) (I11).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::ChannelTransport;
use peerbeam_domain::session::{
    CapabilitySet, ChannelId, ChannelType, SessionError, SessionId, Version,
};

use super::crypto::SessionCrypto;
use super::event::{SessionEvent, SessionRole};
use super::registry::SessionRegistry;
use super::{ChannelEvent, IncomingStreamChannel, PeerSession, SessionConfig};

/// Tuning for the reconnect loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryConfig {
    /// Maximum reconnect attempts before giving up.
    pub max_attempts: u32,
    /// Base backoff between attempts (grows linearly with the attempt number).
    pub backoff_base: Duration,
    /// Per-attempt timeout for obtaining a transport + completing the resume
    /// handshake.
    pub attempt_timeout: Duration,
    /// Resume-token validity window, in milliseconds.
    pub token_ttl_ms: u64,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        RecoveryConfig {
            max_attempts: 5,
            backoff_base: Duration::from_millis(50),
            attempt_timeout: Duration::from_secs(10),
            token_ttl_ms: 30_000,
        }
    }
}

/// Cumulative recovery counters, carried across reconnects for observability.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryStats {
    /// Total reconnect attempts made over the session's life.
    pub attempts: u32,
    /// Successful resumes (times the session was recovered).
    pub resumes: u32,
    /// The current crypto epoch (reconnect generation) in force.
    pub epoch: u64,
}

/// Produces a fresh [`ChannelTransport`] for a reconnect. For the redialling side
/// this dials the peer (e.g. `RouteManager` picks the best current route); for the
/// accepting side it yields the next inbound connection.
#[async_trait]
pub trait TransportFactory: Send {
    /// Obtain the next transport to (re)attach the session over.
    async fn connect(&mut self) -> Result<Arc<dyn ChannelTransport>, SessionError>;
}

/// Everything needed to rebuild a [`PeerSession`] after a reconnect, minus the
/// transport. Captured from a live session at the moment its transport is lost;
/// the master secret it carries is what makes resume possible without re-auth.
pub struct PreservedSession {
    pub(crate) session_id: SessionId,
    /// This endpoint's own device id (half of the resume-token identity pair).
    pub(crate) local: DeviceId,
    pub(crate) peer: DeviceId,
    pub(crate) role: SessionRole,
    pub(crate) version: Version,
    pub(crate) capabilities: CapabilitySet,
    pub(crate) crypto: SessionCrypto,
    /// Highest resume epoch this endpoint has consumed (accepter's replay guard).
    pub(crate) consumed_epoch: u64,
    /// Open **message** channels to re-attach (id + type). Stream channels are not
    /// listed — their capability re-opens them.
    pub(crate) channels: Vec<(ChannelId, ChannelType)>,
    pub(crate) stats: RecoveryStats,
}

impl PreservedSession {
    /// The preserved session id (unchanged across reconnects).
    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// The authenticated peer whose identity is preserved.
    #[must_use]
    pub fn peer(&self) -> &DeviceId {
        &self.peer
    }

    /// The negotiated protocol version (unchanged across reconnects).
    #[must_use]
    pub fn version(&self) -> Version {
        self.version
    }

    /// The current crypto epoch at capture time.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.crypto.epoch()
    }

    /// Recovery statistics at capture time.
    #[must_use]
    pub fn stats(&self) -> RecoveryStats {
        self.stats
    }
}

/// How a session pump exited: cleanly, or with a recoverable transport loss that
/// carries the state needed to resume.
pub enum RunExit {
    /// The session closed (graceful shutdown, peer shutdown, or a fatal error).
    /// Not recoverable.
    Closed,
    /// The transport was lost while the session was `Active`. The boxed state can
    /// be handed to [`PeerSession::resume`]/[`PeerSession::accept_resume`].
    Lost(Box<PreservedSession>),
}

/// The wiring a (re)built session reports through: session events, per-channel
/// events, incoming stream channels, and the optional registry. Cloned for each
/// rebuild across a reconnect.
#[derive(Clone)]
pub struct SessionWiring {
    /// Session lifecycle events.
    pub events: tokio::sync::mpsc::UnboundedSender<SessionEvent>,
    /// Per-channel lifecycle events.
    pub channel_events: tokio::sync::mpsc::UnboundedSender<ChannelEvent>,
    /// Incoming stream channels (for stream capabilities such as transfer).
    pub incoming_streams: tokio::sync::mpsc::UnboundedSender<IncomingStreamChannel>,
    /// Optional session registry.
    pub registry: Option<SessionRegistry>,
}

/// Drives a session across reconnects: runs the current session, and on a
/// recoverable loss re-establishes the transport and resumes, until the session
/// closes or attempts are exhausted.
///
/// One manager per endpoint. The **initiator** side redials and resumes; the
/// **responder** side accepts and validates — determined by the preserved
/// [`SessionRole`], so the crypto direction is stable across reconnects.
pub struct RecoveryManager {
    session: Option<PeerSession>,
    factory: Box<dyn TransportFactory>,
    config: RecoveryConfig,
    session_config: SessionConfig,
    wiring: SessionWiring,
    stats: RecoveryStats,
    /// The current session's handle, republished on every (re)establish so a
    /// long-lived caller always reaches the live session across reconnects.
    handle_tx: tokio::sync::watch::Sender<Option<super::SessionHandle>>,
}

impl RecoveryManager {
    /// Wrap an already-established `session` so it survives transport loss.
    /// `factory` provides fresh transports; `session_config` supplies the handlers
    /// and stream-channel types the rebuilt sessions reuse.
    pub fn new(
        session: PeerSession,
        factory: Box<dyn TransportFactory>,
        config: RecoveryConfig,
        session_config: SessionConfig,
        wiring: SessionWiring,
    ) -> Self {
        let stats = RecoveryStats {
            epoch: session.crypto_epoch(),
            ..RecoveryStats::default()
        };
        let (handle_tx, _) = tokio::sync::watch::channel(Some(session.handle()));
        RecoveryManager {
            session: Some(session),
            factory,
            config,
            session_config,
            wiring,
            stats,
            handle_tx,
        }
    }

    /// Current recovery statistics.
    #[must_use]
    pub fn stats(&self) -> RecoveryStats {
        self.stats
    }

    /// A handle to the currently-live session, if any. Note the handle is
    /// per-connection: after a reconnect the previous handle is dead and this
    /// returns the new one.
    #[must_use]
    pub fn handle(&self) -> Option<super::SessionHandle> {
        self.session.as_ref().map(PeerSession::handle)
    }

    /// Subscribe to the current session handle. Updates on every reconnect (and to
    /// `None` when the session finally closes), so a caller can always send on the
    /// live session without racing a reconnect.
    #[must_use]
    pub fn handle_watch(&self) -> tokio::sync::watch::Receiver<Option<super::SessionHandle>> {
        self.handle_tx.subscribe()
    }

    /// Run the session, recovering across transport losses until it closes or
    /// recovery is exhausted. Returns `Ok(())` on a clean close;
    /// [`SessionError::RecoveryExhausted`] (or a fatal resume rejection) when
    /// recovery gives up.
    pub async fn run(&mut self) -> Result<(), SessionError> {
        loop {
            let mut session = match self.session.take() {
                Some(s) => s,
                None => return Ok(()),
            };
            match session.run().await? {
                RunExit::Closed => {
                    let _ = self.handle_tx.send(None);
                    return Ok(());
                }
                RunExit::Lost(preserved) => {
                    self.recover(*preserved).await?;
                    // `recover` installed a fresh session; loop to run it.
                }
            }
        }
    }

    /// Attempt to resume `preserved` over fresh transports, bounded by config.
    async fn recover(&mut self, mut preserved: PreservedSession) -> Result<(), SessionError> {
        let session_id = preserved.session_id;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            if attempt > self.config.max_attempts {
                let _ = self.handle_tx.send(None);
                let _ = self.wiring.events.send(SessionEvent::RecoveryFailed {
                    session_id,
                    reason: format!("attempts exhausted after {}", attempt - 1),
                });
                return Err(SessionError::RecoveryExhausted {
                    attempts: attempt - 1,
                });
            }
            // Count only real attempts (past the bound check), so the public
            // stat agrees with the `RecoveryExhausted { attempts }` value.
            self.stats.attempts += 1;
            let _ = self.wiring.events.send(SessionEvent::Recovering {
                session_id,
                attempt,
            });
            backoff(self.config.backoff_base, attempt).await;

            let connect = tokio::time::timeout(self.config.attempt_timeout, self.factory.connect());
            let transport = match connect.await {
                Ok(Ok(t)) => t,
                Ok(Err(_)) | Err(_) => continue, // dial failed or timed out → retry
            };

            // The resume handshake must also be bounded by attempt_timeout: a peer
            // that never sends ResumeAck (asymmetric loss / half-open link) would
            // otherwise block recv_frame forever, so recover() never returns and
            // the I11 fail-closed guarantee is never reached. On timeout treat the
            // attempt as failed and retry within the budget.
            let handshake = async {
                match preserved.role {
                    SessionRole::Initiator => {
                        // Bump the epoch per attempt so a partially-consumed epoch
                        // is never re-presented (the accepter rejects replays).
                        let epoch = preserved.crypto.epoch() + attempt as u64;
                        PeerSession::resume(
                            transport,
                            &preserved,
                            epoch,
                            &self.session_config,
                            self.config.token_ttl_ms,
                            self.wiring.clone(),
                        )
                        .await
                        .map(|s| (s, epoch))
                    }
                    SessionRole::Responder => PeerSession::accept_resume(
                        transport,
                        &preserved,
                        &self.session_config,
                        self.wiring.clone(),
                    )
                    .await
                    .map(|(s, consumed)| {
                        preserved.consumed_epoch = consumed;
                        let e = s.crypto_epoch();
                        (s, e)
                    }),
                }
            };
            let outcome = match tokio::time::timeout(self.config.attempt_timeout, handshake).await {
                Ok(o) => o,
                Err(_) => continue, // handshake timed out → retry within budget
            };

            match outcome {
                Ok((session, epoch)) => {
                    self.stats.resumes += 1;
                    self.stats.epoch = epoch;
                    let _ = self.handle_tx.send(Some(session.handle()));
                    let _ = self
                        .wiring
                        .events
                        .send(SessionEvent::Recovered { session_id, epoch });
                    self.session = Some(session);
                    return Ok(());
                }
                // A refused resume token — forged, expired, or replayed.
                Err(
                    e @ (SessionError::ResumeRejected(_)
                    | SessionError::ResumeExpired
                    | SessionError::ResumeReplayed),
                ) => match preserved.role {
                    // Initiator: *we* minted the refused credential, so it will
                    // keep failing — fail closed for a fresh session.
                    SessionRole::Initiator => {
                        let _ = self.handle_tx.send(None);
                        let _ = self.wiring.events.send(SessionEvent::RecoveryFailed {
                            session_id,
                            reason: e.to_string(),
                        });
                        return Err(e);
                    }
                    // Responder: the refused token came from an *inbound*
                    // connection — a stray, stale, or forged peer, not our own
                    // failure. Count it as one attempt and keep accepting the
                    // next inbound within the budget, so the legitimate peer can
                    // still resume. (Otherwise an unauthenticated remote could
                    // force a recoverable session closed by racing a bad
                    // ResumeRequest into the recovery window.)
                    SessionRole::Responder => continue,
                },
                // Transient (dial/link glitch): retry within the attempt budget.
                Err(_) => continue,
            }
        }
    }
}

/// Linear backoff: `base * attempt`.
async fn backoff(base: Duration, attempt: u32) {
    tokio::time::sleep(base * attempt).await;
}
