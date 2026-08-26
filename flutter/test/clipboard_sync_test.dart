// Clipboard sync: the desktop watcher, its echo guard, and the warning the
// Settings toggle is required to carry.
//
// Two properties carry this feature on the Flutter side.
//
// **The echo guard.** A clip a peer sends is written to this device's
// clipboard. The very next poll then sees content it did not have before — and
// if nothing accounted for it, sends it straight back. The peer's watcher does
// the same, and one copy ping-pongs between two machines forever, over the
// network, at a round trip per second each. `ClipboardSyncService._apply`
// calling `guard.adopt` before it writes is the only thing preventing that, so
// the tests below drive a real receive-then-poll sequence and assert on the
// *absence* of a push. Delete that adopt and they fail.
//
// **The honest warning.** There is no password detection in this feature and
// there is deliberately not going to be — nothing in a clipboard read says
// whether the text is a secret. The design answer is to say so on the toggle,
// which means that sentence is not decoration: it is the whole of what the user
// has to make a decision with. The test at the bottom pins the wording so a
// later tidy-up cannot quietly delete it.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/chat_repository.dart';
import 'package:peerbeam/data/notes_repository.dart';
import 'package:peerbeam/data/clipboard_sync.dart';
import 'package:peerbeam/data/discovery_repository.dart';
import 'package:peerbeam/data/history_repository.dart';
import 'package:peerbeam/data/presence_repository.dart';
import 'package:peerbeam/data/saved_devices_repository.dart';
import 'package:peerbeam/data/transfer_repository.dart';
import 'package:peerbeam/data/trust_repository.dart';
import 'package:peerbeam/features/settings/settings_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/exceptions.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/staging.dart';
import 'package:peerbeam/data/view_prefs_repository.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

const _bob = PeerTarget(
  id: 'pb-bob',
  name: 'Bob',
  addresses: ['10.0.0.2'],
  port: 4000,
);

/// A fake system clipboard: what `Clipboard.getData`/`setData` would touch.
class _FakeClipboard {
  String? text;
  int writes = 0;
  bool readThrows = false;

  /// When set, [write] applies the text but does not complete until this does
  /// — the window in which a real poll timer can fire mid-write.
  Completer<void>? blockWrite;

  Future<String?> read() async {
    if (readThrows) throw Exception('clipboard unavailable');
    return text;
  }

  Future<void> write(String t) async {
    text = t;
    writes++;
    final block = blockWrite;
    if (block != null) await block.future;
  }
}

/// A service wired to fakes. `desktop` defaults true because the send half only
/// exists on desktop; the Android case is asserted explicitly below.
({ClipboardSyncService svc, FakePeerBeam api, _FakeClipboard clip}) _service({
  bool desktop = true,
  List<PeerTarget> peers = const [_bob],
  String? initialClipboard,
}) {
  final api = FakePeerBeam();
  final clip = _FakeClipboard()..text = initialClipboard;
  final svc = ClipboardSyncService(
    api: api,
    peers: () => peers,
    nameOf: (id) => id == 'pb-bob' ? 'Bob' : id,
    readClipboard: clip.read,
    writeClipboard: clip.write,
    desktop: desktop,
    // Long enough that the real periodic timer never fires mid-test: every
    // tick below is driven explicitly through `poll()`, so the assertions are
    // about the guard, not about timing.
    interval: const Duration(hours: 1),
  );
  return (svc: svc, api: api, clip: clip);
}

