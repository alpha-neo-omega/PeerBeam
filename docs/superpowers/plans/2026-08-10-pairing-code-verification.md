# Optional Pairing-Code Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an optional, human-verifiable first-contact pairing code (a Signal-style "safety number" over the two device public keys) so a user can detect a man-in-the-middle when a peer is first pinned.

**Architecture:** The code is derived locally from the two already-exchanged X25519 public keys — no wire field, no round-trip, no new crypto dependency. It lives on the `EncryptionProvider` port beside `fingerprint` (its exact sibling: public-key → human-comparable string), is surfaced on `Session`, and is enforced at the **receiver's** approval boundary (invariant I6). A mismatch revokes the just-pinned key via the existing `FsTrust::remove`.

**Tech Stack:** Rust (workspace at `rust/`), SHA-256 (`sha2`), existing X25519 + AES-GCM crypto, Flutter FFI event bridge (event payload only; no Flutter widgets this plan).

## Global Constraints

- No `unwrap`/`expect`/`panic!`/`unsafe` in **library** code (crates). The CLI **binary** (`bins/peerbeam-cli`) may use `expect` only where it already does, matching existing style. Tests may use `unwrap`.
- Per-task gate, run from `rust/`: `cargo fmt --check` **and** `cargo clippy --workspace --all-targets -- -D warnings` **and** `cargo test --workspace`. All three must pass before commit.
- Commit per task. Every commit message ends with the trailer:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
- **Local commits only. Do not push.**
- Constitutional: no wire change (the code is derived from already-exchanged public keys), strengthens I6, no amendment needed. Do not modify constitutional docs.
- Pairing-code algorithm (fixed, used verbatim everywhere): canonically order the two 32-byte public keys by byte value → `(lo, hi)`; `digest = SHA-256(b"peerbeam-pairing-v1" ‖ lo ‖ hi)`; take the **first 16 bytes** (128 bits); **uppercase** hex; group into **8 groups of 4** hex digits separated by single spaces. Result is exactly **39 characters** (32 hex + 7 spaces), e.g. `4F9A 2C71 8BD3 E056 1A2B 3C4D 5E6F 7788`.

---

## File Structure

- `rust/crates/peerbeam-domain/src/port/encryption.rs` — add `pairing_code` to the `EncryptionProvider` trait (its home beside `fingerprint`).
- `rust/crates/peerbeam-crypto/src/lib.rs` — implement `pairing_code` in `AeadCrypto`; add an uppercase-hex helper; add tests.
- `rust/crates/peerbeam-transfer/src/auth.rs` — add `Session.pairing_code` field; compute it in `authenticate()`.
- `rust/crates/peerbeam-transfer/src/secure.rs`, `rust/crates/peerbeam-transfer/src/session/crypto.rs` — set `pairing_code: String::new()` in the non-handshake `Session` constructors.
- `rust/crates/peerbeam-config/src/lib.rs` — add `DeviceConfig.require_pairing_confirmation: bool` (default false).
- `rust/bins/peerbeam-cli/src/commands.rs` — display the code at all first-contact points; gate the **receiver** on the toggle; revoke on decline via `sc.trust.remove`.
- `rust/crates/peerbeam-ffi/src/transfer.rs` — add `pairing_code` + `newly_trusted` to the `transfer_queued` event payload.
- `docs/SECURITY.md` — pairing-code note.
- `docs/FEATURE_CATALOG.md` — reword the Phase-A entry.

---

## Task 1: Pairing code on the EncryptionProvider port + AeadCrypto impl

**Files:**
- Modify: `rust/crates/peerbeam-domain/src/port/encryption.rs` (trait `EncryptionProvider`, after `fingerprint` at ~line 58)
- Modify: `rust/crates/peerbeam-crypto/src/lib.rs` (impl block ~line 34; helpers ~line 120; tests module ~line 129)

**Interfaces:**
- Consumes: `PublicKey` (`pub struct PublicKey(pub [u8; 32])`) from `peerbeam_domain::port`, already imported in both files.
- Produces: `fn pairing_code(&self, a: &PublicKey, b: &PublicKey) -> String` on `EncryptionProvider`; implemented by `AeadCrypto`. Later tasks call it as `enc.pairing_code(&PublicKey(x), &PublicKey(y))`.

