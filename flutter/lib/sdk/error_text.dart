// Maps engine errors to clear, friendly, actionable user text. Widgets show
// these — never raw exception/FFI/engine strings.
import 'exceptions.dart';

/// Friendly message for any thrown error (falls back for non-PeerBeam errors).
String friendlyError(Object error) {
  if (error is PeerBeamException) return _forException(error);
  return 'Something went wrong. Please try again.';
}

/// Friendly message from a stable engine error code (used by repositories that
/// receive `{code,message}` in events rather than a thrown exception).
String friendlyErrorForCode(String code) =>
    _forException(PeerBeamException.fromCode(code, ''));

String _forException(PeerBeamException e) => switch (e) {
  ConnectionException() =>
    "Couldn't reach the device. Make sure both are on the same network, then try again.",
  IntegrityException() =>
    "The file didn't arrive intact. Please send it again.",
  CancelledException() => 'Transfer cancelled.',
  StorageException() =>
    "Couldn't read or save the file. Check storage space and permissions.",
  TransferException() => "The transfer couldn't finish. Please try again.",
  EncryptionException() =>
    'Secure connection failed. Make sure both devices are up to date.',
  NotInitialisedException() =>
    "The engine isn't ready yet — give it a moment and retry.",
  PeerBeamUnavailable() => 'The transfer engine is unavailable in this build.',
  InvalidArgumentException() => "That action can't be completed.",
  UnimplementedException() => "That feature isn't available yet.",
  InternalException() => 'Something went wrong. Please try again.',
};
