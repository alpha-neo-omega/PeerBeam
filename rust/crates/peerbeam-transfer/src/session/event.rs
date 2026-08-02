//! Session configuration, roles, events, and the keepalive scheduler.

use std::time::{Duration, Instant};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{CapabilitySet, SessionId, Version};

/// Which side of the handshake this endpoint is.
///
/// The initiator mints the [`SessionId`]; the responder adopts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRole {
    /// Opened the session (dialled the peer).
    Initiator,
    /// Accepted the session (was dialled).
    Responder,
}

/// Why a session closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    /// This side closed it.
    Local,
    /// The peer sent `Shutdown` with this reason.
    Peer(String),
    /// The keepalive/idle window elapsed with no response.
    Timeout,
    /// A fatal error aborted the session.
    Error(String),
}

/// Events a session emits to its owner (the engine, later the FFI/CLI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// The session is established and negotiated.
    Established {
        /// The negotiated session id.
        session_id: SessionId,
        /// The authenticated peer.
        peer: DeviceId,
        /// The agreed protocol version.
        version: Version,
        /// The agreed capabilities.
        capabilities: CapabilitySet,
    },
    /// A keepalive ping was received from the peer.
    PingReceived {
        /// The session it arrived on.
        session_id: SessionId,
    },
    /// The session closed.
    Closed {
        /// The session that closed.
        session_id: SessionId,
        /// Why.
        reason: CloseReason,
    },
}

/// Tuning for keepalive and idle-timeout behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepaliveConfig {
    /// How long of no activity before a `Ping` is sent.
    pub interval: Duration,
    /// How long of no activity before the peer is declared dead.
    pub idle_timeout: Duration,
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        KeepaliveConfig {
            interval: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(60),
        }
    }
}

/// What the keepalive scheduler wants done at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepaliveAction {
    /// Nothing to do yet.
    Idle,
    /// Send a keepalive `Ping`.
    SendPing,
    /// The idle window elapsed; treat the peer as gone.
    Timeout,
}

/// Pure keepalive scheduler. The session runtime feeds it activity timestamps and
/// asks [`due`](Keepalive::due) what to do; keeping the decision pure makes it
/// deterministically testable without real timers.
#[derive(Debug, Clone)]
pub struct Keepalive {
    config: KeepaliveConfig,
    last_activity: Instant,
    last_ping: Option<Instant>,
}

impl Keepalive {
    /// Start the scheduler; `now` seeds the activity clock.
    #[must_use]
    pub fn new(config: KeepaliveConfig, now: Instant) -> Self {
        Keepalive {
            config,
            last_activity: now,
            last_ping: None,
        }
    }

    /// Record inbound activity (any frame from the peer), resetting the timers.
    pub fn on_activity(&mut self, now: Instant) {
        self.last_activity = now;
        self.last_ping = None;
    }

    /// Record that a `Ping` was just sent, so another is not sent every tick.
    pub fn on_ping_sent(&mut self, now: Instant) {
        self.last_ping = Some(now);
    }

    /// Decide what to do at `now`.
    #[must_use]
    pub fn due(&self, now: Instant) -> KeepaliveAction {
        let idle = now.saturating_duration_since(self.last_activity);
        if idle >= self.config.idle_timeout {
            return KeepaliveAction::Timeout;
        }
        if idle >= self.config.interval {
            match self.last_ping {
                Some(sent) if now.saturating_duration_since(sent) < self.config.interval => {
                    KeepaliveAction::Idle
                }
                _ => KeepaliveAction::SendPing,
            }
        } else {
            KeepaliveAction::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> KeepaliveConfig {
        KeepaliveConfig {
            interval: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn idle_before_interval() {
        let t0 = Instant::now();
        let k = Keepalive::new(cfg(), t0);
        assert_eq!(k.due(t0 + Duration::from_secs(5)), KeepaliveAction::Idle);
    }

    #[test]
    fn sends_ping_after_interval() {
        let t0 = Instant::now();
        let k = Keepalive::new(cfg(), t0);
        assert_eq!(
            k.due(t0 + Duration::from_secs(12)),
            KeepaliveAction::SendPing
        );
    }

    #[test]
    fn does_not_spam_pings_within_interval() {
        let t0 = Instant::now();
        let mut k = Keepalive::new(cfg(), t0);
        let t = t0 + Duration::from_secs(12);
        assert_eq!(k.due(t), KeepaliveAction::SendPing);
        k.on_ping_sent(t);
        // A moment later, still within the interval since the ping: stay idle.
        assert_eq!(k.due(t + Duration::from_secs(3)), KeepaliveAction::Idle);
    }

    #[test]
    fn times_out_after_idle_window() {
        let t0 = Instant::now();
        let k = Keepalive::new(cfg(), t0);
        assert_eq!(
            k.due(t0 + Duration::from_secs(31)),
            KeepaliveAction::Timeout
        );
    }

    #[test]
    fn activity_resets_timers() {
        let t0 = Instant::now();
        let mut k = Keepalive::new(cfg(), t0);
        let t = t0 + Duration::from_secs(25);
        k.on_activity(t);
        assert_eq!(k.due(t + Duration::from_secs(5)), KeepaliveAction::Idle);
    }
}
