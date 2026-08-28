//! Whether an inbound file may be taken from a peer at all.
//!
//! # Why this lives here and not in the app
//!
//! It used to be `pub(crate)` in `peerbeam-ffi`, which meant the CLI could not
//! reach it — and did not enforce it. `peerbeam receive` and `peerbeam daemon`
//! took files from any authenticated peer, so revoking a device's `files`
//! permission changed nothing on a headless box while `docs/SECURITY.md` said
//! the permission was "enforced in both directions". One of the two had to give.
//!
//! Moving the decision beside the other transfer gates makes it one predicate
//! both surfaces ask, which is the same shape `peerbeam-chat::gate` and
//! `peerbeam-transfer::pipe::gate` already have.

use peerbeam_domain::entity::Permission;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;

/// What the trust store says about an inbound transfer from `peer`, decided
/// **before** anyone is asked anything.
///
/// Three outcomes rather than a bool, because the store genuinely has three
/// things to say and collapsing any two of them loses a real behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAdmission {
    /// The user approved this device and then took its `files` permission
    /// away. Refuse without prompting: they have already answered.
    Refused,
    /// Ask the user, exactly as this build always has.
    Prompt,
    /// Accept without asking.
    AutoAccept,
}

/// The trust half of the inbound-transfer decision.
///
/// Extracted from `handle_incoming` as a pure function of data already in hand
/// — no session, no network — so every leg is unit-testable and a refactor
/// cannot delete one silently. It mirrors [`should_send_decline`] and the four
/// `may_*` gates: the predicate is the tested unit, the call site is the thin
/// part.
///
/// The legs, in order:
///
/// 1. **Approved, but `files` revoked → [`Refused`].** The user said "this
///    device may not send me files". Prompting anyway would ask them to
///    re-decide something they already decided, on the schedule of whoever is
///    sending — which is how a permission becomes a nuisance rather than a
///    setting. This also beats a resume: an interrupted transfer that was
///    accepted before the permission was taken away does not get to finish
///    (revoking applies to the *next* operation, and this is one).
/// 2. **`auto_accept` and the device may [`Permission::Files`] →
///    [`AutoAccept`].** Formerly `auto && record.approved`;
///    [`TrustStore::may`] implies that approval, so this is strictly narrower
///    than what it replaces and no device that auto-accepted before stops
///    doing so unless the permission was deliberately removed.
///
///    `auto_accept` is now **either** the global setting **or** this device's
///    own `auto_accept` bit — "stop asking me about this one" — and the two are
///    deliberately an `or`: the per-device answer exists precisely so the
///    global one does not have to be turned on for everybody. Both are still
///    `&& may_files`, so neither can admit a byte the `files` permission would
///    refuse. That conjunction is the whole safety argument and must not be
///    loosened: this is a setting about *asking*, never about *allowing*.
/// 3. **Everything else → [`Prompt`].** In particular a merely *pinned* peer —
///    the state the TOFU handshake leaves every stranger in — is prompted
///    exactly as it always was. Permissions narrow a standing the user granted;
///    they never create one, and they must not turn first contact into a silent
///    refusal.
///
/// [`Refused`]: FileAdmission::Refused
/// [`AutoAccept`]: FileAdmission::AutoAccept
/// [`Prompt`]: FileAdmission::Prompt
/// [`TrustStore::may`]: peerbeam_domain::port::TrustStore::may
pub fn admit_transfer(auto_accept: bool, trust: &dyn TrustStore, peer: &DeviceId) -> FileAdmission {
    let may_files = trust.may(peer, Permission::Files);
    if trust.is_approved(peer) && !may_files {
        return FileAdmission::Refused;
    }
    if auto_accept && may_files {
        return FileAdmission::AutoAccept;
    }
    FileAdmission::Prompt
}

/// [`admit_transfer`], with the per-device *stop asking about this one* bit
/// folded into the global setting.
///
/// Kept as a separate function so [`admit_transfer`]'s existing tests keep
/// asserting the gate itself, and so the `or` between the two settings is one
/// line that can be pointed at rather than a condition spread across callers.
pub fn admit_transfer_for(
    global_auto_accept: bool,
    per_device_auto_accept: bool,
    trust: &dyn TrustStore,
    peer: &DeviceId,
) -> FileAdmission {
    admit_transfer(global_auto_accept || per_device_auto_accept, trust, peer)
}

/// The same decision on the **outbound** path: may this device send files to
/// `peer`?
///
/// The mirror of [`admit_transfer`]'s first leg, and deliberately the same
/// shape: a device the user narrowed is refused, a device they never decided
/// about is not. Sending to a merely-pinned peer must keep working — that is
/// the ordinary "spot a device, send it a file" flow, and gating it on `may`
/// (which implies approval) would break the primary purpose for every device
/// the user has not explicitly accepted.
#[must_use]
pub fn may_send_files(trust: &dyn TrustStore, peer: &DeviceId) -> bool {
    !trust.is_approved(peer) || trust.may(peer, Permission::Files)
}
