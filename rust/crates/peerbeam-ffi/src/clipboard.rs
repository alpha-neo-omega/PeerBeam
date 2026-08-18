//! Clipboard bridge: a synchronized last-item slot with typed classification
//! (text / URL / code / image-metadata). Images cross as metadata only — never
//! large buffers over FFI. Setting an item emits `clipboard_updated`.
//!
//! Since Phase B this module also carries the **auto-sync** half of the
//! Clipboard capability (ChannelType `0x0102`): the opt-in setting's current
//! value, and the sink that surfaces a clip a peer sent. The privacy decision
//! itself lives in `peerbeam_clipboard::may_share_clip` and is enforced by
//! `ClipboardSender::send`. Nothing here may re-implement it or work around it.
//!
//! The two halves are deliberately separate. The [`SLOT`] below is the older
//! manual bridge behind `pb_clipboard_get`/`set`; auto-sync neither reads nor
//! writes it, because conflating "the last item a surface handed us" with
//! "what a peer just sent" would make the local slot change under the user for
//! reasons they never asked about.

use std::sync::Mutex;

use serde_json::{json, Value};

use peerbeam_clipboard::{Clip, ClipboardSink};
use peerbeam_domain::entity::{classify, ClipboardKind};
use peerbeam_domain::id::DeviceId;

use crate::error::Code;
use crate::events;

static SLOT: Mutex<Option<Value>> = Mutex::new(None);

type Op = Result<Value, (Code, String)>;

/// The settings key for the single opt-in toggle.
///
/// *"Sync clipboard with trusted devices"*, **default off**. One key, one
/// meaning: with it false this device sends no clip at all, to anyone. It is
/// deliberately not a per-peer list — the trusted-only rule already scopes who
/// can receive, and a second, finer control would only make it harder to answer
/// "who sees what I copy?" at a glance.
pub const SYNC_KEY: &str = "sync_clipboard";

/// Whether the opt-in setting is currently on. **Defaults to off** when the key
/// is absent, unreadable, or not a boolean — a settings file this build cannot
/// parse must not be read as consent, and a settings document written before
/// this feature existed must never silently opt a user in.
#[must_use]
pub fn sync_enabled() -> bool {
    crate::settings::get()
        .ok()
        .and_then(|s| s.get(SYNC_KEY).and_then(Value::as_bool))
        .unwrap_or(false)
}

/// The settings key for clipboard history.
///
/// *"Keep a short clipboard history on this device"*, **default off**, and
/// deliberately **separate from [`SYNC_KEY`]**. Syncing your clipboard and
/// keeping a record of it are different decisions with different risks, and
/// bundling them would mean someone who wanted their laptop and desktop to
/// share a clipboard also got a stored log they never asked for — which is
/// precisely what clipboard sync promised not to create.
pub const HISTORY_KEY: &str = "clipboard_history";

/// Whether clipboard history is on. **Defaults to off** on an absent,
/// unreadable or non-boolean key: a settings document written before this
/// feature existed must never be read as consent to start recording.
#[must_use]
pub fn history_enabled() -> bool {
    crate::settings::get()
        .ok()
        .is_some_and(|s| history_enabled_in(&s))
}

/// The decision as a pure function of a settings document.
///
/// Split out for the reason the read-receipt opt-in was: read through global
/// settings the fallback is unreachable, because `defaults()` writes the key —
/// so a test going through [`history_enabled`] cannot tell `unwrap_or(false)`
/// from `unwrap_or(true)`. Only an explicit `true` is consent.
#[must_use]
pub fn history_enabled_in(settings: &Value) -> bool {
    settings.get(HISTORY_KEY).and_then(Value::as_bool) == Some(true)
}

/// Emit a `clipboard_received` event so the surface can apply the clip to the
/// system clipboard and tell the user it happened.
///
/// Deliberately **not** `clipboard_updated`: that event belongs to the local
/// slot above and means something different. A surface must be able to tell "I
/// put this here" from "Bob's machine put this here", because only the second
/// needs announcing.
///
/// The text crosses to Dart because applying it is the whole point; it is never
/// logged on the way (`events::emit` hands the JSON straight to the registered
/// callback and writes nothing), which matters more here than anywhere else in
/// the FFI — this is the one payload guaranteed to sometimes be a password.
pub fn emit_received(peer: &DeviceId, clip: &Clip) {
    events::emit(&json!({
        "type": "clipboard_received",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "payload": {
            "device_id": peer.0,
            "text": clip.text,
            "sent_at": clip.sent_at,
        },
    }));
}

