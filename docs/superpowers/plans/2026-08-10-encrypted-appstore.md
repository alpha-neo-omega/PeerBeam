# Encrypted Local AppStore Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A local, encrypted-at-rest, per-namespace keyed-record store (`AppStore` port + `peerbeam-appstore-fs` crate) that Phase-B/C capabilities (chat, clipboard history, notes) will persist into — built now as substrate, with no consumer yet.

**Architecture:** A sync `AppStore` domain port (`put/get/list/delete/clear`, opaque byte values); a `derive_subkey` helper in `peerbeam-crypto` (one 32-byte key from the device-identity secret); and `FsAppStore`, a file-per-record filesystem adapter that seals each value with AES-256-GCM under that key (via the `EncryptionProvider` port) and writes it durably (`0600`, atomic temp+rename+fsync), mirroring `peerbeam-identity-fs`.

**Tech Stack:** Rust workspace; `rand` 0.8 (per-record nonce); reuses `peerbeam-crypto` (`AeadCrypto`) and `peerbeam-domain` ports.

## Global Constraints

- No `unwrap`/`expect`/`panic!`/`unsafe` in library code (`#[cfg(test)]` may use them).
- Quality gate per task, run from `rust/`: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — all green.
- Commit per task on branch `main` (do NOT push) with trailer exactly: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Errors map to `peerbeam_domain::error::{DomainError, Result}`: `DomainError::Storage(String)` for IO/layout, `DomainError::Encryption(String)` (returned by `seal`/`open`).
- At-rest: only record **values** are encrypted; namespace + key are cleartext in the layout. Record files are `0600` on Unix, written atomically (temp+rename) with `fsync`.
- `device_id`/identity are unrelated here; the store receives a raw `[u8;32]` key from its caller.
- Substrate only: **no CLI, no FFI, no engine wiring** this milestone.
- `rust/Cargo.lock` is tracked — build and stage it when adding the crate.

---

### Task 1: Domain — `AppStore` port

**Files:**
- Create: `rust/crates/peerbeam-domain/src/port/appstore.rs`
- Modify: `rust/crates/peerbeam-domain/src/port/mod.rs`

**Interfaces:**
- Consumes: `crate::error::Result` (existing).
- Produces: `port::AppStore` trait with `put/get/list/delete/clear`.

- [ ] **Step 1: Write `rust/crates/peerbeam-domain/src/port/appstore.rs`**

```rust
//! AppStore port: encrypted, local-first, per-namespace keyed record storage for
//! capability data (chat log, clipboard history, notes). Values are opaque bytes;
//! the caller serializes its own record type. Implemented by an infra adapter
//! (e.g. an encrypted filesystem store).

use crate::error::Result;

/// A namespaced keyed-record store. Each `namespace` is an independent set of
/// `key -> value` records; `key` is caller-chosen (a time-ordered id for an
/// append log, or an item id for key/value data).
pub trait AppStore: Send + Sync {
    /// Store `value` under (`namespace`, `key`), replacing any existing value.
    fn put(&self, namespace: &str, key: &str, value: &[u8]) -> Result<()>;

    /// Fetch the value for (`namespace`, `key`), or `Ok(None)` if absent. A
    /// present-but-unreadable record is an `Err`, never a silent `None`.
    fn get(&self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>>;

    /// All (`key`, `value`) pairs in `namespace`, ordered by key ascending.
    /// `Ok(vec![])` if the namespace has no records.
    fn list(&self, namespace: &str) -> Result<Vec<(String, Vec<u8>)>>;

    /// Remove (`namespace`, `key`); returns whether it existed.
    fn delete(&self, namespace: &str, key: &str) -> Result<bool>;

    /// Remove every record in `namespace` (no-op if it has none).
    fn clear(&self, namespace: &str) -> Result<()>;
}
```

- [ ] **Step 2: Wire the module.** In `rust/crates/peerbeam-domain/src/port/mod.rs`, add `mod appstore;` next to the other `mod` lines, and add to the `pub use` block:

```rust
pub use appstore::AppStore;
```

- [ ] **Step 3: Verify.** From `rust/`:

```bash
cargo build -p peerbeam-domain 2>&1 | tail -3
cargo clippy -p peerbeam-domain --all-targets -- -D warnings 2>&1 | tail -3
```
Expected: builds; clippy clean. (No test — the trait is exercised by Task 3.)

- [ ] **Step 4: Commit**

