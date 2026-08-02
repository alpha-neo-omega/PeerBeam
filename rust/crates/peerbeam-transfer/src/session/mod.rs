//! PeerSession runtime: the session skeleton built on the domain session types.
//!
//! A [`PeerSession`] drives a single authenticated, secured [`Link`] as the
//! control channel: it exchanges [`SessionHello`], negotiates version and
//! capabilities, tracks lifecycle state, dispatches inbound frames, and closes
//! cleanly. Multiplexed data channels, per-channel keys, reconnect, and resume
//! are later milestones and are intentionally absent here.
//!
//! PeerSession assumes the [`Link`] it is given is already authenticated and
//! sealed (see [`crate::authenticate`] and [`crate::SecureLink`]); it does not
//! re-implement the handshake — one responsibility per component.

mod control;
mod event;
mod registry;

pub use control::{ControlMessage, SessionHello};
pub use event::{
    CloseReason, Keepalive, KeepaliveAction, KeepaliveConfig, SessionEvent, SessionRole,
};
pub use registry::{
    DispatchOutcome, HandlerRegistry, MessageDispatcher, SessionInfo, SessionRegistry,
};

use std::sync::Arc;
use std::time::Instant;

use rand::rngs::OsRng;
use rand::RngCore;
use tokio::sync::mpsc::UnboundedSender;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{Frame, FrameKind, Link};
use peerbeam_domain::session::{
    negotiate_version, Capability, CapabilitySet, ChannelType, MessageHandler, SessionError,
    SessionFrame, SessionId, SessionState, Version, VersionNegotiation,
};

/// Whether the dispatch loop should continue or the session has closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep processing frames.
    Continue,
    /// The session is closed; stop.
    Closed,
}

/// Parameters for opening a session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// The protocol version this side speaks.
    pub version: Version,
    /// The capabilities this side advertises.
    pub capabilities: CapabilitySet,
    /// Keepalive / idle-timeout tuning.
    pub keepalive: KeepaliveConfig,
}

impl SessionConfig {
    /// A config advertising `capabilities` at the current protocol version with
    /// default keepalive tuning. The control capability is always included.
    #[must_use]
    pub fn new(capabilities: CapabilitySet) -> Self {
        let mut caps = capabilities;
        caps.insert(Capability::new(ChannelType::CONTROL));
        SessionConfig {
            version: Version::CURRENT,
            capabilities: caps,
            keepalive: KeepaliveConfig::default(),
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig::new(CapabilitySet::new())
    }
}

/// A live session with one trusted peer over the control channel.
pub struct PeerSession {
    id: SessionId,
    role: SessionRole,
    peer: DeviceId,
    state: SessionState,
    version: Version,
    capabilities: CapabilitySet,
    link: Box<dyn Link>,
    dispatcher: MessageDispatcher,
    events: UnboundedSender<SessionEvent>,
    keepalive: Keepalive,
    registry: Option<SessionRegistry>,
    ping_nonce: u64,
}

impl PeerSession {
    /// Open a session over an already-authenticated, secured `link`.
    ///
    /// The initiator mints the [`SessionId`]; the responder adopts it. Both sides
    /// exchange [`SessionHello`], then version and capabilities are negotiated.
    /// Returns an `Active` session, or an error if the versions are incompatible
    /// or the peer misbehaves during negotiation.
    pub async fn open(
        mut link: Box<dyn Link>,
        role: SessionRole,
        peer: DeviceId,
        config: SessionConfig,
        events: UnboundedSender<SessionEvent>,
        registry: Option<SessionRegistry>,
    ) -> Result<PeerSession, SessionError> {
        let now = Instant::now();
        let our_hello = |session_id| {
            ControlMessage::Hello(SessionHello {
                version: config.version,
                capabilities: config.capabilities.clone(),
                session_id,
            })
        };

        // Exchange Hellos. The initiator speaks first (mints the id); the
        // responder listens first (adopts it), so a duplex link never deadlocks.
        let (session_id, peer_hello) = match role {
            SessionRole::Initiator => {
                let id = random_session_id();
                send_control(link.as_mut(), &our_hello(id)).await?;
                let peer_hello = recv_hello(link.as_mut()).await?;
                (id, peer_hello)
            }
            SessionRole::Responder => {
                let peer_hello = recv_hello(link.as_mut()).await?;
                let id = peer_hello.session_id;
                send_control(link.as_mut(), &our_hello(id)).await?;
                (id, peer_hello)
            }
        };

        // Negotiate version (fatal if the majors differ) and intersect
        // capabilities (behaviour is chosen by what both advertise).
        let version = match negotiate_version(config.version, peer_hello.version) {
            VersionNegotiation::Agreed(v) => v,
            VersionNegotiation::Incompatible { local, peer } => {
                return Err(SessionError::VersionIncompatible { local, peer })
            }
        };
        let capabilities = config.capabilities.intersect(&peer_hello.capabilities);

        let session = PeerSession {
            id: session_id,
            role,
            peer: peer.clone(),
            state: SessionState::Active,
            version,
            capabilities: capabilities.clone(),
            link,
            dispatcher: MessageDispatcher::new(),
            events,
            keepalive: Keepalive::new(config.keepalive, now),
            registry,
            ping_nonce: 0,
        };

        if let Some(reg) = &session.registry {
            reg.register(SessionInfo {
                id: session_id,
                peer: peer.clone(),
                state: SessionState::Active,
                version,
                capabilities: capabilities.clone(),
            });
        }
        session.emit(SessionEvent::Established {
            session_id,
            peer,
            version,
            capabilities,
        });
        Ok(session)
    }

