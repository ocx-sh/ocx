// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_lib::log;

use crate::error::MirrorError;
use crate::spec;

#[derive(clap::Args)]
pub struct Validate {
    /// Path to the mirror spec YAML file
    pub spec: PathBuf,
}

impl Validate {
    pub async fn execute(&self) -> Result<(), MirrorError> {
        let spec = spec::load_spec(&self.spec).await?;
        log::info!("Mirror spec '{}' is valid", spec.name);
        Ok(())
    }
}
