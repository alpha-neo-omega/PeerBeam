// Status-colour contrast: the green and amber that mark a transfer's outcome,
// measured against the surfaces they are actually painted on.
//
// One vivid pair (green #22C55E, amber #F59E0B) used to serve both themes. On
// the dark scheme's containers it measures ~7.5-8:1; on the light scheme's it
// measures 1.66-2.07:1, which is under half of WCAG AA's 4.5:1 for text — and
// the Transfers card draws the state label and the percentage in exactly that
// colour on `surfaceContainerLow`, with the progress bar filling it against a
// `surfaceContainerHighest` track. Light mode now resolves to a darker step of
// the same ramp.
//
// Every ratio below is computed from the live `ColorScheme` and the live
// `AppColors` values rather than asserted as a remembered number, so editing
// the constants back towards the vivid pair — or a Material update moving the
// surface tones — fails here instead of shipping an unreadable label.

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/app/theme.dart';
import 'package:peerbeam/features/history/history_screen.dart';
import 'package:peerbeam/features/transfers/transfers_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// WCAG AA for text at these sizes (labelSmall, and a 16px bold percentage —
/// bold only counts as "large" from 18.66px up, so neither qualifies).
const double _aaText = 4.5;

/// WCAG 1.4.11 for non-text contrast: the progress-bar fill against its track,
/// and the avatar icons.
const double _aaNonText = 3.0;

/// WCAG 2.1 relative luminance of an opaque sRGB colour.
double _luminance(Color c) {
  double channel(double v) =>
      v <= 0.03928 ? v / 12.92 : math.pow((v + 0.055) / 1.055, 2.4).toDouble();
  return 0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b);
}

/// WCAG 2.1 contrast ratio between two opaque colours, in 1.0..21.0.
double _ratio(Color a, Color b) {
  final la = _luminance(a);
  final lb = _luminance(b);
  return (math.max(la, lb) + 0.05) / (math.min(la, lb) + 0.05);
}

/// [fg] at [alpha] composited over opaque [bg] — what a tinted `CircleAvatar`
/// background actually is once painted, and therefore what its icon has to be
/// legible against.
Color _over(Color fg, Color bg, double alpha) => Color.from(
  alpha: 1,
  red: fg.r * alpha + bg.r * (1 - alpha),
  green: fg.g * alpha + bg.g * (1 - alpha),
  blue: fg.b * alpha + bg.b * (1 - alpha),
);

/// The colour of the `Text` whose content is [label].
Color? _textColor(WidgetTester tester, String label) =>
    tester.widget<Text>(find.text(label)).style?.color;

TransferEvent _event(String kind, String id, [Map<String, Object?>? payload]) =>
    TransferEvent(
      kind: kind,
      transferId: id,
      timestamp: '',
      payload: payload ?? const {},
    );

/// Pump [child] under a light-themed app wired to [fake], and settle the
/// entrance animation. The theme is the real `PeerBeamTheme.light()`, so the
/// surfaces measured here are the ones the app ships.
Future<AppState> _pumpLight(
  WidgetTester tester,
  FakePeerBeam fake,
  Widget child,
) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: MaterialApp(theme: PeerBeamTheme.light(), home: child),
    ),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 600));
  return state;
}

