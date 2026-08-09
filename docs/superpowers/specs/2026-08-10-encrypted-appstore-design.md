# Encrypted Local AppStore — Design

> Phase A foundation feature (ROADMAP.md; `AppStore` port, `peerbeam-appstore-fs`
> crate). Conforms to the constitutional set: local-first, privacy-first,
> zero-config, encrypted at rest (invariant I11), no cloud. One capability,
> single implementation plan.

## Problem

Phase B/C capabilities need local persistence: **chat log** (append + read-all,
the flagship), **clipboard history** (append + retention trim), **notes**
(key→value). Without a shared store each would invent its own on-disk format and
encryption — parallel systems (I2). The constitution (FUTURE_ARCHITECTURE.md,
FEATURE_CATALOG.md) prescribes one `AppStore`: an encrypted, local-first,
per-namespace keyed store, crate `peerbeam-appstore-fs`, reusing
`peerbeam-crypto`, encrypted at rest (I11), with key material from the persistent
device identity (shipped: X25519 secret in `identity.json`).

## Goal

A minimal, correct encrypted keyed-record store that all three known consumers
can use unchanged — the Phase-A substrate. No capability writes to it yet (chat
is Phase B), so build the store and its tests only; do **not** wire a CLI, FFI,
or engine consumer (DR2 — no premature surface for an empty store).

Non-goals (YAGNI): CLI/FFI/engine wiring, retention/TTL policy, per-namespace
subkeys, encrypted namespace/key names, compaction, async API, cross-device sync.
None are precluded.

## Decisions (resolved)

- **Operation shape: a unified keyed-record store** per namespace —
  `put/get/list/delete/clear`. Append (chat, clipboard) uses a time-ordered key
  (e.g. ULID); KV (notes) uses the item id. One port serves all three (I2), no
  second store later.
- **At-rest encryption: one AppStore key, seal values.** Derive a single 32-byte
  key from the identity secret via `peerbeam-crypto` (domain-separated label
  `"peerbeam-appstore-v1"`). Seal each record's **value** with AES-256-GCM and a
  random per-record nonce (`AeadCrypto::seal` already prepends the nonce; `open`
  reads it — records are self-describing). Namespace and key are stored in
  cleartext in the on-disk layout (the file is `0600`; encrypting values is the
  I11 requirement, and cleartext names keep lookup a direct filesystem op).
- **Layout: file-per-record** (`<root>/<namespace>/<hex(key)>`). `put/get/delete`
  are O(1) single-file ops — a new chat message is a new file, no whole-namespace
  rewrite (which a single-JSON-file layout would force, O(n) per message).
- **Sync API**, mirroring `TrustStore`/`IdentityStore`. Per-record file ops are
  small; async consumers may call it directly.
- **Scope: substrate only** — port + `derive_subkey` + `FsAppStore` + tests. No
  CLI/FFI/engine wiring (no consumer exists).

## Architecture

```
peerbeam-domain
  port::AppStore { put, get, list, delete, clear }   (sync, opaque byte values)

peerbeam-crypto
  pub fn derive_subkey(ikm: &[u8], label: &[u8]) -> [u8; 32]   (SHA-256(ikm‖label))

peerbeam-appstore-fs   (new crate)
  FsAppStore { root: PathBuf, key: [u8;32], enc: Arc<dyn EncryptionProvider> }
    open(root, key, enc)
    layout: <root>/<namespace-slug>/<hex(key)>  = seal(key, random_nonce, value)
    durable per-record write (temp + 0600-at-creation + fsync + rename)

(no consumer this milestone; a future capability derives the key from the
 identity secret and opens FsAppStore)
```

### Domain — `port::AppStore`

```rust
pub trait AppStore: Send + Sync {
    /// Store `value` under (`namespace`, `key`), replacing any existing value.
    fn put(&self, namespace: &str, key: &str, value: &[u8]) -> Result<()>;
    /// Fetch the value for (`namespace`, `key`), or `None` if absent.
    fn get(&self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>>;
    /// All (`key`, `value`) pairs in `namespace`, ordered by key ascending.
    /// Empty if the namespace has no records.
    fn list(&self, namespace: &str) -> Result<Vec<(String, Vec<u8>)>>;
    /// Remove (`namespace`, `key`); returns whether it existed.
    fn delete(&self, namespace: &str, key: &str) -> Result<bool>;
    /// Remove every record in `namespace`.
    fn clear(&self, namespace: &str) -> Result<()>;
}
```

Values are opaque bytes — the caller serializes/deserializes its own record type.
Errors use `DomainError::Storage(String)` (IO/layout) and `DomainError::Encryption`
(from seal/open).

### Crypto — `derive_subkey`

```rust
/// Derive a 32-byte subkey from high-entropy input key material and a
/// domain-separation label. Used to derive the at-rest AppStore key from the
/// device identity's X25519 secret.
pub fn derive_subkey(ikm: &[u8], label: &[u8]) -> [u8; 32]
```
Wraps the existing private `kdf(shared, label) = SHA-256(shared ‖ label)`. Adequate
because the input (the identity secret) is already 32 bytes of high-entropy key
material; the label domain-separates it from the session `kdf` uses.

### Infra — `peerbeam-appstore-fs`

