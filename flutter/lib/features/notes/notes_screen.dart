import 'package:flutter/material.dart';

import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../../app/theme.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';

/// Notes kept on this device.
///
/// A list and an editor, and nothing more: notes are text with a heading, and
/// anything richer — folders, attachments, formatting — would be a different
/// feature wearing this one's name.
class NotesScreen extends StatefulWidget {
  const NotesScreen({super.key});

  @override
  State<NotesScreen> createState() => _NotesScreenState();
}

class _NotesScreenState extends State<NotesScreen> {
  @override
  void initState() {
    super.initState();
    // After the first frame, like every other screen: the repository may not
    // be attached to a live engine yet during boot.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      AppScope.of(context).notes.refresh();
    });
  }

  Future<void> _edit(Note? existing) async {
    final state = AppScope.of(context);
    final saved = await showDialog<_Draft>(
      context: context,
      builder: (_) => _NoteEditor(note: existing),
    );
    if (saved == null || !mounted) return;

    if (existing == null) {
      await state.notes.create(saved.body, title: saved.title);
      return;
    }
    final ok = await state.notes.edit(
      existing.id,
      saved.body,
      title: saved.title,
    );
    if (!ok && mounted) {
      // The engine refuses to edit a deleted note rather than resurrecting it,
      // so the user's edit genuinely did not land. Saying nothing would leave
      // them believing it had.
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text('That note was deleted, so the edit was not saved.'),
        ),
      );
    }
  }

  /// Offer the devices that may sync notes, and exchange with the chosen one.
  ///
  /// Only devices that have actually been granted the permission are listed:
  /// offering a device that would be refused turns a permission the user set
  /// into a failure they have to diagnose.
  Future<void> _sync() async {
    final state = AppScope.of(context);
    final permitted = state.trust.items
        .where((d) => d.permissions.contains(PeerBeamPermission.notes))
        .toList();

    if (permitted.isEmpty) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text(
            'No device may sync notes yet. Grant one the Notes permission in '
            'Settings → Trusted devices.',
          ),
        ),
      );
      return;
    }

    final chosen = await showDialog<TrustedDevice>(
      context: context,
      builder: (dialogContext) => SimpleDialog(
        title: const Text('Sync notes with'),
        children: [
          for (final d in permitted)
            SimpleDialogOption(
              onPressed: () => Navigator.of(dialogContext).pop(d),
              child: Text(d.name),
            ),
        ],
      ),
    );
    if (chosen == null || !mounted) return;

    // The device list is the trust store's, which holds no addresses — the
    // engine resolves the peer itself, so an empty target here is honest
    // rather than a guess at where the device is.
    final sent = await state.notes.sync(
      PeerTarget(
        id: chosen.id,
        name: chosen.name,
        addresses: const [],
        port: 0,
      ),
    );
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        content: Text(
          sent
              ? 'Notes sent to ${chosen.name}. Theirs will arrive shortly.'
              : 'Could not reach ${chosen.name}.',
        ),
      ),
    );
  }

  Future<void> _delete(Note note) async {
    final state = AppScope.of(context);
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        title: const Text('Delete note?'),
        content: Text(note.heading),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(dialogContext).pop(false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(dialogContext).pop(true),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
    if (confirmed != true) return;
    await state.notes.delete(note.id);
  }

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    return Scaffold(
      appBar: AppBar(
        title: const Text('Notes'),
        actions: [
          IconButton(
            icon: const Icon(Icons.sync_rounded),
            tooltip: 'Sync notes with a device',
            onPressed: _sync,
          ),
        ],
      ),
      floatingActionButton: FloatingActionButton(
        onPressed: () => _edit(null),
        tooltip: 'New note',
        child: const Icon(Icons.add_rounded),
      ),
      body: AnimatedBuilder(
        animation: state.notes,
        builder: (context, _) {
          final notes = state.notes.notes;
          // `loaded` distinguishes "no notes" from "not read yet", so the empty
          // state does not flash on every open.
          if (!state.notes.loaded) {
            return const SizedBox.shrink();
          }
          if (notes.isEmpty) {
            return const EmptyState(
              icon: Icons.sticky_note_2_outlined,
              title: 'No notes yet',
              message: 'Notes are kept on this device.',
            );
          }
          return ListView.builder(
            padding: const EdgeInsets.all(AppSpace.md),
            itemCount: notes.length,
            itemBuilder: (context, i) {
              final note = notes[i];
              return Appear(
                index: i,
                child: Card(
                  child: ListTile(
                    title: Text(note.heading),
                    subtitle: Text(
                      note.body,
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                    ),
                    onTap: () => _edit(note),
                    trailing: IconButton(
                      icon: const Icon(Icons.delete_outline_rounded),
                      tooltip: 'Delete',
                      onPressed: () => _delete(note),
                    ),
                  ),
                ),
              );
            },
          );
        },
      ),
    );
  }
}

/// What the editor hands back.
class _Draft {
  final String title;
  final String body;
  const _Draft(this.title, this.body);
}

class _NoteEditor extends StatefulWidget {
  final Note? note;
  const _NoteEditor({this.note});

  @override
  State<_NoteEditor> createState() => _NoteEditorState();
}

class _NoteEditorState extends State<_NoteEditor> {
  late final TextEditingController _title = TextEditingController(
    text: widget.note?.title ?? '',
  );
  late final TextEditingController _body = TextEditingController(
    text: widget.note?.body ?? '',
  );

  @override
  void dispose() {
    _title.dispose();
    _body.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      // Scrollable, because this dialog is two text fields and a phone keyboard
      // takes half the screen. `AlertDialog` shrinks to the space the keyboard
      // leaves and hands what is left to `content`; an unscrolled Column in
      // there simply overflowed, and the field being typed into was the part
      // that went missing. The keyboard is up whenever this dialog is useful,
      // so this is the ordinary case and not an edge one.
      scrollable: true,
      title: Text(widget.note == null ? 'New note' : 'Edit note'),
      content: SizedBox(
        width: 420,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              controller: _title,
              decoration: const InputDecoration(
                labelText: 'Title',
                hintText: 'Optional',
              ),
            ),
            const Gap(AppSpace.sm),
            TextField(
              controller: _body,
              minLines: 4,
              maxLines: 10,
              autofocus: true,
              decoration: const InputDecoration(labelText: 'Note'),
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
          animation: _body,
          builder: (context, _) => FilledButton(
            // An empty note is not a note. Disabling beats saving something the
            // list would render as "Untitled" with nothing under it.
            onPressed: _body.text.trim().isEmpty
                ? null
                : () => Navigator.of(
                    context,
                  ).pop(_Draft(_title.text.trim(), _body.text)),
            child: const Text('Save'),
          ),
        ),
      ],
    );
  }
}
