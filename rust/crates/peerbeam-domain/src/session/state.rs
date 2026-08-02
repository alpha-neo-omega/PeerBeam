//! Session lifecycle states and the legal transitions between them.
//!
//! The state set and edges are pure data; the session layer drives them. Reconnect
//! and resume states are intentionally absent here — they arrive in a later
//! milestone and would be dead variants today.

use serde::{Deserialize, Serialize};

/// A session's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// Establishing the transport connection.
    Connecting,
    /// Running the mutual-authentication handshake.
    Authenticating,
    /// Exchanging version and capabilities over the control channel.
    Negotiating,
    /// Established; channels may be used.
    Active,
    /// Draining: no new channels, existing ones finishing.
    ShuttingDown,
    /// Fully closed (terminal).
    Closed,
    /// Aborted by a fatal error (terminal from the caller's view; resolves to
    /// [`Closed`](SessionState::Closed)).
    Failed,
}

impl SessionState {
    /// Whether the session can serve channel traffic.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self, SessionState::Active)
    }

    /// Whether this is a terminal state (no further transitions).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, SessionState::Closed)
    }

    /// Whether a transition from `self` to `next` is legal. Every transition the
    /// session layer performs is checked against this, so an illegal transition
    /// is a typed error rather than a silent corruption.
    #[must_use]
    pub fn can_transition_to(&self, next: SessionState) -> bool {
        use SessionState::{
            Active, Authenticating, Closed, Connecting, Failed, Negotiating, ShuttingDown,
        };
        matches!(
            (self, next),
            (Connecting, Authenticating)
                | (Connecting, Failed)
                | (Connecting, Closed)
                | (Authenticating, Negotiating)
                | (Authenticating, Failed)
                | (Authenticating, Closed)
                | (Negotiating, Active)
                | (Negotiating, Failed)
                | (Negotiating, Closed)
                | (Active, ShuttingDown)
                | (Active, Closed)
                | (Active, Failed)
                | (ShuttingDown, Closed)
                | (Failed, Closed)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::SessionState::{
        Active, Authenticating, Closed, Connecting, Failed, Negotiating, ShuttingDown,
    };

    #[test]
    fn happy_path_transitions_are_legal() {
        assert!(Connecting.can_transition_to(Authenticating));
        assert!(Authenticating.can_transition_to(Negotiating));
        assert!(Negotiating.can_transition_to(Active));
        assert!(Active.can_transition_to(ShuttingDown));
        assert!(ShuttingDown.can_transition_to(Closed));
    }

    #[test]
    fn failure_and_close_transitions_are_legal() {
        assert!(Negotiating.can_transition_to(Failed));
        assert!(Active.can_transition_to(Closed));
        assert!(Failed.can_transition_to(Closed));
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        assert!(!Connecting.can_transition_to(Active)); // must authenticate + negotiate
        assert!(!Active.can_transition_to(Negotiating)); // no going back
        assert!(!Closed.can_transition_to(Active)); // terminal
        assert!(!ShuttingDown.can_transition_to(Active));
    }

    #[test]
    fn active_and_terminal_predicates() {
        assert!(Active.is_active());
        assert!(!Negotiating.is_active());
        assert!(Closed.is_terminal());
        assert!(!Failed.is_terminal()); // Failed still resolves to Closed
    }
}
