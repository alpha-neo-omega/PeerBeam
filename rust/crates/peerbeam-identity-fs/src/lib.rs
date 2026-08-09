//! Filesystem [`IdentityStore`]: the device's long-term identity in one JSON
//! file, written atomically and (on Unix) with mode `0600` so the secret key is
//! readable only by its owner — the same posture as an SSH private key.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use peerbeam_domain::entity::StoredIdentity;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::IdentityStore;

/// An [`IdentityStore`] backed by a single JSON file.
pub struct FsIdentity {
    path: PathBuf,
}

impl FsIdentity {
    /// Point at `path` (does not read until [`load`](IdentityStore::load)).
    #[must_use]
    pub fn open(path: impl Into<PathBuf>) -> Self {
        FsIdentity { path: path.into() }
    }
}

impl IdentityStore for FsIdentity {
    fn load(&self) -> Result<Option<StoredIdentity>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice::<StoredIdentity>(&bytes)
                    .map_err(|e| DomainError::Storage(format!("parse identity: {e}")))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(DomainError::Storage(format!("read identity: {e}"))),
        }
    }

    fn save(&self, identity: &StoredIdentity) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DomainError::Storage(format!("identity dir: {e}")))?;
        }
        let json = serde_json::to_vec_pretty(identity)
            .map_err(|e| DomainError::Storage(format!("serialize identity: {e}")))?;
        // Atomic: write a uniquely-named temp next to the target (private
        // from the moment it's created — see `write_private`), then rename
        // over it — a crash mid-write leaves the old file intact.
        let tmp = unique_tmp(&self.path);
        write_private(&tmp, &json)?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            DomainError::Storage(format!("commit identity: {e}"))
        })
    }
}

/// Create `path` and write `bytes` to it. On Unix the file is created with
/// mode `0600` atomically (via `open(..., O_CREAT|O_EXCL, 0600)`), so there is
/// no window — not even an instant — where the secret key sits at the
/// process's default (typically group/world-readable) umask before a
/// separate chmod call catches up. Elsewhere, permissions are unmanaged.
#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| DomainError::Storage(format!("create identity: {e}")))?;
    if let Err(e) = file.write_all(bytes) {
        let _ = std::fs::remove_file(path);
        return Err(DomainError::Storage(format!("write identity: {e}")));
    }
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).map_err(|e| DomainError::Storage(format!("write identity: {e}")))
}

/// A temp path next to `path`, unique per process and per call.
fn unique_tmp(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{}.{}.tmp", std::process::id(), n));
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::id::DeviceId;
    use peerbeam_domain::port::{PublicKey, SecretKey};

    fn ident(pub_byte: u8) -> StoredIdentity {
        StoredIdentity {
            device_id: DeviceId::from(format!("pb-{pub_byte:012x}")),
            public: PublicKey([pub_byte; 32]),
            secret: SecretKey([pub_byte.wrapping_add(1); 32]),
        }
    }

    #[test]
    fn load_absent_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsIdentity::open(dir.path().join("identity.json"));
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsIdentity::open(dir.path().join("identity.json"));
        let id = ident(5);
        store.save(&id).unwrap();
        let back = store.load().unwrap().expect("present");
        assert_eq!(back.device_id.0, id.device_id.0);
        assert_eq!(back.public.0, id.public.0);
        assert_eq!(back.secret.0, id.secret.0);
    }

    #[test]
    fn identity_is_stable_across_reopen() {
        // The core property: a second open+load returns the same identity.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        let saved = ident(9);
        FsIdentity::open(&path).save(&saved).unwrap();
        let reloaded = FsIdentity::open(&path).load().unwrap().expect("present");
        assert_eq!(reloaded.public.0, saved.public.0);
        assert_eq!(reloaded.secret.0, saved.secret.0);
        assert_eq!(reloaded.device_id.0, saved.device_id.0);
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_owner_only_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        FsIdentity::open(&path).save(&ident(1)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "identity file must be 0600");
    }

    #[test]
    fn corrupt_file_is_an_error_not_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        std::fs::write(&path, b"{ not valid json").unwrap();
        assert!(FsIdentity::open(&path).load().is_err());
    }
}
