// `pickFilesToStage`'s `AttachKind` → OS filter mapping, on both platforms it
// runs on. `chat_screen_test.dart` proves the composer's menu wires each
// choice to this function; this file proves the function itself turns that
// choice into the right native filter — the piece the brief calls load
// bearing, since a wrong mapping either hides files that should be offered or
// (on Linux) silently offers everything.

// Not a new dependency: file_selector_platform_interface is already resolved
// transitively through file_selector itself (see pubspec.lock) — this just
// reaches into it, test-only, to fake the platform the same way
// file_selector_linux/macos/windows each implement it for real.
// ignore: depend_on_referenced_packages
import 'package:file_selector_platform_interface/file_selector_platform_interface.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/platform/desktop_files.dart';

/// Captures the `acceptedTypeGroups` file_selector is asked for, with no
/// native plugin behind it — the same seam file_selector_linux/macos/windows
/// each implement for real.
class _FakeFileSelector extends FileSelectorPlatform {
  List<XTypeGroup>? lastGroups;
  bool sawCall = false;

  @override
  Future<List<XFile>> openFiles({
    List<XTypeGroup>? acceptedTypeGroups,
    String? initialDirectory,
    String? confirmButtonText,
  }) async {
    sawCall = true;
    lastGroups = acceptedTypeGroups;
    return const [];
  }
}

void main() {
  group('Android: pickFilesToStage(kind:) sets EXTRA_MIME_TYPES', () {
    // flutter_test's target platform is Android by default, so this exercises
    // the native `peerbeam/android` channel branch with no override needed.
    Future<MethodCall> pickWith(WidgetTester tester, AttachKind kind) async {
      MethodCall? call;
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        const MethodChannel('peerbeam/android'),
        (c) async {
          if (c.method == 'pickFiles') call = c;
          return <Map<String, Object?>>[];
        },
      );
      await pickFilesToStage(kind: kind);
      return call!;
    }

    testWidgets(
      'any sends no argument at all — an older native build (or a caller '
      'that only understands a bare call) still gets the wildcard picker',
      (tester) async {
        final call = await pickWith(tester, AttachKind.any);
        expect(call.arguments, isNull);
      },
    );

    testWidgets('media asks for images and video', (tester) async {
      final call = await pickWith(tester, AttachKind.media);
      expect((call.arguments as Map)['mimeTypes'], ['image/*', 'video/*']);
    });

    testWidgets('audio asks for audio only', (tester) async {
      final call = await pickWith(tester, AttachKind.audio);
      expect((call.arguments as Map)['mimeTypes'], ['audio/*']);
    });
  });

  group('Desktop: pickFilesToStage(kind:) builds XTypeGroup filters', () {
    late _FakeFileSelector fake;

    setUp(() {
      fake = _FakeFileSelector();
      FileSelectorPlatform.instance = fake;
      // file_selector only takes the file_selector_android branch when the
      // *Flutter* target platform is android; overriding it here is what
      // makes `pickFilesToStage` take the desktop branch under test, same as
      // production does on a real desktop build.
      debugDefaultTargetPlatformOverride = TargetPlatform.linux;
    });

    tearDown(() {
      debugDefaultTargetPlatformOverride = null;
    });

    test(
      'any sends no type groups at all — every file is offered, unfiltered',
      () async {
        await pickFilesToStage(kind: AttachKind.any);
        expect(fake.sawCall, isTrue);
        expect(fake.lastGroups, isEmpty);
      },
    );

    test('media\'s group carries BOTH mimeTypes and extensions — a Linux GTK '
        'picker filters by extension and ignores MIME, so a mimeTypes-only '
        'group would silently show nothing there', () async {
      await pickFilesToStage(kind: AttachKind.media);
      final group = fake.lastGroups!.single;
      expect(group.mimeTypes, ['image/*', 'video/*']);
      expect(group.extensions, contains('jpg'));
      expect(group.extensions, contains('mp4'));
    });

    test('audio\'s group carries both mimeTypes and extensions', () async {
      await pickFilesToStage(kind: AttachKind.audio);
      final group = fake.lastGroups!.single;
      expect(group.mimeTypes, ['audio/*']);
      expect(group.extensions, contains('mp3'));
      // Never the video/image list under the audio choice.
      expect(group.extensions, isNot(contains('mp4')));
    });
  });
}
