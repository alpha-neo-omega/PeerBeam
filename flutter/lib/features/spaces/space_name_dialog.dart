import 'package:flutter/material.dart';

import '../../app/theme.dart';

/// Ask for a Space's name — a new one's, or a replacement for an existing
/// one's. Returns the trimmed name, or null when dismissed.
///
/// # Why this validates almost nothing
///
/// The engine owns the name rules: it refuses an empty name, one over 128
/// bytes, one holding a control or bidi-override character, and one another
/// Space already answers to (compared ignoring case and surrounding space).
/// Each refusal names what was wrong. Restating those rules here would put a
/// second copy of them in the app, and the copy is the one that goes stale —
/// so the caller shows the engine's own reason instead.
///
/// What is checked here is only what this dialog can see for itself: that there
/// is something to submit, and that a rename is a change. Both would come back
/// as a refusal, and a disabled button is a kinder way to say the same thing
/// than a round trip that returns "you typed nothing".
Future<String?> showSpaceNameDialog(BuildContext context, {String? current}) {
  return showDialog<String>(
    context: context,
    builder: (_) => _SpaceNameDialog(current: current),
  );
}

class _SpaceNameDialog extends StatefulWidget {
  /// The name being replaced, or null when creating.
  final String? current;
  const _SpaceNameDialog({this.current});

  @override
  State<_SpaceNameDialog> createState() => _SpaceNameDialogState();
}

class _SpaceNameDialogState extends State<_SpaceNameDialog> {
  late final TextEditingController _name = TextEditingController(
    text: widget.current ?? '',
  );

  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  bool get _renaming => widget.current != null;

  /// Whether the text as it stands is worth sending to the engine.
  bool get _submittable {
    final next = _name.text.trim();
    if (next.isEmpty) return false;
    return next != widget.current;
  }

  void _submit() {
    if (!_submittable) return;
    Navigator.of(context).pop(_name.text.trim());
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return AlertDialog(
      title: Text(_renaming ? 'Rename Space' : 'New Space'),
      content: SizedBox(
        width: 420,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            TextField(
              controller: _name,
              autofocus: true,
              textInputAction: TextInputAction.done,
              onSubmitted: (_) => _submit(),
              decoration: const InputDecoration(
                labelText: 'Name',
                hintText: 'Work laptops',
              ),
            ),
            const Gap(AppSpace.sm),
            Text(
              // Said at the moment the name is typed, because this is where
              // someone might expect naming a Space to announce it to anybody.
              // It does not: the name is a label on this machine.
              'The name stays on this device. No other device is told about it.',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        AnimatedBuilder(
          animation: _name,
          builder: (context, _) => FilledButton(
            onPressed: _submittable ? _submit : null,
            child: Text(_renaming ? 'Rename' : 'Create'),
          ),
        ),
      ],
    );
  }
}
