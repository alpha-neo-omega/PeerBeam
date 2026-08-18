import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../platform/bridge.dart';
import '../../platform/desktop_files.dart';
import '../../platform/open_path.dart';
import '../../platform/saf.dart';
import '../../platform/services.dart';
import '../../sdk/error_text.dart';
import '../../sdk/exceptions.dart';
import '../../sdk/models.dart' show PeerBeamPermission, SaveRule, TrustedDevice;
import '../../state/app_scope.dart';
import '../../state/stores.dart' show SettingsStore;
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
                      const _PairingConfirmationTile(),
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

              const _GroupLabel('Auto-save rules'),
              const _SaveRulesCard(),
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
                            // A pinned-but-unapproved device is a stranger that
                            // reached this machine once and had its key
                            // recorded, not a device the user chose. Only an
                            // approved one is sent presence, clipboard contents
                            // or an accepted pipe, so the two must not look
                            // alike here — the shield is what says "you chose
                            // this".
                            leading: Icon(
                              pins[i].approved
                                  ? Icons.verified_user_rounded
                                  : Icons.help_outline_rounded,
                              color: pins[i].approved
                                  ? null
                                  : Theme.of(context).colorScheme.outline,
                            ),
                            title: Text(
                              pins[i].name.isEmpty ? pins[i].id : pins[i].name,
                            ),
                            subtitle: Column(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              children: [
                                Text(
                                  _shortFingerprint(pins[i].fingerprint),
                                  style: const TextStyle(
                                    fontFeatures: [
                                      FontFeature.tabularFigures(),
                                    ],
                                  ),
                                ),
                                if (!pins[i].approved)
                                  Text(
                                    'Seen once — not approved. Accept a '
                                    'transfer from it to approve.',
                                    style: Theme.of(context).textTheme.bodySmall
                                        ?.copyWith(
                                          color: Theme.of(
                                            context,
                                          ).colorScheme.onSurfaceVariant,
                                        ),
                                  ),
                              ],
                            ),
                            isThreeLine: !pins[i].approved,
                            trailing: IconButton(
                              tooltip: 'Revoke trust',
                              icon: const Icon(Icons.link_off_rounded),
                              onPressed: () => _confirmRevoke(context, pins[i]),
                            ),
                          ),
                          // Permissions belong to an approved device: they
                          // narrow a standing the user granted and never create
                          // one, so a pinned stranger has nothing to narrow and
                          // is offered no switches (the engine reports its
                          // effective set as empty for exactly that reason).
                          if (pins[i].approved)
                            _DevicePermissions(device: pins[i]),
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
              Card(
                child: ListTile(
                  leading: const Icon(Icons.info_outline_rounded),
                  title: const Text('PeerBeam'),
                  // Asked of the engine, never written down here. The previous
                  // hardcoded string sat under a comment asking whoever bumped
                  // the version to keep it in sync, and it was three releases
                  // stale by the time anyone noticed.
                  subtitle: Text(_aboutLine(state.api?.engineVersion)),
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

  /// The About line. The version is whatever the engine reports; with no engine
  /// to ask, it says so rather than inventing a number — an app stating a
  /// version it cannot know is worse than one admitting it does not.
  static String _aboutLine(String? engineVersion) =>
      'Version ${engineVersion ?? 'unknown'} · AGPL-3.0';

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

/// The first-contact verification opt-in.
///
/// **This copy is load-bearing and `test/pairing_test.dart` pins it.** A
/// security toggle has to state its price as plainly as its benefit, or the
/// user cannot make the trade:
///
/// * *What it costs*: one extra step, and only on a device's very first
///   connection. Not every transfer, not every device — saying "you'll be
///   asked to confirm a code" without "the first time a device connects" reads
///   like permanent friction and the setting stays off for the wrong reason.
/// * *What it buys*: detection of someone intercepting that first connection.
///   It is worth being exact that this is *detection*, not prevention: the app
///   shows a code, the user compares it, and a mismatch is the signal. Nothing
///   here blocks an attacker on its own.
///
/// The per-device permission switches, under an approved device's row.
///
/// # Why they are here and not in a dialog
///
/// The distinction this section already draws — approved versus merely pinned —
/// is *whether* the user chose a device. Permissions are *what* that choice
/// left it, which is the same question one level finer, so they belong in the
/// same place rather than behind a tap that has to be discovered. Approving
/// grants all of them, so the common device shows five switches all on and
/// costs the reader nothing; a narrowed one shows exactly which is off.
///
/// # Why each switch carries a sentence
///
/// "Clipboard" alone does not say that turning it on lets another machine
/// receive whatever was last copied. A permission switch with no stated
/// consequence is a switch people flip to see what happens, which is the one
/// thing a security control must not be.
///
/// Revoking takes effect on that device's **next** operation, not its next
/// connection — the engine's gates re-read the trust store per message, clip,
/// heartbeat and accept — so the subtitle can promise "next", and does.
class _DevicePermissions extends StatelessWidget {
  const _DevicePermissions({required this.device});

  final TrustedDevice device;

  @override
  Widget build(BuildContext context) {
    final trust = AppScope.of(context).trust;
    final theme = Theme.of(context);
    return Padding(
      // Keyed by device id so a test — and anything else reaching for one
      // device's switches — can scope to this block. Every approved device
      // renders the same five labels, so an unkeyed finder matches the wrong
      // row the moment there are two devices, which is the normal case.
      key: Key('device-permissions-${device.id}'),
      padding: const EdgeInsets.only(
        left: AppSpace.lg,
        right: AppSpace.sm,
        bottom: AppSpace.sm,
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            'What ${device.name.isEmpty ? device.id : device.name} may do',
            style: theme.textTheme.labelLarge?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
          for (final permission in PeerBeamPermission.all)
            SwitchListTile.adaptive(
              dense: true,
              contentPadding: EdgeInsets.zero,
              title: Text(PeerBeamPermission.label(permission)),
              subtitle: Text(PeerBeamPermission.description(permission)),
              value: device.may(permission),
              onChanged: (v) =>
                  trust.setPermission(device.id, permission, granted: v),
            ),
        ],
      ),
    );
  }
}

/// It must not overclaim. PeerBeam already pins keys on first contact and
/// refuses a changed one; what this adds is a chance to notice that the very
/// first key was the wrong one — the one moment TOFU cannot cover by itself.
class _PairingConfirmationTile extends StatelessWidget {
  const _PairingConfirmationTile();

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    // Its own AnimatedBuilder for the same reason `_ClipboardSyncTile` has
    // one: this widget is `const`, so it is canonicalised and would otherwise
    // never rebuild when the setting changes.
    return AnimatedBuilder(
      animation: state.settings,
      builder: (context, _) => SwitchListTile.adaptive(
        secondary: const Icon(Icons.pin_rounded),
        title: const Text('Verify new devices with a pairing code'),
        subtitle: const Text(
          'The first time a device connects, both screens show the same code '
          'and you confirm they match before accepting. Catches someone '
          'intercepting that first connection. Off by default.',
        ),
        value: state.settings.requirePairingConfirmation,
        onChanged: state.settings.setRequirePairingConfirmation,
      ),
    );
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

/// The auto-save rules editor: view, add, **reorder** and remove.
///
/// # Two things this section must keep saying
///
/// 1. **A rule chooses where a file is saved, never whether it is accepted.**
///    Approval is a separate decision made by the prompt (and the separate
///    "Auto-accept trusted devices" switch above); nothing here can accept
///    anything. The header copy says so, because a list of rules sitting under
///    "Transfers" would otherwise read like an acceptance filter.
/// 2. **The first match wins.** That is why this is a `ReorderableListView` and
///    not a plain list — the order *is* the tie-break, and a user who can drag
///    a rule upward can predict the outcome. Everything below the first
///    catch-all is unreachable, which the list marks rather than leaves to be
///    discovered.
///
/// On Android the editor is replaced by a plain statement of why there is no
/// editor. A section that silently did nothing would be worse than none at all.
class _SaveRulesCard extends StatelessWidget {
  const _SaveRulesCard();

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    return AnimatedBuilder(
      animation: state.settings,
      builder: (context, _) {
        if (!state.settings.rulesSupported) {
          return const Card(
            child: ListTile(
              leading: Icon(Icons.rule_folder_outlined),
              title: Text('Not available on this device'),
              // The honest reason, not a shrug. Android hands the app one
              // user-granted folder and no way to write anywhere else, so
              // there is nothing a rule could point at.
              subtitle: Text(
                'Android saves received files to the folder you granted above '
                'and apps cannot write to any other location, so there is '
                'nowhere for a rule to send them.',
              ),
            ),
          );
        }

        final rules = state.settings.saveRules;
        final theme = Theme.of(context);
        return Card(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(
                  AppSpace.md,
                  AppSpace.md,
                  AppSpace.md,
                  0,
                ),
                child: Text(
                  // Load-bearing copy: what a rule does, and — first — what it
                  // does not.
                  'Rules choose where a file is saved. They never decide '
                  'whether it is accepted. The first rule that matches wins, '
                  'so drag to reorder; anything matching none goes to '
                  '${state.settings.saveDirectory}.',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.onSurfaceVariant,
                  ),
                ),
              ),
              if (rules.isEmpty)
                const ListTile(
                  leading: Icon(Icons.rule_folder_outlined),
                  title: Text('No rules'),
                  subtitle: Text(
                    'Every received file goes to the folder above.',
                  ),
                )
              else
                ReorderableListView.builder(
                  shrinkWrap: true,
                  physics: const NeverScrollableScrollPhysics(),
                  buildDefaultDragHandles: false,
                  itemCount: rules.length,
                  onReorderItem: (from, to) => _reorder(context, from, to),
                  itemBuilder: (context, i) =>
                      _ruleTile(context, rules, i, theme),
                ),
              const Divider(height: 1),
              Align(
                alignment: Alignment.centerLeft,
                child: Padding(
                  padding: const EdgeInsets.all(AppSpace.sm),
                  child: TextButton.icon(
                    onPressed: () => _addRule(context),
                    icon: const Icon(Icons.add_rounded),
                    label: const Text('Add rule'),
                  ),
                ),
              ),
            ],
          ),
        );
      },
    );
  }

  Widget _ruleTile(
    BuildContext context,
    List<SaveRule> rules,
    int i,
    ThemeData theme,
  ) {
    final rule = rules[i];
    // A catch-all makes every rule after it unreachable. Saying so beside the
    // rule that causes it is far kinder than letting someone wonder why the
    // rule they just added does nothing.
    final shadowed = rules
        .take(i)
        .any((r) => r.isCatchAll && r.directory != rule.directory);
    return ListTile(
      key: ValueKey('rule-$i-${rule.directory}'),
      leading: ReorderableDragStartListener(
        index: i,
        child: const Icon(Icons.drag_handle_rounded),
      ),
      title: Text(_criteria(rule)),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(rule.directory),
          if (shadowed)
            Text(
              'Never reached — a catch-all rule above matches everything.',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.error,
              ),
            ),
        ],
      ),
      isThreeLine: shadowed,
      trailing: IconButton(
        tooltip: 'Remove rule',
        icon: const Icon(Icons.delete_outline_rounded),
        onPressed: () => _removeRule(context, i),
      ),
    );
  }

  /// The criteria in one line. A rule with none is named as what it is, since
  /// "matches everything" is the single most consequential thing about it.
  static String _criteria(SaveRule rule) {
    if (rule.isCatchAll) return 'Everything';
    final parts = <String>[];
    if (rule.deviceId != null) parts.add('From ${rule.deviceId}');
    if (rule.extension != null) parts.add('*.${rule.extension}');
    final min = rule.minBytes;
    final max = rule.maxBytes;
    if (min != null && max != null) {
      parts.add('${_size(min)}–${_size(max)}');
    } else if (min != null) {
      parts.add('≥ ${_size(min)}');
    } else if (max != null) {
      parts.add('≤ ${_size(max)}');
    }
    return parts.join(' · ');
  }

  static String _size(int bytes) {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    var v = bytes.toDouble();
    var i = 0;
    while (v >= 1000 && i < units.length - 1) {
      v /= 1000;
      i++;
    }
    return i == 0 ? '$bytes B' : '${v.toStringAsFixed(1)} ${units[i]}';
  }

  /// `onReorderItem` reports `to` already adjusted for the removal at `from`,
  /// so this is a plain remove-then-insert with no off-by-one to get wrong.
  Future<void> _reorder(BuildContext context, int from, int to) async {
    final settings = AppScope.of(context).settings;
    final next = [...settings.saveRules];
    next.insert(to, next.removeAt(from));
    await _save(context, settings, next);
  }

  Future<void> _removeRule(BuildContext context, int i) async {
    final settings = AppScope.of(context).settings;
    final next = [...settings.saveRules]..removeAt(i);
    await _save(context, settings, next);
  }

  Future<void> _addRule(BuildContext context) async {
    final settings = AppScope.of(context).settings;
    final rule = await showDialog<SaveRule>(
      context: context,
      builder: (_) => const _AddRuleDialog(),
    );
    if (rule == null || !context.mounted) return;
    await _save(context, settings, [...settings.saveRules, rule]);
  }

  /// Persist, and surface a refusal.
  ///
  /// The engine validates and can refuse — a destination that is relative, has
  /// a `..` in it, or whose parent does not exist. The list on screen is only
  /// adopted once the write succeeds, so a refused edit leaves the user looking
  /// at the rules that are actually in force rather than at ones that are not.
  static Future<void> _save(
    BuildContext context,
    SettingsStore settings,
    List<SaveRule> next,
  ) async {
    final messenger = ScaffoldMessenger.of(context);
    try {
      await settings.setSaveRules(next);
    } catch (e) {
      messenger
        ..hideCurrentSnackBar()
        ..showSnackBar(SnackBar(content: Text(_ruleError(e))));
    }
  }

  /// A rule refusal carries the engine's own reason, which names the offending
  /// rule and what is wrong with its path — far more useful than the generic
  /// "that action can't be completed". Anything else falls back to the shared
  /// friendly text.
  static String _ruleError(Object e) =>
      e is InvalidArgumentException ? e.message : friendlyError(e);
}

