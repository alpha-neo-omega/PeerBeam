//! Transport selection for file transfer (M7 cutover).
//!
//! From M7, PeerSession is the **default** transport for a transfer; the legacy
//! direct-on-`Link` path (still shipped) is an automatic fallback. This module is
//! purely a *selector*: it decides which transport to run the transfer over and
//! then calls the **existing** transfer engine unchanged — [`send_file`] /
//! [`receive_file`] / [`send_folder`] / [`receive_folder`], via the PeerSession
//! channel helpers ([`send_file_on_session`](crate::send_file_on_session) …) or a
//! [`SecureLink`]. There is **one** transfer pipeline; only the transport in front
//! of it changes.
//!
//! ## Selection
//!
//! ```text
//! attempt PeerSession ──negotiated?──▶ run transfer over the session channel
//!         │ no (older peer / version / capability / explicit compat)
//!         ▼
//!     legacy transfer over SecureLink
//! ```
//!
//! The two transports are supplied by the caller as injected *path openers*
//! ([`SessionSendPath`] / [`SessionReceivePath`] / [`LegacyPath`]), so this crate
//! stays transport-agnostic (I1) and both the in-memory tests and the real QUIC
//! wiring reuse the same selector. The engine wires the concrete openers
//! (RouteManager dial + PeerSession vs. `SecureLink`); this milestone does not
//! touch FFI/CLI.
//!
//! ## Fallback discipline
//!
//! Fallback happens **only at establishment**, before any bytes flow — an older
//! peer, a version or capability mismatch, or an explicit compatibility mode. Once
//! a session is established and the transfer is running, a failure is a real error
//! (handled by the recovery driver, M6), never a silent switch to legacy: that
//! would risk transfer integrity. Both paths run the identical, integrity-checked
//! engine, so the bytes on disk are byte-for-byte identical regardless of choice.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use peerbeam_domain::entity::Progress;
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{EncryptionProvider, Link, StorageProvider};
use peerbeam_domain::session::ChannelType;

use crate::auth::Session;
use crate::control::TransferControl;
use crate::folder::{receive_folder, send_folder, FolderReceived, FolderSendRequest};
use crate::secure::SecureLink;
use crate::session::{
    receive_file_on_channel, receive_folder_on_channel, send_file_on_session,
    send_folder_on_session, IncomingStreamChannel, PeerSession, SessionHandle,
};
use crate::stream::{receive_file, send_file, Received, SendRequest, TransferOutcome};

/// Which transport a transfer used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferPath {
    /// Ran over a PeerSession transfer channel (the default).
    Session,
    /// Ran over the legacy direct-on-`Link` path (fallback).
    Legacy,
}

/// Caller-chosen compatibility mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompatMode {
    /// Prefer PeerSession, fall back to legacy automatically (the default).
    #[default]
    Auto,
    /// Require PeerSession; error rather than fall back (for tests / diagnostics).
    ForceSession,
    /// Always use legacy (explicit compatibility mode).
    ForceLegacy,
}

/// Why the PeerSession path was not used and the transfer fell back to legacy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    /// The peer does not speak PeerSession (its control handshake did not form).
    OlderPeer,
    /// No common protocol major version.
    VersionMismatch,
    /// The peer did not negotiate the Transfer capability.
    CapabilityMismatch,
    /// Session establishment failed for another negotiation reason.
    NegotiationFailed,
    /// A resume/reconnect incompatibility was detected.
    ResumeIncompatible,
    /// The caller requested [`CompatMode::ForceLegacy`].
    ExplicitCompat,
}

impl FallbackReason {
    /// Stable snake_case label (for logs / metrics).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            FallbackReason::OlderPeer => "older_peer",
            FallbackReason::VersionMismatch => "version_mismatch",
            FallbackReason::CapabilityMismatch => "capability_mismatch",
            FallbackReason::NegotiationFailed => "negotiation_failed",
            FallbackReason::ResumeIncompatible => "resume_incompatible",
            FallbackReason::ExplicitCompat => "explicit_compat",
        }
    }
}

