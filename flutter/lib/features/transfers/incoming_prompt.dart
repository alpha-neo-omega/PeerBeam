import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../data/transfer_repository.dart';
import '../../state/app_scope.dart';
import '../../state/models.dart' show Transfer, formatBytes;
import '../../state/stores.dart' show AppState;
import '../../widgets/pairing.dart';

/// Raises the approval prompt for an incoming transfer over whatever is on
/// screen.
///
/// # Why this exists at all
///
/// The Decline / Accept / Trust actions were rendered in exactly two places —
/// the Transfers screen and a chat's file row. A transfer arriving while the
/// user was on Home, Devices, Spaces or Settings therefore waited with nothing
/// on screen to say so: the decision existed, the engine was blocked on it, and
/// the only way to find it was to already know which tab to open. The nav badge
/// counted it, but a number on an icon is not a question.
///
/// # What it must never do
///
/// **Dismissing is not an answer.** Tapping outside, pressing back or Escape
/// leaves the transfer exactly as it was — still pending, still listed on
/// Transfers, still waiting. A prompt that declined on dismissal would turn an
/// accidental tap into a refused file, and one that accepted would be a
/// security hole with a friendly face. Only the three buttons decide anything.
///
/// **It never bypasses the pairing check.** Accept and Trust both route through
/// [acceptWithPairingCheck], the same helper the Transfers screen uses, so a
/// first-contact transfer still demands the code comparison when the setting
/// requires it. This is a second surface onto one decision, never a second
/// policy for it.
///
/// # One at a time, and only once
///
/// Several transfers can be waiting at once — a folder send arrives as many.
/// Showing a dialog per transfer would stack them past the point of being
/// answerable, so this shows the oldest waiting one and lets the next appear
/// after it is answered.
///
/// [_answered] remembers every id this prompt has already put on screen, so a
/// transfer dismissed rather than answered is not re-raised on the next
/// notification — that would make the dialog impossible to get rid of without
/// answering it, which is precisely the coercion a dismissable prompt must not
/// become. It stays on the Transfers screen, which is where an unanswered
/// decision belongs.
class IncomingTransferPrompt extends StatefulWidget {
  const IncomingTransferPrompt({super.key, required this.child});

  final Widget child;

  @override
  State<IncomingTransferPrompt> createState() => _IncomingTransferPromptState();
}

class _IncomingTransferPromptState extends State<IncomingTransferPrompt> {
  /// Ids already raised — answered, dismissed or otherwise. Never re-prompted.
  final Set<String> _seen = <String>{};

  /// Whether a prompt is on screen right now. Guards against a second dialog
  /// being pushed by a notification that lands while the first is still open.
  bool _showing = false;

  /// The app state, captured while a dependency lookup is legal.
  ///
  /// `AppScope.of` registers an inherited-widget dependency, which is a build
  /// -time operation; the listener below fires from a store notification, which
  /// is not a build. Holding the state here keeps the callback out of the
  /// element tree entirely, and gives [dispose] something to detach from after
  /// the context is gone.
  AppState? _app;
  TransferRepository? _transfers;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final state = AppScope.of(context);
    _app = state;
    if (identical(state.transfer, _transfers)) return;
    _transfers?.removeListener(_onTransfers);
    _transfers = state.transfer..addListener(_onTransfers);
  }

  @override
  void dispose() {
    _transfers?.removeListener(_onTransfers);
    super.dispose();
  }

  void _onTransfers() {
    final state = _app;
    if (!mounted || _showing || state == null) return;
    if (!state.view.askOnReceive) return;

    Transfer? next;
    for (final t in state.transfer.awaitingApproval) {
      if (_seen.contains(t.id)) continue;
      next = t;
      break;
    }
    if (next == null) return;

    _seen.add(next.id);
    _showing = true;
    _prompt(next).whenComplete(() {
      if (mounted) _showing = false;
    });
  }

  Future<void> _prompt(Transfer transfer) async {
    final state = _app!;
    await showDialog<void>(
      context: context,
      // Dismissable on purpose: see the class doc. Dismissal answers nothing.
      barrierDismissible: true,
      builder: (dialogContext) => _IncomingDialog(
        transfer: transfer,
        needsConfirmation: state.transfer.needsPairingConfirmation(transfer.id),
        onDecline: () {
          Navigator.of(dialogContext).pop();
          state.transfer.reject(transfer.id);
        },
        onAccept: () async {
          Navigator.of(dialogContext).pop();
          await acceptWithPairingCheck(
            context,
            transfer,
            needsConfirmation: state.transfer.needsPairingConfirmation(
              transfer.id,
            ),
            accept: ({required confirmed}) =>
                state.transfer.accept(transfer.id, confirmed: confirmed),
          );
        },
        onTrust: () async {
          Navigator.of(dialogContext).pop();
          await acceptWithPairingCheck(
            context,
            transfer,
            needsConfirmation: state.transfer.needsPairingConfirmation(
              transfer.id,
            ),
            accept: ({required confirmed}) =>
                state.transfer.acceptTrust(transfer.id, confirmed: confirmed),
          );
        },
      ),
    );
  }

  @override
  Widget build(BuildContext context) => widget.child;
}

/// The prompt's body. Split out so a widget test can build it without an
/// engine, a store or a navigator behind it.
class _IncomingDialog extends StatelessWidget {
  const _IncomingDialog({
    required this.transfer,
    required this.needsConfirmation,
    required this.onDecline,
    required this.onAccept,
    required this.onTrust,
  });

  final Transfer transfer;
  final bool needsConfirmation;
  final VoidCallback onDecline;
  final VoidCallback onAccept;
  final VoidCallback onTrust;

  @override
  Widget build(BuildContext context) {
    final peer = transfer.peerName.isEmpty ? 'A device' : transfer.peerName;
    return AlertDialog(
      icon: const Icon(Icons.download_rounded),
      title: const Text('Incoming file'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('$peer wants to send you:'),
          const Gap(AppSpace.sm),
          Text(
            transfer.fileName,
            style: Theme.of(context).textTheme.titleSmall,
            maxLines: 3,
            overflow: TextOverflow.ellipsis,
          ),
          // Size, when the offer carried one. A zero here means "not stated",
          // not "empty file", and printing "0 B" would assert something the
          // offer never said.
          if (transfer.totalBytes > 0) ...[
            const Gap(AppSpace.xxs),
            Text(
              formatBytes(transfer.totalBytes),
              style: Theme.of(context).textTheme.bodySmall?.copyWith(
                color: Theme.of(context).colorScheme.onSurfaceVariant,
              ),
            ),
          ],
          // First contact gets the same panel the Transfers card shows, for the
          // same reason: a device this one has never spoken to must not look
          // identical to a laptop used daily.
          if (transfer.newlyTrusted) ...[
            const Gap(AppSpace.md),
            PairingCodePanel(transfer: transfer),
          ],
        ],
      ),
      actions: [
        TextButton(onPressed: onDecline, child: const Text('Decline')),
        TextButton(onPressed: onAccept, child: const Text('Accept')),
        Tooltip(
          message: 'Accept and always trust this device',
          child: FilledButton(onPressed: onTrust, child: const Text('Trust')),
        ),
      ],
    );
  }
}
