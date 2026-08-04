//! PeerSession diagnostics — the single, engine-level source of truth for live
//! session, channel, migration, and recovery status (M8).
//!
//! This is the seam frontends (FFI, CLI, daemon) read to *present* PeerSession
//! state without owning any of it and without duplicating engine state. It holds
//! the very same [`SessionRegistry`] a running [`PeerSession`] registers into
//! (M2) and the same [`MigrationMetrics`] the transport selector records into
//! (M7) — nothing is copied or recomputed. Channel snapshots are read live from a
//! session's own handle.
//!
//! [`PeerSession`]: peerbeam_transfer::PeerSession

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use peerbeam_domain::session::{SessionId, SessionState};
use peerbeam_transfer::{
    ChannelInfo, MigrationMetrics, SessionHandle, SessionInfo, SessionRegistry,
};

/// A stable snake_case label for a session state (for JSON output).
fn state_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Connecting => "connecting",
        SessionState::Authenticating => "authenticating",
        SessionState::Negotiating => "negotiating",
        SessionState::Active => "active",
        SessionState::Recovering => "recovering",
        SessionState::ShuttingDown => "shutting_down",
        SessionState::Closed => "closed",
        SessionState::Failed => "failed",
    }
}

/// Parse a 32-char lowercase-hex [`SessionId`] (the `Display` form).
fn parse_session_id(hex: &str) -> Option<SessionId> {
    let hex = hex.trim();
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(SessionId::from_bytes(bytes))
}

/// Read-only diagnostics over the live PeerSession state. Cheaply cloneable
/// (shares the underlying registry, metrics, and handle map).
#[derive(Clone)]
pub struct SessionDiagnostics {
    sessions: SessionRegistry,
    migration: Arc<MigrationMetrics>,
    handles: Arc<Mutex<HashMap<SessionId, SessionHandle>>>,
}