```bash
cd rust && cargo fmt
git add crates/peerbeam-domain/src/port/appstore.rs crates/peerbeam-domain/src/port/mod.rs
git commit -m "feat(domain): AppStore port (namespaced encrypted keyed records)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `peerbeam-crypto` — `derive_subkey`

**Files:**
- Modify: `rust/crates/peerbeam-crypto/src/lib.rs`

**Interfaces:**
- Consumes: the existing private `fn kdf(shared: &[u8], label: &[u8]) -> [u8; 32]` (SHA-256(shared ‖ label)) in the same file.
- Produces: `pub fn derive_subkey(ikm: &[u8], label: &[u8]) -> [u8; 32]`.

- [ ] **Step 1: Add the function.** In `rust/crates/peerbeam-crypto/src/lib.rs`, near the existing private `kdf` (around line 103), add a public wrapper:

```rust
/// Derive a 32-byte subkey from high-entropy input key material `ikm` and a
/// domain-separation `label`. Used to derive an at-rest data key (e.g. the
/// AppStore key) from the device identity's X25519 secret. The secret is already
/// 32 bytes of high-entropy material, so `SHA-256(ikm ‖ label)` is sufficient;
/// `label` domain-separates it from any other use of the same secret.
#[must_use]
pub fn derive_subkey(ikm: &[u8], label: &[u8]) -> [u8; 32] {
    kdf(ikm, label)
}
```

- [ ] **Step 2: Add the test.** In the same file's `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn derive_subkey_is_stable_distinct_and_domain_separated() {
    let ikm_a = [7u8; 32];
    let ikm_b = [8u8; 32];
    // Stable: same inputs -> same key.
    assert_eq!(
        derive_subkey(&ikm_a, b"peerbeam-appstore-v1"),
        derive_subkey(&ikm_a, b"peerbeam-appstore-v1"),
    );
    // Distinct ikm -> distinct key.
    assert_ne!(
        derive_subkey(&ikm_a, b"peerbeam-appstore-v1"),
        derive_subkey(&ikm_b, b"peerbeam-appstore-v1"),
    );
    // Domain separation: same ikm, different label -> distinct key.
    assert_ne!(
        derive_subkey(&ikm_a, b"peerbeam-appstore-v1"),
        derive_subkey(&ikm_a, b"peerbeam-other-v1"),
    );
}
```

- [ ] **Step 3: Verify.** From `rust/`:

```bash
cargo test -p peerbeam-crypto derive_subkey 2>&1 | tail -5
cargo clippy -p peerbeam-crypto --all-targets -- -D warnings 2>&1 | tail -3
```
Expected: the test passes; clippy clean.

- [ ] **Step 4: Commit**

```bash
cd rust && cargo fmt
git add crates/peerbeam-crypto/src/lib.rs
git commit -m "feat(crypto): derive_subkey — domain-separated 32-byte subkey

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `peerbeam-appstore-fs` crate (`FsAppStore`) + docs

**Files:**
- Create: `rust/crates/peerbeam-appstore-fs/Cargo.toml`
- Create: `rust/crates/peerbeam-appstore-fs/src/lib.rs`
- Modify: `rust/Cargo.toml` (workspace members)
- Modify: `docs/SECURITY.md` (AppStore note)

**Interfaces:**
- Consumes: `peerbeam_domain::port::{AppStore, EncryptionProvider, Nonce}` (Task 1 + existing), `peerbeam_domain::error::{DomainError, Result}`. For tests: `peerbeam_crypto::{AeadCrypto, derive_subkey}` (Task 2) and `peerbeam_domain::port::EncryptionProvider` (`seal`/`open`).
- Produces: `FsAppStore` (implements `AppStore`), `FsAppStore::open(root, key, enc)`.

- [ ] **Step 1: Add the crate to the workspace.** In `rust/Cargo.toml`, add to `members` (next to `"crates/peerbeam-identity-fs",`):

```toml
    "crates/peerbeam-appstore-fs",
```

- [ ] **Step 2: Write `rust/crates/peerbeam-appstore-fs/Cargo.toml`**

```toml
[package]
name = "peerbeam-appstore-fs"
description = "Filesystem AppStore: per-namespace keyed records, values encrypted at rest (AES-256-GCM), 0600 atomic files."
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
peerbeam-domain = { workspace = true }
rand = { workspace = true }

[dev-dependencies]
peerbeam-crypto = { workspace = true }
tempfile = { workspace = true }
```
Note: if a `{ workspace = true }` alias for `peerbeam-crypto` does not exist in the root `Cargo.toml` `[workspace.dependencies]`, use `peerbeam-crypto = { path = "../peerbeam-crypto" }` under `[dev-dependencies]` instead (match how sibling crates reference it). `peerbeam-domain`, `rand`, and `tempfile` are workspace deps already (used by other crates).

- [ ] **Step 3: Write `rust/crates/peerbeam-appstore-fs/src/lib.rs`** (implementation + all tests)

