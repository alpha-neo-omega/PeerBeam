import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../platform/bridge.dart';
import '../../platform/desktop_files.dart';
import '../../platform/open_path.dart';
import '../../platform/saf.dart';
import '../../platform/services.dart';
import '../../sdk/models.dart' show TrustedDevice;
import '../../state/app_scope.dart';
import '../../widgets/common.dart';

bool get _isAndroid =>
    !kIsWeb && defaultTargetPlatform == TargetPlatform.android;

/// Settings. Listens to the settings + theme stores. Uses platform-adaptive
/// controls (Switch.adaptive) for a native feel on each platform.
class SettingsScreen extends StatelessWidget {
  const SettingsScreen({super.key});

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text('Settings')),
      body: SafeArea(
        child: ContentPane(
          maxWidth: 720,
          child: ListView(
            padding: const EdgeInsets.all(AppSpace.md),
            children: [
              const _GroupLabel('Appearance'),
              Card(
                child: Padding(
                  padding: const EdgeInsets.all(AppSpace.md),
                  child: AnimatedBuilder(
                    animation: state.theme,
                    builder: (context, _) => Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          'Theme',
                          style: Theme.of(context).textTheme.titleSmall,
                        ),
                        const Gap(AppSpace.sm),
                        SegmentedButton<ThemeMode>(
                          segments: const [
                            ButtonSegment(
                              value: ThemeMode.system,
                              icon: Icon(Icons.brightness_auto_rounded),
                              label: Text('System'),
                            ),
                            ButtonSegment(
                              value: ThemeMode.light,
                              icon: Icon(Icons.light_mode_rounded),
                              label: Text('Light'),
                            ),
                            ButtonSegment(
                              value: ThemeMode.dark,
                              icon: Icon(Icons.dark_mode_rounded),
                              label: Text('Dark'),
                            ),
                          ],
                          selected: {state.theme.mode},
                          onSelectionChanged: (s) =>
                              state.theme.setMode(s.first),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
              const Gap(AppSpace.md),

              const _GroupLabel('Device'),
              AnimatedBuilder(
                animation: state.settings,
                builder: (context, _) => Card(
                  child: Column(
                    children: [
                      ListTile(
                        leading: const Icon(Icons.badge_rounded),
                        title: const Text('Device name'),
                        subtitle: Text(state.settings.deviceName),
                        trailing: const Icon(Icons.edit_rounded),
                        onTap: () => _editName(context),
                      ),
                      const Divider(height: 1),
                      // Android saves via a user-chosen SAF folder (a plain
                      // path isn't user-visible under scoped storage); desktop
                      // uses a real directory path.
                      if (_isAndroid)
                        const _AndroidSaveToTile()
                      else
                        ListTile(
                          leading: const Icon(Icons.folder_rounded),
                          title: const Text('Save to'),
                          subtitle: Text(state.settings.saveDirectory),
                          // Desktop: open the folder in the file manager; tap
                          // the row to change it.
                          trailing: IconButton(
                            tooltip: 'Open folder',
                            icon: const Icon(Icons.open_in_new_rounded),
                            onPressed: () => _openSaveDir(context),
                          ),
                          onTap: () => _pickSaveDir(context),
                        ),
                    ],
                  ),
                ),
              ),
              const Gap(AppSpace.md),

              const _GroupLabel('Privacy'),
              AnimatedBuilder(
                animation: state.settings,
                builder: (context, _) => Card(
                  child: Column(
                    children: [
                      SwitchListTile.adaptive(
                        secondary: const Icon(Icons.monitor_heart_rounded),
                        title: const Text(
                          'Share device status with trusted devices',
                        ),
                        // One line, and it must be true: what leaves, and to
                        // whom. Naming the fields is the point — a vague
                        // "share status" would not let anyone decide.
                        subtitle: const Text(
                          'Sends battery, free storage, network type and app '
                          'version to your trusted devices only. Off by '
                          'default.',
                        ),
                        value: state.settings.sharePresence,
                        onChanged: state.settings.setSharePresence,
                      ),
                      const Divider(height: 1),
                      const _ClipboardSyncTile(),
                    ],
                  ),
                ),
              ),
              const Gap(AppSpace.md),

              const _GroupLabel('Transfers'),
              AnimatedBuilder(
                animation: state.settings,
                builder: (context, _) => Card(
                  child: Column(
                    children: [
                      SwitchListTile.adaptive(
                        secondary: const Icon(Icons.verified_user_rounded),
                        title: const Text('Auto-accept trusted devices'),
                        subtitle: const Text(
                          'Skip the prompt for pinned devices',
                        ),
                        value: state.settings.autoAcceptTrusted,
                        onChanged: state.settings.setAutoAccept,
                      ),
                      const Divider(height: 1),
                      SwitchListTile.adaptive(
                        secondary: const Icon(Icons.notifications_rounded),
                        title: const Text('Notifications'),
                        value: state.settings.notifications,
                        onChanged: state.settings.setNotifications,
                      ),
                    ],
                  ),
                ),
              ),
              const Gap(AppSpace.md),

              const _GroupLabel('Trusted devices'),
              AnimatedBuilder(
                animation: state.trust,
                builder: (context, _) {
                  final pins = state.trust.items;
                  if (pins.isEmpty) {
                    return const Card(
                      child: ListTile(
                        leading: Icon(Icons.verified_user_outlined),
                        title: Text('No trusted devices yet'),
                        subtitle: Text(
                          'Devices you approve are pinned here by their key '
                          'fingerprint.',
                        ),
                      ),
                    );
                  }
                  return Card(
                    child: Column(
                      children: [
                        for (var i = 0; i < pins.length; i++) ...[
                          if (i > 0) const Divider(height: 1),
                          ListTile(
                            leading: const Icon(Icons.verified_user_rounded),
                            title: Text(
                              pins[i].name.isEmpty ? pins[i].id : pins[i].name,
                            ),
                            subtitle: Text(
                              _shortFingerprint(pins[i].fingerprint),
                              style: const TextStyle(
                                fontFeatures: [FontFeature.tabularFigures()],
                              ),
                            ),
                            trailing: IconButton(
                              tooltip: 'Revoke trust',
                              icon: const Icon(Icons.link_off_rounded),
                              onPressed: () => _confirmRevoke(context, pins[i]),
                            ),
                          ),
                        ],
                      ],
                    ),
                  );
                },
              ),
              const Gap(AppSpace.md),

              // Android-only background/battery controls.
              if (_isAndroid) ...[
                const _GroupLabel('Background (Android)'),
                AnimatedBuilder(
                  animation: state.settings,
                  builder: (context, _) => Card(
                    child: Column(
                      children: [
                        SwitchListTile.adaptive(
                          secondary: const Icon(Icons.dns_rounded),
                          title: const Text('Keep receiving in background'),
                          subtitle: const Text(
                            'Runs a foreground service so transfers survive '
                            'backgrounding',
                          ),
                          value: state.settings.backgroundReceive,
                          onChanged: state.settings.setBackgroundReceive,
                        ),
                        const Divider(height: 1),
                        ListTile(
                          leading: const Icon(Icons.battery_saver_rounded),
                          title: const Text('Ignore battery optimization'),
                          subtitle: const Text(
                            'Prevents the system from suspending transfers',
                          ),
                          trailing: const Icon(Icons.open_in_new_rounded),
                          onTap: () => BatteryOptimization(
                            AndroidBridge(),
                          ).requestExemption(),
                        ),
                      ],
                    ),
                  ),
                ),
                const Gap(AppSpace.md),
              ],

              const _GroupLabel('About'),
              const Card(
                child: ListTile(
                  leading: Icon(Icons.info_outline_rounded),
                  title: Text('PeerBeam'),
                  // Keep in sync with pubspec.yaml / workspace version.
                  subtitle: Text('Version 0.3.0 · AGPL-3.0'),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Future<void> _editName(BuildContext context) async {
    final state = AppScope.of(context);
    final controller = TextEditingController(text: state.settings.deviceName);
    try {
      final result = await showDialog<String>(
        context: context,
        builder: (ctx) => AlertDialog(
          title: const Text('Device name'),
          content: TextField(
            controller: controller,
            autofocus: true,
            decoration: const InputDecoration(hintText: 'e.g. My Laptop'),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(ctx),
              child: const Text('Cancel'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(ctx, controller.text.trim()),
              child: const Text('Save'),
            ),
          ],
        ),
      );
      if (result != null && result.isNotEmpty) {
        state.settings.setDeviceName(result);
      }
    } finally {
      controller.dispose();
    }
  }

  /// Confirm before revoking a pin — the next connection re-prompts (TOFU).
  Future<void> _confirmRevoke(BuildContext context, TrustedDevice d) async {
    final state = AppScope.of(context);
    final name = d.name.isEmpty ? d.id : d.name;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text('Revoke $name?'),
        content: const Text(
          'The device will need your approval again the next time it '
          'connects.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('Revoke'),
          ),
        ],
      ),
    );
    if (confirmed == true) await state.trust.remove(d.id);
  }

  /// First 16 hex chars of the fingerprint, grouped for readability.
  static String _shortFingerprint(String fp) {
    final head = fp.length > 16 ? fp.substring(0, 16) : fp;
    final groups = <String>[];
    for (var i = 0; i < head.length; i += 4) {
      groups.add(head.substring(i, (i + 4).clamp(0, head.length)));
    }
    return groups.join(' ');
  }

  /// Open the save directory in the system file manager (desktop).
  Future<void> _openSaveDir(BuildContext context) async {
    final dir = AppScope.of(context).settings.saveDirectory;
    final error = await openLocalPath(dir);
    if (error != null && context.mounted) {
      ScaffoldMessenger.of(context)
        ..hideCurrentSnackBar()
        ..showSnackBar(SnackBar(content: Text(error)));
    }
  }

  /// Choose the save directory with the native directory picker (desktop).
  Future<void> _pickSaveDir(BuildContext context) async {
    final settings = AppScope.of(context).settings;
    final dir = await pickSaveDirectory();
    if (dir != null && dir.isNotEmpty) {
      settings.setSaveDirectory(dir);
    }
  }
}

/// The clipboard-sync opt-in, and the two things it must admit.
///
/// **This copy is load-bearing and `test/clipboard_sync_test.dart` pins it.**
///
/// 1. *Everything copied is sent, passwords included.* There is no password
///    detection in PeerBeam and there is deliberately not going to be:
///    `Clipboard.getData` returns plain text with no sensitivity signal, and
///    X11/Wayland define none, so nothing here can distinguish a password
///    manager's paste buffer from a shopping list. A heuristic would be wrong
///    in both directions — dropping clips the user expected to arrive, or
///    shipping a credential while this screen implies something was checked.
///    The second is far worse than saying nothing, because the user relaxes on
///    the strength of a promise nothing is keeping. So the honest warning is
///    the feature, and softening it would be a security regression, not a
///    copy-edit.
/// 2. *A phone can receive but never send.* Android 10+ forbids background
///    clipboard reads. Stating it here is the difference between a documented
///    platform limit and a toggle that mysteriously does nothing.
class _ClipboardSyncTile extends StatelessWidget {
  const _ClipboardSyncTile();

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    // Its own AnimatedBuilder rather than relying on the parent's: this widget
    // is `const`, so an identical instance is canonicalised and the element is
    // never rebuilt when the enclosing builder runs. Subscribing here is what
    // makes the switch actually move when the setting changes.
    return AnimatedBuilder(
      animation: state.settings,
      builder: (context, _) => Column(
        children: [
          SwitchListTile.adaptive(
            secondary: const Icon(Icons.content_paste_rounded),
            title: const Text('Sync clipboard with trusted devices'),
            subtitle: const Text(
              'Everything you copy is sent to your trusted devices, including '
              'passwords — PeerBeam cannot tell them apart. Off by default.',
            ),
            value: state.settings.syncClipboard,
            onChanged: state.settings.setSyncClipboard,
          ),
          if (_isAndroid)
            const ListTile(
              dense: true,
              leading: Icon(Icons.info_outline_rounded, size: 20),
              title: Text(
                'This device can receive synced clipboards but cannot send '
                'them: Android does not let apps read the clipboard in the '
                'background.',
              ),
            ),
        ],
      ),
    );
  }
}

class _GroupLabel extends StatelessWidget {
  final String text;
  const _GroupLabel(this.text);

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(4, 4, 4, 8),
      child: Text(
        text.toUpperCase(),
        style: Theme.of(context).textTheme.labelMedium?.copyWith(
          color: Theme.of(context).colorScheme.primary,
          fontWeight: FontWeight.w700,
          letterSpacing: 0.6,
        ),
      ),
    );
  }
}

/// The "Save to" row on Android: shows the chosen SAF folder (received files are
/// copied there so they're visible in Files/Gallery); tap to pick a folder.
class _AndroidSaveToTile extends StatefulWidget {
  const _AndroidSaveToTile();

  @override
  State<_AndroidSaveToTile> createState() => _AndroidSaveToTileState();
}

class _AndroidSaveToTileState extends State<_AndroidSaveToTile> {
  SafFolder? _folder;
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final f = await Saf.currentFolder();
    if (!mounted) return;
    setState(() {
      _folder = f;
      _loading = false;
    });
  }

  Future<void> _pick() async {
    final f = await Saf.pickFolder();
    if (f != null && mounted) setState(() => _folder = f);
  }

  @override
  Widget build(BuildContext context) {
    final f = _folder;
    final subtitle = _loading
        ? 'Checking…'
        : f == null
        ? 'Tap to choose a folder for received files'
        : f.isDefault
        ? '${f.name} · tap to change'
        : f.name;
    return ListTile(
      leading: const Icon(Icons.folder_rounded),
      title: const Text('Save to'),
      subtitle: Text(subtitle),
      trailing: const Icon(Icons.edit_rounded),
      onTap: _pick,
    );
  }
}
