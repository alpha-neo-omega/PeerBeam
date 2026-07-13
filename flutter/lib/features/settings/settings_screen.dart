import 'package:flutter/material.dart';

import '../../state/app_scope.dart';
import '../../widgets/common.dart';

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
            padding: const EdgeInsets.all(16),
            children: [
              const _GroupLabel('Appearance'),
              Card(
                child: Padding(
                  padding: const EdgeInsets.all(16),
                  child: AnimatedBuilder(
                    animation: state.theme,
                    builder: (context, _) => Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          'Theme',
                          style: Theme.of(context).textTheme.titleSmall,
                        ),
                        const SizedBox(height: 12),
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
              const SizedBox(height: 16),

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
                      ListTile(
                        leading: const Icon(Icons.folder_rounded),
                        title: const Text('Save to'),
                        subtitle: Text(state.settings.saveDirectory),
                      ),
                    ],
                  ),
                ),
              ),
              const SizedBox(height: 16),

              const _GroupLabel('Transfers'),
              AnimatedBuilder(
                animation: state.settings,
                builder: (context, _) => Card(
                  child: Column(
                    children: [
                      SwitchListTile.adaptive(
                        secondary: const Icon(Icons.verified_user_rounded),
                        title: const Text('Auto-accept trusted devices'),
                        subtitle:
                            const Text('Skip the prompt for pinned devices'),
                        value: state.settings.autoAcceptTrusted,
                        onChanged: state.settings.setAutoAccept,
                      ),
                      const Divider(height: 1),
                      SwitchListTile.adaptive(
                        secondary: const Icon(Icons.compress_rounded),
                        title: const Text('Compression'),
                        subtitle: const Text('Compress compressible files'),
                        value: state.settings.compression,
                        onChanged: state.settings.setCompression,
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
              const SizedBox(height: 16),

              const _GroupLabel('About'),
              const Card(
                child: ListTile(
                  leading: Icon(Icons.info_outline_rounded),
                  title: Text('PeerBeam'),
                  subtitle: Text('Version 0.2.0 · AGPL-3.0'),
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
