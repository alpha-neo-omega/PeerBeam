//! The one decision that answers: *may a message leave this machine for this
//! peer, right now?*
//!
//! Two legs, combined in a single pure function rather than scattered through
//! the send paths — the same shape as `peerbeam_presence::gate::may_share_status`
//! and `peerbeam_clipboard::gate::may_share_clip`, for the same reason: one
//! place to read, one place to test, and no way for a refactor to delete a leg
//! silently.
//!
//! **Only sending is gated**, exactly as for presence and clipboard. A message
//! that has already arrived is in hand; refusing to persist it would lose the
//! user's data to enforce a policy about what *this* machine says. The permission
//! governs what leaves, not what reaches.

use peerbeam_domain::entity::Permission;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;

/// May we put a chat message on the wire toward `peer`?
///
/// Two legs:
///
/// 1. **If the user approved this device, they must have left it its `chat`
///    permission** — [`TrustStore::may`], the workspace's one permission
///    predicate, asked per message rather than per session so revoking silences
///    the *next* message instead of the next reconnect.
/// 2. **Otherwise, the feature's own prior policy applies, which for chat is
///    "permitted".**
///
/// # Why leg 2 exists, when the other gates have no equivalent
///
/// Presence, clipboard, pipe and auto-accept all required approval *before*
/// permissions existed, so for them `may` — which implies approval — is purely a
/// narrowing and the question never arises. **Chat never required approval**: a
/// peer that completes the handshake can exchange messages, which is what makes
/// "open the app, see a device, talk to it" work with no ceremony, and what
/// `peerbeam-chat/tests/roundtrip.rs` and `peerbeam-ffi/tests/chat_ffi.rs` have
/// always exercised. Gating chat on `may` alone would therefore not narrow
/// anything — it would **revoke chat from every device the user never explicitly
/// approved**, which is precisely the silent breakage the permission model was
/// added to avoid.
///
/// So the rule is stated once, for the whole model: *a device the user took a
/// decision about is governed by that decision; a device they did not is
/// governed by the feature's pre-existing policy.* The transfer admission gate
/// (`peerbeam_ffi::transfer::admit_transfer`) says the same thing in its own
/// terms — an approved device without `files` is refused, an unapproved one is
/// prompted exactly as before. `docs/SECURITY.md` records it.
///
/// This is consistent with I6, which requires per-capability consent for
/// **sensitive actions** and names them: auto-accept, remote commands, live
/// clipboard, remote browse. Typing a message to a peer that is already talking
/// to you is not one of those, and this change is not the place to make chat
/// require approval — that would be a behaviour change wearing a permission
/// model's clothes.
///
/// Fails **closed** in the sense that matters: [`TrustStore::may`] answers
/// `false` on a store error, so an approved device whose store cannot be read is
/// refused.
#[must_use]
pub fn may_exchange_chat(trust: &dyn TrustStore, peer: &DeviceId) -> bool {
    if trust.is_approved(peer) {
        trust.may(peer, Permission::Chat)
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::entity::{PermissionSet, TrustRecord};

    /// `approved` — the user explicitly accepted-and-trusted this device.
    /// `pinned` — the handshake recorded its key, which `auth.rs` does for every
    /// never-seen peer with `approved: false`.
    /// `permissions` — what an approved device was left.
    struct FakeTrust {
        approved: bool,
        pinned: bool,
        permissions: PermissionSet,
        broken: bool,
    }

    impl TrustStore for FakeTrust {
        fn record(&self, _record: TrustRecord) -> peerbeam_domain::error::Result<()> {
            Ok(())
        }
        fn lookup(&self, device: &DeviceId) -> peerbeam_domain::error::Result<Option<TrustRecord>> {
            if self.broken {
                return Err(peerbeam_domain::error::DomainError::Storage(
                    "trust store unreadable".into(),
                ));
            }
            if !self.pinned {
                return Ok(None);
            }
            Ok(Some(TrustRecord {
                device: device.clone(),
                fingerprint: "ff".into(),
                name: "Peer".into(),
                trusted_at: chrono::Utc::now(),
                approved: self.approved,
                permissions: if self.approved {
                    self.permissions
                } else {
                    PermissionSet::none()
                },
                expires_at: None,
                mine: false,
            }))
        }
        fn is_trusted(&self, _device: &DeviceId) -> bool {
            self.pinned
        }
    }

    fn approving() -> FakeTrust {
        FakeTrust {
            approved: true,
            pinned: true,
            permissions: PermissionSet::granted_on_approval(),
            broken: false,
        }
    }

    /// Approved, then narrowed — the state a `peerbeam trust revoke-permission`
    /// or the app's toggle leaves behind.
    fn approving_without(permission: Permission) -> FakeTrust {
        FakeTrust {
            permissions: PermissionSet::granted_on_approval().set(permission, false),
            ..approving()
        }
    }

    /// Pinned by the handshake, never chosen — what a peer the user simply
    /// started chatting to looks like.
    fn pinned_only() -> FakeTrust {
        FakeTrust {
            approved: false,
            ..approving()
        }
    }

    fn bob() -> DeviceId {
        DeviceId::from("pb-bob")
    }

    #[test]
    fn an_approved_device_with_its_chat_permission_may_be_messaged() {
        assert!(may_exchange_chat(&approving(), &bob()));
    }

    /// **The permission gate.** Revoking `chat` from an approved device
    /// silences it — deleting `trust.may(peer, Permission::Chat)` from
    /// [`may_exchange_chat`] must make this fail.
    #[test]
    fn revoking_the_chat_permission_refuses_an_approved_device() {
        assert!(
            !may_exchange_chat(&approving_without(Permission::Chat), &bob()),
            "a device whose chat permission was taken away is not messaged"
        );
    }

    /// **The permissions are separate bits, not an alias for `approved`.**
    /// Revoking any *other* permission leaves chat working.
    #[test]
    fn revoking_a_different_permission_leaves_chat_working() {
        for other in Permission::ALL
            .into_iter()
            .filter(|p| *p != Permission::Chat)
        {
            assert!(
                may_exchange_chat(&approving_without(other), &bob()),
                "revoking {other} must not silence chat"
            );
        }
    }

    /// **The backward-compatibility leg.** Chat has never required approval, so
    /// a merely-pinned peer must still be reachable. Replacing the body with a
    /// bare `trust.may(peer, Permission::Chat)` — which implies approval —
    /// would silently cut off every device the user never approved, and must
    /// make this fail.
    #[test]
    fn a_peer_the_user_never_approved_may_still_be_messaged() {
        let trust = pinned_only();
        assert!(!trust.is_approved(&bob()), "nobody approved it");
        assert!(
            may_exchange_chat(&trust, &bob()),
            "chat has never required approval and must not start to here"
        );
    }

    /// A device with no record at all — the first message of a first
    /// conversation — is likewise permitted.
    #[test]
    fn a_peer_with_no_record_at_all_may_be_messaged() {
        let trust = FakeTrust {
            pinned: false,
            ..approving()
        };
        assert!(may_exchange_chat(&trust, &bob()));
    }

    /// A store that cannot be read cannot say a device is approved, so leg 2
    /// applies and chat behaves as it always did. That is the right direction
    /// here: an unreadable store must not silently cut a user off from every
    /// conversation, and no *sensitive* action is being granted — presence,
    /// clipboard, pipe and auto-accept all fail closed on the same error,
    /// because each of those does send something out on the user's behalf
    /// without asking.
    #[test]
    fn an_unreadable_store_leaves_chat_as_it_was() {
        let trust = FakeTrust {
            broken: true,
            ..approving()
        };
        assert!(!trust.is_approved(&bob()));
        assert!(!trust.may(&bob(), Permission::Chat), "may fails closed");
        assert!(may_exchange_chat(&trust, &bob()));
    }
}