- New workspace crate. Depends on `peerbeam-domain` and `rand` (per-put nonce);
  `peerbeam-crypto` + `tempfile` are dev-dependencies (tests derive a key + build
  an `AeadCrypto`). Encryption goes through the `EncryptionProvider` **port**, so
  the crate does not depend on `peerbeam-crypto` at build time.
- `FsAppStore::open(root: impl Into<PathBuf>, key: [u8; 32], enc: Arc<dyn EncryptionProvider>) -> Self`.
- **Name handling (path-traversal safety):**
  - `namespace` must match `^[A-Za-z0-9._-]+$` (and not `.`/`..`); otherwise
    `Err(DomainError::Storage("invalid namespace"))`. Kept readable for later
    CLI/debugging.
  - `key` is stored as its lowercase-hex encoding for the filename — reversible,
    accepts arbitrary key bytes, and cannot contain `/` or `..`, so a hostile key
    can never escape the namespace directory. `list` hex-decodes filenames back to
    keys.
- **put:** validate namespace → `enc.seal(&self.key, random_nonce(), value)` →
  durable write to `<root>/<ns>/<hex(key)>` (write temp `0600`-at-creation, fsync,
  rename; best-effort parent-dir fsync) — the `peerbeam-identity-fs` pattern.
- **get:** read the record file (absent → `Ok(None)`) → `enc.open(&self.key, &bytes)`
  → value; a decrypt failure is `Err` (corrupt/tampered/wrong key), never silent.
- **list:** read the namespace dir (absent → `Ok(vec![])`); for each entry
  hex-decode the key and open the value; sort by key ascending.
- **delete:** remove the record file; `Ok(true)` if it existed, else `Ok(false)`.
- **clear:** remove the namespace directory recursively (absent → `Ok(())`).
- **nonce:** a fresh random 12-byte nonce per `put`, from `rand::rngs::OsRng`. AES-
  GCM random nonces are safe at this volume (a local store); the nonce is embedded
  by `seal`.

## Data flow

`put("chat", ulid, bytes)` → seal value under the identity-derived key with a
random nonce → atomically write `<root>/chat/<hexulid>` (0600). `list("chat")` →
readdir → hex-decode each filename to its ULID → open each value → sort by ULID →
ordered chat log. Values are unreadable without the identity secret; deleting
`identity.json` makes the store undecryptable (documented).

## Error handling

- Absent record/namespace → `Ok(None)` / `Ok(vec![])` (not an error).
- Corrupt/tampered/wrong-key record → `Err` (never silently skipped or returned as
  empty), so a caller sees data loss rather than a wrong answer.
- Invalid namespace → `Err(DomainError::Storage(...))`.
- No `unwrap`/`expect`/`panic!`/`unsafe` in library code; IO/serde/lock errors map
  to `Result`.

## Testing

- **crypto:** `derive_subkey` is stable (same ikm+label → same key), distinct
  (different ikm → different key), and domain-separated (same ikm, different label
  → different key).
- **appstore-fs:**
  - put→get round-trips a value.
  - the on-disk record file bytes are neither the plaintext value nor readable
    without the key (encryption-at-rest proof).
  - get on an absent key → `None`; list on an absent namespace → empty.
  - list returns all keys ordered ascending, and survives reopen (persistence).
  - delete returns `true` then `false`; the record is gone after.
  - clear removes all records in a namespace, leaving other namespaces intact.
  - a record file corrupted on disk → `get`/`list` return `Err`, not `None`.
  - opening with the wrong key → `get` returns `Err` (tamper/wrong-key rejection).
  - a hostile key (e.g. `"../escape"`) is hex-encoded and stays inside the
    namespace directory (assert no file is created outside `root`).
  - namespaces are isolated: a key in ns A is not visible in ns B.
  - (Unix) record files are mode `0600`.

## Files

- Modify: `rust/crates/peerbeam-domain/src/port/appstore.rs` (new) + `port/mod.rs`
  (module + re-export).
- Modify: `rust/crates/peerbeam-crypto/src/lib.rs` (add `derive_subkey` + test).
- Create: `rust/crates/peerbeam-appstore-fs/` (`Cargo.toml`, `src/lib.rs`), add to
  workspace `members`.
- Docs: `docs/SECURITY.md` — a short note (AppStore data lives at
  `<data_directory>/appstore/`, values encrypted at rest with a key derived from
  the device identity; deleting `identity.json` makes it unreadable).

## Risks

- **Cleartext namespace/key names on disk.** Accepted: the file is `0600` and the
  I11 requirement is that *values* are encrypted; metadata privacy is a possible
  later hardening (encrypted names + a keyed index), not precluded.
- **Random-nonce reuse.** Negligible at a local store's write volume; a fresh
  `OsRng` nonce per put, 96-bit, well within the AES-GCM birthday bound.
- **Key bound to the identity.** Deleting `identity.json` makes the store
  undecryptable — consistent with identity being the device's root secret, and
  documented.
- **Durable-write scaffold now duplicated** across `config`, `trust-fs`,
  `identity-fs`, and `appstore-fs`. Acceptable per precedent; a shared
  atomic-write util is a worthwhile later refactor (tracked, out of scope here).
