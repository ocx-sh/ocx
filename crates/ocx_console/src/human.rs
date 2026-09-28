// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Human-readable sizes and times for plain-text reports; JSON surfaces keep the raw values.

/// Formats a byte count in binary units (e.g. `4.21 MiB`); a negative `size` (undetermined) renders `unknown`.
pub fn human_bytes(size: i64) -> String {
    match u64::try_from(size) {
        Ok(bytes) => indicatif::HumanBytes(bytes).to_string(),
        Err(_) => "unknown".to_string(),
    }
}

/// Formats an instant as its distance from now (e.g. `11 hours ago`, `in 3 days`).
///
/// Pair it with [`human_instant`] wherever the exact value is itself evidence.
#[must_use]
pub fn human_time(at: chrono::DateTime<chrono::Utc>) -> String {
    time_since(at, chrono::Utc::now())
}

/// [`human_time`] against an explicit `now`.
fn time_since(at: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    let delta = now - at;
    // A future instant (clock skew, another machine's stamp) is legitimate, and `to_std` rejects negatives.
    let Ok(magnitude) = delta.abs().to_std() else {
        return "unknown".to_owned();
    };
    if magnitude < std::time::Duration::from_secs(1) {
        return "just now".to_owned();
    }
    let rendered = indicatif::HumanDuration(magnitude);
    if delta < chrono::TimeDelta::zero() {
        format!("in {rendered}")
    } else {
        format!("{rendered} ago")
    }
}

/// Formats an instant to the second in UTC for a person (e.g. `2026-08-27 08:53:03 UTC`); wire and JSON keep RFC 3339.
#[must_use]
pub fn human_instant(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(seconds, 0).expect("a representable instant")
    }

    /// The sign is a word, not a `-` in front of a duration.
    #[test]
    fn time_since_names_both_directions() {
        let now = at(1_000_000);
        assert_eq!(time_since(at(1_000_000 - 3 * 3600), now), "3 hours ago");
        assert_eq!(time_since(at(1_000_000 + 3 * 3600), now), "in 3 hours");
    }

    /// Sub-second is its own answer: `0 seconds ago` reads as a rounding bug.
    #[test]
    fn time_since_collapses_the_sub_second_case() {
        let now = at(1_000_000);
        assert_eq!(time_since(now, now), "just now");
    }

    #[test]
    fn human_instant_is_space_separated_and_utc() {
        assert_eq!(human_instant(at(1_756_000_000)), "2025-08-24 01:46:40 UTC");
    }
}
