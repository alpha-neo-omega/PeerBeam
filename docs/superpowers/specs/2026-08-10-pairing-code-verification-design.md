# Optional Pairing-Code Verification — Design

> Phase A final feature (ROADMAP.md; "optional PIN pairing"). Conforms to the
> constitutional set: strengthens invariant I6 (explicit approval), adds no wire
> field, no round-trip, no new crypto dependency — therefore **not** a protocol
> change and needs no amendment. One capability, single implementation plan.

## Problem

Trust-on-first-use already pins a peer's public-key fingerprint on first contact
(`auth.rs`: `TrustRecord{approved:false}`), and the identity-bound transcript
defeats an on-path attacker who rewrites the cleartext identity fields. But first
contact is still **blind**: a user has no in-app way to confirm the key they just
pinned belongs to the peer they intend to reach, rather than to a
man-in-the-middle who completed a valid handshake on each leg. The full SHA-256
fingerprint is computable but never surfaced for human comparison.

This feature adds an **optional, human-verifiable first-contact check**: both
peers display a code derived from their two device keys. Matching codes prove both
sides see the same pair of keys (no MITM); a mismatch is treated as a suspected
MITM and revokes the just-pinned key.

## Goal

A pairing code that (a) is identical on both honest peers and diverges under a
MITM, (b) is derived locally from the two already-exchanged public keys (no wire
change, no round-trip), (c) is stable across sessions (re-verifiable, like a
Signal safety number), and (d) is long enough to resist an offline grind attack.
Plus the config toggle, trust revocation, and the CLI/FFI surfaces needed to use
it end to end. The Flutter UI is deferred to a later wiring task.

## Security rationale (why not a 6-digit code)

For a user to be fooled, an active MITM must present two substituted keys whose
codes **collide** — `code(pub_A, pub_M) == code(pub_M', pub_B)` — so both honest
peers display the same value. The attacker freely generates candidate keypairs on
each leg, so this is a birthday collision search costing ~`2^(n/2)` for an `n`-bit
code. A 6-digit code (~20 bits) is grindable in milliseconds and gives no real
protection; this is why Signal's safety number is long and why ZRTP's short SAS
only holds inside a PAKE/commitment protocol. We use **128 bits** → ~`2^64` work,
infeasible. The strong guarantee remains the full fingerprint + TOFU pinning; this
code is the human-comparable encoding of that guarantee.

## Decisions (resolved)

- **Model: safety-number compare** (not entered-PIN, not PAKE). Both devices show
  the code; the user confirms they match. No wire change, no round-trip, no new
  crypto.
- **Input: the two device public keys** (stable, re-verifiable). The code does
  not depend on per-session nonces, so the same device pair always yields the
  same code — a user can re-verify later, and it precisely captures the
  identity-key binding that matters.
- **Size/format: 128 bits, uppercase hex, 8 groups of 4** (e.g.
  `4F9A 2C71 8BD3 E056 1A2B 3C4D 5E6F 7788`). Grind-resistant and unambiguous
  (hex has no `O`/`0` confusion).
- **Gate at the approval boundary (I6), not in `authenticate()`.** `authenticate`
  stays a pure crypto/TOFU step; the frontend enforces the confirm.
- **Config toggle, default off.** Zero-config first contact stays frictionless;
  the code is still displayed for voluntary verification when the toggle is off.
- **Revoke on mismatch/decline** — delete the just-pinned trust record so a
  suspected-MITM key is not left pinned (which would otherwise silently succeed
  next time).
- **Scope: substrate + CLI + FFI expose; Flutter UI deferred.** The CLI is a
  first-class app (constitution), so gating there makes the feature usable and
  end-to-end testable now; FFI exposes what Flutter needs to wire a dialog later.

Non-goals (YAGNI): QR/scan, wordlist/emoji encoding, mixing the code into key
agreement (PAKE), a "quick" short code, Flutter UI, entered-PIN input. None are
precluded.

