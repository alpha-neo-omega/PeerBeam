//! Wiring inbound group membership messages into every session.
//!
//! Read from the running engine rather than passed down through `dial`/`accept`
//! signatures, for the reason `notes_sync` gives and that this feature proved
//! the hard way: an argument is one more thing a future call site can forget,
//! and a session that does not forward a group frame drops it **silently**. The
//! first cut of groups shipped exactly that — `send_foreign` existed, nothing
//! called `peerbeam_groups::apply`, and every invitation, join and leave that
//! arrived was discarded without a trace. The sending half worked; the
//! receiving half did not exist.

use std::sync::Arc;

use peerbeam_domain::id::DeviceId;

/// A sink that applies inbound group frames to this device's group store.
///
/// Returns `None` when the engine is not initialised — every unit test that
/// builds a session by hand — which is correct rather than convenient: with no
/// engine there is no group store to write, and inventing one would give a test
/// a second store the rest of the process knows nothing about.
#[must_use]
pub fn sink() -> Option<peerbeam_chat::ForeignSink> {
    let mgr = crate::runtime::manager().ok()?;
    Some(Arc::new(
        move |peer: DeviceId, message_type: u16, payload: Vec<u8>| {
            let store = mgr.group_store();
            match peerbeam_groups::apply(&store, &peer, message_type, &payload) {
                // An invitation is **held**, never adopted: it becomes a pending
                // offer for the user to answer, and only their acceptance writes a
                // roster (A2, condition 4).
                Ok(peerbeam_groups::GroupEvent::Invited {
                    group,
                    name,
                    from,
                    members,
                }) => {
                    let invite = peerbeam_groups::PendingInvite {
                        group: group.clone(),
                        name,
                        from,
                        members,
                        at: crate::transfer::timestamp(),
                    };
                    if let Err(e) = store.record_invite(&invite) {
                        // Logged, not fatal: a peer must not be able to break this
                        // device's session by sending something unstorable.
                        tracing::warn!("could not record a group invitation: {e}");
                        return;
                    }
                    crate::events::event(&serde_json::json!({
                        "type": "groups_changed",
                        "reason": "invited",
                        "group": group,
                        "timestamp": crate::transfer::timestamp(),
                    }));
                }
                Ok(peerbeam_groups::GroupEvent::Joined { group, .. })
                | Ok(peerbeam_groups::GroupEvent::Left { group, .. }) => {
                    crate::events::event(&serde_json::json!({
                        "type": "groups_changed",
                        "reason": "roster",
                        "group": group,
                        "timestamp": crate::transfer::timestamp(),
                    }));
                }
                // A frame this build does not implement, or one naming a group this
                // device does not hold. Neither is an error — see `apply`.
                Ok(peerbeam_groups::GroupEvent::Ignored) => {}
                Err(e) => tracing::warn!("could not apply a group message: {e}"),
            }
        },
    ))
}
