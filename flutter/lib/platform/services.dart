import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart' show PlatformException;

import 'bridge.dart';
import 'notifications.dart';

/// The state [ForegroundServiceController.sync] was last asked to reach.
typedef _Wanted = ({int activeTransfers, bool receiving, bool incoming});

/// Why the platform took the foreground service away without being asked to.
enum ServiceStop {
  /// Android 15+ ends a `dataSync` foreground service once the app has spent
  /// six cumulative hours of it in any 24 — it calls `Service.onTimeout` and
  /// gives the service seconds to leave. `backgroundReceive` defaults on, so
  /// this is the ordinary end of a long idle day rather than an edge case, and
  /// the allowance comes back when PeerBeam is next in the foreground: opening
  /// the app is a real fix, which is worth saying out loud.
  timeout,

  /// The platform stopped it for a reason this build has no wording for. Still
  /// a stop — the flag has to come down either way — but naming a cause we
  /// were not told would be worse than admitting there isn't one.
  other,
}

/// Read a platform event announcing that the foreground service is gone, or
/// null when the event is about something else.
///
/// Pure, like `parseSharedEvent`, so the decision is testable without a
/// channel. Both arrive on `peerbeam/android/events`.
ServiceStop? parseServiceStopped(Map<String, dynamic> event) {
  if (event['event'] != 'service_stopped') return null;
  return event['reason'] == 'timeout' ? ServiceStop.timeout : ServiceStop.other;
}

/// Owns the foreground-service lifecycle. The service must run whenever there
/// is work that has to survive backgrounding — an active transfer or an
/// enabled "keep receiving" mode — and stop otherwise. [sync] is idempotent:
/// it starts/stops the service only on an actual transition, and refreshes the
/// ongoing notification while running.
///
/// A [ChangeNotifier] because [running], [lastRefusal] and [stoppedByPlatform]
/// are the answer to "is this device still reachable in the background?", and
/// that answer changes without anything on screen having asked — the six-hour
/// cap arrives hours later, out of band.
class ForegroundServiceController extends ChangeNotifier {
  final PlatformBridge bridge;

  /// Whether the platform has a service running *for us*. Only ever set from a
  /// completed platform call: Android can refuse a start (see [lastRefusal]),
  /// and a flag set optimistically before the round-trip turns that refusal
  /// into a permanent one — every later [sync] sees "already running" and never
  /// tries again.
  bool _running = false;

  /// Signature of the last delivered state (title/body/active/incoming), so a
  /// [sync] call whose state hasn't meaningfully changed can skip re-posting
  /// the notification. Without this, `sync` is called on every
  /// `transfer_progress` tick and would otherwise spam the platform channel +
  /// re-post the notification many times a second.
  String? _lastKey;

  /// The state most recently asked for, and whether a delivery loop is already
  /// working towards it.
  ///
  /// [sync] is driven by store listeners that fire far faster than a platform
  /// round-trip answers, so requests overlap. Only one is ever in flight and
  /// the newest always wins — but none is *dropped*: the loop re-reads
  /// [_wanted] after each await, so a "we went idle, stop" that arrives while a
  /// start is still travelling is honoured on the next pass rather than lost
  /// behind it, which would leave an ongoing notification standing over
  /// nothing. This is the job the old set-`_running`-first ordering was really
  /// doing, minus the lie about the outcome.
  _Wanted? _wanted;
  bool _busy = false;

  String? _lastRefusal;

  ServiceStop? _stoppedByPlatform;

  /// Set in [dispose]. A platform round-trip started before teardown lands
  /// after it, and a `ChangeNotifier` throws if that landing notifies — the
  /// same guard the repositories use.
  bool _disposed = false;

  ForegroundServiceController(this.bridge);

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  bool get running => _running;

  /// Why the platform last refused to start the service, or null if the last
  /// attempt was accepted. Android 12+ rejects a foreground-service start made
  /// from the background (`ForegroundServiceStartNotAllowedException`), which is
  /// a legitimate answer rather than a bug — but an answer worth keeping, since
  /// the previous code let it vanish into a dropped `Future`.
  String? get lastRefusal => _lastRefusal;

  /// Why the platform last took the service away on its own, or null while it
  /// is running or was stopped because this app asked. Cleared by the next
  /// start the platform accepts.
  ///
  /// Distinct from [lastRefusal], which is Android declining a start we made:
  /// this one is a service that *was* running and is not any more. Rendering
  /// it is the difference between a device that has quietly stopped receiving
  /// and one that says why, and what to do about it.
  ServiceStop? get stoppedByPlatform => _stoppedByPlatform;

