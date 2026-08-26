import 'dart:io';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/main.dart';
import 'sdk/fake_peerbeam.dart';

Future<void> loadRealFont() async {
  for (final w in ['400','500','600','700']) {
    final bytes = File('assets/fonts/GoogleSansFlex-$w.ttf').readAsBytesSync();
    final loader = FontLoader('Google Sans Flex')
      ..addFont(Future.value(ByteData.view(bytes.buffer)));
    await loader.load();
  }
}

Future<void> run(WidgetTester tester, Size size, double scale) async {
  final errs = <String>[];
  final prev = FlutterError.onError;
  FlutterError.onError = (d) => errs.add('${d.exceptionAsString()} @ ${d.context}\n${d.library}');
  addTearDown(() => FlutterError.onError = prev);
  tester.view.devicePixelRatio = 1.0;
  tester.view.physicalSize = size;
  tester.platformDispatcher.textScaleFactorTestValue = scale;
  addTearDown(tester.view.reset);
  addTearDown(tester.platformDispatcher.clearTextScaleFactorTestValue);
  await tester.pumpWidget(PeerBeamApp(api: FakePeerBeam()));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
  print('--- $size x$scale');
  for (final e in errs) print('   !! $e');
  errs.clear();
  const tabs = ['Home','Devices','Chats','Transfers','History','Settings','Spaces'];
  for (var i = 0; i < tabs.length; i++) {
    final icons = find.byType(NavigationDestination);
    if (icons.evaluate().isNotEmpty) {
      await tester.tap(icons.at(i), warnIfMissed: false);
    } else {
      final lbl = find.text(tabs[i]);
      if (lbl.evaluate().isNotEmpty) await tester.tap(lbl.first, warnIfMissed: false);
    }
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 500));
    for (final e in errs) print('   !! tab=${tabs[i]}: $e');
    errs.clear();
  }
}

void main() {
  setUpAll(loadRealFont);
  testWidgets('360x800 x1.0', (t) => run(t, const Size(360,800), 1.0));
  testWidgets('320x568 x1.0', (t) => run(t, const Size(320,568), 1.0));
  testWidgets('360x800 x1.3', (t) => run(t, const Size(360,800), 1.3));
  testWidgets('360x800 x1.5', (t) => run(t, const Size(360,800), 1.5));
  testWidgets('360x800 x2.0', (t) => run(t, const Size(360,800), 2.0));
  testWidgets('915x412 x1.0 landscape', (t) => run(t, const Size(915,412), 1.0));
  testWidgets('2560x1440 x1.0', (t) => run(t, const Size(2560,1440), 1.0));
}
