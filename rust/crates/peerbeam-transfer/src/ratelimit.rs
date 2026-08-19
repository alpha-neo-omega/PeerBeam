//! Holding a transfer to a chosen speed.
//!
//! # Why a token bucket rather than a sleep per chunk
//!
//! The naive limiter divides the chunk size by the rate and sleeps that long
//! before every chunk. It is wrong in both directions: a transfer that has been
//! idle gets no credit for the time it did not use, and one that fell behind
//! can never catch up. A bucket accrues allowance while nothing is sent and
//! spends it in a burst, which is what "1 MB/s" means to a person watching —
//! an average, not a cadence.
//!
//! # Why the arithmetic is separated from the waiting
//!
//! [`Bucket`] takes the elapsed time as an argument and returns how long to
//! wait. It holds no clock and never sleeps, so its behaviour can be tested
//! exactly — at any rate, over any span, in microseconds. A limiter that could
//! only be tested by sleeping would be tested by nobody, and would fail under
//! load for reasons unrelated to what it does.

use std::time::Duration;

/// Bytes of credit the bucket may bank while idle.
///
/// One second's worth. Enough that a transfer resuming after a pause moves
/// immediately instead of stuttering, and bounded so a long idle period cannot
/// buy an unbounded burst that defeats the limit the user asked for.
const BURST_SECONDS: f64 = 1.0;

/// The rate decision, with no clock and no sleeping.
#[derive(Debug, Clone, Copy)]
pub struct Bucket {
    /// Bytes per second. `0` means unlimited.
    rate: u64,
    /// Bytes of credit available now.
    allowance: f64,
}

impl Bucket {
    /// A bucket at `rate` bytes per second, starting full.
    ///
    /// Full rather than empty: the first chunk of a transfer should not be
    /// delayed by a limit the user set to shape a long transfer, and starting
    /// empty would make every send begin with a stall.
    #[must_use]
    pub fn new(rate: u64) -> Bucket {
        Bucket {
            rate,
            allowance: rate as f64 * BURST_SECONDS,
        }
    }

    /// Whether this bucket limits anything.
    #[must_use]
    pub fn is_unlimited(&self) -> bool {
        self.rate == 0
    }

    /// Change the rate.
    ///
    /// Applied live because a person who has just turned the limit down is
    /// usually doing it *because* a transfer is saturating their link right
    /// now; taking effect at the next transfer would be too late to help.
    ///
    /// Credit banked at the old rate does **not** need clamping downward here:
    /// [`take`] caps the allowance against the current rate every time it runs.
    /// An earlier version re-capped here too, and mutation testing showed no
    /// test could tell the difference — because there is none.
    ///
    /// Going the *other* way does need handling. An unlimited bucket never
    /// accrues anything (there is nothing to meter), so its allowance is zero;
    /// setting a rate on one without granting a burst would stall the very next
    /// chunk for a full second, which is the stutter [`new`] starts full to
    /// avoid. Only that transition refills — a limited bucket keeps what it has,
    /// or repeatedly re-setting the same rate would mint free credit and defeat
    /// the limit entirely.
    ///
    /// [`take`]: Self::take
    /// [`new`]: Self::new
    pub fn set_rate(&mut self, rate: u64) {
        let was_unlimited = self.rate == 0;
        self.rate = rate;
        if was_unlimited && rate != 0 {
            self.allowance = rate as f64 * BURST_SECONDS;
        }
    }