/// The result of trying to open the PeerSession path: ready to use, or a reason to
/// fall back to legacy.
pub enum SessionOpen<T> {
    /// A compatible session is ready; run the transfer over it.
    Ready(T),
    /// The session is unavailable/incompatible; fall back to legacy.
    Fallback(FallbackReason),
}

/// Opens the PeerSession **send** path: dial + establish a session and return its
/// handle if it negotiated the Transfer capability, else a fallback reason.
#[async_trait]
pub trait SessionSendPath: Send {
    /// Attempt to establish a sending session. `Err` is a hard failure (legacy
    /// would fail too); use [`SessionOpen::Fallback`] to request the legacy path.
    async fn open(&mut self) -> Result<SessionOpen<SessionHandle>>;
}

/// Opens the PeerSession **receive** path: accept a session and its incoming
/// transfer channel, else a fallback reason.
#[async_trait]
pub trait SessionReceivePath: Send {
    /// Attempt to accept a receiving session + its incoming transfer channel.
    async fn open(&mut self) -> Result<SessionOpen<(SessionHandle, IncomingStreamChannel)>>;
}

/// Opens the legacy path: a raw [`Link`] and the authenticated [`Session`] to seal
/// it with a [`SecureLink`].
#[async_trait]
pub trait LegacyPath: Send {
    /// Establish the legacy transport.
    async fn open(&mut self) -> Result<(Box<dyn Link>, Session)>;
}

/// Whether an established [`PeerSession`] can carry a transfer: it must have
/// negotiated the Transfer capability. (Version compatibility is already enforced
/// by [`PeerSession::open`], which errors with `VersionIncompatible` otherwise.)
///
/// Returns `Ok(())` when usable, or the [`FallbackReason`] to record.
pub fn transfer_capability(session: &PeerSession) -> std::result::Result<(), FallbackReason> {
    if session.capabilities().supports(ChannelType::TRANSFER) {
        Ok(())
    } else {
        Err(FallbackReason::CapabilityMismatch)
    }
}

/// Local, in-process migration metrics (no telemetry leaves the device — I4).
/// Cloneable counters shared across concurrent transfers.
#[derive(Debug, Default)]
pub struct MigrationMetrics {
    session_transfers: AtomicU64,
    legacy_transfers: AtomicU64,
    fallbacks: AtomicU64,
    fb_older_peer: AtomicU64,
    fb_version: AtomicU64,
    fb_capability: AtomicU64,
    fb_negotiation: AtomicU64,
    fb_resume: AtomicU64,
    fb_explicit: AtomicU64,
}

/// An immutable snapshot of [`MigrationMetrics`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MigrationSnapshot {
    /// Transfers that ran over PeerSession.
    pub session_transfers: u64,
    /// Transfers that ran over the legacy path.
    pub legacy_transfers: u64,
    /// Total fallbacks to legacy.
    pub fallbacks: u64,
    /// Fallbacks by reason: older peer.
    pub older_peer: u64,
    /// Fallbacks by reason: version mismatch.
    pub version_mismatch: u64,
    /// Fallbacks by reason: capability mismatch.
    pub capability_mismatch: u64,
    /// Fallbacks by reason: negotiation failed.
    pub negotiation_failed: u64,
    /// Fallbacks by reason: resume incompatible.
    pub resume_incompatible: u64,
    /// Fallbacks by reason: explicit compat mode.
    pub explicit_compat: u64,
}

