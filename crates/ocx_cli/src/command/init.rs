// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx init` — create a minimal `ocx.toml` in the selected project, in the
//! current directory when nothing is selected, or in `$OCX_HOME` under the
//! root `--global` selector.

use std::process::ExitCode;

use clap::Parser;

use crate::app::project_context::record_activation_consent_over;

/// Create a minimal `ocx.toml` in the current directory.
///
/// Writes a skeleton config with a default registry comment and an empty
/// `[tools]` table. Non-interactive by design — add bindings with `ocx add`
/// after initialisation.
///
/// `ocx --global init` writes `$OCX_HOME/ocx.toml` instead — the same file
/// every other `--global` command resolves. `--project <dir>` (or
/// `OCX_PROJECT`) scaffolds `<dir>/ocx.toml`.
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
        // whose CWD walk would resolve a *parent* project). The selectors are
        // read directly instead, in the loader's own precedence:
        //
        // - `--global` / `OCX_GLOBAL` → `$OCX_HOME/ocx.toml`, via the one
        //   spelling every other global-tier site uses
        //   (`ProjectConfig::global_manifest_path`). Dropping the selector here
        //   made `ocx --global init` scaffold the CWD while `ocx --global add`
        //   and `ocx --global status` read `$OCX_HOME` (ocx-sh/ocx#443).
        // - `--project` / `OCX_PROJECT` → the loader's own reading of that
        //   selection (`ConfigLoader::explicit_project`, so there is one
        //   ladder, not two): a directory is the project it governs (RUL-55)
        //   and gets `<dir>/ocx.toml`. The selection always exists by now —
        //   `Context::try_init` already refused an absent one with 79 — so
        //   the other arm is an existing file, which `init_project` refuses as
        //   `ConfigAlreadyExists` (64), the same answer a bare `ocx init` gives
        //   on a scaffolded directory. Dropping the selector here scaffolded
        //   the CWD and exited 0 (ocx-sh/ocx#475).
        // - otherwise → `<cwd>/ocx.toml`.
        let toml_path = if context.global() {
            let home = context.file_structure().root();
            ocx_project::init_project(&ocx_project::ProjectConfig::global_manifest_path(home))?
        } else if let Some(selected) = ocx_config::loader::ConfigLoader::explicit_project(context.project_path()) {
            if selected.is_dir() {
                ocx_project::init_project_at_default(&selected)?
            } else {
                ocx_project::init_project(&selected)?
            }
        } else {
            let cwd = ocx_util::env::current_dir()?;
            ocx_project::init_project_at_default(&cwd)?
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
