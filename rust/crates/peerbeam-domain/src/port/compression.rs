//! Compression port.

use crate::error::Result;

/// Compresses and decompresses transfer payloads, skipping formats that
/// are already compressed.
pub trait CompressionProvider: Send + Sync {
    /// Whether a payload of the given MIME type is worth compressing.
    fn should_compress(&self, mime_type: &str) -> bool;

    /// Compress a buffer.
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>>;

    /// Decompress a buffer.
    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>>;
}
