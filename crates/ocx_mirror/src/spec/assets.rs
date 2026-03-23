// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use ocx_lib::oci::Platform;
use serde::Deserialize;
use serde::de::{self, Deserializer};

/// Asset selection rules mapping platforms to ordered lists of filename regexes.
///
/// Platform keys are `os/arch` format (e.g. `linux/amd64`). Each platform maps to
/// one or more regex patterns that match asset filenames for that platform.
#[derive(Debug, Clone)]
pub struct AssetPatterns {
    pub patterns: HashMap<Platform, Vec<String>>,
}

impl<'de> Deserialize<'de> for AssetPatterns {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw: HashMap<String, Vec<String>> = HashMap::deserialize(deserializer)?;
        let mut patterns = HashMap::with_capacity(raw.len());
        for (key, value) in raw {
            let platform: Platform = key
                .parse()
                .map_err(|_| de::Error::custom(format!("invalid platform '{key}'")))?;
            patterns.insert(platform, value);
        }
        Ok(Self { patterns })
    }
}

impl AssetPatterns {
    pub fn validate(&self, errors: &mut Vec<String>) {
        for (platform, patterns) in &self.patterns {
            for pattern in patterns {
                if let Err(e) = regex::Regex::new(pattern) {
                    errors.push(format!("assets.{platform}: invalid regex '{pattern}': {e}"));
                }
            }
        }
    }

    /// Compile pattern strings into regexes, keyed by platform.
    /// Platform validation is done at deserialization time.
    pub fn compiled(&self) -> Result<HashMap<Platform, Vec<regex::Regex>>, String> {
        let mut result = HashMap::new();
        for (platform, patterns) in &self.patterns {
            let mut regexes = Vec::with_capacity(patterns.len());
            for pattern in patterns {
                let re = regex::Regex::new(pattern).map_err(|e| format!("invalid regex '{pattern}': {e}"))?;
                regexes.push(re);
            }
            result.insert(platform.clone(), regexes);
        }
        Ok(result)
    }
}
