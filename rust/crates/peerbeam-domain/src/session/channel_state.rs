//! The per-channel lifecycle state machine — pure data, no IO.
//!
//! Each data channel moves through these states independently of every other
//! channel and of the session. Failure or closure of one channel touches only
//! its own state (see the session spec's per-channel isolation guarantee).

use serde::{Deserialize, Serialize};

/// A data channel's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelState {
    /// Opened locally; awaiting the peer's accept/reject.
    Opening,
    /// Established; carrying messages.
    Open,
    /// Draining after a local or peer close request.
    Closing,
    /// Fully closed (terminal).
    Closed,
    /// The peer refused the open (terminal).
    Rejected,
    /// Aborted by a channel-scoped error (terminal from the caller's view;
    /// resolves to [`Closed`](ChannelState::Closed)).
    Errored,
}

impl ChannelState {
    /// Whether the channel can carry messages.
    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self, ChannelState::Open)
    }

    /// Whether this is a terminal state.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, ChannelState::Closed | ChannelState::Rejected)
    }

    /// Whether a transition from `self` to `next` is legal.
    #[must_use]
    pub fn can_transition_to(&self, next: ChannelState) -> bool {
        use ChannelState::{Closed, Closing, Errored, Open, Opening, Rejected};
        matches!(
            (self, next),
            (Opening, Open)
                | (Opening, Rejected)
                | (Opening, Closing)
                | (Opening, Closed)
                | (Opening, Errored)
                | (Open, Closing)
                | (Open, Closed)
                | (Open, Errored)
                | (Closing, Closed)
                | (Closing, Errored)
                | (Errored, Closed)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ChannelState::{Closed, Closing, Errored, Open, Opening, Rejected};

    #[test]
    fn open_path_is_legal() {
        assert!(Opening.can_transition_to(Open));
        assert!(Open.can_transition_to(Closing));
        assert!(Closing.can_transition_to(Closed));
    }

    #[test]
    fn reject_and_error_paths_are_legal() {
        assert!(Opening.can_transition_to(Rejected));
        assert!(Open.can_transition_to(Errored));
        assert!(Errored.can_transition_to(Closed));
    }

    #[test]
    fn illegal_transitions_rejected() {
        assert!(!Open.can_transition_to(Opening));
        assert!(!Closed.can_transition_to(Open));
        assert!(!Rejected.can_transition_to(Open));
    }

    #[test]
    fn predicates() {
        assert!(Open.is_open());
        assert!(!Opening.is_open());
        assert!(Closed.is_terminal());
        assert!(Rejected.is_terminal());
        assert!(!Errored.is_terminal());
    }
}