- [ ] **Step 1: Add the trait method with docs**

In `rust/crates/peerbeam-domain/src/port/encryption.rs`, inside `pub trait EncryptionProvider`, immediately after the `fingerprint` method (~line 58), add:

```rust
    /// A stable, human-comparable "safety number" over two device public keys,
    /// for optional first-contact verification. Identical for both peers of an
    /// honest handshake (order-independent) and divergent under a
    /// man-in-the-middle. Rendered as eight space-separated groups of four
    /// uppercase hex digits (39 chars total). Unlike a short PIN, its 128-bit
    /// output resists an offline birthday-collision grind (~2^64).
    fn pairing_code(&self, a: &PublicKey, b: &PublicKey) -> String;
```

- [ ] **Step 2: Write the failing tests in the crypto crate**

In `rust/crates/peerbeam-crypto/src/lib.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn pairing_code_is_stable_order_independent_and_distinct() {
        let c = AeadCrypto::new();
        let a = c.generate_keypair();
        let b = c.generate_keypair();

        // Stable: same pair -> same code.
        assert_eq!(
            c.pairing_code(&a.public, &b.public),
            c.pairing_code(&a.public, &b.public),
        );
        // Order-independent: canonical ordering means swap yields the same code.
        assert_eq!(
            c.pairing_code(&a.public, &b.public),
            c.pairing_code(&b.public, &a.public),
        );
        // Distinct: a different second key yields a different code.
        let d = c.generate_keypair();
        assert_ne!(
            c.pairing_code(&a.public, &b.public),
            c.pairing_code(&a.public, &d.public),
        );
    }

    #[test]
    fn pairing_code_format_is_eight_groups_of_four_uppercase_hex() {
        let c = AeadCrypto::new();
        let a = c.generate_keypair();
        let b = c.generate_keypair();
        let code = c.pairing_code(&a.public, &b.public);

        assert_eq!(code.len(), 39, "32 hex + 7 spaces");
        let groups: Vec<&str> = code.split(' ').collect();
        assert_eq!(groups.len(), 8);
        for g in groups {
            assert_eq!(g.len(), 4);
            assert!(
                g.chars().all(|ch| ch.is_ascii_digit() || ('A'..='F').contains(&ch)),
                "uppercase hex only, got {g}"
            );
        }
    }

    /// The check must actually detect a substituted key: a MITM sits between A
    /// and B with its own key M, so A sees (A,M) and B sees (M,B). Those codes
    /// must differ, or the two users would see matching codes and be fooled.
    #[test]
    fn pairing_code_detects_a_mitm_substitution() {
        let c = AeadCrypto::new();
        let a = c.generate_keypair();
        let b = c.generate_keypair();
        let m = c.generate_keypair();

        let a_sees = c.pairing_code(&a.public, &m.public);
        let b_sees = c.pairing_code(&m.public, &b.public);
        assert_ne!(a_sees, b_sees);
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd rust && cargo test -p peerbeam-crypto pairing_code`
Expected: FAIL — `no method named pairing_code` (trait method not yet implemented for `AeadCrypto`).

- [ ] **Step 4: Implement `pairing_code` in AeadCrypto + an uppercase-hex helper**

In `rust/crates/peerbeam-crypto/src/lib.rs`, inside `impl EncryptionProvider for AeadCrypto` (after `fingerprint`, ~line 100), add:

```rust
    fn pairing_code(&self, a: &PublicKey, b: &PublicKey) -> String {
        // Canonical order so both peers hash identical bytes regardless of who
        // is "a" or "b".
        let (lo, hi) = if a.0 <= b.0 { (&a.0, &b.0) } else { (&b.0, &a.0) };
        let mut hasher = Sha256::new();
        hasher.update(b"peerbeam-pairing-v1");
        hasher.update(lo);
        hasher.update(hi);
        let digest = hasher.finalize();
        // 128 bits -> uppercase hex -> eight groups of four.
        let hex = to_hex_upper(&digest[..16]);
        let mut out = String::with_capacity(39);
        for (i, ch) in hex.chars().enumerate() {
            if i != 0 && i % 4 == 0 {
                out.push(' ');
            }
            out.push(ch);
        }
        out
    }
```

