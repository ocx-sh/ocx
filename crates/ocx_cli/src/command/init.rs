// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx init`: create a minimal `ocx.toml`.

use std::process::ExitCode;

use clap::Parser;

use crate::app::project_context::record_activation_consent_over;

/// Create a minimal `ocx.toml` in the current directory.
///
/// Writes a skeleton config with a default registry comment and an empty
/// `[tools]` table; non-interactive, so bindings come later via `ocx add`.
/// `ocx --global init` writes `$OCX_HOME/ocx.toml` instead, and
/// `--project <dir>` (or `OCX_PROJECT`) scaffolds `<dir>/ocx.toml`. Records a
/// shell-activation consent stamp for the new project unless `--no-consent`
/// (`ocx shell allow` grants it later). Fails if `ocx.toml` already exists.
#[derive(Parser, Clone)]
pub struct Init {
    // A `///` here renders nowhere: clap renders the flattened struct's own field docs.
    #[clap(flatten)]
    consent: crate::options::Consent,
}

impl Init {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Selectors read directly: the discovery path errors without `ocx.toml`, and its CWD walk would
        // resolve a parent project instead of scaffolding here.
        let toml_path = if context.global() {
            // Dropping this arm would scaffold the CWD instead of `$OCX_HOME`.
            let home = context.file_structure().root();
            ocx_project::init_project(&ocx_project::ProjectConfig::global_manifest_path(home))?
        } else if let Some(selected) = ocx_config::loader::ConfigLoader::explicit_project(context.project_path()) {
            // `--project <dir>` scaffolds `<dir>/ocx.toml`; dropping this arm would scaffold the CWD.
            if selected.is_dir() {
                ocx_project::init_project_at_default(&selected)?
            } else {
                ocx_project::init_project(&selected)?
            }
        } else {
            let cwd = ocx_util::env::current_dir()?;
            ocx_project::init_project_at_default(&cwd)?
        };

        // Stamped by default so the next prompt does not report the new project inert; the tri-state
        // travels raw so `OCX_NO_CONSENT` still applies.
        record_activation_consent_over(&toml_path, std::collections::BTreeSet::new(), self.consent.explicit()).await;

        context.ui().success(format!("created {}", toml_path.display()));

        Ok(ExitCode::SUCCESS)
    }
}
