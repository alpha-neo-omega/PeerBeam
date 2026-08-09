# Persistent Device Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give each PeerBeam device a stable cryptographic identity (long-term X25519 keypair + a `device_id` derived from its fingerprint) that is generated once, persisted to disk, and loaded on every subsequent start — so TOFU trust pins and history survive restarts.

**Architecture:** A new domain entity (`StoredIdentity`) + port (`IdentityStore`) + pure id-derivation helper live in `peerbeam-domain`; a new infra crate `peerbeam-identity-fs` implements the port over a `0600` JSON file (mirroring `peerbeam-trust-fs`); a `load_or_generate` helper in `peerbeam-transfer` (used by both frontends) loads the identity or generates+persists it on first run. The FFI and CLI replace their ephemeral `generate_keypair()` + `app-<pid>` identity with this helper.

**Tech Stack:** Rust (workspace), serde/serde_json, existing `EncryptionProvider` (X25519 + SHA-256 fingerprint).

## Global Constraints

- No `unwrap`/`expect`/`panic!`/`unsafe` in library code (tests may use them).
- Quality gate for every task: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — all green.
- Commit per task with trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Errors map to `peerbeam_domain::error::{DomainError, Result}` (use `DomainError::Storage(String)` for IO/parse).
- At-rest: secret key in a plaintext JSON file, mode `0600` on Unix. No encryption/keychain. No migration (old identity was ephemeral).
- `device_id` format: `"pb-"` + first 12 hex chars of the public key's fingerprint.
- Run all `cargo` commands from the `rust/` directory.

---

### Task 1: Domain identity surface (entity + serde + id derivation + port)

**Files:**
- Create: `rust/crates/peerbeam-domain/src/entity/identity.rs`
- Modify: `rust/crates/peerbeam-domain/src/entity/mod.rs`
- Create: `rust/crates/peerbeam-domain/src/port/identity.rs`
- Modify: `rust/crates/peerbeam-domain/src/port/mod.rs`

**Interfaces:**
- Consumes: `crate::id::DeviceId`, `crate::port::{PublicKey, SecretKey, Fingerprint}` (existing), `crate::error::Result`.
- Produces:
  - `entity::StoredIdentity { pub device_id: DeviceId, pub public: PublicKey, pub secret: SecretKey }` (Clone; serde as `{device_id, public(hex), secret(hex)}`).
  - `entity::device_id_from_fingerprint(&Fingerprint) -> DeviceId`.
  - `port::IdentityStore { fn load(&self) -> Result<Option<StoredIdentity>>; fn save(&self, &StoredIdentity) -> Result<()>; }`.

- [ ] **Step 1: Write `entity/identity.rs`** (implementation + tests together — this is a pure unit)

