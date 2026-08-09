# Persistent Device Identity — Design

> Phase A foundation feature (ROADMAP.md). Conforms to the constitutional set
> (VISION, ARCHITECTURAL_INVARIANTS, FUTURE_ARCHITECTURE, ROADMAP): local-first,
> privacy-first, zero-config, no cloud. One capability, single implementation plan.

## Problem

The device's cryptographic identity churns on every launch:

- **Keypair is ephemeral.** Both frontends call `enc.generate_keypair()` at
  startup — FFI [`runtime.rs:265`](../../../rust/crates/peerbeam-ffi/src/runtime.rs)
  and CLI [`commands.rs`](../../../rust/bins/peerbeam-cli/src/commands.rs).
- **device_id is ephemeral.** `device_id()` returns `format!("app-{}", process::id())`
  — the PID ([`runtime.rs:169`](../../../rust/crates/peerbeam-ffi/src/runtime.rs)).

TOFU trust pins a peer as `(DeviceId → fingerprint-of-public-key)`
(`TrustRecord`, `entity/trust.rs`). Because both our `device_id` and our public
key change every run, a peer that pinned us once sees a **different device with a
different key** on the next connection — trust pins and history break, and the
key change reads as a potential MITM. This blocks all of Phase B (chat, presence,
clipboard sync) which require a stable peer identity.

## Goal

A **stable device identity** — a long-term X25519 keypair and a `device_id`
derived from it — generated once on first run, persisted, and loaded on every
subsequent start. Threaded identically through the CLI and FFI frontends.

Non-goals (YAGNI): key rotation, multiple identities, OS-keychain storage,
at-rest encryption, a UI for identity management. These are deferred; nothing
here precludes them.

## Decisions (resolved)

- **At-rest protection: plaintext JSON, `0600`.** `identity.json` in the existing
  `data_directory`, owner read/write only — the same trust model as `~/.ssh`
  private keys and the existing `trust.json`. The alternatives are ruled out by
  the constitution: an OS keychain has no equivalent on the required headless
  server / Docker targets, and at-rest encryption in a zero-config, passwordless
  app has nowhere secure to keep the wrapping key (it would sit next to the file).
- **device_id derived from the public key's fingerprint:** `"pb-" + first 12 hex
  chars of `EncryptionProvider::fingerprint(public)`` (the fingerprint is already
  `SHA-256(public)` as hex, 64 chars). One source of truth — the keypair
  determines the fingerprint, which determines the `device_id`. Reusing the
  existing `fingerprint` avoids adding a hash dependency to the pure domain layer.
- **Abstraction: a domain port + a dedicated infra crate**, mirroring
  `TrustStore` → `FsTrust` (`peerbeam-trust-fs`). Not the config file (secrets do
  not belong in the user-editable, non-`0600`, copy-shared `config.json`), and not
  inlined per-frontend (that duplicates identity logic across CLI+FFI — the I2
  parallel-systems fracture).

## Architecture

```
peerbeam-domain
  entity::StoredIdentity { device_id, public, secret }   (serde)
  port::IdentityStore { load() -> Option<StoredIdentity>; save(&StoredIdentity) }
  id derivation: device_id_from_fingerprint(&Fingerprint) -> DeviceId  (pure)

peerbeam-identity-fs   (new crate, mirrors peerbeam-trust-fs)
  FsIdentity : IdentityStore
    open(path) ; load() reads <path> or None ; save() atomic temp+rename, mode 0600

frontends (CLI + FFI)
  load_or_generate(&store, &enc, name) -> Identity
    load() -> build Identity
    None   -> generate_keypair() -> derive device_id -> save() -> build Identity
  path = <data_directory>/identity.json
