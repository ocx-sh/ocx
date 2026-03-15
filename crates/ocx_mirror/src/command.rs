// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod check;
mod options;
#[cfg(feature = "jsonschema")]
mod schema;
mod sync;
mod validate;

use crate::error::MirrorError;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Mirror packages from a spec file to an OCI registry
    Sync(sync::Sync),

    /// Check what would be mirrored without actually pushing (dry-run)
    Check(check::Check),

    /// Validate a mirror spec file
    Validate(validate::Validate),

    /// Generate JSON Schema for mirror types
    #[cfg(feature = "jsonschema")]
    Schema(schema::Schema),
}

impl Command {
    pub async fn execute(&self) -> Result<(), MirrorError> {
        match self {
            Self::Sync(cmd) => cmd.execute().await,
            Self::Check(cmd) => cmd.execute().await,
            Self::Validate(cmd) => cmd.execute().await,
            #[cfg(feature = "jsonschema")]
            Self::Schema(cmd) => cmd.execute().await,
        }
    }
}