```rust
//! Persistent device identity: the long-term keypair + the device id derived
//! from its fingerprint. Serialized with 32-byte keys as lowercase hex so the
//! on-disk file is human-inspectable and independent of in-memory layout.

use serde::{Deserialize, Serialize};

use crate::id::DeviceId;
use crate::port::{Fingerprint, PublicKey, SecretKey};

/// The device's stable cryptographic identity, persisted across restarts.
///
/// Not `Debug`/`PartialEq` on purpose: the secret key must not be printed, and
/// secret equality is not needed outside tests (which compare the raw bytes).
#[derive(Clone, Serialize, Deserialize)]
#[serde(into = "IdentityWire", try_from = "IdentityWire")]
pub struct StoredIdentity {
    /// Stable device id, derived from `public`'s fingerprint.
    pub device_id: DeviceId,
    /// Long-term X25519 public key.
    pub public: PublicKey,
    /// Long-term X25519 secret key. Never leaves the device.
    pub secret: SecretKey,
}

/// On-disk form: keys as hex strings.
#[derive(Serialize, Deserialize)]
struct IdentityWire {
    device_id: String,
    public: String,
    secret: String,
}

impl From<StoredIdentity> for IdentityWire {
    fn from(s: StoredIdentity) -> Self {
        IdentityWire {
            device_id: s.device_id.0,
            public: to_hex(&s.public.0),
            secret: to_hex(&s.secret.0),
        }
    }
}

impl TryFrom<IdentityWire> for StoredIdentity {
    type Error = String;
    fn try_from(w: IdentityWire) -> Result<Self, String> {
        Ok(StoredIdentity {
            device_id: DeviceId::from(w.device_id),
            public: PublicKey(from_hex(&w.public)?),
            secret: SecretKey(from_hex(&w.secret)?),
        })
    }
}

/// Derive the stable device id: `"pb-"` + the first 12 hex chars of the public
/// key's fingerprint (itself `SHA-256(public)` as hex). One source of truth —
/// the keypair determines the fingerprint, which determines the id.
#[must_use]
pub fn device_id_from_fingerprint(fp: &Fingerprint) -> DeviceId {
    let short: String = fp.0.chars().take(12).collect();
    DeviceId::from(format!("pb-{short}"))
}

fn to_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn from_hex(s: &str) -> Result<[u8; 32], String> {
    if s.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", s.len()));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("bad hex at byte {i}: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(hex: &str) -> Fingerprint {
        Fingerprint(hex.to_string())
    }

    #[test]
    fn device_id_derivation_is_stable_distinct_and_formatted() {
        let a = fp(&"a".repeat(64));
        let b = fp(&"b".repeat(64));
        assert_eq!(
            device_id_from_fingerprint(&a).0,
            device_id_from_fingerprint(&a).0,
            "same fingerprint -> same id"
        );
        assert_ne!(
            device_id_from_fingerprint(&a).0,
            device_id_from_fingerprint(&b).0,
            "different fingerprint -> different id"
        );
        let id = device_id_from_fingerprint(&a).0;
        assert!(id.starts_with("pb-"));
        assert_eq!(id.len(), 3 + 12);
    }

    #[test]
    fn stored_identity_json_round_trips_as_hex() {
        let s = StoredIdentity {
            device_id: DeviceId::from("pb-abcdef012345"),
            public: PublicKey([7u8; 32]),
            secret: SecretKey([9u8; 32]),
        };
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(json.contains(&"07".repeat(32)), "public as hex");
        assert!(json.contains(&"09".repeat(32)), "secret as hex");
        let back: StoredIdentity = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.device_id.0, "pb-abcdef012345");
        assert_eq!(back.public.0, [7u8; 32]);
        assert_eq!(back.secret.0, [9u8; 32]);
    }

    #[test]
    fn bad_hex_is_rejected_not_silently_accepted() {
        let json = r#"{"device_id":"pb-x","public":"zz","secret":"00"}"#;
        assert!(serde_json::from_str::<StoredIdentity>(json).is_err());
    }
}
```

- [ ] **Step 2: Wire the entity module.** In `rust/crates/peerbeam-domain/src/entity/mod.rs` add `mod identity;` next to the other `mod` lines, and to the `pub use` block add:

```rust
pub use identity::{device_id_from_fingerprint, StoredIdentity};
```

- [ ] **Step 3: Write the port `port/identity.rs`**

```rust
//! Identity port: the device's persisted long-term identity (keypair + id).

use crate::entity::StoredIdentity;
use crate::error::Result;

/// Loads and persists this device's stable identity. Implemented by an infra
/// adapter (e.g. a JSON file); the frontends generate-once then reuse it.
pub trait IdentityStore: Send + Sync {
    /// Load the stored identity, or `Ok(None)` if none exists yet (first run).
    /// A present-but-unreadable store is an `Err`, never a silent `None` —
    /// silently regenerating would break every peer's trust pin.
    fn load(&self) -> Result<Option<StoredIdentity>>;

    /// Persist `identity`, replacing any previous one.
    fn save(&self, identity: &StoredIdentity) -> Result<()>;
}
```

- [ ] **Step 4: Wire the port module.** In `rust/crates/peerbeam-domain/src/port/mod.rs` add `mod identity;` next to the other `mod` lines, and add to the `pub use` block:

```rust
pub use identity::IdentityStore;
```

- [ ] **Step 5: Verify.** Run from `rust/`:

