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
  });
}
