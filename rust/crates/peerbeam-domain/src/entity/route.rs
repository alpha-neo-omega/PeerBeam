//! Network route entities used by route selection.

use serde::{Deserialize, Serialize};

/// A class of network path to a peer, ordered by preference.
///
/// The `Ord` derive follows declaration order, so `Lan < Relay`. Route
/// selection prefers the *smallest* kind that is healthy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum RouteKind {
    Lan,
    UsbTether,
    Ethernet,
    Wifi,
    TailscaleDirect,
    DirectInternet,
    Relay,
}

/// A concrete candidate path to a peer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Route {
    /// The class of this route (drives priority).
    pub kind: RouteKind,
    /// Target address for this route.
    pub address: String,
    /// Target port for this route.
    pub port: u16,
}

/// The measured health of a route candidate.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteHealth {
    /// Whether the route responded to a probe.
    pub reachable: bool,
    /// Round-trip time in milliseconds, if measured.
    pub rtt_ms: Option<u32>,
}
