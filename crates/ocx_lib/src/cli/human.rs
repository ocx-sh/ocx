// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Human-readable size formatting for plain-text reports.
//!
//! Single source of truth so every report (`inspect` candidates, layers,
//! resolution chain, …) renders byte sizes identically. Wraps
//! [`indicatif::HumanBytes`] (binary units: KiB/MiB/GiB) — `indicatif` is
//! already a dependency for progress rendering, so no new crate is pulled
//! in. Machine surfaces (JSON) keep the raw integer; only plain-text notes
//! use this.

/// Formats a byte count as a binary-unit string (e.g. `4.21 MiB`).
///
/// A negative `size` means the value could not be determined (an
/// unexpectedly-absent on-disk blob) and renders as `unknown`.
pub fn human_bytes(size: i64) -> String {
    match u64::try_from(size) {
        Ok(bytes) => indicatif::HumanBytes(bytes).to_string(),
        Err(_) => "unknown".to_string(),
    }
}

/// Formats an instant as its distance from now (e.g. `11 hours ago`, `in 3
/// days`).
///
/// [`human_bytes`]' twin, and for the same reason: a report that prints
/// `mtime 1756000000` or a bare RFC 3339 string is handing the reader an
/// arithmetic problem, and every site that solved it solved it differently.
/// Wraps [`indicatif::HumanDuration`], already pulled in for progress
/// rendering.
///
/// Machine surfaces keep the raw value — the JSON payload still carries the
/// epoch seconds or the RFC 3339 string — exactly as they keep the raw byte
/// count. Only plain-text reports use this.
///
/// Pair it with [`human_instant`] wherever the exact value is itself evidence
/// (a fingerprint's inputs, a stamp's provenance): "11 hours ago" answers *is
/// this recent*, and the timestamp answers *which one is it*.
#[must_use]
pub fn human_time(at: chrono::DateTime<chrono::Utc>) -> String {
    time_since(at, chrono::Utc::now())
}

/// [`human_time`] against an explicit `now`, so the rendering is testable
/// without a clock.
fn time_since(at: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    let delta = now - at;
    // `to_std` rejects a negative duration, which is why the sign is taken off
    // first and put back as a word: a future instant is a legitimate state
    // (a clock skew, a stamp written by another machine), not an error.
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

/// Formats an instant absolutely, to the second, in UTC (e.g. `2026-08-27
/// 08:53:03 UTC`).
///
/// Deliberately **not** RFC 3339: this form is for a person reading a report,
/// and the `T` separator and the bare `Z` are for a parser. The wire and JSON
/// surfaces keep RFC 3339.
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