```bash
cargo test -p peerbeam-domain identity 2>&1 | tail -5
cargo clippy -p peerbeam-domain --all-targets -- -D warnings 2>&1 | tail -3
```
Expected: the three `identity::tests::*` pass; clippy clean.

- [ ] **Step 6: Commit**

```bash
cd rust && cargo fmt
git add crates/peerbeam-domain/src/entity/identity.rs crates/peerbeam-domain/src/entity/mod.rs crates/peerbeam-domain/src/port/identity.rs crates/peerbeam-domain/src/port/mod.rs
git commit -m "feat(domain): StoredIdentity + IdentityStore port + device_id derivation

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `peerbeam-identity-fs` crate (FsIdentity)

**Files:**
- Create: `rust/crates/peerbeam-identity-fs/Cargo.toml`
- Create: `rust/crates/peerbeam-identity-fs/src/lib.rs`
- Modify: `rust/Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: `peerbeam_domain::entity::StoredIdentity`, `peerbeam_domain::port::IdentityStore`, `peerbeam_domain::error::{DomainError, Result}`.
- Produces: `FsIdentity` (implements `IdentityStore`), `FsIdentity::open(path) -> Self`.

- [ ] **Step 1: Add the crate to the workspace.** In `rust/Cargo.toml`, add to `members` (next to `"crates/peerbeam-trust-fs",`):

```toml
    "crates/peerbeam-identity-fs",
```

- [ ] **Step 2: Write `rust/crates/peerbeam-identity-fs/Cargo.toml`** (mirror `peerbeam-trust-fs`)

```toml
[package]
name = "peerbeam-identity-fs"
description = "Filesystem IdentityStore: the device's long-term keypair + id, persisted as a 0600 JSON file."
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
peerbeam-domain = { workspace = true }
serde_json = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 3: Write `rust/crates/peerbeam-identity-fs/src/lib.rs`** (implementation + tests)

```rust
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
        // Atomic: write a uniquely-named temp next to the target, chmod 0600,
        // then rename over it — a crash mid-write leaves the old file intact.
        let tmp = unique_tmp(&self.path);
        std::fs::write(&tmp, json)
            .map_err(|e| DomainError::Storage(format!("write identity: {e}")))?;
        if let Err(e) = set_owner_only(&tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            DomainError::Storage(format!("commit identity: {e}"))
        })
    }
}

/// Restrict `path` to owner read/write (`0600`) on Unix; a no-op elsewhere.
#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| DomainError::Storage(format!("chmod identity: {e}")))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<()> {
    Ok(())
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
```

- [ ] **Step 4: Verify.** From `rust/`:

```bash
cargo test -p peerbeam-identity-fs 2>&1 | tail -8
cargo clippy -p peerbeam-identity-fs --all-targets -- -D warnings 2>&1 | tail -3
```
Expected: 5 tests pass (4 on non-Unix); clippy clean.

- [ ] **Step 5: Commit**

```bash
cd rust && cargo fmt
git add Cargo.toml crates/peerbeam-identity-fs
git commit -m "feat(identity-fs): FsIdentity — 0600 atomic JSON IdentityStore

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `load_or_generate` helper in `peerbeam-transfer`

**Files:**
- Create: `rust/crates/peerbeam-transfer/src/identity.rs`
- Modify: `rust/crates/peerbeam-transfer/src/lib.rs` (module + re-export)
- Test: co-located `#[cfg(test)]` in `identity.rs`

**Interfaces:**
- Consumes: `peerbeam_domain::entity::{StoredIdentity, device_id_from_fingerprint}`, `peerbeam_domain::port::{IdentityStore, EncryptionProvider, KeyPair}`, `crate::auth::Identity`.
- Produces: `pub fn load_or_generate(store: &dyn IdentityStore, enc: &dyn EncryptionProvider, name: String) -> peerbeam_domain::error::Result<Identity>`.

- [ ] **Step 1: Confirm the `Identity` shape.** `crate::auth::Identity { pub device_id: DeviceId, pub name: String, pub keypair: KeyPair }` (from `rust/crates/peerbeam-transfer/src/auth.rs`). `KeyPair { public: PublicKey, secret: SecretKey }`.

