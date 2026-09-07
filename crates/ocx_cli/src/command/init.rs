// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx init` — create a minimal `ocx.toml` in the current directory.

use std::process::ExitCode;

use clap::Parser;

use crate::app::project_context::record_activation_consent_over;

/// Create a minimal `ocx.toml` in the current directory.
///
/// Writes a skeleton config with a default registry comment and an empty
/// `[tools]` table. Non-interactive by design — add tools with `ocx add`
/// after initialisation.
///
/// Records a shell-activation consent stamp for the new project, so the next
/// shell prompt in this directory applies it. Pass `--no-consent` to create the
/// project without one; `ocx shell allow` grants it later.
///
/// Fails if `ocx.toml` already exists in the current directory.
#[derive(Parser, Clone)]
pub struct Init {
    // `--consent` (the default) / `--no-consent`. A `///` here would be dead
    // text: clap renders the flattened struct's own field docs, not this one.
    #[clap(flatten)]
    consent: crate::options::Consent,
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

        // Creating an `ocx.toml` in a directory is at least as deliberate a
        // gesture as the `ocx add` that already stamps consent, so the default
        // is to stamp — otherwise the very next prompt would report the project
        // it just created as inert. The empty source set is the honest record
        // for a project with no lock; the first `ocx add` re-records.
        //
        // A-44 keeps `ocx init` inside `$OCX_HOME` a no-op rather than an error:
        // `consent::record` answers `OcxHomeNeedsNoStamp` there, and the stamp
        // is best-effort in every direction anyway.
        if self.consent.enabled(true) {
            record_activation_consent_over(&toml_path, std::collections::BTreeSet::new()).await;
        }

        context.ui().success(format!("created {}", toml_path.display()));

        Ok(ExitCode::SUCCESS)
    }
}