Then add the uppercase-hex helper next to the existing `to_hex` (~line 120):

```rust
fn to_hex_upper(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02X}");
    }
    s
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd rust && cargo test -p peerbeam-crypto pairing_code`
Expected: PASS (all three tests).

- [ ] **Step 6: Run the full gate**

Run: `cd rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS. (Adding a trait method compiles because `AeadCrypto` is the only impl — verified: one impl in the workspace.)

- [ ] **Step 7: Commit**

```bash
git add rust/crates/peerbeam-domain/src/port/encryption.rs rust/crates/peerbeam-crypto/src/lib.rs
git commit -m "feat(crypto): pairing_code safety number on EncryptionProvider

128-bit, order-independent, uppercase hex in 8 groups of 4. Derived from
the two device public keys; sibling of fingerprint. No wire change.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Surface `pairing_code` on `Session`

**Files:**
- Modify: `rust/crates/peerbeam-transfer/src/auth.rs` (`struct Session` ~line 59; `authenticate()` ~lines 200-211)
- Modify: `rust/crates/peerbeam-transfer/src/secure.rs` (`Session { .. }` literals ~lines 196, 205)
- Modify: `rust/crates/peerbeam-transfer/src/session/crypto.rs` (`Session { .. }` literals ~lines 383, 392)
- Test: `rust/crates/peerbeam-transfer/tests/secure.rs` (add a test)

**Interfaces:**
- Consumes: `enc.pairing_code(&PublicKey, &PublicKey)` (Task 1). `PublicKey` is already imported in `auth.rs` (`peerbeam_domain::port::PublicKey`).
- Produces: `Session.pairing_code: String` (empty for non-handshake/resume sessions; the 39-char code for a real first/again handshake).

- [ ] **Step 1: Write the failing test**

In `rust/crates/peerbeam-transfer/tests/secure.rs`, add (the file already runs a real handshake producing `sa`/`sb` — mirror the existing `newly_trusted` test near line 179):

```rust
#[tokio::test]
async fn handshake_produces_matching_pairing_codes() {
    let (sa, sb) = handshake_pair().await;
    assert_eq!(sa.pairing_code.len(), 39);
    assert!(!sa.pairing_code.is_empty());
    // Both honest peers derive the same code.
    assert_eq!(sa.pairing_code, sb.pairing_code);
}
```

If there is no `handshake_pair()` helper, use the same setup the existing `sa`/`sb` test at ~line 179 uses (copy its handshake wiring into this test verbatim — do not factor out shared code unless the file already has such a helper).

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd rust && cargo test -p peerbeam-transfer --test secure handshake_produces_matching_pairing_codes`
Expected: FAIL — `no field pairing_code on type Session`.

- [ ] **Step 3: Add the field to `Session`**

In `rust/crates/peerbeam-transfer/src/auth.rs`, add to `pub struct Session` (after `newly_trusted`, ~line 69):

```rust
    /// A stable, human-comparable code over both devices' public keys, for
    /// optional first-contact MITM verification. Empty for resumed/relayed
    /// sessions (which are never first contact).
    pub pairing_code: String,
```

- [ ] **Step 4: Compute it in `authenticate()`**

In `authenticate()`, in the final `Ok(Session { .. })` (~lines 203-211), add the field. `our_pub` (`[u8;32]`, ~line 119) and `peer_pub` (`[u8;32]`, ~line 129) are both in scope:

```rust
    Ok(Session {
        send_prefix: prefix(&kdf(&send_key, b"peerbeam-nonce")),
        recv_prefix: prefix(&kdf(&recv_key, b"peerbeam-nonce")),
        send_key,
        recv_key,
        peer_id,
        peer_name: peer_name_display,
        newly_trusted,
        pairing_code: enc.pairing_code(&PublicKey(our_pub), &PublicKey(peer_pub)),
    })
