/// Typed exceptions mapped from the Rust error envelope. Widgets and
/// repositories catch these; raw FFI/JSON details never leak past the SDK.
sealed class PeerBeamException implements Exception {
  final String message;
  const PeerBeamException(this.message);

  /// Build the right subtype from a stable error code + message.
  factory PeerBeamException.fromCode(String code, String message) {
    return switch (code) {
      'not_initialised' => NotInitialisedException(message),
      'invalid_argument' => InvalidArgumentException(message),
      'connection' => ConnectionException(message),
      'integrity' => IntegrityException(message),
      'cancelled' => CancelledException(message),
      'storage' => StorageException(message),
      'transfer' => TransferException(message),
      'encryption' => EncryptionException(message),
      'unimplemented' => UnimplementedException(message),
      _ => InternalException(message),
    };
  }

  @override
  String toString() => '$runtimeType: $message';
}

class NotInitialisedException extends PeerBeamException {
  const NotInitialisedException(super.message);
}

class InvalidArgumentException extends PeerBeamException {
  const InvalidArgumentException(super.message);
}

class ConnectionException extends PeerBeamException {
  const ConnectionException(super.message);
}

class IntegrityException extends PeerBeamException {
  const IntegrityException(super.message);
}

class CancelledException extends PeerBeamException {
  const CancelledException(super.message);
}

class StorageException extends PeerBeamException {
  const StorageException(super.message);
}

class TransferException extends PeerBeamException {
  const TransferException(super.message);
}

class EncryptionException extends PeerBeamException {
  const EncryptionException(super.message);
}

class UnimplementedException extends PeerBeamException {
  const UnimplementedException(super.message);
}

class InternalException extends PeerBeamException {
  const InternalException(super.message);
}

/// The native library could not be loaded (not bundled / not built). The SDK
/// stays constructible so the app degrades gracefully instead of crashing.
class PeerBeamUnavailable extends PeerBeamException {
  const PeerBeamUnavailable(super.message);
}