impl Default for SessionDiagnostics {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionDiagnostics {
    /// A fresh diagnostics view with an empty registry and zeroed metrics.
    #[must_use]
    pub fn new() -> Self {
        SessionDiagnostics {
            sessions: SessionRegistry::new(),
            migration: Arc::new(MigrationMetrics::new()),
            handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The session registry to pass into [`peerbeam_transfer::PeerSession::open`]
    /// so live sessions register here automatically.
    #[must_use]
    pub fn registry(&self) -> SessionRegistry {
        self.sessions.clone()
    }

    /// The migration metrics to pass into the M7 transport selector so transfers
    /// record their transport choice here.
    #[must_use]
    pub fn migration(&self) -> Arc<MigrationMetrics> {
        self.migration.clone()
    }

    /// Track a live session's control handle so its channels can be snapshotted.
    pub fn register_handle(&self, id: SessionId, handle: SessionHandle) {
        self.lock().insert(id, handle);
    }

    /// Stop tracking a session's handle (on close).
    pub fn unregister_handle(&self, id: SessionId) {
        self.lock().remove(&id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, SessionHandle>> {
        self.handles.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn session_value(info: &SessionInfo) -> Value {
        json!({
            "id": info.id.to_string(),
            "peer": info.peer.as_str(),
            "state": state_label(info.state),
            "version": format!("{}.{}", info.version.major, info.version.minor),
            "capabilities": info
                .capabilities
                .iter()
                .map(|c| json!({ "channel": format!("{:#06x}", c.channel.get()), "features": c.features }))
                .collect::<Vec<_>>(),
        })
    }

    /// `{ sessions: [...], count }` — every active session.
    #[must_use]
    pub fn sessions_json(&self) -> Value {
        let sessions: Vec<Value> = self
            .sessions
            .list()
            .iter()
            .map(Self::session_value)
            .collect();
        json!({ "count": sessions.len(), "sessions": sessions })
    }

    /// `{ session: {...} | null }` for one session id (its `Display` hex form).
    #[must_use]
    pub fn session_json(&self, id_hex: &str) -> Value {
        let found = parse_session_id(id_hex)
            .and_then(|id| self.sessions.get(id))
            .map(|info| Self::session_value(&info));
        json!({ "session": found })
    }

    /// `{ session, channels: [...] }` — a live channel snapshot for one session,
    /// read from its handle. Empty if the session is unknown or already closed.
    pub async fn channels_json(&self, id_hex: &str) -> Value {
        let handle = parse_session_id(id_hex).and_then(|id| self.lock().get(&id).cloned());
        let channels = match handle {
            Some(h) => h.channels().await.unwrap_or_default(),
            None => Vec::new(),
        };
        let list: Vec<Value> = channels.iter().map(Self::channel_value).collect();
        json!({ "session": id_hex, "count": list.len(), "channels": list })
    }

    /// Channel snapshots for **all** tracked sessions.
    pub async fn all_channels_json(&self) -> Value {
        let ids: Vec<SessionId> = self.lock().keys().copied().collect();
        let mut out = Vec::new();
        for id in ids {
            out.push(self.channels_json(&id.to_string()).await);
        }
        json!({ "sessions": out })
    }

    fn channel_value(c: &ChannelInfo) -> Value {
        json!({
            "id": c.id.get(),
            "channel_type": format!("{:#06x}", c.channel_type.get()),
            "state": format!("{:?}", c.state).to_lowercase(),
            "stats": {
                "frames_sent": c.stats.frames_sent,
                "frames_recv": c.stats.frames_recv,
                "bytes_sent": c.stats.bytes_sent,
                "bytes_recv": c.stats.bytes_recv,
            },
        })
    }

    /// `{ session_transfers, legacy_transfers, fallbacks, fallback_reasons }` — the
    /// migration (cutover) counters.
    #[must_use]
    pub fn migration_json(&self) -> Value {
        let s = self.migration.snapshot();
        json!({
            "session_transfers": s.session_transfers,
            "legacy_transfers": s.legacy_transfers,
            "fallbacks": s.fallbacks,
            "fallback_reasons": {
                "older_peer": s.older_peer,
                "version_mismatch": s.version_mismatch,
                "capability_mismatch": s.capability_mismatch,
                "negotiation_failed": s.negotiation_failed,
                "resume_incompatible": s.resume_incompatible,
                "explicit_compat": s.explicit_compat,
            },
        })
    }

    /// `{ recovering, sessions_recovering: [...] }` — sessions currently in the
    /// `Recovering` state (reconnect + resume in flight, M6).
    #[must_use]
    pub fn recovery_json(&self) -> Value {
        let recovering: Vec<Value> = self
            .sessions
            .list()
            .into_iter()
            .filter(|i| i.state == SessionState::Recovering)
            .map(|i| json!({ "id": i.id.to_string(), "peer": i.peer.as_str() }))
            .collect();
        json!({ "recovering": recovering.len(), "sessions_recovering": recovering })
    }

    /// The full aggregate diagnostics object.
    #[must_use]
    pub fn diagnostics_json(&self) -> Value {
        json!({
            "sessions": self.sessions_json(),
            "migration": self.migration_json(),
            "recovery": self.recovery_json(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::id::DeviceId;
    use peerbeam_domain::session::{Capability, CapabilitySet, ChannelType, Version};
    use peerbeam_transfer::SessionInfo as Info;

    fn info(id: u128, state: SessionState) -> Info {
        Info {
            id: SessionId::from_u128(id),
            peer: DeviceId::from("peer-x"),
            state,
            version: Version::new(1, 0),
            capabilities: CapabilitySet::new().with(Capability::new(ChannelType::TRANSFER)),
        }
    }

    #[test]
    fn empty_snapshots_are_well_formed() {
        let d = SessionDiagnostics::new();
        assert_eq!(d.sessions_json()["count"], 0);
        assert!(d.sessions_json()["sessions"].as_array().unwrap().is_empty());
        assert_eq!(d.migration_json()["session_transfers"], 0);
        assert_eq!(d.recovery_json()["recovering"], 0);
        assert_eq!(d.session_json("deadbeef")["session"], Value::Null);
        assert!(d.diagnostics_json()["sessions"].is_object());
    }

    #[test]
    fn sessions_snapshot_reflects_registry() {
        let d = SessionDiagnostics::new();
        d.registry().register(info(1, SessionState::Active));
        d.registry().register(info(2, SessionState::Recovering));
        let s = d.sessions_json();
        assert_eq!(s["count"], 2);
        // Recovery view isolates the recovering session.
        assert_eq!(d.recovery_json()["recovering"], 1);
        // session_json round-trips the id's hex Display form.
        let id_hex = SessionId::from_u128(1).to_string();
        let one = d.session_json(&id_hex);
        assert_eq!(one["session"]["state"], "active");
        assert_eq!(one["session"]["version"], "1.0");
        assert_eq!(one["session"]["capabilities"][0]["channel"], "0x0100");
    }

    #[test]
    fn migration_snapshot_reflects_metrics() {
        let d = SessionDiagnostics::new();
        // Record via the shared metrics handle (what the selector receives).
        let m = d.migration();
        // Use the public snapshot after simulating a couple of transfers is not
        // possible without the selector; assert the zeroed shape here and rely on
        // the selector integration test for populated values.
        let s = m.snapshot();
        assert_eq!(s.session_transfers, 0);
        assert_eq!(d.migration_json()["fallbacks"], 0);
    }

    #[test]
    fn parse_session_id_roundtrip() {
        let id = SessionId::from_u128(0x1234_5678);
        assert_eq!(parse_session_id(&id.to_string()), Some(id));
        assert_eq!(parse_session_id("nothex"), None);
        assert_eq!(parse_session_id(""), None);
    }
}