/// The add-rule dialog: a destination, and any combination of criteria.
///
/// The destination comes from the **native directory picker**, not a text
/// field: it must be an absolute path that already exists, and typing one is
/// how you get the error the engine then has to refuse.
class _AddRuleDialog extends StatefulWidget {
  const _AddRuleDialog();

  @override
  State<_AddRuleDialog> createState() => _AddRuleDialogState();
}

class _AddRuleDialogState extends State<_AddRuleDialog> {
  final _extension = TextEditingController();
  String? _deviceId;
  String? _directory;
  int? _minBytes;
  int? _maxBytes;
  final _min = TextEditingController();
  final _max = TextEditingController();

  @override
  void dispose() {
    _extension.dispose();
    _min.dispose();
    _max.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final devices = AppScope.of(context).trust.items;
    final dir = _directory;
    return AlertDialog(
      title: const Text('Add rule'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            ListTile(
              contentPadding: EdgeInsets.zero,
              leading: const Icon(Icons.folder_rounded),
              title: const Text('Save to'),
              subtitle: Text(dir ?? 'Choose a folder'),
              trailing: const Icon(Icons.edit_rounded),
              onTap: _pickDirectory,
            ),
            const Divider(),
            Text(
              'Leave a field empty to match everything. With all of them '
              'empty this rule matches every file.',
              style: Theme.of(context).textTheme.bodySmall,
            ),
            const Gap(AppSpace.sm),
            // The device criterion is a **picker over known devices**, never a
            // free-text name: what is stored is the authenticated device id,
            // and a name is something any peer can claim.
            DropdownButtonFormField<String?>(
              initialValue: _deviceId,
              decoration: const InputDecoration(labelText: 'From device'),
              items: [
                const DropdownMenuItem(value: null, child: Text('Any device')),
                for (final d in devices)
                  DropdownMenuItem(
                    value: d.id,
                    child: Text(d.name.isEmpty ? d.id : d.name),
                  ),
              ],
              onChanged: (v) => setState(() => _deviceId = v),
            ),
            TextField(
              controller: _extension,
              decoration: const InputDecoration(
                labelText: 'File extension',
                hintText: 'pdf',
              ),
            ),
            TextField(
              controller: _min,
              keyboardType: TextInputType.number,
              decoration: const InputDecoration(
                labelText: 'Minimum size (bytes)',
              ),
              onChanged: (v) => _minBytes = int.tryParse(v.trim()),
            ),
            TextField(
              controller: _max,
              keyboardType: TextInputType.number,
              decoration: const InputDecoration(
                labelText: 'Maximum size (bytes)',
              ),
              onChanged: (v) => _maxBytes = int.tryParse(v.trim()),
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('Cancel'),
        ),
        FilledButton(
          // Disabled until a folder is chosen: a rule with no destination has
          // nowhere to put anything, and the engine would refuse it anyway.
          onPressed: dir == null
              ? null
              : () => Navigator.pop(context, _build(dir)),
          child: const Text('Add'),
        ),
      ],
    );
  }

  SaveRule _build(String directory) {
    final ext = _extension.text.trim().replaceFirst(RegExp(r'^\.+'), '');
    return SaveRule(
      deviceId: _deviceId,
      extension: ext.isEmpty ? null : ext,
      minBytes: _minBytes,
      maxBytes: _maxBytes,
      directory: directory,
    );
  }

  Future<void> _pickDirectory() async {
    final picked = await pickSaveDirectory();
    if (picked != null && picked.isNotEmpty && mounted) {
      setState(() => _directory = picked);
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
