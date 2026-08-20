//! The Browse channel's MessageHandler: decode → gate → list → answer.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use peerbeam_domain::entity::Permission;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;
use peerbeam_domain::session::{ChannelType, MessageHandler, SessionError, SessionFrame};

use crate::message::{Entry, ListRequest, ListResponse, MAX_ENTRIES, MSG_LIST_REQUEST};
use crate::share::Shares;

/// Sends one answer back to the asking peer.
pub type AnswerSink = Arc<dyn Fn(ListResponse) + Send + Sync>;

/// Delivers an answer *we* asked for to whoever is waiting on it.
///
/// Both directions travel the same channel, so one handler serves both roles:
/// a request is something to answer, a response is something we asked for. They
/// go to different places, and conflating them would have this device try to
/// serve its own answers back to the peer.
pub type IncomingSink = Arc<dyn Fn(ListResponse) + Send + Sync>;

/// Serves inbound Browse-channel frames for one session.
pub struct BrowseHandler {
    shares: Shares,
    trust: Arc<dyn TrustStore>,
    peer: Arc<OnceLock<DeviceId>>,
    answer: AnswerSink,
    incoming: IncomingSink,
}

impl BrowseHandler {
    #[must_use]
    pub fn new(
        shares: Shares,
        trust: Arc<dyn TrustStore>,
        answer: AnswerSink,
        incoming: IncomingSink,
    ) -> (Arc<BrowseHandler>, Arc<OnceLock<DeviceId>>) {
        let peer = Arc::new(OnceLock::new());
        let handler = Arc::new(BrowseHandler {
            shares,
            trust: trust.clone(),
            peer: peer.clone(),
            answer,
            incoming,
        });
        (handler, peer)
    }
}

#[async_trait]
impl MessageHandler for BrowseHandler {
    fn channel_type(&self) -> ChannelType {
        ChannelType::BROWSE
    }

    async fn handle(&self, frame: SessionFrame) -> Result<(), SessionError> {
        let Some(peer) = self.peer.get() else {
            return Err(SessionError::FrameDecode("browse peer not bound".into()));
        };
        // An answer to something *we* asked. Delivered to whoever is waiting,
        // never treated as a request — otherwise this device would answer its
        // own answers straight back at the peer.
        if frame.message_type.get() == crate::message::MSG_LIST_RESPONSE {
            if let Ok(r) = ListResponse::from_frame(&frame) {
                (self.incoming)(r);
            }
            return Ok(());
        }
        if frame.message_type.get() != MSG_LIST_REQUEST {
            // An unknown OPTIONAL type is skipped per MESSAGE_REGISTRY.md §6.
            return Ok(());
        }
        let req = ListRequest::from_frame(&frame)
            .map_err(|e| SessionError::FrameDecode(e.to_string()))?;

        // Two independent gates, and **the same empty answer whichever fails**.
        // A peer that may not browse, one asking about a path outside every
        // share, and one asking about something that does not exist must be
        // indistinguishable — otherwise an asker maps a filesystem it may not
        // see, one refused request at a time.
        if !self.trust.may(peer, Permission::Browse) {
            (self.answer)(ListResponse::denied(&req.path));
            return Ok(());
        }
        (self.answer)(list(&self.shares, &req.path));
        Ok(())
    }
}

