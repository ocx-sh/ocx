// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx shell revoke` — withdraw a project's consent stamp.
//!
//! Idempotent by contract: revoking an unstamped project exits 0.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::activation::ProjectIdentity;
use ocx_project::consent::{self, Revoked};

use crate::app::project_context::resolve_project_paths;

/// The `ocx shell revoke` arguments; its help text lives on `Shell::Revoke`.
#[derive(Parser)]
pub struct ShellRevoke {
    /// The directory whose project to revoke (default: the current one)
    ///
    /// Resolved by the same upward walk `ocx shell allow` uses, so the two
    /// commands always name the same project.
    path: Option<PathBuf>,
}

impl ShellRevoke {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let (config_path, _) = resolve_project_paths(&context, self.path.as_deref()).await?;
        let identity = ProjectIdentity::resolve(config_path).await?;

        let dir = identity.dir.clone();
        let revoked = tokio::task::spawn_blocking(move || consent::revoke(&identity.dir)).await??;

        match revoked {
            Revoked::Removed => context
                .ui()
                .success(format!("revoked {} - open a new shell prompt", dir.display())),
            Revoked::Absent => context.ui().status(
                "nothing to revoke",
                format!("{} carries no consent stamp", dir.display()),
            ),
        }
        Ok(ExitCode::SUCCESS)
    }
}
