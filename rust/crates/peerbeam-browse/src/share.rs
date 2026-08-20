//! Which folders a device shares, the name each is addressed by, and the one
//! function that decides whether a requested path is inside one.
//!
//! # The whole security argument
//!
//! Every path in a `ListRequest` comes from a peer. The only thing standing
//! between "list this share" and "list `/etc`" is [`Shares::resolve`], so it is
//! written to be read: canonicalise first, compare after, and refuse anything
//! that does not end up inside a configured share.
//!
//! Canonicalising **before** the comparison is the point. A textual check
//! against `..` is not enough — a symlink inside a share pointing at `/` passes
//! every string test ever written, and `/srv/share/../../etc` is a perfectly
//! ordinary path until something resolves it.
//!
//! # Why a share has a name of its own
//!
//! A peer addresses a share by its **name**, never by its path, so a device's
//! filesystem layout stays its own business. That name used to be computed on
//! demand as the root's basename, which made it neither unique nor always
//! present: two folders called `Documents` were one addressable share and one
//! permanently unreachable one, and a root with no basename (`/`, `D:\`) had no
//! name at all while the UI went on calling it shared. [`Shares::new`] therefore
//! assigns each root a name once, and guarantees it is non-empty and unique
//! within the set.

use std::path::{Component, Path, PathBuf, Prefix};

/// Why a path was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ShareError {
    #[error("no such share")]
    NoShare,
    #[error("path is outside every shared folder")]
    Outside,
    #[error("path does not exist")]
    Missing,
}

/// One shared folder: where it is, and the name a peer addresses it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    /// The name a peer puts in the first segment of a path. Never empty, never
    /// a path, and unique within its [`Shares`].
    pub name: String,
    /// The canonicalised folder. **Never sent to a peer** — see
    /// [`Shares::resolve`].
    pub root: PathBuf,
}

/// The folders this device shares, in the order the user listed them.
#[derive(Debug, Clone, Default)]
pub struct Shares {
    shares: Vec<Share>,
}

impl Shares {
    /// Build from configured paths, keeping only those that exist and resolve,
    /// and naming each one addressably.
    ///
    /// A share that cannot be canonicalised is dropped rather than kept as a
    /// literal path: an unresolvable root cannot be compared against safely,
    /// and silently treating it as a prefix would be the bug this module
    /// exists to prevent.
    #[must_use]
    pub fn new<I, S>(paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<Path>,
    {
        let mut shares: Vec<Share> = Vec::new();
        for p in paths {
            let Ok(root) = std::fs::canonicalize(p.as_ref()) else {
                continue;
            };
            // The same folder listed twice is one share, not two. Canonicalising
            // first is what makes this catch `~/docs`, `/home/me/docs/.` and a
            // literal repeat alike — and without it the second copy would be
            // given a disambiguating name, so one folder would be offered to
            // peers as two shares that browse identically.
            if shares.iter().any(|s| s.root == root) {
                continue;
            }
            let name = unique_name(&label(&root), &shares);
            shares.push(Share { name, root });
        }
        Shares { shares }
    }

    /// Whether anything is shared at all.
    ///
    /// **The default is nothing.** A device with no configured share answers
    /// every request with an empty listing, however trusted the asker — sharing
    /// is something a user does deliberately, not a consequence of granting a
    /// permission.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shares.is_empty()
    }

    /// The shares themselves, as the top level a peer sees.
    #[must_use]
    pub fn shares(&self) -> &[Share] {
        &self.shares
    }

    /// Resolve a peer-supplied path to a real one inside a share, or refuse.
    ///
    /// `rel` is interpreted relative to the share whose **name** it starts with,
    /// so a peer names `photos/2026` rather than `/home/someone/photos/2026` — a
    /// device's real filesystem layout is not a peer's business, and echoing it
    /// back would leak the home directory's name to anyone allowed to browse.
    ///
    /// The name is the one [`Shares::new`] assigned, which is why every root is
    /// reachable: matching the first segment against `root.file_name()` instead
    /// meant that of two folders called `Documents` only the first was ever
    /// resolved — for browsing, folder sync and file requests alike — and that a
    /// root without a basename could not be named at all.
    ///
    /// An empty `rel` names no share; the list of shares themselves is
    /// `shares()`, which is what the handler answers an empty path with.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, ShareError> {
        let mut parts = rel.split('/').filter(|p| !p.is_empty());
        let Some(first) = parts.next() else {
            return Err(ShareError::NoShare);
        };
        let share = self
            .shares
            .iter()
            .find(|s| s.name == first)
            .ok_or(ShareError::NoShare)?;

