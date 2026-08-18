//! Wiring the Notes channel into every session.
//!
//! Read from the running engine rather than passed down through `dial`/`accept`
//! signatures, for the same reason the clipboard handler is registered
//! unconditionally: an argument is one more thing a future call site can forget,
//! and a session with no `NotesHandler` drops an inbound batch **silently**
//! rather than refusing it — the peer believes it synced and nothing arrived.

use std::sync::Arc;

use peerbeam_domain::port::TrustStore;

use crate::session_exec::NotesWiring;

/// The notes wiring for a new session, or `None` when the engine is not
/// initialised (every unit test that builds a session by hand).
///
/// Returning `None` there is correct rather than convenient: with no engine
/// there is no note store to sync against, and inventing one would give a test
/// a second store the rest of the process knows nothing about.
#[must_use]
pub fn wiring() -> Option<NotesWiring> {
    let mgr = crate::runtime::manager().ok()?;
    Some(NotesWiring {
        store: mgr.notes_store(),
        trust: mgr.trust_store() as Arc<dyn TrustStore>,
    })
}
