import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/error_text.dart';
import '../../sdk/exceptions.dart';
import '../../sdk/models.dart' show WakeAttempt;
import '../../sdk/peerbeam.dart';
import '../../state/models.dart';
import 'mac_address.dart';

/// Waking a device: the two limits, the address, and what was sent.
///
/// # The two sentences this dialog exists to keep saying
///
/// 1. **Local network only.** A magic packet is a broadcast, and a broadcast
///    has no destination to be routed to — it does not travel over Tailscale, a
///    VPN or the internet, and no amount of work on PeerBeam's side would
///    change that. A user whose only sighting of a machine was over a tailnet
///    has to read that *before* sending, not deduce it from a wake that
///    appeared to work and did nothing.
/// 2. **Nothing confirms a wake.** The protocol has no reply, so
///    [WakeAttempt] carries what was sent and pointedly no "woken" flag. Every
///    word of the result below is therefore about this machine — the packet
///    left, here is where it went — and the device list is named as the only
///    real confirmation. "Woken", "started" or a green tick here would be an
///    invention.
///
/// Approval is checked here rather than assumed, because sending a magic packet
/// is an action on someone else's hardware (invariant I6) and the engine
/// refuses an unapproved device by name. Surfacing that refusal in the dialog —
/// with the device named, and no address field to fill in — beats letting the
/// user type a MAC and receive a generic failure.
class WakeDialog extends StatefulWidget {
  final PeerBeamApi api;
  final Device device;

  const WakeDialog({super.key, required this.api, required this.device});

  @override
  State<WakeDialog> createState() => _WakeDialogState();
}

/// Opens [WakeDialog] for [device].
Future<void> showWakeDialog(
  BuildContext context, {
  required PeerBeamApi api,
  required Device device,
}) => showDialog<void>(
  context: context,
  builder: (_) => WakeDialog(api: api, device: device),
);

class _WakeDialogState extends State<WakeDialog> {
  final TextEditingController _mac = TextEditingController();

  /// Whether the device is approved: null until the check answers, which is
  /// **not** the same as false. See [_checkError].
  bool? _approved;

  /// The approval check's failure, kept apart from `_approved == false`. A
  /// trust list that did not load says nothing about this device's standing,
  /// and rendering the two alike would accuse an approved device of being a
  /// stranger.
  Object? _checkError;

  bool _sending = false;

  /// What the engine reported it sent. Non-null means one attempt has been
  /// made — never that anything received it.
  WakeAttempt? _attempt;
  Object? _sendError;

  @override
  void initState() {
    super.initState();
    _check();
  }

  @override
  void dispose() {
    _mac.dispose();
    super.dispose();
  }

  /// Re-read trust rather than trusting a cached list: approval can be revoked
  /// between opening the device list and opening this dialog, and this is the
  /// one place where being out of date would offer an action the engine will
  /// refuse.
  Future<void> _check() async {
    setState(() {
      _approved = null;
      _checkError = null;
    });
    try {
      final pins = await widget.api.trustList();
      if (!mounted) return;
      setState(() {
        _approved = pins.any((p) => p.id == widget.device.id && p.approved);
      });
    } catch (e) {
      if (!mounted) return;
      setState(() => _checkError = e);
    }
  }