        let mut candidate = share.root.clone();
        for part in parts {
            candidate.push(part);
        }
        // Canonicalise, then compare. A symlink inside the share pointing
        // anywhere else is resolved here and refused below; a `..` that climbs
        // out is likewise resolved to what it actually means before anything
        // trusts it.
        let real = std::fs::canonicalize(&candidate).map_err(|_| ShareError::Missing)?;
        if !real.starts_with(&share.root) {
            return Err(ShareError::Outside);
        }
        Ok(real)
    }
}

/// The name to offer `root` under: its basename.
///
/// A filesystem root — `/`, `D:\`, a bare UNC share — has **no** basename, and
/// deriving the name from one alone yielded the empty string: the browse listing
/// dropped the share entirely (an entry with no name), no peer could address it
/// (`resolve` matches the first path segment against the name), and the settings
/// UI went on reporting the folder as shared. Such a root is named after its
/// prefix where it has one, and `root` when it has none.
fn label(root: &Path) -> String {
    if let Some(name) = root.file_name() {
        let name = name.to_string_lossy();
        if !name.is_empty() {
            return name.into_owned();
        }
    }
    for component in root.components() {
        if let Component::Prefix(prefix) = component {
            match prefix.kind() {
                // `D:\` → `D`. Read from the parsed prefix rather than the raw
                // string because canonicalising on Windows yields the verbatim
                // form (`\\?\D:\`), whose text is no kind of name.
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    return char::from(letter).to_string()
                }
                // `\\server\backup` → `backup`: the share's own name is the
                // one part of a UNC path a person would recognise.
                Prefix::UNC(_, share) | Prefix::VerbatimUNC(_, share) => {
                    return share.to_string_lossy().into_owned()
                }
                _ => {}
            }
        }
    }
    "root".to_string()
}

