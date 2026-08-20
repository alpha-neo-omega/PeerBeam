import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart' show TrustedDevice;
import '../../state/app_scope.dart';

/// Choose a device to put in a Space. Returns null when dismissed.
///
/// # Why the list is the trust store, and nothing wider
///
/// A Space can only hold devices this machine already trusts — there is no
/// discovery here and nothing to find. The engine refuses a member id it holds
/// no pin for, so offering a device from discovery (which lists whoever is on
/// the network, trusted or not) would put rows in this sheet whose only
/// possible outcome is a refusal the user then has to interpret. Trust is also
/// the *right* list for a second reason: a Space's live/stale split is decided
/// by the same store on every read, so what can be added is exactly what would
/// come back live.
///
/// The refusal is still possible and deliberately not pre-empted: a
/// time-limited grant can run out between this sheet opening and the write, and
/// the trust list carries no deadline to check. The engine re-checks and names
/// the reason. This sheet offers; it does not promise.
///
/// [already] is every device the Space holds, live **and** stale. A stale one
/// is filtered out for the same reason a live one is: it is in the Space, the
/// row for it is on screen saying so, and adding it again would either do
/// nothing or be refused for not being trusted — neither of which is what
/// "Add a device" should mean.
Future<TrustedDevice?> showSpaceDevicePicker(
  BuildContext context, {
  required String spaceName,
  required Set<String> already,
}) {
  final scope = AppScope.of(context);
  return showModalBottomSheet<TrustedDevice>(
    context: context,
    showDragHandle: true,
    // Rebuilt while it is open rather than read once before it opens: pins
    // arrive asynchronously (a refresh after boot, a `trust_changed` event when
    // a transfer is accepted), and a sheet holding a snapshot would keep saying
    // "no trusted devices" with the device sitting in Settings behind it. The
    // device picker on the send path learned this the hard way.
    builder: (ctx) => AnimatedBuilder(
      animation: scope.trust,
      builder: (ctx, _) => _sheet(
        ctx,
        spaceName: spaceName,
        candidates: [
          for (final d in scope.trust.items)
            if (!already.contains(d.id)) d,
        ],
        // Told apart from "nothing is trusted" below: the two want opposite
        // advice, and giving the wrong one sends someone to pair a device they
        // already paired.
        anyTrusted: scope.trust.items.isNotEmpty,
      ),
    ),
  );
}

Widget _sheet(
  BuildContext ctx, {
  required String spaceName,
  required List<TrustedDevice> candidates,
  required bool anyTrusted,
}) {
  final theme = Theme.of(ctx);
  final scheme = theme.colorScheme;
  return SafeArea(
    child: ListView(
      shrinkWrap: true,
      padding: const EdgeInsets.only(bottom: AppSpace.md),
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(
            AppSpace.lg,
            0,
            AppSpace.lg,
            AppSpace.sm,
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                'Add a device to “$spaceName”',
                style: theme.textTheme.titleMedium,
              ),
              const Gap(AppSpace.xxs),
              Text(
                // Load-bearing on two counts. The first sentence is why this
                // list is short and cannot be widened. The second is what
                // adding does *not* do — being in a Space is not a permission,
                // and a list of devices under a name you chose is exactly the
                // thing that would otherwise read like one.
                'Only devices this one already trusts can go in a Space. '
                'Adding a device grants it nothing: every send is still checked '
                'against that device’s own permissions.',
                style: theme.textTheme.bodySmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ],
          ),
        ),
        if (candidates.isEmpty)
          Padding(
            padding: const EdgeInsets.fromLTRB(
              AppSpace.lg,
              AppSpace.sm,
              AppSpace.lg,
              AppSpace.xl,
            ),
            child: Column(
              children: [
                Icon(
                  anyTrusted
                      ? Icons.done_all_rounded
                      : Icons.no_encryption_gmailerrorred_outlined,
                  size: AppIcons.lg,
                  color: scheme.onSurfaceVariant,
                ),
                const Gap(AppSpace.sm),
                Text(
                  anyTrusted
                      ? 'Every trusted device is already in it'
                      : 'No trusted devices yet',
                  style: theme.textTheme.titleMedium,
                ),
                const Gap(AppSpace.xxs),
                Text(
                  anyTrusted
                      ? 'Pair with another device and it can go in here too.'
                      : 'Pair with a device first — from Devices, or by '
                            'accepting a transfer. A Space holds devices you '
                            'have already trusted and can find no others.',
                  textAlign: TextAlign.center,
                  style: theme.textTheme.bodyMedium?.copyWith(
                    color: scheme.onSurfaceVariant,
                  ),
                ),
              ],
            ),
          )
        else
          for (final d in candidates)
            ListTile(
              key: Key('space-candidate-${d.id}'),
              // The same distinction Settings draws, for the same reason: a
              // pinned-but-unapproved device is one that reached this machine
              // once and had its key recorded, not one the user chose. It is
              // trusted enough for the engine to accept it as a member, so it
              // is offered — but not as though it were a device you picked.
              leading: Icon(
                d.approved
                    ? Icons.verified_user_rounded
                    : Icons.help_outline_rounded,
                color: d.approved ? null : scheme.outline,
              ),
              title: Text(d.name.isEmpty ? d.id : d.name),
              // The id, always: it is what the Space stores and what the rows
              // in it are keyed by, and two devices can claim one name.
              subtitle: Text(
                d.approved ? d.id : '${d.id}\nSeen once — not approved yet.',
              ),
              isThreeLine: !d.approved,
              onTap: () => Navigator.of(ctx).pop(d),
            ),
      ],
    ),
  );
}
