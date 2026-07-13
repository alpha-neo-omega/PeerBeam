//! Clipboard bridge: a synchronized last-item slot with typed classification
//! (text / URL / code / image-metadata). Images cross as metadata only — never
//! large buffers over FFI. Setting an item emits `clipboard_updated`.

use std::sync::Mutex;

use serde_json::{json, Value};

use peerbeam_domain::entity::{classify, ClipboardKind};

use crate::error::Code;
use crate::events;

static SLOT: Mutex<Option<Value>> = Mutex::new(None);

type Op = Result<Value, (Code, String)>;

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
