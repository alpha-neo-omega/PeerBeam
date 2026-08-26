import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/data/history_repository.dart';
import 'package:peerbeam/data/transfer_repository.dart';
import 'package:peerbeam/platform/android_integration.dart';
import 'package:peerbeam/platform/bridge.dart';
import 'package:peerbeam/platform/notifications.dart';
import 'package:peerbeam/platform/services.dart';
import 'package:peerbeam/platform/shared_item.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/staging.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// Flush pending microtasks so stream listeners run.
Future<void> flush() => Future(() {});

/// Records bridge interactions for assertions.
class FakeBridge implements PlatformBridge {
  int startCount = 0;
  int stopCount = 0;
  bool? lastActive;
  bool? lastIncoming;
  bool multicast = false;

  /// Every multicast-lock transition asked of the platform, so a test can say
  /// "the lock was never touched" and not merely "it ended up false".
  final List<bool> multicastCalls = [];
  final List<NotificationContent> shown = [];
  bool exempt = false;
  int exemptionRequests = 0;
  int notificationPermissionRequests = 0;

  /// When set, `startForegroundService` fails with it — how Android answers a
  /// foreground-service start made from the background (`MainActivity` turns
  /// `ForegroundServiceStartNotAllowedException` into this channel error).
  PlatformException? refuseStart;

  /// When set, `startForegroundService` waits on it — a stand-in for the
  /// platform round-trip, so a test can inject a second `sync` while the first
  /// is still travelling instead of sleeping and hoping.
  Completer<void>? holdStart;

  /// What `batteryStatus` answers. Null is the honest answer everywhere but
  /// Android, and on a host build whose method channel has no handler.
  BatteryReading? battery;
  int batteryReads = 0;

  /// Platform → Dart events, so a test can deliver one after `start()` has
  /// subscribed (the share/view intents, and the service-stopped notice).
  final StreamController<Map<String, dynamic>> eventSink =
      StreamController<Map<String, dynamic>>.broadcast();

  @override
  Stream<Map<String, dynamic>> events() => eventSink.stream;
  @override
  Future<Map<String, dynamic>?> initialIntent() async => null;
  @override
  Future<void> startForegroundService(
    String title,
    String body, {
    bool active = false,
    bool incoming = false,
  }) async {
    final refusal = refuseStart;
    if (refusal != null) throw refusal;
    final hold = holdStart;
    if (hold != null) await hold.future;
    startCount++;
    lastActive = active;
    lastIncoming = incoming;
  }

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
  Future<void> setMulticastLock(bool enabled) async {
    multicastCalls.add(enabled);
    multicast = enabled;
  }

  @override
  Future<void> requestNotificationPermission() async =>
      notificationPermissionRequests++;

  @override
  Future<BatteryReading?> batteryStatus() async {
    batteryReads++;
    return battery;
  }
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

    test('received includes the peer when known, omits it otherwise', () {
      final withPeer = TransferNotifications.received('f.bin', 'Bob');
      expect(withPeer.title, 'Received f.bin');
      expect(withPeer.body, 'from Bob');

      final withoutPeer = TransferNotifications.received('f.bin', '');
      expect(withoutPeer.body, '');

      // Two received files sharing a display name must not collide: each
      // call gets a distinct, positive id (previously derived from the file
      // name alone, so same-name receives silently replaced one another).
      expect(withPeer.id, isNot(withoutPeer.id));
      expect(withPeer.id, greaterThanOrEqualTo(0));
      expect(withoutPeer.id, greaterThanOrEqualTo(0));

      // A received file is always an incoming transfer → download icon.
      expect(withPeer.incoming, isTrue);
    });

