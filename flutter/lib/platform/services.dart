import 'package:flutter/services.dart' show PlatformException;

import 'bridge.dart';
import 'notifications.dart';

/// The state [ForegroundServiceController.sync] was last asked to reach.
typedef _Wanted = ({int activeTransfers, bool receiving, bool incoming});

/// Owns the foreground-service lifecycle. The service must run whenever there
/// is work that has to survive backgrounding — an active transfer or an
/// enabled "keep receiving" mode — and stop otherwise. [sync] is idempotent:
/// it starts/stops the service only on an actual transition, and refreshes the
/// ongoing notification while running.
class ForegroundServiceController {
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

  ForegroundServiceController(this.bridge);

  bool get running => _running;

  /// Why the platform last refused to start the service, or null if the last
  /// attempt was accepted. Android 12+ rejects a foreground-service start made
  /// from the background (`ForegroundServiceStartNotAllowedException`), which is
  /// a legitimate answer rather than a bug — but an answer worth keeping, since
  /// the previous code let it vanish into a dropped `Future`.
  String? get lastRefusal => _lastRefusal;

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