```

- [ ] **Step 5: Set the field empty in the other `Session` constructors**

Find every remaining `Session {` literal (they will now fail to compile):

Run: `cd rust && grep -rn "Session {" crates/peerbeam-transfer/src`

For each one in `secure.rs` (~lines 196, 205) and `session/crypto.rs` (~lines 383, 392) — these construct resumed/placeholder sessions without peer public keys — add:

```rust
        pairing_code: String::new(),
```

(Do not compute a code there; those paths have no handshake public keys and are never first contact.)

- [ ] **Step 6: Run the test to verify it passes**

Run: `cd rust && cargo test -p peerbeam-transfer --test secure handshake_produces_matching_pairing_codes`
Expected: PASS.

- [ ] **Step 7: Run the full gate**

Run: `cd rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS. If any other `Session {` literal (e.g. in another test file) fails to compile, add `pairing_code: String::new()` to it.

- [ ] **Step 8: Commit**

```bash
git add rust/crates/peerbeam-transfer
git commit -m "feat(transfer): expose pairing_code on Session

Computed in authenticate() from both handshake public keys; empty for
resumed/relayed sessions (never first contact).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Config toggle `require_pairing_confirmation`

**Files:**
- Modify: `rust/crates/peerbeam-config/src/lib.rs` (`struct DeviceConfig` ~line 46)
- Test: same file's `mod tests` (add one)

**Interfaces:**
- Produces: `DeviceConfig.require_pairing_confirmation: bool` (default `false`). Read later by the CLI as `config.device.require_pairing_confirmation`.

- [ ] **Step 1: Write the failing test**

In `rust/crates/peerbeam-config/src/lib.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn require_pairing_confirmation_defaults_off_and_round_trips() {
        // Default is off (zero-config stays frictionless).
        assert!(!DeviceConfig::default().require_pairing_confirmation);

        // Absent in TOML -> false via serde(default).
        let cfg: EngineConfig = toml::from_str("[device]\nname = \"x\"\n").unwrap();
        assert!(!cfg.device.require_pairing_confirmation);

        // Present -> honored.
        let cfg: EngineConfig =
            toml::from_str("[device]\nrequire_pairing_confirmation = true\n").unwrap();
        assert!(cfg.device.require_pairing_confirmation);
    }
```

If the test module deserializes with `serde_json` rather than `toml`, mirror the existing tests' format instead (use whatever `mod tests` already imports; do not add a new dependency).

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd rust && cargo test -p peerbeam-config require_pairing_confirmation`
Expected: FAIL — `no field require_pairing_confirmation on type DeviceConfig`.

- [ ] **Step 3: Add the field**

In `rust/crates/peerbeam-config/src/lib.rs`, add to `struct DeviceConfig` (after `auto_accept_trusted`), with a doc comment matching the file's style:

```rust
    /// Require an explicit first-contact pairing-code confirmation before
    /// accepting a transfer from a newly pinned peer (optional MITM check).
    /// Off by default so zero-config first contact stays frictionless.
    pub require_pairing_confirmation: bool,
```

`DeviceConfig` derives `Default` and the struct carries `#[serde(default)]`, so `false` is the automatic default and an absent TOML/JSON key deserializes to `false` — no extra attribute needed. If `DeviceConfig` does **not** derive `Default`, add `#[derive(Default)]` (matching sibling config structs) rather than a manual impl.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd rust && cargo test -p peerbeam-config require_pairing_confirmation`
Expected: PASS.

- [ ] **Step 5: Run the full gate**

Run: `cd rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add rust/crates/peerbeam-config/src/lib.rs
git commit -m "feat(config): add device.require_pairing_confirmation (default off)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: CLI — display the code + gate the receiver

The CLI receiver (`serve_loop`) currently auto-accepts and only prints `pinned new peer`. This task: (a) display `session.pairing_code` at all three first-contact points and in JSON output; (b) on the **receiver**, when `config.device.require_pairing_confirmation` is set and the peer is newly pinned, require an interactive "codes match?" confirmation before proceeding — decline revokes the pin (`sc.trust.remove`, which exists) and skips the transfer. The sender paths only display the code (the sender has no approval step; the sending user reads the code aloud to compare). The decision logic is extracted into pure, unit-tested functions; stdin I/O is a thin shell.

**Files:**
- Modify: `rust/bins/peerbeam-cli/src/commands.rs` (first-contact points ~lines 830, 998, 1114; JSON objects ~lines 887, 1037; `SecureCtx` ~line 953)
- Test: same file's `#[cfg(test)]` module (add one; if none exists, create `mod tests` at the end of the file)

**Interfaces:**
- Consumes: `session.pairing_code` (Task 2), `session.newly_trusted`, `session.peer_id`, `config.device.require_pairing_confirmation` (Task 3), `sc.trust: Arc<FsTrust>` with `remove(&DeviceId) -> Result<bool>` (exists at `peerbeam-trust-fs/src/lib.rs:128`).
- Produces: `enum PairingGate { Proceed, Confirmed, Revoke }` and `fn pairing_gate(newly_trusted: bool, require: bool, answer: Option<bool>) -> PairingGate`; `fn read_confirm(reader: &mut impl std::io::BufRead) -> Option<bool>`.

- [ ] **Step 1: Write the failing unit tests**

In `rust/bins/peerbeam-cli/src/commands.rs`, add a test module (or extend the existing one):

```rust
#[cfg(test)]
mod pairing_tests {
    use super::{pairing_gate, read_confirm, PairingGate};
    use std::io::Cursor;

    #[test]
    fn gate_proceeds_when_not_first_contact_or_toggle_off() {
        assert!(matches!(pairing_gate(false, true, None), PairingGate::Proceed));
        assert!(matches!(pairing_gate(true, false, None), PairingGate::Proceed));
    }

    #[test]
    fn gate_confirms_on_yes_and_revokes_on_no_or_no_answer() {
        assert!(matches!(pairing_gate(true, true, Some(true)), PairingGate::Confirmed));
        assert!(matches!(pairing_gate(true, true, Some(false)), PairingGate::Revoke));
        // No answer available (non-interactive / EOF) -> safe default: revoke.
        assert!(matches!(pairing_gate(true, true, None), PairingGate::Revoke));
    }

    #[test]
    fn read_confirm_parses_yes_no_and_eof() {
        assert_eq!(read_confirm(&mut Cursor::new(b"y\n")), Some(true));
        assert_eq!(read_confirm(&mut Cursor::new(b"Yes\n")), Some(true));
        assert_eq!(read_confirm(&mut Cursor::new(b"n\n")), Some(false));
        assert_eq!(read_confirm(&mut Cursor::new(b"\n")), Some(false));
        assert_eq!(read_confirm(&mut Cursor::new(b"")), None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd rust && cargo test -p peerbeam-cli pairing`
Expected: FAIL — `cannot find function pairing_gate` / `read_confirm` / `PairingGate`.

- [ ] **Step 3: Add the pure decision helpers**

In `rust/bins/peerbeam-cli/src/commands.rs` (near the other free helpers, e.g. above `target_device` ~line 896), add:

```rust
/// Outcome of the optional first-contact pairing check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairingGate {
    /// Not first contact, or the toggle is off — proceed without blocking.
    Proceed,
    /// First contact + toggle on + the user confirmed the codes match.
    Confirmed,
    /// First contact + toggle on + declined or no answer — revoke and abort.
    Revoke,
}

/// Decide what to do at first contact. `answer` is `Some(true/false)` for an
/// explicit yes/no, or `None` when no confirmation could be obtained (JSON /
/// non-interactive / EOF), which is treated as a decline (safe default).
pub(crate) fn pairing_gate(newly_trusted: bool, require: bool, answer: Option<bool>) -> PairingGate {
    if !newly_trusted || !require {
        return PairingGate::Proceed;
    }
    match answer {
        Some(true) => PairingGate::Confirmed,
        _ => PairingGate::Revoke,
    }
}

/// Read a yes/no answer from `reader`. `None` on EOF/error (no answer); an
/// empty line counts as "no" (the prompt's default is No).
pub(crate) fn read_confirm(reader: &mut impl std::io::BufRead) -> Option<bool> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        )),
        Err(_) => None,
    }
}
```

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cd rust && cargo test -p peerbeam-cli pairing`
Expected: PASS.

- [ ] **Step 5: Display the code at all three first-contact points**

At each `if newly_trusted && !ctx.json { ctx.line(&ctx.dim(&format!("pinned new peer {peer_id}"))); }` block (~lines 832, 1000, 1116), change the human-readable line to also show the code:

```rust
    if newly_trusted && !ctx.json {
        ctx.line(&ctx.dim(&format!("pinned new peer {peer_id}")));
        ctx.line(&format!("  pairing code: {}", ctx.bold(&session.pairing_code)));
    }
```

- [ ] **Step 6: Add `pairing_code` to the JSON outputs**

In the `sent_folder` JSON object (~line 887) and the `sent` JSON object (~line 1037), and any receive-side JSON object that already carries `newly_trusted`, add the field beside `newly_trusted`:

```rust
            "newly_trusted": newly_trusted,
            "pairing_code": session.pairing_code,
```

(In the receiver `serve_loop`, `session` is in scope where the incoming/first-contact event is emitted — add `pairing_code` there too if a JSON object reports `newly_trusted`/`pinned`.)

- [ ] **Step 7: Gate the receiver on the toggle**

In `serve_loop` (~line 1114), after `newly_trusted`/`peer_id` are bound and the code is displayed, and **before** the transfer is received, insert the gate. `config` is in scope in `serve_loop`:

```rust
        let answer = if config.device.require_pairing_confirmation && newly_trusted {
            if ctx.json {
                None // cannot prompt in JSON/non-interactive mode
            } else {
                ctx.line("Does the pairing code match the other device? [y/N]");
                let mut stdin = std::io::stdin().lock();
                read_confirm(&mut stdin)
            }
        } else {
            None
        };
        match pairing_gate(newly_trusted, config.device.require_pairing_confirmation, answer) {
            PairingGate::Proceed | PairingGate::Confirmed => {}
            PairingGate::Revoke => {
                let _ = sc.trust.remove(&peer_id);
                if ctx.json {
                    ctx.json_line(&json!({
                        "event": "error",
                        "message": "pairing code not confirmed; peer un-pinned",
                        "peer": peer_id,
                    }));
                } else {
                    ctx.line(&ctx.red(
                        "pairing code not confirmed — un-pinned peer (possible MITM); transfer aborted",
                    ));
                }
                session.close().await;
                continue; // move on to the next inbound connection
            }
        }
```

Notes for the implementer: `sc` is the `SecureCtx` built at the top of `serve_loop` (line 1058) and holds `trust: Arc<FsTrust>`; `peer_id` is the `DeviceId` bound from `session.peer_id`. `session.close()` and `continue` match how `serve_loop` already handles a failed inbound (see ~lines 1123-1129). Confirm the surrounding control flow compiles (the block is inside the `loop`).

- [ ] **Step 8: Run the full gate**

Run: `cd rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS. Resolve any `clippy` lints (e.g. borrow of `session.pairing_code` inside `json!` — clone if needed: `"pairing_code": session.pairing_code.clone()`).

- [ ] **Step 9: Commit**

```bash
git add rust/bins/peerbeam-cli/src/commands.rs
git commit -m "feat(cli): show pairing code + gate receiver on require_pairing_confirmation

Displays the first-contact pairing code (human + JSON). When the toggle is
on, the receiver must confirm the code matches before accepting; a decline
(or non-interactive/JSON mode) un-pins the peer via trust.remove and aborts.
Decision logic is pure + unit-tested; stdin is a thin shell.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: FFI — surface `pairing_code` in the incoming-transfer event

Trust revocation is **already** exposed to Flutter (`pb_trust_remove` → `manager().trust_remove()` → `FsTrust::remove`; `TrustRepository.remove` calls it). So Flutter can already un-pin. The only missing piece is the code itself: add `pairing_code` (and `newly_trusted`, currently not forwarded) to the `transfer_queued` event so a future Flutter dialog can display it.

**Files:**
- Modify: `rust/crates/peerbeam-ffi/src/transfer.rs` (`handle_incoming`, `transfer_queued` event ~lines 1011-1015; `session` established ~line 985)
- Test: `rust/crates/peerbeam-ffi/tests/transfer_ffi.rs` (extend an existing incoming-transfer test, or add one asserting the event payload)

**Interfaces:**
- Consumes: `session.pairing_code` (Task 2), `session.newly_trusted`.
- Produces: `transfer_queued` event payload gains `pairing_code` (string) and `newly_trusted` (bool).

- [ ] **Step 1: Write/extend the failing test**

In `rust/crates/peerbeam-ffi/tests/transfer_ffi.rs`, in the test that drives an incoming transfer to the queued/approval point (the one exercising `wait_for_accept`/accept), assert the `transfer_queued` event JSON now carries the fields. Match the file's existing event-capture pattern; the assertion is:

```rust
    // The incoming-transfer event exposes the pairing code for out-of-band
    // verification and whether the peer was newly pinned.
    let queued = /* the captured transfer_queued event's json payload */;
    assert!(queued.get("pairing_code").and_then(|v| v.as_str()).is_some());
    assert_eq!(queued.get("pairing_code").unwrap().as_str().unwrap().len(), 39);
    assert!(queued.get("newly_trusted").and_then(|v| v.as_bool()).is_some());
```

If the existing tests do not capture emitted events, add the minimal capture the harness supports (follow how other tests in this file observe `events::transfer`). If event capture is not feasible in this test harness, instead assert at the unit level that the payload builder includes the fields (extract the `json!` payload into a small pure helper `fn queued_payload(peer: &str, pairing_code: &str, newly_trusted: bool) -> serde_json::Value` and test that) — prefer this if event capture isn't already available.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd rust && cargo test -p peerbeam-ffi transfer`
Expected: FAIL — the `pairing_code`/`newly_trusted` assertion fails (fields absent).

- [ ] **Step 3: Add the fields to the event**

In `handle_incoming` (`rust/crates/peerbeam-ffi/src/transfer.rs`), the `transfer_queued` event (~lines 1011-1015). `session` (with `pairing_code` and `newly_trusted`) is in scope from ~line 985:

```rust
        events::transfer(
            &id,
            "transfer_queued",
            json!({
                "peer": peer,
                "incoming": true,
                "newly_trusted": session.newly_trusted,
                "pairing_code": session.pairing_code,
            }),
        );
```

If the chosen test used the extracted `queued_payload` helper, build the `json!` there and call it here instead, so the tested code path is the shipped one.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd rust && cargo test -p peerbeam-ffi transfer`
Expected: PASS.

- [ ] **Step 5: Run the full gate**

Run: `cd rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS. (If `session` is moved/borrowed later, clone the string: `"pairing_code": session.pairing_code.clone()`.)

- [ ] **Step 6: Commit**

```bash
git add rust/crates/peerbeam-ffi
git commit -m "feat(ffi): expose pairing_code + newly_trusted on transfer_queued

Lets a frontend display the first-contact pairing code for MITM
verification. Revocation is already reachable via pb_trust_remove.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Documentation

**Files:**
- Modify: `docs/SECURITY.md` (add a pairing-code subsection)
- Modify: `docs/FEATURE_CATALOG.md` (reword the Phase-A "Trust hardening + optional PIN pairing" entry ~line 65)

**Interfaces:** none (docs only). No code, no gate beyond `cargo fmt --check` (unaffected).

- [ ] **Step 1: Add the SECURITY.md note**

Append to `docs/SECURITY.md` a subsection (match the file's heading style):

```markdown
## Pairing code (optional first-contact verification)

On first contact PeerBeam pins a peer's public-key fingerprint (trust on first
use). To let a user confirm that pin is the intended peer and not a
man-in-the-middle, each device can display a **pairing code**: a 128-bit
"safety number" derived from both devices' public keys
(`SHA-256("peerbeam-pairing-v1" ‖ lo ‖ hi)`, first 16 bytes, shown as eight
groups of four uppercase hex digits). Both honest peers compute the **same**
code; under a man-in-the-middle each side computes a **different** code.

