//! Which folders a device shares, and the one function that decides whether a
//! requested path is inside one.
//!
//! # The whole security argument
//!
//! Every path in a `ListRequest` comes from a peer. The only thing standing
//! between "list this share" and "list `/etc`" is [`resolve`], so it is written
//! to be read: canonicalise first, compare after, and refuse anything that does
//! not end up inside a configured share.
//!
//! Canonicalising **before** the comparison is the point. A textual check
//! against `..` is not enough — a symlink inside a share pointing at `/` passes
//! every string test ever written, and `/srv/share/../../etc` is a perfectly
//! ordinary path until something resolves it.

use std::path::{Path, PathBuf};

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

/// The folders this device shares, in the order the user listed them.
#[derive(Debug, Clone, Default)]
pub struct Shares {
    roots: Vec<PathBuf>,
}

impl Shares {
    /// Build from configured paths, keeping only those that exist and resolve.
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
        Shares {
            roots: paths
                .into_iter()
                .filter_map(|p| std::fs::canonicalize(p.as_ref()).ok())
                .collect(),
        }
    }

    /// Whether anything is shared at all.
    ///
    /// **The default is nothing.** A device with no configured share answers
    /// every request with an empty listing, however trusted the asker — sharing
    /// is something a user does deliberately, not a consequence of granting a
    /// permission.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// The shares themselves, as the top level a peer sees.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Resolve a peer-supplied path to a real one inside a share, or refuse.
    ///
    /// `rel` is interpreted relative to the share whose name it starts with, so
    /// a peer names `photos/2026` rather than `/home/someone/photos/2026` — a
    /// device's real filesystem layout is not a peer's business, and echoing it
    /// back would leak the home directory's name to anyone allowed to browse.
    ///
    /// An empty `rel` resolves to the list of shares themselves.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, ShareError> {
        let mut parts = rel.split('/').filter(|p| !p.is_empty());
        let Some(first) = parts.next() else {
            return Err(ShareError::NoShare);
        };
        let root = self
            .roots
            .iter()
            .find(|r| r.file_name().is_some_and(|n| n == first))
            .ok_or(ShareError::NoShare)?;

        let mut candidate = root.clone();
        for part in parts {
            candidate.push(part);
        }
        // Canonicalise, then compare. A symlink inside the share pointing
        // anywhere else is resolved here and refused below; a `..` that climbs
        // out is likewise resolved to what it actually means before anything
        // trusts it.
        let real = std::fs::canonicalize(&candidate).map_err(|_| ShareError::Missing)?;
        if !real.starts_with(root) {
            return Err(ShareError::Outside);
        }
        Ok(real)
    }
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
}
