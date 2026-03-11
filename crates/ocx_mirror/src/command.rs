// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod check;
mod options;
mod sync;
mod sync_all;
mod validate;

use crate::error::MirrorError;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Mirror packages from a spec file to an OCI registry
    Sync(sync::Sync),

    /// Check what would be mirrored without actually pushing (dry-run)
    Check(check::Check),

    /// Mirror all specs in a directory (mirror-*.yaml)
    SyncAll(sync_all::SyncAll),

    /// Validate a mirror spec file
    Validate(validate::Validate),
}

impl Command {
    pub async fn execute(&self) -> Result<(), MirrorError> {
        match self {
            Self::Sync(cmd) => cmd.execute().await,
            Self::Check(cmd) => cmd.execute().await,
            Self::SyncAll(cmd) => cmd.execute().await,
            Self::Validate(cmd) => cmd.execute().await,
        }
    }
}
