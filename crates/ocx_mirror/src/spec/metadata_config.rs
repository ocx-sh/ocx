// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct MetadataConfig {
    pub default: PathBuf,
    #[serde(default)]
    pub platforms: HashMap<String, PathBuf>,
}

impl MetadataConfig {
    pub fn validate(&self, spec_dir: &Path, errors: &mut Vec<String>) {
        let default_path = spec_dir.join(&self.default);
        if !default_path.exists() {
            errors.push(format!("metadata.default: file not found: {}", self.default.display()));
        }

        for (platform, path) in &self.platforms {
            let full_path = spec_dir.join(path);
            if !full_path.exists() {
                errors.push(format!(
                    "metadata.platforms.{platform}: file not found: {}",
                    path.display()
                ));
            }
        }
    }
}
