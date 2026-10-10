// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx shell allow` — grant a project's shell activation explicitly.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::activation::ProjectIdentity;
use ocx_project::consent::{self, Recorded};

use crate::app::CliRefusal;
use crate::app::project_context::resolve_project_paths;

/// The `ocx shell allow` arguments; its help text lives on `Shell::Allow`.
#[derive(Parser)]
pub struct ShellAllow {
    /// The directory whose project to consent to (default: the current one)
    ///
    /// The walk upward from this directory is the same one a shell prompt
    /// makes, so this consents to exactly the project a prompt would activate.
    /// A `--project` or `--global` selector still takes precedence, as
    /// everywhere.
    path: Option<PathBuf>,
}

impl ShellAllow {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let (config_path, lock_path) = resolve_project_paths(&context, self.path.as_deref()).await?;
        // A `PATH`, `--project` or `--global` targets another project, whose stamp this shell's prompt never reads.
        let is_this_shells_project = self.path.is_none() && context.project_path().is_none() && !context.global();
        // Must be the canonical dir the prompt keys on, or the stamp never matches.
        let identity = ProjectIdentity::resolve(config_path).await?;

        // An absent or unparseable lock stamps an empty set, not a refusal:
        // `ocx shell state` already names why it is inert.
        let sources = ocx_project::ProjectLock::from_path(&lock_path)
            .await
            .ok()
            .flatten()
            .as_ref()
            .map(consent::lock_sources)
            .unwrap_or_default();

        let dir = identity.dir.clone();
        let recorded = tokio::task::spawn_blocking(move || consent::record(&identity.dir, &sources)).await??;

        match recorded {
            Recorded::Stamped => {
                let when = if is_this_shells_project {
                    "active at the next prompt in this shell"
                } else {
                    "active at the next prompt there"
                };
                context.ui().success(format!("consented to {} - {when}", dir.display()));
                Ok(ExitCode::SUCCESS)
            }
            Recorded::OcxHomeNeedsNoStamp => Err(CliRefusal::OcxHomeNeedsNoConsent(format!(
                "{} is the ocx home; the global toolchain is always active and carries no consent stamp",
                dir.display()
            ))
            .into()),
        }
    }
}