- [ ] **Step 2: Write `rust/crates/peerbeam-transfer/src/identity.rs`** (implementation + test)

```rust
//! Load the device's persistent identity, generating it once on first run.

use peerbeam_domain::entity::{device_id_from_fingerprint, StoredIdentity};
use peerbeam_domain::error::Result;
use peerbeam_domain::port::{EncryptionProvider, IdentityStore, KeyPair};

use crate::auth::Identity;

/// Return this device's stable [`Identity`]: load it from `store`, or — on first
/// run — generate a keypair, derive the device id from its fingerprint, persist
/// it, and return it. `name` is the human-facing device name (from config), not
/// part of the stored identity.
pub fn load_or_generate(
    store: &dyn IdentityStore,
    enc: &dyn EncryptionProvider,
    name: String,
) -> Result<Identity> {
    if let Some(stored) = store.load()? {
        return Ok(Identity {
            device_id: stored.device_id,
            name,
            keypair: KeyPair {
                public: stored.public,
                secret: stored.secret,
            },
        });
    }
    let keypair = enc.generate_keypair();
    let device_id = device_id_from_fingerprint(&enc.fingerprint(&keypair.public));
    store.save(&StoredIdentity {
        device_id: device_id.clone(),
        public: keypair.public.clone(),
        secret: keypair.secret.clone(),
    })?;
    Ok(Identity {
        device_id,
        name,
        keypair,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_crypto::AeadCrypto;
    use peerbeam_identity_fs::FsIdentity;

    #[test]
    fn generates_once_then_loads_the_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        let enc = AeadCrypto::new();

        let first =
            load_or_generate(&FsIdentity::open(&path), &enc, "dev".into()).expect("first");
        let second =
            load_or_generate(&FsIdentity::open(&path), &enc, "dev".into()).expect("second");

        assert_eq!(first.device_id.0, second.device_id.0, "stable device id");
        assert_eq!(
            first.keypair.public.0, second.keypair.public.0,
            "stable public key"
        );
        assert_eq!(first.keypair.secret.0, second.keypair.secret.0);
        assert!(first.device_id.0.starts_with("pb-"));
    }
}
```

- [ ] **Step 3: Add the test deps.** In `rust/crates/peerbeam-transfer/Cargo.toml` `[dev-dependencies]`, ensure these are present (add any missing):

```toml
peerbeam-crypto = { workspace = true }
peerbeam-identity-fs = { workspace = true }
tempfile = { workspace = true }
```
(If `peerbeam-identity-fs` is not yet a workspace dependency alias, add `peerbeam-identity-fs = { path = "../peerbeam-identity-fs" }` under `[dev-dependencies]` instead.)

- [ ] **Step 4: Register + re-export the module.** In `rust/crates/peerbeam-transfer/src/lib.rs`, add `mod identity;` with the other `mod` declarations and re-export:

```rust
pub use identity::load_or_generate;
```

- [ ] **Step 5: Verify.** From `rust/`:

```bash
cargo test -p peerbeam-transfer --lib identity 2>&1 | tail -5
cargo clippy -p peerbeam-transfer --all-targets -- -D warnings 2>&1 | tail -3
```
Expected: `generates_once_then_loads_the_same_identity` passes; clippy clean.

- [ ] **Step 6: Commit**

```bash
cd rust && cargo fmt
git add crates/peerbeam-transfer/src/identity.rs crates/peerbeam-transfer/src/lib.rs crates/peerbeam-transfer/Cargo.toml
git commit -m "feat(transfer): load_or_generate persistent device identity

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Wire the FFI to the persistent identity

**Files:**
- Modify: `rust/crates/peerbeam-ffi/src/runtime.rs`
- Modify: `rust/crates/peerbeam-ffi/Cargo.toml` (add `peerbeam-identity-fs` dependency)

**Interfaces:**
- Consumes: `peerbeam_transfer::load_or_generate`, `peerbeam_identity_fs::FsIdentity`.
- Produces: the runtime's `Identity` and the discovery-facing device id both come from the persisted identity.

**Context:** Today `runtime.rs` has `fn device_id() -> DeviceId { DeviceId::from(format!("app-{}", std::process::id())) }` (~line 169), used by `me()` (discovery) and again at engine init (~line 245), and the transfer manager builds its own `Identity` with a fresh `generate_keypair()` (~line 258-270). All of these must resolve to the **one** persisted identity, loaded once.

- [ ] **Step 1: Add the dependency.** In `rust/crates/peerbeam-ffi/Cargo.toml` `[dependencies]`:

```toml
peerbeam-identity-fs = { workspace = true }
```
(or `{ path = "../peerbeam-identity-fs" }` if the workspace alias isn't defined).

- [ ] **Step 2: Load the identity once, early in engine init.** In `runtime.rs`, where the engine/config is set up (before `me()` and the transfer manager are built), construct the identity from the persisted store:

```rust
let enc = std::sync::Arc::new(AeadCrypto::new());
let identity_path =
    std::path::Path::new(&config.storage.data_directory).join("identity.json");
