//! One separator on the wire, whatever the host uses.
//!
//! Every share-relative path this project sends — sync manifests, chunk-map
//! requests, browse listings — is `/`-separated, because [`Shares::resolve`]
//! splits on `/` to walk into a share. A `Path` rendered with
//! `to_string_lossy` uses the *host* separator instead, so a Windows device
//! would put `photos\june.jpg` on the wire while every other device calls the
//! same file `photos/june.jpg`. The peer then fails to resolve it, and folder
//! sync between Windows and anything else silently loses every file below the
//! top level.
//!
//! [`Shares::resolve`]: https://example.invalid

use std::path::Path;

/// Render a **relative** path the way the protocol writes it.
///
/// Joins the path's own components with `/`, rather than replacing `\` in the
/// rendered string. The difference matters on Unix, where a backslash is a
/// legal character in a filename: a file genuinely called `a\b.txt` would be
/// turned into the two-segment path `a/b.txt` by a blind replace, inventing a
/// directory that does not exist.
///
/// Absolute paths are not expected here — this describes a location *inside* a
/// share — and any root or prefix component is dropped, which keeps a host path
/// from leaking to a peer even if one is passed by mistake.
#[must_use]
pub fn wire_path(rel: &Path) -> String {
    use std::path::Component;
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy()),
            // `.` carries no meaning in a wire path, and `..` must never travel
            // in one — a peer that accepted it could be walked out of a share.
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Turn a `/`-separated **wire** path into a native path under `root`.
///
/// The exact inverse of [`wire_path`], and needed for the same reason read the
/// other way: a local path is a location on *this* machine, so it must use this
/// machine's separator. Gluing segments on with `format!("{root}/{rel}")` yields
/// `C:\Users\me\recv/photo.jpg` on Windows — which opens, because Windows
/// accepts both, but is what gets stored as the file's `local_path` and shown
/// as the tap-to-open target. A consumer that splits on `\` sees one segment
/// where there are two.
///
/// # Containment
///
/// Every segment must be exactly one ordinary file name. Anything else is
/// dropped, so a path built by a peer lands inside `root` or nowhere.
///
/// **Checking for the literal strings `.` and `..` was not enough, and on
/// Windows it was a remote write.** `rel` arrives `/`-separated from a peer, so
/// `..\..\x` is a *single* segment: it is neither `.` nor `..`, and
/// `PathBuf::push` then expands it using the host's separator, climbing two
/// levels out of the sync root. Worse, `push` **replaces** the whole path when
/// given something rooted — `C:\evil` and `\evil` discard `root` entirely and
/// write wherever the peer named, with this process's privileges.
///
/// Splitting on `\` as well would fix that and break something else: a
/// backslash is a legal character in a Unix filename, so a file genuinely
/// called `a\b.txt` would become the two-segment path `a/b.txt`, inventing a
/// directory — the same trap [`wire_path`] documents. Asking `Path` what a
/// segment actually *is* answers both: on Windows `..\..\x` yields several
/// components and is refused, while on Unix `a\b.txt` is one ordinary name and
/// is kept.
#[must_use]
pub fn local_path(root: &Path, rel: &str) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = root.to_path_buf();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            continue;
        }
        // Exactly one `Normal` component, or nothing. This is what rejects a
        // drive prefix (`C:`), a root (`\evil`), a UNC share (`\\host\s`)
        // and any embedded `..` on the platform where they mean something.
        let mut parts = Path::new(seg).components();
        match (parts.next(), parts.next()) {
            (Some(Component::Normal(name)), None) => out.push(name),
            _ => continue,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_nested_path_uses_forward_slashes() {
        let p: PathBuf = ["photos", "june", "a.jpg"].iter().collect();
        assert_eq!(wire_path(&p), "photos/june/a.jpg");
    }

    #[test]
    fn a_top_level_file_is_just_its_name() {
        assert_eq!(wire_path(Path::new("a.txt")), "a.txt");
    }

    #[test]
    fn an_empty_path_is_empty() {
        assert_eq!(wire_path(Path::new("")), "");
    }

    /// **Why components, not a string replace.** On Unix a backslash is a legal
    /// filename character, and `"a\\b.txt".replace('\\', "/")` would invent a
    /// directory that does not exist.
    #[cfg(unix)]
    #[test]
    fn a_backslash_in_a_unix_filename_is_not_a_separator() {
        assert_eq!(wire_path(Path::new("a\\b.txt")), "a\\b.txt");
    }

    /// A `..` must never reach a peer: one that resolved it could be walked out
    /// of the share it was serving.
    #[test]
    fn a_parent_component_is_dropped_rather_than_sent() {
        let p: PathBuf = ["photos", "..", "secrets.txt"].iter().collect();
        assert_eq!(wire_path(&p), "photos/secrets.txt");
    }

    #[test]
    fn a_current_directory_component_is_dropped() {
        let p: PathBuf = ["photos", ".", "a.jpg"].iter().collect();
        assert_eq!(wire_path(&p), "photos/a.jpg");
    }

    /// An absolute path should never be passed here, but if one is, no host
    /// location leaks to the peer.
    #[test]
    fn local_path_uses_the_platform_separator() {
        let got = local_path(Path::new("root"), "a/b/c.txt");
        let want: PathBuf = ["root", "a", "b", "c.txt"].iter().collect();
        assert_eq!(got, want);
    }

    #[test]
    fn local_path_round_trips_with_wire_path() {
        let rel = "photos/june/a.jpg";
        let joined = local_path(Path::new("root"), rel);
        let back = wire_path(joined.strip_prefix("root").unwrap());
        assert_eq!(back, rel, "the two must be exact inverses");
    }

    #[test]
    fn local_path_cannot_climb_out_of_its_root() {
        let got = local_path(Path::new("root"), "../../etc/passwd");
        let want: PathBuf = ["root", "etc", "passwd"].iter().collect();
        assert_eq!(got, want);
    }

    /// **The Windows remote-write.** `rel` comes from a peer and is
    /// `/`-separated, so each of these is a *single* segment: neither `.` nor
    /// `..`, and so pushed verbatim by the old code. On Windows `push` then
    /// reads the backslashes as separators — and for a rooted segment discards
    /// `root` altogether — so a peer could name any location on the disk.
    ///
    /// Asserted structurally rather than per-platform so Linux CI catches a
    /// regression too: whatever the host, the result must stay under `root` and
    /// must never contain a `..` component.
    #[test]
    fn a_peer_supplied_path_can_never_leave_the_root() {
        use std::path::Component;
        let root = Path::new("root");
        for hostile in [
            r"..\..\etc\passwd",
            r"C:\evil.txt",
            r"\evil.txt",
            r"\\host\share\evil.txt",
            r"..\..\..\..\..\..\Windows\System32\drivers\etc\hosts",
            "a/..\\..\\b",
        ] {
            let got = local_path(root, hostile);
            assert!(
                got.starts_with(root),
                "{hostile:?} escaped the root entirely: {got:?}"
            );
            assert!(
                !got.components().any(|c| c == Component::ParentDir),
                "{hostile:?} kept a `..` that the filesystem would follow: {got:?}"
            );
        }
    }

    /// A drive-rooted segment is dropped, not pushed — `PathBuf::push` replaces
    /// the whole path when given one, which is how `root` was lost.
    ///
    /// Windows only, and that is the point: on Unix `C:\evil` is not rooted at
    /// all, just an oddly named file, and keeping it under `root` is correct.
    /// The containment property that must hold *everywhere* is asserted by
    /// `a_peer_supplied_path_can_never_leave_the_root`.
    #[cfg(windows)]
    #[test]
    fn a_rooted_segment_contributes_nothing() {
        assert_eq!(local_path(Path::new("root"), r"C:\evil"), Path::new("root"));
    }

    /// The Unix half of the same decision: a backslash is a legal character in
    /// a filename there, so `a\b.txt` is one ordinary name and must survive.
    /// Splitting on `\` instead of asking `Path` would have invented a
    /// directory here.
    #[cfg(unix)]
    #[test]
    fn a_unix_filename_containing_a_backslash_is_kept_whole() {
        let got = local_path(Path::new("root"), "a\\b.txt");
        assert_eq!(got, Path::new("root").join("a\\b.txt"));
    }

    #[test]
    fn local_path_tolerates_empty_and_dot_segments() {
        let got = local_path(Path::new("root"), "a//./b");
        let want: PathBuf = ["root", "a", "b"].iter().collect();
        assert_eq!(got, want);
    }

    #[cfg(unix)]
    #[test]
    fn an_absolute_path_loses_its_root() {
        assert_eq!(wire_path(Path::new("/etc/passwd")), "etc/passwd");
    }
}
