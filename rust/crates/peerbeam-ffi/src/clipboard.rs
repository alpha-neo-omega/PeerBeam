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
    std::sync::Arc::new(|peer: DeviceId, clip: Clip| emit_received(&peer, &clip))
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

    *SLOT.lock().unwrap() = Some(item.clone());
    events::emit(&json!({
        "type": "clipboard_updated",
        "timestamp": timestamp(),
        "payload": { "item": item },
    }));
    Ok(json!({ "set": true }))
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