/// Let the service's event subscription and its async apply run.
Future<void> _settle() async {
  for (var i = 0; i < 4; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}


/// Scroll Settings until [text] is built.
///
/// The screen is a lazy scroll view, so a tile below the fold is not in the
/// tree at all — a `find.text` for it returns nothing whether the tile is absent
/// or merely further down. Adding a card above the clipboard section is enough
/// to move it out of range, which is a property of the viewport rather than of
/// the setting, so the test scrolls to it instead of assuming where it sits.
Future<void> _scrollTo(WidgetTester tester, String text) async {
  final target = find.text(text);
  if (target.evaluate().isNotEmpty) return;
  await tester.scrollUntilVisible(
    target,
    300,
    scrollable: find.byType(Scrollable).first,
  );
  await tester.pumpAndSettle();
}

Widget _settingsApp(SettingsStore settings) => AppScope(
  state: AppState(
    theme: ThemeController(),
    device: DiscoveryRepository(),
    transfer: TransferRepository(),
    history: HistoryRepository(),
    saved: SavedDevicesRepository(),
    view: ViewPrefsRepository(),
    trust: TrustRepository(),
    chat: ChatRepository(),
    notes: NotesRepository(),
    ring: RingAlert(),
    presence: PresenceRepository(),
    settings: settings,
    staging: StagingStore(),
  ),
  child: const MaterialApp(home: SettingsScreen()),
);

void main() {
  group('ClipboardEchoGuard', () {
    test('unchanged content is not sent again', () {
      final g = ClipboardEchoGuard();
      expect(g.shouldSend('hello'), isTrue);
      g.adopt('hello');
      expect(g.shouldSend('hello'), isFalse);
      // ...and a genuinely new copy still is.
      expect(g.shouldSend('goodbye'), isTrue);
    });

    test('an empty clipboard is never sent', () {
      // An empty clip would *erase* every peer's clipboard; the engine refuses
      // one for that reason, and the watcher must not offer one either.
      final g = ClipboardEchoGuard();
      expect(g.shouldSend(''), isFalse);
    });

    test('adopting a peer clip makes it un-sendable', () {
      final g = ClipboardEchoGuard();
      g.adopt('from bob');
      expect(
        g.shouldSend('from bob'),
        isFalse,
        reason: 'this is the echo guard: what a peer sent is not ours to send',
      );
    });
  });

  group('the echo guard, end to end', () {
    test('a clip received from a peer is never sent back', () async {
      // THE ping-pong test. Bob sends a clip; it lands on our clipboard; the
      // watcher polls and finds it there. Nothing may go out.
      final f = _service();
      addTearDown(f.svc.dispose);
      f.svc.start();

      f.api.emit(
        const ClipboardReceived(
          deviceId: 'pb-bob',
          text: 'from bob',
          sentAt: 't',
        ),
      );
      await _settle();

      expect(f.clip.text, 'from bob', reason: 'it really was applied locally');
      await f.svc.poll();
      await _settle();

      expect(
        f.api.clipboardPushes,
        isEmpty,
        reason:
            'the clip was sent back to the device it came from — two machines '
            'will now ping-pong it forever',
      );
    });

    test('the guard survives repeated polls, not just the first', () async {
      // A guard that only suppressed the next tick would still loop, just
      // slower. Poll several times.
      final f = _service();
      addTearDown(f.svc.dispose);
      f.svc.start();

      f.api.emit(
        const ClipboardReceived(deviceId: 'pb-bob', text: 'x', sentAt: 't'),
      );
      await _settle();
      for (var i = 0; i < 5; i++) {
        await f.svc.poll();
        await _settle();
      }

      expect(f.api.clipboardPushes, isEmpty);
    });

    test('a poll landing mid-write does not send the clip back', () async {
      // The ordering half of the guard. `_apply` adopts *before* it writes, so
      // a tick that fires while the clipboard write is still in flight already
      // sees content that has been accounted for. Adopt-after-write leaves
      // exactly this window open, and on a 1s timer against a slow clipboard
      // (a contended X11 selection owner is not instant) it is a window that
      // really gets hit.
      final f = _service();
      addTearDown(f.svc.dispose);
      f.svc.start();
      final block = Completer<void>();
      f.clip.blockWrite = block;

      f.api.emit(
        const ClipboardReceived(
          deviceId: 'pb-bob',
          text: 'from bob',
          sentAt: 't',
        ),
      );
      await _settle();
      expect(f.clip.text, 'from bob', reason: 'the write has been applied...');

      // ...but has not returned yet, and the timer fires now.
      await f.svc.poll();
      await _settle();

      expect(
        f.api.clipboardPushes,
        isEmpty,
        reason: 'a tick during the clipboard write echoed the clip back',
      );

      block.complete();
      await _settle();
      expect(f.api.clipboardPushes, isEmpty);
    });

    test('a local copy after a received clip IS sent', () async {
      // The guard must not be a blanket mute: after Bob's clip lands, the user
      // copying something of their own must still sync. Without this the
      // ping-pong tests above would pass against a service that never sends.
      final f = _service();
      addTearDown(f.svc.dispose);
      f.svc.start();

      f.api.emit(
        const ClipboardReceived(
          deviceId: 'pb-bob',
          text: 'from bob',
          sentAt: 't',
        ),
      );
      await _settle();
      await f.svc.poll();
      expect(f.api.clipboardPushes, isEmpty);

      f.clip.text = 'my own copy'; // the user copies something
      await f.svc.poll();
      await _settle();

      expect(f.api.clipboardPushes.map((p) => p.text), ['my own copy']);
    });
  });

  group('the watcher', () {
    test('a new copy is pushed to the peers it was given', () async {
      final f = _service();
      addTearDown(f.svc.dispose);
      f.svc.start();

      f.clip.text = 'hello';
      await f.svc.poll();
      await _settle();

      expect(f.api.clipboardPushes.single.text, 'hello');
      expect(f.api.clipboardPushes.single.peers, 1);
    });

    test('unchanged clipboard content does not re-send', () async {
      final f = _service();
      addTearDown(f.svc.dispose);
      f.svc.start();

      f.clip.text = 'hello';
      await f.svc.poll();
      await _settle();
      await f.svc.poll();
      await f.svc.poll();
      await _settle();

      expect(
        f.api.clipboardPushes.length,
        1,
        reason: 'polling is not copying — only a change is a send',
      );
    });

    test(
      'whatever was already on the clipboard at start is not sent',
      () async {
        // Flipping the toggle must not sync a buffer the user had forgotten
        // about — quite possibly the password they pasted five minutes ago.
        final f = _service(initialClipboard: 'a password from earlier');
        addTearDown(f.svc.dispose);
        f.svc.start();
        await _settle();

        await f.svc.poll();
        await _settle();

        expect(f.api.clipboardPushes, isEmpty);

        // The next genuine copy still syncs.
        f.clip.text = 'something new';
        await f.svc.poll();
        await _settle();
        expect(f.api.clipboardPushes.single.text, 'something new');
      },
    );

    test('with no peers, nothing is pushed', () async {
      final f = _service(peers: const []);
      addTearDown(f.svc.dispose);
      f.svc.start();

      f.clip.text = 'hello';
      await f.svc.poll();
      await _settle();

      expect(f.api.clipboardPushes, isEmpty);
    });

    test('an unreadable clipboard is not a change', () async {
      final f = _service();
      addTearDown(f.svc.dispose);
      f.svc.start();
      f.clip.readThrows = true;

      await f.svc.poll();
      await _settle();

      expect(f.api.clipboardPushes, isEmpty);
    });

    test('an over-cap clip is reported once, not retried every tick', () async {
      final f = _service();
      addTearDown(f.svc.dispose);
      f.svc.start();
      final seen = <String>[];
      f.svc.notices.listen(seen.add);
      f.api.clipboardSyncThrows = const InvalidArgumentException(
        'clipboard too large to sync: 70000 bytes (max 65536)',
      );

      f.clip.text = 'x' * 70000;
      await f.svc.poll();
      await _settle();
      await f.svc.poll();
      await f.svc.poll();
      await _settle();

      expect(seen.length, 1, reason: 'told once, not once a second');
      expect(seen.single, contains('too large'));
      expect(
        seen.single,
        contains('65536'),
        reason: 'the limit is named, so the message is actionable',
      );
    });
  });

  group('start/stop', () {
    test('the watcher follows the setting without a restart', () async {
      final f = _service();
      addTearDown(f.svc.dispose);
      expect(f.svc.watching, isFalse, reason: 'off by default');

      f.svc.applySetting(enabled: true);
      expect(f.svc.watching, isTrue);

      f.svc.applySetting(enabled: false);
      expect(f.svc.watching, isFalse);

      f.svc.applySetting(enabled: true);
      expect(f.svc.watching, isTrue, reason: 'and back on again');
    });

    test('the watcher never runs on Android', () async {
      // Android 10+ forbids background clipboard reads, so a watcher there
      // would poll forever and find nothing it is allowed to see.
      final f = _service(desktop: false);
      addTearDown(f.svc.dispose);

      f.svc.start();
      expect(f.svc.watching, isFalse);
      f.svc.applySetting(enabled: true);
      expect(
        f.svc.watching,
        isFalse,
        reason: 'the setting cannot turn on what the platform forbids',
      );
    });

    test('a phone still receives and applies a clip', () async {
      // The other half of "desktop sends, every platform receives": receiving
      // is not gated on being desktop, and not gated on the opt-in either.
      final f = _service(desktop: false);
      addTearDown(f.svc.dispose);

      f.api.emit(
        const ClipboardReceived(
          deviceId: 'pb-bob',
          text: 'from bob',
          sentAt: 't',
        ),
      );
      await _settle();

      expect(f.clip.text, 'from bob');
      expect(f.clip.writes, 1);
    });

    test('stopping does not re-push on restart', () async {
      final f = _service();
      addTearDown(f.svc.dispose);
      f.svc.start();
      f.clip.text = 'hello';
      await f.svc.poll();
      await _settle();

      f.svc.stop();
      f.svc.start();
      await _settle();
      await f.svc.poll();
      await _settle();

      expect(
        f.api.clipboardPushes.length,
        1,
        reason: 'toggling off and on is not a copy',
      );
    });
  });

  group('a received clip is surfaced', () {
    test('the user is told which device changed their clipboard', () async {
      final f = _service();
      addTearDown(f.svc.dispose);
      final seen = <String>[];
      f.svc.notices.listen(seen.add);

      f.api.emit(
        const ClipboardReceived(
          deviceId: 'pb-bob',
          text: 'hunter2',
          sentAt: 't',
        ),
      );
      await _settle();

      expect(seen.single, 'Clipboard from Bob');
    });

    test('the toast never contains the clip itself', () async {
      // It is on their clipboard already, and a toast is a poor place for
      // something that may well be a password — it is rendered on screen, and
      // on some platforms mirrored into notification history.
      const secret = 'correct-horse-battery-staple';
      final f = _service();
      addTearDown(f.svc.dispose);
      final seen = <String>[];
      f.svc.notices.listen(seen.add);

      f.api.emit(
        const ClipboardReceived(deviceId: 'pb-bob', text: secret, sentAt: 't'),
      );
      await _settle();

      expect(seen.single, isNot(contains(secret)));
    });

    test('an empty clip is not applied', () async {
      // Applying one would erase the local clipboard on a peer's say-so. The
      // engine refuses to send one; the watcher refuses to apply one.
      final f = _service(initialClipboard: 'mine');
      addTearDown(f.svc.dispose);

      f.api.emit(
        const ClipboardReceived(deviceId: 'pb-bob', text: '', sentAt: 't'),
      );
      await _settle();

      expect(f.clip.text, 'mine');
      expect(f.clip.writes, 0);
    });
  });

  group('the Settings toggle', () {
    testWidgets('warns that everything copied is sent, passwords included', (
      tester,
    ) async {
      // **This test exists to stop the warning being softened or deleted.**
      //
      // There is no password detection in PeerBeam and there is deliberately
      // not going to be: a clipboard read carries no sensitivity signal on any
      // supported platform, so a heuristic would be wrong in both directions —
      // dropping clips the user expected, or shipping a credential while this
      // screen implies something was checked. The second is worse than saying
      // nothing at all, because the user relaxes on the strength of a promise
      // nothing is keeping.
      //
      // So this sentence is the entire safety story of the feature. If a
      // future change makes this test fail, the answer is almost certainly to
      // restore the warning, not to update the expectation.
      final settings = SettingsStore(
        deviceName: 'This Device',
        saveDirectory: '/tmp',
        autoAcceptTrusted: false,
        notifications: true,
        compression: true,
      );
      await tester.pumpWidget(_settingsApp(settings));
      await tester.pumpAndSettle();
      await _scrollTo(tester, 'Sync clipboard with trusted devices');

      expect(find.text('Sync clipboard with trusted devices'), findsOneWidget);
      // The warning, pinned verbatim. Asserting the whole sentence rather than
      // a keyword is deliberate: a softened rewrite that still happened to
      // contain "passwords" would slip past a looser check.
      expect(
        find.text(
          'Everything you copy is sent to your trusted devices, including '
          'passwords — PeerBeam cannot tell them apart. Off by default.',
        ),
        findsOneWidget,
        reason:
            'the warning must name passwords explicitly AND admit that nothing '
            'is checking — an unqualified "sends what you copy" would read as '
            'though something were',
      );
    });

    testWidgets('defaults to off and reflects the store', (tester) async {
      final settings = SettingsStore(
        deviceName: 'This Device',
        saveDirectory: '/tmp',
        autoAcceptTrusted: false,
        notifications: true,
        compression: true,
      );
      expect(settings.syncClipboard, isFalse);

      await tester.pumpWidget(_settingsApp(settings));
      await tester.pumpAndSettle();
      await _scrollTo(tester, 'Sync clipboard with trusted devices');

      final tile = tester.widget<SwitchListTile>(
        find.ancestor(
          of: find.text('Sync clipboard with trusted devices'),
          matching: find.byType(SwitchListTile),
        ),
      );
      expect(tile.value, isFalse);

      settings.setSyncClipboard(true);
      await tester.pumpAndSettle();
      expect(
        tester
            .widget<SwitchListTile>(
              find.ancestor(
                of: find.text('Sync clipboard with trusted devices'),
                matching: find.byType(SwitchListTile),
              ),
            )
            .value,
        isTrue,
      );
    });
  });
}