/// `base`, or the first `base (2)`, `base (3)`… no share in `taken` already
/// answers to.
///
/// **Two shares must never answer to the same name.** [`Shares::resolve`] takes
/// the first share whose name matches, so sharing `~/Documents` and
/// `/mnt/nas/Documents` used to list `Documents` twice, resolve both to the
/// first, and leave the second permanently unreachable — no error anywhere, and
/// the settings UI insisting both were shared.
///
/// The suffix follows the order the user listed the folders in, so a share keeps
/// its name across restarts. A folder genuinely called `Documents (2)` listed
/// after those two collides in turn and becomes `Documents (2) (2)`: ugly, but
/// deterministic, and the alternative is one of them being unaddressable again.
fn unique_name(base: &str, taken: &[Share]) -> String {
    let free = |name: &str| !taken.iter().any(|s| s.name == name);
    if free(base) {
        return base.to_string();
    }
    (2u32..)
        .map(|n| format!("{base} ({n})"))
        .find(|candidate| free(candidate))
        // Unreachable in practice (it would need 4 billion same-named shares),
        // and a name that collides is still better than a panic in a path the
        // peer controls the inputs to.
        .unwrap_or_else(|| base.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, Shares) {
        let dir = tempfile::tempdir().unwrap();
        let share = dir.path().join("share");
        std::fs::create_dir(&share).unwrap();
        std::fs::write(share.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir(share.join("sub")).unwrap();
        std::fs::write(share.join("sub").join("b.txt"), b"deep").unwrap();
        std::fs::write(dir.path().join("secret.txt"), b"not shared").unwrap();
        let shares = Shares::new([share]);
        (dir, shares)
    }

    #[test]
    fn a_path_inside_the_share_resolves() {
        let (_dir, shares) = tree();
        assert!(shares.resolve("share/a.txt").is_ok());
        assert!(shares.resolve("share/sub/b.txt").is_ok());
    }

    #[test]
    fn a_dot_dot_climb_is_refused() {
        // The obvious attack, and the reason canonicalisation happens before
        // the comparison rather than after.
        let (_dir, shares) = tree();
        assert_eq!(
            shares.resolve("share/../secret.txt"),
            Err(ShareError::Outside)
        );
        assert_eq!(
            shares.resolve("share/sub/../../secret.txt"),
            Err(ShareError::Outside)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_share_is_refused() {
        // A textual check against `..` passes this every time. Only resolving
        // the link catches it.
        let (dir, shares) = tree();
        let link = dir.path().join("share").join("escape");
        std::os::unix::fs::symlink(dir.path().join("secret.txt"), &link).unwrap();
        assert_eq!(shares.resolve("share/escape"), Err(ShareError::Outside));
    }

    #[test]
    fn a_path_naming_no_share_is_refused() {
        let (_dir, shares) = tree();
        assert_eq!(shares.resolve("nope/a.txt"), Err(ShareError::NoShare));
        assert_eq!(shares.resolve(""), Err(ShareError::NoShare));
        // An absolute path is not a share name, so it names nothing.
        assert_eq!(shares.resolve("/etc/passwd"), Err(ShareError::NoShare));
    }

    #[test]
    fn a_device_with_no_shares_shares_nothing() {
        // The default. A permission is not a share.
        let shares = Shares::new(Vec::<PathBuf>::new());
        assert!(shares.is_empty());
        assert_eq!(shares.resolve("anything"), Err(ShareError::NoShare));
    }

    #[test]
    fn a_share_that_does_not_exist_is_dropped_rather_than_trusted() {
        let shares = Shares::new(["/definitely/not/here/at/all"]);
        assert!(shares.is_empty());
    }

    /// Two folders with the same basename, each holding a file only it has.
    fn two_documents() -> (tempfile::TempDir, Shares) {
        let dir = tempfile::tempdir().unwrap();
        for (parent, marker) in [("home", "mine.txt"), ("nas", "theirs.txt")] {
            let docs = dir.path().join(parent).join("Documents");
            std::fs::create_dir_all(&docs).unwrap();
            std::fs::write(docs.join(marker), b"x").unwrap();
        }
        let shares = Shares::new([
            dir.path().join("home").join("Documents"),
            dir.path().join("nas").join("Documents"),
        ]);
        (dir, shares)
    }

    /// **Both shares must be reachable.** Addressing a share by its root's
    /// basename made the second `Documents` unreachable forever: it listed under
    /// a name that resolved to the first one, so browsing it, syncing it and
    /// requesting a file from it all silently answered from the wrong folder.
    #[test]
    fn two_shares_with_the_same_basename_are_both_reachable() {
        let (_dir, shares) = two_documents();
        let names: Vec<&str> = shares.shares().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Documents", "Documents (2)"]);

        // Each name resolves inside its **own** root, which is the whole point:
        // the file that only the second share holds is only reachable under the
        // second share's name.
        assert!(shares.resolve("Documents/mine.txt").is_ok());
        assert!(shares.resolve("Documents (2)/theirs.txt").is_ok());
        assert_eq!(
            shares.resolve("Documents/theirs.txt"),
            Err(ShareError::Missing),
            "the first share must not answer for the second's contents"
        );
        assert_eq!(
            shares.resolve("Documents (2)/mine.txt"),
            Err(ShareError::Missing)
        );
    }

    /// A name is disambiguated even against a folder that literally holds the
    /// disambiguated form, so no two shares can ever collide.
    #[test]
    fn a_folder_literally_named_like_a_disambiguation_still_gets_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a/Documents", "b/Documents", "c/Documents (2)"] {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        let shares = Shares::new([
            dir.path().join("a").join("Documents"),
            dir.path().join("b").join("Documents"),
            dir.path().join("c").join("Documents (2)"),
        ]);
        let names: Vec<&str> = shares.shares().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Documents", "Documents (2)", "Documents (2) (2)"]);
        // Every one of them resolves, which is the property that matters.
        for name in names {
            assert!(shares.resolve(name).is_ok(), "{name} is unaddressable");
        }
    }

    /// The same folder named twice is one share. Without the dedupe it would be
    /// offered as `Documents` and `Documents (2)`, two names for one folder.
    #[test]
    fn the_same_folder_shared_twice_is_one_share() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("Documents");
        std::fs::create_dir(&docs).unwrap();
        let shares = Shares::new([docs.clone(), docs.join("."), docs]);
        assert_eq!(shares.shares().len(), 1);
        assert_eq!(shares.shares()[0].name, "Documents");
    }

    /// A root with no basename must still be addressable. `/` reported an empty
    /// name: the browse listing dropped it, no peer could name it, and the UI
    /// still said it was shared.
    #[cfg(unix)]
    #[test]
    fn a_filesystem_root_is_named_so_it_can_be_addressed() {
        let shares = Shares::new(["/"]);
        assert_eq!(shares.shares().len(), 1);
        assert_eq!(shares.shares()[0].name, "root");
        assert_eq!(shares.resolve("root"), Ok(PathBuf::from("/")));
    }

    /// Every share has a usable name: non-empty, and never a path segment a
    /// `/`-separated request could not carry.
    #[test]
    fn every_share_name_is_non_empty_and_addressable() {
        let (_dir, shares) = two_documents();
        for share in shares.shares() {
            assert!(!share.name.is_empty(), "{share:?} has no name");
            assert!(
                !share.name.contains('/'),
                "{share:?} cannot be a path segment"
            );
        }
    }
}
