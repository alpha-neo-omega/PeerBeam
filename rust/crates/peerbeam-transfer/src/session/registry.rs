//! Registries: active sessions, capability handlers, and the message dispatcher.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::{
    CapabilitySet, ChannelId, ChannelType, MessageHandler, SessionError, SessionFrame, SessionId,
    SessionState, Version,
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
}

/// Maps a [`ChannelType`] to the handler that serves it.
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

/// The result of dispatching one data-channel frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// A handler processed the frame.
    Handled,
    /// The channel is bound to a type, but no handler is registered for it.
    NoHandler,
    /// The channel id is not bound to any type (no channel was opened for it).
    Unbound,
}

/// Routes inbound data-channel frames to the handler registered for the channel's
/// type. Control-channel frames are handled by the session itself, not here —
/// this dispatcher owns *capability* routing only (one responsibility).
///
/// In this milestone no data channels are opened, so a session's dispatcher stays
/// empty and every non-control frame resolves to [`DispatchOutcome::Unbound`]; the
/// binding/handler paths are exercised directly by the dispatcher's tests and are
/// the extension seam later milestones populate.
#[derive(Clone, Default)]
pub struct MessageDispatcher {
    handlers: HandlerRegistry,
    channels: HashMap<ChannelId, ChannelType>,
}

impl MessageDispatcher {
    /// An empty dispatcher.
    #[must_use]
    pub fn new() -> Self {
        MessageDispatcher {
            handlers: HandlerRegistry::new(),
            channels: HashMap::new(),
        }
    }

    /// Register a capability handler.
    pub fn register(&mut self, handler: Arc<dyn MessageHandler>) {
        self.handlers.register(handler);
    }

    /// Bind a channel id to a channel type (done when a channel is opened).
    pub fn bind(&mut self, channel: ChannelId, channel_type: ChannelType) {
        self.channels.insert(channel, channel_type);
    }

    /// Whether a handler is registered for `channel_type`.
    #[must_use]
    pub fn has_handler(&self, channel_type: ChannelType) -> bool {
        self.handlers.contains(channel_type)
    }

    /// Route one frame to its handler.
    pub async fn dispatch(&self, frame: SessionFrame) -> Result<DispatchOutcome, SessionError> {
        let Some(channel_type) = self.channels.get(&frame.channel).copied() else {
            return Ok(DispatchOutcome::Unbound);
        };
        match self.handlers.get(channel_type) {
            Some(handler) => {
                handler.handle(frame).await?;
                Ok(DispatchOutcome::Handled)
            }
            None => Ok(DispatchOutcome::NoHandler),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use bytes::Bytes;
    use peerbeam_domain::session::{MessageFlags, MessageType};
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    struct CountingHandler {
        channel: ChannelType,
        count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl MessageHandler for CountingHandler {
        fn channel_type(&self) -> ChannelType {
            self.channel
        }
        async fn handle(&self, _frame: SessionFrame) -> Result<(), SessionError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn data_frame(channel: ChannelId) -> SessionFrame {
        SessionFrame::new(
            channel,
            MessageType::new(1),
            MessageFlags::NONE,
            Bytes::from_static(b"x"),
        )
    }

    #[tokio::test]
    async fn dispatch_routes_to_registered_handler() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut d = MessageDispatcher::new();
        d.register(Arc::new(CountingHandler {
            channel: ChannelType::TRANSFER,
            count: count.clone(),
        }));
        d.bind(ChannelId::new(4), ChannelType::TRANSFER);

        assert_eq!(
            d.dispatch(data_frame(ChannelId::new(4))).await.unwrap(),
            DispatchOutcome::Handled
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(d.has_handler(ChannelType::TRANSFER));
    }

    #[tokio::test]
    async fn dispatch_reports_unbound_and_no_handler() {
        let mut d = MessageDispatcher::new();
        // Unbound: no channel bound to id 9.
        assert_eq!(
            d.dispatch(data_frame(ChannelId::new(9))).await.unwrap(),
            DispatchOutcome::Unbound
        );
        // Bound but no handler registered for the type.
        d.bind(ChannelId::new(9), ChannelType::TRANSFER);
        assert_eq!(
            d.dispatch(data_frame(ChannelId::new(9))).await.unwrap(),
            DispatchOutcome::NoHandler
        );
    }
}
