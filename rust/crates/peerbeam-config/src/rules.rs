//! Rules-based auto-save: **where** an accepted item is written.
//!
//! # The line this module does not cross
//!
//! A rule decides **where** a received item is saved. A rule never decides
//! **whether** it is accepted.
//!
//! Nothing here is reachable from the approval path, and nothing here can be:
//! a rule carries no verdict, no "accept" action and no peer-facing state, and
//! [`destination`] is called only *after* a transfer has been accepted and is
//! about to be written. Auto-accept ([`crate::DeviceConfig::auto_accept_trusted`])
//! is a separate, pre-existing setting, gated on an explicitly approved device,
//! and this module neither reads nor influences it. That separation is I6: the
//! approval gate's decision logic is exactly what it was before rules existed.
//!
//! If a future change wants a rule field that affects acceptance, it needs a
//! constitutional amendment, not a new `Option<bool>` here.
//!
//! # Order is the tie-break
//!
//! Rules are an **ordered list** and the **first match wins**. There is
//! deliberately no specificity score: a user who can reorder a list can predict
//! the outcome, whereas a ranking function they cannot see turns "why did my
//! file go there?" into a support question. Reordering the list is the
//! supported way to change which of two overlapping rules applies.
//!
//! # An omitted criterion matches everything
//!
//! Each of [`SaveRule::device`], [`SaveRule::extension`],
//! [`SaveRule::min_bytes`] and [`SaveRule::max_bytes`] is optional, and `None`
//! means "do not test this". A rule with no criteria at all therefore matches
//! every item — a legitimate "catch-all, put it here" rule, not a mistake.
//!
//! # Platforms
//!
//! A rule's destination is an absolute filesystem path, so rules apply on
//! **desktop and headless** only. Android receives into a SAF-granted location
//! and cannot write arbitrary absolute paths; its surface says so rather than
//! offering a list that silently does nothing (I12: document the limit, don't
//! weaken the design for everyone).

use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One auto-save rule: a **match**, and a **destination**.
///
/// Every criterion is optional and an omitted one matches everything, so the
/// default value — all criteria `None`, empty directory — is a catch-all whose
/// only invalid part is the directory. [`SaveRule::validate`] is what refuses
/// that, and it is called before a rule is ever stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SaveRule {
    /// The sending device, by its **authenticated device id** (`pb-…`) — the
    /// id established by the mutual-auth handshake, never the peer's
    /// self-reported name.
    ///
    /// A name is peer-supplied and a peer may present any name it likes, so
    /// matching on one would let a stranger claim another device's rule simply
    /// by calling itself "laptop". Matching is exact and case-sensitive; ids
    /// are generated, not typed.
    pub device: Option<String>,
    /// File extension, without the leading dot, matched **case-insensitively**
    /// against the *sanitised* name (see [`SaveRule::matches`]). A leading dot
    /// is tolerated and ignored, so `pdf` and `.pdf` mean the same thing.
    pub extension: Option<String>,
    /// Smallest size that matches, in bytes. **Inclusive.**
    pub min_bytes: Option<u64>,
    /// Largest size that matches, in bytes. **Inclusive.**
    pub max_bytes: Option<u64>,
    /// The absolute directory a matching item is written to.
    ///
    /// Supplied entirely by the rule: the peer never contributes any part of
    /// it. The file's own name still goes through the receive path's existing
    /// `sanitize_file_name`, so neither half of the final path is
    /// peer-controlled.
    pub directory: String,
}