```

### Domain

- `entity::StoredIdentity`
  - `device_id: DeviceId`
  - `public: PublicKey` (`[u8; 32]`)
  - `secret: SecretKey` (`[u8; 32]`)
  - `Serialize`/`Deserialize`: 32-byte keys as lowercase hex strings (not raw byte
    arrays) so the file is human-inspectable and stable.
- `port::IdentityStore`
  - `fn load(&self) -> Result<Option<StoredIdentity>>`
  - `fn save(&self, identity: &StoredIdentity) -> Result<()>`
- `fn device_id_from_fingerprint(fp: &Fingerprint) -> DeviceId` — `"pb-"` + the
  first 12 hex chars of `fp.0`. A pure string operation (no hashing in domain);
  the caller computes the fingerprint via `EncryptionProvider::fingerprint`.
  Deterministic, stable, collision-resistant enough for a device label.

### Infra — `peerbeam-identity-fs`

- New workspace crate, same shape as `peerbeam-trust-fs`.
- `FsIdentity { path: PathBuf }`; `open(path)` stores the path (does not read yet).
- `load()`: if the file is absent → `Ok(None)`; if present → parse JSON into
  `StoredIdentity`; parse/read failure → `Err` (a clear message), **never** a
  silent `None` (silent regeneration would silently break trust).
- `save()`: serialize to pretty JSON, write to a uniquely-named temp file next to
  the target, `fsync`, set mode `0600` (Unix; best-effort/no-op elsewhere), then
  atomically rename over `identity.json` — the same atomic pattern as `FsTrust`.

### Frontend wiring

- A shared helper (co-located with the identity construction, not duplicated):
  `load_or_generate(store: &dyn IdentityStore, enc: &dyn EncryptionProvider, name: String) -> Result<Identity>`.
  - `store.load()?`:
    - `Some(s)` → `Identity { device_id: s.device_id, name, keypair: KeyPair { public: s.public, secret: s.secret } }`.
    - `None` → `kp = enc.generate_keypair()`;
      `id = device_id_from_fingerprint(&enc.fingerprint(&kp.public))`;
      `store.save(&StoredIdentity { device_id: id, public: kp.public, secret: kp.secret })?`;
      build `Identity`.
- FFI `runtime.rs`: replace the `generate_keypair()` + `device_id()` (`app-<pid>`)
  path with `load_or_generate`, using `data_directory/identity.json`. The
  discovery-facing `device_id()`/`me()` use the same persisted id.
- CLI: the transfer/daemon paths build `Identity` via the same helper + path.
  (One-shot diagnostic keypairs, e.g. `doctor`, may stay ephemeral — they are not
  the device identity; call this out so they are not "fixed" by mistake.)

## Data flow

First run: no file → generate → derive id → write `identity.json` (`0600`) →
identity in memory. Every later run: read `identity.json` → same keypair, same
`device_id`, same fingerprint → peers' TOFU pins and history keep matching.

## Error handling

- Missing file → generate (expected first-run path).
- Present but unreadable/corrupt → return an error that names the file; the
  frontend surfaces it (FFI: an error envelope; CLI: a non-zero exit with the
  path). Do **not** regenerate silently.
- No `unwrap`/`expect`/`panic!` in library code (project rule). Poisoned-lock and
  IO errors map to the domain `Result`.

## Testing

- **Domain**
  - `device_id_from_fingerprint` is stable (same fingerprint → same id) and
    distinct (different fingerprints → different ids); format is `pb-` + 12 hex
    chars.
  - `StoredIdentity` serde round-trips; keys encode as hex.
- **Infra (`peerbeam-identity-fs`)**
  - `load()` on an absent file → `Ok(None)`.
  - generate → `save()` → `load()` returns an equal `StoredIdentity`.
  - **stability:** two `FsIdentity::open` + `load` cycles over the same path
    return the same identity (the core "stable across restarts" property).
  - saved file has mode `0600` (Unix).
  - corrupt JSON → `Err`, not `None`.
- **Integration**
  - `load_or_generate` twice against the same path yields the same `device_id`
    and public key (once via generate, once via load).

## Files

- Modify: `rust/crates/peerbeam-domain/src/entity/` (add `StoredIdentity`),
  `rust/crates/peerbeam-domain/src/port/` (add `IdentityStore`), a domain id
  helper, and the entity/port `mod.rs` re-exports.
- Create: `rust/crates/peerbeam-identity-fs/` (crate: `Cargo.toml`, `src/lib.rs`),
  add to the workspace members.
- Modify: `rust/crates/peerbeam-ffi/src/runtime.rs` and the CLI identity
  construction to use `load_or_generate`.
- Docs: `docs/SECURITY.md` (or the security doc) — note the identity file, its
  `0600` protection, and that deleting it resets the device's identity.

## Risks

- **Secret on disk in plaintext.** Accepted, constitution-driven; mitigated by
  `0600` and documented (same posture as SSH keys). A future encryption/keychain
  option is not precluded.
- **device_id format change** (`app-<pid>` → `pb-…`). No migration needed — the
  old value was never stable, so no real data keys on it; stale trust records are
  harmless orphans.
- **Concurrent first-run writes** (two processes, same data dir). The atomic
  temp+rename makes the last writer win with a complete file; both end with a
  valid identity (one may overwrite the other's brand-new id on the very first
  run — acceptable and vanishingly rare; documented).
