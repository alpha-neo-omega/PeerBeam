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
char*    pb_discovery_start(void);             // {"discovering":true,"port"?}
char*    pb_discovery_stop(void);
char*    pb_devices_json(void);                // {"devices":[…]}
```

`pb_abi_version` is bumped on any breaking change to a signature or the
envelope/DTO shape. Error codes: `not_initialised`, `invalid_argument`,
`connection`, `integrity`, `cancelled`, `storage`, `transfer`, `encryption`,
`unimplemented`, `queue_unreadable`, `internal`.

`queue_unreadable` is narrower than the rest: `pb_chat_delete` and
`pb_chat_delete_messages` return it when the shared outbox holds an entry they
cannot decode, so the delete is refused rather than risking a queued file's only
staged copy (see `ChatStore::delete_conversation`). Unlike every other code,
retrying the exact same call will not clear it on its own — the offending entry
may not even belong to the conversation being deleted, since the outbox is
shared across every peer. Any other failure of the same calls still reports
`internal`.

`pb_discovery_start`'s result additionally carries `port`: the UDP discovery
port actually bound (`peerbeam_config::DiscoveryConfig::port`, default
`DEFAULT_DISCOVERY_PORT`; `0` requests an OS-assigned one — see
[DISCOVERY.md](DISCOVERY.md)), read back via `UdpDiscovery::bound_port()` once
`start_discovery` has returned. Purely additive: `discovering` keeps its
existing key and a caller that ignores `port` is unaffected.

### Transfer (M2, additive — ABI still v1)

```c
char* pb_transfer_send(const char* json);        // {peer:{name,addresses[],port}, paths:[…]} → {ids:[…]}
char* pb_transfer_send_folder(const char* json); // {peer, path} → {id}
char* pb_transfer_pause(const char* json);       // {id}
char* pb_transfer_resume(const char* json);      // {id}
char* pb_transfer_cancel(const char* json);      // {id}
char* pb_transfer_accept(const char* json);      // {id}  approve an incoming transfer
char* pb_transfer_reject(const char* json);      // {id}
char* pb_transfers_active(void);                 // {transfers:[{id,direction,peer,file,status,stats}]}
char* pb_transfer_get(const char* json);         // {id} → {transfer} | invalid_argument
char* pb_history_get(void);                       // {history:[…]}
```

`pb_init` also starts a **receive server** on `transfer.port` so incoming
transfers can be accepted/rejected. `subscribe_to_transfer_events` is the M1
`pb_set_event_callback` — one stream carries everything, tagged by `type`.

**Stats** (in `transfer_progress` and `pb_transfer_get`):
`{transferred_bytes, total_bytes, current_speed, average_speed, eta_secs}`.

### Transfer events

Every transfer event: `{ "type", "transfer_id", "timestamp": <rfc3339>,
"payload": {…} }`. Types: `transfer_queued`, `transfer_started`,
`transfer_progress` (payload = `{stats, file}`), `transfer_paused`,
`transfer_resumed`, `transfer_retrying`, `transfer_completed`,
`transfer_cancelled`, `transfer_failed` (payload = `{error:{code,message}}`),
plus `history_updated`. Per-transfer ordering is guaranteed — each transfer's
events are emitted from its own task in sequence.

### Concurrency & performance

Multiple transfers run at once, each its own background task on the shared
runtime (they continue across UI navigation). Control (pause/resume/cancel) is
by id via a shared `TransferControl`. **No file bytes cross FFI** — send takes
paths, receive writes to the configured save directory; only ids, metadata,
progress, and stats are marshalled. Folder receive is dispatched FFI-side with a
`PeekLink` (peek the first frame → file vs folder receiver), so the transfer
engine's public API is unchanged.

### Sequence diagrams

**Send**
```
Flutter        FFI (Rust)                         Peer
  │ pb_transfer_send({peer,paths})                 │
  │──────────────▶ register + spawn task           │
  │◀── {ids}                                        │
  │            emit transfer_queued                 │
  │            RouteManager.connect ──── dial ─────▶│
  │            authenticate ⇄ SecureLink ⇄─────────▶│
  │            emit transfer_started                │
  │            send_file (streamed) ───chunks──────▶│
  │◀ transfer_progress (× N, ordered)               │
  │◀ transfer_completed + history_updated           │