let identity = peerbeam_transfer::load_or_generate(
    &peerbeam_identity_fs::FsIdentity::open(identity_path),
    enc.as_ref(),
    config.device.name.clone(),
)
.map_err(crate::error::from_domain)?;
let device_id = identity.device_id.clone();
```
Reuse this `enc` for the transfer manager (do not create a second `AeadCrypto` there).

- [ ] **Step 3: Replace the ephemeral id + keypair.**
  - Change `me()` and any discovery/status construction to use the loaded `device_id` instead of calling `device_id()`. If `device_id()` becomes unused, delete it; if it is still called from a context without access to the loaded id, have that context read the same persisted store (do **not** reintroduce `app-<pid>`).
  - At the transfer-manager construction (~line 258-270), remove the local `let keypair = enc.generate_keypair();` and the `Identity { device_id: id.clone(), name: ..., keypair }` literal; pass the `identity` loaded in Step 2 instead.

- [ ] **Step 4: Verify build + a real run.** From `rust/`:

```bash
cargo clippy -p peerbeam-ffi --all-targets -- -D warnings 2>&1 | tail -3
cargo build -p peerbeam-cli 2>&1 | tail -1
```
Then confirm identity persistence end-to-end with the CLI (Task 5 wires the CLI, but the FFI file is created by any run that inits the engine). After Task 5, the manual E2E in Task 5 Step 4 covers this. For now, assert the FFI crate builds and clippy is clean.

- [ ] **Step 5: Commit**

```bash
cd rust && cargo fmt
git add crates/peerbeam-ffi/src/runtime.rs crates/peerbeam-ffi/Cargo.toml
git commit -m "feat(ffi): use the persistent device identity (drop app-<pid> + fresh keypair)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Wire the CLI to the persistent identity + document it

**Files:**
- Modify: `rust/bins/peerbeam-cli/src/commands.rs` (identity construction on the transfer/daemon paths)
- Modify: `rust/bins/peerbeam-cli/Cargo.toml` (add `peerbeam-identity-fs`)
- Modify: `docs/SECURITY.md` (identity note)

**Interfaces:**
- Consumes: `peerbeam_transfer::load_or_generate`, `peerbeam_identity_fs::FsIdentity`.

**Context:** The CLI builds `Identity` for real transfers/daemon at `commands.rs:943-945` (the `generate_keypair()` + `Identity { ... }` block). The `doctor` command's keypair at `commands.rs:257` is a one-shot health check, **not** the device identity — leave it ephemeral.

- [ ] **Step 1: Add the dependency.** In `rust/bins/peerbeam-cli/Cargo.toml` `[dependencies]`:

```toml
peerbeam-identity-fs = { workspace = true }
```
(or a `path` dependency, matching how the CLI references other workspace crates).

- [ ] **Step 2: Replace the transfer/daemon identity construction.** At `commands.rs:943-945`, replace the `let keypair = enc.generate_keypair(); let ident = Identity { device_id: <ephemeral>, name, keypair };` block with:

