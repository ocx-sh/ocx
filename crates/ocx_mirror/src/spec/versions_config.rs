// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;

use ocx_lib::package::version::Version;
use serde::Deserialize;

/// Controls the order in which non-mirrored versions are selected when
/// `new_per_run` caps the batch size.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackfillOrder {
    /// Prioritise the most recent versions first (default). New mirrors get
    /// the latest releases immediately; older versions trickle in over
    /// subsequent runs.
    #[default]
    NewestFirst,
    /// Start from the oldest non-mirrored version and work forward. Useful
    /// when chronological completeness matters more than freshness.
    OldestFirst,
}

impl fmt::Display for BackfillOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NewestFirst => write!(f, "newest_first"),
            Self::OldestFirst => write!(f, "oldest_first"),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct VersionsConfig {
    pub min: Option<String>,
    pub max: Option<String>,
    pub new_per_run: Option<usize>,
    #[serde(default)]
    pub backfill: BackfillOrder,
}

impl VersionsConfig {
    pub fn validate(&self, errors: &mut Vec<String>) {
        if let Some(min) = &self.min
            && Version::parse(min).is_none()
        {
            errors.push(format!("versions.min: invalid version '{min}'"));
        }
        if let Some(max) = &self.max
            && Version::parse(max).is_none()
        {
            errors.push(format!("versions.max: invalid version '{max}'"));
        }
    }
}