```

**Receive**
```
Peer                 FFI (Rust)                    Flutter
  │── dial ──────────▶ accept + authenticate        │
  │                    emit transfer_queued ────────▶│ (shows approval)
  │  (parked)          await approval                │
  │                    ◀───────── pb_transfer_accept │
  │                    emit transfer_started ───────▶│
  │── chunks ────────▶ receive_file (to save dir)    │
  │                    ◀ transfer_progress ─────────▶│
  │                    emit transfer_completed ─────▶│
```
(reject → `pb_transfer_reject` → connection closed, `transfer_cancelled`.)

**Cancel**
```
Flutter        FFI (Rust)                          Peer
  │ pb_transfer_cancel({id})                         │
  │──────────────▶ TransferControl.cancel()          │
  │◀ {cancelling:true}                               │
  │            send/receive loop observes cancel ───▶│ Cancel frame
  │◀ transfer_cancelled                              │
```

**Resume** (pause → resume; byte-level resume is engine-side)
```
Flutter        FFI (Rust)
  │ pb_transfer_pause({id}) ─▶ TransferControl.pause()  →  ◀ transfer_paused
  │ pb_transfer_resume({id})─▶ TransferControl.resume() →  ◀ transfer_resumed
```

### Chat (additive — ABI still v1)

```c
char* pb_chat_send(const char* json);           // {peer, text} → {id}
char* pb_chat_send_file(const char* json);      // {peer, path} → {id}  (id is also the transfer id)
char* pb_chat_history(const char* json);        // {peer_id} → {messages:[…]}
char* pb_chat_reconcile(const char* json);      // {peer_id} → {changed}
char* pb_chat_conversations(const char* json);  // {} or null → {peers:[{peer_id,last_timestamp,unread_hint}]}
char* pb_chat_cancel(const char* json);         // {peer_id, message_id} → {cancelled}
char* pb_chat_delete(const char* json);         // {peer_id} → {removed, kept}
char* pb_chat_delete_messages(const char* json);// {peer_id, message_ids:[…]} → {removed, kept:[…]}

char* pb_presence_json(void);                   // {} → {sharing, self:{…}, devices:{id:{…}}}
char* pb_presence_battery(const char* json);    // {percent, charging} → {}  (Android pushes its own reading down)