    /// The session id.
    #[must_use]
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// This side's role.
    #[must_use]
    pub fn role(&self) -> SessionRole {
        self.role
    }

    /// The authenticated peer.
    #[must_use]
    pub fn peer(&self) -> &DeviceId {
        &self.peer
    }

    /// The current lifecycle state.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// The negotiated protocol version.
    #[must_use]
    pub fn version(&self) -> Version {
        self.version
    }

    /// The negotiated capabilities (the intersection of both sides').
    #[must_use]
    pub fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    /// Register a capability handler for a data channel type. Data channels are
    /// opened in a later milestone; this is the registration seam.
    pub fn register_handler(&mut self, handler: Arc<dyn MessageHandler>) {
        self.dispatcher.register(handler);
    }

    /// Ask the keepalive scheduler what is due at `now` (read-only). The owner's
    /// event loop calls this on a timer and acts on the result via
    /// [`send_ping`](PeerSession::send_ping) / [`close`](PeerSession::close).
    #[must_use]
    pub fn poll_keepalive(&self, now: Instant) -> KeepaliveAction {
        self.keepalive.due(now)
    }

    /// Send a keepalive `Ping` and record it with the scheduler.
    pub async fn send_ping(&mut self) -> Result<(), SessionError> {
        self.ping_nonce = self.ping_nonce.wrapping_add(1);
        let nonce = self.ping_nonce;
        self.send(&ControlMessage::Ping(nonce)).await?;
        self.keepalive.on_ping_sent(Instant::now());
        Ok(())
    }

    /// Receive one frame and act on it. Returns whether to continue or that the
    /// session has closed.
    pub async fn recv_and_dispatch(&mut self) -> Result<Flow, SessionError> {
        let frame = match recv_session_frame(self.link.as_mut()).await? {
            Some(frame) => frame,
            None => {
                self.mark_closed(CloseReason::Peer("link closed".into()));
                return Ok(Flow::Closed);
            }
        };
        self.keepalive.on_activity(Instant::now());

        if frame.channel.is_control() {
            self.handle_control(&frame).await
        } else {
            match self.dispatcher.dispatch(frame.clone()).await? {
                DispatchOutcome::Handled => Ok(Flow::Continue),
                DispatchOutcome::Unbound | DispatchOutcome::NoHandler => {
                    // Unknown/unroutable data frame: ignore if optional, else tell
                    // the peer we could not handle it. Never tears down the session.
                    if !frame.flags.is_optional() {
                        self.send(&ControlMessage::Unsupported(frame.message_type.get()))
                            .await?;
                    }
                    Ok(Flow::Continue)
                }
            }
        }
    }

    /// Run the receive loop until the session closes.
    pub async fn run(&mut self) -> Result<(), SessionError> {
        while self.recv_and_dispatch().await? == Flow::Continue {}
        Ok(())
    }

    /// Close the session gracefully, notifying the peer.
    pub async fn close(&mut self) -> Result<(), SessionError> {
        self.close_gracefully("local close").await
    }

    /// Close the session gracefully with a specific reason sent to the peer.
    pub async fn shutdown(&mut self, reason: impl Into<String>) -> Result<(), SessionError> {
        self.close_gracefully(&reason.into()).await
    }