```rust
let identity_path =
    std::path::Path::new(&config.storage.data_directory).join("identity.json");
let ident = peerbeam_transfer::load_or_generate(
    &peerbeam_identity_fs::FsIdentity::open(identity_path),
    &enc,
    config.device.name.clone(),
)?;
```
Use the existing `enc` in scope (an `AeadCrypto`); match the surrounding error handling (`?` into the command's error type, or map as neighboring calls do). Ensure every path that previously used an ephemeral device id now uses `ident.device_id`.

- [ ] **Step 3: Document the identity file.** Add to `docs/SECURITY.md` (under the trust/keys section):

```markdown
### Device identity

Each device has a long-term X25519 identity keypair, generated on first run and
stored at `<data_directory>/identity.json` with owner-only permissions (`0600`
on Unix). The `device_id` is derived from the key's fingerprint (`pb-…`). This
file is the device's identity: peers pin it via TOFU, so **deleting it resets
the device's identity** (peers will see a new, untrusted device and must trust
it again). It is never transmitted and never leaves the device.
```

- [ ] **Step 4: Verify build + real-run persistence.** From `rust/`:

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo build -p peerbeam-cli 2>&1 | tail -1
BIN=./target/debug/peerbeam
W=$(mktemp -d)
printf '{"storage":{"data_directory":"%s/data","save_directory":"%s/recv"}}' "$W" "$W" > "$W/config.json"
# `status` inits the engine (creating identity.json); run it twice and confirm the id is stable.
"$BIN" --config "$W/config.json" --json status >/dev/null 2>&1
ID1=$(cat "$W/data/identity.json" | tr -d ' \n')
"$BIN" --config "$W/config.json" --json status >/dev/null 2>&1
ID2=$(cat "$W/data/identity.json" | tr -d ' \n')
test "$ID1" = "$ID2" && echo "IDENTITY STABLE" || echo "IDENTITY CHANGED"
stat -c '%a' "$W/data/identity.json"   # expect 600
```
Expected: `IDENTITY STABLE`, permissions `600`. (If `status` does not create the identity, use a command that inits the transfer path, e.g. a `receive --once` started and killed, or `daemon status`.)

- [ ] **Step 5: Full gate + commit**

```bash
cd rust && cargo fmt && cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1
cd ..
git add rust/bins/peerbeam-cli/src/commands.rs rust/bins/peerbeam-cli/Cargo.toml docs/SECURITY.md
git commit -m "feat(cli): use the persistent device identity; document identity.json

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- StoredIdentity entity + hex serde → Task 1 ✅
- IdentityStore port → Task 1 ✅
- device_id_from_fingerprint (pure, `pb-`+12 hex) → Task 1 ✅
- peerbeam-identity-fs / FsIdentity (open/load/save, atomic, 0600, mirrors trust-fs) → Task 2 ✅
- load_or_generate (generate-once/load, derive id) → Task 3 ✅
- FFI wiring (replace generate_keypair + app-<pid>) → Task 4 ✅
- CLI wiring (transfer/daemon; doctor left ephemeral) → Task 5 ✅
- Tests: domain derivation stable/distinct/format + serde round-trip (T1); infra None/round-trip/**stability across reopen**/0600/corrupt→Err (T2); integration load_or_generate twice = same id+pubkey (T3) ✅
- No migration → stated; old records are harmless orphans ✅
- Docs (SECURITY note) → Task 5 ✅
- Error handling: corrupt→Err not None (T1 port doc + T2 test); no unwrap/expect/panic in lib code (all `?`/`map_err`) ✅

**Placeholder scan:** none — every code step has complete code; the only "if missing / match surrounding style" notes are dependency-declaration and error-mapping conventions, not logic placeholders.

**Type consistency:** `StoredIdentity { device_id: DeviceId, public: PublicKey, secret: SecretKey }`, `device_id_from_fingerprint(&Fingerprint) -> DeviceId`, `IdentityStore::{load -> Result<Option<StoredIdentity>>, save(&StoredIdentity) -> Result<()>}`, `FsIdentity::open(impl Into<PathBuf>)`, `load_or_generate(&dyn IdentityStore, &dyn EncryptionProvider, String) -> Result<Identity>` — used identically across Tasks 1–5. `PublicKey`/`SecretKey` are `.0: [u8;32]`; `Fingerprint`/`DeviceId` are `.0: String`; all match the existing domain definitions.
