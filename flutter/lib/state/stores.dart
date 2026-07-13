import 'package:flutter/material.dart';

import 'models.dart';
import 'staging.dart';

/// Per-domain [ChangeNotifier]s. Screens listen to only the store they need
/// (via `AnimatedBuilder`), so a change in one domain never rebuilds the whole
/// app — the opposite of a single god-provider driving the entire tree.

class ThemeController extends ChangeNotifier {
  ThemeMode _mode = ThemeMode.system;
  ThemeMode get mode => _mode;
  void setMode(ThemeMode mode) {
    if (mode == _mode) return;
    _mode = mode;
    notifyListeners();
  }
}

class DeviceStore extends ChangeNotifier {
  final List<Device> _devices;
  bool _scanning = true;

  DeviceStore(this._devices);

  List<Device> get devices => List.unmodifiable(_devices);
  bool get scanning => _scanning;
  int get onlineCount => _devices.where((d) => d.online).length;

  void toggleScan() {
    _scanning = !_scanning;
    notifyListeners();
  }
}

class TransferStore extends ChangeNotifier {
  final List<Transfer> _transfers;
  TransferStore(this._transfers);

  List<Transfer> get transfers => List.unmodifiable(_transfers);
  int get activeCount => _transfers
      .where((t) =>
          t.state == TransferState.transferring ||
          t.state == TransferState.paused ||
          t.state == TransferState.pending)
      .length;

  void _replace(String id, Transfer Function(Transfer) f) {
    final i = _transfers.indexWhere((t) => t.id == id);
    if (i < 0) return;
    _transfers[i] = f(_transfers[i]);
    notifyListeners();
  }

  void pause(String id) => _replace(id, (t) => t.copyWith(state: TransferState.paused));
  void resume(String id) =>
      _replace(id, (t) => t.copyWith(state: TransferState.transferring));
  void cancel(String id) {
    _transfers.removeWhere((t) => t.id == id);
    notifyListeners();
  }
}

class HistoryStore extends ChangeNotifier {
  final List<HistoryItem> _items;
  HistoryStore(this._items);
  List<HistoryItem> get items => List.unmodifiable(_items);
  void clear() {
    _items.clear();
    notifyListeners();
  }
}

class SettingsStore extends ChangeNotifier {
  String deviceName;
  String saveDirectory;
  bool autoAcceptTrusted;
  bool notifications;
  bool compression;

  SettingsStore({
    required this.deviceName,
    required this.saveDirectory,
    required this.autoAcceptTrusted,
    required this.notifications,
    required this.compression,
  });

  void setDeviceName(String v) {
    deviceName = v;
    notifyListeners();
  }

  void setAutoAccept(bool v) {
    autoAcceptTrusted = v;
    notifyListeners();
  }

  void setNotifications(bool v) {
    notifications = v;
    notifyListeners();
  }

  void setCompression(bool v) {
    compression = v;
    notifyListeners();
  }
}

/// Top-level container of all stores, created once and shared via [AppScope].
class AppState {
  final ThemeController theme;
  final DeviceStore device;
  final TransferStore transfer;
  final HistoryStore history;
  final SettingsStore settings;
  final StagingStore staging;

  AppState({
    required this.theme,
    required this.device,
    required this.transfer,
    required this.history,
    required this.settings,
    required this.staging,
  });

  void dispose() {
    theme.dispose();
    device.dispose();
    transfer.dispose();
    history.dispose();
    settings.dispose();
    staging.dispose();
  }

  /// Sample data so the modern UI is fully explorable without the engine.
  factory AppState.sample() {
    final now = DateTime.now();
    return AppState(
      theme: ThemeController(),
      device: DeviceStore([
        const Device(
          id: 'd1',
          name: "Alice's MacBook",
          kind: DeviceKind.laptop,
          online: true,
          reach: {Reach.lan, Reach.tailscale},
          latencyMs: 4,
        ),
        const Device(
          id: 'd2',
          name: 'Pixel 9',
          kind: DeviceKind.phone,
          online: true,
          reach: {Reach.lan},
          latencyMs: 11,
        ),
        const Device(
          id: 'd3',
          name: 'home-server',
          kind: DeviceKind.server,
          online: true,
          reach: {Reach.tailscale},
          latencyMs: 38,
        ),
        const Device(
          id: 'd4',
          name: 'Studio PC',
          kind: DeviceKind.desktop,
          online: false,
          reach: {Reach.lan},
        ),
      ]),
      transfer: TransferStore([
        const Transfer(
          id: 't1',
          peerName: "Alice's MacBook",
          fileName: 'holiday-photos.zip',
          direction: TransferDirection.sending,
          state: TransferState.transferring,
          totalBytes: 512 * 1024 * 1024,
          doneBytes: 331 * 1024 * 1024,
        ),
        const Transfer(
          id: 't2',
          peerName: 'Pixel 9',
          fileName: 'presentation.pdf',
          direction: TransferDirection.receiving,
          state: TransferState.paused,
          totalBytes: 24 * 1024 * 1024,
          doneBytes: 8 * 1024 * 1024,
        ),
      ]),
      history: HistoryStore([
        HistoryItem(
          id: 'h1',
          peerName: "Alice's MacBook",
          fileName: 'design-review.sketch',
          direction: TransferDirection.receiving,
          at: now.subtract(const Duration(minutes: 12)),
          success: true,
          bytes: 84 * 1024 * 1024,
        ),
        HistoryItem(
          id: 'h2',
          peerName: 'home-server',
          fileName: 'backup-2026.tar.zst',
          direction: TransferDirection.sending,
          at: now.subtract(const Duration(hours: 3)),
          success: true,
          bytes: 3 * 1024 * 1024 * 1024,
        ),
        HistoryItem(
          id: 'h3',
          peerName: 'Pixel 9',
          fileName: 'video.mov',
          direction: TransferDirection.sending,
          at: now.subtract(const Duration(days: 1)),
          success: false,
          bytes: 1200 * 1024 * 1024,
        ),
      ]),
      settings: SettingsStore(
        deviceName: 'This Device',
        saveDirectory: '~/Downloads/PeerBeam',
        autoAcceptTrusted: false,
        notifications: true,
        compression: true,
      ),
      staging: StagingStore(),
    );
  }
}
