//! Runtime status helpers (memory + build info). The full status object is
//! assembled in [`crate::runtime::status`] where the engine + manager handles
//! live.

use serde_json::{json, Value};

/// Resident set size in bytes (Linux, via `/proc/self/statm`); `None` elsewhere.
pub fn rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // /proc/self/statm reports resident_pages in kernel-page-size units,
        // which is 16 KiB on some ARM64/Android kernels, not always 4 KiB.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0 {
            return None;
        }
        let page_size = page_size as u64;
        Some(resident_pages * page_size)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Version + build metadata.
pub fn build_info() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "abi": crate::ABI_VERSION,
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// `rss_bytes()` must scale by the *actual* runtime page size (queried
    /// via `sysconf`), not a hardcoded 4 KiB — otherwise it under-reports by
    /// 4x on 16 KiB-page kernels. We can't force a 16 KiB-page kernel in CI,
    /// but we can assert the reported value is consistent with whatever the
    /// real page size on this machine is, which the hardcoded-4096 version
    /// would fail were this ever run on a non-4 KiB-page host.
    #[test]
    fn rss_bytes_is_a_whole_multiple_of_the_real_page_size() {
        let bytes = rss_bytes().expect("rss_bytes should succeed on linux");
        assert!(bytes > 0, "a running process must have nonzero RSS");

        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page_size > 0);
        assert_eq!(
            bytes % page_size as u64,
            0,
            "RSS ({bytes}) should be a whole number of {page_size}-byte pages"
        );
    }
}
