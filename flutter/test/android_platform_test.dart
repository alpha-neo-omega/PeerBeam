import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/platform/bridge.dart';
import 'package:peerbeam/platform/notifications.dart';
import 'package:peerbeam/platform/services.dart';
import 'package:peerbeam/platform/shared_item.dart';

/// Records bridge interactions for assertions.
class FakeBridge implements PlatformBridge {
  int startCount = 0;
  int stopCount = 0;
  bool multicast = false;
  final List<NotificationContent> shown = [];
  bool exempt = false;
  int exemptionRequests = 0;

  @override
  Stream<Map<String, dynamic>> events() => const Stream.empty();
  @override
  Future<Map<String, dynamic>?> initialIntent() async => null;
  @override
  Future<void> startForegroundService(String title, String body) async =>
      startCount++;
  @override
  Future<void> stopForegroundService() async => stopCount++;
  @override
  Future<void> showNotification(NotificationContent content) async =>
      shown.add(content);
  @override
  Future<void> cancelNotification(int id) async {}
  @override
  Future<bool> isIgnoringBatteryOptimizations() async => exempt;
  @override
  Future<void> requestIgnoreBatteryOptimizations() async => exemptionRequests++;
  @override
  Future<void> setMulticastLock(bool enabled) async => multicast = enabled;
}

void main() {
  group('parseSharedEvent', () {
    test('shared text', () {
      final items = parseSharedEvent({'event': 'share', 'text': 'hello'});
      expect(items, hasLength(1));
      expect(items.single.kind, SharedKind.text);
      expect(items.single.text, 'hello');
    });

    test('shared files with names', () {
      final items = parseSharedEvent({
        'event': 'share',
        'paths': ['content://x/1', 'content://x/2'],
        'names': ['a.jpg', 'b.pdf'],
      });
      expect(items, hasLength(2));
      expect(items[0].kind, SharedKind.file);
      expect(items[0].path, 'content://x/1');
      expect(items[0].name, 'a.jpg');
      expect(items[1].name, 'b.pdf');
    });

    test('view intent', () {
      final items = parseSharedEvent({
        'event': 'view',
        'paths': ['/storage/movie.mkv'],
      });
      expect(items.single.name, 'movie.mkv'); // basename fallback
    });

    test('ignores unknown / empty', () {
      expect(parseSharedEvent({'event': 'other'}), isEmpty);
      expect(parseSharedEvent({'event': 'share'}), isEmpty);
      expect(parseSharedEvent({'event': 'share', 'text': '  '}), isEmpty);
    });
  });

  group('TransferNotifications', () {
    test('service notification reflects state', () {
      expect(
        TransferNotifications.service(
          activeTransfers: 2,
          receiving: false,
        ).body,
        '2 transfers in progress',
      );
      expect(
        TransferNotifications.service(activeTransfers: 0, receiving: true).body,
        'Ready to receive files',
      );
      final s = TransferNotifications.service(
        activeTransfers: 1,
        receiving: false,
      );
      expect(s.ongoing, isTrue);
      expect(s.id, TransferNotifications.serviceId);
    });

    test('progress / complete / failed', () {
      final p = TransferNotifications.progress(
        notificationId: 5,
        fileName: 'f.bin',
        percent: 42,
        sending: true,
      );
      expect(p.title, 'Sending f.bin');
      expect(p.progress, 42);
      expect(
        TransferNotifications.complete(
          notificationId: 5,
          fileName: 'f.bin',
          sending: false,
        ).title,
        'Received',
      );
      expect(
        TransferNotifications.failed(
          notificationId: 5,
          fileName: 'f.bin',
        ).title,
        'Transfer failed',
      );
    });
  });

  group('ForegroundServiceController', () {
    test('starts once on work, stops once when idle', () async {
      final bridge = FakeBridge();
      final svc = ForegroundServiceController(bridge);

      await svc.sync(activeTransfers: 0, receiving: false);
      expect(svc.running, isFalse);
      expect(bridge.startCount, 0);

      await svc.sync(activeTransfers: 1, receiving: false);
      expect(svc.running, isTrue);
      expect(bridge.startCount, 1);
      expect(bridge.multicast, isTrue);

      // More work while running → refresh notification, no second start.
      await svc.sync(activeTransfers: 2, receiving: false);
      expect(bridge.startCount, 1);
      expect(bridge.shown, isNotEmpty);

      // Receiving keeps it alive even with no transfers.
      await svc.sync(activeTransfers: 0, receiving: true);
      expect(svc.running, isTrue);
      expect(bridge.stopCount, 0);

      // Fully idle → stop once, multicast released.
      await svc.sync(activeTransfers: 0, receiving: false);
      expect(svc.running, isFalse);
      expect(bridge.stopCount, 1);
      expect(bridge.multicast, isFalse);
    });
  });

  group('BatteryOptimization', () {
    test('queries and requests exemption', () async {
      final bridge = FakeBridge()..exempt = true;
      final battery = BatteryOptimization(bridge);
      expect(await battery.isExempt(), isTrue);
      await battery.requestExemption();
      expect(bridge.exemptionRequests, 1);
    });
  });
}
