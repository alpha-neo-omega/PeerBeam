/// Desktop notifications — the one file that talks to the notification plugin.
///
/// Android already has a notification path of its own (a foreground service and
/// hand-written channels in `android/`), and it is the one that keeps receiving
/// files while the app is backgrounded, so it is left alone. Desktop had none
/// at all: `docs/DESKTOP.md` recorded "OS notifications ✗", which meant a
/// message arriving while PeerBeam sat behind another window was invisible
/// until someone thought to look.
///
/// **Presentation only**, like `tray.dart`: nothing here is a capability, and
/// nothing here decides whether to notify. That decision is
/// `shouldNotifyForMessage` in `chat_notifications.dart`, which is pure and
/// tested; this file is the part that needs a desktop session to exercise.
library;

import 'package:flutter/foundation.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';

import 'chat_notifications.dart';
import 'desktop_files.dart' show isDesktop;

/// Identifies PeerBeam to the Windows notification platform.
///
/// Windows routes a toast by AppUserModelID and its activation callback by
/// GUID, and it remembers both — a machine that has seen one value keeps
/// showing notifications under it. So these are fixed constants, never
/// generated: a fresh GUID per launch would leave a trail of dead entries in
/// the user's notification settings.
const _kWindowsAppId = 'PeerBeam.PeerBeam';
const _kWindowsGuid = 'a598fbf7-b171-4d65-96f4-abe4c313096a';

/// Shows notifications on macOS, Windows and Linux. A no-op anywhere else, so
/// callers need no platform check.
class DesktopNotifier {
  final FlutterLocalNotificationsPlugin _plugin;

  /// Injected so a test can drive [show] against a fake without a desktop
  /// session — and so [start] can be skipped entirely off desktop.
  DesktopNotifier({FlutterLocalNotificationsPlugin? plugin})
    : _plugin = plugin ?? FlutterLocalNotificationsPlugin();

  bool _started = false;

  /// macOS only: whether the user has been asked yet, and what they said.
  ///
  /// The ask is deferred to the first notification rather than made at launch.
  /// macOS shows a system dialog for it, and a dialog on first run — before
  /// anyone has sent a message, for a feature they have not used — is a prompt
  /// about nothing. Asked once per process; a "no" is remembered for the
  /// session so a chatty peer cannot turn one refusal into a stream of them.
  bool _asked = false;
  bool _permitted = true;

  /// Bring the plugin up and register the tap handler. Returns whether
  /// notifications can be shown at all; false off desktop, and false if the
  /// platform refused to initialise.
  ///
  /// [onOpen] is called with a peer id when someone clicks a notification —
  /// the conversation it came from.
  Future<bool> start({required void Function(String peerId) onOpen}) async {
    if (!isDesktop || _started) return _started;
    void handle(NotificationResponse response) {
      final peerId = response.payload;
      if (peerId != null && peerId.isNotEmpty) onOpen(peerId);
    }

    try {
      final ok = await _plugin.initialize(
        settings: InitializationSettings(
          // Every permission false: see [_asked]. The plugin requests them at
          // initialise time by default, which is the prompt-on-first-run this
          // deliberately avoids.
          macOS: const DarwinInitializationSettings(
            requestAlertPermission: false,
            requestSoundPermission: false,
            requestBadgePermission: false,
          ),
          linux: LinuxInitializationSettings(
            defaultActionName: 'Open',
            defaultIcon: AssetsLinuxIcon('assets/brand/tray/peerbeam.png'),
          ),
          windows: const WindowsInitializationSettings(
            appName: 'PeerBeam',
            appUserModelId: _kWindowsAppId,
            guid: _kWindowsGuid,
          ),
        ),
        onDidReceiveNotificationResponse: handle,
      );
      _started = ok ?? false;
    } catch (_) {
      // A desktop session with no notification server — a bare Linux WM, a
      // Windows build where the toast registration did not take. Not a reason
      // to fail boot: the window is still the primary surface, exactly as with
      // a tray that will not draw.
      _started = false;
    }
    return _started;
  }

  /// Show [notice], asking macOS for permission the first time.
  ///
  /// Silent on failure by design. A notification that will not appear is a
  /// missing convenience; surfacing it as an error inside the app would put a
  /// dialog in front of someone to tell them a dialog did not appear.
  Future<void> show(ChatNotice notice) async {
    if (!_started) return;
    if (!await _ensurePermitted()) return;
    try {
      await _plugin.show(
        id: notice.id,
        title: notice.title,
        body: notice.body,
        // The conversation to open on a click — see `chatThreadKey`.
        payload: notice.threadKey,
        notificationDetails: const NotificationDetails(
          macOS: DarwinNotificationDetails(),
          linux: LinuxNotificationDetails(
            // `im.received` is the freedesktop category for an instant
            // message; a notification server may group or style by it.
            category: LinuxNotificationCategory.imReceived,
          ),
          windows: WindowsNotificationDetails(),
        ),
      );
    } catch (_) {
      // As above: best-effort.
    }
  }

  /// Take down the notification for one conversation — used when the user
  /// opens that thread, so a notice they have plainly acted on does not sit in
  /// the notification centre claiming otherwise.
  Future<void> clear(int id) async {
    if (!_started) return;
    try {
      await _plugin.cancel(id: id);
    } catch (_) {
      // Best-effort.
    }
  }

  /// True when it is safe to post. Only macOS gates this; Linux and Windows
  /// have no per-app permission to request.
  Future<bool> _ensurePermitted() async {
    if (defaultTargetPlatform != TargetPlatform.macOS) return true;
    if (_asked) return _permitted;
    _asked = true;
    try {
      final granted = await _plugin
          .resolvePlatformSpecificImplementation<
            MacOSFlutterLocalNotificationsPlugin
          >()
          ?.requestPermissions(alert: true, sound: true);
      // A null answer means the platform did not say. Treating that as a
      // refusal would silence notifications on a machine that never objected;
      // the post itself is the safe place to find out.
      _permitted = granted ?? true;
    } catch (_) {
      _permitted = false;
    }
    return _permitted;
  }
}
