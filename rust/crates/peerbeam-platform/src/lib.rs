//! Platform layer.
//!
//! The one place that touches host specifics: which OS we run on, the
//! device's default name, and where to put config/data/downloads. Every
//! other crate asks this layer instead of calling `cfg!`/`dirs`/`hostname`
//! directly, so platform branching lives in exactly one module.

use std::path::PathBuf;

use peerbeam_domain::entity::Platform;

/// The application directory name used under the OS config/data roots.
const APP_DIR: &str = "peerbeam";

/// Detect the platform this build is running on.
pub fn current() -> Platform {
    if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::MacOS
    } else if cfg!(target_os = "android") {
        Platform::Android
    } else if cfg!(target_os = "ios") {
        Platform::IOS
    } else if cfg!(target_os = "linux") {
        Platform::Linux
    } else {
        // Fallback for unknown/wasm targets; frontends may override.
        Platform::Web
    }
}

/// The host name, or a stable fallback when it cannot be read.
pub fn hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "PeerBeam Device".to_string())
}

/// Directory for received files (defaults to the OS Downloads folder).
pub fn download_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(temp_fallback)
}

/// Directory for PeerBeam configuration files.
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .map(|d| d.join(APP_DIR))
        .unwrap_or_else(|| temp_fallback().join(APP_DIR))
}

/// Directory for PeerBeam application data (checkpoints, trust store, …).
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join(APP_DIR))
        .unwrap_or_else(|| temp_fallback().join(APP_DIR))
}

/// Bytes currently available on the filesystem holding `path`, or `None` when
/// that cannot be determined.
///
/// Lives here because this crate is already the one place that touches host
/// specifics (`hostname`, `config_dir`, `data_dir`, `download_dir`). `fs4`
/// covers Windows, macOS, Linux and Android behind one safe call — the
/// alternatives were a Unix-only `statvfs` or a raw Windows FFI binding
/// needing `unsafe`, and I12 makes every platform first-class.
///
/// `None` means "could not measure", never "no space". Callers must treat it
/// as permission to proceed: refusing an operation because a platform would
/// not answer is worse than the risk the measurement guards against.
#[must_use]
pub fn available_bytes(path: &str) -> Option<u64> {
    // Walk up to the nearest existing ancestor: the staging directory may not
    // be created yet the first time this is asked.
    let mut p = std::path::Path::new(path);
    loop {
        if p.exists() {
            return fs4::available_space(p).ok();
        }
        p = p.parent()?;
    }
}

/// Last-resort writable location when the OS provides no standard dir.
fn temp_fallback() -> PathBuf {
    std::env::temp_dir().join(APP_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_bytes_reports_a_real_figure_for_an_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let free = available_bytes(&dir.path().to_string_lossy())
            .expect("a real filesystem must report its free space");
        assert!(free > 0, "a writable tempdir cannot have zero bytes free");
    }

    /// The staging directory does not exist yet the first time a send asks
    /// whether there is room for it, so the answer must come from the nearest
    /// existing ancestor rather than being `None` (which callers read as
    /// "could not measure" and proceed on).
    #[test]
    fn available_bytes_walks_up_to_the_nearest_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-created-yet/outbox-blobs");
        assert!(!missing.exists());
        let free = available_bytes(&missing.to_string_lossy())
            .expect("a path under an existing root must still measure");
        assert!(free > 0);
    }

    /// A path with no existing ancestor at all cannot be measured. `None` is
    /// the honest answer — callers must proceed rather than refuse a send
    /// because a platform would not answer.
    #[test]
    fn available_bytes_is_none_when_nothing_in_the_path_exists() {
        assert_eq!(available_bytes(""), None);
    }
}
