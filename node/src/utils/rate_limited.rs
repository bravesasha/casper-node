//! Rate limiting for log messages.
//!
//! Implements the `rate_limited!` macro which can be used to ensure that a log message does not
//! spam the logs if triggered many times in a row. See its documentation for details.

// Note: This module uses 64 bit microseconds, so it is only usable a few hundred thousand years.
//       Code accordingly.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::sync::Semaphore;

/// Default interval to add tickets in.
pub(crate) const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Default count to add to tickets after interval has passed.
pub(crate) const DEFAULT_REFRESH_COUNT: usize = 100;

/// Macro for rate limiting log message (and other things).
///
/// Every rate limiter needs a unique identifier, which is used to create a static variable holding
/// the count and time of last update.
///
/// Every call of this macro will result, on average, in the load of two atomics in the success
/// path, three in the failure case, with the latter potentially doing additional work. Overall, it
/// is fairly cheap to call.
///
/// Associated with each call (and defaulting to `DEFAULT_REFRESH_INTERVAL` and
/// `DEFAULT_REFRESH_COUNT`) is an interval and a refresh count. Whenever the macro is called, it
/// will see if messages are available, if this is not the case, it will top up the count by `count`
/// if at least the interval has passed since the last top-up.
///
/// ## Example usage
///
/// The `rate_limited!` macro expects at least two arguments, the identifier described above, and a
/// function taking a single `usize` argument that will be called to make the actual log message.
/// The argument is the number of times this call has been skipped since the last time it was
/// called.
///
/// ```
/// rate_limited!(
///     CONNECTION_THRESHOLD_EXCEEDED,
///     |count| warn!(count, "exceeded connection threshold")
/// );
/// ```
///
/// The macro can alternatively called with a specific count-per:
///
/// ```
/// rate_limited!(
///     CONNECTION_THRESHOLD_EXCEEDED,
///     20,
///     Duration::from_secs(30),
///     |count| warn!(count, "exceeded connection threshold")
/// );
/// ```
///
/// The example above limits to 20 executions per 30 seconds.

macro_rules! rate_limited {
    ($key:ident, $action:expr) => {
        rate_limited!(
            $key,
            $crate::utils::rate_limited::DEFAULT_REFRESH_COUNT,
            $crate::utils::rate_limited::DEFAULT_REFRESH_INTERVAL,
            $action
        );
    };
    ($key:ident, $count:expr, $per:expr, $action:expr) => {
        static $key: $crate::utils::rate_limited::RateLimited =
            $crate::utils::rate_limited::RateLimited::new();

        #[allow(clippy::redundant_closure_call)]
        if let Some(skipped) = $key.acquire($count, $per) {
            $action(skipped);
        }
    };
}
pub(crate) use rate_limited;

/// Helper struct for the `rate_limited!` macro.
///
/// There is usually little use in constructing these directly.
#[derive(Debug)]
pub(crate) struct RateLimited {
    /// The count indicating how many messages are remaining.
    remaining: Semaphore,
    /// How many were skipped in the meantime.
    skipped: AtomicU64,
    /// The last time `remaining` was topped up.
    last_refresh_us: AtomicU64,
}

/// Returns the current time in microseconds.
#[inline(always)]
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or_default()
}

impl RateLimited {
    /// Constructs a new once-per instance.
    #[inline(always)]
    pub(crate) const fn new() -> Self {
        Self {
            remaining: Semaphore::const_new(0),
            skipped: AtomicU64::new(0),
            last_refresh_us: AtomicU64::new(0),
        }
    }

    /// Checks if there are tickets available.
    ///
    /// Returns `Some` on success with the count of skipped items that now has been reset to 0. Will
    /// add tickets if `per` has passed since the last top-up.
    pub(crate) fn acquire(&self, count: usize, per: Duration) -> Option<u64> {
        if count == 0 {
            return None;
        }

        if let Some(rv) = self.consume_permit() {
            return Some(rv);
        }

        // We failed to acquire a ticket. Check if we can refill tickets.
        let interval = per.as_micros() as u64;

        let now = now_micros();
        let last_refresh = self.last_refresh_us.load(Ordering::Relaxed);
        if last_refresh + interval > now {
            // No dice, not enough time has passed. Indicate we skipped our output and return.
            self.skipped.fetch_add(1, Ordering::Relaxed);

            return None;
        }

        // Enough time has passed! Let's see if we won the race for the next refresh.
        if self
            .last_refresh_us
            .compare_exchange(last_refresh, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            // We won! Add tickets.
            self.remaining.add_permits(count);
        }

        // Regardless, tickets have been added at this point. Try one more time before giving up.
        if let Some(rv) = self.consume_permit() {
            Some(rv)
        } else {
            self.skipped.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// Consume a permit from the counter/semaphore.
    ///
    /// Will reset skip count to 0 on success, and return the number of skipped calls.
    #[inline(always)]
    pub(crate) fn consume_permit(&self) -> Option<u64> {
        let permit = self.remaining.try_acquire().ok()?;

        permit.forget();
        Some(self.skipped.swap(0, Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        thread,
        time::Duration,
    };

    #[test]
    fn rate_limited_is_rate_limited() {
        let counter = AtomicUsize::new(0);

        let run = || {
            rate_limited!(
                RATE_LIMITED_IS_RATE_LIMITED_TEST,
                1,
                Duration::from_secs(60),
                |dropped| {
                    counter.fetch_add(1, Ordering::Relaxed);
                    assert_eq!(dropped, 0);
                }
            );
        };

        for _ in 0..10 {
            run();
        }

        // We expect one call in the default configuration.
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn rate_limiting_refreshes_properly() {
        let mut drop_counts = Vec::new();

        let run = |dc: &mut Vec<u64>| {
            rate_limited!(
                RATE_LIMITED_IS_RATE_LIMITED_TEST,
                2,
                Duration::from_secs(1),
                |dropped| {
                    dc.push(dropped);
                }
            );
        };

        for _ in 0..5 {
            run(&mut drop_counts);
        }
        assert_eq!(&[0, 0], drop_counts.as_slice());

        // Sleep long enough for the counter to refresh.
        thread::sleep(Duration::from_secs(1));

        for _ in 0..5 {
            run(&mut drop_counts);
        }
        assert_eq!(&[0, 0, 3, 0], drop_counts.as_slice());
    }
}