/// The sink every session's `ClipboardHandler` is built with.
///
/// A free function rather than a `ClipboardWiring` struct threaded through
/// every call site (as `ChatWiring` and `PresenceWiring` are) because clipboard
/// sync needs no per-session state: no store, no registry, nothing to hold. So
/// `session_exec::establish` registers it unconditionally, and the
/// missed-call-site bug those wiring types exist to guard against cannot occur
/// here at all — there is no call site to miss.
#[must_use]
pub fn sink() -> ClipboardSink {
    std::sync::Arc::new(|peer: DeviceId, clip: Clip| {
        // Recorded before the event, so history reflects what arrived even if
        // the surface never applies it — and gated here, once, rather than at
        // each surface that might want to remember something.
        if history_enabled() {
            if let Ok(mgr) = crate::runtime::manager() {
                let _ = mgr.clip_history().record(&clip.text, Some(peer.0.as_str()));
            }
        }
        emit_received(&peer, &clip);
    })
}

fn kind_str(k: ClipboardKind) -> &'static str {
    match k {
        ClipboardKind::Text => "text",
        ClipboardKind::Url => "url",
        ClipboardKind::Code => "code",
        ClipboardKind::Image => "image",
    }
}

/// Set the clipboard item. `{text:"…"}` is auto-classified (text/url/code);
/// `{kind:"image", mime, size}` stores image *metadata* only.
pub fn set(req: &Value) -> Op {
    let item = if let Some(text) = req.get("text").and_then(|t| t.as_str()) {
        json!({
            "kind": kind_str(classify(text)),
            "text": text,
            "at": timestamp(),
        })
    } else if req.get("kind").and_then(|k| k.as_str()) == Some("image") {
        json!({
            "kind": "image",
            "mime": req.get("mime").and_then(|m| m.as_str()).unwrap_or("application/octet-stream"),
            "size": req.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
            "at": timestamp(),
        })
    } else {
        return Err((
            Code::InvalidArgument,
            "clipboard set needs `text` or `{kind:\"image\",…}`".into(),
        ));
    };

    // Remember what this device put on its own clipboard, when history is on.
    // Text only: an image entry carries metadata, not content, and a log of
    // sizes and MIME types is noise nobody can paste.
    if history_enabled() {
        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
            if let Ok(mgr) = crate::runtime::manager() {
                let _ = mgr.clip_history().record(text, None);
            }
        }
    }

    *SLOT.lock().unwrap() = Some(item.clone());
    events::emit(&json!({
        "type": "clipboard_updated",
        "timestamp": timestamp(),
        "payload": { "item": item },
    }));
    Ok(json!({ "set": true }))
}

/// Every remembered clip, newest first: `{}` → `{entries:[…]}`.
///
/// Empty unless the user turned history on. Reading is never gated — an empty
/// list is the honest answer for a device that records nothing, and refusing
/// the call would make "off" indistinguishable from "broken".
pub fn history_list() -> Op {
    let entries = crate::runtime::manager()?
        .clip_history()
        .list()
        .map_err(|e| (Code::Internal, e.to_string()))?;
    Ok(json!({ "entries": entries }))
}

/// Forget every remembered clip: `{}` → `{cleared: n}`.
///
/// Works whether or not history is currently on: turning the setting off stops
/// new entries but does not erase what was already recorded, and a user who
/// wants it gone needs a way to say so.
pub fn history_clear() -> Op {
    let cleared = crate::runtime::manager()?
        .clip_history()
        .clear()
        .map_err(|e| (Code::Internal, e.to_string()))?;
    Ok(json!({ "cleared": cleared }))
}

/// The current clipboard item, or `{item:null}`.
pub fn get() -> Op {
    Ok(json!({ "item": *SLOT.lock().unwrap() }))
}

/// Enable clipboard events (they always flow through the event callback; this
/// exists for API symmetry with the other subscribe calls).
pub fn subscribe() -> Op {
    Ok(json!({ "subscribed": true }))
}

fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The promise clipboard sync made.** With history off — which includes a
    /// settings document written before this feature existed, one this build
    /// cannot parse, and one storing the key as anything but a bool — nothing
    /// is recorded. Only an explicit `true` is consent.
    ///
    /// Asserted against the pure function rather than through global settings:
    /// `defaults()` writes the key, so the fallback is unreachable that way and
    /// a test could not tell `unwrap_or(false)` from `unwrap_or(true)`.
    #[test]
    fn clipboard_history_records_nothing_without_explicit_consent() {
        for doc in [
            serde_json::json!({}),
            serde_json::json!({ "clipboard_history": false }),
            serde_json::json!({ "clipboard_history": "yes" }),
            serde_json::json!({ "clipboard_history": 1 }),
            serde_json::json!({ "clipboard_history": null }),
            // Syncing is not consent to record: they are separate decisions.
            serde_json::json!({ "sync_clipboard": true }),
        ] {
            assert!(
                !history_enabled_in(&doc),
                "read as consent to record the clipboard: {doc}"
            );
        }
        assert!(history_enabled_in(
            &serde_json::json!({ "clipboard_history": true })
        ));
    }
}