  /// The platform is telling us the service it ran for us is gone.
  ///
  /// [_running] is only ever set from a *successful start*, which is what
  /// makes it trustworthy — and what makes this call necessary. Nothing else
  /// ever lowers it: every later [sync] would short-circuit on "already
  /// running", so a device whose six-hour allowance expired overnight would
  /// stay silently unreachable for the rest of the process's life, with the
  /// app still reporting a service that Android destroyed.
  void platformStopped(ServiceStop reason) {
    _running = false;
    // The delivered-state signature describes a service that no longer exists;
    // left standing it would deduplicate away the very next start attempt.
    _lastKey = null;
    _stoppedByPlatform = reason;
    _notify();
  }

  Future<void> sync({
    required int activeTransfers,
    required bool receiving,
    required bool incoming,
  }) async {
    _wanted = (
      activeTransfers: activeTransfers,
      receiving: receiving,
      incoming: incoming,
    );
    if (_busy) return; // the running loop will see this before it finishes
    _busy = true;
    try {
      // Reading and clearing `_wanted` on this side of the await is what lets
      // the newest request win without any request going unseen.
      while (_wanted != null) {
        final wanted = _wanted!;
        _wanted = null;
        await _apply(wanted);
      }
    } finally {
      _busy = false;
    }
  }

  Future<void> _apply(_Wanted wanted) async {
    final wasRunning = _running;
    final wasRefusal = _lastRefusal;
    final wasStop = _stoppedByPlatform;
    final activeTransfers = wanted.activeTransfers;
    final receiving = wanted.receiving;
    final incoming = wanted.incoming;
    final shouldRun = activeTransfers > 0 || receiving;
    // "Active" = a transfer is actually moving bytes. Idle receive-ready keeps
    // the service alive (to accept incoming) but holds no CPU wake lock and
    // shows a static notification — the wake lock + animated notification only
    // engage during an active transfer (battery-friendly background receive).
    final active = activeTransfers > 0;
    final note = TransferNotifications.service(
      activeTransfers: activeTransfers,
      receiving: receiving,
    );
    final key = '${note.title}|${note.body}|$active|$incoming';

    if (!shouldRun) {
      if (_running) {
        _running = false;
        _lastKey = null;
        // Stopped because this app asked, so there is nothing left to explain.
        _stoppedByPlatform = null;
        _notifyIfChanged(wasRunning, wasRefusal, wasStop);
        await bridge.stopForegroundService();
      }
      return;
    }

    // Running with nothing visibly changed → nothing to deliver. Refreshing
    // otherwise is cheap but no longer unconditional.
    if (_running && key == _lastKey) return;

    final started = await _deliver(note, active: active, incoming: incoming);
    _running = started;
    _lastKey = started ? key : null;
    _notifyIfChanged(wasRunning, wasRefusal, wasStop);
  }

  /// Notify only when something a listener can actually read has changed.
  /// [_apply] runs off store listeners that fire on every progress tick, and a
  /// rebuild per tick for a notification body no widget renders is noise.
  void _notifyIfChanged(bool running, String? refusal, ServiceStop? stop) {
    if (running != _running ||
        refusal != _lastRefusal ||
        stop != _stoppedByPlatform) {
      _notify();
    }
  }

  /// (Re)deliver the current state to the platform, reporting whether a service
  /// is running afterwards.
  ///
  /// A [PlatformException] here is Android *declining* — the start came from
  /// the background, or the `dataSync` allowance is spent — not a fault in this
  /// app. Absorbing it at this one spot is what makes a refusal both survivable
  /// and honest: [_running] stays false, so the next [sync]
  /// (typically once the app is in the foreground again, where starts are
  /// allowed) tries once more instead of assuming a service exists.
  Future<bool> _deliver(
    NotificationContent note, {
    required bool active,
    required bool incoming,
  }) async {
    try {
      await bridge.startForegroundService(
        note.title,
        note.body,
        active: active,
        incoming: incoming,
      );
      _lastRefusal = null;
      // A service is running again, so whatever ended the last one is history
      // rather than the current state of this device.
      _stoppedByPlatform = null;
      return true;
    } on PlatformException catch (e) {
      _lastRefusal = e.message ?? e.code;
      return false;
    }
  }
}