/// Build the answer for one request.
///
/// Separated from the handler so the listing rules can be tested without a
/// session: what a peer is shown is as much a security decision as whether it
/// is answered at all.
#[must_use]
pub fn list(shares: &Shares, path: &str) -> ListResponse {
    // Empty path means "what do you share?" — the share names only, never the
    // paths they live at.
    if path.split('/').find(|p| !p.is_empty()).is_none() {
        // The names `Shares::new` assigned, so every share is listed under the
        // name `resolve` will actually accept. Deriving them here from
        // `root.file_name()` instead listed two folders called `Documents` under
        // one name — only one of which could be opened — and dropped a root with
        // no basename from the listing altogether.
        let entries: Vec<Entry> = shares
            .shares()
            .iter()
            .map(|s| Entry {
                name: s.name.clone(),
                is_dir: true,
                size: 0,
            })
            .collect();
        return ListResponse {
            path: path.to_string(),
            entries,
            truncated: false,
            denied: false,
        };
    }

    let Ok(real) = shares.resolve(path) else {
        return ListResponse::denied(path);
    };
    let Ok(dir) = std::fs::read_dir(&real) else {
        // A file rather than a directory, or one this process cannot read.
        // Same empty answer: which of those it is, is not the asker's business.
        return ListResponse::denied(path);
    };

    let mut entries: Vec<Entry> = Vec::new();
    let mut truncated = false;
    for e in dir.flatten() {
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(meta) = e.metadata() else { continue };
        // A symlink's *target* metadata is what `metadata()` reports, and
        // `resolve` will refuse to descend into one that leaves the share — so
        // a link out is visible as a name but leads nowhere.
        entries.push(Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
        });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    ListResponse {
        path: path.to_string(),
        entries,
        truncated,
        denied: false,
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
        std::fs::write(dir.path().join("secret.txt"), b"nope").unwrap();
        (dir, Shares::new([share]))
    }

    use peerbeam_domain::entity::{PermissionSet, TrustRecord};
    use peerbeam_domain::session::ChannelId;
    use std::sync::Mutex;

    struct Trust(Option<TrustRecord>);

    impl TrustStore for Trust {
        fn record(&self, _record: TrustRecord) -> peerbeam_domain::Result<()> {
            Ok(())
        }
        fn lookup(&self, _device: &DeviceId) -> peerbeam_domain::Result<Option<TrustRecord>> {
            Ok(self.0.clone())
        }
        fn is_trusted(&self, _device: &DeviceId) -> bool {
            self.0.is_some()
        }
    }

    fn trust(browse: bool) -> Arc<dyn TrustStore> {
        Arc::new(Trust(Some(TrustRecord {
            device: DeviceId::from("pb-bob"),
            fingerprint: "ff".into(),
            name: "Bob".into(),
            trusted_at: chrono::Utc::now(),
            approved: true,
            permissions: PermissionSet::granted_on_approval().set(Permission::Browse, browse),
            expires_at: None,
            mine: false,
        })))
    }

    type Answers = Arc<Mutex<Vec<ListResponse>>>;

    fn handler(shares: Shares, trust: Arc<dyn TrustStore>) -> (Arc<BrowseHandler>, Answers) {
        let answers: Answers = Arc::new(Mutex::new(Vec::new()));
        let seen = answers.clone();
        let (h, slot) = BrowseHandler::new(
            shares,
            trust,
            Arc::new(move |r| seen.lock().unwrap().push(r)),
            Arc::new(|_| {}),
        );
        let _ = slot.set(DeviceId::from("pb-bob"));
        (h, answers)
    }

    /// **The gate this feature rests on.** A device the user never granted
    /// `browse` learns nothing — not the share names, not that shares exist.
    #[tokio::test]
    async fn a_peer_without_the_permission_is_told_nothing() {
        let (_dir, shares) = tree();
        let (h, answers) = handler(shares, trust(false));

        h.handle(
            ListRequest::new("share")
                .to_frame(ChannelId::new(1))
                .unwrap(),
        )
        .await
        .unwrap();

        let got = answers.lock().unwrap();
        assert_eq!(got.len(), 1, "the asker was left waiting");
        assert!(got[0].entries.is_empty(), "an ungranted peer saw a listing");
        assert!(got[0].denied);
    }

    /// And the refusal is **indistinguishable** from a path that does not
    /// exist: an asker must not be able to tell "you may not" from "there is
    /// nothing", or it can map the filesystem one request at a time.
    #[tokio::test]
    async fn a_refusal_looks_exactly_like_an_absent_path() {
        let (_dir, shares) = tree();

        let (denied_h, denied) = handler(shares.clone(), trust(false));
        denied_h
            .handle(
                ListRequest::new("share")
                    .to_frame(ChannelId::new(1))
                    .unwrap(),
            )
            .await
            .unwrap();

        let (allowed_h, missing) = handler(shares, trust(true));
        allowed_h
            .handle(
                ListRequest::new("share/not-here")
                    .to_frame(ChannelId::new(1))
                    .unwrap(),
            )
            .await
            .unwrap();

        let a = &denied.lock().unwrap()[0];
        let b = &missing.lock().unwrap()[0];
        assert_eq!(a.entries, b.entries);
        assert_eq!(a.denied, b.denied);
        assert_eq!(a.truncated, b.truncated);
    }

    #[tokio::test]
    async fn a_permitted_peer_sees_the_listing() {
        let (_dir, shares) = tree();
        let (h, answers) = handler(shares, trust(true));

        h.handle(
            ListRequest::new("share")
                .to_frame(ChannelId::new(1))
                .unwrap(),
        )
        .await
        .unwrap();

        assert!(!answers.lock().unwrap()[0].entries.is_empty());
    }

    #[test]
    fn an_empty_path_lists_the_share_names_and_not_their_locations() {
        // A device's real filesystem layout is not the asker's business, and
        // echoing it would leak the home directory's name to anyone allowed to
        // browse.
        let (dir, shares) = tree();
        let out = list(&shares, "");
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.entries[0].name, "share");
        let dumped = serde_json::to_string(&out).unwrap();
        assert!(
            !dumped.contains(&dir.path().to_string_lossy().into_owned()),
            "the listing leaked an absolute path"
        );
    }

    #[test]
    fn a_share_lists_its_contents_directories_first() {
        let (_dir, shares) = tree();
        let out = list(&shares, "share");
        assert!(!out.denied);
        assert_eq!(out.entries[0].name, "sub");
        assert!(out.entries[0].is_dir);
        assert_eq!(out.entries[1].name, "a.txt");
        assert_eq!(out.entries[1].size, 5);
    }

    /// Every reason for "nothing" is the same answer.
    #[test]
    fn escaping_missing_and_unshared_paths_are_indistinguishable() {
        let (_dir, shares) = tree();
        let escaped = list(&shares, "share/../secret.txt");
        let missing = list(&shares, "share/not-here");
        let unshared = list(&shares, "elsewhere");
        for out in [&escaped, &missing, &unshared] {
            assert!(out.denied);
            assert!(out.entries.is_empty());
        }
        // Byte-identical but for the echoed path, which the asker already knew.
        assert_eq!(escaped.entries, missing.entries);
        assert_eq!(missing.entries, unshared.entries);
    }

    #[test]
    fn a_device_sharing_nothing_lists_nothing() {
        let shares = Shares::new(Vec::<std::path::PathBuf>::new());
        assert!(list(&shares, "").entries.is_empty());
        assert!(list(&shares, "anything").denied);
    }

    /// **Every listed name must open.** Two folders with the same basename were
    /// listed twice under one name, so a peer that clicked the second row got
    /// the first folder's contents and the second share was unreachable.
    #[test]
    fn two_shares_with_the_same_basename_are_listed_under_names_that_both_open() {
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

        let top = list(&shares, "");
        let names: Vec<&str> = top.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names.len(), 2, "both shares are offered");
        assert_eq!(
            names.iter().collect::<std::collections::HashSet<_>>().len(),
            2,
            "under two different names"
        );

        // And each name lists the file only *its* folder holds.
        let first = list(&shares, names[0]);
        let second = list(&shares, names[1]);
        assert_eq!(
            first
                .entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["mine.txt"]
        );
        assert_eq!(
            second
                .entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["theirs.txt"]
        );
    }
}
