import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../data/transfer_repository.dart';
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';
import '../../widgets/pairing.dart';

/// Active transfers with animated progress and per-transfer controls. Listens
/// to the transfer store only.
///
/// Owns the bulk-approval selection (`_selected`/`_selecting`) itself rather
/// than leaving it inside the banner: a checkbox lives on every card, so both
/// the banner (its count, its Decline/Accept) and the list (each card's
/// checkbox) have to see the same selection. A progress tick rebuilds this
/// widget many times a second, so the selection needs state a rebuild does
/// not reset.
class TransfersScreen extends StatefulWidget {
  const TransfersScreen({super.key});

  @override
  State<TransfersScreen> createState() => _TransfersScreenState();
}

class _TransfersScreenState extends State<TransfersScreen> {
  /// Ids the user has checked. Can outlive their transfer — a card can settle
  /// while still checked — so this is never trusted directly; `build` always
  /// narrows it to what is still waiting first.
  Set<String> _selected = {};

  /// Whether `Select` has been tapped. Kept apart from "is anything checked"
  /// because entering selection mode must start from nothing: a pre-filled
  /// selection paired with an Accept button would be a batch decision the
  /// user never actually composed.
  bool _selecting = false;

  void _enterSelecting() => setState(() {
    _selecting = true;
    _selected = {};
  });

  void _exitSelecting() => setState(() {
    _selecting = false;
    _selected = {};
  });

  void _toggle(String id) => setState(() {
    if (!_selected.remove(id)) _selected.add(id);
  });

  /// Leave selection mode for real once the banner has nothing left to show.
  ///
  /// `build` only ever *masks* the flag (`_selecting && showBanner`), which is
  /// not the same as clearing it. Walk away with two of three checked, let them
  /// all time out, and the banner disappears with `_selecting` still true and
  /// `_selected` still populated; two new inbound transfers then bring the
  /// banner back **already in selection mode**, with checkboxes on every card
  /// and the per-card Decline/Accept/Trust gone until the user finds Cancel.
  /// This screen lives in an `indexedStack` branch, so that state survives tab
  /// switches indefinitely. Worse, inbound transfer ids come from the *sender*,
  /// so a peer reusing an id the stale set still holds renders that card
  /// pre-checked — exactly the pre-composed batch decision `_enterSelecting`'s
  /// reset exists to prevent.
  ///
  /// Deferred to a post-frame callback because the caller is `build`, and
  /// mutating state from inside a build is the property this screen already
  /// keeps (see the derivation comment there). The condition is re-read when
  /// the callback runs rather than trusted from when it was scheduled: a batch
  /// can arrive in the gap, and clearing then would drop a selection the user
  /// had just started composing.
  void _leaveSelectingAfterFrame(TransferRepository transfers) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || !_selecting) return;
      if (transfers.awaitingApproval.map((t) => t.id).toSet().length >= 2) {
        return;
      }
      setState(() {
        _selecting = false;
        _selected = {};
      });
    });
  }

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text('Transfers')),
      body: SafeArea(
        child: ContentPane(
          child: AnimatedBuilder(
            animation: state.transfer,
            builder: (context, _) {
              final items = state.transfer.transfers;
              if (items.isEmpty) {
                return const EmptyState(
                  icon: Icons.swap_horiz_rounded,
                  title: 'No active transfers',
                  message: 'Files you send or receive will show up here.',
                );
              }
              // Only worth a banner from two upwards: with a single waiting
              // transfer its own card's Accept is already the shortest path,
              // and a banner over one card is just a second button saying the
              // same thing.
              final waitingIds = state.transfer.awaitingApproval
                  .map((t) => t.id)
                  .toSet();
              final waiting = waitingIds.length;
              final showBanner = waiting >= 2;
              // Engine events keep arriving while a selection sits open, so
              // neither flag below is trusted as last set. `_selecting` only
              // means something while there is a batch left to run it
              // against — otherwise the banner would have to claim "0 of 0
              // selected" instead of simply not being in selection mode — and
              // `_selected` is narrowed to ids still genuinely waiting so a
              // settled transfer can't keep inflating the count. Both are
              // derived here, every build, rather than written back into
              // state: a dead id is left to age out on the next tick, never
              // pruned by mutating state from inside build.
              final selecting = _selecting && showBanner;
              final effectiveSelected = _selected.intersection(waitingIds);
              // Masking is not leaving. Once the banner is gone there is no
              // batch to come back to, so the mode is actually ended — after
              // this frame, never during it.
              if (_selecting && !showBanner) {
                _leaveSelectingAfterFrame(state.transfer);
              }
              return Column(
                children: [
                  if (showBanner)
                    Padding(
                      padding: const EdgeInsets.fromLTRB(
                        AppSpace.md,
                        AppSpace.md,
                        AppSpace.md,
                        0,
                      ),
                      child: _BulkApprovalBanner(
                        waiting: waiting,
                        selecting: selecting,
                        selectedIds: effectiveSelected,
                        onSelect: _enterSelecting,
                        onCancel: _exitSelecting,
                        onSettled: _exitSelecting,
                      ),
                    ),
                  Expanded(
                    child: ListView.builder(
                      padding: const EdgeInsets.all(AppSpace.md),
                      itemCount: items.length,
                      itemBuilder: (context, i) {
                        final t = items[i];
                        return Appear(
                          key: ValueKey(t.id),
                          index: i,
                          child: Padding(
                            padding: const EdgeInsets.only(bottom: AppSpace.sm),
                            child: _TransferCard(
                              transfer: t,
                              selecting: selecting,
                              selected: effectiveSelected.contains(t.id),
                              onToggle: () => _toggle(t.id),
                            ),
                          ),
                        );
                      },
                    ),
                  ),
                ],
              );
            },
          ),
        ),
      ),
    );
  }
}