    test('progress is direction-aware for the small icon', () {
      expect(
        TransferNotifications.progress(
          notificationId: 5,
          fileName: 'f.bin',
          percent: 10,
          sending: true,
        ).incoming,
        isFalse,
      );
      expect(
        TransferNotifications.progress(
          notificationId: 5,
          fileName: 'f.bin',
          percent: 10,
          sending: false,
        ).incoming,
        isTrue,
      );
    });
  });

  group('ForegroundServiceController', () {
    test(
      'runs while there is work; wake-lock only while transferring',
      () async {
        final bridge = FakeBridge();
        final svc = ForegroundServiceController(bridge);

        await svc.sync(activeTransfers: 0, receiving: false, incoming: false);
        expect(svc.running, isFalse);
        expect(bridge.startCount, 0);

        // A transfer starts → running, active (wake lock).
        await svc.sync(activeTransfers: 1, receiving: false, incoming: false);
        expect(svc.running, isTrue);
        expect(bridge.startCount, 1);
        expect(bridge.lastActive, isTrue);

        // More work while running → re-delivered, still active.
        await svc.sync(activeTransfers: 2, receiving: false, incoming: false);
        expect(bridge.startCount, 2);
        expect(bridge.lastActive, isTrue);

        // A repeated sync with unchanged state (same title/body/active/
        // incoming) must NOT re-deliver — avoids spamming the platform channel
        // + re-posting the notification on every transfer_progress tick.
        await svc.sync(activeTransfers: 2, receiving: false, incoming: false);
        expect(bridge.startCount, 2);

        // Transfers done but receiving on → stays running, now IDLE (no wake
        // lock) rather than stopping.
        await svc.sync(activeTransfers: 0, receiving: true, incoming: false);
        expect(svc.running, isTrue);
        expect(bridge.stopCount, 0);
        expect(bridge.lastActive, isFalse);

        // Fully idle → stop once.
        await svc.sync(activeTransfers: 0, receiving: false, incoming: false);
        expect(svc.running, isFalse);
        expect(bridge.stopCount, 1);

        // Across that whole lifecycle the multicast lock was never touched:
        // it belongs to discovery, not to this notification (see the
        // MulticastLockController group).
        expect(bridge.multicastCalls, isEmpty);
      },
    );

    test('threads incoming through to the bridge for the direction-aware '
        'small icon', () async {
      final bridge = FakeBridge();
      final svc = ForegroundServiceController(bridge);

      // An active receive → incoming=true reaches the bridge.
      await svc.sync(activeTransfers: 1, receiving: false, incoming: true);
      expect(bridge.lastIncoming, isTrue);

      // An active send → incoming=false.
      await svc.sync(activeTransfers: 1, receiving: false, incoming: false);
      expect(bridge.lastIncoming, isFalse);
    });

    test('a refused start leaves the flag honest and is retried', () async {
      final bridge = FakeBridge()
        ..refuseStart = PlatformException(
          code: 'fgs_denied',
          message: 'startForegroundService() not allowed from background',
        );
      final svc = ForegroundServiceController(bridge);

      // Android 12+ refuses a foreground-service start made from the
      // background. The refusal must neither escape (it used to be dropped by
      // an `unawaited` and reported nowhere) nor be recorded as a success.
      await svc.sync(activeTransfers: 1, receiving: false, incoming: false);
      expect(svc.running, isFalse);
      expect(svc.lastRefusal, contains('not allowed from background'));

      // And it must not latch: with the flag set before the platform call, a
      // single refusal convinced every later sync that a service was already
      // running, so the app never started one again for the rest of its life.
      bridge.refuseStart = null;
      await svc.sync(activeTransfers: 1, receiving: false, incoming: false);
      expect(svc.running, isTrue);
      expect(bridge.startCount, 1);
      expect(svc.lastRefusal, isNull);

      // Nothing was left half-recorded: the retry delivered a fresh state key,
      // so the *next* identical sync is still correctly deduplicated.
      await svc.sync(activeTransfers: 1, receiving: false, incoming: false);
      expect(bridge.startCount, 1);
    });

    test('concurrent syncs issue one start, not one each', () async {
      final bridge = FakeBridge();
      final svc = ForegroundServiceController(bridge);

      // `sync` hangs off a ChangeNotifier that fires on every progress tick,
      // so overlapping calls are normal. The in-flight guard is what replaced
      // the old set-the-flag-before-awaiting trick.
      await Future.wait([
        svc.sync(activeTransfers: 1, receiving: false, incoming: false),
        svc.sync(activeTransfers: 1, receiving: false, incoming: false),
        svc.sync(activeTransfers: 1, receiving: false, incoming: false),
      ]);
      expect(bridge.startCount, 1);
      expect(svc.running, isTrue);
    });

    test('a stop that arrives mid-start is not lost behind it', () async {
      final bridge = FakeBridge()..holdStart = Completer<void>();
      final svc = ForegroundServiceController(bridge);

      // A transfer starts; the platform has not answered yet.
      final starting = svc.sync(
        activeTransfers: 1,
        receiving: false,
        incoming: false,
      );

      // It fails instantly and background-receive is off, so the very next
      // store change says "idle". This has to survive the in-flight start:
      // dropping it leaves an ongoing notification standing over nothing, with
      // no later tick guaranteed to come along and clear it.
      final stopping = svc.sync(
        activeTransfers: 0,
        receiving: false,
        incoming: false,
      );

      bridge.holdStart!.complete();
      await Future.wait([starting, stopping]);

      expect(bridge.startCount, 1);
      expect(bridge.stopCount, 1);
      expect(svc.running, isFalse);
    });
  });

  group('MulticastLockController', () {
    test('follows discovery, and only on a transition', () async {
      final bridge = FakeBridge();
      final lock = MulticastLockController(bridge);

      await lock.setDiscovering(true);
      expect(lock.held, isTrue);
      expect(bridge.multicastCalls, [true]);

      // Idempotent: safe to call from a listener without re-latching.
      await lock.setDiscovering(true);
      expect(bridge.multicastCalls, [true]);

      await lock.setDiscovering(false);
      expect(lock.held, isFalse);
      expect(bridge.multicastCalls, [true, false]);
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

  group('requestNotificationPermission', () {
    test('is invoked through the bridge', () async {
      final bridge = FakeBridge();
      await bridge.requestNotificationPermission();
      expect(bridge.notificationPermissionRequests, 1);
    });
  });

  group('AndroidIntegration send notifications', () {
    HistoryEntry entry(
      String id, {
      required String direction,
      required bool success,
      String file = 'f.bin',
    }) => HistoryEntry(
      id: id,
      direction: direction,
      peer: 'Bob',
      file: file,
      path: '',
      bytes: 10,
      success: success,
      at: '2026-01-01T00:00:00Z',
    );

    test('notifies for newly-settled sends only, skipping pre-existing '
        'history and receives', () async {
      // Pre-existing history entry from a *previous* app run.
      final fake = FakePeerBeam()
        ..historyEntries = [entry('h0', direction: 'sending', success: true)];
      final bridge = FakeBridge();
      final history = HistoryRepository(api: fake);
      // HistoryRepository no longer refreshes in its constructor (it would
      // run before the engine is initialized in production); mirror the real
      // boot sequence and load persisted history before starting the
      // integration, so its notify-baseline is seeded from the *loaded*
      // history rather than an empty list.
      await history.refresh();
      await flush();
      final integration = AndroidIntegration(
        bridge: bridge,
        staging: StagingStore(),
        transfer: TransferRepository(api: fake),
        settings: SettingsStore(
          deviceName: 'd',
          saveDirectory: '/x',
          autoAcceptTrusted: false,
          notifications: true,
          compression: true,
        ),
        history: history,
      );

      await integration.start();
      await flush();
      expect(bridge.notificationPermissionRequests, 1);
      expect(bridge.shown, isEmpty); // cold-start baseline, not a notify

      // A new send completes successfully.
      fake.historyEntries = [
        ...fake.historyEntries,
        entry('h1', direction: 'sending', success: true, file: 'a.bin'),
      ];
      fake.emit(const HistoryUpdated());
      await flush();
      expect(bridge.shown.where((n) => n.title == 'Sent'), hasLength(1));

      // A new send fails.
      fake.historyEntries = [
        ...fake.historyEntries,
        entry('h2', direction: 'sending', success: false, file: 'b.bin'),
      ];
      fake.emit(const HistoryUpdated());
      await flush();
      expect(
        bridge.shown.where((n) => n.title == 'Transfer failed'),
        hasLength(1),
      );

      // A receive completes — handled elsewhere (main.dart); must not
      // double-notify here.
      fake.historyEntries = [
        ...fake.historyEntries,
        entry('h3', direction: 'receiving', success: true, file: 'c.bin'),
      ];
      fake.emit(const HistoryUpdated());
      await flush();
      expect(bridge.shown.where((n) => n.title == 'Sent'), hasLength(1));
      expect(
        bridge.shown.where((n) => n.title == 'Transfer failed'),
        hasLength(1),
      );

      integration.dispose();
    });
  });

  group('AndroidIntegration multicast lock', () {
    AndroidIntegration build(FakeBridge bridge, SettingsStore settings) {
      final fake = FakePeerBeam();
      return AndroidIntegration(
        bridge: bridge,
        staging: StagingStore(),
        transfer: TransferRepository(api: fake),
        settings: settings,
        history: HistoryRepository(api: fake),
      );
    }

    SettingsStore newSettings() => SettingsStore(
      deviceName: 'd',
      saveDirectory: '/x',
      autoAcceptTrusted: false,
      notifications: true,
      compression: true,
    );

    test('survives turning off background receive', () async {
      final bridge = FakeBridge();
      final settings = newSettings();
      final integration = build(bridge, settings);

      // Discovery starts in the same boot sequence, so the lock is up front:
      // the engine's mDNS provider (224.0.0.251) and the UDP provider's
      // broadcasts are both invisible to this device without it.
      await integration.start();
      await flush();
      expect(integration.multicast.held, isTrue);
      expect(bridge.multicast, isTrue);

      // `backgroundReceive` defaults on, so the service is up too.
      expect(integration.service.running, isTrue);

      // The user turns off "Keep receiving in background" — a preference about
      // an ongoing *notification*. The service stops, and that is all it may
      // do: the lock used to come down with it, which silently emptied the
      // device list and left nothing on screen connecting the two.
      await settings.setBackgroundReceive(false);
      await flush();
      expect(integration.service.running, isFalse);
      expect(integration.multicast.held, isTrue);
      expect(bridge.multicast, isTrue);
      expect(bridge.multicastCalls, [true]);

      // Only teardown releases it.
      integration.dispose();
      await flush();
      expect(bridge.multicastCalls, [true, false]);
    });

    test('a refused foreground-service start does not fail boot', () async {
      final bridge = FakeBridge()
        ..refuseStart = PlatformException(code: 'fgs_denied');
      final integration = build(bridge, newSettings());

      // `start` is awaited inside `main`'s boot block, whose catch marks the
      // whole engine as failed — so a refusal reaching it would turn every
      // screen into an error state over a notification that could not be
      // posted. It must be handled where it happens instead.
      await integration.start();
      await flush();
      expect(integration.service.running, isFalse);
      expect(integration.service.lastRefusal, isNotNull);

      // Discovery is unaffected: the lock is not the service's to hold.
      expect(integration.multicast.held, isTrue);
      integration.dispose();
    });
  });

  group('parseServiceStopped', () {
    test('recognises the notice and ignores everything else', () {
      expect(
        parseServiceStopped({'event': 'service_stopped', 'reason': 'timeout'}),
        ServiceStop.timeout,
      );

      // A stop with a reason this build has no wording for is still a stop:
      // the service is gone either way, and the flag has to come down.
      expect(
        parseServiceStopped({'event': 'service_stopped', 'reason': 'wat'}),
        ServiceStop.other,
      );
      expect(parseServiceStopped({'event': 'service_stopped'}), ServiceStop.other);

      expect(parseServiceStopped({'event': 'share', 'text': 'x'}), isNull);
      expect(parseServiceStopped(const {}), isNull);
    });
  });

  group('ForegroundServiceController platform stop', () {
    test('a service Android destroyed is not still reported as running', () async {
      final bridge = FakeBridge();
      final svc = ForegroundServiceController(bridge);

      await svc.sync(activeTransfers: 0, receiving: true, incoming: false);
      expect(svc.running, isTrue);
      expect(bridge.startCount, 1);

      // Android 15+ ends a `dataSync` service at six cumulative hours. Nothing
      // else ever lowers `running`, so before this existed every later sync
      // short-circuited on "already running" and the device stayed silently
      // unreachable for the rest of the process's life.
      svc.platformStopped(ServiceStop.timeout);
      expect(svc.running, isFalse);
      expect(svc.stoppedByPlatform, ServiceStop.timeout);

      // The state being asked for has not changed one bit — and it must still
      // reach the platform, because the delivered-state signature described a
      // service that no longer exists.
      await svc.sync(activeTransfers: 0, receiving: true, incoming: false);
      expect(bridge.startCount, 2);
      expect(svc.running, isTrue);
      expect(svc.stoppedByPlatform, isNull);
    });

    test('a stop this app asked for explains nothing', () async {
      final bridge = FakeBridge();
      final svc = ForegroundServiceController(bridge);

      await svc.sync(activeTransfers: 0, receiving: true, incoming: false);
      svc.platformStopped(ServiceStop.timeout);

      // Turning off "keep receiving" stops the service on purpose. Leaving the
      // old explanation standing would tell the user Android took something
      // away when they put it down themselves.
      await svc.sync(activeTransfers: 0, receiving: true, incoming: false);
      await svc.sync(activeTransfers: 0, receiving: false, incoming: false);
      expect(svc.running, isFalse);
      expect(svc.stoppedByPlatform, isNull);
    });

    test('notifies on what a listener can read, not on every tick', () async {
      final bridge = FakeBridge();
      final svc = ForegroundServiceController(bridge);
      var notifications = 0;
      svc.addListener(() => notifications++);

      await svc.sync(activeTransfers: 1, receiving: false, incoming: false);
      expect(notifications, 1); // not running → running

      // A second transfer changes the notification body, and nothing a
      // listener can read: `sync` runs off progress ticks, so a rebuild here
      // would be one per tick for text no widget renders.
      await svc.sync(activeTransfers: 2, receiving: false, incoming: false);
      expect(notifications, 1);

      svc.platformStopped(ServiceStop.timeout);
      expect(notifications, 2);
    });
  });

  group('BatteryReporter', () {
    /// A reporter over [bridge] that records what reached the engine.
    ({BatteryReporter reporter, List<({int? percent, bool? charging})> pushes})
    build(FakeBridge bridge, {Future<void> Function()? onPush}) {
      final pushes = <({int? percent, bool? charging})>[];
      return (
        reporter: BatteryReporter(
          bridge: bridge,
          push: ({int? percent, bool? charging}) async {
            if (onPush != null) await onPush();
            pushes.add((percent: percent, charging: charging));
          },
        ),
        pushes: pushes,
      );
    }

    test('pushes a reading once, and again only when it changes', () async {
      final bridge = FakeBridge()
        ..battery = const BatteryReading(percent: 64, charging: false);
      final h = build(bridge);

      await h.reporter.refresh();
      expect(h.pushes, [(percent: 64, charging: false)]);

      // A phone sitting still reads the same value every minute; re-crossing
      // the FFI with it changes nothing about the next heartbeat.
      await h.reporter.refresh();
      expect(h.pushes, hasLength(1));

      bridge.battery = const BatteryReading(percent: 63, charging: true);
      await h.reporter.refresh();
      expect(h.pushes, hasLength(2));
      expect(h.pushes.last, (percent: 63, charging: true));
    });

    test('a platform with no battery is never pushed anything', () async {
      // Every desktop, and any Android host whose method channel has no
      // handler. The engine's own collector is the authority there, and a
      // pushed nothing would be this side asserting a measurement it never
      // took.
      final h = build(FakeBridge());
      await h.reporter.refresh();
      await h.reporter.refresh();
      expect(h.pushes, isEmpty);
    });

    test('losing access clears the engine instead of leaving a stale level', () async {
      final bridge = FakeBridge()
        ..battery = const BatteryReading(percent: 12, charging: false);
      final h = build(bridge);
      await h.reporter.refresh();

      // The engine holds the last value it was handed, so silence here would
      // keep sharing 12% long after anything was measuring it.
      bridge.battery = null;
      await h.reporter.refresh();
      expect(h.pushes.last, (percent: null, charging: null));

      // …and having cleared it, there is nothing left to keep clearing.
      await h.reporter.refresh();
      expect(h.pushes, hasLength(2));
    });

    test('a push that throws is retried, not recorded as delivered', () async {
      final bridge = FakeBridge()
        ..battery = const BatteryReading(percent: 80, charging: true);
      var broken = true;
      final h = build(
        bridge,
        onPush: () async {
          if (broken) throw StateError('not_initialised');
        },
      );

      // The engine is still booting. This runs on a timer, so the failure is
      // kept rather than reported once a minute forever.
      await h.reporter.refresh();
      expect(h.pushes, isEmpty);
      expect(h.reporter.lastFailure, isA<StateError>());

      broken = false;
      await h.reporter.refresh();
      expect(h.pushes, [(percent: 80, charging: true)]);
      expect(h.reporter.lastFailure, isNull);
    });

    test('start reads immediately; stop ends the polling', () async {
      final bridge = FakeBridge()
        ..battery = const BatteryReading(percent: 50, charging: false);
      final h = build(bridge);

      await h.reporter.start();
      expect(bridge.batteryReads, 1); // not waiting a minute for the first one
      expect(h.reporter.active, isTrue);

      await h.reporter.start(); // idempotent
      expect(h.reporter.active, isTrue);

      h.reporter.stop();
      expect(h.reporter.active, isFalse);
    });
  });

  group('AndroidIntegration platform stop and battery', () {
    SettingsStore newSettings() => SettingsStore(
      deviceName: 'd',
      saveDirectory: '/x',
      autoAcceptTrusted: false,
      notifications: true,
      compression: true,
    );

    AndroidIntegration build(
      FakeBridge bridge,
      SettingsStore settings, {
      FakePeerBeam? api,
    }) {
      final fake = api ?? FakePeerBeam();
      return AndroidIntegration(
        bridge: bridge,
        staging: StagingStore(),
        transfer: TransferRepository(api: fake),
        settings: settings,
        history: HistoryRepository(api: fake),
        api: api,
      );
    }

    test('a service_stopped notice lowers the flag and asks for it back', () async {
      final bridge = FakeBridge();
      final integration = build(bridge, newSettings());

      await integration.start();
      await flush();
      expect(integration.service.running, isTrue);
      expect(bridge.startCount, 1);

      bridge.eventSink.add({'event': 'service_stopped', 'reason': 'timeout'});
      await flush();

      // "Keep receiving" is still on, so the service is still wanted. Asking
      // again is what actually fixes this when the user is in the app: Android
      // restores the allowance in the foreground.
      expect(bridge.startCount, 2);
      expect(integration.service.running, isTrue);
      expect(integration.service.stoppedByPlatform, isNull);

      integration.dispose();
    });

    test('a refused restart leaves both the cause and the refusal readable', () async {
      final bridge = FakeBridge();
      final integration = build(bridge, newSettings());
      await integration.start();
      await flush();

      // Backgrounded, the allowance is spent and Android says no. The app must
      // not claim a service either way.
      bridge.refuseStart = PlatformException(
        code: 'fgs_denied',
        message: 'time limit already exhausted',
      );
      bridge.eventSink.add({'event': 'service_stopped', 'reason': 'timeout'});
      await flush();

      expect(integration.service.running, isFalse);
      expect(integration.service.stoppedByPlatform, ServiceStop.timeout);
      expect(integration.service.lastRefusal, contains('time limit'));

      integration.dispose();
    });

    test('the battery reaches the engine, and only with an engine to reach', () async {
      final fake = FakePeerBeam();
      final bridge = FakeBridge()
        ..battery = const BatteryReading(percent: 41, charging: true);

      final wired = build(bridge, newSettings(), api: fake);
      await wired.start();
      await flush();
      expect(fake.batteryPushes, [(percent: 41, charging: true)]);
      wired.dispose();

      // Without an engine there is nowhere to push, and the platform is not
      // read at all.
      final plain = FakeBridge()
        ..battery = const BatteryReading(percent: 41, charging: true);
      final unwired = build(plain, newSettings());
      await unwired.start();
      await flush();
      expect(plain.batteryReads, 0);
      expect(unwired.batteryReporter, isNull);
      unwired.dispose();
    });
  });
}