The 128-bit width resists an offline grind (a short 6-digit code would not —
an attacker could grind substituted keys until the two sides' codes collide).

It is **optional and off by default** (`device.require_pairing_confirmation`).
When enabled, the receiver must confirm the codes match before accepting a
transfer from a newly pinned peer; a mismatch (or a decline) **un-pins** the
peer (treated as a suspected MITM) and aborts. The code is stable across
sessions, so it can be re-verified later. Revoking trust later is available in
the app (Trusted Devices) and CLI.
```

- [ ] **Step 2: Reword the FEATURE_CATALOG entry**

In `docs/FEATURE_CATALOG.md`, find the Phase-A "Trust hardening + optional PIN pairing" entry (~line 65) and replace any "6-digit PIN" wording with the safety-number-compare model, e.g.:

> Optional first-contact **pairing-code (safety-number) comparison**: a
> 128-bit code derived from both device keys, compared out of band to detect a
> man-in-the-middle; off by default. (A short 6-digit code was considered but
> is grindable; the compared code is full-width.)

Keep the rest of the entry (crate references, sizing) intact.

- [ ] **Step 3: Verify formatting doesn't break the gate**

Run: `cd rust && cargo fmt --check`
Expected: PASS (no Rust changed).

- [ ] **Step 4: Commit**

```bash
git add docs/SECURITY.md docs/FEATURE_CATALOG.md
git commit -m "docs: pairing-code verification (SECURITY.md + FEATURE_CATALOG reword)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- Safety-number compare model, 128-bit, from public keys, stable → Task 1 (algorithm) + Global Constraints. ✓
- No wire change / round-trip / new crypto → derived locally in `authenticate` from exchanged pubkeys (Task 2). ✓
- Session exposes the code → Task 2. ✓
- Config toggle default off → Task 3. ✓
- Gate at approval boundary (I6) + revoke on mismatch/decline → Task 4 (receiver gate + `sc.trust.remove`). ✓
- Revocation reuses existing `FsTrust::remove` (spec's "forget") → Task 4/5; already exposed to Flutter via `pb_trust_remove`. ✓ (No separate trust task needed — noted deviation from the spec's "add forget": `remove` already exists.)
- CLI surface (display + JSON + gate) → Task 4. ✓
- FFI expose (`pairing_code` for Flutter dialog) → Task 5. Flutter UI deferred. ✓
- Docs SECURITY.md + FEATURE_CATALOG reword → Task 6. ✓

**Deviations from the spec (intentional, both simplify):**
1. `pairing_code` lives on the `EncryptionProvider` **port** (implemented in `AeadCrypto`), not as a free fn in `peerbeam-crypto`. Reason: `peerbeam-transfer` does not depend on `peerbeam-crypto`; the port keeps the layering the auth module already relies on, and `pairing_code` is the exact sibling of `fingerprint` (also on the port). Zero blast radius — one impl in the workspace.
2. No new `FsTrust::forget` — the identical `FsTrust::remove` already exists and is already exposed to Flutter via `pb_trust_remove`. Revocation "just works"; FFI task shrinks to one event field.
3. CLI gate is on the **receiver** only (sender just displays the code): approval (I6) is a receiver concept, so that is where blocking belongs; the sender has no approval step.

**Placeholder scan:** none — every step has concrete code or an exact command. ✓

**Type consistency:** `pairing_code(&PublicKey, &PublicKey) -> String` (Task 1) matches the call in Task 2; `Session.pairing_code: String` matches CLI (`session.pairing_code`) and FFI use; `pairing_gate(bool, bool, Option<bool>) -> PairingGate` and `read_confirm(&mut impl BufRead) -> Option<bool>` are used exactly as defined; `FsTrust::remove(&DeviceId) -> Result<bool>` matches the existing signature. ✓
