// Temporary probe: does a BURIED route get didChangeDependencies on a theme
// change, and does ChatPresence.enter then push a duplicate?
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/state/chat_presence.dart';

class Probe extends StatefulWidget {
  const Probe({super.key, required this.presence, required this.key_});
  final ChatPresence presence;
  final String key_;
  @override
  State<Probe> createState() => _ProbeState();
}

class _ProbeState extends State<Probe> {
  int deps = 0;
  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    deps++;
    widget.presence.enter(widget.key_); // same shape as group_chat_screen.dart:72
  }

  @override
  void dispose() {
    widget.presence.leave(widget.key_);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context); // group_chat_screen.dart:159
    return Scaffold(body: Text('${widget.key_} ${theme.brightness} deps=$deps'));
  }
}

void main() {
  testWidgets('buried screen re-enters on theme change', (tester) async {
    final presence = ChatPresence();
    final nav = GlobalKey<NavigatorState>();
    var mode = ThemeMode.light;
    late StateSetter setOuter;

    await tester.pumpWidget(StatefulBuilder(builder: (c, setState) {
      setOuter = setState;
      return MaterialApp(
        navigatorKey: nav,
        theme: ThemeData.light(),
        darkTheme: ThemeData.dark(),
        themeMode: mode,
        home: Probe(presence: presence, key_: 'group:A'),
      );
    }));
    expect(presence.openConversation, 'group:A');

    // Push a second chat screen over it (the desktop notification path).
    unawaited(nav.currentState!.push(MaterialPageRoute(
        builder: (_) => Probe(presence: presence, key_: 'pb-bob'))));
    await tester.pumpAndSettle();
    expect(presence.openConversation, 'pb-bob');

    // Theme toggle while A is buried.
    setOuter(() => mode = ThemeMode.dark);
    await tester.pumpAndSettle();
    // ignore: avoid_print
    print('after theme change, front=${presence.openConversation}');

    // Pop the top screen, then the bottom one.
    nav.currentState!.pop();
    await tester.pumpAndSettle();
    // ignore: avoid_print
    print('after popping top, front=${presence.openConversation}');

    await tester.pumpWidget(const SizedBox()); // unmount everything
    await tester.pumpAndSettle();
    // ignore: avoid_print
    print('after everything closed, front=${presence.openConversation}');
    expect(presence.openConversation, isNull,
        reason: 'nothing is on screen any more');
  });
}

void unawaited(Future<void> f) {}
