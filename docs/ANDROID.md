# Android platform integration

The v2 client's Android layer, split into a **testable Dart layer** behind a
`PlatformBridge` and a thin **native (Kotlin)** implementation. Everything is
a safe no-op off Android, so the same app code runs on desktop unchanged.

## Features

| Feature | How |
|---|---|
| **Share intent** | Manifest `SEND` / `SEND_MULTIPLE` filters (any type). `MainActivity` parses the intent (text or content URIs, display names resolved via `OpenableColumns`) and forwards it to Dart. |
| **Receive intent** | `VIEW` filter → same parse path → staged for sending. |
| **Foreground service** | `PeerBeamService` (type `dataSync`) with an ongoing notification; started when a transfer is active or background-receive is on. |
| **Background sync** | `ForegroundServiceController.sync()` starts/stops the service on transitions and refreshes its notification; the service holds a Wi-Fi lock, a partial wake lock, and (via the activity) a multicast lock so Doze doesn't suspend discovery/transfers. |
| **Clipboard** | Shared text arrives via the share intent and is surfaced on `AndroidIntegration.sharedText`. |
| **Notifications** | One low-importance channel; `NotificationCompat` builders (service, progress, complete, failed). `POST_NOTIFICATIONS` handled gracefully when denied. |
| **Battery optimization** | `BatteryOptimization` queries `isIgnoringBatteryOptimizations` and launches the exemption request; surfaced in Settings. |

## Permissions (trimmed per the Android review)

Networking (`INTERNET`, `ACCESS_WIFI_STATE`, `ACCESS_NETWORK_STATE`,
`CHANGE_WIFI_MULTICAST_STATE`), discovery (`NEARBY_WIFI_DEVICES` with
`neverForLocation`; `ACCESS_FINE_LOCATION` scoped `maxSdkVersion=32`),
service (`FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_DATA_SYNC`, `WAKE_LOCK`),
UX (`POST_NOTIFICATIONS`, `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`, `CAMERA`).

**No legacy storage / media permissions** — files are chosen and saved via the
Storage Access Framework, which grants per-item access.

## Dart layer (`lib/platform/`)

```
bridge.dart              PlatformBridge (interface) + AndroidBridge (channels) + NotificationContent
shared_item.dart         SharedItem + pure parseSharedEvent()
notifications.dart       pure TransferNotifications builders
services.dart            ForegroundServiceController (sync state machine) + BatteryOptimization
android_integration.dart coordinator: events→staging/text, stores→service sync
```

Channels: method `peerbeam/android`, events `peerbeam/android/events`.

## Native (`android/app/src/main/kotlin/com/peerbeam/peerbeam/`)

`MainActivity.kt` (channels + intent parsing + locks + battery),
`PeerBeamService.kt` (foreground service + Wi-Fi/wake locks),
`Notifications.kt` (channel + builders).

## Testing / verification

- **Dart unit/widget** (`flutter test`, 16 pass): `parseSharedEvent`
  (text/files/view/unknown), notification builders, and the
  `ForegroundServiceController` state machine (starts once on work, refreshes
  while running, stops once idle, multicast lock toggled) via a `FakeBridge`;
  plus `BatteryOptimization` delegation.
- **Native**: `flutter build apk --debug` succeeds — manifest merges, Kotlin
  compiles, plugin/AAR config resolves. (Runtime behaviour of intents /
  service / locks requires a device/emulator, not run here.)

### Build note

`desktop_drop` ≥ 0.7 compiles against `compileSdk 36`; the SDK's `android-36`
platform must be installed. Earlier `desktop_drop` versions hardcode a lower
`compileSdk` that conflicts with Flutter's androidx dependencies.

## Not yet

Actual transfers/receive are placeholders until the Rust engine is bridged
(FFI). A SAF/MediaStore `StorageProvider` for content URIs, and runtime
permission request flows (POST_NOTIFICATIONS, NEARBY_WIFI_DEVICES), land with
the engine.
