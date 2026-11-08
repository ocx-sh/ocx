// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx shell revoke` — withdraw a project's consent stamp.
//!
//! Idempotent by contract: revoking an unstamped project exits 0.

use std::path::{Path, PathBuf};
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
            Revoked::Removed => context.ui().success(revoked_notice(&dir)),
            Revoked::Absent => context.ui().status(
                "nothing to revoke",
                format!("{} carries no consent stamp", dir.display()),
            ),
        }
        Ok(ExitCode::SUCCESS)
    }
}

/// The success line after a stamp is removed.
fn revoked_notice(dir: &Path) -> String {
    format!(
        "revoked {} - the next prompt drops it; an `ocx shell` session keeps the layer it started with",
        dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell opened by `ocx shell` keeps the layer it was started with, so "open a new shell prompt"
    /// alone promises a revert that never happens there; the line must name that shell.
    #[test]
    fn revoked_notice_holds_in_a_pinned_shell() {
        let notice = revoked_notice(Path::new("/work/acme"));

        assert!(notice.contains("/work/acme"), "names the project; got {notice:?}");
        assert!(
            notice.contains("ocx shell"),
            "must say a pinned `ocx shell` session keeps its layer; got {notice:?}"
        );
        assert!(
            notice.is_ascii() && !notice.contains('\n'),
            "one ASCII line; got {notice:?}"
        );
    }
}
