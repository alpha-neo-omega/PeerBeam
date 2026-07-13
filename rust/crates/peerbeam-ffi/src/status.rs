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
        let page_size = 4096u64; // conventional; avoids a libc dep
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
