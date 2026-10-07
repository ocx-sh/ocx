// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Background refresh vocabulary shared by every config section that checks for drift:
//! the [`RefreshPolicy`] posture and the `\d+[smhd]?` throttle interval grammar.

use std::time::Duration;

use ocx_util::wire_words;
use serde::{Deserialize, Deserializer, Serialize};

/// Default throttle interval between two background checks.
pub const DEFAULT_INTERVAL: &str = "1d";

wire_words! {
    /// What a background check does when it finds drift.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub enum RefreshPolicy {
        /// Drift is applied without asking, by the background check only, which runs only on an
        /// interactive terminal, outside CI, and online.
        Apply = "apply",
        /// Drift prints a stderr advisory naming the command that applies it; nothing is fetched.
        Notify = "notify",
        /// The background check is skipped entirely; only an explicit command refreshes.
        Manual = "manual",
    }
}

impl std::fmt::Display for RefreshPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl RefreshPolicy {
    /// The posture spelled `value` (`apply`, `notify` or `manual`), or `None` for anything else.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|policy| policy.as_str() == value)
    }
}

/// Deserializes an optional [`RefreshPolicy`] for the config key `key`; an unknown value, or a
/// value that is not a string, becomes "not set" with one warning.
///
/// Strict parsing would let a posture added by a later ocx fail the whole `config.toml` of an
/// older one, a managed payload included.
///
/// # Errors
///
/// None of its own: only a deserializer that cannot produce any value at all fails.
pub(crate) fn deserialize_lenient<'de, D: Deserializer<'de>>(
    deserializer: D,
    key: &str,
) -> Result<Option<RefreshPolicy>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Text(String),
        Other(serde::de::IgnoredAny),
    }

    let Some(raw) = Option::<Raw>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let parsed = match &raw {
        Raw::Text(value) => RefreshPolicy::parse(value),
        Raw::Other(_) => None,
    };
    if parsed.is_none() {
        let shown = match raw {
            Raw::Text(value) => format!("'{value}'"),
            Raw::Other(_) => "a non-string value".to_string(),
        };
        log::warn!("{key} = {shown} is not one of apply, notify or manual; ignored, the default applies");
    }
    Ok(parsed)
}

/// An interval value that does not match the `\d+[smhd]?` grammar.
#[derive(Debug, thiserror::Error)]
#[error("interval '{value}' is not a valid duration")]
pub struct IntervalError {
    value: String,
}

