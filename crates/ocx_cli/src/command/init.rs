// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx init` — create a minimal `ocx.toml` in the current directory, or in
//! `$OCX_HOME` under the root `--global` selector.

use std::process::ExitCode;

use clap::Parser;

use crate::app::project_context::record_activation_consent_over;

/// Create a minimal `ocx.toml` in the current directory.
///
/// Writes a skeleton config with a default registry comment and an empty
/// `[tools]` table. Non-interactive by design — add tools with `ocx add`
/// after initialisation.
///
/// `ocx --global init` writes `$OCX_HOME/ocx.toml` instead — the same file
/// every other `--global` command resolves.
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
        // project-discovery path (which errors when ocx.toml is absent, and
        // whose CWD walk would resolve a *parent* project). The two tiers are
        // selected directly instead:
        //
        // - `--global` / `OCX_GLOBAL` → `$OCX_HOME/ocx.toml`, via the one
        //   spelling every other global-tier site uses
        //   (`ProjectConfig::global_manifest_path`). Dropping the selector here
        //   made `ocx --global init` scaffold the CWD while `ocx --global add`
        //   and `ocx --global status` read `$OCX_HOME` (ocx-sh/ocx#443).
        // - otherwise → `<cwd>/ocx.toml`, via the directory variant of the lib
        //   API. `ocx init` does not yet expose a `--project=<custom>.toml`
        //   ingress, and `--project` is exclusive with `--global` anyway.
        let toml_path = if context.global() {
            let home = context.file_structure().root();
            ocx_lib::project::init_project(&ocx_lib::project::ProjectConfig::global_manifest_path(home))?
        } else {
            let cwd = ocx_lib::env::current_dir()?;
            ocx_lib::project::init_project_at_default(&cwd)?
        };

        // Creating an `ocx.toml` in a directory is at least as deliberate a
        // gesture as the `ocx add` that already stamps consent, so the default
        // is to stamp — otherwise the very next prompt would report the project
        // it just created as inert. The empty source set is the honest record
        // for a project with no lock; the first `ocx add` re-records.
        //
        // The tri-state travels rather than a resolved bool, so the flag/env
        // ladder has one definition, at the seam: deciding `enabled(true)` here
        // would answer for a user who typed nothing and leave `OCX_NO_CONSENT`
        // unheard by the one command that scaffolds a project non-interactively
        // (ocx-sh/ocx#400).
        //
        // A-44 keeps `ocx init` inside `$OCX_HOME` a no-op rather than an error:
        // `consent::record` answers `OcxHomeNeedsNoStamp` there, and the stamp
        // is best-effort in every direction anyway.
        record_activation_consent_over(&toml_path, std::collections::BTreeSet::new(), self.consent.explicit()).await;

        context.ui().success(format!("created {}", toml_path.display()));

        Ok(ExitCode::SUCCESS)
    }
}
