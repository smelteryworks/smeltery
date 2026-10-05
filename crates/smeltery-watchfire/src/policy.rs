//! Restart policies, exponential backoff with full jitter, and the jitter source.

use std::ops::RangeInclusive;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

/// When an agent whose run ended on its own is started again.
///
/// A run ended by `stop`, `pause`, `restart`, `remove` or shutdown is never restarted by the
/// policy. A stalled run (heartbeat timeout) counts as a failure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Restart {
    /// Never restart.
    Never,
    /// Restart after an error, a panic or a stall, not after `Ok(())`.
    #[default]
    OnFailure,
    /// Restart after every exit, including `Ok(())`.
    Always,
}

impl Restart {
    /// `never`, `on_failure` or `always`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnFailure => "on_failure",
            Self::Always => "always",
        }
    }
}

/// Exponential backoff between automatic restarts, with full jitter.
///
/// Restart number `n` (0-based, counting consecutive restarts) waits a random delay between zero
/// and `min(cap, initial × 2ⁿ)` ("full jitter", so many failing agents do not restart in step).
/// A run that lasted at least [`Backoff::reset_after`] (60 s) resets the count.
///
/// ```
/// use std::time::Duration;
/// use smeltery_watchfire::{Backoff, DurationExt};
///
/// let backoff = Backoff::new(1.secs()..=30.secs());
/// assert_eq!(backoff.ceiling(0), 1.secs());
/// assert_eq!(backoff.ceiling(3), 8.secs());
/// assert_eq!(backoff.ceiling(40), 30.secs());
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backoff {
    initial: Duration,
    cap: Duration,
}

impl Default for Backoff {
    /// 1 s initial, 60 s cap.
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            cap: Duration::from_secs(60),
        }
    }
}

/// A run lasting at least this long resets the restart count.
const RESET_AFTER: Duration = Duration::from_secs(60);

impl Backoff {
    /// Backoff from `initial` up to `cap` (a cap below `initial` is raised to it; a zero
    /// `initial` is treated as 1 ms).
    pub fn new(range: RangeInclusive<Duration>) -> Self {
        let (initial, cap) = range.into_inner();
        let initial = initial.max(Duration::from_millis(1));
        Self {
            initial,
            cap: cap.max(initial),
        }
    }

    /// The first delay's upper bound.
    pub fn initial(&self) -> Duration {
        self.initial
    }

    /// The largest delay.
    pub fn cap(&self) -> Duration {
        self.cap
    }

    /// The upper bound for restart number `attempt`: `min(cap, initial × 2^attempt)`.
    pub fn ceiling(&self, attempt: u32) -> Duration {
        2_u32
            .checked_pow(attempt)
            .and_then(|factor| self.initial.checked_mul(factor))
            .map_or(self.cap, |d| d.min(self.cap))
    }

    /// A delay for restart number `attempt`: uniform in `0..=ceiling(attempt)`, drawn from `rng`.
    pub(crate) fn delay(&self, attempt: u32, rng: &Jitter) -> Duration {
        rng.below(self.ceiling(attempt))
    }

    /// How long a run must last to reset the restart count (60 s).
    pub fn reset_after(&self) -> Duration {
        RESET_AFTER
    }
}

/// A small, seedable random source for jitter (SplitMix64), seeded from the OS by default.
/// Jitter needs no cryptographic strength; a seed makes tests repeatable.
#[derive(Debug)]
pub(crate) struct Jitter {
    state: Mutex<u64>,
}

impl Jitter {
    pub(crate) fn from_os() -> Self {
        let mut seed = [0_u8; 8];
        let seed = match getrandom::fill(&mut seed) {
            Ok(()) => u64::from_le_bytes(seed),
            Err(_) => crate::time::system_ms().unsigned_abs() ^ 0x9E37_79B9_7F4A_7C15,
        };
        Self::seeded(seed)
    }

    pub(crate) fn seeded(seed: u64) -> Self {
        Self {
            state: Mutex::new(seed),
        }
    }

    pub(crate) fn next_u64(&self) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..=max` at millisecond resolution.
    pub(crate) fn below(&self, max: Duration) -> Duration {
        let max_ms = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
        if max_ms == 0 {
            return Duration::ZERO;
        }
        let span = max_ms.saturating_add(1);
        Duration::from_millis(self.next_u64() % span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_grows_and_caps() {
        let b = Backoff::default();
        let secs: Vec<u64> = (0..8).map(|n| b.ceiling(n).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(b.ceiling(u32::MAX), Duration::from_secs(60));
    }

    #[test]
    fn range_is_normalised() {
        let b = Backoff::new(Duration::ZERO..=Duration::ZERO);
        assert_eq!(b.initial(), Duration::from_millis(1));
        assert_eq!(b.cap(), Duration::from_millis(1));
        let b = Backoff::new(Duration::from_secs(5)..=Duration::from_secs(1));
        assert_eq!(b.cap(), Duration::from_secs(5));
    }

    #[test]
    fn full_jitter_stays_within_bounds_and_spreads() {
        let b = Backoff::new(Duration::from_secs(1)..=Duration::from_secs(30));
        let rng = Jitter::seeded(7);
        for attempt in 0..10 {
            let ceiling = b.ceiling(attempt);
            let delays: Vec<Duration> = (0..200).map(|_| b.delay(attempt, &rng)).collect();
            assert!(delays.iter().all(|d| *d <= ceiling));
            let min = delays.iter().min().copied().unwrap_or_default();
            let max = delays.iter().max().copied().unwrap_or_default();
            // 200 draws cover most of the range.
            assert!(min < ceiling / 4, "attempt {attempt}: min {min:?}");
            assert!(max > ceiling * 3 / 4, "attempt {attempt}: max {max:?}");
        }
        assert_eq!(rng.below(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn seeded_jitter_repeats() {
        let a = Jitter::seeded(42);
        let b = Jitter::seeded(42);
        assert_eq!(a.next_u64(), b.next_u64());
        assert_ne!(a.next_u64(), Jitter::seeded(43).next_u64());
    }
}
