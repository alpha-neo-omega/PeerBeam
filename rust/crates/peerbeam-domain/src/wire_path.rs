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
/// `.` and `..` segments are dropped, so a relative path can never climb out of
/// `root` however it was built.
#[must_use]
pub fn local_path(root: &Path, rel: &str) -> std::path::PathBuf {
    let mut out = root.to_path_buf();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            continue;
        }
        out.push(seg);
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
