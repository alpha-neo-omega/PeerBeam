import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';

/// How the device lists are *displayed*, persisted locally.
///
/// **Local, not engine settings.** Nothing here changes what PeerBeam does —
/// no device is forgotten, no discovery is turned off, nothing is hidden from
/// the CLI. These are answers to "what do I want to look at", which is why they
/// live beside the theme rather than in `EngineConfig`: an engine setting has to
/// be reachable from every frontend, and a preference about this window's list
/// would be meaningless to a headless server.
class ViewPrefsRepository extends ChangeNotifier {
  static const _hideOfflineKey = 'view_hide_offline_v1';

  bool _hideOffline = false;
  bool _disposed = false;

  /// Whether Home's nearby list shows only devices that are reachable now.
  ///
  /// Off by default, because an offline row is not inert: its chat opens the
  /// conversation you already have with that peer, and a device that dropped
  /// for a moment comes back on its own. Turning this on is a choice to trade
  /// that for a shorter list.
  bool get hideOffline => _hideOffline;

  /// Load the stored preferences (call once at startup).
  Future<void> load() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      _hideOffline = prefs.getBool(_hideOfflineKey) ?? false;
    } catch (_) {
      // A platform without a preferences implementation (a unit test, a host
      // build missing the plugin) keeps the defaults rather than failing
      // startup over a display preference.
      return;
    }
    if (_disposed) return;
    notifyListeners();
  }

  /// Show only reachable devices in Home's nearby list, or all of them.
  Future<void> setHideOffline(bool value) async {
    if (value == _hideOffline) return;
    _hideOffline = value;
    notifyListeners();
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setBool(_hideOfflineKey, value);
    } catch (_) {
      // The toggle already applies to this session; failing to persist it is
      // worth less than an error dialog over a list filter.
    }
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