char* pb_clipboard_sync(const char* json);      // {text, peers:[{name,addresses[],port}]} → {queued, sync}
```

`pb_presence_json` is a snapshot of live state: what this device would share
(`self`), whether it is currently sharing at all (`sharing`, default **false**),
and the last heartbeat received from each peer (`devices`). Nothing here is
persisted — a fresh process reports no peers, which is presence working as
designed rather than a gap. Every field inside a status is optional; a caller
must render an absent one as absent, never as `0`. `ageSeconds` counts from
**our** receipt, not the peer's `sent_at`, because peer clocks are not
synchronised.

`pb_presence_battery` exists because only the platform layer can read a battery
on Android; the Rust collector handles Linux via sysfs and reports `None` on
Windows and macOS by design. It pushes a reading down for the next heartbeat to
carry; it never puts anything on the wire by itself.

Neither call can leak status. Sending is gated by
`peerbeam_presence::gate::may_share_status`, which requires all of: the user's
opt-in (default off), the peer being **trusted** (not configurable), and the
peer having negotiated `PRESENCE_FEAT_STATUS`. A device with sharing off still
receives and displays other devices' status.

`pb_clipboard_sync` is called by the desktop watcher when the user copies
something. It is the *only* way a clip leaves, and the two decisions that need
no network are made synchronously before anything is dialed: with the opt-in
off it returns `{queued: 0, sync: false}` having done nothing at all — no dial,
no handshake, no packet, because "off" must be observably silent rather than
merely undelivered — and an empty or over-cap clip is refused with
`invalid_argument` naming the size and the limit, so a surface can say "too
large to sync" instead of leaving the user wondering. An over-cap clip is
**never truncated**: a shortened clipboard silently corrupts what the user
believes they copied.

**Naming a peer does not send to it.** The remaining gates are per-peer and
cannot be decided until a session exists, so they stay in
`peerbeam_clipboard::gate::may_share_clip`, consulted after the handshake
against the **authenticated** peer rather than the id discovery offered: the
peer must be **trusted** (not configurable) and must have negotiated
`CLIPBOARD_FEAT_CLIP`. Delivery runs one background task per peer, so the call
returns without blocking a watcher on a handshake; a push that fails is dropped
rather than queued, because the clipboard is live state and delivering what was
copied ten minutes ago on top of what has been copied since would be worse than
not delivering it.

There is no receive call: an inbound clip arrives as a `clipboard_received`
event. Receiving is **ungated** — a device with sync off still applies what its
trusted peers send, which is also the only half that can run on Android, where
background clipboard reads are forbidden.

Both deletes are **local only**: nothing goes on the wire and the peer keeps its
own copy. Neither is "unsend". Both leave behind every record that still backs a
**queued** outbound message — a queued file's record is what the drain re-opens
to deliver it, and a *missing* one is read as "nothing will ever settle this",
releasing the entry and deleting the only staged copy of the bytes. The rule
deciding that has exactly one implementation, shared by the two
(`ChatStore`'s `KeepRule`), and covers a file still being staged as well as one
already queued.

They differ only in what they report, because they were asked different
questions: `pb_chat_delete` answers with counts (`kept` is how many records
survived), while `pb_chat_delete_messages` **names** the kept ids, so a surface
can tell the user which of the messages they picked are still on their way out.
An id the conversation does not hold is neither removed nor kept, and an empty
`message_ids` deletes nothing rather than failing.

## Events (no polling)

On `pb_init`, Rust spawns a forwarder subscribing to the engine's device-change
stream and pushes each as a JSON event to the registered callback. Dart wires
that callback with `NativeCallable.listener` (safe cross-isolate delivery) and
republishes to a broadcast `Stream`. Event types (growing per milestone):
`device_added`, `device_updated`, `status_changed`, `latency_changed`,
`device_removed`; (M2) `transfer_started/progress/paused/resumed/finished/
failed`; (M3) `clipboard_updated`, `settings_changed`, `connection_changed`;
(Phase B) `presence_updated`, `clipboard_received`.

The two clipboard events are **not** the same thing and a surface must not
conflate them. `clipboard_updated` is the local slot bridge behind
`pb_clipboard_get`/`set` — "a surface put this here". `clipboard_received`
carries a trusted peer's synced clip (`{device_id, text, sent_at}`) and means
"another machine put this here", which is the only one that needs announcing to
the user.

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
- **M2 (done):** transfer ops (send / send-folder / receive+accept/reject /
  pause / resume / cancel), active/get/history state, live stats, and the full
  transfer event set — wrapping RouteManager + authenticate + SecureLink. Real
  E2E tests over QUIC (send-out, receive-in with accept), events/ordering,
  stats, history. Route migration on the FFI path is deferred (SecureLink
  lifetimes); pause/resume/cancel work.
- **M3 (done):** clipboard, settings, daemon, status, logs — see below.
- **M4 (done):** Dart SDK + repositories — see below.

## Runtime management (M3)

Additive C-ABI functions (ABI still v1); same envelope + typed codes.

```c
// Clipboard (text/url/code auto-classified; images = metadata only)
char* pb_clipboard_get(void);              // {item|null}
char* pb_clipboard_set(const char* json);  // {text} | {kind:"image",mime,size}
char* pb_clipboard_subscribe(void);
// Settings (versioned, persisted under the data dir; applied on next init)
char* pb_settings_get(void);               // {version,transfer_directory,auto_accept,theme,
                                           //  discovery_enabled,notifications,logging,
                                           //  experimental,trusted_devices[]}
char* pb_settings_set(const char* json);   // partial merge → persist → settings_changed
char* pb_settings_reset(void);
// Daemon = the receive server (idempotent; started at init)
char* pb_daemon_start(void);  pb_daemon_stop(void);  pb_daemon_restart(void);
char* pb_daemon_status(void);              // {running, port}
// Status
char* pb_status(void);                     // {runtime,build{version,abi,profile},devices,
                                           //  active_transfers,daemon{running,port},memory_bytes}
