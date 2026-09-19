// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! UTC build-timestamp helpers shared by the mirror tool and `ocx package push`.
//!
//! See the underscore-build-separator ADR for the wire-format rationale: build
//! metadata is parsed from `+` or `_`, but always rendered with `_` because OCI
//! tag references forbid `+`.

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Failure modes when attaching build metadata to a [`super::Version`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildMetaError {
    /// The version lacks the `X.Y.Z` core required to carry build metadata.
    #[error("version '{0}' lacks X.Y.Z form needed for build metadata")]
    NoPatch(String),
    /// The version already carries a build-metadata segment.
    #[error("version '{0}' already has build metadata")]
    AlreadyPresent(String),
}

/// Wire-format selector for the build-metadata suffix.
///
/// Used by mirror specs and by `ocx package push --build-timestamp` to choose
/// between UTC datetime, UTC date, or no suffix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum BuildTimestampFormat {
    /// `YYYYMMDDhhmmss` — UTC down to the second.
    #[default]
    Datetime,
    /// `YYYYMMDD` — UTC date only.
    Date,
    /// No suffix; `build_timestamp` returns `None`.
    None,
}

/// Returns the UTC build timestamp for the current run, or `None` for
/// [`BuildTimestampFormat::None`].
pub fn build_timestamp(format: &BuildTimestampFormat) -> Option<String> {
    let now = Utc::now();
    match format {
        BuildTimestampFormat::Datetime => Some(now.format("%Y%m%d%H%M%S").to_string()),
        BuildTimestampFormat::Date => Some(now.format("%Y%m%d").to_string()),
        BuildTimestampFormat::None => None,
    }
}

impl clap_builder::ValueEnum for BuildTimestampFormat {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Datetime, Self::Date, Self::None]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        use clap_builder::builder::PossibleValue;
        Some(match self {
            Self::Datetime => PossibleValue::new("datetime"),
            Self::Date => PossibleValue::new("date"),
            Self::None => PossibleValue::new("none"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_format_is_fourteen_digits() {
        let ts = build_timestamp(&BuildTimestampFormat::Datetime).expect("Datetime returns Some");
        assert_eq!(ts.len(), 14, "expected 14 digits, got {ts}");
        assert!(ts.chars().all(|c| c.is_ascii_digit()), "non-digit in {ts}");
    }

    #[test]
    fn date_format_is_eight_digits() {
        let ts = build_timestamp(&BuildTimestampFormat::Date).expect("Date returns Some");
        assert_eq!(ts.len(), 8, "expected 8 digits, got {ts}");
        assert!(ts.chars().all(|c| c.is_ascii_digit()), "non-digit in {ts}");
    }

    #[test]
    fn none_returns_none() {
        assert!(build_timestamp(&BuildTimestampFormat::None).is_none());
    }

    #[test]
    fn value_enum_round_trips_known_values() {
        use clap_builder::ValueEnum;
        assert!(matches!(
            BuildTimestampFormat::from_str("datetime", true),
            Ok(BuildTimestampFormat::Datetime)
        ));
        assert!(matches!(
            BuildTimestampFormat::from_str("date", true),
            Ok(BuildTimestampFormat::Date)
        ));
        assert!(matches!(
            BuildTimestampFormat::from_str("none", true),
            Ok(BuildTimestampFormat::None)
        ));
        assert!(BuildTimestampFormat::from_str("bogus", true).is_err());
    }
}