/// `1 item` / `3 items` — deliberately not "files". A waiting transfer can be a
/// whole folder, so the banner counts whatever is queued for a decision rather
/// than claiming to know what each one holds.
///
/// Shared by the banner's heading and its result report so the two can never
/// describe the same batch differently.
String _items(int n) => '$n ${n == 1 ? 'item' : 'items'}';

/// A verified report of what a bulk approval actually did.
///
/// Every number here comes from a decision the engine answered — never from
/// the count that happened to be on screen when the button was tapped. Between
/// the render and the tap a transfer can time out, its sender can give up, or
/// the user can answer it from its own card, and saying "Accepted 5" when two
/// of them were already gone would be a claim, not a fact.
String _bulkReport(String verbPast, BulkDecision d) {
  if (d.requested == 0) return 'Nothing was waiting';
  if (d.settled == d.requested) return '$verbPast ${_items(d.settled)}';
  if (d.settled == 0 && d.failed == 0) return 'None were still waiting';
  final why = <String>[
    if (d.gone > 0)
      '${d.gone} ${d.gone == 1 ? 'was' : 'were'} no longer waiting',
    if (d.failed > 0) "${d.failed} couldn't be ${verbPast.toLowerCase()}",
  ];
  return '$verbPast ${d.settled} of ${d.requested} — ${why.join(', ')}';
}

/// Shown above the list when **two or more** inbound transfers are waiting on
/// the user: one decision for the batch instead of a tap per card. With one
/// waiting transfer its own card is already the shortest path, so the banner
/// stays hidden rather than duplicating it.
///
/// Two views of the same batch. Not selecting, it offers the whole batch at
/// once (`Decline all` / `Accept all`) plus `Select`, which hands the user a
/// checkbox per card instead. Selecting, it answers only the checked ids
/// (`Decline` / `Accept`) plus `Cancel`. Either way there is exactly one
/// **Trust** action, and it lives on the card: trusting a device is a lasting
/// grant of auto-accept for everything it sends from then on — a materially
/// stronger act than approving what is on screen right now — so it stays a
/// deliberate per-device choice. There is no "Trust all" and no "Trust
/// selected", and nothing here is remembered for next time (I6).
class _BulkApprovalBanner extends StatefulWidget {
  /// How many inbound transfers are awaiting approval right now.
  final int waiting;

  /// Whether the user has tapped `Select`. Already reconciled against
  /// liveness by the caller (`TransfersScreen`) — this widget never
  /// second-guesses [waiting]/[selectedIds], only renders them.
  final bool selecting;

  /// The checked ids, already narrowed to ones still awaiting approval.
  final Set<String> selectedIds;

  final VoidCallback onSelect;
  final VoidCallback onCancel;

  /// Called once a selection-mode batch has landed, so the screen can leave
  /// selection mode. Not-selecting's `Decline all`/`Accept all` never call
  /// this — there is no selection mode to leave.
  final VoidCallback onSettled;

  const _BulkApprovalBanner({
    required this.waiting,
    required this.selecting,
    required this.selectedIds,
    required this.onSelect,
    required this.onCancel,
    required this.onSettled,
  });

