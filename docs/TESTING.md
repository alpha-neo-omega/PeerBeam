# Testing

PeerBeam's tests are layered to match the architecture: pure logic in unit
tests, wired components in integration tests, and the transfer pipeline
exercised end-to-end over an in-memory `Link` (no network required).

## Run

```
# Rust — whole workspace
cd rust && cargo test --workspace

# Rust — include the >4 GiB large-file check (slow, hashes ~10 GiB)
cargo test --workspace -- --ignored

# Flutter
cd flutter && flutter test
```

## What each category covers

| Category | Where | Covers |
|---|---|---|
| **Unit** | `peerbeam-cli` `commands::config_key_tests`; `peerbeam-domain` `entity::device` | dotted-key `navigate`/`set_path`/`parse_value`; `Device::same_identity` field-by-field |
| **Integration** | `peerbeam-config/tests/config.rs`; `peerbeam-cli/tests/config_cli.rs` | config save/load round-trip, defaults, malformed/missing/unknown fields; CLI `config get/set/show/path` end-to-end through the compiled binary, incl. exit codes |
| **Cross-platform** | `peerbeam-storage-fs` (0600 perms, `#[cfg(unix)]`); `peerbeam-transfer/tests/regression.rs` | owner-only permissions on finalized files; filename sanitization / traversal rejection independent of OS |
| **Stress** | `peerbeam-transfer/tests/stress.rs` | 16 concurrent transfers, each distinct payload, all verify with no cross-talk |
| **Resume** | `peerbeam-transfer/tests/resume.rs`; `tests/recovery.rs`; `tests/folder.rs` | partial `.part` continued not restarted; already-complete `.part`; reconnect-and-resume; per-file folder resume |
| **Large file** | `peerbeam-transfer/tests/largefile.rs` | 128 MiB streamed through a generator→sink storage holding no full copy (constant memory); `#[ignore]`d 5 GiB proves 64-bit sizing |
| **Regression** | `peerbeam-transfer/tests/regression.rs`; `protocol.rs`; `flutter/test/regression_test.dart` | receive-path traversal; zero-copy `chunk_frame_owned` == copying variant; `StatusDot` offline-dispose crash; `DeviceTile` long-name overflow |

## Harness notes

- **`MemLink`** (`peerbeam-transfer/tests/common/mod.rs`) is a bounded in-memory
  duplex `Link` — the channel bound exerts real backpressure, so streaming,
  pause, and cancel behave as they would over a socket, with no network.
- **Streaming proof.** The large-file test uses a `StorageProvider` that
  *generates* source bytes and *discards* received bytes: there is no buffer
  sized to the payload anywhere, yet both ends compute and agree on the
  whole-file SHA-256. That is the structural guarantee behind "never load the
  whole file into RAM".
- **CLI tests** invoke the real binary via `CARGO_BIN_EXE_peerbeam` against a
  throwaway `--config` file, so argument parsing, dotted-key logic, file I/O,
  and exit codes are all covered in one cross-process pass.

## Known gap

No test exercises transfer over a real network socket — everything rides
`MemLink`. That closes when the QUIC `TransferProvider` lands; the pipeline
logic itself is already covered.
