//! The result envelope crossing the FFI boundary.
//!
//! Every `char*`-returning FFI function yields a JSON object: either
//! `{"ok":true,"data":…}` or `{"ok":false,"error":{"code","message"}}`. The
//! Dart wrapper decodes this and maps `code` to a typed exception — raw Rust
//! error/panic text never reaches user code except as a sanitized `message`.

use serde_json::{json, Value};

use peerbeam_domain::error::DomainError;
use peerbeam_engine::EngineError;

/// Stable, strongly-typed error codes (mirror as a Dart enum). Adding variants
/// is backward-compatible; renaming is not. Some are only produced by
/// operations landing in later milestones (transfer/clipboard/etc.).
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum Code {
    /// The engine has not been initialised (`pb_init` not called / failed).
    NotInitialised,
    /// A required argument was missing or malformed JSON.
    InvalidArgument,
    /// A network connection could not be established or was lost.
    Connection,
    /// Integrity check failed (checksum mismatch).
    Integrity,
    /// The operation was cancelled.
    Cancelled,
    /// Filesystem / storage error.
    Storage,
    /// A transfer-protocol error.
    Transfer,
    /// Cryptography / authentication error.
    Encryption,
    /// The feature is not implemented yet.
    Unimplemented,
    /// The feature cannot work on this platform, and no future build of it
    /// will. Distinct from [`Code::Unimplemented`] on purpose: "not yet" tells
    /// a user to wait, while this tells them to stop looking — as with
    /// auto-save rules on Android, where an app cannot write to an arbitrary
    /// absolute path at all (I12: the limit is documented, not papered over).
    Unsupported,
    /// A destructive chat operation refused because the shared send queue
    /// holds an entry this build cannot decode, and guessing which records it
    /// backs risks discarding something still waiting to be delivered (see
    /// `ChatStore::delete_conversation`). Unlike every other code here,
    /// retrying the exact same call will not clear this on its own — the
    /// offending entry is not necessarily even owned by the conversation the
    /// caller asked about, since the queue is shared across every peer.
    QueueUnreadable,
    /// A Rust panic was caught at the boundary (never propagated as UB).
    Internal,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::NotInitialised => "not_initialised",
            Code::InvalidArgument => "invalid_argument",
            Code::Connection => "connection",
            Code::Integrity => "integrity",
            Code::Cancelled => "cancelled",
            Code::Storage => "storage",
            Code::Transfer => "transfer",
            Code::Encryption => "encryption",
            Code::Unimplemented => "unimplemented",
            Code::Unsupported => "unsupported",
            Code::QueueUnreadable => "queue_unreadable",
            Code::Internal => "internal",
        }
    }
}

/// Map a domain error to a stable FFI code (never leaks internal structure).
/// Used by the transfer/clipboard operations landing in the next milestone.
#[allow(dead_code)]
pub fn code_of(err: &DomainError) -> Code {
    match err {
        DomainError::Connection(_) => Code::Connection,
        DomainError::Integrity(_) => Code::Integrity,
        DomainError::Cancelled => Code::Cancelled,
        DomainError::Storage(_) => Code::Storage,
        DomainError::Transfer(_) => Code::Transfer,
        DomainError::Encryption(_) => Code::Encryption,
        _ => Code::Internal,
    }
}

/// A successful envelope carrying `data`.
pub fn ok(data: Value) -> Value {
    json!({ "ok": true, "data": data })
}

/// An error envelope.
pub fn err(code: Code, message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": { "code": code.as_str(), "message": message.into() } })
}

/// Convert a `Result<Value, (Code, String)>` into an envelope value.
pub fn envelope(result: Result<Value, (Code, String)>) -> Value {
    match result {
        Ok(data) => ok(data),
        Err((code, message)) => err(code, message),
    }
}

/// Map a domain error to an FFI `(code, message)` pair. Used by the transfer
/// operations landing in the next milestone.
#[allow(dead_code)]
pub fn from_domain(e: DomainError) -> (Code, String) {
    (code_of(&e), e.to_string())
}

/// Map an engine-build error to an FFI `(code, message)` pair.
pub fn from_engine(e: EngineError) -> (Code, String) {
    (Code::Internal, e.to_string())
}