/// Why a rule was refused when it was added.
///
/// Every variant is something the person adding the rule can fix, which is the
/// whole reason validation happens at add time rather than only at save time.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuleError {
    /// The destination is empty or only whitespace.
    #[error("a rule needs a destination directory")]
    EmptyDirectory,
    /// The destination is not an absolute path.
    #[error("destination must be an absolute path: {0}")]
    NotAbsolute(String),
    /// The destination contains a `..` component.
    #[error("destination must not contain `..`: {0}")]
    ParentTraversal(String),
    /// The destination's parent directory does not exist.
    #[error("destination's parent directory does not exist: {0}")]
    MissingParent(String),
    /// A criterion was present but blank, which matches nothing.
    #[error("`{0}` was given but is empty — omit it to match everything")]
    BlankCriterion(&'static str),
    /// `min_bytes` is greater than `max_bytes`, so the rule can never match.
    #[error("size range {min}–{max} bytes can never match")]
    ImpossibleRange {
        /// The lower bound.
        min: u64,
        /// The upper bound, which is below it.
        max: u64,
    },
}

impl SaveRule {
    /// Does this rule apply to an item from `device`, named `name`, of `size`
    /// bytes?
    ///
    /// `name` must be the **sanitised** single-component file name the receive
    /// path will actually write — the same value `sanitize_file_name` produced
    /// — not the raw name off the wire. Matching the wire name would let a
    /// peer aim a rule with a name the receiver never writes.
    ///
    /// Every criterion that is `Some` must hold; every criterion that is
    /// `None` is not tested. All-`None` therefore matches everything.
    #[must_use]
    pub fn matches(&self, device: &str, name: &str, size: u64) -> bool {
        if let Some(want) = &self.device {
            if want != device {
                return false;
            }
        }
        if let Some(want) = self.wanted_extension() {
            match extension_of(name) {
                Some(have) if have.eq_ignore_ascii_case(want) => {}
                _ => return false,
            }
        }
        if let Some(min) = self.min_bytes {
            if size < min {
                return false;
            }
        }
        if let Some(max) = self.max_bytes {
            if size > max {
                return false;
            }
        }
        true
    }

    /// Check everything about this rule that can be checked without receiving
    /// anything, so the problem is reported while the user is still looking at
    /// the rule they just wrote.
    ///
    /// The destination must be **absolute**, must contain no `..` component,
    /// and its **parent must already exist**. Requiring the parent rather than
    /// the directory itself is deliberate: creating one missing leaf on first
    /// use is a convenience, while creating a whole missing tree would happily
    /// manufacture `/mnt/nas/videos` on the local root when the NAS is not
    /// mounted, and the user would never notice.
    ///
    /// A destination that passes here can still fail later — a disk fills, a
    /// mount goes away, permissions change — which is why [`destination`] also
    /// falls back and says so. Validation reduces that to the unforeseeable
    /// cases; it does not pretend to eliminate them.
    pub fn validate(&self) -> Result<(), RuleError> {
        if let Some(d) = &self.device {
            if d.trim().is_empty() {
                return Err(RuleError::BlankCriterion("device"));
            }
        }
        if let Some(e) = &self.extension {
            if e.trim_start_matches('.').trim().is_empty() {
                return Err(RuleError::BlankCriterion("extension"));
            }
        }
        if let (Some(min), Some(max)) = (self.min_bytes, self.max_bytes) {
            if min > max {
                return Err(RuleError::ImpossibleRange { min, max });
            }
        }

        let dir = self.directory.trim();
        if dir.is_empty() {
            return Err(RuleError::EmptyDirectory);
        }
        let path = Path::new(dir);
        if !path.is_absolute() {
            return Err(RuleError::NotAbsolute(dir.to_string()));
        }
        // Checked on components, not on the string: a substring search would
        // reject the perfectly good `/home/me/..dotfiles` and miss nothing a
        // component walk misses.
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(RuleError::ParentTraversal(dir.to_string()));
        }
        match path.parent() {
            // A filesystem root has no parent; it is its own existence check.
            None => {
                if !path.is_dir() {
                    return Err(RuleError::MissingParent(dir.to_string()));
                }
            }
            Some(parent) if parent.as_os_str().is_empty() || parent.is_dir() => {}
            Some(parent) => return Err(RuleError::MissingParent(parent.display().to_string())),
        }
        Ok(())
    }

    /// The extension criterion with any leading dot removed, or `None` when
    /// there is no extension criterion.
    fn wanted_extension(&self) -> Option<&str> {
        self.extension
            .as_deref()
            .map(|e| e.trim().trim_start_matches('.'))
            .filter(|e| !e.is_empty())
    }
}

