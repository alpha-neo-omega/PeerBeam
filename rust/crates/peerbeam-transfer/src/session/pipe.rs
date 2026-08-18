//! The pipe as a PeerSession channel — one stream channel, one direction of
//! bytes, then closed.
//!
//! Same mechanism as [`super::transfer`] and for the same reason (I2): the
//! sender opens a stream channel ([`SessionHandle::open_stream_channel`]) and
//! runs [`send_pipe`] over the sealed link; the receiver takes the matching
//! [`IncomingStreamChannel`] off the session's incoming-streams receiver and
//! runs [`receive_pipe`] over it. No second socket, no second dial path, no
//! second chunk loop. The session pump is never in the data path, so a pipe that
//! fails, is refused, or runs for an hour cannot stall the session or any
//! sibling channel.
//!
//! [`SessionHandle::open_stream_channel`]: super::SessionHandle::open_stream_channel

use futures::io::{AsyncRead, AsyncWrite};

use peerbeam_domain::entity::Permission;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;
use peerbeam_domain::session::{CapabilitySet, ChannelType, SessionError};

use super::channel::IncomingStreamChannel;
use super::SessionHandle;
use crate::pipe::{may_accept_pipe, receive_pipe, send_pipe, PipeStats};

fn sess_to_dom(e: SessionError) -> DomainError {
    DomainError::Connection(e.to_string())
}

/// What one process's consent to an inbound pipe consists of — the four legs of
/// [`may_accept_pipe`] minus the peer, bundled so that **every** accept site in
/// the codebase passes the same shape and states its `listening` answer out
/// loud at the call site.
///
/// The fields are public and constructed literally rather than through
/// constructors, deliberately: `listening: false` written into `serve_loop`'s
/// own source is a stronger statement about what a daemon does than a
/// `Consent::refusing()` a reader has to go and look up.
pub struct PipeConsent<'a> {
    /// Whether **this process** is a `peerbeam pipe --listen`.
    ///
    /// `false` for `receive`, `daemon start`, `chat watch` and the Flutter GUI —
    /// all of which advertise the Pipe capability and refuse every pipe offered
    /// to them. See [`may_accept_pipe`] leg 1.
    pub listening: bool,
    /// The trust store, asked per pipe rather than cached, so revoking a device
    /// refuses the next pipe rather than the next reconnect.
    pub trust: &'a dyn TrustStore,
    /// `pipe --listen --from <device>`, already resolved to an **authenticated**
    /// device id. `None` accepts any trusted peer.
    pub only_from: Option<&'a DeviceId>,
    /// The session's **negotiated** (intersected) capability set.
    pub negotiated: &'a CapabilitySet,
}

impl PipeConsent<'_> {
    /// The gate, asked. The only path from an inbound channel to `out`; see
    /// [`accept_pipe`].
    #[must_use]
    pub fn permits(&self, peer: &DeviceId) -> bool {
        may_accept_pipe(
            self.listening,
            self.trust,
            peer,
            self.only_from,
            self.negotiated,
        )
    }
}

/// Open a pipe channel on `session` and stream `src` to the peer over it.
///
/// The channel is closed (best-effort) when the stream ends, whatever the
/// outcome. The caller is expected to have checked
/// [`caps_support_stream`](crate::pipe::caps_support_stream) against the
/// negotiated set **first**, so a peer that cannot receive a pipe is refused
/// before a byte of stdin is consumed rather than after.
pub async fn send_pipe_on_session(
    session: &SessionHandle,
    src: &mut (dyn AsyncRead + Unpin + Send),
    chunk_size: u32,
) -> Result<PipeStats> {
    let (channel, mut link) = session
        .open_stream_channel(ChannelType::PIPE)
        .await
        .map_err(sess_to_dom)?;
    let out = send_pipe(link.as_mut(), src, chunk_size).await;
    session.close_channel(channel);
    out
}

