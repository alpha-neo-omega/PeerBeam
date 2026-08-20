//! PeerSession runtime: an authenticated session multiplexing N channels.
//!
//! A [`PeerSession`] owns one authenticated, secured connection
//! ([`ChannelTransport`]) and drives it: it takes the first stream as the
//! **control channel**, exchanges [`SessionHello`], negotiates version and
//! capabilities, then runs a single pump that routes control messages, accepts
//! inbound data streams, and coordinates the [`ChannelManager`]. Each data
//! channel is an independent stream with its own actor, ordering, flow control,
//! and lifecycle — one channel's failure never touches another.
//!
//! Per-channel encryption keys (M4), transfer as a channel (M5), and reconnect +
//! resume (M6) are built on this runtime. Reconnect/resume live in
//! [`recovery`]: a lost `Active` session is captured as a [`PreservedSession`] and
//! re-established over a fresh transport with a single-use [`ResumeToken`], at a
//! bumped crypto epoch so no nonce is ever reused. PeerSession assumes the
//! transport's streams are already authenticated and sealed (see
//! [`crate::authenticate`] and [`crate::SecureLink`]).

mod channel;
mod channel_manager;
mod control;
mod crypto;
mod event;
mod pipe;
mod recovery;
mod registry;
mod resume;
mod sealed_link;
mod transfer;

pub use channel::{ChannelEvent, ChannelInfo, ChannelStats, IncomingStreamChannel};
pub use channel_manager::ChannelManager;
pub use control::{ControlMessage, SessionHello};
pub(crate) use crypto::{ChannelCrypto, SessionCrypto};
pub use event::{
    CloseReason, Keepalive, KeepaliveAction, KeepaliveConfig, SessionEvent, SessionRole,
};
pub use pipe::{accept_pipe, send_pipe_on_session, PipeConsent};
pub use recovery::{
    PreservedSession, RecoveryConfig, RecoveryManager, RecoveryStats, RunExit, SessionWiring,
    TransportFactory,
};
pub use registry::{HandlerRegistry, SessionInfo, SessionRegistry};
pub use resume::{ResumeBinding, ResumeToken};
pub use transfer::{
    peek_incoming_meta, receive_file_on_channel, receive_folder_on_channel, receive_on_channel,
    send_file_on_session, send_file_on_session_recover, send_folder_on_session, ChannelReceived,
    TransferPreview,
};

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use rand::rngs::OsRng;
use rand::RngCore;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{
    ChannelTransport, EncryptionProvider, Frame, FrameKind, Link, TrustStore,
};
use peerbeam_domain::session::{
    negotiate_version, Capability, CapabilitySet, ChannelId, ChannelType, MessageFlags,
    MessageType, SessionError, SessionFrame, SessionId, SessionState, Version, VersionNegotiation,
};

use crate::auth::{authenticate, Identity};

use channel::ActorEvent;

/// Whether the pump should continue or the session has closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Closed,
}

/// Parameters for opening a session.
#[derive(Clone)]
pub struct SessionConfig {
    /// The protocol version this side speaks.
    pub version: Version,
    /// The capabilities this side advertises.
    pub capabilities: CapabilitySet,
    /// Keepalive / idle-timeout tuning.
    pub keepalive: KeepaliveConfig,
    /// Handlers for data-channel capabilities.
    pub handlers: HandlerRegistry,
    /// Maximum concurrently-open data channels.
    pub channel_limit: usize,
    /// Capabilities whose channels are **stream** channels: the caller owns the
    /// sealed stream and runs its own protocol over it (e.g. transfer), instead
    /// of the frame being dispatched to a [`MessageHandler`]. Empty by default —
    /// every channel is a message channel unless opted in here.
    ///
    /// [`MessageHandler`]: peerbeam_domain::session::MessageHandler
    pub stream_channel_types: HashSet<ChannelType>,
}

impl SessionConfig {
    /// A config advertising `capabilities` at the current protocol version with
    /// default keepalive and no handlers. The control capability is always
    /// included.
    #[must_use]
    pub fn new(capabilities: CapabilitySet) -> Self {
        let mut caps = capabilities;
        caps.insert(Capability::new(ChannelType::CONTROL));
        SessionConfig {
            version: Version::CURRENT,
            capabilities: caps,
            keepalive: KeepaliveConfig::default(),
            handlers: HandlerRegistry::new(),
            channel_limit: channel_manager::DEFAULT_CHANNEL_LIMIT,
            stream_channel_types: HashSet::new(),
        }
    }

    /// Set the data-channel handlers.
    #[must_use]
    pub fn with_handlers(mut self, handlers: HandlerRegistry) -> Self {
        self.handlers = handlers;
        self
    }

    /// Mark `channel_type` as a stream capability: its channels deliver the
    /// sealed stream to the caller (opener via
    /// [`SessionHandle::open_stream_channel`], accepter via the session's
    /// incoming-streams receiver) instead of dispatching to a handler.
    #[must_use]
    pub fn with_stream_channel_type(mut self, channel_type: ChannelType) -> Self {
        self.stream_channel_types.insert(channel_type);
        self
    }

