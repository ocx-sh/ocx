// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Bounded fork-readiness backoff schedule.
//!
//! Ported from grimoire `src/catalog/forge.rs` (`PollBounds` / `next_interval`
//! / `poll_until_ready`, same owner), transport-adjusted to REST-only and
//! owned by OCX (design register S5). GitHub provisions a fork's git objects
//! asynchronously — a fork's metadata reads ready before its first write can —
//! so readiness runs to a wall-clock deadline on exponential backoff, each
//! request bounded by a short timeout so one black-holed attempt cannot consume
//! the whole deadline (design register X5).

use std::time::Duration;

/// Initial backoff interval before the readiness poll doubles.
pub const DEFAULT_INITIAL_INTERVAL: Duration = Duration::from_secs(2);
/// Backoff cap — the interval never doubles past this.
pub const DEFAULT_MAX_INTERVAL: Duration = Duration::from_secs(30);
/// Wall-clock deadline for the whole readiness poll.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(300);
/// Per-request timeout so one hung request cannot eat the deadline.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Timing bounds for the bounded exponential-backoff readiness poll. A
/// dedicated struct so a test can assert the schedule (and drive the loop on a
/// short deadline) without real wall-clock waits.
#[derive(Debug, Clone, Copy)]
pub struct PollSchedule {
    /// The first sleep interval.
    pub initial_interval: Duration,
    /// The interval ceiling reached by doubling.
    pub max_interval: Duration,
    /// The cumulative wall-clock deadline.
    pub deadline: Duration,
    /// The per-request timeout applied to each readiness probe.
    pub request_timeout: Duration,
}

impl Default for PollSchedule {
    fn default() -> Self {
        Self {
            initial_interval: DEFAULT_INITIAL_INTERVAL,
            max_interval: DEFAULT_MAX_INTERVAL,
            deadline: DEFAULT_DEADLINE,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

/// The next backoff interval: double the current, capped at `max`.
fn next_interval(current: Duration, max: Duration) -> Duration {
    current.saturating_mul(2).min(max)
}

/// The exact sleep delays a bounded readiness poll uses, in order.
///
/// Doubles from `initial_interval`, capped at `max_interval`, with the final
/// delay clamped so cumulative wall time reaches — and never exceeds — the
/// `deadline`. Deterministic and clock-free: the readiness loop drives off this
/// list, and tests assert the schedule without sleeping.
#[must_use]
pub fn backoff_delays(schedule: &PollSchedule) -> Vec<Duration> {
    let mut delays = Vec::new();
    let mut interval = schedule.initial_interval;
    let mut accumulated = Duration::ZERO;
    loop {
        let remaining = schedule.deadline.saturating_sub(accumulated);
        if remaining.is_zero() {
            break;
        }
        let delay = interval.min(remaining);
        delays.push(delay);
        accumulated += delay;
        interval = next_interval(interval, schedule.max_interval);
    }
    delays
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_schedule_matches_x5_bounds() {
        let schedule = PollSchedule::default();
        assert_eq!(schedule.initial_interval, Duration::from_secs(2));
        assert_eq!(schedule.max_interval, Duration::from_secs(30));
        assert_eq!(schedule.deadline, Duration::from_secs(300));
        assert_eq!(schedule.request_timeout, Duration::from_secs(8));
    }

    #[test]
    fn backoff_doubles_caps_at_30s_and_bounds_the_deadline() {
        let delays = backoff_delays(&PollSchedule::default());
        // Doubling prefix 2, 4, 8, 16, then capped at the 30s ceiling.
        assert_eq!(
            &delays[..5],
            &[
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(30),
            ]
        );
        // No delay ever exceeds the cap.
        assert!(delays.iter().all(|delay| *delay <= Duration::from_secs(30)));
        // Cumulative wall time reaches the deadline exactly, never past it.
        let total: Duration = delays.iter().sum();
        assert_eq!(total, Duration::from_secs(300));
    }

    #[test]
    fn backoff_clamps_the_final_delay_to_the_remaining_deadline() {
        let schedule = PollSchedule {
            initial_interval: Duration::from_secs(2),
            max_interval: Duration::from_secs(30),
            deadline: Duration::from_secs(5),
            request_timeout: Duration::from_secs(8),
        };
        // 2 + clamp(4 -> 3) = 5, then the deadline is exhausted.
        assert_eq!(
            backoff_delays(&schedule),
            vec![Duration::from_secs(2), Duration::from_secs(3)]
        );
    }

    #[test]
    fn next_interval_doubles_up_to_the_cap() {
        assert_eq!(
            next_interval(Duration::from_secs(2), Duration::from_secs(30)),
            Duration::from_secs(4)
        );
        assert_eq!(
            next_interval(Duration::from_secs(16), Duration::from_secs(30)),
            Duration::from_secs(30)
        );
        assert_eq!(
            next_interval(Duration::from_secs(30), Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }
}
