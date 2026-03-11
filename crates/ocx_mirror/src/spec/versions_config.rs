// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::package::version::Version;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct VersionsConfig {
    pub min: Option<String>,
    pub max: Option<String>,
    pub new_per_run: Option<usize>,
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
