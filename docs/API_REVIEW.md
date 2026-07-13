# API Review — M7

Review of the public API surfaces for consistency, documentation, and ergonomic
naming. Scope: the Rust engine embedding API, the C-ABI FFI, and the Dart SDK.
No API was changed in M7 (docs milestone); this records the state and any
naming inconsistencies for a future pre-1.0 pass.

Legend: ✓ Verified · 🟡 Code-reviewed.

## Rust engine API (`peerbeam-domain` ports, `peerbeam-engine`)

- Ports are traits in `peerbeam-domain` (`Link`, `StorageProvider`,
  `EncryptionProvider`, `TrustStore`, `ReliabilityStore`, discovery/transfer
  providers). Adapters implement them; the engine composes them. 🟡 Aligned
  with [Architecture](ARCHITECTURE.md) and [API](API.md).
- `cargo doc --no-deps -p peerbeam-domain` produced **no** missing-doc output on
  the public items sampled. ✓
- Transfer functions (`authenticate`, `send_file`, `receive_file`,
  `send_file_recover`, folder variants) have module- and item-level docs and
  roundtrip/tamper tests. 🟡 The runnable `quic_transfer` example exercises the
  full path. ✓

## FFI (C ABI, `peerbeam-ffi`)

Exported symbols (verified from `#[no_mangle]` in source):

```
pb_abi_version  pb_init  pb_shutdown  pb_version_json  pb_free_string
pb_set_event_callback
pb_discovery_start  pb_discovery_stop  pb_devices_json
pb_transfer_send  pb_transfer_send_folder  pb_transfer_get  pb_transfers_active
pb_transfer_pause  pb_transfer_resume  pb_transfer_cancel
pb_transfer_accept  pb_transfer_reject
pb_history_get
pb_clipboard_get  pb_clipboard_set  pb_clipboard_subscribe
pb_settings_get  pb_settings_set  pb_settings_reset
pb_daemon_start  pb_daemon_stop  pb_daemon_restart  pb_daemon_status
pb_status  pb_logs_get  pb_logs_subscribe  pb_logs_export
```

Consistency (🟡 code-reviewed):

- **Uniform envelope** — every call returns JSON `{"ok":true,"data":…}` or
  `{"ok":false,"error":{code,message}}`. ✓ Matches [FFI](FFI.md).
- **Versioning** — `pb_abi_version` returns the ABI integer (currently `1`);
  `pb_version_json` returns the semantic version inside an envelope. Two
  distinct concepts, correctly separated.
- **Ownership** — Rust allocates returned strings; the caller frees via
  `pb_free_string`. Consistent across all string-returning calls.
- **Naming note** — the surface is `pb_<noun>_<verb>` except a few `pb_<verb>`
  cases (`pb_init`, `pb_shutdown`, `pb_status`). Acceptable; not worth a
  pre-1.0 rename.

## Dart SDK (`flutter/lib/sdk`)

`PeerBeamApi` (interface) implemented by `PeerBeam` (FFI-backed). Verified
signatures:

```dart
Stream<BridgeEvent> get events;
Future<void> initialize({String configJson = ''});
void shutdown();
Future<void> startDiscovery();
Future<void> stopDiscovery();
Future<List<SdkDevice>> devices();
Future<List<String>> sendFile(PeerTarget peer, List<String> paths);
Future<String>       sendFolder(PeerTarget peer, String path);
Future<void> pause/resume/cancel/accept/reject(String id);
Future<List<TransferSnapshot>> activeTransfers();
Future<List<HistoryEntry>> history();
```

Consistency (🟡):

- Interface/impl split is clean — UI depends on `PeerBeamApi`, not the FFI.
- Errors surface as a sealed `PeerBeamException`; the app renders friendly text
  via `friendlyError` and never leaks engine/FFI strings (✓ unit-tested in
  `test/sdk/error_text_test.dart`).
- **Naming note** — Dart uses `startDiscovery`/`stopDiscovery` while the FFI
  uses `pb_discovery_start/stop`. Cosmetic ordering difference across the
  boundary; documented, not changed.

## Findings

| # | Surface | Finding | Action |
|---|---|---|---|
| 1 | Docs (examples) | Example snippets originally used non-existent `pb_version`/`api.init`/`transferSend` | ✓ Fixed to real names (`pb_version_json`, `initialize`, `sendFile`) |
| 2 | FFI ↔ Dart | Minor verb-order naming difference (`discovery_start` vs `startDiscovery`) | Noted; no change (cosmetic, both documented) |
| 3 | Rust docs | No missing-doc lint output on sampled public items | ✓ No action |

## Verdict

The three surfaces are internally consistent, documented, and match each other
semantically. The only concrete defect found — inaccurate names in the new
example snippets — was fixed. No source API change is warranted for Beta.