/// The extension of a sanitised file name, or `None` when it has none.
///
/// Uses [`Path::extension`], so a dotfile like `.bashrc` has **no** extension
/// (it is a name, not a suffix) and `archive.tar.gz` has `gz` — matching what
/// a user means when they write a rule for `gz`.
fn extension_of(name: &str) -> Option<&str> {
    Path::new(name).extension().and_then(|e| e.to_str())
}

/// **The matcher.** The first rule that matches, or `None` when none does.
///
/// This is the whole decision, as one pure function of its inputs: the
/// authenticated sender id, the sanitised file name, and the size. It performs
/// no IO, consults no global state, and is therefore exhaustively table-testable
/// — which is the point, since a scattered set of `if`s in the receive path is
/// how "where did my file go?" becomes unanswerable.
///
/// `None` means the caller uses its existing save directory, unchanged. That is
/// the guarantee for every user who defines no rules: nothing about their
/// receive path moves.
#[must_use]
pub fn matching_directory<'a>(
    rules: &'a [SaveRule],
    device: &str,
    name: &str,
    size: u64,
) -> Option<&'a str> {
    rules
        .iter()
        .find(|r| r.matches(device, name, size))
        .map(|r| r.directory.trim())
}

/// A rule's destination that could not be used, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fallback {
    /// The directory the matching rule named.
    pub rule_directory: String,
    /// Why it could not be written to, in the words the OS used.
    pub reason: String,
}

/// Where an accepted item will actually be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    /// The directory to hand the receive path.
    pub directory: String,
    /// `Some` when a rule matched but its destination could not be used, so
    /// [`Destination::directory`] is the save directory instead.
    ///
    /// A caller that has this **must report it**. A file landing somewhere
    /// other than where the rules said, silently, is worse than having no
    /// rules at all: the user believes a sort happened that did not.
    pub fallback: Option<Fallback>,
}

impl Destination {
    /// A destination with no rule involved — today's save directory.
    fn plain(save_directory: &str) -> Self {
        Self {
            directory: save_directory.to_string(),
            fallback: None,
        }
    }
}

/// Resolve where an accepted item lands: [`matching_directory`], then a check
/// that the chosen directory can actually be written to, falling back to
/// `save_directory` if it cannot.
///
/// The check is the only IO in this module, and it exists because the
/// alternative is losing the file. Without it a rule pointing at a read-only
/// or vanished directory does not merely mis-file the item — the write fails
/// and the whole transfer fails, after the user already accepted it. Falling
/// back turns an unrecoverable failure into a recoverable surprise, and the
/// [`Destination::fallback`] the caller must report is what keeps it from being
/// a *silent* surprise.
///
/// The check both creates the directory (as the storage adapter would anyway)
/// and writes a probe file, because "the directory exists" and "this process
/// may write in it" are different questions and only the second one matters.
/// The probe is removed immediately; its name is recognisable if a crash ever
/// leaves one behind.
#[must_use]
pub fn destination(
    rules: &[SaveRule],
    save_directory: &str,
    device: &str,
    name: &str,
    size: u64,
) -> Destination {
    let Some(dir) = matching_directory(rules, device, name, size) else {
        return Destination::plain(save_directory);
    };
    match writable(dir) {
        Ok(()) => Destination {
            directory: dir.to_string(),
            fallback: None,
        },
        Err(reason) => Destination {
            directory: save_directory.to_string(),
            fallback: Some(Fallback {
                rule_directory: dir.to_string(),
                reason,
            }),
        },
    }
}

