// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Toolchain-tier `ocx status` command.
//!
//! Reads `ocx.toml` and its sibling `ocx.lock` and reports what they say. No
//! network, no advisory lock, no staleness gate, no object-store probe — the
//! command has to answer on exactly the broken project you reach for it with,
//! so an absent or drifted lock is payload rather than an error.
//!
//! Deliberately NOT routed through `load_project_with_lock`: that helper exits
//! 78 on a missing lock and 65 on a stale one, which are the two states status
//! exists to describe.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::{ProjectConfig, ProjectLock};

use crate::api::data::status::StatusReport;

/// Show what `ocx.toml` and `ocx.lock` declare, without resolving anything.
///
/// Reports every declared group with its tool bindings and `[env]` table, each
/// binding's locked platform digests, the `[package."<id>"]` settings, and
/// whether the lock is still current for the declaration.
///
/// Offline and read-only: no registry is contacted, nothing is installed, and
/// neither file is written. A missing or stale `ocx.lock` is reported as such
/// and still exits 0 - use `ocx lock --check` for the CI gate that fails on
/// exactly that condition, and `ocx inspect` for the resolved surface (what
/// each binding resolves to on this host, and what it would put on `PATH`).
///
/// Takes no group or name filter: the report is a keyed object a caller can
/// narrow itself, and a filter here would only hide rows rather than change
/// any answer.
///
/// Exits 64 when no `ocx.toml` is in scope.
#[derive(Parser)]
pub struct Status {}

impl Status {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let (config_path, lock_path) = crate::app::project_context::resolve_project_paths(&context, None).await?;

        let config = ProjectConfig::from_path(&config_path).await?;

        // `from_path` yields `None` for an absent lock. A parse failure (an
        // unsupported `lock_version`, a corrupt file) is caught rather than
        // propagated: it is one of the states this command exists to name, and
        // the declaration half of the report is still perfectly readable.
        let lock = match ProjectLock::from_path(&lock_path).await {
            Ok(lock) => Ok(lock),
            Err(error) => Err(format!("{error}")),
        };

        let report = StatusReport::new(
            &config_path,
            &config,
            lock.as_ref().map(Option::as_ref).map_err(String::clone),
        );
        context.api().report(&report)?;

        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `status` takes no selectors - a `-g` or a NAME must be a usage error,
    /// not a silently-ignored argument.
    #[test]
    fn status_rejects_selectors() {
        assert!(Status::try_parse_from(["status", "-g", "ci"]).is_err());
        assert!(Status::try_parse_from(["status", "go-task"]).is_err());
        assert!(Status::try_parse_from(["status"]).is_ok());
    }
}
