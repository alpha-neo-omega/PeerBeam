//! Filesystem [`AppStore`]: per-namespace keyed records, each value encrypted at
//! rest with AES-256-GCM under one caller-supplied key, written as an individual
//! `0600` file (atomic temp+rename+fsync — the `peerbeam-identity-fs` pattern).
//!
//! Layout: `<root>/<namespace>/<hex(key)>`. The namespace is a validated slug
//! (kept readable); the key is hex-encoded as the filename — reversible, accepts
//! arbitrary key bytes, and cannot contain `/` or `..`, so a hostile key can
//! never escape the namespace directory. Only values are encrypted; namespace and
//! key are cleartext in the layout (the file is `0600`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rand::rngs::OsRng;
use rand::RngCore;

use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{AppStore, EncryptionProvider, Nonce};

/// An [`AppStore`] backed by per-record files under `root`, values sealed with
/// `key` via `enc`.
pub struct FsAppStore {
    root: PathBuf,
    key: [u8; 32],
    enc: Arc<dyn EncryptionProvider>,
}

impl FsAppStore {
    /// Open a store rooted at `root`, sealing/opening values with `key` (a 32-byte
    /// data key derived by the caller, e.g. via `peerbeam_crypto::derive_subkey`
    /// from the device-identity secret) using `enc`.
    #[must_use]
    pub fn open(root: impl Into<PathBuf>, key: [u8; 32], enc: Arc<dyn EncryptionProvider>) -> Self {
        FsAppStore {
            root: root.into(),
            key,
            enc,
        }
    }

    /// The directory for `namespace`, validating it as a slug that cannot escape
    /// `root` (rejects empty, `.`, `..`, and anything outside `[A-Za-z0-9._-]`).
    fn namespace_dir(&self, namespace: &str) -> Result<PathBuf> {
        let ok = !namespace.is_empty()
            && namespace != "."
            && namespace != ".."
            && namespace
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !ok {
            return Err(DomainError::Storage(format!(
                "invalid namespace {namespace:?}"
            )));
        }
        Ok(self.root.join(namespace))
    }

    /// The file path for (`namespace`, `key`) — key hex-encoded for safety.
    fn record_path(&self, namespace: &str, key: &str) -> Result<PathBuf> {
        Ok(self
            .namespace_dir(namespace)?
            .join(hex_encode(key.as_bytes())))
    }
}

impl AppStore for FsAppStore {
    fn put(&self, namespace: &str, key: &str, value: &[u8]) -> Result<()> {
        let path = self.record_path(namespace, key)?;
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let sealed = self.enc.seal(&self.key, &Nonce(nonce), value)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DomainError::Storage(format!("appstore dir: {e}")))?;
        }
        write_private(&path, &sealed)?;
        if let Some(parent) = path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }

    fn get(&self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let path = self.record_path(namespace, key)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(self.enc.open(&self.key, &bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(DomainError::Storage(format!("read record: {e}"))),
        }
    }

    fn list(&self, namespace: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let dir = self.namespace_dir(namespace)?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(DomainError::Storage(format!("list namespace: {e}"))),
        };
        let mut out: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| DomainError::Storage(format!("list entry: {e}")))?;
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let file_name = entry.file_name();
            // A non-hex (or non-UTF-8-decoded) filename is not a record we
            // wrote — most commonly an orphaned `<hex>.<pid>.<seq>.tmp` left
            // behind by a `put` interrupted between temp-file creation and
            // the atomic rename (crash/power loss). Skip it rather than
            // failing the whole listing; only a genuine record (valid hex
            // name) whose *value* fails to decrypt is a real error.
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Ok(key_bytes) = hex_decode(name) else {
                continue;
            };
            let Ok(key) = String::from_utf8(key_bytes) else {
                continue;
            };
            let bytes = std::fs::read(entry.path())
                .map_err(|e| DomainError::Storage(format!("read record: {e}")))?;
            let value = self.enc.open(&self.key, &bytes)?;
            out.push((key, value));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    fn namespaces(&self, prefix: &str) -> Result<Vec<String>> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            // No root yet means nothing has ever been stored.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(DomainError::Storage(format!("list namespaces: {e}"))),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue; // a non-UTF-8 directory is not one we wrote
            };
            if !name.starts_with(prefix) {
                continue;
            }
            // Populated only: `clear` removes the directory, but a crash
            // between `create_dir_all` and the first write can leave an empty
            // one, and that is not a conversation.
            let populated = std::fs::read_dir(entry.path())
                .map(|mut d| d.next().is_some())
                .unwrap_or(false);
            if populated {
                out.push(name);
            }
        }
        out.sort();
        Ok(out)
    }

    fn delete(&self, namespace: &str, key: &str) -> Result<bool> {
        let path = self.record_path(namespace, key)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(DomainError::Storage(format!("delete record: {e}"))),
        }
    }

    fn clear(&self, namespace: &str) -> Result<()> {
        let dir = self.namespace_dir(namespace)?;
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DomainError::Storage(format!("clear namespace: {e}"))),
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> std::result::Result<Vec<u8>, String> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return Err("odd length or non-ascii".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Durable private write: temp (`0600`-at-creation on Unix) + fsync + atomic
/// rename. Mirrors `peerbeam-identity-fs`.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = unique_tmp(path);
    write_tmp(&tmp, bytes)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        DomainError::Storage(format!("commit record: {e}"))
    })
}