// Logs (structured ring buffer; severity/timestamp/source/component/message)
char* pb_logs_get(const char* json);       // {limit?} → {logs:[…]}
char* pb_logs_subscribe(const char* json); // {enabled} toggles log_received events
char* pb_logs_export(const char* json);    // {path?} → {path,count}
```

New events (same `{type,timestamp,payload}` shape): `clipboard_updated`,
`settings_changed`, `daemon_started`, `daemon_stopped`, `daemon_restarted`,
`log_received`. All flow through the single event callback; ordering preserved.

Notes / honest scope:
- **Clipboard** is a local synchronized slot + events; cross-device clipboard
  *over the network* (receive-side detection) is a follow-up.
- **Settings** persist to `<data_dir>/ffi_settings.json` and are versioned; they
  apply to the engine on next `pb_init` (no live engine-mutation API).
- **Logs** are captured by a `tracing` layer installed once via `try_init`; if a
  global subscriber already exists, capture degrades gracefully.
- Thread-safe: clipboard slot, settings file, log ring + emit flag, and daemon
  task/flag are all synchronized; daemon start/stop just (re)spawn/abort the
  receive-server task and never block the UI.

## Dart side (M4)

The Flutter app is now presentation-only; it talks to the engine through a Dart
SDK and never touches `dart:ffi`.

```
Flutter widgets → ChangeNotifier repositories (lib/data) → PeerBeam SDK
  (lib/sdk) → dart:ffi (lib/sdk/ffi) → peerbeam-ffi → Rust engine
```

- **SDK** (`lib/sdk/`): `PeerBeamApi` (interface) + `PeerBeam` (FFI-backed);
  `models.dart` (immutable), `events.dart` (typed `BridgeEvent`),
  `exceptions.dart` (typed `PeerBeamException` per error code),
  `ffi/bindings.dart` (the only `dart:ffi` file). Clean API:
  `initialize`, `startDiscovery`/`stopDiscovery`, `devices`, `sendFile`,
  `sendFolder`, `pause`/`resume`/`cancel`, `accept`/`reject`, `activeTransfers`,
  `history`, `events` (broadcast stream).
- **Repositories** (`lib/data/`): `DiscoveryRepository`, `TransferRepository`,
  `HistoryRepository` — `ChangeNotifier`s driven by the SDK event stream (no
  polling), delegating commands to the engine. They back the existing app state,
  so no widget changes. (Settings stays local until the M3 settings ops land.)
- **Memory ownership:** Rust allocates returned strings; Dart frees them
  (`pb_free_string`) after copying. Dart allocates argument strings and frees
  them. The event `NativeCallable.listener` is held for the SDK's lifetime and
  closed on `shutdown`. A stress/leak test hammers the boundary.
- **Graceful degradation:** if the native library isn't present (e.g. a test
  host, unbuilt platform), `PeerBeam.available` is false and calls throw
  `PeerBeamUnavailable`; the app still runs (empty state).

### Tests
- Repository unit tests over a `FakePeerBeam` (no native lib).
- Real-FFI Dart test (`test/sdk/ffi_test.dart`): `dlopen` the built cdylib,
  init, list, **typed error mapping over real FFI**, **event delivery through
  the callback**, and a stress loop. Skipped if the lib isn't built.

### Platform packaging
- **Linux:** bundled by `linux/CMakeLists.txt` (installs
  `rust/target/{release,debug}/libpeerbeam_ffi.so`). Build the crate first.
- **Windows/macOS/Android/iOS (to wire):** copy `peerbeam_ffi.dll` beside the
  runner / add the `.dylib` to the macOS bundle & `DynamicLibrary.process()` /
  place `libpeerbeam_ffi.so` under `android/app/src/main/jniLibs/<abi>/` /
  static-link for iOS. The loader (`ffi/bindings.dart`) already picks the right
  name per platform.

Until M4, the Flutter app still renders sample data; the Rust boundary it will
consume is what M1–M3 build and test.
