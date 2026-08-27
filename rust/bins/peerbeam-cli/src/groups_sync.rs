//! Applying inbound group membership frames in the CLI.
//!
//! # Why a process-global rather than an argument
//!
//! Group frames ride the Chat channel, so they arrive through the chat handler
//! or not at all — a session maps one handler per channel. `establish` builds
//! that handler, and it has no configuration to build a group store from;
//! threading one through would mean changing eighteen `dial`/`accept` call
//! sites, and every one of them a future call site could forget.
//!
//! That is not a hypothetical cost. The first cut of groups had a `send_foreign`
//! and nothing on the receiving side at all: every invitation, join and leave
//! that arrived was discarded without a trace. The sending half worked and the
//! receiving half did not exist, and nothing failed — which is exactly what
//! `session_exec`'s own comment warns about for notes.
//!
//! So the store is installed once, by `dispatch`, and read here. A command that
//! never installs one behaves exactly as it did before: frames are ignored,
//! as they were for every build that predates groups.

use std::sync::OnceLock;

use peerbeam_domain::id::DeviceId;
use peerbeam_groups::GroupStore;

static STORE: OnceLock<GroupStore> = OnceLock::new();

/// Make this process able to receive group membership frames.
///
/// Idempotent, and deliberately silent on a second call: `dispatch` runs once
/// per process, and a command that somehow installed twice has not done
/// anything wrong.
pub fn install(store: GroupStore) {
    let _ = STORE.set(store);
}

/// The sink `establish` registers, or `None` when no store was installed.
#[must_use]
pub fn sink() -> Option<peerbeam_chat::ForeignSink> {
    let store = STORE.get()?.clone();
    Some(std::sync::Arc::new(
        move |peer: DeviceId, message_type: u16, payload: Vec<u8>| {
            match peerbeam_groups::apply(&store, &peer, message_type, &payload) {
                // Held, never adopted: an invitation becomes a pending offer
                // and only the user's acceptance writes a roster (A2,
                // condition 4).
                Ok(peerbeam_groups::GroupEvent::Invited {
                    group,
                    name,
                    from,
                    members,
                }) => {
                    let invite = peerbeam_groups::PendingInvite {
                        group,
                        name,
                        from,
                        members,
                        at: chrono::Utc::now().to_rfc3339(),
                    };
                    // Silently dropped on a store error, and deliberately:
                    // this runs inside a session's frame loop, where there is
                    // no `Ctx` to print through and no caller to return to. The
                    // user sees the consequence — no invitation appears — and
                    // `group list` is where they look. Adding a `tracing`
                    // dependency to this binary for one line is not worth it.
                    let _ = store.record_invite(&invite);
                }
                // The roster write already happened inside `apply`.
                Ok(_) => {}
                // As above: no channel to report through from here.
                Err(_) => {}
            }
        },
    ))
}
