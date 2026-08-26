//! Sources of Tailscale status JSON.
//!
//! The provider is agnostic to *how* status is obtained; it just needs the
//! `tailscale status --json` document. Two production sources plus an
//! injectable trait (used by tests to feed canned JSON):
//!
//! - [`CliStatusSource`] — runs the `tailscale` binary. Most portable.
//! - [`LocalApiStatusSource`] — talks to `tailscaled`'s LocalAPI over its
//!   Unix socket (no subprocess). Unix only.

use async_trait::async_trait;

use peerbeam_domain::error::{DomainError, Result};

/// Default path to the `tailscaled` LocalAPI Unix socket.
#[cfg(unix)]
pub const DEFAULT_SOCKET_PATH: &str = "/var/run/tailscale/tailscaled.sock";

/// Something that can produce a `tailscale status --json` document.
#[async_trait]
pub trait StatusSource: Send + Sync {
    /// Fetch the current status JSON.
    async fn fetch(&self) -> Result<String>;
}

/// Obtains status by running `tailscale status --json`.
pub struct CliStatusSource {
    binary: String,
}

impl CliStatusSource {
    /// Use the `tailscale` binary found on `PATH`.
    pub fn new() -> Self {
        Self {
            binary: "tailscale".to_string(),
        }
    }

    /// Use a specific `tailscale` binary path.
    pub fn with_binary(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}

impl Default for CliStatusSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StatusSource for CliStatusSource {
    async fn fetch(&self) -> Result<String> {
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg("status").arg("--json");
        // In a Windows GUI process every child spawn pops a console window —
        // and this runs on every scan tick. CREATE_NO_WINDOW suppresses it.
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000 /* CREATE_NO_WINDOW */);
        let output = cmd
            .output()
            .await
            .map_err(|e| DomainError::Discovery(format!("tailscale cli: {e}")))?;

        if !output.status.success() {
            return Err(DomainError::Discovery(format!(
                "tailscale status exited with {}",
                output.status
            )));
        }
        String::from_utf8(output.stdout)
            .map_err(|e| DomainError::Discovery(format!("tailscale cli output: {e}")))
    }
}

/// Obtains status from `tailscaled`'s LocalAPI over its Unix socket.
#[cfg(unix)]
pub struct LocalApiStatusSource {
    socket_path: String,
}

#[cfg(unix)]
impl LocalApiStatusSource {
    /// Use the default LocalAPI socket path.
    pub fn new() -> Self {
        Self {
            socket_path: DEFAULT_SOCKET_PATH.to_string(),
        }
    }

    /// Use a specific socket path.
    pub fn with_socket(path: impl Into<String>) -> Self {
        Self {
            socket_path: path.into(),
        }
    }
}

#[cfg(unix)]
impl Default for LocalApiStatusSource {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
#[async_trait]
impl StatusSource for LocalApiStatusSource {
    async fn fetch(&self) -> Result<String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| DomainError::Discovery(format!("tailscale localapi connect: {e}")))?;

        // Minimal HTTP/1.0 request; the Host header is required by tailscaled.
        let request = "GET /localapi/v0/status HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n";
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| DomainError::Discovery(format!("tailscale localapi write: {e}")))?;

        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .await
            .map_err(|e| DomainError::Discovery(format!("tailscale localapi read: {e}")))?;

        let text = String::from_utf8_lossy(&raw);
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_headers, body)| body.to_string())
            .ok_or_else(|| DomainError::Discovery("tailscale localapi: no body".to_string()))?;
        Ok(body)
    }
}

/// Where a `tailscale` binary lives on macOS, in the order worth trying.
///
/// **A GUI app does not inherit a login shell's `PATH`.** Launched from Finder
/// it gets `/usr/bin:/bin:/usr/sbin:/sbin`, which contains neither Homebrew
/// prefix nor `/usr/local/bin` — so `Command::new("tailscale")` fails with
/// "no such file" no matter how the user installed it. The Mac App Store build
/// does not create `/var/run/tailscale/tailscaled.sock` either, so the socket
/// branch above misses as well and the app was left with no working source at
/// all while the CLI in a terminal worked fine.
#[cfg(target_os = "macos")]
const MACOS_BINARIES: &[&str] = &[
    // The app bundle ships its own CLI; present for both the App Store and
    // standalone builds, and needs no separate install.
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    "/opt/homebrew/bin/tailscale",
    "/usr/local/bin/tailscale",
];

/// Pick the best available source for this platform: the LocalAPI socket if
/// present (no subprocess), otherwise the CLI.
pub fn default_source() -> Box<dyn StatusSource> {
    #[cfg(unix)]
    {
        if std::path::Path::new(DEFAULT_SOCKET_PATH).exists() {
            return Box::new(LocalApiStatusSource::new());
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(found) = MACOS_BINARIES
            .iter()
            .find(|p| std::path::Path::new(p).exists())
        {
            return Box::new(CliStatusSource::with_binary(*found));
        }
    }
    Box::new(CliStatusSource::new())
}

#[cfg(all(test, target_os = "macos"))]
mod macos_source_tests {
    use super::*;

    /// Every candidate is absolute: a relative entry would be resolved against
    /// the process's working directory, which for an app launched from Finder is
    /// `/` — and would then depend on `PATH` again, which is the thing this list
    /// exists to stop depending on.
    #[test]
    fn every_macos_candidate_is_an_absolute_path() {
        for p in MACOS_BINARIES {
            assert!(
                std::path::Path::new(p).is_absolute(),
                "{p} is not absolute, so it would be looked up relative to the \
                 launching directory"
            );
        }
    }
}