```rust
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
            return Err(DomainError::Storage(format!("invalid namespace {namespace:?}")));
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
            let name = file_name
                .to_str()
                .ok_or_else(|| DomainError::Storage("non-utf8 record name".into()))?;
            let key_bytes =
                hex_decode(name).map_err(|e| DomainError::Storage(format!("bad record name: {e}")))?;
            let key = String::from_utf8(key_bytes)
                .map_err(|e| DomainError::Storage(format!("non-utf8 key: {e}")))?;
            let bytes = std::fs::read(entry.path())
                .map_err(|e| DomainError::Storage(format!("read record: {e}")))?;
            let value = self.enc.open(&self.key, &bytes)?;
            out.push((key, value));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
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
        assert_ne!(raw, b"secret-plaintext".to_vec(), "value must not be stored in the clear");
        assert!(
            raw.windows(b"secret-plaintext".len()).all(|w| w != b"secret-plaintext"),
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
        assert_eq!(s.get("notes", "n").unwrap(), Some(b"note".to_vec()), "other namespace intact");
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
    fn wrong_key_cannot_open() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path()).put("chat", "k", b"v").unwrap();
        let other_key = derive_subkey(&[99u8; 32], b"peerbeam-appstore-v1");
        let other = FsAppStore::open(dir.path(), other_key, Arc::new(AeadCrypto::new()));
        assert!(other.get("chat", "k").is_err(), "wrong key must fail, not return garbage");
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
        assert!(dir.path().join("chat").join(hex_encode(b"../escape")).exists());
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
}
```

- [ ] **Step 4: Verify.** From `rust/`:

```bash
cargo test -p peerbeam-appstore-fs 2>&1 | tail -12
cargo clippy -p peerbeam-appstore-fs --all-targets -- -D warnings 2>&1 | tail -3
```
Expected: 11 tests pass (10 on non-Unix); clippy clean.

- [ ] **Step 5: Document in `docs/SECURITY.md`.** Add a short section (find the trust/keys / device-identity area and place it after the identity note; match the doc's heading level):

```markdown
### Application data (AppStore)

Capability data (chat log, clipboard history, notes) is stored under
`<data_directory>/appstore/<namespace>/`, one file per record. Each record's
**value** is encrypted at rest with AES-256-GCM under a key derived from the
device identity's secret (`peerbeam-appstore-v1`); files are `0600` on Unix.
Namespace and record-key names are stored in the clear (the directory is
`0600`-protected). Because the key derives from the device identity, **deleting
`identity.json` makes existing AppStore data unreadable**. Clearing a namespace
deletes its records.
```

- [ ] **Step 6: Full gate + commit.** From `rust/`:

```bash
cd rust && cargo fmt && cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build -p peerbeam-appstore-fs   # ensure Cargo.lock is updated
cargo test --workspace -- --test-threads=1
cd ..
git add rust/Cargo.toml rust/Cargo.lock rust/crates/peerbeam-appstore-fs docs/SECURITY.md
git commit -m "feat(appstore-fs): FsAppStore — encrypted per-record keyed store

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```
Confirm `git status --short` is clean (Cargo.lock committed). Note: the real-QUIC e2e tests are timing-sensitive; if one flakes under the full run, re-run it in isolation to confirm it passes.

---

## Self-Review

**Spec coverage:**
- `AppStore` port (put/get/list/delete/clear, opaque values) → Task 1 ✅
- `derive_subkey` (SHA-256(ikm‖label), domain-separated) + test → Task 2 ✅
- `FsAppStore` file-per-record, `<root>/<ns>/<hex(key)>`, value sealed with random OsRng nonce via `EncryptionProvider` → Task 3 ✅
- namespace slug validation (reject empty/`.`/`..`/non-slug) + hex key filename (traversal-safe) → Task 3 ✅
- durable write (temp, `0600`-at-creation, fsync, rename, parent-dir fsync) → Task 3 ✅
- get absent→None, decrypt-fail→Err; list absent→empty, ordered, hex-decode; delete→bool; clear→rm dir → Task 3 ✅
- deps (`peerbeam-domain`, `rand`; dev `peerbeam-crypto`, `tempfile`); workspace member; Cargo.lock committed → Task 3 ✅
- Tests: derive_subkey (T2); put/get, **encrypted-at-rest**, absent→None/empty, ordered+reopen, delete true/false, clear-isolates, corrupt→Err, wrong-key→Err, hostile-key-contained, invalid-namespace, 0600 (T3) ✅
- SECURITY.md note → Task 3 Step 5 ✅
- Substrate only (no CLI/FFI/engine wiring) → nothing in the plan wires a consumer ✅

**Placeholder scan:** none — every code step is complete; the only conditional note is the dev-dependency declaration form (`workspace` vs `path`), a real repo convention check, not a logic placeholder.

**Type consistency:** `AppStore::{put(&str,&str,&[u8])->Result<()>, get->Result<Option<Vec<u8>>>, list->Result<Vec<(String,Vec<u8>)>>, delete->Result<bool>, clear->Result<()>}` used identically in Task 1 (definition) and Task 3 (impl + tests). `derive_subkey(&[u8],&[u8])->[u8;32]` used in Task 2 (def) and Task 3 tests. `FsAppStore::open(impl Into<PathBuf>, [u8;32], Arc<dyn EncryptionProvider>)`, `Nonce([u8;12])`, `EncryptionProvider::{seal(&[u8;32],&Nonce,&[u8]), open(&[u8;32],&[u8])}` all match the existing domain definitions.
