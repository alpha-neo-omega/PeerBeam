// Unit tests for the chat SDK layer: ChatMessage decoding and the
// `chat_received` bridge event.
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
}
