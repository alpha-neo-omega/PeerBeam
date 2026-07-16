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
      incoming: !sending,
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

  /// A regular (non-clipboard) file finished downloading.
  ///
  /// Uses a unique id per call rather than [idFor] on the file name: two
  /// received files sharing a display name would otherwise collide on the
  /// same notification id and the second would silently replace the first.
  static NotificationContent received(String fileName, String peer) {
    return NotificationContent(
      id: _uniqueReceivedId(),
      title: 'Received $fileName',
      body: peer.isNotEmpty ? 'from $peer' : '',
      incoming: true,
    );
  }

  /// Derive a stable-ish, platform-safe notification id from a string key
  /// (file name or transfer id) — masked to a positive 32-bit value so it
  /// survives the method-channel round trip into a Kotlin `Int`.
  static int idFor(String key) => key.hashCode & 0x7fffffff;

  /// Monotonic counter backing [_uniqueReceivedId], so each received-file
  /// notification gets a distinct id even when file names repeat.
  static int _receivedSeq = 0;

  /// A monotonically increasing, positive 31-bit id reserved for received-file
  /// notifications — distinct from both [idFor]'s hash-based ids and
  /// [serviceId].
  static int _uniqueReceivedId() =>
      0x40000000 + (_receivedSeq++ & 0x3fffffff);
}
