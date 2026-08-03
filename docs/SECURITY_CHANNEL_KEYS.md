# Security Review — Per-Channel Keys (M4)

> Reviews the cryptographic isolation added in Phase A5 (M4): every PeerSession
> channel derives its own keys from the one authenticated master secret. Scope:
> `session/crypto.rs`, the `SecureLink` refactor, and the channel/control wiring.
> Conforms to [ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md) I5/I6/I10
> and the risk-register entries R1/R11 from
> [PEERSESSION_RISK_REGISTER.md](PEERSESSION_RISK_REGISTER.md).

## Model

- **One handshake per session.** `authenticate()` runs exactly once, over the raw
  control stream, and yields directional master keys (`master_send`,
  `master_recv`) bound to the handshake transcript. Nothing re-handshakes
  per channel.
- **Per-channel derivation.** Each channel key is
  `HKDF-Expand(directional_master, info)` where `info = domain_label ‖ purpose ‖
  channel_id ‖ protocol_version ‖ direction`. The AEAD (AES-256-GCM) and the
  frame-sealing scheme (12-byte counter nonce, strict-sequential replay) are
  unchanged from the pre-M4 `SecureLink`; only the key/nonce *source* changed.
- **Directional keys.** `direction` is forward (initiator→responder) or reverse,
  chosen from this side's role so both peers derive an identical key for the same
  traffic direction (`master_send == peer.master_recv`). No per-endpoint role byte
  is mixed in — that would desync the two sides.

## Findings

### Nonce uniqueness — OK
A nonce is `prefix(4) ‖ counter(8)`. The prefix is derived per (channel,
direction, version); the counter is per-channel, monotonic, and never reused
within a key. Different channels get different prefixes **and** different keys, so
no (key, nonce) pair repeats across channels. Verified: `nonces_are_unique_per_frame`,
`different_channels_get_different_keys`.

### Key uniqueness — OK
`info` includes channel_id, direction, version, and a purpose byte, so every
(channel, direction) pair yields a distinct key, and key-material vs nonce-prefix
material never collide (distinct purpose). The control channel uses a fixed
`CONTROL_VERSION`, distinct from any negotiated data-channel version. Verified:
`different_channels_get_different_keys`, `version_is_mixed_into_derivation`.

### Counter rollover — OK (fails closed)
`advance_send`/`open` advance via `checked_add`; at `u64::MAX` they return an error
rather than wrapping, so a nonce is never reused. A 2^64-frame channel is
unreachable in practice; the guard is exercised directly. Verified:
`counter_overflow_fails_closed`.

### Replay protection — OK, per-channel
Each `ChannelCrypto` owns its own receive counter and accepts only the next
expected counter (strict sequential — QUIC streams are in-order). Replay state is
**not** shared between channels: a replayed or reordered frame on one channel is
rejected and cannot affect another. Verified: `replay_and_reorder_are_rejected`,
plus the integration `frames_are_ordered_per_channel_and_isolated`.

### Key destruction / memory lifetime — OK
`ChannelCrypto` and `SessionCrypto` implement `Drop` that `zeroize()`s their key
material, so keys are wiped when a channel closes (the actor drops its context) and
when the session ends (the manager/session drop). `zeroize` prevents dead-store
elimination. Counters/prefixes are not secret and are not zeroized.

### Error propagation / failure isolation — OK
Open/seal failures surface as `SessionError`. In the channel actor an open failure
(tamper, replay, wrong key) is reported as a **channel-scoped** `ActorEvent::Errored`
that closes only that channel; it never tears down the session or its neighbours.
The control channel treats a failure as fatal for the session (correct — the
control plane is the session). Verified: `tampered_ciphertext_fails_to_open`,
`wrong_channel_cannot_decrypt`, `closing_one_channel_leaves_others_open`.

### Cross-channel decryption / replay — OK
A frame sealed for channel A cannot be opened by channel B (different key → GCM
open fails), and A's counter/replay state is independent of B's. Verified:
`wrong_channel_cannot_decrypt`; cross-channel isolation in the integration suite.

