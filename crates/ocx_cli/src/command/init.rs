// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx init` — create a minimal `ocx.toml` in the current directory.

use std::process::ExitCode;

use clap::Parser;

/// Create a minimal `ocx.toml` in the current directory.
///
/// Writes a skeleton config with a default registry comment and an empty
/// `[tools]` table. Non-interactive by design — add tools with `ocx add`
/// after initialisation.
///
/// Fails if `ocx.toml` already exists in the current directory.
#[derive(Parser, Clone)]
pub struct Init {
    // No flags for v1; init is non-interactive minimal.
}

impl Init {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // `ocx init` bootstraps the project, so it must NOT use the context's
        // project-discovery path (which errors when ocx.toml is absent). Use
        // the raw process cwd instead. The directory variant of the lib API
        // (`init_project_at_default`) creates `<cwd>/ocx.toml` — `ocx init`
        // does not yet expose a `--project=<custom>.toml` ingress.
        let cwd = ocx_lib::env::current_dir()?;

        let toml_path = ocx_lib::project::init_project_at_default(&cwd)?;
        context.ui().success(format!("created {}", toml_path.display()));

        Ok(ExitCode::SUCCESS)
    }
}
