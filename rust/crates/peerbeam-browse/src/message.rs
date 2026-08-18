//! The Browse channel's two messages: ask what is in a folder, and answer.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use peerbeam_domain::session::{ChannelId, MessageFlags, MessageType, SessionFrame};

/// MessageType id for a listing request.
pub const MSG_LIST_REQUEST: u16 = 1;
/// MessageType id for a listing response.
pub const MSG_LIST_RESPONSE: u16 = 2;

/// Longest path a peer may ask about, in bytes.
pub const MAX_PATH: usize = 4096;
/// Most entries in one response.
///
/// A directory can hold a million files. Answering with all of them would make
/// one request cost the responder a million stat calls and the asker a frame it
/// has to buffer — so the answer is capped and says when it was.
pub const MAX_ENTRIES: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum BrowseError {
    #[error("browse serialization: {0}")]
    Serialization(String),
    #[error("unexpected browse message type {0}")]
    WrongType(u16),
    #[error("path too long: {0} bytes (max {MAX_PATH})")]
    PathTooLong(usize),
}

/// "What is in this folder?"
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListRequest {
    /// A share-relative path — `photos/2026`, never `/home/someone/photos`.
    ///
    /// Empty means "list the shares themselves". A device's real filesystem
    /// layout is not the asker's business, and answering with absolute paths
    /// would leak a home directory's name to anyone allowed to browse.
    pub path: String,
}

/// One thing in a folder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// Size in bytes; `0` for a directory.
    pub size: u64,
}

/// What is in the folder that was asked about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListResponse {
    /// Echoed so an asker with several requests in flight can tell them apart.
    pub path: String,
    pub entries: Vec<Entry>,
    /// Whether entries were dropped to fit [`MAX_ENTRIES`].
    ///
    /// Reported rather than hidden: silently returning the first 500 of a
    /// larger folder reads as "that is all there is", which is a wrong answer
    /// rather than a partial one.
    #[serde(default)]
    pub truncated: bool,
    /// Why there is nothing, when there is nothing.
    ///
    /// Deliberately coarse. A peer that may not browse, one that asked about a
    /// path outside every share, and one that asked about something that does
    /// not exist all get the same empty answer — distinguishing them would let
    /// an asker map a filesystem it was never allowed to see, one refused
    /// request at a time.
    #[serde(default)]
    pub denied: bool,
}

impl ListRequest {
    #[must_use]
    pub fn new(path: &str) -> ListRequest {
        ListRequest {
            path: path.to_string(),
        }
    }

    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_LIST_REQUEST)
    }

    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, BrowseError> {
        frame(channel, Self::message_type(), self)
    }

    pub fn from_frame(f: &SessionFrame) -> Result<ListRequest, BrowseError> {
        if f.message_type.get() != MSG_LIST_REQUEST {
            return Err(BrowseError::WrongType(f.message_type.get()));
        }
        let r: ListRequest = serde_json::from_slice(&f.payload)
            .map_err(|e| BrowseError::Serialization(e.to_string()))?;
        if r.path.len() > MAX_PATH {
            return Err(BrowseError::PathTooLong(r.path.len()));
        }
        Ok(r)
    }
}

impl ListResponse {
    /// The answer given when there is nothing to say — and the same answer for
    /// every reason there might be nothing. See [`denied`](Self::denied).
    #[must_use]
    pub fn denied(path: &str) -> ListResponse {
        ListResponse {
            path: path.to_string(),
            entries: Vec::new(),
            truncated: false,
            denied: true,
        }
    }

    #[must_use]
    pub fn message_type() -> MessageType {
        MessageType::new(MSG_LIST_RESPONSE)
    }

    pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, BrowseError> {
        frame(channel, Self::message_type(), self)
    }

    pub fn from_frame(f: &SessionFrame) -> Result<ListResponse, BrowseError> {
        if f.message_type.get() != MSG_LIST_RESPONSE {
            return Err(BrowseError::WrongType(f.message_type.get()));
        }
        let mut r: ListResponse = serde_json::from_slice(&f.payload)
            .map_err(|e| BrowseError::Serialization(e.to_string()))?;
        // A peer's claim about how much it is sending is not trusted: bound it
        // here as well as when building it.
        if r.entries.len() > MAX_ENTRIES {
            r.entries.truncate(MAX_ENTRIES);
            r.truncated = true;
        }
        Ok(r)
    }
}

fn frame<T: Serialize>(
    channel: ChannelId,
    ty: MessageType,
    value: &T,
) -> Result<SessionFrame, BrowseError> {
    let payload = serde_json::to_vec(value)
        .map(Bytes::from)
        .map_err(|e| BrowseError::Serialization(e.to_string()))?;
    Ok(SessionFrame::new(
        channel,
        ty,
        // OPTIONAL, like every message added after the session's first release:
        // a peer without browsing skips it rather than failing the channel.
        MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
        payload,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips_and_ships_optional() {
        let r = ListRequest::new("share/sub");
        let f = r.to_frame(ChannelId::new(1)).unwrap();
        assert!(f.flags.contains(MessageFlags::OPTIONAL));
        assert_eq!(ListRequest::from_frame(&f).unwrap(), r);
    }

    #[test]
    fn an_overlong_path_is_refused() {
        let mut r = ListRequest::new("x");
        r.path = "a".repeat(MAX_PATH + 1);
        let f = r.to_frame(ChannelId::new(1)).unwrap();
        assert!(matches!(
            ListRequest::from_frame(&f),
            Err(BrowseError::PathTooLong(_))
        ));
    }

    #[test]
    fn an_oversized_response_is_bounded_on_the_way_in() {
        let entries = (0..MAX_ENTRIES + 10)
            .map(|i| Entry {
                name: format!("f{i}"),
                is_dir: false,
                size: 1,
            })
            .collect();
        let r = ListResponse {
            path: "share".into(),
            entries,
            truncated: false,
            denied: false,
        };
        let f = r.to_frame(ChannelId::new(1)).unwrap();
        let back = ListResponse::from_frame(&f).unwrap();
        assert_eq!(back.entries.len(), MAX_ENTRIES);
        assert!(back.truncated, "truncation was not reported");
    }

    /// Every reason for "nothing" looks the same on the wire. Distinguishing
    /// them would let an asker map a filesystem it may not see, one refused
    /// request at a time.
    #[test]
    fn a_denial_says_nothing_about_why() {
        let d = ListResponse::denied("share/secret");
        assert!(d.entries.is_empty());
        assert!(d.denied);
        let dumped = serde_json::to_string(&d).unwrap();
        for word in ["permission", "exists", "outside", "denied_because"] {
            assert!(!dumped.contains(word), "the denial leaked a reason: {word}");
        }
    }
}
