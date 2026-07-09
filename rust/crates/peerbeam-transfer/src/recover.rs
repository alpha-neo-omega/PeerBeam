//! Interrupted-transfer recovery: reconnect and resume automatically.
//!
//! A [`LinkFactory`] produces a fresh [`Link`] on demand. The recovery
//! drivers retry the transfer across new links up to `max_attempts`, with
//! backoff. Because [`send_file`]/[`receive_file`] negotiate a resume offset
//! from the receiver's on-disk bytes, each retry continues where the last
//! left off rather than restarting. The sender persists a checkpoint via the
//! [`ReliabilityStore`] so a transfer can even be resumed after a process
//! restart, and clears it on success.
//!
//! Non-recoverable outcomes are never retried: a cancel returns immediately,
//! and an integrity failure surfaces as an error.

use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use peerbeam_domain::entity::{Progress, TransferSession};
use peerbeam_domain::error::{DomainError, Result};
use peerbeam_domain::port::{Link, ReliabilityStore, StorageProvider};

use crate::control::TransferControl;
use crate::stream::{receive_file, send_file, Received, SendRequest, TransferOutcome};

/// Base backoff between reconnect attempts (grows linearly).
const RECONNECT_BACKOFF: Duration = Duration::from_millis(30);

/// Produces a fresh link, e.g. by dialing a peer or accepting a connection.
#[async_trait]
pub trait LinkFactory: Send {
    /// Establish a new link.
    async fn connect(&mut self) -> Result<Box<dyn Link>>;
}

/// Whether an error is worth retrying. Integrity failures and cancellations
/// are terminal; connection/transfer/storage errors are transient.
fn recoverable(error: &DomainError) -> bool {
    !matches!(error, DomainError::Integrity(_) | DomainError::Cancelled)
}

async fn backoff(attempt: u32) {
    tokio::time::sleep(RECONNECT_BACKOFF * attempt).await;
}

/// Send a file with automatic reconnect-and-resume.
///
/// Persists `checkpoint` up front (state persistence), retries across fresh
/// links up to `max_attempts`, and clears the checkpoint once the transfer
/// completes.
#[allow(clippy::too_many_arguments)]
pub async fn send_file_recover(
    factory: &mut dyn LinkFactory,
    storage: &dyn StorageProvider,
    reliability: &dyn ReliabilityStore,
    req: SendRequest,
    checkpoint: TransferSession,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    max_attempts: u32,
    inner_retries: u32,
) -> Result<TransferOutcome> {
    reliability.save_checkpoint(&checkpoint)?;
    let id = checkpoint.id.clone();

    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let result = match factory.connect().await {
            Ok(mut link) => {
                send_file(
                    &mut *link,
                    storage,
                    req.clone(),
                    ctrl,
                    progress,
                    inner_retries,
                )
                .await
            }
            Err(e) => Err(e),
        };

        match result {
            Ok(outcome) => {
                reliability.clear_checkpoint(&id)?;
                return Ok(outcome);
            }
            Err(e) if recoverable(&e) && attempt < max_attempts => {
                tracing::warn!("send attempt {attempt} failed: {e}; retrying");
                backoff(attempt).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Receive a file with automatic reconnect-and-resume.
pub async fn receive_file_recover(
    factory: &mut dyn LinkFactory,
    storage: &dyn StorageProvider,
    dest_dir: &str,
    ctrl: &TransferControl,
    progress: &UnboundedSender<Progress>,
    max_attempts: u32,
) -> Result<Received> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let result = match factory.connect().await {
            Ok(mut link) => receive_file(&mut *link, storage, dest_dir, ctrl, progress).await,
            Err(e) => Err(e),
        };

        match result {
            Ok(received) => return Ok(received),
            Err(e) if recoverable(&e) && attempt < max_attempts => {
                tracing::warn!("receive attempt {attempt} failed: {e}; retrying");
                backoff(attempt).await;
            }
            Err(e) => return Err(e),
        }
    }
}
