import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

/// Content for a system notification (foreground-service or transfer event).
@immutable
class NotificationContent {
  final int id;
  final String title;
  final String body;

  /// Ongoing notifications can't be dismissed (used for the service).
  final bool ongoing;

  /// 0..100 for a determinate progress bar, or null for none.
  final int? progress;

  /// True when this notification concerns an incoming (receive) transfer —
  /// selects the download small-icon instead of the upload one.
  final bool incoming;

  const NotificationContent({
    required this.id,
    required this.title,
    required this.body,
    this.ongoing = false,
    this.progress,
    this.incoming = false,
  });
}

/// A battery reading taken from the platform.
///
/// Mirrors `peerbeam_platform::Battery` field for field, because that is what
/// it becomes: the engine merges a pushed reading into the status it collects
/// as though it had measured it itself.
@immutable
class BatteryReading {
  /// Charge level, 0-100.
  final int percent;

  /// Whether it is charging right now, or null when the platform reports a
  /// state it cannot classify. Kept apart from [percent] on purpose: the level
  /// says nothing about the direction it is moving.
  final bool? charging;

  const BatteryReading({required this.percent, this.charging});

  @override
  bool operator ==(Object other) =>
      other is BatteryReading &&
      other.percent == percent &&
      other.charging == charging;

  @override
  int get hashCode => Object.hash(percent, charging);

  @override
  String toString() => 'BatteryReading($percent%, charging: $charging)';
}

/// Abstraction over the Android platform channels so the Dart controllers are
/// unit-testable with a fake and are safe no-ops on non-Android platforms.
abstract class PlatformBridge {
  /// Stream of platform → Dart events (share/receive intents, actions).
  Stream<Map<String, dynamic>> events();

  /// Any intent that launched the app cold (share/view), or null.
  Future<Map<String, dynamic>?> initialIntent();

  /// Start/refresh the foreground service. [active] = a transfer is in progress
  /// (drives the CPU wake lock + an animated notification); false = idle
  /// receive-ready (no wake lock, static notification). [incoming] = at least
  /// one active transfer is a receive (selects the download small-icon).
  Future<void> startForegroundService(
    String title,
    String body, {
    bool active = false,
    bool incoming = false,
  });
  Future<void> stopForegroundService();

  Future<void> showNotification(NotificationContent content);
  Future<void> cancelNotification(int id);

  Future<bool> isIgnoringBatteryOptimizations();
  Future<void> requestIgnoreBatteryOptimizations();

  /// Acquire/release a Wi-Fi multicast lock so mDNS/UDP discovery can receive.
  Future<void> setMulticastLock(bool enabled);

  /// Ask for the POST_NOTIFICATIONS runtime permission (Android 13+; a no-op
  /// on older Android, where it's granted implicitly, and off Android).
  Future<void> requestNotificationPermission();

  /// This device's battery, or null when it has none, or when the platform
  /// declines to say.
  ///
  /// Android is the only platform that needs this. `peerbeam_platform::battery`
  /// reads sysfs on Linux and reports nothing on Windows and macOS by design,
  /// and on Android it reports nothing because the Rust layer has no route to
  /// `BatteryManager` — its own comment names the Flutter side as the half that
  /// fills that gap. Reading a battery here does not share it; see
  /// `BatteryReporter` for what happens to the value.
  Future<BatteryReading?> batteryStatus();
}

/// Real Android implementation over method/event channels. Every call is a
/// no-op (or empty/false) off Android so the same controllers run everywhere.
class AndroidBridge implements PlatformBridge {
  static const MethodChannel _method = MethodChannel('peerbeam/android');
  static const EventChannel _event = EventChannel('peerbeam/android/events');

  bool get _enabled =>
      !kIsWeb && defaultTargetPlatform == TargetPlatform.android;

  @override
  Stream<Map<String, dynamic>> events() {
    if (!_enabled) return const Stream.empty();
    return _event.receiveBroadcastStream().map(
      (e) => Map<String, dynamic>.from(e as Map),
    );
  }

  Future<T?> _invoke<T>(String method, [Map<String, dynamic>? args]) async {
    if (!_enabled) return null;
    return _method.invokeMethod<T>(method, args);
  }

  @override
  Future<Map<String, dynamic>?> initialIntent() async {
    if (!_enabled) return null;
    final result = await _method.invokeMethod<Map<Object?, Object?>>(
      'initialIntent',
    );
    return result == null ? null : Map<String, dynamic>.from(result);
  }

  @override
  Future<void> startForegroundService(
    String title,
    String body, {
    bool active = false,
    bool incoming = false,
  }) => _invoke('startForegroundService', {
    'title': title,
    'body': body,
    'active': active,
    'incoming': incoming,
  });

  @override
  Future<void> stopForegroundService() => _invoke('stopForegroundService');

  @override
  Future<void> showNotification(NotificationContent c) =>
      _invoke('showNotification', {
        'id': c.id,
        'title': c.title,
        'body': c.body,
        'ongoing': c.ongoing,
        'progress': c.progress,
        'incoming': c.incoming,
      });

  @override
  Future<void> cancelNotification(int id) =>
      _invoke('cancelNotification', {'id': id});

  @override
  Future<bool> isIgnoringBatteryOptimizations() async =>
      (await _invoke<bool>('isIgnoringBatteryOptimizations')) ?? false;

  @override
  Future<void> requestIgnoreBatteryOptimizations() =>
      _invoke('requestIgnoreBatteryOptimizations');

  @override
  Future<void> setMulticastLock(bool enabled) =>
      _invoke('setMulticastLock', {'enabled': enabled});

  @override
  Future<void> requestNotificationPermission() =>
      _invoke('requestNotificationPermission');

  @override
  Future<BatteryReading?> batteryStatus() async {
    if (!_enabled) return null;
    final Map<Object?, Object?>? map;
    try {
      map = await _method.invokeMethod<Map<Object?, Object?>>('batteryStatus');
    } on MissingPluginException {
      // A host build without the handler answers this. "No reading" is what
      // every non-Android platform already answers and what the presence
      // schema was built to express, so it degrades to an omitted field rather
      // than taking the app down once a minute.
      return null;
    } on PlatformException {
      return null;
    }
    if (map == null) return null;
    final percent = (map['percent'] as num?)?.toInt();
    // Out of range is dropped, not clamped — the engine's own override drops it
    // too (`Status::with_battery_override` ignores anything over
    // MAX_BATTERY_PERCENT), and a clamped 0 would render as a dead battery that
    // was never measured.
    if (percent == null || percent < 0 || percent > 100) return null;
    return BatteryReading(percent: percent, charging: map['charging'] as bool?);
  }
}
