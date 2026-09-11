/// The desktop tray / menu-bar icon, and what closing the window does.
///
/// Windows calls it the notification area, macOS has no taskbar at all and puts
/// this in the menu bar, and Linux desktops vary. `tray_manager` presents the
/// three as one thing, and this is the only file that knows about it.
///
/// **Presentation only.** Everything the menu shows is state the engine already
/// reports, and every action it offers already exists elsewhere in the app —
/// nothing here is a capability reachable only from the GUI, which is what
/// invariant I7 forbids. The menu's *content* is decided by `tray_model.dart`,
/// a pure function with tests; this file is the part that talks to the plugin
/// and cannot be exercised without a desktop session.
library;

import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:tray_manager/tray_manager.dart';
import 'package:window_manager/window_manager.dart';

import '../state/stores.dart';
import 'desktop_files.dart' show isDesktop;
import 'tray_model.dart';

/// Menu item ids. Strings rather than an enum because that is what the plugin
/// hands back on a click.
const _kOpen = 'open';
const _kSend = 'send';
const _kQuit = 'quit';

/// Owns the tray icon for the app's lifetime.
///
/// A no-op everywhere but desktop, so callers need no platform check: on
/// Android and in a widget test [start] returns without touching a plugin that
/// is not there.
class TrayService with TrayListener, WindowListener {
  final AppState state;

  /// Bring the window back and show it. Injected so a test can observe it
  /// without a window.
  final Future<void> Function() showWindow;

  /// Open the file picker and stage what comes back — the same action Home's
  /// "Send files" button runs.
  final Future<void> Function() sendFiles;

  /// Really exit, past the close-to-tray interception.
  final Future<void> Function() quit;

  bool _started = false;

  TrayService({
    required this.state,
    required this.showWindow,
    required this.sendFiles,
    required this.quit,
  });

  /// Install the icon and begin tracking state. Safe to call on any platform.
  Future<void> start() async {
    if (!isDesktop || _started) return;
    _started = true;
    trayManager.addListener(this);
    windowManager.addListener(this);

    await trayManager.setIcon(_iconPath);
    await _render();

    // The menu is rebuilt from state, so it has to follow state. These are the
    // two repositories the menu reads; a third would need adding here too.
    state.transfer.addListener(_scheduleRender);
    state.device.addListener(_scheduleRender);
    // Close-to-tray is a preference, and turning it off has to take effect
    // without a restart — otherwise the setting is a lie until next launch.
    state.view.addListener(_applyCloseBehaviour);
    await _applyCloseBehaviour();
  }

  /// The icon file, per platform. Windows will not accept a PNG here.
  String get _iconPath => defaultTargetPlatform == TargetPlatform.windows
      ? 'assets/brand/tray/peerbeam.ico'
      : 'assets/brand/tray/peerbeam.png';

  /// The shortest gap between two native menu rebuilds. Rebuilding per event
  /// is wasteful and visibly janky on Windows, where an open menu closes when
  /// it is replaced.
  static const _renderEvery = Duration(milliseconds: 500);

  /// Time since the last completed render, for the throttle below.
  final Stopwatch _sinceRender = Stopwatch();
  Timer? _pending;

  /// Throttle, **not** debounce.
  ///
  /// This was `_pending?.cancel()` followed by a fresh 500 ms timer — a
  /// trailing-edge debounce, which never fires while events keep arriving. The
  /// engine emits `transfer_progress` every 50 ms
  /// (`PROGRESS_INTERVAL`, peerbeam-ffi/src/transfer.rs), so the timer was
  /// cancelled and restarted ten times per interval and the menu was never
  /// redrawn for the whole of any transfer lasting longer than half a second —
  /// which is to say, for every transfer worth showing. The tray's live status
  /// only appeared once the transfer had already finished and left the list.
  ///
  /// A throttle renders on the leading edge and then at most once per
  /// [_renderEvery], so the first event is immediate and a continuous stream
  /// still redraws twice a second.
  void _scheduleRender() {
    if (_pending != null) return; // a render is already booked
    final elapsed = _sinceRender.isRunning
        ? _sinceRender.elapsed
        : _renderEvery;
    if (elapsed >= _renderEvery) {
      unawaited(_render());
      return;
    }
    _pending = Timer(_renderEvery - elapsed, () {
      _pending = null;
      unawaited(_render());
    });
  }