    /// Set the maximum number of concurrently-open data channels.
    #[must_use]
    pub fn with_channel_limit(mut self, limit: usize) -> Self {
        self.channel_limit = limit;
        self
    }
}

/// Reply to an [`SessionCommand::OpenStreamChannel`]: the new channel's id and
/// the sealed stream the caller owns.
type OpenStreamReply = Result<(ChannelId, Box<dyn Link>), SessionError>;

/// A command sent to a running session's pump via its [`SessionHandle`].
enum SessionCommand {
    OpenChannel {
        channel_type: ChannelType,
        reply: oneshot::Sender<Result<ChannelId, SessionError>>,
    },
    OpenStreamChannel {
        channel_type: ChannelType,
        reply: oneshot::Sender<OpenStreamReply>,
    },
    SendOnChannel {
        channel: ChannelId,
        message_type: MessageType,
        flags: MessageFlags,
        payload: Bytes,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    CloseChannel {
        channel: ChannelId,
    },
    Snapshot {
        reply: oneshot::Sender<Vec<ChannelInfo>>,
    },
    Ping,
    Close,
}

/// A cloneable handle for controlling a session while its [`run`](PeerSession::run)
/// pump owns the session object.
#[derive(Clone)]
pub struct SessionHandle {
    commands: UnboundedSender<SessionCommand>,
}

impl SessionHandle {
    /// Open a data channel of `channel_type`; resolves to its id once the peer
    /// accepts the open request has been sent (the channel becomes `Open` on the
    /// peer's accept, observable via [`ChannelEvent::Opened`]).
    pub async fn open_channel(&self, channel_type: ChannelType) -> Result<ChannelId, SessionError> {
        let (reply, rx) = oneshot::channel();
        self.commands
            .send(SessionCommand::OpenChannel {
                channel_type,
                reply,
            })
            .map_err(|_| SessionError::Closed)?;
        rx.await.map_err(|_| SessionError::Closed)?
    }

    /// Open a **stream** channel of `channel_type` (must be configured as a
    /// stream capability via [`SessionConfig::with_stream_channel_type`]).
    /// Resolves to the channel id and the sealed stream, which the caller owns
    /// and runs its own protocol over (e.g. `send_file`). The peer's accepter
    /// receives the matching [`IncomingStreamChannel`] on the session's
    /// incoming-streams receiver.
    pub async fn open_stream_channel(
        &self,
        channel_type: ChannelType,
    ) -> Result<(ChannelId, Box<dyn Link>), SessionError> {
        let (reply, rx) = oneshot::channel();
        self.commands
            .send(SessionCommand::OpenStreamChannel {
                channel_type,
                reply,
            })
            .map_err(|_| SessionError::Closed)?;
        rx.await.map_err(|_| SessionError::Closed)?
    }

    /// Send an application frame on an open channel.
    pub async fn send_on_channel(
        &self,
        channel: ChannelId,
        message_type: MessageType,
        flags: MessageFlags,
        payload: Bytes,
    ) -> Result<(), SessionError> {
        let (reply, rx) = oneshot::channel();
        self.commands
            .send(SessionCommand::SendOnChannel {
                channel,
                message_type,
                flags,
                payload,
                reply,
            })
            .map_err(|_| SessionError::Closed)?;
        rx.await.map_err(|_| SessionError::Closed)?
    }

    /// Close a channel (best-effort).
    pub fn close_channel(&self, channel: ChannelId) {
        let _ = self.commands.send(SessionCommand::CloseChannel { channel });
    }

    /// Send a keepalive ping; the peer replies with a pong (best-effort).
    pub fn ping(&self) {
        let _ = self.commands.send(SessionCommand::Ping);
    }