/// Owns the Wi-Fi multicast lock.
///
/// Android's Wi-Fi stack drops packets that aren't addressed to this device
/// unless a `MulticastLock` is held — and that is exactly what discovery is
/// made of: mDNS on 224.0.0.251:5353, which the engine runs on Android
/// alongside the UDP provider, and that provider's 255.255.255.255 broadcasts.
/// Without the lock PeerBeam neither sees peers nor is seen by them.
///
/// The lock belongs to the discovery session, which is why it lives here and
/// not in [ForegroundServiceController]. Acquiring it inside `sync` quietly
/// promoted "Keep receiving in background" — a preference about a
/// *notification* — into the switch that decided whether discovery could
/// receive anything at all: turning it off to be rid of the ongoing
/// notification also emptied the device list, with nothing to connect the two.
class MulticastLockController {
  final PlatformBridge bridge;
  bool _held = false;

  MulticastLockController(this.bridge);

  bool get held => _held;

  /// Hold the lock while discovery is running, release it when it stops.
  /// Idempotent: only a real transition reaches the platform, so this is safe
  /// to call from a listener.
  ///
  /// Unlike a foreground-service start this is recorded before the round-trip
  /// answers, which is safe for the opposite reason: no OS policy can refuse a
  /// multicast lock, so there is no refusal for the optimism to hide.
  Future<void> setDiscovering(bool discovering) async {
    if (discovering == _held) return;
    _held = discovering;
    await bridge.setMulticastLock(discovering);
  }
}

/// Battery-optimization exemption. Asking the OS to exempt PeerBeam keeps its
/// sockets alive under Doze during long/background transfers.
class BatteryOptimization {
  final PlatformBridge bridge;
  BatteryOptimization(this.bridge);

  Future<bool> isExempt() => bridge.isIgnoringBatteryOptimizations();
  Future<void> requestExemption() => bridge.requestIgnoreBatteryOptimizations();
}

/// Feeds this device's battery into the engine's presence heartbeat.
///
/// The engine collects every other field of a status itself, but not this one
/// on Android: `peerbeam_platform::battery` reads sysfs on Linux, declines on
/// Windows and macOS by design, and declines on Android because the Rust layer
/// has no route to `BatteryManager` — its comment names the Flutter side as
/// the half that answers there. Without this class the single field a phone is
/// actually asked for is the one field it never shares.
///
/// **Reading is not sharing.** `presenceBattery` only changes what a heartbeat
/// *would* carry; whether any heartbeat leaves is decided by the opt-in setting
/// (default off) and the trusted-only gate inside the engine, neither of which
/// is reachable from here.
class BatteryReporter {
  final PlatformBridge bridge;

  /// Where a reading goes — `PeerBeamApi.presenceBattery`, taken as a function
  /// rather than importing the SDK so this file stays on the platform side of
  /// that boundary and is testable without an engine.
  final Future<void> Function({int? percent, bool? charging}) push;

  /// How often to re-read. Matches `peerbeam_presence::HEARTBEAT_INTERVAL`
  /// (60s): reading faster produces values no heartbeat will ever carry,
  /// reading slower means a heartbeat carrying a level from two beats ago.
  final Duration interval;

  Timer? _timer;

  /// The last reading the engine accepted — the comparison that keeps an
  /// unchanged battery from crossing the FFI every minute. Only set after
  /// [push] returns, so a failed push is retried rather than remembered as
  /// delivered.
  BatteryReading? _delivered;

  /// What the last read or push threw, or null. Kept rather than reported:
  /// this runs on a timer, and a `not_initialised` during boot would otherwise
  /// file the same error report once a minute forever.
  Object? get lastFailure => _lastFailure;
  Object? _lastFailure;

  BatteryReporter({
    required this.bridge,
    required this.push,
    this.interval = const Duration(seconds: 60),
  });

  /// Whether the polling timer is running.
  bool get active => _timer != null;

  /// Read now, then keep reading every [interval]. Idempotent.
  Future<void> start() async {
    if (_timer != null) return;
    _timer = Timer.periodic(interval, (_) => unawaited(refresh()));
    await refresh();
  }

  void stop() {
    _timer?.cancel();
    _timer = null;
  }

  /// Read the battery once and push it if it changed.
  ///
  /// A platform that answers nothing gets no reading pushed at all — that is
  /// every desktop, where the engine's own collector is already the authority.
  /// But a platform that *stops* answering after it has answered gets an
  /// explicit clear, because the engine holds the last value it was handed and
  /// would otherwise keep sharing a level nothing is measuring any more.
  Future<void> refresh() async {
    try {
      final reading = await bridge.batteryStatus();
      if (reading == null) {
        if (_delivered == null) return;
        await push();
        _delivered = null;
      } else {
        if (reading == _delivered) return;
        await push(percent: reading.percent, charging: reading.charging);
        _delivered = reading;
      }
      _lastFailure = null;
    } catch (e) {
      _lastFailure = e;
    }
  }
}
