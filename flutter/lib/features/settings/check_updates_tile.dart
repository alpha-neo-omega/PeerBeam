import 'package:flutter/material.dart';

import '../../sdk/error_text.dart';
import '../../sdk/models.dart' show UpdateCheck;
import '../../sdk/peerbeam.dart' show PeerBeamApi;

/// "Check for updates", and the answer.
///
/// Stateful for one reason: the request must not start until someone presses
/// the button, so the result cannot come from anything the screen builds
/// eagerly. Failure is reported as plainly as success — an app that cannot
/// reach the internet is working normally, and amendment A1 makes "never a
/// precondition" a term of the check being allowed to exist.
class CheckUpdatesTile extends StatefulWidget {
  final PeerBeamApi? api;
  const CheckUpdatesTile({super.key, required this.api});

  @override
  State<CheckUpdatesTile> createState() => _CheckUpdatesTileState();
}

class _CheckUpdatesTileState extends State<CheckUpdatesTile> {
  bool _busy = false;
  UpdateCheck? _result;
  Object? _error;

  Future<void> _check() async {
    final api = widget.api;
    if (api == null || _busy) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final r = await api.checkForUpdates();
      if (!mounted) return;
      setState(() {
        _result = r;
        _busy = false;
      });
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _error = e;
        _busy = false;
      });
    }
  }

  String get _subtitle {
    if (_busy) return 'Asking…';
    final e = _error;
    if (e != null) return 'Could not check — ${friendlyError(e)}';
    final r = _result;
    if (r == null) {
      return 'PeerBeam does not check on its own. Ask, and it will look once.';
    }
    if (!r.reachable) {
      return 'Could not reach the release list. You have ${r.current}.';
    }
    if (r.updateAvailable) {
      return '${r.latest} is available — you have ${r.current}.';
    }
    return 'You have ${r.current}, which is the newest.';
  }

  @override
  Widget build(BuildContext context) => ListTile(
    leading: const Icon(Icons.system_update_alt_rounded),
    title: const Text('Check for updates'),
    subtitle: Text(_subtitle),
    trailing: _busy
        ? const SizedBox.square(
            dimension: 20,
            child: CircularProgressIndicator(strokeWidth: 2),
          )
        : FilledButton.tonal(
            onPressed: widget.api == null ? null : _check,
            child: const Text('Check'),
          ),
  );
}
