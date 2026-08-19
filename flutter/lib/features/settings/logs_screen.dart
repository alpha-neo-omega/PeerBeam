import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../app/theme.dart';
import '../../sdk/error_text.dart';
import '../../sdk/models.dart' show LogLine;
import '../../state/app_scope.dart';
import '../../widgets/common.dart';

/// What this device recorded, and how to get it out of here.
///
/// # Why the newest line is at the bottom, and why the view starts there
///
/// The engine hands the buffer back oldest-first, which is the order a log has
/// to be read in: a line only means anything next to the ones around it.
/// Reversing it would put every line's cause below its effect. So the list
/// keeps the engine's order and scrolls to the end instead — the newest line is
/// the one the log was opened for.
///
/// # Why a problem is coloured, not merely worded
///
/// Nobody opens a log to read it. They open it to find the two lines that went
/// wrong among two hundred that did not, and scanning for the word `ERROR` is
/// exactly the search a colour saves. `LogLine.isProblem` is the engine's own
/// answer to which those are, so the level strings are never parsed here.
///
/// # Why there is a Refresh button and no live stream
///
/// The SDK can ask the engine to push `log_received` events, but nothing in
/// `BridgeEvent` decodes one yet, so subscribing would raise traffic that is
/// dropped on arrival. A button that re-reads is honest about that; a screen
/// that looked live and was not would be worse than one that plainly is not.
class LogsScreen extends StatefulWidget {
  const LogsScreen({super.key});

  @override
  State<LogsScreen> createState() => _LogsScreenState();
}

class _LogsScreenState extends State<LogsScreen> {
  /// How many lines to ask for. The buffer is bounded anyway; this bounds what
  /// is rendered, which is the part that costs.
  static const _limit = 200;

  final _scroll = ScrollController();
  List<LogLine> _lines = const [];
  bool _loaded = false;
  bool _exporting = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) => _load());
  }

  @override
  void dispose() {
    _scroll.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    final api = AppScope.of(context).api;
    final lines = api == null ? <LogLine>[] : await api.logs(limit: _limit);
    if (!mounted) return;
    setState(() {
      _lines = lines;
      _loaded = true;
    });
    // After layout, not during it: the extent is unknown until the list has
    // been built with the new lines in it.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_scroll.hasClients) _scroll.jumpTo(_scroll.position.maxScrollExtent);
    });
  }

  /// Copy the buffer to a file and say where it went.
  ///
  /// Naming the destination is the whole point: this exists so a bug report can
  /// carry the logs, and "Exported" with no path leaves someone hunting for a
  /// file they were never told the name of.
  Future<void> _export() async {
    final api = AppScope.of(context).api;
    if (api == null) return;
    setState(() => _exporting = true);
    String message;
    String? written;
    try {
      written = await api.exportLogs();
      message = 'Logs written to $written';
    } catch (e) {
      message = friendlyError(e);
    }
    if (!mounted) return;
    setState(() => _exporting = false);
    final path = written;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(
        SnackBar(
          content: Text(message),
          // A path is gone the moment the snackbar is, and it is the one thing
          // that has to be typed somewhere else — attach it rather than expect
          // it to be memorised.
          action: path == null
              ? null
              : SnackBarAction(
                  label: 'Copy path',
                  onPressed: () => Clipboard.setData(ClipboardData(text: path)),
                ),
        ),
      );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Logs'),
        actions: [
          IconButton(
            icon: const Icon(Icons.refresh_rounded),
            tooltip: 'Refresh',
            onPressed: _load,
          ),
          IconButton(
            icon: _exporting
                ? const SizedBox(
                    width: AppIcons.sm,
                    height: AppIcons.sm,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.ios_share_rounded),
            tooltip: 'Export to a file',
            onPressed: _exporting ? null : _export,
          ),
        ],
      ),
      body: SafeArea(
        child: !_loaded
            ? const SizedBox.shrink()
            : _lines.isEmpty
            // An empty buffer is a fact worth stating. A blank page reads as a
            // screen that failed to load, and the two are not the same news.
            ? const EmptyState(
                icon: Icons.article_outlined,
                title: 'Nothing logged yet',
                message:
                    'Logs cover the session this device is in: they start '
                    'empty each time it starts, and fill as it does things.',
              )
            : ContentPane(
                maxWidth: 720,
                child: ListView.builder(
                  controller: _scroll,
                  padding: const EdgeInsets.all(AppSpace.md),
                  itemCount: _lines.length,
                  itemBuilder: (context, i) => _LogTile(line: _lines[i]),
                ),
              ),
      ),
    );
  }
}

/// One line: the message, and beneath it whatever the engine could say about
/// where and when it came from.
class _LogTile extends StatelessWidget {
  const _LogTile({required this.line});

  final LogLine line;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final problem = line.isProblem;
    final foreground = problem ? scheme.onErrorContainer : null;
    return Container(
      margin: const EdgeInsets.only(bottom: AppSpace.xxs),
      padding: const EdgeInsets.all(AppSpace.sm),
      decoration: BoxDecoration(
        color: problem ? scheme.errorContainer : scheme.surfaceContainerLow,
        borderRadius: BorderRadius.circular(AppRadius.md),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(
            problem ? Icons.error_outline_rounded : Icons.info_outline_rounded,
            size: AppIcons.sm,
            color: problem ? scheme.error : scheme.outline,
          ),
          const Gap(AppSpace.sm),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  line.message,
                  style: theme.textTheme.bodyMedium?.copyWith(
                    color: foreground,
                  ),
                ),
                Text(
                  _meta(line),
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: foreground ?? scheme.onSurfaceVariant,
                    fontFeatures: const [FontFeature.tabularFigures()],
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }

  /// `10:04:12 · WARN · peerbeam_sync`, skipping whatever the engine could not
  /// state rather than rendering an empty slot for it.
  static String _meta(LogLine line) => [
    if (line.at.isNotEmpty) _clock(line.at),
    if (line.level.isNotEmpty) line.level,
    if (line.target.isNotEmpty) line.target,
  ].join(' · ');

  /// The time out of an RFC-3339 stamp: `2026-08-19T10:04:12.123Z` reads as
  /// `10:04:12`. The date is dropped because the buffer only ever covers the
  /// running session. Anything that is not a stamp is shown exactly as it came
  /// — guessing at its shape would be worse than a long string.
  static String _clock(String at) {
    final t = at.indexOf('T');
    if (t < 0 || at.length < t + 9) return at;
    return at.substring(t + 1, t + 9);
  }
}