/// Parses `\d+[smhd]?` (bare digits = seconds) into a [`Duration`]; the multiplication saturates.
///
/// # Errors
///
/// Returns [`IntervalError`] when `value` does not match the grammar or its digits overflow `u64`.
pub fn parse_interval(value: &str) -> Result<Duration, IntervalError> {
    let invalid = || IntervalError {
        value: value.to_string(),
    };

    if value.is_empty() {
        return Err(invalid());
    }

    let (digits, suffix) = match value.chars().last() {
        Some(last) if last.is_ascii_alphabetic() => (&value[..value.len() - 1], Some(last)),
        _ => (value, None),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let count: u64 = digits.parse().map_err(|_| invalid())?;
    let multiplier = match suffix {
        None | Some('s') => 1,
        Some('m') => 60,
        Some('h') => 3_600,
        Some('d') => 86_400,
        Some(_) => return Err(invalid()),
    };
    Ok(Duration::from_secs(count.saturating_mul(multiplier)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ocx_env` cannot name `RefreshPolicy` (the dependency runs the other way), so its `Choice` literals are pinned here.
    #[test]
    fn the_refresh_env_vars_accept_exactly_the_policy_words() {
        let words: Vec<&str> = RefreshPolicy::ALL.iter().map(|policy| policy.as_str()).collect();
        for var in [&ocx_env::OCX_SELF_UPDATE, &ocx_env::OCX_TOOLCHAIN_UPDATE] {
            let ocx_env::EnvValue::Choice(choices) = var.value else {
                panic!("{} is not a choice variable", var.name);
            };
            assert_eq!(choices, words.as_slice(), "{}", var.name);
        }
    }

    #[test]
    fn parse_interval_bare_digits_is_seconds() {
        assert_eq!(parse_interval("30").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn parse_interval_seconds_suffix() {
        assert_eq!(parse_interval("45s").unwrap(), Duration::from_secs(45));
    }

    #[test]
    fn parse_interval_minutes_suffix() {
        assert_eq!(parse_interval("5m").unwrap(), Duration::from_secs(5 * 60));
    }

    #[test]
    fn parse_interval_hours_suffix() {
        assert_eq!(parse_interval("2h").unwrap(), Duration::from_secs(2 * 3600));
    }

    #[test]
    fn parse_interval_days_suffix() {
        assert_eq!(parse_interval("1d").unwrap(), Duration::from_secs(86_400));
    }

    #[test]
    fn parse_interval_default_constant_parses_to_one_day() {
        assert_eq!(parse_interval(DEFAULT_INTERVAL).unwrap(), Duration::from_secs(86_400));
    }

    /// Zero seconds is valid: nothing in the grammar excludes it.
    #[test]
    fn parse_interval_zero_is_valid_zero_duration() {
        assert_eq!(parse_interval("0").unwrap(), Duration::ZERO);
    }

    /// Digits that overflow `u64` fail at the parse step, before the saturating multiply.
    #[test]
    fn parse_interval_rejects_overflow_magnitude_value() {
        assert!(matches!(
            parse_interval("99999999999999999999d"),
            Err(IntervalError { .. })
        ));
    }

    #[test]
    fn parse_interval_rejects_empty_string() {
        assert!(matches!(parse_interval(""), Err(IntervalError { .. })));
    }

    #[test]
    fn parse_interval_rejects_garbage() {
        assert!(matches!(parse_interval("garbage"), Err(IntervalError { .. })));
    }

    #[test]
    fn parse_interval_rejects_negative() {
        assert!(matches!(parse_interval("-5"), Err(IntervalError { .. })));
    }

    #[test]
    fn parse_interval_rejects_unknown_suffix() {
        assert!(matches!(parse_interval("5x"), Err(IntervalError { .. })));
    }

    #[test]
    fn parse_interval_rejects_trailing_garbage_after_suffix() {
        assert!(matches!(parse_interval("5ss"), Err(IntervalError { .. })));
    }

    #[derive(Debug, Deserialize)]
    struct Probe {
        #[serde(default, deserialize_with = "probe_policy")]
        policy: Option<RefreshPolicy>,
    }

    fn probe_policy<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<RefreshPolicy>, D::Error> {
        deserialize_lenient(deserializer, "probe")
    }

    #[test]
    fn lenient_policy_reads_every_known_posture() {
        for (text, expected) in [
            ("apply", RefreshPolicy::Apply),
            ("notify", RefreshPolicy::Notify),
            ("manual", RefreshPolicy::Manual),
        ] {
            let probe: Probe = toml::from_str(&format!("policy = \"{text}\"")).unwrap();
            assert_eq!(probe.policy, Some(expected), "{text}");
        }
    }

    #[test]
    fn lenient_policy_turns_an_unknown_value_into_not_set() {
        let probe: Probe = toml::from_str("policy = \"someday\"").unwrap();
        assert_eq!(probe.policy, None);
    }

    #[test]
    fn lenient_policy_turns_a_non_string_into_not_set() {
        let probe: Probe = toml::from_str("policy = 3").unwrap();
        assert_eq!(probe.policy, None);
    }

    #[test]
    fn lenient_policy_absent_is_not_set() {
        let probe: Probe = toml::from_str("").unwrap();
        assert_eq!(probe.policy, None);
    }

    #[test]
    fn interval_error_message_names_the_value() {
        assert_eq!(
            parse_interval("5x").unwrap_err().to_string(),
            "interval '5x' is not a valid duration"
        );
    }
}
