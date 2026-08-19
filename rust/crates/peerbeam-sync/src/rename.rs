//! Spotting a rename instead of re-sending the file.
//!
//! A rename looks exactly like a delete plus a create: one path disappears, an
//! identical one appears elsewhere. Treated literally that costs a full
//! re-transfer of a file both sides already have, and — worse on a slow link —
//! a window where the receiver has deleted its copy and not yet received the
//! replacement.
//!
//! # What counts as a rename
//!
//! **Identical content, and nothing else will do.** Same size is not enough
//! (two different files of the same length are common), and same name is
//! irrelevant — that is the thing that changed. So a rename is a deletion and a
//! creation in the same scan whose content hashes match.
//!
//! # Why pairs are matched conservatively
//!
//! If two files with the same content are deleted and two identical ones
//! appear, there is no way to tell which became which — and it does not matter,
//! because the content is the same either way. But if the counts *disagree*,
//! guessing would be wrong, so only the pairs that can be matched are reported
//! and the remainder fall back to ordinary deletes and creates.

use std::collections::BTreeMap;

/// A file that vanished and reappeared under another name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub from: String,
    pub to: String,
    /// The content hash both paths share.
    pub hash: String,
}

/// Pair deletions with creations that carry the same content.
///
/// `deleted` and `created` are `(path, content_hash)`. Returns the renames, and
/// the paths left over as genuine deletions and creations.
#[must_use]
pub fn detect(
    deleted: &[(String, String)],
    created: &[(String, String)],
) -> (Vec<Rename>, Vec<String>, Vec<String>) {
    let mut by_hash: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (path, hash) in created {
        by_hash
            .entry(hash.as_str())
            .or_default()
            .push(path.as_str());
    }

    let mut renames = Vec::new();
    let mut still_deleted = Vec::new();
    let mut claimed: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for (path, hash) in deleted {
        // `pop` takes the last unclaimed creation with this content. Which one
        // is arbitrary and that is fine — the content is identical, so any
        // pairing produces the same bytes on disk. What matters is that each
        // creation is claimed at most once.
        match by_hash.get_mut(hash.as_str()).and_then(Vec::pop) {
            Some(to) => {
                claimed.insert(to);
                renames.push(Rename {
                    from: path.clone(),
                    to: to.to_string(),
                    hash: hash.clone(),
                });
            }
            None => still_deleted.push(path.clone()),
        }
    }

    let still_created = created
        .iter()
        .map(|(p, _)| p)
        .filter(|p| !claimed.contains(p.as_str()))
        .cloned()
        .collect();

    (renames, still_deleted, still_created)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
            .collect()
    }

    #[test]
    fn a_deletion_and_a_creation_with_the_same_content_is_a_rename() {
        let (renames, deleted, created) =
            detect(&pairs(&[("old.txt", "aaa")]), &pairs(&[("new.txt", "aaa")]));
        assert_eq!(
            renames,
            vec![Rename {
                from: "old.txt".into(),
                to: "new.txt".into(),
                hash: "aaa".into(),
            }]
        );
        assert!(deleted.is_empty());
        assert!(created.is_empty());
    }

    /// **Same size is not same content.** Pairing on anything but the hash
    /// would silently swap two unrelated files.
    #[test]
    fn different_content_is_never_paired() {
        let (renames, deleted, created) =
            detect(&pairs(&[("old.txt", "aaa")]), &pairs(&[("new.txt", "bbb")]));
        assert!(renames.is_empty(), "unrelated files were paired");
        assert_eq!(deleted, vec!["old.txt"]);
        assert_eq!(created, vec!["new.txt"]);
    }

    #[test]
    fn a_move_into_a_subdirectory_is_still_a_rename() {
        let (renames, _, _) = detect(
            &pairs(&[("a.txt", "aaa")]),
            &pairs(&[("archive/2026/a.txt", "aaa")]),
        );
        assert_eq!(renames.len(), 1);
        assert_eq!(renames[0].to, "archive/2026/a.txt");
    }

    #[test]
    fn a_plain_deletion_stays_a_deletion() {
        let (renames, deleted, _) = detect(&pairs(&[("gone.txt", "aaa")]), &[]);
        assert!(renames.is_empty());
        assert_eq!(deleted, vec!["gone.txt"]);
    }

    #[test]
    fn a_plain_creation_stays_a_creation() {
        let (renames, _, created) = detect(&[], &pairs(&[("fresh.txt", "aaa")]));
        assert!(renames.is_empty());
        assert_eq!(created, vec!["fresh.txt"]);
    }

    /// A copy is not a rename: the original is still there, so nothing was
    /// deleted and there is nothing to pair with.
    #[test]
    fn copying_a_file_is_not_a_rename() {
        let (renames, deleted, created) = detect(&[], &pairs(&[("copy.txt", "aaa")]));
        assert!(renames.is_empty());
        assert!(deleted.is_empty());
        assert_eq!(created, vec!["copy.txt"]);
    }

    /// **Each creation is claimed at most once.** Two deletions must not both
    /// map onto the same new path, or one file would be reported as having
    /// moved somewhere it did not.
    #[test]
    fn two_deletions_cannot_claim_the_same_creation() {
        let (renames, deleted, created) = detect(
            &pairs(&[("a.txt", "same"), ("b.txt", "same")]),
            &pairs(&[("c.txt", "same")]),
        );
        assert_eq!(renames.len(), 1, "a creation was claimed twice");
        assert_eq!(deleted.len(), 1, "the unmatched deletion was lost");
        assert!(created.is_empty());
    }

    #[test]
    fn surplus_creations_remain_creations() {
        let (renames, deleted, created) = detect(
            &pairs(&[("a.txt", "same")]),
            &pairs(&[("b.txt", "same"), ("c.txt", "same")]),
        );
        assert_eq!(renames.len(), 1);
        assert!(deleted.is_empty());
        assert_eq!(created.len(), 1, "the surplus creation was dropped");
    }

    /// Nothing may be silently lost: every input path has to come back as a
    /// rename, a deletion or a creation.
    #[test]
    fn every_path_is_accounted_for() {
        let deleted_in = pairs(&[("a", "1"), ("b", "2"), ("c", "3")]);
        let created_in = pairs(&[("x", "1"), ("y", "9")]);
        let (renames, deleted, created) = detect(&deleted_in, &created_in);

        let mut seen_deleted: Vec<&str> = renames.iter().map(|r| r.from.as_str()).collect();
        seen_deleted.extend(deleted.iter().map(String::as_str));
        seen_deleted.sort_unstable();
        assert_eq!(seen_deleted, vec!["a", "b", "c"]);

        let mut seen_created: Vec<&str> = renames.iter().map(|r| r.to.as_str()).collect();
        seen_created.extend(created.iter().map(String::as_str));
        seen_created.sort_unstable();
        assert_eq!(seen_created, vec!["x", "y"]);
    }
}
