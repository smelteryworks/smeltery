//! Durations, rates, the runtime clock and UTC calendar helpers.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

/// `30.secs()`, `5.mins()`, `2.hours()`: durations from integers.
///
/// Implemented for `u64` only, so an unsuffixed literal picks it.
///
/// ```
/// use std::time::Duration;
/// use smeltery_watchfire::DurationExt;
///
/// assert_eq!(30.secs(), Duration::from_secs(30));
/// assert_eq!(5.mins(), Duration::from_secs(300));
/// assert_eq!(2.hours(), Duration::from_secs(7200));
/// assert_eq!(1.days(), Duration::from_secs(86_400));
/// assert_eq!(250.millis(), Duration::from_millis(250));
/// ```
pub trait DurationExt {
    /// Milliseconds.
    fn millis(self) -> Duration;
    /// Seconds.
    fn secs(self) -> Duration;
    /// Minutes.
    fn mins(self) -> Duration;
    /// Hours.
    fn hours(self) -> Duration;
    /// Days.
    fn days(self) -> Duration;
}

impl DurationExt for u64 {
    fn millis(self) -> Duration {
        Duration::from_millis(self)
    }
    fn secs(self) -> Duration {
        Duration::from_secs(self)
    }
    fn mins(self) -> Duration {
        Duration::from_secs(self.saturating_mul(60))
    }
    fn hours(self) -> Duration {
        Duration::from_secs(self.saturating_mul(3600))
    }
    fn days(self) -> Duration {
        Duration::from_secs(self.saturating_mul(86_400))
    }
}

/// A request rate for [`Watchfire::rate_limit`](crate::Watchfire::rate_limit): `count` requests
/// per `per`, as a token bucket holding at most `count` tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate {
    count: u32,
    per: Duration,
}

impl Rate {
    /// `count` requests per `per` (a zero count or period is treated as 1 per second).
    pub fn new(count: u32, per: Duration) -> Self {
        if count == 0 || per.is_zero() {
            return Self {
                count: 1,
                per: Duration::from_secs(1),
            };
        }
        Self { count, per }
    }

    /// Requests per period.
    pub fn count(self) -> u32 {
        self.count
    }

    /// The period.
    pub fn per(self) -> Duration {
        self.per
    }
}

/// `2.per_second()`, `30.per_minute()`: [`Rate`]s from integers.
///
/// ```
/// use smeltery_watchfire::RateExt;
///
/// assert_eq!(2.per_second().count(), 2);
/// assert_eq!(30.per_minute().per().as_secs(), 60);
/// ```
pub trait RateExt {
    /// Per second.
    fn per_second(self) -> Rate;
    /// Per minute.
    fn per_minute(self) -> Rate;
    /// Per hour.
    fn per_hour(self) -> Rate;
}

impl RateExt for u32 {
    fn per_second(self) -> Rate {
        Rate::new(self, Duration::from_secs(1))
    }
    fn per_minute(self) -> Rate {
        Rate::new(self, Duration::from_secs(60))
    }
    fn per_hour(self) -> Rate {
        Rate::new(self, Duration::from_secs(3600))
    }
}

/// Wall-clock milliseconds derived from Tokio's monotonic clock: the wall time at creation plus
/// the elapsed [`Instant`]. Under paused Tokio time (tests) it moves with `advance`, so the
/// scheduler, run records and job timestamps are testable.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
    anchor: Instant,
    anchor_ms: i64,
}

impl Clock {
    pub(crate) fn new() -> Self {
        Self::starting_at(system_ms())
    }

    pub(crate) fn starting_at(ms: i64) -> Self {
        Self {
            anchor: Instant::now(),
            anchor_ms: ms,
        }
    }

    pub(crate) fn now_ms(&self) -> i64 {
        let elapsed = i64::try_from(self.anchor.elapsed().as_millis()).unwrap_or(i64::MAX);
        self.anchor_ms.saturating_add(elapsed)
    }
}

/// The system's wall clock in Unix milliseconds (0 before 1970).
pub(crate) fn system_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

pub(crate) fn duration_ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month, day)` for days since 1970-01-01.
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Day of the week, 0 = Sunday.
pub(crate) fn weekday(days: i64) -> u32 {
    u32::try_from((days + 4).rem_euclid(7)).unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM:SS` (UTC) for Unix milliseconds.
pub fn format_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        for days in [-1000, 0, 59, 60, 11_016, 19_000, 20_729, 100_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        // 2026-10-03 is a Saturday.
        assert_eq!(weekday(days_from_civil(2026, 10, 3)), 6);
        assert_eq!(format_utc(0), "1970-01-01 00:00:00");
        assert_eq!(
            format_utc(days_from_civil(2024, 2, 29) * 86_400_000 + 3_723_000),
            "2024-02-29 01:02:03"
        );
    }

    #[test]
    fn zero_rate_is_sane() {
        assert_eq!(Rate::new(0, Duration::ZERO), 1.per_second());
    }

    #[tokio::test(start_paused = true)]
    async fn clock_moves_with_paused_time() {
        let clock = Clock::starting_at(1_000);
        tokio::time::advance(Duration::from_millis(250)).await;
        assert_eq!(clock.now_ms(), 1_250);
    }
}
