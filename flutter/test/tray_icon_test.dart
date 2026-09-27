// How the tray / menu-bar icon is handed to each OS, and what a left click does.
//
// These are the rules that were wrong in every shipped Windows and macOS build
// up to v0.12.0: a pure-white glyph installed as a non-template image, and a
// left click that raised the window on a platform that expects a menu. None of
// it can be seen from Linux, which is exactly why each rule is a pure function
// here rather than a comment in `tray.dart` asserting what the plugin does.

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/platform/tray.dart';

/// Every desktop the app ships a tray on.
const _desktops = [
  TargetPlatform.windows,
  TargetPlatform.macOS,
  TargetPlatform.linux,
];

void main() {
  group('which icon each platform gets', () {
    test('macOS uses the PNG, in both system themes', () {
      for (final b in Brightness.values) {
        expect(
          TrayService.iconPathFor(TargetPlatform.macOS, b),
          'assets/brand/tray/peerbeam.png',
        );
      }
    });

    test('Linux uses the PNG, in both system themes', () {
      for (final b in Brightness.values) {
        expect(
          TrayService.iconPathFor(TargetPlatform.linux, b),
          'assets/brand/tray/peerbeam.png',
        );
      }
    });

    test('Windows gets the light glyph on a dark taskbar', () {
      expect(
        TrayService.iconPathFor(TargetPlatform.windows, Brightness.dark),
        'assets/brand/tray/peerbeam.ico',
      );
    });

    // The defect: Windows shipped one all-white .ico and used it in both
    // themes, so on the default Light theme the notification area held a white
    // glyph on a white strip. With close-to-tray on, that is an app with no
    // window and no visible way back to it.
    test('Windows gets the dark glyph on a light taskbar', () {
      expect(
        TrayService.iconPathFor(TargetPlatform.windows, Brightness.light),
        'assets/brand/tray/peerbeam-dark.ico',
      );
    });

    test('Windows picks a different icon per theme', () {
      expect(
        TrayService.iconPathFor(TargetPlatform.windows, Brightness.light),
        isNot(TrayService.iconPathFor(TargetPlatform.windows, Brightness.dark)),
      );
    });
  });

  group('template rendering', () {
    // macOS recolours a template image for the current menu-bar appearance,
    // using only its alpha channel. Without this the white glyph is drawn as
    // white pixels and vanishes on a Light-appearance menu bar.
    test('only macOS asks for a template image', () {
      for (final p in _desktops) {
        expect(
          TrayService.isTemplateIconOn(p),
          p == TargetPlatform.macOS,
          reason: '$p',
        );
      }
    });
  });

  group('what a left click does', () {
    // tray_manager only attaches the menu to the status item inside
    // `popUpContextMenu`; `setContextMenu` just stores it. So on macOS nothing
    // opens the menu unless the app asks for it, and a left click that calls
    // `_open()` yanks the window forward instead — the opposite of the
    // platform convention, and of what the old comment claimed.
    test('macOS opens the menu', () {
      expect(TrayService.leftClickOpensMenuOn(TargetPlatform.macOS), isTrue);
    });

    test('Windows and Linux open the window', () {
      expect(TrayService.leftClickOpensMenuOn(TargetPlatform.windows), isFalse);
      expect(TrayService.leftClickOpensMenuOn(TargetPlatform.linux), isFalse);
    });
  });

  group('the assets actually exist', () {
    // A rule that names a file is only as good as the file. These caught
    // nothing on Linux precisely because Linux never loads the .ico.
    for (final p in _desktops) {
      for (final b in Brightness.values) {
        test('$p / ${b.name} resolves to a file on disk', () {
          final asset = TrayService.iconPathFor(p, b);
          expect(
            File(
              '${asset.startsWith('assets/') ? '' : 'assets/'}$asset',
            ).existsSync(),
            isTrue,
            reason: '$asset is missing from the repository',
          );
        });
      }
    }

    test('every tray asset is declared in pubspec.yaml', () {
      final pubspec = File('pubspec.yaml').readAsStringSync();
      for (final p in _desktops) {
        for (final b in Brightness.values) {
          final asset = TrayService.iconPathFor(p, b);
          expect(
            pubspec.contains(asset),
            isTrue,
            reason: '$asset is not listed under flutter: assets:',
          );
        }
      }
    });
  });
}
