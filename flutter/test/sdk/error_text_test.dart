// Phase 7: user-facing errors are friendly and never leak internal detail.
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/sdk/error_text.dart';
import 'package:peerbeam/sdk/exceptions.dart';

void main() {
  const codes = [
    'not_initialised',
    'invalid_argument',
    'connection',
    'integrity',
    'cancelled',
    'storage',
    'transfer',
    'encryption',
    'unimplemented',
    'queue_unreadable',
    'internal',
  ];

  test('every code maps to friendly, non-technical text', () {
    for (final c in codes) {
      final msg = friendlyErrorForCode(c);
      expect(msg.trim(), isNotEmpty, reason: c);
      // No internal/implementation detail leaks to the user.
      for (final leak in ['quic', 'ffi', 'exception', 'panic', 'rust', 'dlopen']) {
        expect(msg.toLowerCase(), isNot(contains(leak)), reason: '$c leaked "$leak"');
      }
    }
  });

  test('a raw engine message is not shown verbatim', () {
    final msg = friendlyError(
      const ConnectionException('quic: connection lost: transport error'),
    );
    expect(msg.toLowerCase(), isNot(contains('quic')));
    expect(msg.toLowerCase(), contains('network'));
  });

  test('unknown errors get a safe generic message', () {
    expect(friendlyError(StateError('boom')), 'Something went wrong. Please try again.');
  });

  test('queue_unreadable maps to QueueUnreadableException with its own sentence', () {
    final ex = PeerBeamException.fromCode(
      'queue_unreadable',
      'outbox entry x is unreadable',
    );
    expect(ex, isA<QueueUnreadableException>());

    final msg = friendlyError(ex);
    expect(
      msg,
      "Something still queued to send can't be read right now, so deleting "
      "is on hold — that could discard it before it goes out.",
    );
    // The whole point of a distinct code: it must never tell the user to
    // retry, since retrying cannot clear this on its own.
    expect(msg.toLowerCase(), isNot(contains('try again')));
  });
}
