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

  /// Whether the window is currently hidden into the tray, so the icon's
  /// primary click knows whether to show or focus.
  bool _hidden = false;

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

  /// Coalesce bursts. A running transfer emits progress continuously and
  /// rebuilding a native menu per frame is both wasteful and visibly janky on
  /// Windows, where an open menu closes when it is replaced.
  Timer? _pending;
  void _scheduleRender() {
    _pending?.cancel();
    _pending = Timer(const Duration(milliseconds: 500), _render);
  }

  Future<void> _render() async {
    if (!_started) return;
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
    } catch (_) {
      // A tray that will not draw is not a reason to take the app down with
      // it: the window is the primary surface and still works. Some Linux
      // desktops have no status-notifier host at all.
    }
  }

  static MenuItem _item(TrayLine line) =>
      MenuItem(label: line.label, disabled: !line.enabled);

  /// Ask the window manager to hand us the close event, or stop asking.
  Future<void> _applyCloseBehaviour() async {
    if (!isDesktop) return;
    try {
      await windowManager.setPreventClose(state.view.keepInTray);
    } catch (_) {
      // If this fails the window closes normally, which is the safe direction:
      // the app quits rather than silently staying resident.
    }
  }

  @override
  void onWindowClose() {
    // Only reached while `setPreventClose(true)` is in force, i.e. while the
    // preference is on. Hide rather than exit; Quit is in the menu.
    unawaited(() async {
      await windowManager.hide();
      _hidden = true;
    }());
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

  Future<void> _open() async {
    await showWindow();
    _hidden = false;
  }

  /// Whether the window is hidden in the tray right now. For tests and for the
  /// shell, which must not report "closed" while the app is still resident.
  bool get isHidden => _hidden;

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
