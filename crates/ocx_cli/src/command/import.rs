// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::ProjectImportRequest;

use crate::api::data::import::ImportReport;
use crate::app::project_context::{ensure_group_segments_nonempty, load_project_with_lock};
use crate::{conventions, options};

/// Assembles the project lock's packages from an export directory, offline.
#[derive(Parser, Clone)]
pub struct Import {
    /// The export directory: the one holding `ocx-export.json`, not its `index` or `blobs` subdirectory.
    #[arg(value_name = "DIR")]
    input: PathBuf,

    #[clap(flatten)]
    groups: options::GroupSelection,

    /// Import only this platform; repeat for several. Defaults to the host platform.
    #[arg(short = 'p', long = "platform", value_name = "PLATFORM")]
    platforms: Vec<ocx_oci::Platform>,
}

impl Import {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        ensure_group_segments_nonempty(self.groups.names())?;
        let project = load_project_with_lock(&context).await?;
        // No `-g` is the default group, as for `ocx pull`.
        let groups = self.groups.effective(&project.config)?;
        let platforms = if self.platforms.is_empty() {
            vec![conventions::platform_or_default(None)]
        } else {
            self.platforms.clone()
        };
        let lock_pins = super::export::lock_pins(&project.lock.tools, &groups, &platforms)?;
        // `--global` renders `$OCX_HOME`'s toolchain and registers no project.
        let project_dir = if context.global() {
            None
        } else {
            project.config_path.parent().map(std::path::Path::to_path_buf)
        };
        let render_platform = platforms
            .first()
            .cloned()
            .unwrap_or_else(|| conventions::platform_or_default(None));
        let outcome = context
            .manager()
            .import_project(ProjectImportRequest {
                input: conventions::absolute_dir(&self.input)?,
                lock_pins,
                platforms,
                project_dir,
            })
            .await?;
        // After the import, as a pull renders after its lock re-save.
        super::pull::render_and_warn(&context, &project, &groups, &render_platform, false).await;
        context.api().report(&ImportReport::from_outcome(outcome))?;
        Ok(ExitCode::SUCCESS)
    }
}
