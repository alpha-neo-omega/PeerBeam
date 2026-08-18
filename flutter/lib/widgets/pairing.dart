// First-contact verification: what the approval prompt shows when the sending
// device has never connected before, and how the user confirms it.
import 'package:flutter/material.dart';

import '../app/theme.dart';
import '../state/models.dart';

/// The one sentence that tells a user what to *do* with a pairing code.
///
/// **This copy is load-bearing and `test/pairing_test.dart` pins it.** Two
/// things it has to say, and a third it has to avoid:
///
/// 1. *Compare it with the other device.* A code shown and not compared is
///    decoration. Both honest peers derive the same 128-bit safety number from
///    the keys they actually negotiated; under a man-in-the-middle each side
///    derives a different one. The mismatch is the only signal there is.
/// 2. *Look at the other device itself* — its screen, not a message about it.
///    An attacker who can relay the handshake can also relay a screenshot, a
///    chat message or a read-aloud over a call they are sitting on. Saying
///    "check the other device shows this" without saying *look at it* leaves
///    the check satisfiable by the very channel it is meant to bypass.
/// 3. It must never imply PeerBeam checked anything. It cannot: this device has
///    no way to know what the other screen displays. Softening this into
///    "verified" or "secure" would be a security regression dressed as a
///    copy-edit — the user would relax on a promise nothing is keeping.
const String pairingCodeInstruction =
    'Check that the other device is showing this same code. Look at the device '
    'itself, not a message or a screenshot of it.';

/// The heading for a device that has never connected before.
const String firstContactTitle = 'This device has never connected before';

/// The first-contact panel on an approval prompt: says the device is new, and
/// shows its pairing code in full.
///
/// Rendered whenever the transfer is first contact — **not** only when the
/// confirmation check is on. Knowing a device is new, and being able to check
/// it, is worth having by default; the setting decides whether that check is
/// *required*, not whether the information exists.
class PairingCodePanel extends StatelessWidget {
  const PairingCodePanel({super.key, required this.transfer});

  final Transfer transfer;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return Container(
      padding: const EdgeInsets.all(AppSpace.sm),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(AppSpace.xs),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(
                Icons.new_releases_outlined,
                size: 18,
                color: scheme.onSurfaceVariant,
              ),
              const Gap(AppSpace.xs),
              Expanded(
                child: Text(
                  firstContactTitle,
                  style: text.labelLarge?.copyWith(color: scheme.onSurface),
                ),
              ),
            ],
          ),
          const Gap(AppSpace.xs),
          PairingCodeText(code: transfer.pairingCode),
          const Gap(AppSpace.xs),
          Text(
            pairingCodeInstruction,
            style: text.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
          ),
        ],
      ),
    );
  }
}

/// The code itself, in the engine's grouping.
///
/// Rendered whole and never abbreviated. All 128 bits are what make the code
/// expensive to forge — an attacker who only has to match a truncated prefix
/// can grind substituted keys until the two sides agree — so there is no
/// ellipsis, no `maxLines`, and no "tap to see the rest". It wraps instead.
class PairingCodeText extends StatelessWidget {
  const PairingCodeText({super.key, required this.code});

  final String code;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return SelectableText(
      code,
      style: text.titleMedium?.copyWith(
        fontFamily: 'monospace',
        fontFeatures: const [FontFeature.tabularFigures()],
        letterSpacing: 1.2,
      ),
    );
  }
}

/// Ask the user to confirm the pairing code matches, and report what they said.
///
/// Returns true **only** on the explicit confirm action. Cancel, the back
/// button, a tap outside and a route popped from under it all return false:
/// `showDialog` completes with null when it is dismissed, and null is not a
/// confirmation. Nothing is pre-selected and there is no default action, so the
/// dialog cannot be dispatched by reflex — the same bar the CLI's gate sets
/// when it treats everything that is not an explicit `y` as a refusal.
///
/// It only ever *asks*. The comparison is the user's, made against the other
/// device; this returns their answer and never forms one.
Future<bool> confirmPairingCode(BuildContext context, Transfer transfer) async {
  final confirmed = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      icon: const Icon(Icons.new_releases_outlined),
      title: const Text(firstContactTitle),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            '${transfer.peerName.isEmpty ? 'This device' : transfer.peerName} '
            'has not connected to this one before.',
          ),
          const Gap(AppSpace.md),
          PairingCodeText(code: transfer.pairingCode),
          const Gap(AppSpace.md),
          Text(pairingCodeInstruction),
          const Gap(AppSpace.sm),
          Text(
            'If the codes are different, someone may be intercepting the '
            'connection. Decline instead.',
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(false),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(true),
          child: const Text('The codes match'),
        ),
      ],
    ),
  );
  // A dismissed dialog answers null. Not confirmed.
  return confirmed ?? false;
}

/// Run an accept through the first-contact check: ask when one is required,
/// then accept **only** if the user confirmed.
///
/// Takes `accept` as a callback rather than a repository so this layer stays
/// free of `data/` — and so the flow itself (ask, then act only on a yes) is
/// testable without an engine.
///
/// When no confirmation is required this is a plain call-through: no dialog, no
/// change, exactly the accept that shipped before this check existed. When one
/// is required and the user does not give it — Cancel, or a dismissal — nothing
/// is accepted and nothing is declined. The transfer stays pending, which is
/// what makes being asked to verify a device cost the user nothing: they can
/// go and read the other screen, come back, and answer properly.
///
/// [transfer] is nullable because a surface can render an approval control for
/// a row whose live transfer the engine has no entry for — a chat file offer
/// whose first frame has not landed yet. A confirmation that is required with
/// nothing to display fails **closed**: no code, no dialog, no accept.
Future<void> acceptWithPairingCheck(
  BuildContext context,
  Transfer? transfer, {
  required bool needsConfirmation,
  required void Function({required bool confirmed}) accept,
}) async {
  if (!needsConfirmation) {
    accept(confirmed: false);
    return;
  }
  if (transfer == null) return;
  if (!await confirmPairingCode(context, transfer)) return;
  accept(confirmed: true);
}
