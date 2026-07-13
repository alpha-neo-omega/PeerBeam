# FFI Bridge (Flutter ⇄ Rust)

The Flutter app is a **thin client**; the Rust engine is the single source of
truth. They talk over a stable **C ABI** exposed by the `peerbeam-ffi` crate.
No business logic lives in Dart.

```
Flutter → FFI Bridge → Rust Public API → Application → TransferEngine
        → RouteManager → TransferProvider → Networking
```

## Boundary invariants

- **Only strings + one callback pointer cross.** No domain/internal structs are
  exposed — the wire contract is versioned JSON DTOs ([`dto.rs`]).
- **Result envelope.** Every `char*`-returning function yields
  `{"ok":true,"data":…}` or `{"ok":false,"error":{"code","message"}}`. Dart maps
  `code` to a typed exception; raw Rust error/panic text never reaches user code.
- **Panic-safe.** Every `extern "C"` function is `catch_unwind`-wrapped — a Rust
  panic becomes an `internal` error, never undefined behaviour across FFI.
- **Ownership.** Rust allocates every returned string; **Dart frees it with
  `pb_free_string`**. Dart allocates argument strings and frees them itself.
- **No bytes cross.** Files are referred to by **path**; streaming stays inside
  Rust. Large files never enter Dart memory.

## ABI (v1)

```c
uint32_t pb_abi_version(void);                 // integer, checked at startup
char*    pb_version_json(void);                // {"abi","semver"}
char*    pb_init(const char* config_json);     // "" → defaults
void     pb_shutdown(void);
void     pb_set_event_callback(void (*cb)(const char*));  // null clears
void     pb_free_string(char*);
char*    pb_discovery_start(void);
char*    pb_discovery_stop(void);
char*    pb_devices_json(void);                // {"devices":[…]}
```

`pb_abi_version` is bumped on any breaking change to a signature or the
envelope/DTO shape. Error codes: `not_initialised`, `invalid_argument`,
`connection`, `integrity`, `cancelled`, `storage`, `transfer`, `encryption`,
`unimplemented`, `internal`.

## Events (no polling)

On `pb_init`, Rust spawns a forwarder subscribing to the engine's device-change
stream and pushes each as a JSON event to the registered callback. Dart wires
that callback with `NativeCallable.listener` (safe cross-isolate delivery) and
republishes to a broadcast `Stream`. Event types (growing per milestone):
`device_added`, `device_updated`, `status_changed`, `latency_changed`,
`device_removed`; (M2) `transfer_started/progress/paused/resumed/finished/
failed`; (M3) `clipboard_received`, `settings_changed`, `connection_changed`.

## Threading

One global multi-thread tokio runtime owns the engine and all async work, so
background transfers continue across UI navigation. FFI functions are thin and
non-blocking (discovery start/stop are fast); long work runs on the runtime and
surfaces via events. Dart never blocks.

## Platform support

`crate-type = ["cdylib", "staticlib", "rlib"]`: `cdylib` for
Windows/Linux/macOS/Android, `staticlib` for iOS (future), `rlib` so Rust tests
call the C-ABI functions directly. Per-platform packaging (bundling the shared
library into each Flutter runner) is wired in the Dart-integration milestone.

## Testing

- **Rust unit tests** call the `pb_*` functions directly (envelope, panic guard,
  not-initialised path, bad-config).
- **Real FFI test** (`tests/ffi.rs`) `dlopen`s the built cdylib via `libloading`
  and calls the exported symbols the way Dart will — proving symbol export + ABI
  + the string-ownership contract, not just Rust-calling-Rust.

## Status / milestones

- **M1 (done):** foundation — versioning, init/shutdown, event callback, error
  envelope, panic guard, tokio runtime + engine lifecycle, discovery
  (start/stop/list) + device events. Rust + dlopen FFI tests pass.
- **M2:** transfer ops (send / send-folder / receive / pause / resume / cancel)
  + progress/stats events, wrapping `RouteManager` + the recovery driver.
- **M3:** clipboard, settings, history, daemon, status, logs.
- **M4:** Dart bridge (`flutter/lib/bridge/*`) — typed wrappers, models, event
  stream, typed exceptions — wire the stores, add platform build glue, Flutter
  integration + leak tests.

Until M4, the Flutter app still renders sample data; the Rust boundary it will
consume is what M1–M3 build and test.
