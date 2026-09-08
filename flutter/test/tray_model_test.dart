// What the tray / menu-bar menu says, for every state it can be in.
//
// The menu's content is a pure function precisely so it can be tested here:
// the plugin that draws it needs a desktop session, and a rule that can only be
// checked by launching an app on three operating systems is a rule that gets
// checked on none of them.

import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:peerbeam/data/view_prefs_repository.dart';
import 'package:peerbeam/platform/tray_model.dart';
import 'package:peerbeam/state/models.dart';

Device _device(String name, {bool online = true}) => Device(
  id: 'pb-$name',
  name: name,
  kind: DeviceKind.laptop,
  online: online,
  reach: const {Reach.lan},
  platform: 'linux',
);

Transfer _transfer(
  String file, {
  TransferState state = TransferState.transferring,
  int total = 100,
  int done = 0,
}) => Transfer(
  id: 'tx-$file',
  peerName: 'Bob',
  fileName: file,
  direction: TransferDirection.receiving,
  state: state,
  totalBytes: total,
  doneBytes: done,
);

void main() {
  group('which transfers appear', () {
    test('only what is actually moving', () {
      expect(trayShowsTransfer(_transfer('a')), isTrue);
      expect(
        trayShowsTransfer(_transfer('a', state: TransferState.paused)),
        isTrue,
      );
      for (final state in [
        TransferState.completed,
        TransferState.failed,
        TransferState.interrupted,
      ]) {
        expect(
          trayShowsTransfer(_transfer('a', state: state)),
          isFalse,
          reason: '$state is not "happening now"',
        );
      }
    });

    // `pending` is excluded deliberately: it can be a transfer waiting on the
    // user's consent, and a tray menu is not where that question gets asked or
    // answered. Listing it there would suggest otherwise.
    test('a transfer awaiting a decision is not listed', () {
      expect(
        trayShowsTransfer(_transfer('a', state: TransferState.pending)),
        isFalse,
      );
    });
  });

  group('the tooltip', () {
    test('says just the name when nothing is happening', () {
      final m = trayModel(devices: const [], transfers: const []);
      expect(m.tooltip, 'PeerBeam');
    });

    test('counts transfers and devices, singular and plural', () {
      expect(
        trayModel(
          devices: [_device('a')],
          transfers: [_transfer('x', done: 1)],
        ).tooltip,
        'PeerBeam — 1 transfer · 1 device online',
      );
      expect(
        trayModel(
          devices: [_device('a'), _device('b')],
          transfers: [_transfer('x'), _transfer('y')],
        ).tooltip,
        'PeerBeam — 2 transfers · 2 devices online',
      );
    });

    test('an offline device is not counted as online', () {
      final m = trayModel(
        devices: [_device('a', online: false)],
        transfers: const [],
      );
      expect(m.tooltip, 'PeerBeam');
      expect(m.devices, isEmpty);
    });
  });

  group('the lines', () {
    test('a moving transfer shows its percentage', () {
      final m = trayModel(
        devices: const [],
        transfers: [_transfer('movie.mkv', total: 100, done: 62)],
      );
      expect(m.transfers.single.label, 'movie.mkv  62%');
    });

    // "0%" on a file that is visibly moving reads as stuck. A transfer whose
    // total the engine has not reported yet has no honest percentage, so it
    // gets none.
    test('an unknown total shows no percentage rather than 0%', () {
      final m = trayModel(
        devices: const [],
        transfers: [_transfer('movie.mkv', total: 0)],
      );
      expect(m.transfers.single.label, 'movie.mkv');
    });

    test('a paused transfer says so instead of a stale percentage', () {
      final m = trayModel(
        devices: const [],
        transfers: [
          _transfer(
            'movie.mkv',
            state: TransferState.paused,
            total: 100,
            done: 30,
          ),
        ],
      );
      expect(m.transfers.single.label, 'movie.mkv  paused');
    });

    test('status lines are not clickable', () {
      final m = trayModel(
        devices: [_device('laptop')],
        transfers: [_transfer('x')],
      );
      expect(m.devices.single.enabled, isFalse);
      expect(m.transfers.single.enabled, isFalse);
    });
  });

  group('overflow', () {
    // A menu that stops at five with nothing to say reads as "these are all of
    // them" — the same silent truncation the engine's search results refuse to
    // do.
    test('a long device list says how many it did not show', () {
      final devices = List.generate(9, (i) => _device('dev$i'));
      final m = trayModel(devices: devices, transfers: const []);

      expect(m.devices, hasLength(kTrayDeviceLimit + 1));
      expect(m.devices.last.label, '…and 4 more');
      expect(m.tooltip, contains('9 devices online'));
    });

    test('exactly at the limit adds no overflow line', () {
      final devices = List.generate(kTrayDeviceLimit, (i) => _device('dev$i'));
      final m = trayModel(devices: devices, transfers: const []);

      expect(m.devices, hasLength(kTrayDeviceLimit));
      expect(m.devices.last.label, 'dev${kTrayDeviceLimit - 1}');
    });

    test('transfers overflow the same way', () {
      final transfers = List.generate(7, (i) => _transfer('f$i'));
      final m = trayModel(devices: const [], transfers: transfers);

      expect(m.transfers, hasLength(kTrayTransferLimit + 1));
      expect(m.transfers.last.label, '…and 2 more');
    });
  });

  test('an empty state has nothing to draw', () {
    final m = trayModel(devices: const [], transfers: const []);
    expect(m.transfers, isEmpty);
    expect(m.devices, isEmpty);
  });

  group('keep-running-in-the-tray is opt-in', () {
    setUp(() => SharedPreferences.setMockInitialValues({}));

    // The default is the safety property. Someone who closes a window believes
    // they closed the program; a build that kept receiving files after that
    // would have decided something about their machine for them.
    test('defaults to off, so closing the window still quits', () async {
      final prefs = ViewPrefsRepository();
      addTearDown(prefs.dispose);
      await prefs.load();

      expect(prefs.keepInTray, isFalse);
    });

    test('turning it on persists and notifies', () async {
      final prefs = ViewPrefsRepository();
      addTearDown(prefs.dispose);
      await prefs.load();

      var notified = 0;
      prefs.addListener(() => notified++);
      await prefs.setKeepInTray(true);

      expect(prefs.keepInTray, isTrue);
      expect(notified, 1);

      // A fresh repository over the same store reads it back — the setting
      // has to outlive the session that set it.
      final reloaded = ViewPrefsRepository();
      addTearDown(reloaded.dispose);
      await reloaded.load();
      expect(reloaded.keepInTray, isTrue);
    });

    test('setting the value it already has notifies nobody', () async {
      final prefs = ViewPrefsRepository();
      addTearDown(prefs.dispose);
      await prefs.load();

      var notified = 0;
      prefs.addListener(() => notified++);
      await prefs.setKeepInTray(false);

      expect(notified, 0);
    });

    // The tray rebuilds and the close behaviour re-arms off this notification,
    // so a stored value that never fires one would leave the window quitting
    // while Settings showed the switch on.
    test('a stored preference survives a reload', () async {
      SharedPreferences.setMockInitialValues({'view_keep_in_tray_v1': true});
      final prefs = ViewPrefsRepository();
      addTearDown(prefs.dispose);
      await prefs.load();

      expect(prefs.keepInTray, isTrue);
    });
  });
}
