# Examples

Runnable and copy-paste examples for embedding and driving PeerBeam.

## Runnable Rust example

**`quic_transfer`** — a complete file transfer over the real QUIC transport in
a single process: bind → serve/dial → mutual authentication → `SecureLink` →
`send_file`/`receive_file`, byte-exact. This is the fastest way to understand
the transfer API.

Source: [`rust/bins/peerbeam-cli/examples/quic_transfer.rs`](../rust/bins/peerbeam-cli/examples/quic_transfer.rs)

```bash
cd rust
cargo run --example quic_transfer -p peerbeam-cli
# received hello.txt (30 bytes) → /tmp/pb-example-<pid>/out/hello.txt
```

Verify it compiles as part of the merge gate:

```bash
cd rust && cargo build --examples
```

## FFI init (C ABI)

The engine is exposed as a C-ABI cdylib (`peerbeam-ffi`). Every call returns a
JSON envelope `{"ok":true,"data":…}` or `{"ok":false,"error":{code,message}}`.
Strings returned by Rust are freed with `pb_free_string`.

```c
#include <stdio.h>
// exported by peerbeam-ffi (cdylib)
extern int   pb_abi_version(void);
extern char* pb_version_json(void);
extern char* pb_init(const char* config_json);
extern void  pb_free_string(char* s);

int main(void) {
    char* v = pb_version_json();          // {"ok":true,"data":{...}}
    printf("version envelope: %s (abi %d)\n", v, pb_abi_version());
    pb_free_string(v);
    char* r = pb_init("{}");              // {"ok":true,...}
    pb_free_string(r);
    return 0;
}
```

See [FFI](../docs/FFI.md) for the full call surface and ownership rules.

## Dart SDK usage

The app talks to the engine only through the Dart SDK
(`flutter/lib/sdk`). Events are typed; errors are a sealed exception type.

```dart
import 'package:peerbeam/sdk/peerbeam.dart';

final PeerBeamApi api = PeerBeam();   // FFI-backed implementation
await api.initialize();
api.events.listen((e) => print('event: $e'));

await api.startDiscovery();
final devices = await api.devices();
// ... resolve a PeerTarget, then:
await api.sendFile(target, ['/path/to/movie.mkv']);
```

Repositories (`flutter/lib/data`) wrap this as event-driven `ChangeNotifier`s —
UI listens to them, never to the FFI directly. See [FFI](../docs/FFI.md) and
[UI](../docs/UI.md).

## CLI automation (scripting / SSH / headless)

The CLI is a first-class frontend, script- and SSH-friendly.

```bash
# Receiver (e.g. a headless server)
peerbeam receive

# Sender — by discovered name, or explicit address
peerbeam send movie.mkv --to "living-room"
peerbeam send movie.mkv --addr 100.101.102.103:49600

# Discover and act on the result
peerbeam discover
peerbeam doctor          # environment diagnostics
```

Full reference: [CLI](../docs/CLI.md).

## Error handling

- **Rust / engine** — functions return `Result<_, DomainError>`; variants:
  `Connection`, `Transfer`, `Storage`, `Integrity`, `Cancelled`, `Encryption`,
  … Integrity failures and cancellations are terminal (never retried); other
  errors are transient and drive reconnect-and-resume (see
  [Transfer Protocol](../docs/TRANSFER_PROTOCOL.md)).
- **FFI** — errors arrive as `{"ok":false,"error":{code,message}}` with a
  stable `code`.
- **Dart** — the SDK maps codes to a sealed `PeerBeamException`; the app maps
  those to friendly user text via `friendlyError` (`flutter/lib/sdk/error_text.dart`)
  and never shows raw engine/FFI strings.
