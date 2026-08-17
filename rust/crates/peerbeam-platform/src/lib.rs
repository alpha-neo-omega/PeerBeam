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

/// This device's battery reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    /// Charge level, 0-100.
    pub percent: u8,
    /// Whether it is charging right now; `None` when the state is unreadable or
    /// the kernel reports `Unknown`. Deliberately separate from `percent`:
    /// knowing the level says nothing about the direction it is moving.
    pub charging: Option<bool>,
}

/// This device's battery, or `None` when it has none or cannot read one.
///
/// Lives here for the same reason [`available_bytes`] does: this crate is the
/// one place that touches host specifics.
///
/// **Coverage is deliberately partial.** Linux reads `/sys/class/power_supply`,
/// which is already present and costs one small file read. Every other platform
/// returns `None` — Windows and macOS would each need a new dependency or a
/// hand-rolled `unsafe` FFI binding for a number that is a nicety, and Android
/// is served from above by the Flutter layer's `BatteryManager` access. `None`
/// is a first-class answer here, not a gap: a desktop with no battery is the
/// case the presence schema was built around.
#[must_use]
pub fn battery() -> Option<Battery> {
    #[cfg(target_os = "linux")]
    {
        linux_battery()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Read the first system battery under `/sys/class/power_supply`.
///
/// Entries are filtered to `type == "Battery"`, which excludes the AC adapter
/// (`Mains`) that sits beside it and has no capacity at all. Among batteries,
/// a `BAT*`-named one wins: that is the kernel's convention for the machine's
/// own pack, and it keeps a connected game controller or wireless mouse — which
/// also registers as a `Battery` — from being reported as this laptop's charge.
#[cfg(target_os = "linux")]
fn linux_battery() -> Option<Battery> {
    let read = |dir: &std::path::Path, file: &str| -> Option<String> {
        std::fs::read_to_string(dir.join(file))
            .ok()
            .map(|s| s.trim().to_string())
    };

    let mut fallback: Option<Battery> = None;
    for entry in std::fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
        let path = entry.path();
        if read(&path, "type").as_deref() != Some("Battery") {
            continue;
        }
        let Some(percent) = read(&path, "capacity").and_then(|c| c.parse::<u8>().ok()) else {
            continue;
        };
        if percent > 100 {
            // A kernel that reports an impossible level has not measured
            // anything; skip it rather than pass it on to be rejected on the
            // wire (peerbeam_presence::Status::to_frame would refuse it).
            continue;
        }
        let charging = match read(&path, "status").as_deref() {
            Some("Charging") => Some(true),
            // "Full" is not charging: the pack is done taking current.
            Some("Discharging" | "Full" | "Not charging") => Some(false),
            _ => None,
        };
        let battery = Battery { percent, charging };
        let is_system_pack = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("BAT"));
        if is_system_pack {
            return Some(battery);
        }
        fallback.get_or_insert(battery);
    }
    fallback
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

    /// Whatever this host is, the answer must be *expressible*: either no
    /// battery (a desktop, a CI box, a non-Linux target) or a reading inside
    /// the range the presence wire type accepts. A collector that could emit
    /// 137% would have its own status refused by `Status::to_frame`.
    #[test]
    fn battery_is_either_absent_or_a_reading_in_range() {
        match battery() {
            None => {}
            Some(b) => assert!(
                b.percent <= 100,
                "a battery reading outside 0-100 is not a measurement: {b:?}"
            ),
        }
    }

    /// Calling it must never panic or block, on any host — it runs on a
    /// 60-second timer inside a live session.
    #[test]
    fn battery_is_cheap_and_repeatable() {
        let first = battery();
        let second = battery();
        // Not asserting equality: a real battery may tick between calls. What
        // matters is that both calls answer at all, consistently about whether
        // this machine HAS one.
        assert_eq!(first.is_some(), second.is_some());
    }
}
