//! Shared pause/resume/cancel handle for an in-flight transfer.
//!
//! Cloneable (all clones share one state via `Arc`) so the UI keeps a handle
//! while the transfer task holds another. The send loop consults it every
//! chunk: it blocks while paused and aborts promptly on cancel.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

#[derive(Default)]
struct State {
    paused: AtomicBool,
    cancelled: AtomicBool,
    /// Wakes the send loop when resumed or cancelled.
    wake: Notify,
}

/// Controls the lifecycle of a running transfer.
#[derive(Clone, Default)]
pub struct TransferControl {
    state: Arc<State>,
}

impl TransferControl {
    /// Create a fresh control (not paused, not cancelled).
    pub fn new() -> Self {
        Self::default()
    }

    /// Request a pause. The send loop stops before its next chunk.
    pub fn pause(&self) {
        self.state.paused.store(true, Ordering::SeqCst);
    }

    /// Resume after a pause and wake the send loop.
    pub fn resume(&self) {
        self.state.paused.store(false, Ordering::SeqCst);
        self.state.wake.notify_waiters();
    }

    /// Request cancellation and wake any paused loop so it can exit.
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::SeqCst);
        self.state.wake.notify_waiters();
    }

    /// Whether the transfer is paused.
    pub fn is_paused(&self) -> bool {
        self.state.paused.load(Ordering::SeqCst)
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::SeqCst)
    }

    /// Block while paused, returning as soon as resumed or cancelled.
    pub async fn wait_while_paused(&self) {
        while self.is_paused() && !self.is_cancelled() {
            // Register for wake, then re-check to avoid a lost-notify race.
            let notified = self.state.wake.notified();
            if !self.is_paused() || self.is_cancelled() {
                break;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_running() {
        let c = TransferControl::new();
        assert!(!c.is_paused());
        assert!(!c.is_cancelled());
    }

    #[test]
    fn pause_resume_cancel_flags() {
        let c = TransferControl::new();
        c.pause();
        assert!(c.is_paused());
        c.resume();
        assert!(!c.is_paused());
        c.cancel();
        assert!(c.is_cancelled());
    }

    #[test]
    fn clones_share_state() {
        let a = TransferControl::new();
        let b = a.clone();
        a.cancel();
        assert!(b.is_cancelled(), "clone observes the same state");
    }

    #[tokio::test]
    async fn wait_returns_immediately_when_not_paused() {
        let c = TransferControl::new();
        // Should not hang.
        c.wait_while_paused().await;
    }

    #[tokio::test]
    async fn wait_unblocks_on_resume() {
        let c = TransferControl::new();
        c.pause();
        let c2 = c.clone();
        let waiter = tokio::spawn(async move { c2.wait_while_paused().await });
        // Give the waiter a moment to park, then resume.
        tokio::task::yield_now().await;
        c.resume();
        waiter.await.unwrap();
    }
}