  @override
  State<_BulkApprovalBanner> createState() => _BulkApprovalBannerState();
}

class _BulkApprovalBannerState extends State<_BulkApprovalBanner> {
  /// A batch decision is in flight. Every action here — including
  /// `Select`/`Cancel` — is disabled until it lands, so the user cannot swap
  /// views mid-flight and have the result land back in a mode they already
  /// left.
  bool _busy = false;

  Future<void> _decideBatch({required bool accepting}) async {
    if (_busy) return;
    final messenger = ScaffoldMessenger.of(context);
    final transfers = AppScope.of(context).transfer;
    // Captured once: `widget.selecting` could flip under an in-flight
    // request (a rebuild can land while this awaits), and which call was
    // actually made must agree with whether selection mode is left
    // afterward — not with whatever happens to be true when the future
    // resolves.
    final selecting = widget.selecting;
    final ids = widget.selectedIds.toList(growable: false);
    setState(() => _busy = true);
    try {
      // `accept`, never anything that trusts, either way.
      final result = selecting
          ? (accepting
                ? await transfers.acceptOnly(ids)
                : await transfers.declineOnly(ids))
          : (accepting
                ? await transfers.acceptAll()
                : await transfers.declineAll());
      messenger
        ..hideCurrentSnackBar()
        ..showSnackBar(
          SnackBar(
            content: Text(
              _bulkReport(accepting ? 'Accepted' : 'Declined', result),
            ),
          ),
        );
      if (selecting) widget.onSettled();
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final selecting = widget.selecting;
    final nothingSelected = widget.selectedIds.isEmpty;
    return Card(
      color: scheme.secondaryContainer,
      child: Padding(
        padding: const EdgeInsets.all(AppSpace.md),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(
                  Icons.download_rounded,
                  size: AppIcons.md,
                  color: scheme.onSecondaryContainer,
                ),
                const Gap(AppSpace.sm),
                Expanded(
                  child: Text(
                    selecting
                        ? '${widget.selectedIds.length} of ${widget.waiting} selected'
                        : '${_items(widget.waiting)} waiting for approval',
                    style: text.titleSmall?.copyWith(
                      fontWeight: FontWeight.w600,
                      color: scheme.onSecondaryContainer,
                    ),
                  ),
                ),
              ],
            ),
            const Gap(AppSpace.xs),
            Align(
              alignment: Alignment.centerRight,
              child: Wrap(
                alignment: WrapAlignment.end,
                spacing: AppSpace.xs,
                runSpacing: AppSpace.xs,
                children: selecting
                    ? [
                        TextButton(
                          onPressed: _busy ? null : widget.onCancel,
                          child: const Text('Cancel'),
                        ),
                        TextButton(
                          onPressed: (_busy || nothingSelected)
                              ? null
                              : () => _decideBatch(accepting: false),
                          child: const Text('Decline'),
                        ),
                        FilledButton(
                          onPressed: (_busy || nothingSelected)
                              ? null
                              : () => _decideBatch(accepting: true),
                          child: const Text('Accept'),
                        ),
                      ]
                    : [
                        TextButton(
                          onPressed: _busy ? null : widget.onSelect,
                          child: const Text('Select'),
                        ),
                        TextButton(
                          onPressed: _busy
                              ? null
                              : () => _decideBatch(accepting: false),
                          child: const Text('Decline all'),
                        ),
                        FilledButton(
                          onPressed: _busy
                              ? null
                              : () => _decideBatch(accepting: true),
                          child: const Text('Accept all'),
                        ),
                      ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// State → accent colour for the progress bar and status. Kept here (UI-only)
/// so the shared model stays presentation-free.
///
/// [scheme] is read for its brightness as well as its roles: the success and
/// warning greens/ambers are only legible on a light card once darkened, so
/// asking `AppColors` for a fixed value here would put a 2:1 label back on the
/// screen in light mode.
Color _stateColor(TransferState s, ColorScheme scheme) => switch (s) {
  TransferState.completed => AppColors.success(scheme.brightness),
  TransferState.failed => scheme.error,
  // Interrupted is a warning, not an error: nothing went wrong that the user
  // has to fix, and the bytes already moved are still there. Sharing the
  // paused colour is the honest reading — this is a transfer that stopped and
  // can go again.
  TransferState.paused ||
  TransferState.interrupted => AppColors.warning(scheme.brightness),
  _ => scheme.primary,
};

/// Progress meta line: `done / total · speed · ETA`. Speed/ETA only while
/// actively transferring (and only when the engine reports them).
String _meta(Transfer t) {
  final parts = <String>[
    '${formatBytes(t.doneBytes)} / ${formatBytes(t.totalBytes)}',
  ];
  if (t.state == TransferState.transferring) {
    final speed = formatSpeed(t.speedBps);
    final eta = formatEta(t.etaSecs);
    if (speed.isNotEmpty) parts.add(speed);
    if (eta.isNotEmpty) parts.add(eta);
  }
  return parts.join(' · ');
}

class _TransferCard extends StatelessWidget {
  final Transfer transfer;

  /// Whether the screen is in selection mode. Changes only what an
  /// awaiting-approval card renders — everything else (pause/cancel, the
  /// progress bar) is identical either way, since a transfer whose decision
  /// is already made has nothing left to select.
  final bool selecting;

  /// Whether this transfer's id is in the current (liveness-pruned)
  /// selection. Meaningless unless [selecting] and this card is awaiting
  /// approval.
  final bool selected;

  /// Toggles this transfer's membership in the selection.
  final VoidCallback onToggle;

  const _TransferCard({
    required this.transfer,
    required this.selecting,
    required this.selected,
    required this.onToggle,
  });

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final sending = transfer.direction == TransferDirection.sending;
    final paused = transfer.state == TransferState.paused;
    // Not running at all — its checkpoint outlived it. The pause/cancel
    // controls are meaningless here (there is nothing to pause and nothing to
    // cancel), so this card gets its own pair.
    final interrupted = transfer.state == TransferState.interrupted;
    final pct = (transfer.progress * 100).round();
    final accent = _stateColor(transfer.state, scheme);
    // An inbound transfer awaiting the user's approval — needs Accept/Decline,
    // not the pause/cancel controls.
    final awaitingApproval =
        !sending && transfer.state == TransferState.pending;
    // Only an awaiting-approval card has a decision to add to a selection; an
    // outbound send or one already running, paused, completed or failed has
    // nothing left to check.
    final selectable = selecting && awaitingApproval;

    return Semantics(
      container: true,
      label:
          '${sending ? 'Sending' : 'Receiving'} ${transfer.fileName} '
          '${sending ? 'to' : 'from'} ${transfer.peerName}, '
          '$pct percent, ${transfer.state.label}',
      child: Card(
        child: InkWell(
          // The whole card is the hit target while selecting, not just the
          // checkbox — a row of small checkboxes is a fiddly target to chase
          // on a touch screen.
          onTap: selectable ? onToggle : null,
          child: Padding(
            padding: const EdgeInsets.all(AppSpace.md),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    if (selectable) ...[
                      Checkbox(value: selected, onChanged: (_) => onToggle()),
                      const Gap(AppSpace.sm),
                    ],
                    CircleAvatar(
                      radius: 22,
                      backgroundColor: accent.withValues(alpha: 0.15),
                      child: Icon(
                        sending ? Icons.upload_rounded : Icons.download_rounded,
                        size: AppIcons.md,
                        color: accent,
                      ),
                    ),
                    const Gap(AppSpace.sm),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            transfer.fileName,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: text.titleSmall?.copyWith(
                              fontWeight: FontWeight.w600,
                            ),
                          ),
                          const Gap(AppSpace.xxs),
                          Row(
                            children: [
                              Text(
                                '${sending ? 'To' : 'From'} ${transfer.peerName}',
                                style: text.bodySmall?.copyWith(
                                  color: scheme.onSurfaceVariant,
                                ),
                              ),
                              const Gap(AppSpace.xs),
                              Text(
                                transfer.state.label,
                                style: text.labelSmall?.copyWith(
                                  color: accent,
                                  fontWeight: FontWeight.w600,
                                ),
                              ),
                            ],
                          ),
                        ],
                      ),
                    ),
                    const Gap(AppSpace.xs),
                    Text(
                      '$pct%',
                      style: text.titleMedium?.copyWith(
                        fontWeight: FontWeight.w700,
                        color: accent,
                      ),
                    ),
                  ],
                ),
                const Gap(AppSpace.sm),
                TweenAnimationBuilder<double>(
                  tween: Tween(begin: 0, end: transfer.progress),
                  duration: AppMotion.duration(context, AppMotion.slow),
                  curve: AppMotion.curve,
                  builder: (context, value, _) => ClipRRect(
                    borderRadius: BorderRadius.circular(AppRadius.sm),
                    child: LinearProgressIndicator(
                      value: value,
                      minHeight: 8,
                      color: accent,
                      backgroundColor: scheme.surfaceContainerHighest,
                    ),
                  ),
                ),
                const Gap(AppSpace.xs),
                // First contact: say so, and show the pairing code. Above the
                // actions, because it is what the Accept beneath it is a
                // decision about — a device this one has never spoken to
                // before must not look identical to a laptop used daily.
                if (awaitingApproval && transfer.newlyTrusted) ...[
                  PairingCodePanel(transfer: transfer),
                  const Gap(AppSpace.xs),
                ],
                // A `Wrap` (not a `Row`) so the action cluster can drop to its
                // own line on narrow widths instead of overflowing — the
                // awaitingApproval case has three actions (Decline/Accept/
                // Trust) where the old two-button row used to just fit.
                Wrap(
                  alignment: WrapAlignment.spaceBetween,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  spacing: AppSpace.xs,
                  runSpacing: AppSpace.xs,
                  children: [
                    Text(
                      _meta(transfer),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: text.bodySmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                    Wrap(
                      alignment: WrapAlignment.end,
                      spacing: AppSpace.xs,
                      runSpacing: AppSpace.xs,
                      children: interrupted
                          // An interrupted transfer: pick it up, or let it go.
                          // Resume only when the engine says this side can —
                          // an inbound transfer resumes when its *sender*
                          // offers it again, and a button here would do
                          // nothing but lie about that.
                          ? [
                              if (transfer.resumable)
                                FilledButton.tonalIcon(
                                  onPressed: () => state.transfer
                                      .resumeInterrupted(transfer.id),
                                  icon: const Icon(
                                    Icons.play_arrow_rounded,
                                    size: 18,
                                  ),
                                  label: const Text('Resume'),
                                )
                              else
                                Tooltip(
                                  message:
                                      'This will continue on its own when '
                                      '${transfer.peerName} sends it again',
                                  child: Text(
                                    'Waiting for sender',
                                    style: text.bodySmall?.copyWith(
                                      color: scheme.onSurfaceVariant,
                                    ),
                                  ),
                                ),
                              TextButton(
                                onPressed: () => state.transfer
                                    .discardInterrupted(transfer.id),
                                child: const Text('Discard'),
                              ),
                            ]
                          : !awaitingApproval
                          ? [
                              IconButton(
                                tooltip: paused ? 'Resume' : 'Pause',
                                onPressed: () => paused
                                    ? state.transfer.resume(transfer.id)
                                    : state.transfer.pause(transfer.id),
                                icon: Icon(
                                  paused
                                      ? Icons.play_arrow_rounded
                                      : Icons.pause_rounded,
                                ),
                              ),
                              IconButton(
                                tooltip: 'Cancel',
                                onPressed: () =>
                                    state.transfer.cancel(transfer.id),
                                icon: const Icon(Icons.close_rounded),
                              ),
                            ]
                          // Selecting: the checkbox and the card tap above are
                          // the one action path on screen. Showing Decline/
                          // Accept/Trust here too would be a second path to the
                          // same decision, and Trust specifically must never
                          // read as a batch action.
                          : selecting
                          ? const []
                          : [
                              TextButton(
                                onPressed: () =>
                                    state.transfer.reject(transfer.id),
                                child: const Text('Decline'),
                              ),
                              // Accept and Trust both go through the
                              // first-contact check. Decline does not: refusing
                              // needs no verification, and it is the answer a
                              // user who cannot match the codes should be able
                              // to give without another prompt in the way.
                              FilledButton.tonal(
                                onPressed: () => acceptWithPairingCheck(
                                  context,
                                  transfer,
                                  needsConfirmation: state.transfer
                                      .needsPairingConfirmation(transfer.id),
                                  accept: ({required confirmed}) =>
                                      state.transfer.accept(
                                        transfer.id,
                                        confirmed: confirmed,
                                      ),
                                ),
                                child: const Text('Accept'),
                              ),
                              Tooltip(
                                message: 'Accept and always trust this device',
                                child: FilledButton(
                                  onPressed: () => acceptWithPairingCheck(
                                    context,
                                    transfer,
                                    needsConfirmation: state.transfer
                                        .needsPairingConfirmation(transfer.id),
                                    accept: ({required confirmed}) =>
                                        state.transfer.acceptTrust(
                                          transfer.id,
                                          confirmed: confirmed,
                                        ),
                                  ),
                                  child: const Text('Trust'),
                                ),
                              ),
                            ],
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
