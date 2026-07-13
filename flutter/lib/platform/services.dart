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
  }) async {
    final shouldRun = activeTransfers > 0 || receiving;
    final note = TransferNotifications.service(
      activeTransfers: activeTransfers,
      receiving: receiving,
    );

    if (shouldRun && !_running) {
      _running = true;
      await bridge.startForegroundService(note.title, note.body);
      // Discovery needs the multicast lock while we're actively running.
      await bridge.setMulticastLock(true);
    } else if (!shouldRun && _running) {
      _running = false;
      await bridge.setMulticastLock(false);
      await bridge.stopForegroundService();
    } else if (shouldRun && _running) {
      // Keep the ongoing notification current.
      await bridge.showNotification(note);
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