/// Can this process create files in `dir`? Returns the OS's own words on
/// failure, since they are what a user needs to fix it.
fn writable(dir: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let probe = {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        Path::new(dir).join(format!(".peerbeam-write-test.{}.{}", std::process::id(), n))
    };
    std::fs::File::create(&probe).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rule matching only on device.
    fn from_device(id: &str, dir: &str) -> SaveRule {
        SaveRule {
            device: Some(id.to_string()),
            directory: dir.to_string(),
            ..SaveRule::default()
        }
    }

    /// A rule matching only on extension.
    fn with_ext(ext: &str, dir: &str) -> SaveRule {
        SaveRule {
            extension: Some(ext.to_string()),
            directory: dir.to_string(),
            ..SaveRule::default()
        }
    }

    /// A rule with no criteria at all.
    fn catch_all(dir: &str) -> SaveRule {
        SaveRule {
            directory: dir.to_string(),
            ..SaveRule::default()
        }
    }

    // ── the tie-break ───────────────────────────────────────────────────────

    /// **First match wins**, and reordering the list changes the outcome.
    ///
    /// This is the whole ordering contract in one test. Turning
    /// `matching_directory`'s `find` into a last-match (`filter(...).last()`)
    /// must fail it.
    #[test]
    fn the_first_matching_rule_wins_and_reordering_changes_the_answer() {
        let by_device = from_device("pb-alice", "/srv/from-alice");
        let by_ext = with_ext("pdf", "/srv/pdfs");

        let device_first = vec![by_device.clone(), by_ext.clone()];
        assert_eq!(
            matching_directory(&device_first, "pb-alice", "report.pdf", 10),
            Some("/srv/from-alice"),
            "both rules match; the first one in the list must win"
        );

        let ext_first = vec![by_ext, by_device];
        assert_eq!(
            matching_directory(&ext_first, "pb-alice", "report.pdf", 10),
            Some("/srv/pdfs"),
            "the same two rules, reordered, must give the other answer"
        );
    }

    /// A catch-all placed first shadows everything after it — the predictable
    /// consequence of first-match-wins, and the reason ordering is the user's
    /// lever rather than a hidden specificity score.
    #[test]
    fn a_catch_all_first_shadows_every_later_rule() {
        let rules = vec![catch_all("/srv/everything"), with_ext("pdf", "/srv/pdfs")];
        assert_eq!(
            matching_directory(&rules, "pb-alice", "report.pdf", 10),
            Some("/srv/everything")
        );
    }

    // ── the "nothing changed" guarantee ─────────────────────────────────────

    /// **No rules → the save directory.** The guarantee for every existing
    /// user: define no rules and the receive path is exactly what it was.
    #[test]
    fn no_rules_means_the_save_directory() {
        assert_eq!(matching_directory(&[], "pb-alice", "report.pdf", 10), None);
        let d = destination(&[], "/home/me/Downloads", "pb-alice", "report.pdf", 10);
        assert_eq!(d.directory, "/home/me/Downloads");
        assert!(d.fallback.is_none());
    }

    /// **No *matching* rule → the save directory**, which is the other half of
    /// the same guarantee: a rule that does not apply must not divert anything.
    #[test]
    fn a_non_matching_rule_means_the_save_directory() {
        let rules = vec![with_ext("pdf", "/srv/pdfs")];
        assert_eq!(matching_directory(&rules, "pb-alice", "clip.mp4", 10), None);
        let d = destination(&rules, "/home/me/Downloads", "pb-alice", "clip.mp4", 10);
        assert_eq!(d.directory, "/home/me/Downloads");
        assert!(d.fallback.is_none());
    }

    // ── each criterion, independently ───────────────────────────────────────

    /// The device criterion matches its device and rejects every other.
    #[test]
    fn the_device_criterion_matches_and_rejects() {
        let rules = vec![from_device("pb-alice", "/srv/from-alice")];
        assert_eq!(
            matching_directory(&rules, "pb-alice", "x.bin", 1),
            Some("/srv/from-alice")
        );
        assert_eq!(matching_directory(&rules, "pb-bob", "x.bin", 1), None);
    }

    /// **Sender matching is by authenticated id, not by name.** A peer that
    /// presents the *name* of a device with a rule must not inherit that rule:
    /// a name is peer-supplied and free to claim. Making `matches` compare
    /// against a name must fail this.
    #[test]
    fn a_peer_presenting_another_devices_name_does_not_match_its_rule() {
        // Alice's device id, and a rule for it.
        let rules = vec![from_device("pb-alice", "/srv/from-alice")];

        // A stranger connects. Its authenticated id is its own; the only thing
        // it controls — the name it presents — is "alice".
        let impostor_id = "pb-impostor";
        let impostor_presented_name = "alice";

        assert_eq!(
            matching_directory(&rules, impostor_id, "x.bin", 1),
            None,
            "the id is what is matched, and it is not Alice's"
        );
        assert_eq!(
            matching_directory(&rules, impostor_presented_name, "x.bin", 1),
            None,
            "and the name the peer chose must not be a way in either"
        );
    }

    /// The extension criterion matches case-insensitively, tolerates a leading
    /// dot in the rule, and rejects a different extension or none at all.
    #[test]
    fn the_extension_criterion_matches_and_rejects() {
        let rules = vec![with_ext("pdf", "/srv/pdfs")];
        assert_eq!(
            matching_directory(&rules, "pb-a", "report.pdf", 1),
            Some("/srv/pdfs")
        );
        assert_eq!(
            matching_directory(&rules, "pb-a", "REPORT.PDF", 1),
            Some("/srv/pdfs"),
            "extensions are matched case-insensitively"
        );
        assert_eq!(matching_directory(&rules, "pb-a", "clip.mp4", 1), None);
        assert_eq!(
            matching_directory(&rules, "pb-a", "README", 1),
            None,
            "a name with no extension cannot match an extension rule"
        );

        let dotted = vec![with_ext(".pdf", "/srv/pdfs")];
        assert_eq!(
            matching_directory(&dotted, "pb-a", "report.pdf", 1),
            Some("/srv/pdfs"),
            "`.pdf` and `pdf` must mean the same thing"
        );
    }

    /// A dotfile is a name, not a suffix: `.bashrc` has no extension, so it
    /// must not be caught by a rule for `bashrc`.
    #[test]
    fn a_dotfile_has_no_extension() {
        assert_eq!(extension_of(".bashrc"), None);
        assert_eq!(extension_of("archive.tar.gz"), Some("gz"));
        assert_eq!(extension_of("README"), None);
    }

    /// The size range is inclusive at both ends and rejects outside it.
    #[test]
    fn the_size_range_is_inclusive_and_rejects_outside_it() {
        let rules = vec![SaveRule {
            min_bytes: Some(100),
            max_bytes: Some(200),
            directory: "/srv/mid".into(),
            ..SaveRule::default()
        }];
        assert_eq!(matching_directory(&rules, "pb-a", "x", 99), None);
        assert_eq!(
            matching_directory(&rules, "pb-a", "x", 100),
            Some("/srv/mid"),
            "min is inclusive"
        );
        assert_eq!(
            matching_directory(&rules, "pb-a", "x", 150),
            Some("/srv/mid")
        );
        assert_eq!(
            matching_directory(&rules, "pb-a", "x", 200),
            Some("/srv/mid"),
            "max is inclusive"
        );
        assert_eq!(matching_directory(&rules, "pb-a", "x", 201), None);
    }

    /// A one-sided range tests only the side that is set.
    #[test]
    fn a_one_sided_range_leaves_the_other_side_open() {
        let big = vec![SaveRule {
            min_bytes: Some(1_000),
            directory: "/srv/big".into(),
            ..SaveRule::default()
        }];
        assert_eq!(matching_directory(&big, "pb-a", "x", 999), None);
        assert_eq!(
            matching_directory(&big, "pb-a", "x", u64::MAX),
            Some("/srv/big")
        );

        let small = vec![SaveRule {
            max_bytes: Some(1_000),
            directory: "/srv/small".into(),
            ..SaveRule::default()
        }];
        assert_eq!(
            matching_directory(&small, "pb-a", "x", 0),
            Some("/srv/small")
        );
        assert_eq!(matching_directory(&small, "pb-a", "x", 1_001), None);
    }

    /// **A rule with no criteria is a catch-all**, not a no-op.
    #[test]
    fn a_rule_with_no_criteria_matches_everything() {
        let rules = vec![catch_all("/srv/inbox")];
        assert_eq!(
            matching_directory(&rules, "pb-anyone", "anything.xyz", 0),
            Some("/srv/inbox")
        );
        assert_eq!(
            matching_directory(&rules, "", "", u64::MAX),
            Some("/srv/inbox"),
            "even when nothing at all is known about the item"
        );
    }

    /// Criteria combine with AND: every one that is set must hold.
    #[test]
    fn combined_criteria_all_have_to_hold() {
        let rules = vec![SaveRule {
            device: Some("pb-alice".into()),
            extension: Some("mp4".into()),
            min_bytes: Some(1_000),
            max_bytes: Some(9_000),
            directory: "/srv/alice-videos".into(),
        }];
        assert_eq!(
            matching_directory(&rules, "pb-alice", "clip.mp4", 5_000),
            Some("/srv/alice-videos")
        );
        // Each one, broken in turn.
        assert_eq!(
            matching_directory(&rules, "pb-bob", "clip.mp4", 5_000),
            None
        );
        assert_eq!(
            matching_directory(&rules, "pb-alice", "clip.mkv", 5_000),
            None
        );
        assert_eq!(
            matching_directory(&rules, "pb-alice", "clip.mp4", 999),
            None
        );
        assert_eq!(
            matching_directory(&rules, "pb-alice", "clip.mp4", 9_001),
            None
        );
    }

    // ── validation ──────────────────────────────────────────────────────────

    #[test]
    fn validation_accepts_a_real_absolute_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("videos");
        let rule = catch_all(&dir.to_string_lossy());
        // The leaf need not exist yet — only its parent.
        assert_eq!(rule.validate(), Ok(()));
    }

    /// A relative destination is refused **at add time**, where the user can
    /// still fix it. "Relative to what?" has no answer in a daemon.
    #[test]
    fn validation_rejects_a_relative_path() {
        let rule = catch_all("videos/incoming");
        assert_eq!(
            rule.validate(),
            Err(RuleError::NotAbsolute("videos/incoming".into()))
        );
    }

    /// A `..` component is refused, checked on components rather than on the
    /// string so a legitimate name containing dots survives.
    #[test]
    fn validation_rejects_a_parent_traversal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sneaky = tmp.path().join("..").join("elsewhere");
        let rule = catch_all(&sneaky.to_string_lossy());
        assert!(matches!(
            rule.validate(),
            Err(RuleError::ParentTraversal(_))
        ));

        // …and a directory whose *name* merely contains dots is fine.
        let dotted = tmp.path().join("..dotfiles");
        assert_eq!(catch_all(&dotted.to_string_lossy()).validate(), Ok(()));
    }

    /// A destination whose parent does not exist is refused, so a typo in a
    /// path is caught while the user is looking at it.
    #[test]
    fn validation_rejects_a_missing_parent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested = tmp.path().join("no-such-parent").join("videos");
        let rule = catch_all(&nested.to_string_lossy());
        assert!(matches!(rule.validate(), Err(RuleError::MissingParent(_))));
    }

    #[test]
    fn validation_rejects_an_empty_destination() {
        assert_eq!(catch_all("").validate(), Err(RuleError::EmptyDirectory));
        assert_eq!(catch_all("   ").validate(), Err(RuleError::EmptyDirectory));
    }

    /// A criterion that is present but blank matches nothing, which is never
    /// what anyone meant — omitting it is how you say "match everything".
    #[test]
    fn validation_rejects_a_blank_criterion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();

        let mut rule = catch_all(&dir);
        rule.device = Some("  ".into());
        assert_eq!(rule.validate(), Err(RuleError::BlankCriterion("device")));

        let mut rule = catch_all(&dir);
        rule.extension = Some(".".into());
        assert_eq!(rule.validate(), Err(RuleError::BlankCriterion("extension")));
    }

    /// An inverted size range can never match, so it is refused rather than
    /// stored as a rule that silently does nothing.
    #[test]
    fn validation_rejects_an_impossible_size_range() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut rule = catch_all(&tmp.path().to_string_lossy());
        rule.min_bytes = Some(500);
        rule.max_bytes = Some(100);
        assert_eq!(
            rule.validate(),
            Err(RuleError::ImpossibleRange { min: 500, max: 100 })
        );
    }

    // ── the fallback ────────────────────────────────────────────────────────

    /// A usable destination is used, and reports no fallback.
    #[test]
    fn a_usable_destination_is_used() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("sorted");
        let save = tmp.path().join("downloads");
        std::fs::create_dir_all(&save).expect("save dir");

        let rules = vec![catch_all(&dest.to_string_lossy())];
        let d = destination(
            &rules,
            &save.to_string_lossy(),
            "pb-alice",
            "report.pdf",
            10,
        );
        assert_eq!(d.directory, dest.to_string_lossy());
        assert!(d.fallback.is_none());
        assert!(dest.is_dir(), "the destination is created on first use");
    }

    /// **The file must not be lost.** A destination that cannot be written to
    /// falls back to the save directory *and* reports which directory failed,
    /// so the surface can say so.
    #[test]
    fn an_unusable_destination_falls_back_and_reports() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let save = tmp.path().join("downloads");
        std::fs::create_dir_all(&save).expect("save dir");

        // A *file* where the rule expects a directory: `create_dir_all` cannot
        // make this work, which is precisely the "it broke after you added the
        // rule" case.
        let blocked = tmp.path().join("not-a-dir");
        std::fs::write(&blocked, b"in the way").expect("write blocker");

        let rules = vec![catch_all(&blocked.to_string_lossy())];
        let d = destination(
            &rules,
            &save.to_string_lossy(),
            "pb-alice",
            "report.pdf",
            10,
        );
        assert_eq!(
            d.directory,
            save.to_string_lossy(),
            "the item lands in the save directory rather than nowhere"
        );
        let fb = d
            .fallback
            .expect("the fallback must be reported, not silent");
        assert_eq!(fb.rule_directory, blocked.to_string_lossy());
        assert!(!fb.reason.is_empty(), "and it must say why");
    }

    /// A destination that exists but cannot be written to also falls back —
    /// `create_dir_all` succeeds on a read-only directory, so existence alone
    /// is not the question worth asking.
    #[cfg(unix)]
    #[test]
    fn a_read_only_destination_falls_back_too() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let save = tmp.path().join("downloads");
        std::fs::create_dir_all(&save).expect("save dir");
        let locked = tmp.path().join("locked");
        std::fs::create_dir_all(&locked).expect("locked dir");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500))
            .expect("chmod 500");

        // Running as root defeats permission bits entirely; skip rather than
        // assert something the environment cannot express.
        let probe = locked.join(".root-check");
        let is_root = std::fs::File::create(&probe).is_ok();
        let _ = std::fs::remove_file(&probe);
        if is_root {
            return;
        }

        let rules = vec![catch_all(&locked.to_string_lossy())];
        let d = destination(&rules, &save.to_string_lossy(), "pb-a", "x.bin", 1);
        assert_eq!(d.directory, save.to_string_lossy());
        assert!(d.fallback.is_some(), "a read-only directory must fall back");

        // Let the tempdir clean itself up.
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700));
    }

    /// The write probe leaves nothing behind.
    #[test]
    fn the_write_probe_cleans_up_after_itself() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("sorted");
        let rules = vec![catch_all(&dest.to_string_lossy())];
        let _ = destination(&rules, &tmp.path().to_string_lossy(), "pb-a", "x.bin", 1);

        let left: Vec<_> = std::fs::read_dir(&dest)
            .expect("read dest")
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        assert!(left.is_empty(), "probe files must not accumulate: {left:?}");
    }
}
