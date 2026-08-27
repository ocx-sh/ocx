// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx shell revoke` — withdraw a project's consent stamp.
//!
//! The undo half of `ocx shell allow`. Immediately effective: clause 1 of the
//! activation predicate reads the stamp file on every prompt, so the next one
//! is inert unless a `[shell.consent]` grant still covers the project.
//!
//! **Idempotent by contract.** Revoking a project that was never stamped is
//! exit 0 with a line saying so — the requested state is the state that
//! already held, and refusing there would put a failure on the most ordinary
//! outcome of a command whose whole job is to make sure a stamp is gone.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::activation::ProjectIdentity;
use ocx_lib::project::consent::{self, Revoked};

use crate::app::project_context::resolve_project_paths;

/// The `ocx shell revoke` arguments.
///
/// The user-facing description lives on the `Shell::Revoke` variant, which is
/// the surface clap renders as this subcommand's help; a doc here would be
/// rustdoc-only.
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