#[cfg(unix)]
fn write_tmp(tmp: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
        .map_err(|e| DomainError::Storage(format!("create record tmp: {e}")))?;
    let r = (|| {
        f.write_all(bytes)
            .map_err(|e| DomainError::Storage(format!("write record tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| DomainError::Storage(format!("fsync record tmp: {e}")))
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    r
}

#[cfg(not(unix))]
fn write_tmp(tmp: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let r = (|| {
        let mut f = std::fs::File::create(tmp)
            .map_err(|e| DomainError::Storage(format!("create record tmp: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| DomainError::Storage(format!("write record tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| DomainError::Storage(format!("fsync record tmp: {e}")))
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    r
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
    use peerbeam_crypto::{derive_subkey, AeadCrypto};

    fn store(root: &std::path::Path) -> FsAppStore {
        let key = derive_subkey(&[42u8; 32], b"peerbeam-appstore-v1");
        FsAppStore::open(root, key, Arc::new(AeadCrypto::new()))
    }

    fn new_store() -> (FsAppStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        (s, dir)
    }

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.put("chat", "k1", b"hello").unwrap();
        assert_eq!(s.get("chat", "k1").unwrap(), Some(b"hello".to_vec()));
    }

    #[test]
    fn record_file_is_encrypted_at_rest() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.put("chat", "k1", b"secret-plaintext").unwrap();
        let path = dir.path().join("chat").join(hex_encode(b"k1"));
        let raw = std::fs::read(&path).unwrap();
        assert_ne!(
            raw,
            b"secret-plaintext".to_vec(),
            "value must not be stored in the clear"
        );
        assert!(
            raw.windows(b"secret-plaintext".len())
                .all(|w| w != b"secret-plaintext"),
            "plaintext must not appear in the record file"
        );
    }

    #[test]
    fn get_absent_is_none_and_list_absent_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        assert_eq!(s.get("chat", "nope").unwrap(), None);
        assert!(s.list("chat").unwrap().is_empty());
    }

    #[test]
    fn list_is_ordered_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = store(dir.path());
            s.put("chat", "b", b"2").unwrap();
            s.put("chat", "a", b"1").unwrap();
            s.put("chat", "c", b"3").unwrap();
        }
        let s = store(dir.path());
        let got = s.list("chat").unwrap();
        let keys: Vec<&str> = got.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["a", "b", "c"], "ordered by key ascending");
        assert_eq!(got[0].1, b"1".to_vec());
    }

    #[test]
    fn delete_returns_true_then_false() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.put("chat", "k", b"v").unwrap();
        assert!(s.delete("chat", "k").unwrap());
        assert!(!s.delete("chat", "k").unwrap());
        assert_eq!(s.get("chat", "k").unwrap(), None);
    }

    #[test]
    fn clear_isolates_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.put("chat", "k", b"v").unwrap();
        s.put("notes", "n", b"note").unwrap();
        s.clear("chat").unwrap();
        assert!(s.list("chat").unwrap().is_empty());
        assert_eq!(
            s.get("notes", "n").unwrap(),
            Some(b"note".to_vec()),
            "other namespace intact"
        );
    }

    #[test]
    fn corrupt_record_is_an_error_not_none() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.put("chat", "k", b"v").unwrap();
        let path = dir.path().join("chat").join(hex_encode(b"k"));
        std::fs::write(&path, b"garbage-not-a-sealed-record").unwrap();
        assert!(s.get("chat", "k").is_err());
        assert!(s.list("chat").is_err());
    }

    #[test]
    fn list_skips_orphaned_non_hex_files() {
        // An interrupted `put` (crash/power loss between temp-file creation
        // and the atomic rename) can leave a stray `<hex>.<pid>.<seq>.tmp`
        // file in the namespace dir. `list` must skip it, not fail the whole
        // namespace.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.put("chat", "k1", b"hello").unwrap();
        let stray = dir.path().join("chat").join("deadbeef.1234.0.tmp");
        std::fs::write(&stray, b"orphaned temp file, not a record").unwrap();
        let got = s.list("chat").unwrap();
        assert_eq!(got, vec![("k1".to_string(), b"hello".to_vec())]);
    }

    #[test]
    fn wrong_key_cannot_open() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path()).put("chat", "k", b"v").unwrap();
        let other_key = derive_subkey(&[99u8; 32], b"peerbeam-appstore-v1");
        let other = FsAppStore::open(dir.path(), other_key, Arc::new(AeadCrypto::new()));
        assert!(
            other.get("chat", "k").is_err(),
            "wrong key must fail, not return garbage"
        );
    }

    #[test]
    fn hostile_key_stays_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.put("chat", "../escape", b"v").unwrap();
        // Round-trips despite the traversal-looking key.
        assert_eq!(s.get("chat", "../escape").unwrap(), Some(b"v".to_vec()));
        // No file escaped the namespace dir: nothing named "escape" at root or its parent.
        assert!(!dir.path().join("escape").exists());
        assert!(!dir.path().parent().unwrap().join("escape").exists());
        // The record lives at the hex-encoded path inside the namespace.
        assert!(dir
            .path()
            .join("chat")
            .join(hex_encode(b"../escape"))
            .exists());
    }

    #[test]
    fn invalid_namespace_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        assert!(s.put("../evil", "k", b"v").is_err());
        assert!(s.put("", "k", b"v").is_err());
        assert!(s.put("has/slash", "k", b"v").is_err());
        assert!(s.get("..", "k").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn record_file_is_owner_only_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.put("chat", "k", b"v").unwrap();
        let path = dir.path().join("chat").join(hex_encode(b"k"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "record must be 0600");
    }

    #[test]
    fn namespaces_lists_populated_namespaces_matching_a_prefix() {
        let (store, _tmp) = new_store();
        // Inserted out of ascending order, and with enough entries that an
        // implementation which forgot to sort would be overwhelmingly
        // unlikely to come out ascending by luck of the filesystem's own
        // (unspecified, non-insertion-order-guaranteed) directory order.
        store.put("chat-pb-dave", "k", b"v").unwrap();
        store.put("chat-pb-bob", "k", b"v").unwrap();
        store.put("chat-pb-erin", "k", b"v").unwrap();
        store.put("chat-pb-alice", "k", b"v").unwrap();
        store.put("chat-pb-carol", "k", b"v").unwrap();
        store.put("chat.outbox", "k", b"v").unwrap();
        store.put("clipboard", "k", b"v").unwrap();

        let got = store.namespaces("chat-").unwrap();
        assert_eq!(
            got,
            vec![
                "chat-pb-alice",
                "chat-pb-bob",
                "chat-pb-carol",
                "chat-pb-dave",
                "chat-pb-erin",
            ]
        );
        // Belt-and-suspenders: this holds regardless of what order the
        // assertion above assumed, so it still catches a broken sort even if
        // the hardcoded expectation were ever wrong.
        assert!(
            got.windows(2).all(|w| w[0] <= w[1]),
            "namespaces must be sorted ascending"
        );
        // The outbox is deliberately `chat.outbox` (a dot, not a dash) so no
        // peer-supplied device id can collide with it — and so a `chat-`
        // prefix scan never picks it up as a conversation.
        assert!(!got.contains(&"chat.outbox".to_string()));
        assert_eq!(store.namespaces("").unwrap().len(), 7);
        assert!(store.namespaces("nothing-").unwrap().is_empty());
    }

    #[test]
    fn a_namespace_emptied_by_delete_is_not_listed() {
        let (store, _tmp) = new_store();
        store.put("chat-pb-gone", "k", b"v").unwrap();
        assert_eq!(store.namespaces("chat-").unwrap(), vec!["chat-pb-gone"]);
        store.delete("chat-pb-gone", "k").unwrap();
        assert!(
            store.namespaces("chat-").unwrap().is_empty(),
            "an empty namespace is not a conversation"
        );
    }
}
