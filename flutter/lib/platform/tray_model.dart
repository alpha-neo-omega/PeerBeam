/// What the tray / menu-bar icon should say, derived from app state.
///
/// **Pure, and separate from the plugin that renders it.** Building the menu is
/// the part with rules in it — what counts as "in progress", how many devices
/// are worth listing, what the tooltip says when nothing is happening — and a
/// rule that can only be exercised by launching a desktop app is a rule nobody
/// tests. `TrayService` does the talking to `tray_manager`; everything here is
/// a function of data, and every case below has a test.
///
/// This is also what keeps the tray inside invariant I7: it renders state the
/// engine already reports and offers actions that already exist. Nothing here
/// is a capability that exists only in the GUI.
library;

import '../state/models.dart';

/// How many devices the menu lists before it stops naming them individually.
///
/// A menu is not a device list. Someone on a busy network can have dozens of
/// peers, and a status menu that becomes a scrolling column of names has
/// stopped being a status menu — the window is one click away and does that
/// job properly.
const int kTrayDeviceLimit = 5;

/// How many transfers the menu lists, for the same reason.
const int kTrayTransferLimit = 5;

/// One line in the tray menu.
class TrayLine {
  /// What the line reads.
  final String label;

  /// Whether the OS should draw it as unavailable. Status lines are not
  /// actions; making them look tappable invites a click that does nothing.
  final bool enabled;

  const TrayLine(this.label, {this.enabled = false});

  @override
  bool operator ==(Object other) =>
      other is TrayLine && other.label == label && other.enabled == enabled;

  @override
  int get hashCode => Object.hash(label, enabled);

  @override
  String toString() => 'TrayLine($label, enabled: $enabled)';
}

/// The whole menu, ready to render.
class TrayModel {
  /// The icon's hover text. One line, because that is all a tooltip gets.
  final String tooltip;

  /// Transfers currently moving, newest activity first. Empty when idle.
  final List<TrayLine> transfers;

  /// Devices reachable right now. Empty when none are.
  final List<TrayLine> devices;

  const TrayModel({
    required this.tooltip,
    required this.transfers,
    required this.devices,
  });
}

/// Whether a transfer is worth a line in the menu.
///
/// `transferring` and `paused` only. A `pending` one is deliberately excluded:
/// it may be a decision waiting on the user, and the tray is not where a
/// consent question gets answered — the prompt and the Transfers screen are.
/// Terminal states are excluded because the menu answers "what is happening
/// now", and a completed transfer answers a different question that History
/// already answers better.
bool trayShowsTransfer(Transfer t) =>
    t.state == TransferState.transferring || t.state == TransferState.paused;

/// Build the menu for [devices] and [transfers].
///
/// Both lists are read as given; ordering is the caller's, so the menu matches
/// what the window would show.
TrayModel trayModel({
  required List<Device> devices,
  required List<Transfer> transfers,
}) {
  final moving = transfers.where(trayShowsTransfer).toList(growable: false);
  final online = devices.where((d) => d.online).toList(growable: false);

  return TrayModel(
    tooltip: _tooltip(moving.length, online.length),
    transfers: _lines(
      moving.map((t) => TrayLine(_transferLabel(t))).toList(growable: false),
      kTrayTransferLimit,
      (hidden) => '…and $hidden more',
    ),
    devices: _lines(
      online.map((d) => TrayLine(d.name)).toList(growable: false),
      kTrayDeviceLimit,
      (hidden) => '…and $hidden more',
    ),
  );
}

/// `name  62%`, or just the name when there is no honest percentage.
///
/// A transfer whose total is unknown reports 0 progress, and "0%" on a file
/// that is actually moving reads as stuck. Saying nothing is the honest form.
String _transferLabel(Transfer t) {
  final name = t.fileName.isEmpty ? 'file' : t.fileName;
  if (t.state == TransferState.paused) return '$name  paused';
  if (t.totalBytes <= 0) return name;
  return '$name  ${(t.progress * 100).round()}%';
}

/// The tooltip, which has to fit one line and be true when nothing is
/// happening — the state the icon is in almost all of the time.
String _tooltip(int moving, int online) {
  final activity = switch (moving) {
    0 => 'PeerBeam',
    1 => 'PeerBeam — 1 transfer',
    _ => 'PeerBeam — $moving transfers',
  };
  return switch (online) {
    0 => activity,
    1 => '$activity · 1 device online',
    _ => '$activity · $online devices online',
  };
}

/// Cap [all] at [limit], appending a line naming what was left out.
///
/// The overflow line exists so the menu never silently truncates: a list that
/// stops at five with nothing to say reads as "these are all of them", which is
/// the same lie `SearchResults.truncated` exists to prevent in the engine.
List<TrayLine> _lines(
  List<TrayLine> all,
  int limit,
  String Function(int hidden) overflow,
) {
  if (all.length <= limit) return all;
  return [...all.take(limit), TrayLine(overflow(all.length - limit))];
}