    /// Account for `bytes` about to be sent after `elapsed` since the last
    /// call, and answer how long to wait first.
    ///
    /// Returns [`Duration::ZERO`] when the bucket has the credit — which is the
    /// common case, so an unlimited or under-budget transfer pays nothing.
    pub fn take(&mut self, bytes: u64, elapsed: Duration) -> Duration {
        if self.rate == 0 {
            return Duration::ZERO;
        }
        let rate = self.rate as f64;
        self.allowance = (self.allowance + elapsed.as_secs_f64() * rate).min(rate * BURST_SECONDS);

        let need = bytes as f64;
        if self.allowance >= need {
            self.allowance -= need;
            return Duration::ZERO;
        }
        // Short by this much; wait for exactly that credit to accrue. The
        // allowance goes to zero rather than negative: the debt is paid by the
        // wait itself, and carrying it forward would charge for it twice.
        let deficit = need - self.allowance;
        self.allowance = 0.0;
        Duration::from_secs_f64(deficit / rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KB: u64 = 1024;

    #[test]
    fn an_unlimited_bucket_never_waits() {
        let mut b = Bucket::new(0);
        assert!(b.is_unlimited());
        assert_eq!(b.take(u64::MAX, Duration::ZERO), Duration::ZERO);
        assert_eq!(b.take(64 * KB, Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn the_first_chunk_is_not_delayed() {
        // Starting empty would make every limited transfer begin with a stall.
        let mut b = Bucket::new(100 * KB);
        assert_eq!(b.take(64 * KB, Duration::ZERO), Duration::ZERO);
    }

    /// **What the limit means.** Sending faster than the rate must cost time
    /// proportional to the excess.
    #[test]
    fn sending_past_the_allowance_waits_for_the_shortfall() {
        let mut b = Bucket::new(100 * KB);
        // Spend the full starting allowance.
        assert_eq!(b.take(100 * KB, Duration::ZERO), Duration::ZERO);
        // Now ask for half a second's worth with no time passed.
        let wait = b.take(50 * KB, Duration::ZERO);
        assert!(
            (wait.as_secs_f64() - 0.5).abs() < 0.01,
            "expected ~0.5s, got {wait:?}"
        );
    }

    #[test]
    fn idle_time_accrues_credit() {
        let mut b = Bucket::new(100 * KB);
        b.take(100 * KB, Duration::ZERO); // drain
                                          // A quarter second of idling buys a quarter second of bytes.
        assert_eq!(b.take(25 * KB, Duration::from_millis(250)), Duration::ZERO);
    }

    /// Credit is capped, or a transfer left alone overnight would blow straight
    /// past the limit the moment it resumed.
    #[test]
    fn banked_credit_cannot_exceed_one_seconds_worth() {
        let mut b = Bucket::new(100 * KB);
        b.take(100 * KB, Duration::ZERO);
        // An hour of idling must buy one second, not an hour.
        assert_eq!(b.take(100 * KB, Duration::from_secs(3600)), Duration::ZERO);
        let wait = b.take(100 * KB, Duration::ZERO);
        assert!(wait > Duration::ZERO, "the cap did not hold: {wait:?}");
    }

    /// Over a long run the average must land on the configured rate. This is
    /// the property a user actually cares about, and it is checked by
    /// arithmetic rather than by waiting.
    #[test]
    fn the_average_over_many_chunks_matches_the_rate() {
        let rate = 1_000_000u64;
        let chunk = 64 * KB;
        let mut b = Bucket::new(rate);
        let mut virtual_elapsed = Duration::ZERO;
        let mut sent = 0u64;

        for _ in 0..200 {
            let wait = b.take(chunk, Duration::ZERO);
            virtual_elapsed += wait;
            sent += chunk;
        }
        // Ignore the one-second head start the initial burst grants.
        let achieved = sent as f64 / (virtual_elapsed.as_secs_f64() + BURST_SECONDS);
        let ratio = achieved / rate as f64;
        assert!(
            (ratio - 1.0).abs() < 0.05,
            "achieved {achieved:.0} B/s against a {rate} B/s limit (ratio {ratio:.3})"
        );
    }

    /// Someone turning the limit down is usually doing it because a transfer is
    /// saturating their link right now.
    ///
    /// **Not drained first, deliberately.** An earlier version of this test
    /// spent the allowance before lowering the rate, which hid the bug it was
    /// meant to catch: without re-capping, a bucket that had banked 10 MB of
    /// credit at the old rate would spend all of it at full speed before the
    /// new limit bit at all — precisely the moment the user was trying to
    /// affect.
    #[test]
    fn lowering_the_rate_re_caps_credit_banked_at_the_old_one() {
        let mut b = Bucket::new(10 * 1024 * KB); // 10 MB/s, bucket starts full
        b.set_rate(50 * KB); // now 50 KB/s

        // One second's worth at the NEW rate is free; anything beyond it waits.
        assert_eq!(b.take(50 * KB, Duration::ZERO), Duration::ZERO);
        let wait = b.take(50 * KB, Duration::ZERO);
        assert!(
            wait > Duration::ZERO,
            "credit banked at the old rate outlived it: {wait:?}"
        );
    }

    /// **Setting a limit must not stall the next chunk.** A bucket created
    /// unlimited holds no allowance — nothing meters it — so taking it from
    /// unlimited to limited has to grant the same burst `new` does, or the
    /// first chunk after the user moves the slider waits a full second.
    #[test]
    fn setting_a_rate_on_an_unlimited_bucket_grants_a_burst() {
        let mut b = Bucket::new(0);
        b.set_rate(100 * KB);
        assert_eq!(
            b.take(100 * KB, Duration::ZERO),
            Duration::ZERO,
            "the first chunk after setting a limit stalled"
        );
    }

    /// ...but re-setting the same rate must not mint credit, or a caller that
    /// applies settings on a timer would lift the limit by accident.
    #[test]
    fn re_setting_a_live_rate_does_not_refill() {
        let mut b = Bucket::new(100 * KB);
        b.take(100 * KB, Duration::ZERO); // drain
        b.set_rate(100 * KB);
        assert!(
            b.take(100 * KB, Duration::ZERO) > Duration::ZERO,
            "re-setting the rate refilled the bucket"
        );
    }

    #[test]
    fn raising_the_rate_does_not_strand_the_transfer() {
        let mut b = Bucket::new(10 * KB);
        b.take(10 * KB, Duration::ZERO);
        b.set_rate(0); // back to unlimited
        assert_eq!(b.take(10 * 1024 * KB, Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn a_chunk_larger_than_a_seconds_budget_still_makes_progress() {
        // A 1 MiB chunk under a 100 KiB/s limit must not deadlock; it waits.
        let mut b = Bucket::new(100 * KB);
        b.take(100 * KB, Duration::ZERO);
        let wait = b.take(1024 * KB, Duration::ZERO);
        assert!(wait.as_secs_f64() > 9.0, "expected ~10s, got {wait:?}");
        assert!(wait.as_secs_f64() < 11.0, "expected ~10s, got {wait:?}");
    }
}