    /// A snapshot of every live channel's metadata and statistics.
    /// Wait until `channel` is open, or say why it will never be.
    ///
    /// # Why every caller needs this
    ///
    /// [`open_channel`](Self::open_channel) resolves as soon as the open is
    /// *queued locally*. The peer's `ChannelAccept` has not arrived, so a send
    /// on the very next line races it — and over QUIC it does not merely
    /// arrive early, it hard-fails with "channel not open". A caller that skips
    /// this wait therefore fails on the **first frame of every channel it
    /// opens**, and reports it as a peer that did not answer.
    ///
    /// This lives on the handle because four separate copies of the same loop
    /// had already grown in `peerbeam-chat`, `peerbeam-presence`,
    /// `peerbeam-clipboard` and the FFI's session executor — and the CLI, which
    /// had none, was silently broken for browse, sync and pairing. Those four
    /// are worth migrating here; this is the home they should migrate to.
    ///
    /// An absent channel is terminal, not "not yet": the pump registers it
    /// before `open_channel` returns and this reads the same registry over the
    /// same ordered queue, so missing means already removed — refused, or its
    /// actor died.
    pub async fn await_channel_open(
        &self,
        channel: ChannelId,
        budget: std::time::Duration,
    ) -> Result<(), SessionError> {
        const POLL: std::time::Duration = std::time::Duration::from_millis(10);
        let deadline = std::time::Instant::now() + budget;
        loop {
            match self
                .channels()
                .await?
                .iter()
                .find(|c| c.id == channel)
                .map(|c| c.state)
            {
                Some(peerbeam_domain::session::ChannelState::Opening) => {}
                Some(s) if s.is_open() => return Ok(()),
                _ => {
                    return Err(SessionError::Channel(
                        "channel was refused by the device".into(),
                    ))
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(SessionError::Channel(format!(
                    "channel did not open within {budget:?}"
                )));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    pub async fn channels(&self) -> Result<Vec<ChannelInfo>, SessionError> {
        let (reply, rx) = oneshot::channel();
        self.commands
            .send(SessionCommand::Snapshot { reply })
            .map_err(|_| SessionError::Closed)?;
        rx.await.map_err(|_| SessionError::Closed)
    }

    /// Close the whole session (best-effort).
    pub fn close(&self) {
        let _ = self.commands.send(SessionCommand::Close);
    }
}

/// Owned value produced by one pump wake-up, so the `select!` borrows end before
/// the session is mutated.
enum Wake {
    Control(Result<Option<SessionFrame>, SessionError>),
    Accept(Result<Option<Box<dyn Link>>, SessionError>),
    Actor(Option<ActorEvent>),
    Command(Option<SessionCommand>),
    /// The idle keepalive timer elapsed — consult the scheduler.
    Tick,
}

/// A live multiplexed session with one trusted peer.
pub struct PeerSession {
    id: SessionId,
    role: SessionRole,
    /// This endpoint's own device id (for the resume-token identity pair).
    local: DeviceId,
    peer: DeviceId,
    state: SessionState,
    version: Version,
    capabilities: CapabilitySet,
    control: Box<dyn Link>,
    control_crypto: ChannelCrypto,
    enc: Arc<dyn EncryptionProvider>,
    manager: ChannelManager,
    events: UnboundedSender<SessionEvent>,
    keepalive: Keepalive,
    registry: Option<SessionRegistry>,
    control_out: VecDeque<ControlMessage>,
    commands_tx: UnboundedSender<SessionCommand>,
    commands_rx: UnboundedReceiver<SessionCommand>,
    actor_events_rx: UnboundedReceiver<ActorEvent>,
    accepting: bool,
    ping_nonce: u64,
    /// Highest resume epoch this endpoint has consumed (accepter's single-use
    /// replay guard); preserved across reconnects. 0 for a freshly-opened session.
    consumed_epoch: u64,
    /// Whether the peer was newly TOFU-pinned during this session's handshake
    /// (surfaced to the UI/CLI). Always false for a resumed session (no handshake).
    newly_trusted: bool,
    /// The peer's presented human name from the handshake (empty for a resumed
    /// session — the name is not re-exchanged).
    peer_name: String,
    /// The first-contact pairing code from this session's handshake (empty for
    /// a resumed session — there is no handshake to derive it from).
    pairing_code: String,
    /// This handshake's transcript, for binding a PIN-pairing proof to it.
    /// Empty for a resumed session, which has no handshake to bind to — and a
    /// PIN proved against nothing is not a proof.
    transcript: Vec<u8>,
}

impl PeerSession {
    /// Open a session over an authenticated, secured `transport`.
    ///
    /// Takes the first stream as the control channel, exchanges [`SessionHello`]
    /// (initiator mints the id, responder adopts it), then negotiates version and
    /// capabilities. `channel_events` receives per-channel lifecycle events.
    #[allow(clippy::too_many_arguments)]
    pub async fn open(
        transport: Arc<dyn ChannelTransport>,
        role: SessionRole,
        config: SessionConfig,
        events: UnboundedSender<SessionEvent>,
        channel_events: UnboundedSender<ChannelEvent>,
        incoming_streams: UnboundedSender<IncomingStreamChannel>,
        registry: Option<SessionRegistry>,
        identity: Identity,
        enc: Arc<dyn EncryptionProvider>,
        trust: Arc<dyn TrustStore>,
    ) -> Result<PeerSession, SessionError> {
        let now = Instant::now();

        // The control channel is the first stream: the initiator opens it, the
        // responder accepts it (before any data streams).
        let mut control: Box<dyn Link> = match role {
            SessionRole::Initiator => transport.open_stream().await?,
            SessionRole::Responder => transport.accept_stream().await?.ok_or_else(|| {
                SessionError::Link("transport closed before control stream".into())
            })?,
        };

        let local = identity.device_id.clone();
        // The authenticated handshake runs exactly once per session, over the raw
        // control stream. Its master secret keys every channel (control included).
        let auth_session = authenticate(control.as_mut(), &identity, &*enc, &*trust).await?;
        let peer = auth_session.peer_id.clone();
        let newly_trusted = auth_session.newly_trusted;
        let peer_name = auth_session.peer_name.clone();
        let pairing_code = auth_session.pairing_code.clone();
        let transcript = auth_session.transcript.clone();
        let session_crypto = SessionCrypto::from_session(&auth_session, role, enc.clone());
        let mut control_crypto = session_crypto.control()?;

        // From here the control channel is sealed with its own derived key.
        let our_hello = |session_id| {
            ControlMessage::Hello(SessionHello {
                version: config.version,
                capabilities: config.capabilities.clone(),
                session_id,
            })
        };
        let (session_id, peer_hello) = match role {
            SessionRole::Initiator => {
                let id = random_session_id();
                send_control(control.as_mut(), &mut control_crypto, &*enc, &our_hello(id)).await?;
                (
                    id,
                    recv_hello(control.as_mut(), &mut control_crypto, &*enc).await?,
                )
            }
            SessionRole::Responder => {
                let peer_hello = recv_hello(control.as_mut(), &mut control_crypto, &*enc).await?;
                let id = peer_hello.session_id;
                send_control(control.as_mut(), &mut control_crypto, &*enc, &our_hello(id)).await?;
                (id, peer_hello)
            }
        };

        let version = match negotiate_version(config.version, peer_hello.version) {
            VersionNegotiation::Agreed(v) => v,
            VersionNegotiation::Incompatible {
                local,
                peer: peer_version,
            } => {
                return Err(SessionError::VersionIncompatible {
                    local,
                    peer: peer_version,
                })
            }
        };
        let capabilities = config.capabilities.intersect(&peer_hello.capabilities);

        let wiring = SessionWiring {
            events,
            channel_events,
            incoming_streams,
            registry,
        };
        let mut session = Self::assemble(
            transport,
            role,
            session_id,
            local,
            peer,
            version,
            capabilities,
            session_crypto,
            control,
            control_crypto,
            enc,
            &config,
            wiring,
            0,
            now,
        );
        session.newly_trusted = newly_trusted;
        session.peer_name = peer_name;
        session.pairing_code = pairing_code;
        session.transcript = transcript;
        Ok(session)
    }

    /// Build a live `PeerSession` from already-established, already-keyed parts.
    /// Shared by [`open`](PeerSession::open) (fresh handshake) and
    /// [`resume`](PeerSession::resume)/[`accept_resume`](PeerSession::accept_resume)
    /// (reconnect), so the assembly, registry registration, and `Established`
    /// event are defined exactly once.
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        transport: Arc<dyn ChannelTransport>,
        role: SessionRole,
        session_id: SessionId,
        local: DeviceId,
        peer: DeviceId,
        version: Version,
        capabilities: CapabilitySet,
        session_crypto: SessionCrypto,
        control: Box<dyn Link>,
        control_crypto: ChannelCrypto,
        enc: Arc<dyn EncryptionProvider>,
        config: &SessionConfig,
        wiring: SessionWiring,
        consumed_epoch: u64,
        now: Instant,
    ) -> PeerSession {
        let (actor_events_tx, actor_events_rx) = unbounded_channel();
        let (commands_tx, commands_rx) = unbounded_channel();
        let manager = ChannelManager::new(
            transport,
            session_crypto,
            version,
            role,
            config.handlers.clone(),
            capabilities.clone(),
            config.channel_limit,
            config.stream_channel_types.clone(),
            wiring.channel_events,
            actor_events_tx,
            wiring.incoming_streams,
        );

        let session = PeerSession {
            id: session_id,
            role,
            local,
            peer: peer.clone(),
            state: SessionState::Active,
            version,
            capabilities: capabilities.clone(),
            control,
            control_crypto,
            enc,
            manager,
            events: wiring.events,
            keepalive: Keepalive::new(config.keepalive, now),
            registry: wiring.registry,
            control_out: VecDeque::new(),
            commands_tx,
            commands_rx,
            actor_events_rx,
            accepting: true,
            ping_nonce: 0,
            consumed_epoch,
            newly_trusted: false,
            peer_name: String::new(),
            pairing_code: String::new(),
            // A resumed session has no handshake, so nothing to bind a PIN
            // proof to. Empty is the honest answer, and `transcript()` says so.
            transcript: Vec::new(),
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
        session
    }

    /// A cloneable handle to control this session while [`run`](PeerSession::run)
    /// owns it.
    #[must_use]
    pub fn handle(&self) -> SessionHandle {
        SessionHandle {
            commands: self.commands_tx.clone(),
        }
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

    /// Whether the peer was newly TOFU-pinned during this session's handshake.
    #[must_use]
    pub fn newly_trusted(&self) -> bool {
        self.newly_trusted
    }

    /// The peer's presented human name from the handshake (may be empty).
    #[must_use]
    pub fn peer_name(&self) -> &str {
        &self.peer_name
    }

    /// The first-contact pairing code from this session's handshake (empty for
    /// a resumed session, which has no handshake).
    #[must_use]
    pub fn pairing_code(&self) -> &str {
        &self.pairing_code
    }

    /// This handshake's transcript, for PIN pairing.
    ///
    /// **Not a secret** — every byte of it crossed the wire in the clear. Its
    /// value is being unique to this handshake, so a PIN proof over it cannot
    /// be replayed onto another connection, which is exactly what a machine in
    /// the middle would need to do. Empty for a resumed session: there is no
    /// handshake to bind to, and a proof against nothing proves nothing.
    #[must_use]
    pub fn transcript(&self) -> &[u8] {
        &self.transcript
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

    /// The negotiated capabilities.
    #[must_use]
    pub fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    /// Number of live data channels.
    #[must_use]
    pub fn channel_count(&self) -> usize {
        self.manager.channel_count()
    }

    /// The keepalive action due at `now` (read-only).
    #[must_use]
    pub fn poll_keepalive(&self, now: Instant) -> KeepaliveAction {
        self.keepalive.due(now)
    }

    /// The current crypto epoch (reconnect generation, M6).
    #[must_use]
    pub fn crypto_epoch(&self) -> u64 {
        self.manager.crypto_epoch()
    }

    /// Snapshot the state needed to resume this session after a transport loss:
    /// the master secret (for the resume key + re-keying), identity, negotiated
    /// version + capabilities, and the open message channels to re-attach.
    fn preserve(&self) -> PreservedSession {
        let crypto = self.manager.crypto_clone();
        let epoch = crypto.epoch();
        PreservedSession {
            session_id: self.id,
            local: self.local.clone(),
            peer: self.peer.clone(),
            role: self.role,
            version: self.version,
            capabilities: self.capabilities.clone(),
            crypto,
            consumed_epoch: self.consumed_epoch,
            channels: self.manager.reattachable_channels(),
            stats: RecoveryStats {
                attempts: 0,
                resumes: 0,
                epoch,
            },
        }
    }

    /// Capture the lost session for recovery: snapshot its state, tear down the
    /// now-dead channel actors (their streams died with the transport), and mark
    /// the session `Recovering`.
    fn capture_loss(&mut self) -> RunExit {
        let preserved = self.preserve();
        self.manager.shutdown_all();
        if self.state.can_transition_to(SessionState::Recovering) {
            self.state = SessionState::Recovering;
            // Reflect the recovering state in the registry so diagnostics
            // (recovery_json / transport_json) observe it; assemble() re-registers
            // Active on a successful resume, and finish_close() removes on give-up.
            if let Some(reg) = &self.registry {
                reg.set_state(self.id, SessionState::Recovering);
            }
        }
        RunExit::Lost(Box::new(preserved))
    }

    /// Resume a lost session over a fresh `transport` (the redialling side).
    ///
    /// Mints a single-use [`ResumeToken`] for `epoch`, runs the resume handshake
    /// on a new control stream, re-keys every channel to `epoch` (fresh keys, no
    /// nonce reuse), and re-attaches the preserved message channels — all without
    /// repeating the authenticated handshake. The authenticated identity, session
    /// id, and negotiated version are preserved from `preserved`.
    #[allow(clippy::too_many_arguments)]
    pub async fn resume(
        transport: Arc<dyn ChannelTransport>,
        preserved: &PreservedSession,
        epoch: u64,
        config: &SessionConfig,
        token_ttl_ms: u64,
        wiring: SessionWiring,
    ) -> Result<PeerSession, SessionError> {
        let now = Instant::now();
        let enc = preserved.crypto.enc();
        let mut control = transport.open_stream().await?;

        // Rebind crypto to the new epoch: fresh keys for every channel.
        let session_crypto = preserved.crypto.with_epoch(epoch);
        let mut control_crypto = session_crypto.control()?;

        // Mint + send the resume token (plaintext — the token is self-authenticating
        // via its MAC; the QUIC/TLS transport still encrypts it in transit).
        let resume_key = session_crypto.resume_key()?;
        let binding = ResumeBinding {
            session_id: preserved.session_id,
            local: preserved.local.clone(),
            peer: preserved.peer.clone(),
            version: preserved.version,
        };
        let token = ResumeToken::mint(&resume_key, &binding, epoch, now_ms(), token_ttl_ms)?;
        send_control_plain(control.as_mut(), &ControlMessage::ResumeRequest(token)).await?;

        // Await the reply: accepted → sealed under the epoch control key (proves
        // the peer holds the master); rejected → plaintext with a reason.
        match recv_resume_reply(control.as_mut(), &mut control_crypto, &*enc).await? {
            ResumeReply::Accepted => {}
            ResumeReply::Rejected(reason) => return Err(SessionError::ResumeRejected(reason)),
        }

        let mut session = Self::assemble(
            transport,
            preserved.role,
            preserved.session_id,
            preserved.local.clone(),
            preserved.peer.clone(),
            preserved.version,
            preserved.capabilities.clone(),
            session_crypto,
            control,
            control_crypto,
            enc,
            config,
            wiring,
            preserved.consumed_epoch,
            now,
        );
        session.reattach_channels(&preserved.channels).await?;
        Ok(session)
    }

    /// Accept a peer's resume over a fresh `transport` (the accepting side).
    ///
    /// Validates the resume token against the preserved binding and the single-use
    /// replay guard, re-keys to the token's epoch, and confirms with a sealed
    /// `ResumeAck`. Returns the resumed session and the epoch now consumed (so the
    /// caller updates its replay guard). A rejected token yields a typed error and
    /// a plaintext refusal to the peer (fail-closed, I11).
    pub async fn accept_resume(
        transport: Arc<dyn ChannelTransport>,
        preserved: &PreservedSession,
        config: &SessionConfig,
        wiring: SessionWiring,
    ) -> Result<(PeerSession, u64), SessionError> {
        let now = Instant::now();
        let enc = preserved.crypto.enc();
        let mut control = transport.accept_stream().await?.ok_or_else(|| {
            SessionError::Link("transport closed before resume control stream".into())
        })?;

        let token = match recv_control_plain(control.as_mut()).await? {
            ControlMessage::ResumeRequest(token) => token,
            other => {
                return Err(SessionError::UnexpectedMessage {
                    state: SessionState::Recovering,
                    detail: format!("expected ResumeRequest, got {other:?}"),
                })
            }
        };

        let resume_key = preserved.crypto.resume_key()?;
        let binding = ResumeBinding {
            session_id: preserved.session_id,
            local: preserved.local.clone(),
            peer: preserved.peer.clone(),
            version: preserved.version,
        };
        if let Err(e) = token.verify(&resume_key, &binding, now_ms(), preserved.consumed_epoch) {
            let _ = send_control_plain(
                control.as_mut(),
                &ControlMessage::ResumeAck {
                    accepted: false,
                    reason: e.to_string(),
                },
            )
            .await;
            return Err(e);
        }
        let epoch = token.epoch;

        let session_crypto = preserved.crypto.with_epoch(epoch);
        let mut control_crypto = session_crypto.control()?;
        // Sealed accept: only a holder of the master can produce this frame.
        send_control(
            control.as_mut(),
            &mut control_crypto,
            &*enc,
            &ControlMessage::ResumeAck {
                accepted: true,
                reason: String::new(),
            },
        )
        .await?;

        let session = Self::assemble(
            transport,
            preserved.role,
            preserved.session_id,
            preserved.local.clone(),
            preserved.peer.clone(),
            preserved.version,
            preserved.capabilities.clone(),
            session_crypto,
            control,
            control_crypto,
            enc,
            config,
            wiring,
            epoch,
            now,
        );
        // Re-opened channels arrive as ordinary ChannelOpens on the pump.
        Ok((session, epoch))
    }

    /// Re-open preserved message channels with their original ids under the new
    /// epoch keys, queueing each `ChannelOpen` for the pump to flush.
    async fn reattach_channels(
        &mut self,
        channels: &[(ChannelId, ChannelType)],
    ) -> Result<(), SessionError> {
        for &(id, channel_type) in channels {
            let msg = self.manager.reopen_channel(id, channel_type).await?;
            self.control_out.push_back(msg);
        }
        Ok(())
    }

    /// Run the session pump until it closes or its transport is lost. Returns
    /// [`RunExit::Closed`] on a graceful/fatal close, or [`RunExit::Lost`] with the
    /// state to resume from when the transport drops while `Active`.
    pub async fn run(&mut self) -> Result<RunExit, SessionError> {
        // Fixed-cadence keepalive timer, created ONCE so other pump wakes never
        // reset it. A fresh per-iteration sleep would be starved by any
        // sub-interval local command/actor traffic, so a peer that goes silent
        // while this side stays busy would never be pinged or timed out. The
        // scheduler's on_activity (peer frames only) still decides whether a given
        // tick actually pings/closes; the interval only guarantees it is consulted.
        let mut keepalive_tick = tokio::time::interval(self.keepalive.interval());
        keepalive_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            while let Some(msg) = self.control_out.pop_front() {
                if send_control(
                    self.control.as_mut(),
                    &mut self.control_crypto,
                    &*self.enc,
                    &msg,
                )
                .await
                .is_err()
                {
                    // Control write failed → the transport is gone. Recoverable.
                    return Ok(self.capture_loss());
                }
            }
            if self.state.is_terminal() {
                return Ok(RunExit::Closed);
            }

            let transport = self.manager.transport();
            let accepting = self.accepting;
            let enc = self.enc.clone();
            let wake = {
                let Self {
                    control,
                    control_crypto,
                    actor_events_rx,
                    commands_rx,
                    ..
                } = &mut *self;
                tokio::select! {
                    r = recv_session_frame(control.as_mut(), control_crypto, &*enc) => Wake::Control(r),
                    a = transport.accept_stream(), if accepting => Wake::Accept(a.map_err(SessionError::from)),
                    e = actor_events_rx.recv() => Wake::Actor(e),
                    c = commands_rx.recv() => Wake::Command(c),
                    _ = keepalive_tick.tick() => Wake::Tick,
                }
            };

            match wake {
                // A control-link failure or clean EOF from the peer's transport is
                // a recoverable loss, not a session close: hand back the state to
                // resume from.
                Wake::Control(Err(_)) | Wake::Control(Ok(None)) => {
                    return Ok(self.capture_loss());
                }
                Wake::Control(Ok(Some(frame))) => {
                    self.keepalive.on_activity(Instant::now());
                    if self.route_control(&frame) == Flow::Closed {
                        return Ok(RunExit::Closed);
                    }
                }
                Wake::Accept(Ok(Some(link))) => {
                    let out = self.manager.on_stream_accepted(link);
                    self.control_out.extend(out);
                }
                Wake::Accept(_) => self.accepting = false,
                Wake::Actor(Some(event)) => {
                    let out = self.manager.on_actor_event(event);
                    self.control_out.extend(out);
                }
                Wake::Actor(None) => {}
                Wake::Command(Some(cmd)) => {
                    if self.handle_command(cmd).await == Flow::Closed {
                        return Ok(RunExit::Closed);
                    }
                }
                Wake::Command(None) => {}
                Wake::Tick => match self.keepalive.due(Instant::now()) {
                    KeepaliveAction::Idle => {}
                    KeepaliveAction::SendPing => {
                        self.ping_nonce = self.ping_nonce.wrapping_add(1);
                        self.control_out
                            .push_back(ControlMessage::Ping(self.ping_nonce));
                        self.keepalive.on_ping_sent(Instant::now());
                    }
                    // The idle window elapsed with no peer response — the peer is
                    // unresponsive even though the transport may still look alive
                    // (e.g. its app pump stalled while QUIC keep-alive holds the
                    // connection up). Close cleanly with Timeout rather than hang.
                    KeepaliveAction::Timeout => {
                        self.mark_closed(CloseReason::Timeout);
                        return Ok(RunExit::Closed);
                    }
                },
            }
        }
    }

    fn route_control(&mut self, frame: &SessionFrame) -> Flow {
        let msg = match ControlMessage::from_frame(frame) {
            Ok(msg) => msg,
            Err(_) => {
                if !frame.flags.is_optional() {
                    self.control_out
                        .push_back(ControlMessage::Unsupported(frame.message_type.get()));
                }
                return Flow::Continue;
            }
        };
        match msg {
            // Resume messages are exchanged out-of-band on a freshly-dialled
            // control stream during recovery (see `resume`/`accept_resume`), never
            // on the live pump — ignore a stray one rather than tear down.
            ControlMessage::Hello(_)
            | ControlMessage::Pong(_)
            | ControlMessage::Unsupported(_)
            | ControlMessage::ResumeRequest(_)
            | ControlMessage::ResumeAck { .. } => {}
            ControlMessage::Ping(nonce) => {
                self.emit(SessionEvent::PingReceived {
                    session_id: self.id,
                });
                self.control_out.push_back(ControlMessage::Pong(nonce));
            }
            ControlMessage::Shutdown(reason) => {
                self.mark_closed(CloseReason::Peer(reason));
                return Flow::Closed;
            }
            ControlMessage::ProtocolError(detail) => {
                self.mark_closed(CloseReason::Error(detail));
                return Flow::Closed;
            }
            ControlMessage::ChannelOpen {
                channel,
                channel_type,
            } => {
                let out = self.manager.handle_channel_open(channel, channel_type);
                self.control_out.extend(out);
            }
            ControlMessage::ChannelAccept { channel } => {
                self.manager.handle_channel_accept(channel)
            }
            ControlMessage::ChannelReject { channel, reason } => {
                self.manager.handle_channel_reject(channel, reason);
            }
            ControlMessage::ChannelClose { channel } => {
                let out = self.manager.handle_channel_close(channel);
                self.control_out.extend(out);
            }
            ControlMessage::ChannelClosed { channel } => {
                self.manager.handle_channel_closed(channel);
            }
            ControlMessage::ChannelError { channel, detail } => {
                self.manager.handle_channel_error(channel, detail);
            }
        }
        Flow::Continue
    }

    async fn handle_command(&mut self, cmd: SessionCommand) -> Flow {
        match cmd {
            SessionCommand::OpenChannel {
                channel_type,
                reply,
            } => {
                match self.manager.open_channel(channel_type).await {
                    Ok((id, msg)) => {
                        self.control_out.push_back(msg);
                        let _ = reply.send(Ok(id));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
                Flow::Continue
            }
            SessionCommand::OpenStreamChannel {
                channel_type,
                reply,
            } => {
                match self.manager.open_stream_channel(channel_type).await {
                    Ok((id, link, msg)) => {
                        self.control_out.push_back(msg);
                        let _ = reply.send(Ok((id, link)));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
                Flow::Continue
            }
            SessionCommand::SendOnChannel {
                channel,
                message_type,
                flags,
                payload,
                reply,
            } => {
                let _ =
                    reply.send(
                        self.manager
                            .send_on_channel(channel, message_type, flags, payload),
                    );
                Flow::Continue
            }
            SessionCommand::CloseChannel { channel } => {
                if let Some(msg) = self.manager.close_channel(channel) {
                    self.control_out.push_back(msg);
                }
                Flow::Continue
            }
            SessionCommand::Snapshot { reply } => {
                let _ = reply.send(self.manager.snapshot());
                Flow::Continue
            }
            SessionCommand::Ping => {
                self.ping_nonce = self.ping_nonce.wrapping_add(1);
                self.control_out
                    .push_back(ControlMessage::Ping(self.ping_nonce));
                Flow::Continue
            }
            SessionCommand::Close => {
                self.close_gracefully().await;
                Flow::Closed
            }
        }
    }

    async fn close_gracefully(&mut self) {
        if self.state.is_terminal() {
            return;
        }
        if self.state.is_active() {
            self.state = SessionState::ShuttingDown;
            if let Some(reg) = &self.registry {
                reg.set_state(self.id, SessionState::ShuttingDown);
            }
        }
        let _ = send_control(
            self.control.as_mut(),
            &mut self.control_crypto,
            &*self.enc,
            &ControlMessage::Shutdown("local close".into()),
        )
        .await;
        self.manager.shutdown_all();
        // Gracefully close the control stream — which shares the one QUIC
        // connection — so the Shutdown frame above is delivered before the
        // connection tears down. An abrupt transport close here would drop the
        // buffered Shutdown, making the peer misread this clean close as a
        // recoverable transport loss (and burn its whole reconnect budget).
        let _ = self.control.graceful_close().await;
        self.state = SessionState::Closed;
        self.finish_close(CloseReason::Local);
    }

    fn mark_closed(&mut self, reason: CloseReason) {
        if self.state.is_terminal() {
            return;
        }
        self.state = SessionState::Closed;
        self.manager.shutdown_all();
        self.finish_close(reason);
    }

    fn finish_close(&mut self, reason: CloseReason) {
        self.accepting = false;
        if let Some(reg) = &self.registry {
            reg.remove(self.id);
        }
        self.emit(SessionEvent::Closed {
            session_id: self.id,
            reason,
        });
    }

    fn emit(&self, event: SessionEvent) {
        let _ = self.events.send(event);
    }
}

/// The outcome of the resume handshake as seen by the redialling side.
enum ResumeReply {
    /// The peer accepted (and proved it holds the master via a sealed ack).
    Accepted,
    /// The peer refused, with a reason.
    Rejected(String),
}

/// Current unix time in milliseconds (for resume-token freshness — not a nonce,
/// so wall-clock is appropriate here).
fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Send an **unsealed** control message on a freshly-dialled stream (used only for
/// the resume request/refusal, before the epoch keys are in force on both sides).
async fn send_control_plain(link: &mut dyn Link, msg: &ControlMessage) -> Result<(), SessionError> {
    let frame = msg.to_frame()?;
    link.send_frame(Frame {
        kind: FrameKind::Control,
        payload: frame.encode(),
    })
    .await?;
    Ok(())
}

/// Receive one **unsealed** control message (the resume request).
async fn recv_control_plain(link: &mut dyn Link) -> Result<ControlMessage, SessionError> {
    let frame = link
        .recv_frame()
        .await?
        .ok_or_else(|| SessionError::Link("link closed during resume".into()))?;
    let session_frame = SessionFrame::decode(&frame.payload)?;
    ControlMessage::from_frame(&session_frame)
}

/// Receive the resume reply: an accepted `ResumeAck` is sealed under the epoch
/// control key (so opening it proves the peer holds the master); a refusal is
/// plaintext. Try sealed first, then fall back to plaintext.
async fn recv_resume_reply(
    link: &mut dyn Link,
    crypto: &mut ChannelCrypto,
    enc: &dyn EncryptionProvider,
) -> Result<ResumeReply, SessionError> {
    let frame = link
        .recv_frame()
        .await?
        .ok_or_else(|| SessionError::Link("link closed awaiting resume ack".into()))?;

    // Accepted path: sealed under the epoch control key.
    if let Ok(plain) = crypto.open(enc, &frame.payload) {
        if let Ok(sf) = SessionFrame::decode(&plain) {
            if let Ok(ControlMessage::ResumeAck { accepted, reason }) =
                ControlMessage::from_frame(&sf)
            {
                return Ok(reply_from(accepted, reason));
            }
        }
        return Err(SessionError::ResumeRejected(
            "malformed sealed resume ack".into(),
        ));
    }

    // Refusal path: plaintext.
    let sf = SessionFrame::decode(&frame.payload)?;
    match ControlMessage::from_frame(&sf)? {
        ControlMessage::ResumeAck { accepted, reason } => Ok(reply_from(accepted, reason)),
        other => Err(SessionError::UnexpectedMessage {
            state: SessionState::Recovering,
            detail: format!("expected ResumeAck, got {other:?}"),
        }),
    }
}

fn reply_from(accepted: bool, reason: String) -> ResumeReply {
    if accepted {
        ResumeReply::Accepted
    } else {
        ResumeReply::Rejected(reason)
    }
}

/// Seal a control message with the control channel's key and send it.
async fn send_control(
    link: &mut dyn Link,
    crypto: &mut ChannelCrypto,
    enc: &dyn EncryptionProvider,
    msg: &ControlMessage,
) -> Result<(), SessionError> {
    let session_frame = msg.to_frame()?;
    let sealed = crypto.seal(enc, &session_frame.encode())?;
    link.send_frame(Frame {
        kind: FrameKind::Control,
        payload: Bytes::from(sealed),
    })
    .await?;
    crypto.advance_send()?;
    Ok(())
}

/// Receive one transport frame, open it with the control key, and decode the
/// session frame it carries.
async fn recv_session_frame(
    link: &mut dyn Link,
    crypto: &mut ChannelCrypto,
    enc: &dyn EncryptionProvider,
) -> Result<Option<SessionFrame>, SessionError> {
    match link.recv_frame().await? {
        Some(frame) => {
            let plain = crypto.open(enc, &frame.payload)?;
            Ok(Some(SessionFrame::decode(&plain)?))
        }
        None => Ok(None),
    }
}

/// Receive the peer's opening [`SessionHello`], rejecting anything else.
async fn recv_hello(
    link: &mut dyn Link,
    crypto: &mut ChannelCrypto,
    enc: &dyn EncryptionProvider,
) -> Result<SessionHello, SessionError> {
    match recv_session_frame(link, crypto, enc).await? {
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
