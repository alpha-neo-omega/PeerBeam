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
  static const _askOnReceiveKey = 'view_ask_on_receive_v1';
  static const _keepInTrayKey = 'view_keep_in_tray_v1';

  bool _hideOffline = false;
  bool _askOnReceive = true;
  bool _keepInTray = false;
  bool _disposed = false;

  /// Whether Home's nearby list shows only devices that are reachable now.
  ///
  /// Off by default, because an offline row is not inert: its chat opens the
  /// conversation you already have with that peer, and a device that dropped
  /// for a moment comes back on its own. Turning this on is a choice to trade
  /// that for a shorter list.
  bool get hideOffline => _hideOffline;

  /// Whether an incoming transfer raises a prompt over whatever is on screen.
  ///
  /// **On by default, and the default matters.** Approval used to be offered
  /// only on the Transfers screen and inside a chat, so a file arriving while
  /// the user was anywhere else waited with nothing on screen to say so — the
  /// decision existed and was unreachable without knowing where to look.
  ///
  /// Turning this off does **not** auto-accept anything. The transfer still
  /// waits, still shows on Transfers, and still has to be answered; all that
  /// changes is whether it interrupts. That distinction is why this is a view
  /// preference and not an engine setting: `auto_accept` decides *whether* a
  /// transfer needs an answer, this decides only *where the question appears*.
  bool get askOnReceive => _askOnReceive;

  /// Whether closing the window leaves PeerBeam running in the tray.
  ///
  /// **Off by default, and that default is the whole point.** With it on, the
  /// window closing does not stop the app: it stays discoverable and keeps
  /// accepting transfers, which is what a tray app is for and also a thing
  /// somebody can be wrong about. A person who closed a window generally
  /// believes they closed the program, and a build that silently kept
  /// receiving files after that would be making a decision about their machine
  /// on their behalf. So it is opt-in, the tray menu always offers Quit, and
  /// the first close after turning it on says where the app went.
  ///
  /// A view preference rather than an engine setting, for the reason the class
  /// doc gives: it is an answer to "what should this window do", and it would
  /// be meaningless to a headless server, which has `peerbeam daemon` for the
  /// same job.
  bool get keepInTray => _keepInTray;

  /// Load the stored preferences (call once at startup).
  Future<void> load() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      _hideOffline = prefs.getBool(_hideOfflineKey) ?? false;
      _askOnReceive = prefs.getBool(_askOnReceiveKey) ?? true;
      _keepInTray = prefs.getBool(_keepInTrayKey) ?? false;
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

  /// Raise a prompt when a transfer arrives, or leave it to the Transfers
  /// screen. Never changes whether the transfer needs answering.
  Future<void> setAskOnReceive(bool value) async {
    if (value == _askOnReceive) return;
    _askOnReceive = value;
    notifyListeners();
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setBool(_askOnReceiveKey, value);
    } catch (_) {
      // As above: the preference holds for this session either way.
    }
  }

  /// Keep running in the tray when the window closes, or quit as before.
  Future<void> setKeepInTray(bool value) async {
    if (value == _keepInTray) return;
    _keepInTray = value;
    notifyListeners();
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setBool(_keepInTrayKey, value);
    } catch (_) {
      // As above. Note the failure direction is safe: a preference that did
      // not persist reverts to quitting on close, never to staying resident.
    }
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