  Future<void> _render() async {
    if (!_started) return;
    _sinceRender
      ..reset()
      ..start();
    final model = trayModel(
      devices: state.device.devices,
      transfers: state.transfer.transfers,
    );

    final items = <MenuItem>[
      MenuItem(key: _kOpen, label: 'Open PeerBeam'),
      MenuItem(key: _kSend, label: 'Send files…'),
    ];

    // Status sections are only drawn when there is something to say. A menu
    // with an empty "Transfers" heading under it reads as broken.
    if (model.transfers.isNotEmpty) {
      items.add(MenuItem.separator());
      items.add(MenuItem(label: 'Transfers', disabled: true));
      items.addAll(model.transfers.map(_item));
    }
    if (model.devices.isNotEmpty) {
      items.add(MenuItem.separator());
      items.add(MenuItem(label: 'Online', disabled: true));
      items.addAll(model.devices.map(_item));
    }

    // Always last, and always present. The one thing a tray app must never do
    // is become unquittable — especially this one, which keeps receiving files
    // while the window is closed.
    items.add(MenuItem.separator());
    items.add(MenuItem(key: _kQuit, label: 'Quit PeerBeam'));

    try {
      await trayManager.setToolTip(model.tooltip);
      await trayManager.setContextMenu(Menu(items: items));
      _menuDrew = true;
    } catch (_) {
      // A tray that will not draw is not a reason to take the app down with
      // it: the window is the primary surface and still works. Some Linux
      // desktops have no status-notifier host at all.
      //
      // It *is* a reason not to hide the window into it — see
      // `_applyCloseBehaviour`.
      _menuDrew = false;
      await _applyCloseBehaviour();
    }
  }

  /// Whether the menu has been handed to the OS successfully at least once.
  ///
  /// Close-to-tray is only safe while there is a tray to close into. If the
  /// icon or its menu will not draw — the case the catch above anticipates, a
  /// Linux session with no status-notifier host — then hiding the window would
  /// leave the app running with no window, no icon, and no menu to quit from:
  /// unreachable except by killing the process.
  bool _menuDrew = false;

  static MenuItem _item(TrayLine line) =>
      MenuItem(label: line.label, disabled: !line.enabled);

  /// Ask the window manager to hand us the close event, or stop asking.
  Future<void> _applyCloseBehaviour() async {
    if (!isDesktop) return;
    // Both halves must hold: the user asked for it, AND there is a tray to be
    // hidden into. Arming this without a drawable tray produces an app with no
    // window and no icon, which can only be quit by killing the process.
    final hide = state.view.keepInTray && _menuDrew;
    try {
      await windowManager.setPreventClose(hide);
    } catch (_) {
      // If this fails the window closes normally, which is the safe direction:
      // the app quits rather than silently staying resident.
    }
  }

  @override
  void onWindowClose() {
    // Only reached while `setPreventClose(true)` is in force, i.e. while the
    // preference is on. Hide rather than exit; Quit is in the menu.
    unawaited(windowManager.hide());
  }

  @override
  void onTrayIconMouseDown() {
    // Windows and most Linux desktops expect a left click to open the app;
    // macOS expects it to open the menu, which the plugin does for us there.
    unawaited(_open());
  }

  @override
  void onTrayIconRightMouseDown() => unawaited(trayManager.popUpContextMenu());

  @override
  void onTrayMenuItemClick(MenuItem menuItem) {
    switch (menuItem.key) {
      case _kOpen:
        unawaited(_open());
      case _kSend:
        unawaited(sendFiles());
      case _kQuit:
        unawaited(quit());
    }
  }

  Future<void> _open() async => showWindow();

  Future<void> dispose() async {
    if (!_started) return;
    _pending?.cancel();
    state.transfer.removeListener(_scheduleRender);
    state.device.removeListener(_scheduleRender);
    state.view.removeListener(_applyCloseBehaviour);
    trayManager.removeListener(this);
    windowManager.removeListener(this);
    try {
      await trayManager.destroy();
    } catch (_) {
      // Shutting down; a tray that will not clean up is not worth an error.
    }
  }
}
