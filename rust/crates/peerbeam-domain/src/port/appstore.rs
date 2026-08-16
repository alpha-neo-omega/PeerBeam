//! AppStore port: encrypted, local-first, per-namespace keyed record storage for
//! capability data (chat log, clipboard history, notes). Values are opaque bytes;
//! the caller serializes its own record type. Implemented by an infra adapter
//! (e.g. an encrypted filesystem store).

use crate::error::Result;

/// A namespaced keyed-record store. Each `namespace` is an independent set of
/// `key -> value` records; `key` is caller-chosen (a time-ordered id for an
/// append log, or an item id for key/value data).
pub trait AppStore: Send + Sync {
    /// Store `value` under (`namespace`, `key`), replacing any existing value.
    fn put(&self, namespace: &str, key: &str, value: &[u8]) -> Result<()>;

    /// Fetch the value for (`namespace`, `key`), or `Ok(None)` if absent. A
    /// present-but-unreadable record is an `Err`, never a silent `None`.
    fn get(&self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>>;

    /// All (`key`, `value`) pairs in `namespace`, ordered by key ascending.
    /// `Ok(vec![])` if the namespace has no records.
    fn list(&self, namespace: &str) -> Result<Vec<(String, Vec<u8>)>>;

    /// Every namespace holding at least one record whose name starts with
    /// `prefix`, sorted ascending. `Ok(vec![])` when none match.
    ///
    /// A namespace that exists but holds no records is not returned: callers
    /// use this to enumerate real conversations, and an empty directory left
    /// by a `clear` is not one.
    fn namespaces(&self, prefix: &str) -> Result<Vec<String>>;

    /// Remove (`namespace`, `key`); returns whether it existed.
    fn delete(&self, namespace: &str, key: &str) -> Result<bool>;

    /// Remove every record in `namespace` (no-op if it has none).
    fn clear(&self, namespace: &str) -> Result<()>;
}
