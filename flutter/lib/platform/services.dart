import 'bridge.dart';
import 'notifications.dart';

/// Owns the foreground-service lifecycle. The service must run whenever there
/// is work that has to survive backgrounding — an active transfer or an
/// enabled "keep receiving" mode — and stop otherwise. [sync] is idempotent:
/// it starts/stops the service only on an actual transition, and refreshes the
/// ongoing notification while running.
class ForegroundServiceController {
  final PlatformBridge bridge;
  bool _running = false;

  ForegroundServiceController(this.bridge);

  bool get running => _running;

  Future<void> sync({
    required int activeTransfers,
    required bool receiving,
    required bool incoming,
  }) async {
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

    if (shouldRun) {
      if (!_running) {
        _running = true;
        // Discovery needs the multicast lock while we're running.
        await bridge.setMulticastLock(true);
      }
      // (Re)deliver so the service updates its wake lock + notification for the
      // current active/idle state. Idempotent + cheap.
      await bridge.startForegroundService(
        note.title,
        note.body,
        active: active,
        incoming: incoming,
      );
    } else if (_running) {
      _running = false;
      await bridge.setMulticastLock(false);
      await bridge.stopForegroundService();
    }
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
