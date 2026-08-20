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
pub fn list_shares() -> Op {
    Ok(shares_dto(&shares()))
}

/// The share list as the FFI reports it.
///
/// Split from [`list_shares`] so the shape can be asserted without touching the
/// process-global share list — the same reason `peerbeam_browse::list` is a free
/// function rather than a method on the handler.
///
/// Name **and** path. The name is the one `Shares::new` assigned and the only
/// thing a peer can address the share by, so the UI must show *that* rather than
/// re-derive a basename: two folders called `Documents` are one share named
/// `Documents` and one named `Documents (2)`, and a UI showing "Documents"
/// twice would be offering a name only one of them answers to. The path is what
/// the person choosing the folder needs in order to tell them apart at all.
#[must_use]
fn shares_dto(shares: &peerbeam_browse::Shares) -> Value {
    let entries: Vec<Value> = shares
        .shares()
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "path": s.root.to_string_lossy(),
                "exists": s.root.is_dir(),
            })
        })
        .collect();
    let names: Vec<&str> = shares.shares().iter().map(|s| s.name.as_str()).collect();
    // `shares` kept as names for callers that predate `entries`.
    json!({ "shares": names, "entries": entries })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **The UI must show the name a peer can actually use.** Reporting each
    /// root's basename showed two folders called `Documents` as two rows both
    /// named `Documents`, only one of which any peer could open — and a root
    /// with no basename (`/`, `D:\\`) as a nameless row that could never be
    /// addressed at all while the row insisted it was shared.
    #[test]
    fn every_reported_share_carries_the_name_that_addresses_it() {
        let dir = tempfile::tempdir().unwrap();
        for parent in ["home", "nas"] {
            std::fs::create_dir_all(dir.path().join(parent).join("Documents")).unwrap();
        }
        let shares = peerbeam_browse::Shares::new([
            dir.path().join("home").join("Documents"),
            dir.path().join("nas").join("Documents"),
        ]);
        let dto = shares_dto(&shares);
        let entries = dto["entries"].as_array().expect("entries");
        assert_eq!(entries.len(), 2);

        let names: Vec<&str> = entries
            .iter()
            .map(|e| e["name"].as_str().expect("a name"))
            .collect();
        assert!(names.iter().all(|n| !n.is_empty()), "{names:?}");
        assert_eq!(
            names.iter().collect::<std::collections::HashSet<_>>().len(),
            2,
            "two shares reported under one name: {names:?}"
        );
        // And the reported name is the one the peer-facing resolver accepts.
        for name in names {
            assert!(shares.resolve(name).is_ok(), "{name} is unaddressable");
        }
        // Both paths are still reported, so the user can tell them apart.
        let paths: Vec<&str> = entries
            .iter()
            .map(|e| e["path"].as_str().expect("a path"))
            .collect();
        assert_eq!(
            paths.iter().collect::<std::collections::HashSet<_>>().len(),
            2
        );
    }
}
