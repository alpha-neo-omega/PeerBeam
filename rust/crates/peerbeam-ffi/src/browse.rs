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
    let mut entries: Vec<Value> = shares
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
    // Folders the user configured that this device cannot serve — an unmounted
    // drive, a folder since deleted, a path the process may not read.
    //
    // They belong here and **only** here. This DTO answers the device's own
    // user, which is why it carries the path at all; the peer-facing listing is
    // `peerbeam_browse::list`, which reads `shares()` and never sees these.
    //
    // Reporting them is the whole point: `Shares::new` cannot canonicalise them,
    // so they were dropped, so the Settings card that promises "a folder that
    // has gone is still listed, and marked broken" silently showed a shorter
    // list instead — a user whose external drive was unplugged saw a share they
    // had configured simply absent, which is the one belief that section exists
    // to prevent.
    //
    // `name` is empty because they have none: naming is assigned by
    // `Shares::new` to things a peer can address, and these are addressable by
    // nobody. The path is what identifies them to the person who chose it.
    entries.extend(shares.unresolved().iter().map(|p| {
        json!({
            "name": "",
            "path": p.to_string_lossy(),
            "exists": false,
        })
    }));
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

    /// **A configured folder this device cannot serve is still listed.**
    ///
    /// It used to vanish: `Shares::new` cannot canonicalise an unmounted drive
    /// or a deleted folder, so it dropped the path, so this DTO never mentioned
    /// it and the Settings card showed a shorter list than the user had
    /// configured. The card's own promise — "a folder that has gone is still
    /// listed... it is marked broken instead" — could not be kept about a path
    /// that had already been thrown away, and the user was left believing they
    /// had never shared it.
    #[test]
    fn a_folder_that_cannot_be_reached_is_listed_as_broken_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("photos");
        std::fs::create_dir(&real).unwrap();
        let gone = dir.path().join("external-drive");

        let shares = peerbeam_browse::Shares::new([
            real.to_string_lossy().to_string(),
            gone.to_string_lossy().to_string(),
        ]);
        let dto = shares_dto(&shares);
        let entries = dto["entries"].as_array().unwrap();

        assert_eq!(entries.len(), 2, "the broken folder was dropped: {dto}");
        let broken = entries
            .iter()
            .find(|e| e["exists"] == serde_json::Value::Bool(false))
            .expect("no entry reported as missing");
        assert!(
            broken["path"].as_str().unwrap().contains("external-drive"),
            "the broken row does not identify the folder: {broken}"
        );
    }

    /// **...and it is still reachable by nobody.** Listing it locally must not
    /// make it servable: an unresolvable root cannot be compared against
    /// safely, which is why `Shares::new` refuses to treat it as a prefix.
    #[test]
    fn a_broken_folder_is_visible_to_its_owner_and_addressable_by_no_peer() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("external-drive");
        let shares = peerbeam_browse::Shares::new([gone.to_string_lossy().to_string()]);

        // Visible to its owner...
        assert_eq!(shares.unresolved().len(), 1);
        assert_eq!(shares_dto(&shares)["entries"].as_array().unwrap().len(), 1);

        // ...and absent from everything a peer can reach.
        assert!(
            shares.shares().is_empty(),
            "a peer can see the broken share"
        );
        assert!(shares.is_empty(), "the peer-facing set is not empty");
        assert!(
            dto_names(&shares_dto(&shares)).is_empty(),
            "the broken share was given an addressable name"
        );
        for probe in ["external-drive", "external-drive/x", ""] {
            assert!(
                shares.resolve(probe).is_err(),
                "a peer resolved `{probe}` to a folder this device cannot serve"
            );
        }
    }

    /// The `shares` array is names only, and a nameless broken row must not
    /// smuggle an empty string into it.
    fn dto_names(dto: &Value) -> Vec<String> {
        dto["shares"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect()
    }

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