## Architecture

```
peerbeam-crypto
  pub fn pairing_code(pub_a: &[u8;32], pub_b: &[u8;32]) -> String
    canonical order (lo,hi) by bytes -> SHA-256("peerbeam-pairing-v1" ‖ lo ‖ hi)
    -> first 16 bytes -> uppercase hex -> "XXXX XXXX XXXX XXXX XXXX XXXX XXXX XXXX"

peerbeam-transfer  (auth.rs)
  Session { .., pairing_code: String }   // computed in authenticate() from our_pub + peer_pub

peerbeam-config
  DeviceConfig { name, auto_accept_trusted, require_pairing_confirmation: bool }  // default false

peerbeam-trust-fs
  FsTrust::forget(&DeviceId) -> Result<bool>   // mirrors approve(); atomic rewrite

bins/peerbeam-cli, peerbeam-ffi
  surface pairing_code + gate first-contact approval on it (CLI interactive; FFI expose)
```

### Crypto — `pairing_code`

```rust
/// A stable, human-comparable "safety number" over two device public keys.
/// Identical for both peers of an honest handshake (order-independent) and
/// divergent under a man-in-the-middle. 128 bits of output resist an offline
/// birthday-collision grind (~2^64). Rendered as eight space-separated groups of
/// four uppercase hex digits.
#[must_use]
pub fn pairing_code(pub_a: &[u8; 32], pub_b: &[u8; 32]) -> String
```

Canonical order the two keys by their byte value, hash
`"peerbeam-pairing-v1" ‖ lo ‖ hi` with SHA-256, take the first 16 bytes, hex-
encode uppercase, and group into 8×4. Order-independence makes it symmetric
without either peer knowing who spoke first. Deriving from public keys (not the
`Fingerprint` hex string) keeps the function a pure `&[u8;32]`→`String` with no
dependency on the `Fingerprint` type.

### Transfer — `Session.pairing_code`

`authenticate()` already holds `our_pub` (`identity.keypair.public.0`) and
`peer_pub`. Compute `pairing_code(&our_pub, &peer_pub)` and store it on the
returned `Session`. The non-handshake `Session` constructors (resume/relay paths
in `secure.rs` and `session/crypto.rs`, and any test builders) set
`pairing_code: String::new()` — those are already-trusted continuations, never
first contact, and the gate only fires on `newly_trusted`.

### Config — the toggle

Add `require_pairing_confirmation: bool` to `DeviceConfig`, `#[serde(default)]`
(false). It sits beside `auto_accept_trusted`: both govern first-contact/accept
behavior. Default false preserves zero-config.

### Trust — `forget`

`FsTrust::forget(&DeviceId) -> Result<bool>` removes a device's trust record via
the same atomic temp-write-then-rename path as `approve`, returning whether the
record existed (`Ok(false)` if absent, never an error). Used to un-pin a key when
the pairing code is rejected.

### Surfaces — CLI and FFI

- **CLI:** at first contact (`session.newly_trusted`), always display
  `session.pairing_code` (human text) and include it in the JSON output beside
  `newly_trusted`. When `require_pairing_confirmation` is set, prompt the user to
  confirm the codes match **before** approval proceeds: confirm → continue to the
  normal accept flow; decline → call `forget(peer_id)` and abort the transfer as
  a suspected MITM. When the toggle is off, the code is shown as information and
  does not block.
- **FFI:** expose `pairing_code` (and the existing `newly_trusted`) on whatever
  first-contact/session view FFI returns, and make `forget` callable (mirroring
  the existing `approve` call at `transfer.rs:974`) so the Flutter layer can
  present a confirm dialog and revoke on mismatch. No Flutter widgets in this
  plan.

## Data flow

