// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx shell allow` — grant a project's shell activation explicitly.
//!
//! The stamp already existed; what did not was a way to *ask* for one. Six
//! mutating commands (`add`, `remove`, `lock`, `update`, `pull`, `run`) wrote
//! `state/projects/<key>/consent.json` as a side effect, so consent appeared
//! from nowhere and outranked the `[shell.consent]` table the user reads. That
//! silent grant is deliberate and stays — running a mutating command in a
//! directory *is* consent — but a grant with no gesture behind it and no way
//! to see or undo it is the defect. This is the gesture; `ocx shell revoke`
//! is the undo, and `ocx shell state` now names which clause activated.
//!
//! **A-44 — `$OCX_HOME` is refused.** The ocx home toolchain is always
//! consented and never carries a stamp. The guard lives in
//! [`ocx_lib::project::consent::record`], one point every writer routes
//! through; this command reads the answer it returns rather than re-testing
//! the predicate, so there is exactly one place the invariant is enforced.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::activation::ProjectIdentity;
use ocx_lib::cli;
use ocx_lib::project::consent::{self, Recorded};

use crate::app::CommandError;
use crate::app::project_context::resolve_project_paths;

/// The `ocx shell allow` arguments.
///
/// The user-facing description lives on the `Shell::Allow` variant, which is
/// the surface clap renders as this subcommand's help; a doc here would be
/// rustdoc-only.
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
        // A-30's canonical directory, through the one helper that derives it —
        // the stamp's identity has to be the same value the prompt keys on.
        let identity = ProjectIdentity::resolve(config_path).await?;

        // What the lock resolves from *now*. An absent or unparseable lock
        // yields an empty set rather than a refusal: a project with no lock is
        // inert for a reason `ocx shell state` already names, and refusing the
        // consent gesture over it would be a second, less informative sentence
        // for the same state.
        let sources = ocx_lib::project::ProjectLock::from_path(&lock_path)
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
                context
                    .ui()
                    .success(format!("consented to {} - open a new shell prompt", dir.display()));
                Ok(ExitCode::SUCCESS)
            }
            // Not a write failure: A-44 makes the ocx home permanently
            // consented, so there is nothing here to grant. Saying so beats
            // reporting a stamp that was never written.
            Recorded::OcxHomeNeedsNoStamp => Err(CommandError::new(
                format!(
                    "{} is the ocx home; the global toolchain is always active and carries no consent stamp",
                    dir.display()
                ),
                cli::ExitCode::UsageError,
            )
            .into()),
        }
    }
}
