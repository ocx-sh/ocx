// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_lib::log;

use crate::error::MirrorError;
use crate::spec::MirrorSpec;

#[derive(clap::Args)]
pub struct Validate {
    /// Path to the mirror spec YAML file
    pub spec: PathBuf,
}

impl Validate {
    pub async fn execute(&self) -> Result<(), MirrorError> {
        let spec_path = &self.spec;

        if !spec_path.exists() {
            return Err(MirrorError::SpecNotFound(spec_path.display().to_string()));
        }

        let content = tokio::fs::read_to_string(spec_path)
            .await
            .map_err(|e| MirrorError::SpecNotFound(format!("{}: {e}", spec_path.display())))?;

        let spec: MirrorSpec = serde_yaml_ng::from_str(&content)
            .map_err(|e| MirrorError::SpecInvalid(vec![format!("YAML parse error: {e}")]))?;

        let errors = spec.validate(spec_path);
        if !errors.is_empty() {
            return Err(MirrorError::SpecInvalid(errors));
        }

        log::info!("Mirror spec '{}' is valid", spec.name);
        Ok(())
    }
}