impl MigrationMetrics {
    /// A fresh, zeroed metrics collector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn record_session(&self) {
        self.session_transfers.fetch_add(1, Ordering::Relaxed);
    }

    fn record_legacy(&self) {
        self.legacy_transfers.fetch_add(1, Ordering::Relaxed);
    }

    fn record_fallback(&self, reason: FallbackReason) {
        self.fallbacks.fetch_add(1, Ordering::Relaxed);
        let counter = match reason {
            FallbackReason::OlderPeer => &self.fb_older_peer,
            FallbackReason::VersionMismatch => &self.fb_version,
            FallbackReason::CapabilityMismatch => &self.fb_capability,
            FallbackReason::NegotiationFailed => &self.fb_negotiation,
            FallbackReason::ResumeIncompatible => &self.fb_resume,
            FallbackReason::ExplicitCompat => &self.fb_explicit,
        };
        counter.fetch_add(1, Ordering::Relaxed);
        // Local telemetry only — a structured log line, never a network beacon.
        tracing::info!(
            reason = reason.label(),
            "transfer fell back to legacy transport"
        );
    }

    /// A consistent snapshot of the counters.
    #[must_use]
    pub fn snapshot(&self) -> MigrationSnapshot {
        MigrationSnapshot {
            session_transfers: self.session_transfers.load(Ordering::Relaxed),
            legacy_transfers: self.legacy_transfers.load(Ordering::Relaxed),
            fallbacks: self.fallbacks.load(Ordering::Relaxed),
            older_peer: self.fb_older_peer.load(Ordering::Relaxed),
            version_mismatch: self.fb_version.load(Ordering::Relaxed),
            capability_mismatch: self.fb_capability.load(Ordering::Relaxed),
            negotiation_failed: self.fb_negotiation.load(Ordering::Relaxed),
            resume_incompatible: self.fb_resume.load(Ordering::Relaxed),
            explicit_compat: self.fb_explicit.load(Ordering::Relaxed),
        }
    }
}

/// Decide whether to try the session path and, if a session open was attempted,
/// turn its result into `Some(ready)` (use session) or `None` (use legacy),
/// recording metrics. `Err` short-circuits (fatal, or `ForceSession` refused).
async fn resolve<T>(
    mode: CompatMode,
    open: impl std::future::Future<Output = Result<SessionOpen<T>>>,
    metrics: &MigrationMetrics,
) -> Result<Option<T>> {
    if mode == CompatMode::ForceLegacy {
        metrics.record_fallback(FallbackReason::ExplicitCompat);
        return Ok(None);
    }
    match open.await {
        Ok(SessionOpen::Ready(ready)) => Ok(Some(ready)),
        Ok(SessionOpen::Fallback(reason)) => {
            if mode == CompatMode::ForceSession {
                return Err(DomainError::Connection(format!(
                    "PeerSession required but unavailable: {}",
                    reason.label()
                )));
            }
            metrics.record_fallback(reason);
            Ok(None)
        }
        Err(e) => {
            if mode == CompatMode::ForceSession {
                return Err(e);
            }
            // A hard session-establishment error against a peer that may simply be
            // older: record it and let legacy try.
            metrics.record_fallback(FallbackReason::NegotiationFailed);
            tracing::warn!(error = %e, "session establishment failed; trying legacy");
            Ok(None)
        }
    }
}

/// Send a file over the selected transport (PeerSession by default, legacy
/// fallback). Reuses [`send_file_on_session`] / [`send_file`] — no duplicated
/// pipeline. Returns which path was used and the transfer outcome.
#[allow(clippy::too_many_arguments)]
pub async fn send_file_selected(
    mode: CompatMode,
    session: &mut dyn SessionSendPath,
    legacy: &mut dyn LegacyPath,
    enc: &dyn EncryptionProvider,
    storage: &dyn StorageProvider,
    req: SendRequest,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    retries: u32,
    metrics: &MigrationMetrics,
) -> Result<(TransferPath, TransferOutcome)> {
    if let Some(handle) = resolve(mode, session.open(), metrics).await? {
        tracing::info!(path = "session", "transfer transport selected");
        let outcome = send_file_on_session(&handle, storage, req, ctrl, progress, retries).await?;
        metrics.record_session();
        return Ok((TransferPath::Session, outcome));
    }
    tracing::info!(path = "legacy", "transfer transport selected");
    let (mut link, sess) = legacy.open().await?;
    let mut secure = SecureLink::new(link.as_mut(), enc, sess);
    let outcome = send_file(&mut secure, storage, req, ctrl, progress, retries).await?;
    metrics.record_legacy();
    Ok((TransferPath::Legacy, outcome))
}

