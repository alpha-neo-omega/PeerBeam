//! What a peer has in a folder, and what this device needs from it.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use peerbeam_domain::session::{ChannelId, MessageFlags, MessageType, SessionFrame};

/// MessageType ids within the Sync channel namespace.
pub const MSG_MANIFEST_REQUEST: u16 = 1;
pub const MSG_MANIFEST: u16 = 2;
/// Ask how a file splits into chunks.
pub const MSG_CHUNKMAP_REQUEST: u16 = 4;
/// The answer: the file's chunk map.
pub const MSG_CHUNKMAP: u16 = 5;
/// Ask for specific chunks by content hash.
pub const MSG_CHUNK_REQUEST: u16 = 6;
/// One chunk's bytes.
pub const MSG_CHUNK_DATA: u16 = 7;
pub const MSG_FILE_REQUEST: u16 = 3;

/// Most files one manifest describes.
///
/// A shared folder can hold a hundred thousand files. Answering with all of
/// them would make one request cost the responder that many stat calls and the
/// asker a frame it must buffer — so it is capped, and says when it was.
pub const MAX_FILES: usize = 2000;

/// Longest share-relative path, in bytes.
pub const MAX_PATH: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("sync serialization: {0}")]
    Serialization(String),
    #[error("unexpected sync message type {0}")]
    WrongType(u16),
    #[error("path too long: {0} bytes (max {MAX_PATH})")]
    PathTooLong(usize),
}

/// "What do you have under this path?"
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestRequest {
    /// A **share-relative** path, as browsing uses. Never absolute: a device's
    /// filesystem layout is not the asker's business.
    pub path: String,
}

/// One file a peer holds.
///
/// Size and modification time, and nothing else. **No checksum**, deliberately:
/// hashing a shared folder on every manifest would read every byte of it to
/// answer a question about what changed, which is the opposite of what a
/// manifest is for. Size-and-mtime is what every practical mirror uses, and its
/// weakness — a file edited in place, same length, same second — is stated
/// rather than papered over.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    /// Path relative to the manifest's own path.
    pub path: String,
    pub size: u64,
    /// Unix seconds. `0` when the peer could not read one.
    pub modified: i64,
    /// Per-device edit counters — what makes bidirectional sync possible.
    ///
    /// `default` so a manifest from a build that predates version vectors
    /// still decodes. Such an entry has an **empty** vector, which relates as
    /// `Behind` to anything at all: an older peer's files are taken rather than
    /// treated as conflicts, which is the safe reading when it cannot tell us
    /// what it changed.
    #[serde(default)]
    pub version: crate::version::VersionVector,
    /// Whether this records a deletion rather than a file.
    #[serde(default)]
    pub deleted: bool,
}

/// What the peer has.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub path: String,
    pub files: Vec<FileEntry>,
    /// Whether files were dropped to fit [`MAX_FILES`].
    #[serde(default)]
    pub truncated: bool,
    /// Nothing to report, for any reason. Same rule browsing follows: a peer
    /// that may not look, a path outside every share and a path that does not
    /// exist are indistinguishable.
    #[serde(default)]
    pub denied: bool,
}

/// "Send me this file."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileRequest {
    /// Share-relative path of the file wanted.
    pub path: String,
}

/// What a mirror must do to match a peer's manifest.
///
/// Deliberately only ever **additive or replacing**: nothing here deletes. A
/// pull that removed local files because a peer no longer has them turns a
/// mirror into a weapon — one misconfigured share, and a folder empties.
/// Removing is a separate decision the user makes with their own file manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Files to fetch: missing locally, or a different size/time.
    pub fetch: Vec<FileEntry>,
    /// Files present locally and already matching. Reported so a caller can say
    /// "12 already up to date" rather than implying it did nothing.
    pub up_to_date: usize,
}

impl Manifest {
    #[must_use]
    pub fn denied(path: &str) -> Manifest {
        Manifest {
            path: path.to_string(),
            files: Vec::new(),
            truncated: false,
            denied: true,
        }
    }
}

