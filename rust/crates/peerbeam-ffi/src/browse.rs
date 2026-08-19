//! Wiring the Browse channel into every session, and asking a peer to list.

use std::sync::Mutex;

use serde_json::{json, Value};

use crate::error::Code;

/// The shape every FFI operation returns, as the sibling modules define it.
type Op = Result<Value, (Code, String)>;

/// The folders this device shares, set once the engine reads its config.
///
/// A process-global for the reason presence's is: `establish` reads it
/// unconditionally, so no dial or accept call site can forget to wire browsing
/// and leave a peer's request silently unanswered.
///
/// **Empty until configured, and empty is the default configuration.**
static SHARES: Mutex<Option<peerbeam_browse::Shares>> = Mutex::new(None);

/// Point this process's browsing at `directories`.
pub fn configure(directories: &[String]) {
    let shares = peerbeam_browse::Shares::new(directories);
    *SHARES.lock().unwrap_or_else(|e| e.into_inner()) = Some(shares);
}

/// The shares a new session should serve, or an empty set when unconfigured.
///
/// Never `None` to the caller: a session with no share list must still answer
/// requests — with nothing — rather than leaving an asker waiting.
#[must_use]
pub fn shares() -> peerbeam_browse::Shares {
    SHARES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// What this device shares, for its own user: `{}` → `{shares:[…]}`.
///
/// Names only, the same view a peer gets. Reporting absolute paths would be
/// harmless here — it is the user's own machine — but keeping one shape means
/// the UI cannot accidentally render a path it then sends somewhere.
pub fn list_shares() -> Op {
    let shares = shares();
    // Name **and** path. The name is what a peer addresses the share by; the
    // path is what the person choosing it needs to see, because two folders
    // called `Documents` are indistinguishable by name and a UI that showed
    // only names would ask someone to confirm a share they cannot identify.
    let entries: Vec<Value> = shares
        .roots()
        .iter()
        .map(|r| {
            json!({
                "name": r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                "path": r.to_string_lossy(),
                "exists": r.is_dir(),
            })
        })
        .collect();
    let names: Vec<String> = entries
        .iter()
        .filter_map(|e| e.get("name").and_then(|n| n.as_str()))
        .map(str::to_string)
        .collect();
    // `shares` kept as names for callers that predate `entries`.
    Ok(json!({ "shares": names, "entries": entries }))
}

/// Turn a peer's answer into the FFI's response shape.
#[must_use]
pub fn response_dto(r: &peerbeam_browse::ListResponse) -> Value {
    json!({
        "path": r.path,
        "entries": r.entries.iter().map(|e| json!({
            "name": e.name,
            "is_dir": e.is_dir,
            "size": e.size,
        })).collect::<Vec<_>>(),
        "truncated": r.truncated,
        // Reported so a surface can say "nothing here, or not allowed" rather
        // than "empty folder" — but deliberately without a reason, because the
        // peer did not send one and inventing one would be a guess.
        "denied": r.denied,
    })
}

/// The error a caller gets when a peer cannot be asked at all.
#[must_use]
pub fn unreachable(path: &str) -> (Code, String) {
    (
        Code::Connection,
        format!("could not ask the device about {path}"),
    )
}