/// Handle an inbound stream channel that claims to be a pipe: write it to `out`,
/// or refuse it.
///
/// **Every process that can receive an inbound pipe funnels through here** — the
/// one-shot `pipe --listen` with `listening: true`, and `receive`/`daemon`/
/// `chat watch`/the GUI with `listening: false` and an `out` that goes nowhere.
/// That is the point: the listen gate is then a single decision in a single
/// function that production really does execute on both paths, rather than an
/// unreachable branch a mutation test could satisfy vacuously.
///
/// Nothing is read from the link and nothing is written to `out` unless
/// [`PipeConsent::permits`] says so. The refusal closes the channel, which the
/// sender observes as its next write failing — the reason cannot travel on the
/// wire (a channel close carries none), and deliberately so: "you are not
/// running `pipe --listen`" and "I have revoked your device" are the same
/// refusal to a peer, and telling a stranger which one applies is free
/// reconnaissance.
///
/// `channel_type` is checked too. It should always be
/// [`ChannelType::PIPE`] — the session only routes a channel here when the
/// caller dispatched on that type — but a stream channel arrives carrying its
/// own claim about what it is, and treating a mislabelled one as a pipe would
/// write a file transfer's `Meta` frame to somebody's terminal.
pub async fn accept_pipe(
    incoming: IncomingStreamChannel,
    session: &SessionHandle,
    peer: &DeviceId,
    consent: &PipeConsent<'_>,
    out: &mut (dyn AsyncWrite + Unpin + Send),
) -> Result<PipeStats> {
    let IncomingStreamChannel {
        channel,
        channel_type,
        mut link,
    } = incoming;
    if channel_type != ChannelType::PIPE {
        session.close_channel(channel);
        return Err(DomainError::Connection(format!(
            "refused channel {:#06x} offered as a pipe by {}",
            channel_type.get(),
            peer.0
        )));
    }
    if !consent.permits(peer) {
        session.close_channel(channel);
        return Err(DomainError::Connection(refusal(consent, peer)));
    }
    let stats = receive_pipe(link.as_mut(), out).await;
    session.close_channel(channel);
    stats
}