void main() {
  group('AppColors on the light scheme', () {
    final scheme = PeerBeamTheme.light().colorScheme;
    final success = AppColors.success(Brightness.light);
    final warning = AppColors.warning(Brightness.light);

    // Cards are `surfaceContainerLow` (cardTheme), the progress track and chips
    // are `surfaceContainerHighest`, and `surface` is the scaffold behind both.
    // Text in these colours can land on any of the three.
    final surfaces = <String, Color>{
      'surface': scheme.surface,
      'surfaceContainerLow': scheme.surfaceContainerLow,
      'surfaceContainerHighest': scheme.surfaceContainerHighest,
    };

    for (final entry in surfaces.entries) {
      test('success text clears AA on ${entry.key}', () {
        expect(
          _ratio(success, entry.value),
          greaterThanOrEqualTo(_aaText),
          reason:
              'success ${success.toARGB32().toRadixString(16)} on '
              '${entry.key} measures ${_ratio(success, entry.value)}',
        );
      });

      test('warning text clears AA on ${entry.key}', () {
        expect(
          _ratio(warning, entry.value),
          greaterThanOrEqualTo(_aaText),
          reason:
              'warning ${warning.toARGB32().toRadixString(16)} on '
              '${entry.key} measures ${_ratio(warning, entry.value)}',
        );
      });
    }

    test('avatar icons clear the non-text threshold on their own tint', () {
      // Both screens draw the icon in the accent over a `CircleAvatar` filled
      // with the same accent at 15% — a much closer pairing than accent on a
      // bare surface, and the one a reader actually sees.
      for (final accent in [success, warning]) {
        final tinted = _over(accent, scheme.surfaceContainerLow, 0.15);
        expect(_ratio(accent, tinted), greaterThanOrEqualTo(_aaNonText));
      }
    });
  });

  group('AppColors on the dark scheme', () {
    final scheme = PeerBeamTheme.dark().colorScheme;

    test('keeps the vivid pair, which already passed there', () {
      // Pinned as literals on purpose: this is the half of the fix that must
      // NOT move. Darkening for light mode is only correct if dark mode is
      // left exactly as it was.
      expect(AppColors.success(Brightness.dark), const Color(0xFF22C55E));
      expect(AppColors.warning(Brightness.dark), const Color(0xFFF59E0B));
    });

    test('the vivid pair clears AA on dark containers', () {
      for (final accent in [
        AppColors.success(Brightness.dark),
        AppColors.warning(Brightness.dark),
      ]) {
        for (final surface in [
          scheme.surface,
          scheme.surfaceContainerLow,
          scheme.surfaceContainerHighest,
        ]) {
          expect(_ratio(accent, surface), greaterThanOrEqualTo(_aaText));
        }
      }
    });
  });

  // The token tests above prove the values are sound; these prove the widgets
  // reach for the light ones. A per-theme token nobody resolves is the same
  // bug in a new place.
  testWidgets('a paused transfer card is readable in light mode', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    await _pumpLight(tester, fake, const TransfersScreen());
    fake.emit(
      _event('transfer_queued', 't1', {
        'peer': 'Bob',
        'file': 'movie.mkv',
        'incoming': false,
      }),
    );
    fake.emit(_event('transfer_started', 't1'));
    fake.emit(_event('transfer_paused', 't1'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));

    final scheme = PeerBeamTheme.light().colorScheme;
    final accent = _textColor(tester, 'Paused');
    expect(accent, AppColors.warning(Brightness.light));

    // The label and the percentage are text on the card's own surface.
    expect(
      _ratio(accent!, scheme.surfaceContainerLow),
      greaterThanOrEqualTo(_aaText),
    );
    expect(
      _ratio(_textColor(tester, '0%')!, scheme.surfaceContainerLow),
      greaterThanOrEqualTo(_aaText),
    );

    // The bar is a graphic, so 3:1 against its own track is the bar it has to
    // clear — measured against the track the widget declares, not an assumed
    // one.
    final bar = tester.widget<LinearProgressIndicator>(
      find.byType(LinearProgressIndicator),
    );
    expect(bar.color, accent);
    expect(
      _ratio(bar.color!, bar.backgroundColor!),
      greaterThanOrEqualTo(_aaNonText),
    );
  });

  testWidgets('a successful history row is readable in light mode', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..historyEntries = [
        const HistoryEntry(
          id: 'h1',
          direction: 'sending',
          peer: 'Bob',
          file: 'movie.mkv',
          path: '/tmp/movie.mkv',
          bytes: 1024,
          success: true,
          at: '2026-01-01T00:00:00Z',
        ),
      ];
    final state = await _pumpLight(tester, fake, const HistoryScreen());
    await state.history.refresh();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));

    // The row spends the success colour on an icon over a 15% tint of itself —
    // non-text, so 3:1 — but it is the same token the Transfers label uses, so
    // assert the text threshold on the card surface too.
    final scheme = PeerBeamTheme.light().colorScheme;
    final icon = tester.widget<Icon>(find.byIcon(Icons.upload_rounded).first);
    expect(icon.color, AppColors.success(Brightness.light));
    expect(
      _ratio(icon.color!, _over(icon.color!, scheme.surfaceContainerLow, 0.15)),
      greaterThanOrEqualTo(_aaNonText),
    );
    expect(
      _ratio(icon.color!, scheme.surfaceContainerLow),
      greaterThanOrEqualTo(_aaText),
    );
  });
}