/// Receive a file over the selected transport (PeerSession by default, legacy
/// fallback). Reuses [`receive_file_on_channel`] / [`receive_file`].
#[allow(clippy::too_many_arguments)]
pub async fn receive_file_selected(
    mode: CompatMode,
    session: &mut dyn SessionReceivePath,
    legacy: &mut dyn LegacyPath,
    enc: &dyn EncryptionProvider,
    storage: &dyn StorageProvider,
    dest_dir: &str,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    metrics: &MigrationMetrics,
) -> Result<(TransferPath, Received)> {
    if let Some((handle, incoming)) = resolve(mode, session.open(), metrics).await? {
        let received =
            receive_file_on_channel(incoming, &handle, storage, dest_dir, ctrl, progress).await?;
        metrics.record_session();
        return Ok((TransferPath::Session, received));
    }
    let (mut link, sess) = legacy.open().await?;
    let mut secure = SecureLink::new(link.as_mut(), enc, sess);
    let received = receive_file(&mut secure, storage, dest_dir, ctrl, progress).await?;
    metrics.record_legacy();
    Ok((TransferPath::Legacy, received))
}

/// Send a folder over the selected transport. Reuses [`send_folder_on_session`] /
/// [`send_folder`].
#[allow(clippy::too_many_arguments)]
pub async fn send_folder_selected(
    mode: CompatMode,
    session: &mut dyn SessionSendPath,
    legacy: &mut dyn LegacyPath,
    enc: &dyn EncryptionProvider,
    storage: &dyn StorageProvider,
    req: FolderSendRequest,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    retries: u32,
    metrics: &MigrationMetrics,
) -> Result<(TransferPath, TransferOutcome)> {
    if let Some(handle) = resolve(mode, session.open(), metrics).await? {
        let outcome =
            send_folder_on_session(&handle, storage, req, ctrl, progress, retries).await?;
        metrics.record_session();
        return Ok((TransferPath::Session, outcome));
    }
    let (mut link, sess) = legacy.open().await?;
    let mut secure = SecureLink::new(link.as_mut(), enc, sess);
    let outcome = send_folder(&mut secure, storage, req, ctrl, progress, retries).await?;
    metrics.record_legacy();
    Ok((TransferPath::Legacy, outcome))
}

/// Receive a folder over the selected transport. Reuses
/// [`receive_folder_on_channel`] / [`receive_folder`].
#[allow(clippy::too_many_arguments)]
pub async fn receive_folder_selected(
    mode: CompatMode,
    session: &mut dyn SessionReceivePath,
    legacy: &mut dyn LegacyPath,
    enc: &dyn EncryptionProvider,
    storage: &dyn StorageProvider,
    dest_dir: &str,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    metrics: &MigrationMetrics,
) -> Result<(TransferPath, FolderReceived)> {
    if let Some((handle, incoming)) = resolve(mode, session.open(), metrics).await? {
        let received =
            receive_folder_on_channel(incoming, &handle, storage, dest_dir, ctrl, progress).await?;
        metrics.record_session();
        return Ok((TransferPath::Session, received));
    }
    let (mut link, sess) = legacy.open().await?;
    let mut secure = SecureLink::new(link.as_mut(), enc, sess);
    let received = receive_folder(&mut secure, storage, dest_dir, ctrl, progress).await?;
    metrics.record_legacy();
    Ok((TransferPath::Legacy, received))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_record_and_snapshot() {
        let m = MigrationMetrics::new();
        m.record_session();
        m.record_session();
        m.record_legacy();
        m.record_fallback(FallbackReason::OlderPeer);
        m.record_fallback(FallbackReason::CapabilityMismatch);
        let s = m.snapshot();
        assert_eq!(s.session_transfers, 2);
        assert_eq!(s.legacy_transfers, 1);
        assert_eq!(s.fallbacks, 2);
        assert_eq!(s.older_peer, 1);
        assert_eq!(s.capability_mismatch, 1);
        assert_eq!(s.version_mismatch, 0);
    }

    #[test]
    fn fallback_reason_labels_are_stable() {
        assert_eq!(FallbackReason::OlderPeer.label(), "older_peer");
        assert_eq!(FallbackReason::ExplicitCompat.label(), "explicit_compat");
    }

    #[test]
    fn compat_mode_default_is_auto() {
        assert_eq!(CompatMode::default(), CompatMode::Auto);
    }
}
