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

/// Last-resort writable location when the OS provides no standard dir.
fn temp_fallback() -> PathBuf {
    std::env::temp_dir().join(APP_DIR)
}