/// Decide what to fetch, given a peer's manifest and the local mirror.
///
/// A file is fetched when it is absent locally, a different size, or newer on
/// the peer. **A local file that is newer is left alone**: this is a pull, and
/// silently overwriting something the user edited here would lose work with no
/// warning and no undo.
#[must_use]
pub fn plan(manifest: &Manifest, local_root: &std::path::Path) -> Plan {
    let mut fetch = Vec::new();
    let mut up_to_date = 0;
    for f in &manifest.files {
        let local = local_root.join(&f.path);
        match std::fs::metadata(&local) {
            Err(_) => fetch.push(f.clone()),
            Ok(meta) => {
                let local_mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs() as i64);
                // Two reasons to fetch, and both defer to a newer local file:
                // the peer's copy is a different size and no older than ours,
                // or it is simply newer. A local file that is newer is left
                // alone whatever its size — this is a pull, and overwriting
                // work done here would lose it with no warning and no undo.
                let differs_and_not_older = meta.len() != f.size && local_mtime <= f.modified;
                let peer_is_newer = local_mtime < f.modified;
                if differs_and_not_older || peer_is_newer {
                    fetch.push(f.clone());
                } else {
                    up_to_date += 1;
                }
            }
        }
    }
    Plan { fetch, up_to_date }
}

/// Ask a peer how one of its files splits into chunks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkMapRequest {
    /// Share-relative path of the file.
    pub path: String,
}

/// A peer's answer: how a file is built from chunks.
///
/// An **empty** map is a valid answer meaning "I cannot describe this by
/// chunks" — the file is too large to chunk in memory, or the peer predates
/// delta transfer. The requester falls back to fetching the whole file, which
/// is slower and always correct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkMapResponse {
    pub path: String,
    pub chunks: Vec<peerbeam_chunk::Chunk>,
    /// Whether the peer refused. Indistinguishable from "no such file" on
    /// purpose, exactly as a denied listing is: a caller able to tell them
    /// apart could map a filesystem it may never see.
    #[serde(default)]
    pub denied: bool,
}

/// Ask for specific chunks, by content hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkRequest {
    pub path: String,
    /// Hashes wanted. Bounded by the sender so one request cannot ask a peer to
    /// read an unbounded amount of its disk.
    pub hashes: Vec<String>,
}

/// One chunk's bytes, in answer to a [`ChunkRequest`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkData {
    /// The content hash these bytes claim to be. **Claims**: a receiver must
    /// verify rather than trust it, which is why `reassemble` re-hashes every
    /// chunk before writing it.
    pub hash: String,
    /// Base64 would cost a third more; hex is used everywhere else in this
    /// protocol and reads the same in a log.
    #[serde(with = "hex_bytes")]
    pub bytes: Vec<u8>,
}

/// Hex encoding for chunk payloads.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        use std::fmt::Write;
        let mut out = String::with_capacity(v.len() * 2);
        for b in v {
            let _ = write!(out, "{b:02x}");
        }
        s.serialize_str(&out)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        if s.len() % 2 != 0 {
            return Err(serde::de::Error::custom("odd-length hex"));
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(serde::de::Error::custom))
            .collect()
    }
}

macro_rules! wire {
    ($t:ty, $id:expr) => {
        impl $t {
            #[must_use]
            pub fn message_type() -> MessageType {
                MessageType::new($id)
            }

            pub fn to_frame(&self, channel: ChannelId) -> Result<SessionFrame, SyncError> {
                let payload = serde_json::to_vec(self)
                    .map(Bytes::from)
                    .map_err(|e| SyncError::Serialization(e.to_string()))?;
                Ok(SessionFrame::new(
                    channel,
                    Self::message_type(),
                    // OPTIONAL: a peer without folder sync skips it rather than
                    // failing the channel.
                    MessageFlags::OPTIONAL.with(MessageFlags::END_OF_MESSAGE),
                    payload,
                ))
            }

            pub fn from_frame(f: &SessionFrame) -> Result<$t, SyncError> {
                if f.message_type.get() != $id {
                    return Err(SyncError::WrongType(f.message_type.get()));
                }
                serde_json::from_slice(&f.payload)
                    .map_err(|e| SyncError::Serialization(e.to_string()))
            }
        }
    };
}

