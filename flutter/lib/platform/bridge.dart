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

  const NotificationContent({
    required this.id,
    required this.title,
    required this.body,
    this.ongoing = false,
    this.progress,
  });
}

/// Abstraction over the Android platform channels so the Dart controllers are
/// unit-testable with a fake and are safe no-ops on non-Android platforms.
abstract class PlatformBridge {
  /// Stream of platform → Dart events (share/receive intents, actions).
  Stream<Map<String, dynamic>> events();

  /// Any intent that launched the app cold (share/view), or null.
  Future<Map<String, dynamic>?> initialIntent();

  Future<void> startForegroundService(String title, String body);
  Future<void> stopForegroundService();

  Future<void> showNotification(NotificationContent content);
  Future<void> cancelNotification(int id);

  Future<bool> isIgnoringBatteryOptimizations();
  Future<void> requestIgnoreBatteryOptimizations();

  /// Acquire/release a Wi-Fi multicast lock so mDNS/UDP discovery can receive.
  Future<void> setMulticastLock(bool enabled);
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
  Future<void> startForegroundService(String title, String body) =>
      _invoke('startForegroundService', {'title': title, 'body': body});

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
}