  /// Record the address, then send. Both, in that order, on every first send:
  /// the engine keeps the address as a note and offers no way to read it back,
  /// so the field is the only thing that can be shown — and a field that
  /// displayed nothing while the engine held something else would be worse than
  /// no field at all.
  Future<void> _send({required bool again}) async {
    final device = widget.device;
    setState(() {
      _sending = true;
      _sendError = null;
    });
    try {
      if (!again) {
        final parsed = parseMac(_mac.text);
        final mac = parsed.mac;
        // Belt and braces: the button is disabled without a parse, so this is
        // a refusal that should never be reachable rather than a validation
        // path. It exists so a future caller cannot make it reachable quietly.
        if (mac == null) {
          setState(() {
            _sending = false;
            _sendError = InvalidArgumentException(parsed.refusal ?? '');
          });
          return;
        }
        await widget.api.setWakeAddress(device.id, mac);
      }
      final attempt = await widget.api.wakeDevice(device.id);
      if (!mounted) return;
      setState(() {
        _attempt = attempt;
        _sending = false;
      });
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _sendError = e;
        _sending = false;
      });
    }
  }

  /// Drop the recorded address. Offered because the note is about someone
  /// else's hardware and this machine is the only place it exists — a user who
  /// wants it gone has nowhere else to go.
  Future<void> _forget() async {
    final messenger = ScaffoldMessenger.of(context);
    final navigator = Navigator.of(context);
    String message;
    try {
      final forgotten = await widget.api.forgetWakeAddress(widget.device.id);
      // "Nothing to forget" is a real answer, not a failure: the engine may
      // never have had an address for this device.
      message = forgotten
          ? 'Address forgotten. A later wake will ask for it again.'
          : 'There was no address recorded to forget.';
    } catch (e) {
      message = friendlyError(e);
    }
    navigator.pop();
    messenger
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(message)));
  }

  @override
  Widget build(BuildContext context) {
    final device = widget.device;
    return AlertDialog(
      icon: const Icon(Icons.power_settings_new_rounded),
      title: Text(_attempt == null ? 'Wake ${device.name}' : 'Packet sent'),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 420),
        child: SingleChildScrollView(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: _content(context),
          ),
        ),
      ),
      actions: _actions(context),
    );
  }

  List<Widget> _content(BuildContext context) {
    final attempt = _attempt;
    if (attempt != null) return _result(context, attempt);
    if (_checkError != null) return _checkFailed(context);
    if (_approved == null) return _checking(context);
    if (_approved == false) return _refused(context);
    return _form(context);
  }

  List<Widget> _actions(BuildContext context) {
    final close = TextButton(
      onPressed: () => Navigator.of(context).pop(),
      child: const Text('Close'),
    );
    if (_attempt != null) {
      return [
        TextButton(onPressed: _forget, child: const Text('Forget address')),
        TextButton(
          onPressed: _sending ? null : () => _send(again: true),
          child: const Text('Send again'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Done'),
        ),
      ];
    }
    if (_checkError != null) {
      return [
        close,
        FilledButton.tonalIcon(
          onPressed: _check,
          icon: const Icon(Icons.refresh_rounded),
          label: const Text('Try again'),
        ),
      ];
    }
    if (_approved != true) return [close];
    final ready = parseMac(_mac.text).mac != null;
    return [
      TextButton(
        onPressed: () => Navigator.of(context).pop(),
        child: const Text('Cancel'),
      ),
      FilledButton(
        onPressed: ready && !_sending ? () => _send(again: false) : null,
        child: const Text('Send wake packet'),
      ),
    ];
  }

  // ── The four states before a send ───────────────────────────

  List<Widget> _checking(BuildContext context) => [
    Row(
      children: [
        const SizedBox.square(
          dimension: 16,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
        const Gap(AppSpace.sm),
        Expanded(
          child: Text('Checking whether ${widget.device.name} is approved…'),
        ),
      ],
    ),
  ];

  List<Widget> _checkFailed(BuildContext context) => [
    _Note(
      icon: Icons.error_outline_rounded,
      tone: _Tone.error,
      // Deliberately not "not approved": the check did not answer, and the
      // difference between "we may not" and "we could not ask" is the whole
      // point of keeping `_checkError` separate from `_approved == false`.
      text:
          'PeerBeam could not check whether ${widget.device.name} is approved, '
          'and will not send a wake packet on a guess. ${friendlyError(_checkError!)}',
    ),
  ];

  List<Widget> _refused(BuildContext context) => [
    _Note(
      icon: Icons.block_rounded,
      tone: _Tone.error,
      // The engine's own refusal, in the engine's own terms, so the CLI and
      // the app give one answer to the same question.
      text:
          '${widget.device.name} is not an approved device, so PeerBeam will '
          'not wake it.',
    ),
    const Gap(AppSpace.sm),
    Text(
      'Waking a machine is an action on its hardware, so it is limited to '
      'devices you chose. Approve this one first — accept a transfer from it, '
      'or approve it under Trusted devices in Settings.',
      style: Theme.of(context).textTheme.bodySmall,
    ),
  ];

  List<Widget> _form(BuildContext context) {
    final device = widget.device;
    final refusal = _mac.text.trim().isEmpty
        ? null
        : parseMac(_mac.text).refusal;
    return [
      _Note(
        icon: Icons.lan_rounded,
        tone: _Tone.plain,
        text:
            'Local network only. A wake packet is a broadcast, so it cannot '
            'travel over Tailscale, a VPN or the internet.',
      ),
      // The warning a tailnet user needs before they try, not after. It does
      // not disable the send: a machine last seen over Tailscale may well be
      // sitting on this network right now — asleep and therefore invisible to
      // discovery — and refusing outright would be a claim about where the
      // hardware is that nothing here can support.
      if (!device.reach.contains(Reach.lan)) ...[
        const Gap(AppSpace.sm),
        _Note(
          icon: Icons.warning_amber_rounded,
          tone: _Tone.warning,
          text:
              '${device.name} was last seen over ${device.reach.map((r) => r.label).join(' and ')}, '
              'not on this network. A broadcast cannot follow that path — send '
              'this only if the machine is on the same network as this one.',
        ),
      ],
      const Gap(AppSpace.sm),
      _Note(
        icon: Icons.help_outline_rounded,
        tone: _Tone.plain,
        text:
            'Nothing confirms a wake. The packet has no reply, so PeerBeam can '
            'only report what it sent.',
      ),
      const Gap(AppSpace.sm),
      _Note(
        icon: Icons.memory_rounded,
        tone: _Tone.plain,
        text: _listenHint(device.platform),
      ),
      const Gap(AppSpace.md),
      TextField(
        controller: _mac,
        autofocus: true,
        // Rebuild on every keystroke: the Send button's enabled state and the
        // field's own reason both come straight from the parser, so the two
        // can never disagree about whether the address is usable.
        onChanged: (_) => setState(() {}),
        onSubmitted: (_) =>
            parseMac(_mac.text).mac == null ? null : _send(again: false),
        decoration: InputDecoration(
          labelText: 'Hardware address (MAC)',
          errorText: refusal,
          helperText: 'Accepted: $macShapes',
          helperMaxLines: 2,
          errorMaxLines: 3,
        ),
      ),
      const Gap(AppSpace.xs),
      Text(
        // Why the user is typing this at all, said once: it forestalls the
        // reasonable assumption that the app is being lazy.
        'PeerBeam cannot discover this for a machine that is off. '
        '${_addressHint(device.platform)}',
        style: Theme.of(context).textTheme.bodySmall?.copyWith(
          color: Theme.of(context).colorScheme.onSurfaceVariant,
        ),
      ),
      if (_sendError != null) ...[
        const Gap(AppSpace.md),
        _Note(
          icon: Icons.error_outline_rounded,
          tone: _Tone.error,
          text: wakeFailureText(_sendError!, device.name),
        ),
      ],
    ];
  }

  // ── After a send ────────────────────────────────────────────

  /// What was sent, and — at length, because this is where a user is most
  /// likely to read a promise that was never made — what that does and does
  /// not mean.
  List<Widget> _result(BuildContext context, WakeAttempt attempt) {
    final device = widget.device;
    final text = Theme.of(context).textTheme;
    return [
      if (attempt.sentTo.isEmpty)
        // The engine reported no destination. Saying "sent" here would invent
        // the one fact this screen has: that a packet left the machine.
        _Note(
          icon: Icons.warning_amber_rounded,
          tone: _Tone.warning,
          text:
              'The engine reported no address for the packet, so nothing left '
              'this machine. Check that this network has a broadcast address, '
              'then try again.',
        )
      else
        _Note(
          icon: Icons.outbox_rounded,
          tone: _Tone.plain,
          text:
              'A packet for ${attempt.mac} went out to '
              '${_joinAddresses(attempt.sentTo)}.',
        ),
      const Gap(AppSpace.md),
      Text(
        'That is everything PeerBeam knows. A wake packet has no reply, so '
        'whether ${device.name} received it — and whether it is coming up — is '
        'not something this app can see, and it will not pretend otherwise.',
        style: text.bodyMedium,
      ),
      const Gap(AppSpace.sm),
      Text(
        'Watch the device list. ${device.name} appearing there is the only '
        'real confirmation.',
        style: text.bodyMedium?.copyWith(fontWeight: FontWeight.w600),
      ),
      const Gap(AppSpace.sm),
      Text(
        // Straight from the protocol's shape: the packet is idempotent and a
        // single broadcast can be dropped with nothing to notice the loss.
        'Sending again is safe — a machine that is already up ignores the '
        'packet, and a lone broadcast can go missing unnoticed.',
        style: text.bodySmall?.copyWith(
          color: Theme.of(context).colorScheme.onSurfaceVariant,
        ),
      ),
      if (_sendError != null) ...[
        const Gap(AppSpace.md),
        _Note(
          icon: Icons.error_outline_rounded,
          tone: _Tone.error,
          text: wakeFailureText(_sendError!, device.name),
        ),
      ],
    ];
  }
}

/// A refused or failed wake, in the user's terms.
///
/// The engine's refusals are recognised by class and re-stated here rather than
/// forwarded: raw engine strings never reach a widget, but "That action can't
/// be completed" in front of a device the engine declined to wake *by name*
/// throws away the only useful part of the answer.
String wakeFailureText(Object error, String deviceName) {
  final detail = error is PeerBeamException
      ? error.message.toLowerCase()
      : error.toString().toLowerCase();
  if (detail.contains('approve')) {
    return '$deviceName is not an approved device, so PeerBeam will not wake it.';
  }
  if (detail.contains('no address') || detail.contains('not recorded')) {
    return 'PeerBeam has no hardware address recorded for $deviceName, so '
        'there was nothing to send.';
  }
  if (detail.contains('hardware address')) {
    return 'The engine would not accept that hardware address. Check it '
        'against the device and try again.';
  }
  return friendlyError(error);
}

/// `a`, `a and b`, `a, b and c` — the addresses a packet went to, which is
/// two or three of them on a typical network.
String _joinAddresses(List<String> addresses) {
  if (addresses.length == 1) return addresses.single;
  final head = addresses.take(addresses.length - 1).join(', ');
  return '$head and ${addresses.last}';
}

/// That the target has to be listening, in the terms of its own platform.
String _listenHint(String platform) => switch (platform) {
  'android' || 'ios' =>
    'Most phones and tablets do not listen for wake packets at all, and '
        'PeerBeam cannot change that.',
  _ =>
    'The device must have Wake-on-LAN switched on in its own firmware — '
        'PeerBeam cannot enable it remotely.',
};

/// Where to read the address off the device itself.
String _addressHint(String platform) => switch (platform) {
  'linux' => 'On Linux: `ip link`, the link/ether line.',
  'macos' => 'On macOS: System Settings → Network → Details → Hardware.',
  'windows' => 'On Windows: `ipconfig /all`, the “Physical Address” line.',
  _ => 'Read it off the device’s own network settings.',
};

enum _Tone { plain, warning, error }

/// One short paragraph with an icon: the shape every limit on this dialog is
/// stated in, so none of them reads as decoration next to the others.
class _Note extends StatelessWidget {
  final IconData icon;
  final String text;
  final _Tone tone;

  const _Note({required this.icon, required this.text, required this.tone});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final color = switch (tone) {
      _Tone.plain => theme.colorScheme.onSurfaceVariant,
      _Tone.warning => AppColors.warning(theme.brightness),
      _Tone.error => theme.colorScheme.error,
    };
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Icon(icon, size: AppIcons.sm, color: color),
        const Gap(AppSpace.xs),
        Expanded(
          child: Text(
            text,
            style: theme.textTheme.bodyMedium?.copyWith(
              color: tone == _Tone.plain ? theme.colorScheme.onSurface : color,
            ),
          ),
        ),
      ],
    );
  }
}