### Cryptographic boundaries — OK
Plaintext exists only in-process, above `ChannelCrypto`. Every session/data frame
is sealed before it reaches the transport; the control channel is sealed
immediately after the (plaintext, ECDH) handshake. A relay or intermediary sees
only ciphertext (I5). The primitive is unchanged; M4 reuses `EncryptionProvider`.

### Attack surface — OK / noted
- Malformed/short frames are rejected before any decrypt attempt
  (`malformed_frame_is_rejected`).
- The plaintext handshake window is the pre-existing `authenticate()` step
  (unchanged by M4); its security is the same as the legacy transfer path.
- HKDF-Expand uses the existing HMAC-SHA256; masters are already high-entropy
  (ECDH + hash), so no Extract step is required.

## Residual risks / recommendations
- **Not yet exercised on the wire by a real capability.** The per-channel sealing
  is proven over the in-memory transport and by unit tests; the real-QUIC path is
  exercised for stream multiplexing (M3) but the *sealed* per-channel path over
  QUIC lands with M5 (transfer-as-a-channel). Recommend a real-QUIC sealed-channel
  test at M5.
- **Control-channel version is fixed** (`CONTROL_VERSION`). If the control frame
  format ever changes incompatibly, bump it deliberately (it is a constant, not
  the negotiated version).
- No change to the handshake's known limitations (TOFU trust, plaintext handshake
  window) — out of M4 scope.

## Conclusion
The per-channel derivation and sealing provide cryptographic isolation between
channels while reusing the audited AEAD + sealing scheme. Every M4 requirement
(independent keys, counters, replay, overflow-fail-closed, zeroization) is
implemented and tested. No invariant is violated.

## M5 addendum — transfer as the first sealed capability

M5 puts file transfer on a PeerSession channel, so the per-channel sealing is now
exercised end-to-end by a real capability (previously only by unit tests and the
control channel).

- **Owned sealed stream.** A transfer channel's stream is handed to the caller as
  an owned `SealedLink` (`session::sealed_link`) — the same `ChannelCrypto` seal /
  open scheme and the same frame codec as `SecureLink`, reused verbatim (one
  implementation of the sealing scheme; `secure::{encode_frame, decode_frame}` are
  shared, not duplicated). The unmodified transfer engine (`send_file` /
  `receive_file`) runs over it exactly as over a directly-dialed `SecureLink`.
- **Fresh per-channel counter, no probe.** Stream channels do **not** send the M3
  probe frame. Both peers start the channel's send/recv counters at 0 and the
  sender's first real frame (the transfer `Meta`) materialises the stream, so the
  counters cannot desync. Keys are derived per channel via HKDF exactly as for
  message channels (M4), so two concurrent transfers use independent keys and
  counters — a nonce is never reused across channels.
- **Failure isolation.** The transfer runs entirely caller-side; the session pump
  is never in its data path. A transfer that errors, is cancelled, or panics
  therefore closes only its own channel — the pump keeps servicing control and
  sibling channels. Verified by `cancelled_transfer_does_not_terminate_session`
  and `concurrent_transfers_use_independent_channels`.
- **Opt-in, legacy untouched.** Session transfer is enabled only by advertising
  `ChannelType::TRANSFER` as a stream capability
  (`SessionConfig::with_stream_channel_type`); the default config carries no stream
  types, so the legacy transfer path is byte-for-byte unchanged and remains the
  default.

### Residual risk update
The M4 "not yet exercised on the wire by a real capability" item is partially
closed: the sealed per-channel path is now exercised end-to-end by the transfer
capability over the in-memory `ChannelTransport` with the **real** authenticated
handshake and **real** per-channel crypto (only the socket is simulated). A
dedicated real-QUIC sealed-transfer test remains a recommended follow-up (the
QUIC transport itself is already covered for multiplexing at M3).