    async fn close_gracefully(&mut self, reason: &str) -> Result<(), SessionError> {
        if self.state.is_terminal() {
            return Ok(());
        }
        if self.state.is_active() {
            self.state = SessionState::ShuttingDown;
        }
        // Best-effort: notify the peer and close the transport even if either
        // step fails (we are tearing down regardless).
        let _ = self
            .send(&ControlMessage::Shutdown(reason.to_string()))
            .await;
        let _ = self.link.close().await;
        self.state = SessionState::Closed;
        self.finish_close(CloseReason::Local);
        Ok(())
    }

    async fn handle_control(&mut self, frame: &SessionFrame) -> Result<Flow, SessionError> {
        let msg = match ControlMessage::from_frame(frame) {
            Ok(msg) => msg,
            Err(_) => {
                // Unknown/corrupt control message: reply Unsupported unless the
                // sender marked it ignorable. The session survives.
                if !frame.flags.is_optional() {
                    self.send(&ControlMessage::Unsupported(frame.message_type.get()))
                        .await?;
                }
                return Ok(Flow::Continue);
            }
        };
        match msg {
            ControlMessage::Hello(_) => {
                // A stray Hello after establishment is a protocol nicety we
                // ignore rather than fault the session over.
                tracing::debug!(session = %self.id, "ignoring unexpected Hello after establishment");
                Ok(Flow::Continue)
            }
            ControlMessage::Ping(nonce) => {
                self.emit(SessionEvent::PingReceived {
                    session_id: self.id,
                });
                self.send(&ControlMessage::Pong(nonce)).await?;
                Ok(Flow::Continue)
            }
            ControlMessage::Pong(_) => Ok(Flow::Continue),
            ControlMessage::Shutdown(reason) => {
                self.mark_closed(CloseReason::Peer(reason));
                Ok(Flow::Closed)
            }
            ControlMessage::Unsupported(_) => Ok(Flow::Continue),
        }
    }

    async fn send(&mut self, msg: &ControlMessage) -> Result<(), SessionError> {
        send_control(self.link.as_mut(), msg).await
    }

    /// Mark a peer/timeout-initiated close. `Closed` is a legal transition from
    /// any non-terminal state, so this always succeeds without a forced override.
    fn mark_closed(&mut self, reason: CloseReason) {
        if self.state.is_terminal() {
            return;
        }
        debug_assert!(self.state.can_transition_to(SessionState::Closed));
        self.state = SessionState::Closed;
        self.finish_close(reason);
    }

    fn finish_close(&mut self, reason: CloseReason) {
        if let Some(reg) = &self.registry {
            reg.remove(self.id);
        }
        self.emit(SessionEvent::Closed {
            session_id: self.id,
            reason,
        });
    }

    fn emit(&self, event: SessionEvent) {
        // Best-effort: a dropped receiver just means no one is listening.
        let _ = self.events.send(event);
    }
}

/// Wrap a session frame in a transport frame and send it.
async fn send_control(link: &mut dyn Link, msg: &ControlMessage) -> Result<(), SessionError> {
    let session_frame = msg.to_frame()?;
    let frame = Frame {
        kind: FrameKind::Control,
        payload: session_frame.encode(),
    };
    link.send_frame(frame).await?;
    Ok(())
}

/// Receive one transport frame and decode the session frame it carries.
async fn recv_session_frame(link: &mut dyn Link) -> Result<Option<SessionFrame>, SessionError> {
    match link.recv_frame().await? {
        Some(frame) => Ok(Some(SessionFrame::decode(&frame.payload)?)),
        None => Ok(None),
    }
}

/// Receive the peer's opening [`SessionHello`], rejecting anything else.
async fn recv_hello(link: &mut dyn Link) -> Result<SessionHello, SessionError> {
    match recv_session_frame(link).await? {
        Some(frame) => match ControlMessage::from_frame(&frame)? {
            ControlMessage::Hello(hello) => Ok(hello),
            other => Err(SessionError::UnexpectedMessage {
                state: SessionState::Negotiating,
                detail: format!("expected Hello, got {other:?}"),
            }),
        },
        None => Err(SessionError::Link("link closed during negotiation".into())),
    }
}

/// Mint a random 128-bit session id from the OS RNG.
fn random_session_id() -> SessionId {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    SessionId::from_bytes(bytes)
}
