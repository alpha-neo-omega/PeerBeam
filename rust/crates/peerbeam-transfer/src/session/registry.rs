//! Registries: active sessions and capability handlers.
//!
//! Channel routing itself lives in [`ChannelManager`](super::ChannelManager),
//! which owns the channels and dispatches each channel's frames to the handler
//! registered here — one routing path, no duplication.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{
    CapabilitySet, ChannelType, MessageHandler, SessionId, SessionState, Version,
};

/// A snapshot of an active session, held by the [`SessionRegistry`] so the owner
/// can enumerate and inspect live sessions without holding the session objects
/// themselves.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// The session id.
    pub id: SessionId,
    /// The authenticated peer.
    pub peer: DeviceId,
    /// The current lifecycle state.
    pub state: SessionState,
    /// The negotiated protocol version.
    pub version: Version,
    /// The negotiated capabilities.
    pub capabilities: CapabilitySet,
}

/// A thread-safe registry of active sessions, keyed by [`SessionId`]. Cloning
/// shares the same underlying store.
#[derive(Clone, Default)]
pub struct SessionRegistry {
    inner: Arc<Mutex<HashMap<SessionId, SessionInfo>>>,
}

impl SessionRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        SessionRegistry {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Lock the store, recovering from a poisoned mutex rather than panicking.
    fn guard(&self) -> MutexGuard<'_, HashMap<SessionId, SessionInfo>> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Register or replace a session.
    pub fn register(&self, info: SessionInfo) {
        self.guard().insert(info.id, info);
    }

    /// Update the recorded state of a session, if present.
    pub fn set_state(&self, id: SessionId, state: SessionState) {
        if let Some(info) = self.guard().get_mut(&id) {
            info.state = state;
        }
    }

    /// Fetch a snapshot of a session.
    #[must_use]
    pub fn get(&self, id: SessionId) -> Option<SessionInfo> {
        self.guard().get(&id).cloned()
    }

    /// Remove a session, returning its last snapshot.
    pub fn remove(&self, id: SessionId) -> Option<SessionInfo> {
        self.guard().remove(&id)
    }

    /// Number of active sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.guard().len()
    }

    /// Whether there are no active sessions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.guard().is_empty()
    }

    /// The ids of all active sessions.
    #[must_use]
    pub fn active_ids(&self) -> Vec<SessionId> {
        self.guard().keys().copied().collect()
    }

    /// A snapshot of every active session, for diagnostics/enumeration.
    #[must_use]
    pub fn list(&self) -> Vec<SessionInfo> {
        self.guard().values().cloned().collect()
    }
}

/// Maps a [`ChannelType`] to the handler that serves it. A session's
/// [`ChannelManager`](super::ChannelManager) consults this when a channel opens,
/// binding the channel's actor to the matching handler.
#[derive(Clone, Default)]
pub struct HandlerRegistry {
    map: HashMap<ChannelType, Arc<dyn MessageHandler>>,
}

impl HandlerRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        HandlerRegistry {
            map: HashMap::new(),
        }
    }

    /// Register a handler under its own [`MessageHandler::channel_type`].
    pub fn register(&mut self, handler: Arc<dyn MessageHandler>) {
        self.map.insert(handler.channel_type(), handler);
    }

    /// Builder-style [`register`](HandlerRegistry::register).
    #[must_use]
    pub fn with(mut self, handler: Arc<dyn MessageHandler>) -> Self {
        self.register(handler);
        self
    }

    /// The handler for `channel_type`, if any.
    #[must_use]
    pub fn get(&self, channel_type: ChannelType) -> Option<Arc<dyn MessageHandler>> {
        self.map.get(&channel_type).cloned()
    }

    /// Whether a handler is registered for `channel_type`.
    #[must_use]
    pub fn contains(&self, channel_type: ChannelType) -> bool {
        self.map.contains_key(&channel_type)
    }

    /// Number of registered handlers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether no handlers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: u128) -> SessionInfo {
        SessionInfo {
            id: SessionId::from_u128(id),
            peer: DeviceId::from("peer"),
            state: SessionState::Active,
            version: Version::CURRENT,
            capabilities: CapabilitySet::new(),
        }
    }

    #[test]
    fn session_registry_register_get_remove() {
        let reg = SessionRegistry::new();
        assert!(reg.is_empty());
        reg.register(info(1));
        reg.register(info(2));
        assert_eq!(reg.len(), 2);
        assert!(reg.get(SessionId::from_u128(1)).is_some());
        reg.set_state(SessionId::from_u128(1), SessionState::Closed);
        assert_eq!(
            reg.get(SessionId::from_u128(1)).map(|i| i.state),
            Some(SessionState::Closed)
        );
        assert!(reg.remove(SessionId::from_u128(1)).is_some());
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn session_registry_is_concurrency_safe() {
        let reg = SessionRegistry::new();
        std::thread::scope(|scope| {
            for n in 0..16u128 {
                let reg = reg.clone();
                scope.spawn(move || {
                    reg.register(info(n));
                    let _ = reg.active_ids();
                    reg.set_state(SessionId::from_u128(n), SessionState::ShuttingDown);
                });
            }
        });
        assert_eq!(reg.len(), 16);
    }

    #[test]
    fn handler_registry_len_and_contains() {
        let reg = HandlerRegistry::new();
        assert!(reg.is_empty());
        assert!(!reg.contains(ChannelType::TRANSFER));
        assert_eq!(reg.len(), 0);
    }
}
