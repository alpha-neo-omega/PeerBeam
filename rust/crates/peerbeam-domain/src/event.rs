//! Domain events — the outbound notification surface.
//!
//! The engine publishes these on a broadcast channel; every frontend
//! (Flutter, CLI, daemon) renders the same stream. Frontends never poll
//! internal state — they react to events.

use crate::entity::{ClipboardItem, Device, Progress, Route, TransferSession};
use crate::id::{DeviceId, TransferId};

/// A single event emitted by the engine.
#[derive(Debug, Clone)]
pub enum DomainEvent {
    /// A new peer became visible (from any discovery provider, deduped).
    PeerFound(Device),
    /// A known peer's details changed.
    PeerUpdated(Device),
    /// A peer is no longer visible.
    PeerLost(DeviceId),
    /// A route was chosen for a transfer.
    RouteSelected {
        /// The transfer the route was chosen for.
        transfer: TransferId,
        /// The selected route.
        route: Route,
    },
    /// An incoming transfer needs user approval.
    TransferRequested(TransferSession),
    /// Progress update for an active transfer.
    Progress(Progress),
    /// A transfer finished successfully.
    TransferCompleted(TransferId),
    /// A transfer failed.
    TransferFailed {
        /// The affected transfer.
        transfer: TransferId,
        /// Human-readable reason.
        reason: String,
    },
    /// A peer shared clipboard content.
    ClipboardUpdated(ClipboardItem),
    /// A non-fatal engine error worth surfacing.
    Error(String),
}
