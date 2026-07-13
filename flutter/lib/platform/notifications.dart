import 'bridge.dart';

/// Pure builders for the app's notifications. Kept free of channels so the
/// exact copy/ids are unit-testable.
class TransferNotifications {
  TransferNotifications._();

  /// Fixed id for the ongoing foreground-service notification.
  static const int serviceId = 1;

  /// The persistent notification shown while the foreground service runs.
  static NotificationContent service({
    required int activeTransfers,
    required bool receiving,
  }) {
    final String body;
    if (activeTransfers > 0) {
      body =
          '$activeTransfers transfer${activeTransfers == 1 ? '' : 's'} in progress';
    } else if (receiving) {
      body = 'Ready to receive files';
    } else {
      body = 'Active';
    }
    return NotificationContent(
      id: serviceId,
      title: 'PeerBeam',
      body: body,
      ongoing: true,
    );
  }

  static NotificationContent progress({
    required int notificationId,
    required String fileName,
    required int percent,
    required bool sending,
  }) {
    return NotificationContent(
      id: notificationId,
      title: '${sending ? 'Sending' : 'Receiving'} $fileName',
      body: '$percent%',
      ongoing: true,
      progress: percent.clamp(0, 100),
    );
  }

  static NotificationContent complete({
    required int notificationId,
    required String fileName,
    required bool sending,
  }) {
    return NotificationContent(
      id: notificationId,
      title: sending ? 'Sent' : 'Received',
      body: fileName,
    );
  }

  static NotificationContent failed({
    required int notificationId,
    required String fileName,
  }) {
    return NotificationContent(
      id: notificationId,
      title: 'Transfer failed',
      body: fileName,
    );
  }
}