/// The refusal, worded for **this** side's operator or log — never for the peer,
/// which only ever sees a closed channel.
///
/// Naming the leg that shut is what makes the refusal actionable: a user piping
/// into a machine running `peerbeam daemon start` needs to be told to run
/// `pipe --listen` there, and one whose `--from` did not match needs to be told
/// which device actually connected.
fn refusal(consent: &PipeConsent<'_>, peer: &DeviceId) -> String {
    if !consent.listening {
        return format!(
            "refused an inbound pipe from {}: this process is not `peerbeam pipe --listen`. \
             A pipe writes raw bytes to stdout, so only a process started for exactly that \
             accepts one — a running receive/daemon/serve never does",
            peer.0
        );
    }
    if !consent.trust.is_approved(peer) {
        return format!(
            "refused an inbound pipe from {}: this device is not approved. Its key may \
             well be pinned — the handshake pins every peer it sees, which records no \
             decision — so run `peerbeam trust approve {}` here to grant it",
            peer.0, peer.0
        );
    }
    if !consent.trust.may(peer, Permission::Pipe) {
        return format!(
            "refused an inbound pipe from {}: this device is approved but its `pipe` \
             permission was revoked. Run `peerbeam trust permit {} pipe` here to restore it",
            peer.0, peer.0
        );
    }
    match consent.only_from {
        Some(want) if want != peer => format!(
            "refused an inbound pipe from {}: --from restricts this listener to {}",
            peer.0, want.0
        ),
        _ => format!(
            "refused an inbound pipe from {}: it did not negotiate the pipe stream capability",
            peer.0
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::entity::PermissionSet;
    use peerbeam_domain::session::{Capability, PIPE_FEAT_STREAM};

    /// A store in one of the states a real one can be in for a given peer,
    /// because the pipe gate asks `is_approved` and `may`, and the refusal
    /// message has to tell "pinned but never chosen" from "never seen at all"
    /// and from "approved, but its pipe permission was revoked".
    struct FakeTrust {
        approved: bool,
        pinned: bool,
        permissions: PermissionSet,
    }

    /// The user chose this device.
    fn approving() -> FakeTrust {
        FakeTrust {
            approved: true,
            pinned: true,
            permissions: PermissionSet::granted_on_approval(),
        }
    }

    /// Chosen, then narrowed: the user revoked its `pipe` permission.
    fn approving_without_pipe() -> FakeTrust {
        FakeTrust {
            permissions: PermissionSet::granted_on_approval().set(Permission::Pipe, false),
            ..approving()
        }
    }

    /// Its key was recorded by the handshake and nobody decided anything — what
    /// a stranger looks like the instant after connecting.
    fn pinned_only() -> FakeTrust {
        FakeTrust {
            approved: false,
            pinned: true,
            permissions: PermissionSet::granted_on_approval(),
        }
    }

    /// No record at all.
    fn unknown() -> FakeTrust {
        FakeTrust {
            approved: false,
            pinned: false,
            permissions: PermissionSet::granted_on_approval(),
        }
    }

    impl TrustStore for FakeTrust {
        fn record(
            &self,
            _r: peerbeam_domain::entity::TrustRecord,
        ) -> peerbeam_domain::error::Result<()> {
            Ok(())
        }
        fn lookup(
            &self,
            d: &DeviceId,
        ) -> peerbeam_domain::error::Result<Option<peerbeam_domain::entity::TrustRecord>> {
            if !self.pinned {
                return Ok(None);
            }
            Ok(Some(peerbeam_domain::entity::TrustRecord {
                device: d.clone(),
                fingerprint: "ff".into(),
                name: "Peer".into(),
                trusted_at: chrono::Utc::now(),
                approved: self.approved,
                permissions: if self.approved {
                    self.permissions
                } else {
                    PermissionSet::none()
                },
            }))
        }
        fn is_trusted(&self, _d: &DeviceId) -> bool {
            self.pinned
        }
    }

    fn caps() -> CapabilitySet {
        CapabilitySet::new().with(Capability::with_features(
            ChannelType::PIPE,
            PIPE_FEAT_STREAM,
        ))
    }

    fn bob() -> DeviceId {
        DeviceId::from("pb-bob")
    }

    /// Each refusal must name the leg that actually shut, or an operator cannot
    /// tell "run `pipe --listen` over there" from "trust that device first".
    #[test]
    fn the_refusal_names_the_leg_that_shut() {
        let trust = approving();
        let caps = caps();
        let not_listening = PipeConsent {
            listening: false,
            trust: &trust,
            only_from: None,
            negotiated: &caps,
        };
        assert!(refusal(&not_listening, &bob()).contains("pipe --listen"));

        let untrusting = unknown();
        let untrusted = PipeConsent {
            listening: true,
            trust: &untrusting,
            only_from: None,
            negotiated: &caps,
        };
        assert!(refusal(&untrusted, &bob()).contains("not approved"));

        // **A pinned-but-unapproved peer must be told to approve it.** The
        // refusal walks the legs in order, so a leg asking a different question
        // from the gate falls through to the next one and blames it: before this
        // asked `is_approved`, a peer the handshake had just pinned was refused
        // by the gate and then told its build "did not negotiate the pipe stream
        // capability" — an operator acting on that would go looking for a
        // version mismatch that does not exist.
        let pinned = pinned_only();
        let unapproved = PipeConsent {
            listening: true,
            trust: &pinned,
            only_from: None,
            negotiated: &caps,
        };
        assert!(
            !unapproved.permits(&bob()),
            "precondition: the gate refuses a merely pinned peer"
        );
        let msg = refusal(&unapproved, &bob());
        assert!(msg.contains("trust approve"), "{msg}");
        assert!(
            !msg.contains("negotiate"),
            "the refusal must not blame the peer's build: {msg}"
        );

        let carol = DeviceId::from("pb-carol");
        let wrong_peer = PipeConsent {
            listening: true,
            trust: &trust,
            only_from: Some(&carol),
            negotiated: &caps,
        };
        let msg = refusal(&wrong_peer, &bob());
        assert!(msg.contains("--from"), "{msg}");
        assert!(msg.contains("pb-carol"), "{msg}");

        let no_caps = CapabilitySet::new();
        let incapable = PipeConsent {
            listening: true,
            trust: &trust,
            only_from: None,
            negotiated: &no_caps,
        };
        assert!(refusal(&incapable, &bob()).contains("negotiate"));
    }

    /// `permits` must be the gate and nothing else — the same four legs, so a
    /// site that holds a `PipeConsent` cannot accidentally consult a weaker
    /// predicate.
    #[test]
    fn permits_delegates_to_the_gate() {
        let trust = approving();
        let caps = caps();
        let listening = PipeConsent {
            listening: true,
            trust: &trust,
            only_from: None,
            negotiated: &caps,
        };
        assert!(listening.permits(&bob()));
        assert_eq!(
            listening.permits(&bob()),
            may_accept_pipe(true, &trust, &bob(), None, &caps)
        );

        let daemon = PipeConsent {
            listening: false,
            ..listening
        };
        assert!(!daemon.permits(&bob()), "a daemon permits nothing");
    }

    /// **The refusal names the leg that shut.** A device the user approved and
    /// then narrowed must not be told it "did not negotiate the pipe stream
    /// capability" — that would send its operator to debug a wire problem that
    /// does not exist. It is told which permission to restore.
    #[test]
    fn a_revoked_pipe_permission_is_refused_by_name_not_as_a_capability_problem() {
        let trust = approving_without_pipe();
        let caps = caps();
        let consent = PipeConsent {
            listening: true,
            trust: &trust,
            only_from: None,
            negotiated: &caps,
        };
        assert!(!consent.permits(&bob()), "the gate is shut");
        let message = refusal(&consent, &bob());
        assert!(
            message.contains("permission was revoked"),
            "the refusal must name the permission leg: {message}"
        );
        assert!(
            message.contains("trust permit"),
            "and say how to restore it: {message}"
        );
        assert!(
            !message.contains("negotiate"),
            "and must not blame the wire: {message}"
        );
    }
}
