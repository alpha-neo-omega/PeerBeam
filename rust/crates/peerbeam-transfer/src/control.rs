//! Shared pause/resume/cancel handle for an in-flight transfer.
//!
//! Cloneable (all clones share one state via `Arc`) so the UI keeps a handle
//! while the transfer task holds another. The send loop consults it every
//! chunk: it blocks while paused and aborts promptly on cancel.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

#[derive(Default)]
struct State {
    paused: AtomicBool,
    cancelled: AtomicBool,
    /// Wakes the send loop when resumed or cancelled.
    wake: Notify,
    /// Outbound speed ceiling in bytes per second; `0` is unlimited.
    ///
    /// Lives beside pause and cancel because it is the same kind of thing: a
    /// knob the user turns while a transfer is already running, which the send
    /// loop must notice without being restarted.
    rate: AtomicU64,
    /// The bucket the send loop meters against, and when it last did.
    ///
    /// A `Mutex` rather than more atomics: the allowance and the timestamp are
    /// only meaningful together, and updating them separately would let two
    /// chunks each believe they had the same credit.
    meter: Mutex<Meter>,
}

/// The rate bucket plus the instant it was last charged.
struct Meter {
    bucket: crate::ratelimit::Bucket,
    last: Option<Instant>,
}

impl Default for Meter {
    fn default() -> Self {
        // Unlimited until someone says otherwise; `set_rate_limit` replaces the
        // bucket's rate rather than the bucket, so the accrual logic has one
        // home.
        Meter {
            bucket: crate::ratelimit::Bucket::new(0),
            last: None,
        }
    }
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

    /// Set the outbound ceiling in bytes per second; `0` is unlimited.
    ///
    /// Takes effect on the next chunk, not the next transfer — someone turning
    /// this down is usually doing it because a transfer is saturating their
    /// link right now.
    pub fn set_rate_limit(&self, bytes_per_sec: u64) {
        self.state.rate.store(bytes_per_sec, Ordering::SeqCst);
        self.state
            .meter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .bucket
            .set_rate(bytes_per_sec);
    }

    /// The current ceiling; `0` is unlimited.
    #[must_use]
    pub fn rate_limit(&self) -> u64 {
        self.state.rate.load(Ordering::SeqCst)
    }

