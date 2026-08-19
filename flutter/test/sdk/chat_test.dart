// Unit tests for the chat SDK layer: ChatMessage decoding/copyWith and the
// `chat_received`/`chat_status` bridge events.
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';

void main() {
  group('ChatMessage.fromJson', () {
    test('decodes a full message', () {
      final msg = ChatMessage.fromJson({
        'id': 'msg-1',
        'peer_id': 'peer-1',
        'direction': 'out',
        'timestamp': '2026-08-10T12:00:00Z',
        'body': 'hello',
        'status': 'sent',
      });

      expect(msg.id, 'msg-1');
      expect(msg.peerId, 'peer-1');
      expect(msg.direction, 'out');
      expect(msg.body, 'hello');
      expect(msg.status, 'sent');
      expect(msg.at, DateTime.parse('2026-08-10T12:00:00Z'));
      expect(msg.isMine, isTrue);
    });

    test('defaults missing/malformed fields', () {
      final msg = ChatMessage.fromJson(const {});

      expect(msg.id, '');
      expect(msg.peerId, '');
      expect(msg.direction, 'in');
      expect(msg.body, '');
      expect(msg.status, 'received');
      expect(msg.isMine, isFalse);
    });

    // The exact shape `events::record_dto` emits for a file record: an EMPTY
    // body (rendering it as text would produce a blank bubble) plus `kind` and
    // a `file` object. `local_path` is null until a receive completes.
    test('decodes a file record: kind + file{name,size,local_path}', () {
      final msg = ChatMessage.fromJson(const {
        'id': 'fr-1',
        'peer_id': 'pb-bob',
        'direction': 'in',
        'timestamp': '2026-08-12T09:00:00Z',
        'body': '',
        'status': 'pendingapproval',
        'kind': 'file',
        'file': {'name': 'report.pdf', 'size': 4096, 'local_path': null},
      });

      expect(msg.isFile, isTrue);
      expect(msg.kind, 'file');
      expect(msg.fileName, 'report.pdf');
      expect(msg.fileSize, 4096);
      expect(msg.localPath, isNull);
      expect(msg.body, isEmpty);
      expect(msg.status, ChatStatusValue.pendingApproval);
      expect(msg.awaitingApproval, isTrue);
    });

    test('a completed received file carries its saved local_path', () {
      final msg = ChatMessage.fromJson(const {
        'id': 'fr-2',
        'peer_id': 'pb-bob',
        'direction': 'in',
        'timestamp': '2026-08-12T09:00:00Z',
        'body': '',
        'status': 'received',
        'kind': 'file',
        'file': {
          'name': 'report.pdf',
          'size': 4096,
          'local_path': '/home/me/Downloads/report.pdf',
        },
      });

      expect(msg.localPath, '/home/me/Downloads/report.pdf');
      expect(msg.awaitingApproval, isFalse);
    });

    // A 1a/1b-era persisted record has NO `kind` and NO `file` keys at all.
    // It must still decode and read as an ordinary text message — the Rust
    // side guarantees the same via `#[serde(default)]` (see
    // `legacy_record_json_decodes_as_text`).
    test('a legacy record with no kind/file keys stays a text message', () {
      final msg = ChatMessage.fromJson(const {
        'id': 'm-legacy',
        'peer_id': 'pb-alice',
        'direction': 'out',
        'timestamp': '2026-08-10T12:00:00Z',
        'body': 'hello from 1a',
        'status': 'sent',
      });

      expect(msg.kind, ChatMessageKind.text);
      expect(msg.isFile, isFalse);
      expect(msg.fileName, isNull);
      expect(msg.fileSize, isNull);
      expect(msg.localPath, isNull);
      expect(msg.body, 'hello from 1a');
    });

    // `record_dto` always emits the key, as an explicit null, for a text
    // record — the null must not be mistaken for a file.
    test('an explicit null file is not a file record', () {
      final msg = ChatMessage.fromJson(const {
        'id': 'm1',
        'peer_id': 'pb-alice',
        'direction': 'out',
        'timestamp': '2026-08-10T12:00:00Z',
        'body': 'hi',
        'status': 'sent',
        'kind': 'text',
        'file': null,
      });

      expect(msg.isFile, isFalse);
      expect(msg.fileName, isNull);
    });

    test(
      'a file record with a malformed file object degrades, not crashes',
      () {
        final msg = ChatMessage.fromJson(const {
          'id': 'fr-3',
          'peer_id': 'pb-bob',
          'direction': 'in',
          'body': '',
          'status': 'transferring',
          'kind': 'file',
          'file': 'not-an-object',
        });

        expect(msg.isFile, isTrue);
        expect(msg.fileName, isNull);
        expect(msg.fileSize, isNull);
      },
    );
  });

  // The statuses are the Rust `peerbeam_chat::Status` enum under
  // `#[serde(rename_all = "lowercase")]`, which lowercases the whole variant
  // name WITHOUT a separator. Getting `pendingapproval` wrong (as
  // `pendingApproval` or `pending_approval`) silently disables the inline
  // approval UI, so it is pinned here.
  group('ChatStatusValue', () {
    test('spells every status exactly as the engine serializes it', () {
      expect(ChatStatusValue.pending, 'pending');
      expect(ChatStatusValue.sent, 'sent');
      expect(ChatStatusValue.received, 'received');
      expect(ChatStatusValue.pendingApproval, 'pendingapproval');
      expect(ChatStatusValue.transferring, 'transferring');
      expect(ChatStatusValue.declined, 'declined');
      expect(ChatStatusValue.failed, 'failed');
      expect(ChatStatusValue.interrupted, 'interrupted');
      // 2b. `Status::Staging` under the same `rename_all = "lowercase"`, so
      // one word and no separator — pinned by the Rust side's own
      // `staging_serializes_as_lowercase_and_is_not_settleable`. A wrong
      // spelling here would silently disable the staging bar and its Cancel.
      expect(ChatStatusValue.staging, 'staging');
    });
  });

  group('BridgeEvent chat_received', () {
    test('parses into a ChatReceived event carrying the message', () {
      final event = BridgeEvent.fromJson({
        'type': 'chat_received',
        'timestamp': '2026-08-10T12:00:00Z',
        'message': {
          'id': 'msg-2',
          'peer_id': 'peer-1',
          'direction': 'in',
          'timestamp': '2026-08-10T12:01:00Z',
          'body': 'hi there',
          'status': 'received',
        },
      });

      expect(event, isA<ChatReceived>());
      final message = (event as ChatReceived).message;
      expect(message.id, 'msg-2');
      expect(message.peerId, 'peer-1');
      expect(message.body, 'hi there');
      expect(message.isMine, isFalse);
    });

    test('unrecognised type yields null', () {
      expect(BridgeEvent.fromJson({'type': 'not_a_real_event'}), isNull);
    });

    test('carries a file offer through to the message', () {
      final event = BridgeEvent.fromJson({
        'type': 'chat_received',
        'timestamp': '2026-08-12T09:00:00Z',
        'message': {
          'id': 'fr-1',
          'peer_id': 'pb-bob',
          'direction': 'in',
          'timestamp': '2026-08-12T09:00:00Z',
          'body': '',
          'status': 'pendingapproval',
          'kind': 'file',
          'file': {'name': 'a.bin', 'size': 7, 'local_path': null},
        },
      });

      final message = (event as ChatReceived).message;
      expect(message.isFile, isTrue);
      expect(message.fileName, 'a.bin');
      expect(message.fileSize, 7);
      expect(message.awaitingApproval, isTrue);
    });
  });

  group('BridgeEvent chat_status', () {
    test('parses into a ChatStatus event carrying all three fields', () {
      final event = BridgeEvent.fromJson({
        'type': 'chat_status',
        'timestamp': '2026-08-10T12:02:00Z',
        'message_id': 'msg-3',
        'peer_id': 'peer-1',
        'status': 'sent',
      });

      expect(event, isA<ChatStatus>());
      final status = event as ChatStatus;
      expect(status.messageId, 'msg-3');
      expect(status.peerId, 'peer-1');
      expect(status.status, 'sent');
    });

    test('defaults missing fields to empty strings', () {
      final event = BridgeEvent.fromJson(const {'type': 'chat_status'});

      expect(event, isA<ChatStatus>());
      final status = event as ChatStatus;
      expect(status.messageId, '');
      expect(status.peerId, '');
      expect(status.status, '');
      // `error` is present only when the engine has something to say
      // (`chat_status_detail`), so every plain status leaves it null.
      expect(status.error, isNull);
    });

    // Staging progress rides THIS event — there is deliberately no new event
    // type for it — so a surface that already routes `chat_status` to a row
    // can draw the bar without learning a second vocabulary.
    test('a staging event carries its progress object', () {
      final event = BridgeEvent.fromJson(const {
        'type': 'chat_status',
        'timestamp': '2026-08-13T09:00:00Z',
        'message_id': 'fr-1',
        'peer_id': 'pb-bob',
        'status': 'staging',
        'progress': {'done': 4096, 'total': 8192},
      });

      final status = event as ChatStatus;
      expect(status.status, ChatStatusValue.staging);
      expect(status.progress?.done, 4096);
      expect(status.progress?.total, 8192);
    });

    test('every other status carries no progress at all', () {
      final event =
          BridgeEvent.fromJson(const {
                'type': 'chat_status',
                'message_id': 'fr-1',
                'peer_id': 'pb-bob',
                'status': 'pending',
              })
              as ChatStatus;

      expect(event.progress, isNull);
    });

    // The engine emits one bare `staging` event before the first byte moves
    // (from `chat_send_file` itself), and `done` can exceed `total` when the
    // source is appended to mid-copy. Neither may crash a consumer.
    test('a staging event with no progress, and an overshooting one, both '
        'decode', () {
      final bare =
          BridgeEvent.fromJson(const {
                'type': 'chat_status',
                'message_id': 'fr-1',
                'peer_id': 'pb-bob',
                'status': 'staging',
              })
              as ChatStatus;
      expect(bare.progress, isNull);

      final over =
          BridgeEvent.fromJson(const {
                'type': 'chat_status',
                'message_id': 'fr-1',
                'peer_id': 'pb-bob',
                'status': 'staging',
                'progress': {'done': 9000, 'total': 8192},
              })
              as ChatStatus;
      // Reported as sent, not clamped here: clamping is the renderer's job,
      // and swallowing it would hide that the source grew.
      expect(over.progress?.done, 9000);
      expect(over.progress?.total, 8192);
    });

    test('carries the engine\'s reason when a file share fails', () {
      final event = BridgeEvent.fromJson(const {
        'type': 'chat_status',
        'timestamp': '2026-08-12T09:00:00Z',
        'message_id': 'fr-1',
        'peer_id': 'pb-bob',
        'status': 'failed',
        'error': 'cannot reach Bob to send a.bin: no route',
      });

      final status = event as ChatStatus;
      expect(status.status, ChatStatusValue.failed);
      expect(status.error, 'cannot reach Bob to send a.bin: no route');
    });
  });

  // Every `transfer_*` event now carries the peer's device id alongside the
  // human-readable name — without it a surface cannot route a transfer event
  // to a conversation (the name is neither unique nor stable).
  group('TransferEvent', () {
    test('exposes peer_id and the queued size', () {
      final event =
          BridgeEvent.fromJson(const {
                'type': 'transfer_queued',
                'transfer_id': 'fr-1',
                'timestamp': '2026-08-12T09:00:00Z',
                'payload': {
                  'peer': 'Bob',
                  'peer_id': 'pb-bob',
                  'file': 'a.bin',
                  'size': 4096,
                },
              })
              as TransferEvent;

      expect(event.peerId, 'pb-bob');
      expect(event.peer, 'Bob');
      expect(event.file, 'a.bin');
      expect(event.size, 4096);
    });

    test('a payload without them yields nulls', () {
      final event =
          BridgeEvent.fromJson(const {
                'type': 'transfer_started',
                'transfer_id': 'tx-1',
                'payload': {},
              })
              as TransferEvent;

      expect(event.peerId, isNull);
      expect(event.size, isNull);
    });
  });

  // The exact shape `Manager::chat_conversations` emits.
  group('ChatConversation.fromJson', () {
    test('decodes a conversation row', () {
      final c = ChatConversation.fromJson(const {
        'peer_id': 'pb-bob',
        'last_timestamp': '2026-08-13T09:00:00Z',
        'unread_hint': 2,
      });

      expect(c.peerId, 'pb-bob');
      expect(c.lastAt, DateTime.parse('2026-08-13T09:00:00Z'));
      expect(c.unreadHint, 2);
      expect(c.needsAttention, isTrue);
    });

    // A thread this build cannot read is still LISTED (dropping it would hide
    // the very conversation the list exists to reach), with a null timestamp.
    test('a null last_timestamp decodes as "nothing known", not as now', () {
      final c = ChatConversation.fromJson(const {
        'peer_id': 'pb-quiet',
        'last_timestamp': null,
        'unread_hint': 0,
      });

      expect(c.peerId, 'pb-quiet');
      expect(c.lastAt, isNull);
      expect(c.unreadHint, 0);
      expect(c.needsAttention, isFalse);
    });

    test('missing fields default rather than throw', () {
      final c = ChatConversation.fromJson(const {});

      expect(c.peerId, '');
      expect(c.lastAt, isNull);
      expect(c.unreadHint, 0);
    });

    // `unread_hint` counts inbound file offers awaiting a decision — nothing
    // else. Zero therefore means "this thread is not waiting on you", NOT
    // "you have read everything", and no surface may render it as a count of
    // unread messages.
    test('needsAttention is the only claim unreadHint supports', () {
      const none = ChatConversation(
        peerId: 'pb-bob',
        lastAt: null,
        unreadHint: 0,
      );
      const some = ChatConversation(
        peerId: 'pb-bob',
        lastAt: null,
        unreadHint: 3,
      );

      expect(none.needsAttention, isFalse);
      expect(some.needsAttention, isTrue);
    });
  });

  group('ChatMessage.copyWith', () {
    test('changes only status, leaving every other field unchanged', () {
      final original = ChatMessage(
        id: 'msg-1',
        peerId: 'peer-1',
        direction: 'out',
        body: 'hello',
        at: DateTime.parse('2026-08-10T12:00:00Z'),
        status: 'pending',
      );

      final updated = original.copyWith(status: 'sent');

      expect(updated.status, 'sent');
      expect(updated.id, original.id);
      expect(updated.peerId, original.peerId);
      expect(updated.direction, original.direction);
      expect(updated.body, original.body);
      expect(updated.at, original.at);
    });

    test('omitting status keeps the original status', () {
      final original = ChatMessage(
        id: 'msg-1',
        peerId: 'peer-1',
        direction: 'out',
        body: 'hello',
        at: DateTime.parse('2026-08-10T12:00:00Z'),
        status: 'pending',
      );

      expect(original.copyWith().status, 'pending');
    });

    test('preserves a file record\'s kind and metadata', () {
      final original = ChatMessage(
        id: 'fr-1',
        peerId: 'pb-bob',
        direction: 'in',
        body: '',
        at: DateTime.parse('2026-08-12T09:00:00Z'),
        status: ChatStatusValue.pendingApproval,
        kind: ChatMessageKind.file,
        fileName: 'a.bin',
        fileSize: 7,
      );

      final updated = original.copyWith(status: ChatStatusValue.received);

      expect(updated.status, ChatStatusValue.received);
      expect(updated.isFile, isTrue);
      expect(updated.fileName, 'a.bin');
      expect(updated.fileSize, 7);
      expect(updated.localPath, isNull);
    });

    // A staging row learns its size (and, from a picker that only gave a path,
    // its name) after it was created, so `copyWith` has to be able to carry
    // them — while still leaving every omitted field alone.
    test('can set kind, name and size on a row that learns them late', () {
      final original = ChatMessage(
        id: 'local-1',
        peerId: 'pb-bob',
        direction: 'out',
        body: '',
        at: DateTime.parse('2026-08-13T09:00:00Z'),
        status: ChatStatusValue.staging,
      );

      final updated = original.copyWith(
        kind: ChatMessageKind.file,
        fileName: 'movie.mkv',
        fileSize: 5368709120,
      );

      expect(updated.isFile, isTrue);
      expect(updated.fileName, 'movie.mkv');
      expect(updated.fileSize, 5368709120);
      // Untouched.
      expect(updated.status, ChatStatusValue.staging);
      expect(updated.id, 'local-1');
      expect(updated.at, original.at);
    });

    test('omitting the new fields never clears them', () {
      final original = ChatMessage(
        id: 'fr-1',
        peerId: 'pb-bob',
        direction: 'out',
        body: '',
        at: DateTime.parse('2026-08-13T09:00:00Z'),
        status: ChatStatusValue.staging,
        kind: ChatMessageKind.file,
        fileName: 'movie.mkv',
        fileSize: 7,
      );

      final updated = original.copyWith(status: ChatStatusValue.pending);

      expect(updated.status, ChatStatusValue.pending);
      expect(updated.kind, ChatMessageKind.file);
      expect(updated.fileName, 'movie.mkv');
      expect(updated.fileSize, 7);
    });

    test('can set the local path once a receive completes', () {
      final original = ChatMessage(
        id: 'fr-1',
        peerId: 'pb-bob',
        direction: 'in',
        body: '',
        at: DateTime.parse('2026-08-12T09:00:00Z'),
        status: ChatStatusValue.received,
        kind: ChatMessageKind.file,
        fileName: 'a.bin',
        fileSize: 7,
      );

      expect(
        original.copyWith(localPath: '/tmp/a.bin').localPath,
        '/tmp/a.bin',
      );
    });
  });
}