First contact, toggle on: A and B complete the handshake → each derives the same
`pairing_code` from `(pub_A, pub_B)` → each frontend displays it → the user
compares the two screens. Match → both confirm → normal approval (I6) →
`approve` pins as trusted. Mismatch → decline → `forget` deletes the pinned
record → transfer aborts. Under a real MITM, A shows `code(pub_A, pub_M)` and B
shows `code(pub_M', pub_B)`, which differ, so the user sees a mismatch.

Toggle off: unchanged behavior — the code is displayed for optional verification
but never blocks.

## Error handling

- Empty `pairing_code` (non-handshake sessions) never triggers the gate.
- `forget` on an absent device → `Ok(false)`, not an error.
- Corrupt trust file on `forget` → `Err` (same as `approve`), never silent.
- No `unwrap`/`expect`/`panic!`/`unsafe` in library code; IO/serde/lock errors
  map to `Result`.

## Testing

- **crypto (`pairing_code`):** stable (same pair → same code); order-independent
  (`pairing_code(a,b) == pairing_code(b,a)`); distinct (different key → different
  code); format (39 chars: 32 hex + 7 spaces, uppercase, 8 groups of 4); a MITM
  triple `(A, M, B)` yields `code(A,M) != code(M,B)` (the check actually detects
  the substitution).
- **trust-fs (`forget`):** removes a pinned record and returns `true`, then
  `false` on a second call; the device is no longer `is_trusted`; survives reopen
  (persisted).
- **auth (`Session.pairing_code`):** non-empty after a real handshake and equal
  across the two honest peers; differs when a peer presents a different key.
- **CLI:** toggle on + confirm-yes → transfer proceeds and the peer ends up
  approved; toggle on + confirm-no → `forget` is called and the transfer aborts,
  peer not trusted; toggle off → code is shown, transfer not blocked; JSON output
  carries `pairing_code`.

## Files

- Modify: `rust/crates/peerbeam-crypto/src/lib.rs` — add `pairing_code` + tests.
- Modify: `rust/crates/peerbeam-transfer/src/auth.rs` — `Session.pairing_code`
  field + compute in `authenticate()`; update other `Session` constructors in
  `rust/crates/peerbeam-transfer/src/secure.rs` and
  `rust/crates/peerbeam-transfer/src/session/crypto.rs` to set it empty.
- Modify: `rust/crates/peerbeam-config/src/lib.rs` — `DeviceConfig.require_pairing_confirmation`.
- Modify: `rust/crates/peerbeam-trust-fs/src/lib.rs` — `FsTrust::forget` + tests.
- Modify: `bins/peerbeam-cli/src/commands.rs` — display code, gate on toggle,
  revoke on decline, add `pairing_code` to JSON.
- Modify: `rust/crates/peerbeam-ffi/src/transfer.rs` (and any session view) —
  expose `pairing_code`, make `forget` reachable.
- Modify: `docs/SECURITY.md` — pairing-code note.
- Modify: `docs/FEATURE_CATALOG.md` — reword the Phase-A "6-digit PIN" entry to
  "safety-number compare" (derived doc; no amendment).

## Risks

- **Code length vs UX.** 32 hex chars is longer than a 6-digit PIN. Accepted: it
  is the minimum that resists grinding, this is an opt-in feature for users who
  want MITM protection, and comparison in 8 groups is manageable. QR/scan is a
  possible later convenience, not precluded.
- **Stable (nonce-independent) code.** Because it derives from long-term keys,
  the code is constant across sessions — a re-verification benefit, and it does
  not weaken detection (the identity-key binding is exactly what MITM subverts).
- **Gate lives in frontends.** The Rust core exposes the code and revocation but
  does not itself block approval; each surface must enforce the confirm. This
  plan wires CLI + FFI; Flutter enforcement is a tracked follow-up.
- **Revocation is destructive.** `forget` deletes a pin; a false-alarm decline
  simply re-pins on the next handshake (first contact again), so the cost is one
  extra confirmation, not lockout.