    /// How long the send loop must wait before putting `bytes` on the wire.
    ///
    /// [`Duration::ZERO`] when unlimited or within budget, which is the common
    /// case — an unthrottled transfer pays one atomic load for this.
    pub fn throttle(&self, bytes: u64) -> Duration {
        if self.state.rate.load(Ordering::SeqCst) == 0 {
            return Duration::ZERO;
        }
        let mut meter = self.state.meter.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let elapsed = meter.last.map_or(Duration::ZERO, |t| now.duration_since(t));
        meter.last = Some(now);
        meter.bucket.take(bytes, elapsed)
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
            tokio::pin!(notified);
            notified.as_mut().enable(); // register BEFORE re-check or a wake is lost
            if !self.is_paused() || self.is_cancelled() {
                break;
            }
            notified.await;
        }
    }

    /// Resolves as soon as cancellation is requested. Intended for a
    /// `select!` raced against a blocking `recv_frame`, so cancelling a
    /// receive that is parked waiting on the peer interrupts it promptly
    /// instead of only being noticed between frames.
    ///
    /// Loops around the notify (rather than awaiting it once) because `wake`
    /// also fires on [`resume`](Self::resume): a resume-triggered wake must
    /// not be mistaken for cancellation, so we re-check `is_cancelled` and go
    /// back to waiting if it was actually a resume.
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            // Register for wake, then re-check to avoid a lost-notify race.
            let notified = self.state.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable(); // register BEFORE re-check or a wake is lost
            if self.is_cancelled() {
                return;
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

    #[tokio::test]
    async fn cancelled_returns_immediately_when_already_cancelled() {
        let c = TransferControl::new();
        c.cancel();
        // Should not hang.
        c.cancelled().await;
    }

    #[tokio::test]
    async fn cancelled_unblocks_on_cancel() {
        let c = TransferControl::new();
        let c2 = c.clone();
        let waiter = tokio::spawn(async move { c2.cancelled().await });
        // Give the waiter a moment to park, then cancel.
        tokio::task::yield_now().await;
        c.cancel();
        waiter.await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_ignores_a_resume_wake() {
        // `resume()` also calls `notify_waiters`; a waiter on `cancelled()`
        // must not mistake that wake for cancellation and must keep waiting.
        let c = TransferControl::new();
        c.pause();
        let c2 = c.clone();
        let waiter = tokio::spawn(async move { c2.cancelled().await });
        tokio::task::yield_now().await;
        c.resume(); // wakes the notify, but cancelled() must keep waiting
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!waiter.is_finished(), "resume must not resolve cancelled()");
        c.cancel();
        waiter.await.unwrap();
    }

    /// Regression test for the lost-wakeup race: `notified()` must be
    /// registered (via `enable()`) *before* the pause re-check, or a
    /// `resume()` that lands in the gap between building the `Notified`
    /// future and its first poll is dropped and the waiter never wakes.
    ///
    /// A single-threaded executor can't preempt between two synchronous
    /// statements with no `.await` between them, so this only manifests with
    /// real OS-thread parallelism: a genuine `std::thread` races a raw
    /// `resume()` against the waiter's build-then-await window, with no
    /// synchronization at all, over many iterations to make the race land.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn wait_while_paused_never_misses_a_racing_resume() {
        for _ in 0..300 {
            let c = TransferControl::new();
            c.pause();
            let c2 = c.clone();
            let waiter = tokio::spawn(async move { c2.wait_while_paused().await });

            let c3 = c.clone();
            std::thread::spawn(move || c3.resume());

            tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
                .await
                .expect("resume racing wait_while_paused must not be lost")
                .unwrap();
        }
    }

    /// Same race, but against `cancelled()` and a racing `cancel()` — the
    /// counterpart fix in that method must not miss a wake either.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancelled_never_misses_a_racing_cancel() {
        for _ in 0..300 {
            let c = TransferControl::new();
            let c2 = c.clone();
            let waiter = tokio::spawn(async move { c2.cancelled().await });

            let c3 = c.clone();
            std::thread::spawn(move || c3.cancel());

            tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
                .await
                .expect("cancel racing cancelled() must not be lost")
                .unwrap();
        }
    }
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    const KB: u64 = 1024;

    #[test]
    fn a_control_is_unlimited_until_asked_otherwise() {
        // A limit nobody set is a slow transfer nobody can explain.
        let c = TransferControl::new();
        assert_eq!(c.rate_limit(), 0);
        assert_eq!(c.throttle(64 * KB), Duration::ZERO);
        assert_eq!(c.throttle(u64::MAX), Duration::ZERO);
    }

    #[test]
    fn a_limit_eventually_costs_time() {
        let c = TransferControl::new();
        c.set_rate_limit(100 * KB);
        // The bucket starts full, so the first second's worth is free.
        assert_eq!(c.throttle(100 * KB), Duration::ZERO);
        assert!(
            c.throttle(100 * KB) > Duration::ZERO,
            "a set limit never made anything wait"
        );
    }

    #[test]
    fn clearing_the_limit_restores_full_speed() {
        let c = TransferControl::new();
        c.set_rate_limit(KB);
        c.throttle(KB);
        assert!(c.throttle(KB) > Duration::ZERO);

        c.set_rate_limit(0);
        assert_eq!(c.rate_limit(), 0);
        assert_eq!(
            c.throttle(100 * 1024 * KB),
            Duration::ZERO,
            "clearing the limit left the transfer throttled"
        );
    }

    /// The control is cloned into the send task; a limit set through one handle
    /// must be seen by the other, or changing it mid-transfer would do nothing.
    #[test]
    fn a_limit_set_on_one_clone_is_seen_by_the_others() {
        let a = TransferControl::new();
        let b = a.clone();
        a.set_rate_limit(50 * KB);
        assert_eq!(b.rate_limit(), 50 * KB);
        b.throttle(50 * KB);
        assert!(
            a.throttle(50 * KB) > Duration::ZERO,
            "the two handles are metering separate buckets"
        );
    }
}