wire!(ManifestRequest, MSG_MANIFEST_REQUEST);
wire!(Manifest, MSG_MANIFEST);
wire!(FileRequest, MSG_FILE_REQUEST);
wire!(ChunkMapRequest, MSG_CHUNKMAP_REQUEST);
wire!(ChunkMapResponse, MSG_CHUNKMAP);
wire!(ChunkRequest, MSG_CHUNK_REQUEST);
wire!(ChunkData, MSG_CHUNK_DATA);

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn entry(path: &str, size: u64, modified: i64) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            size,
            modified,
            version: crate::version::VersionVector::new(),
            deleted: false,
        }
    }

    fn manifest(files: Vec<FileEntry>) -> Manifest {
        Manifest {
            path: "share".into(),
            files,
            truncated: false,
            denied: false,
        }
    }

    #[test]
    fn a_missing_file_is_fetched() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan(&manifest(vec![entry("a.txt", 10, 100)]), dir.path());
        assert_eq!(p.fetch.len(), 1);
        assert_eq!(p.up_to_date, 0);
    }

    #[test]
    fn an_identical_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"0123456789").unwrap();
        let meta = std::fs::metadata(dir.path().join("a.txt")).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let p = plan(&manifest(vec![entry("a.txt", 10, mtime)]), dir.path());
        assert!(p.fetch.is_empty(), "an unchanged file was refetched");
        assert_eq!(p.up_to_date, 1);
    }

    /// **A pull never overwrites newer local work.** Silently replacing
    /// something the user edited here would lose it with no warning and no
    /// undo.
    #[test]
    fn a_locally_newer_file_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"edited here").unwrap();
        // The peer's copy is older and a different size — still not taken.
        let p = plan(&manifest(vec![entry("a.txt", 999, 1)]), dir.path());
        assert!(p.fetch.is_empty(), "a pull clobbered newer local work");
    }

    #[test]
    fn a_remotely_newer_file_is_fetched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let p = plan(&manifest(vec![entry("a.txt", 3, future)]), dir.path());
        assert_eq!(p.fetch.len(), 1);
    }

    /// **Nothing is ever deleted.** A pull that removed local files because a
    /// peer no longer has them turns a mirror into a weapon: one misconfigured
    /// share and a folder empties.
    #[test]
    fn a_local_file_the_peer_does_not_have_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mine.txt"), b"local only").unwrap();
        let p = plan(&manifest(vec![]), dir.path());
        assert!(p.fetch.is_empty());
        assert!(
            Path::new(&dir.path().join("mine.txt")).exists(),
            "planning deleted a local file"
        );
    }

    /// **A manifest from a build that predates version vectors still decodes**,
    /// and its files carry an empty vector. That relates as `Behind` to
    /// anything, so an older peer's files are *taken* rather than treated as
    /// conflicts — the safe reading when a device cannot say what it changed.
    /// The alternative, refusing to parse, would make one old peer break sync
    /// for everyone.
    #[test]
    fn a_manifest_without_versions_decodes_and_reads_as_behind() {
        let legacy = serde_json::json!({
            "path": "share",
            "files": [{ "path": "a.txt", "size": 10, "modified": 5 }],
            "truncated": false,
            "denied": false,
        });
        let m: Manifest = serde_json::from_value(legacy).expect("legacy manifests must load");
        assert_eq!(m.files.len(), 1);
        assert!(m.files[0].version.is_empty());
        assert!(!m.files[0].deleted);

        let mut mine = crate::version::VersionVector::new();
        mine.bump("pb-me");
        assert_eq!(
            m.files[0].version.relate(&mine),
            crate::version::Relation::Behind,
            "an older peer's file must be safe to take, never a conflict"
        );
    }

    #[test]
    fn messages_round_trip() {
        let r = ManifestRequest {
            path: "share".into(),
        };
        assert_eq!(
            ManifestRequest::from_frame(&r.to_frame(ChannelId::new(1)).unwrap()).unwrap(),
            r
        );
        let m = manifest(vec![entry("a", 1, 2)]);
        assert_eq!(
            Manifest::from_frame(&m.to_frame(ChannelId::new(1)).unwrap()).unwrap(),
            m
        );
        let f = FileRequest {
            path: "share/a".into(),
        };
        assert_eq!(
            FileRequest::from_frame(&f.to_frame(ChannelId::new(1)).unwrap()).unwrap(),
            f
        );
    }
}
