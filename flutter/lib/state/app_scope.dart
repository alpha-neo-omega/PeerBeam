import 'package:flutter/widgets.dart';

import 'stores.dart';

/// Provides the shared [AppState] to the widget tree. Screens read stores via
/// `AppScope.of(context)` and listen to just the ones they render.
class AppScope extends InheritedWidget {
  final AppState state;

  const AppScope({super.key, required this.state, required super.child});

  static AppState of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<AppScope>();
    assert(scope != null, 'AppScope not found in the widget tree');
    return scope!.state;
  }

  @override
  bool updateShouldNotify(AppScope oldWidget) => oldWidget.state != state;
}
