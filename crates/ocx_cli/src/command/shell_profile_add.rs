// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{oci, profile};

use crate::api::data::profile_added::{ProfileAdded, ProfileAddedEntry, ProfileAddedStatus};
use crate::conventions::platforms_or_default;
use crate::options;

/// Add one or more packages to the shell profile.
///
/// Packages added to the profile will have their environment variables loaded
/// into every new shell session via `ocx shell profile load`.
///
/// By default, packages are resolved via the `candidates/{tag}` symlink (pinned
/// to a specific tag). Use `--current` to resolve via the floating `current`
/// symlink, or `--content` to use the content-addressed object store path.
///
/// Packages that are not yet installed will be auto-installed.
#[derive(Parser)]
pub struct ShellProfileAdd {
    /// Resolve via the `candidates/{tag}` symlink (pinned to a specific tag).
    #[clap(long, conflicts_with_all = ["current", "content"])]
    candidate: bool,

    /// Resolve via the `current` symlink (floating pointer set by `ocx select`).
    #[clap(long, conflicts_with_all = ["candidate", "content"])]
    current: bool,

    /// Resolve via the content-addressed object store path.
    /// The path changes whenever the package is reinstalled at a different version.
    #[clap(long, conflicts_with_all = ["candidate", "current"])]
    content: bool,

    /// Target platforms to consider when resolving packages.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM")]
    platforms: Vec<oci::Platform>,

    /// Package identifiers to add to the profile.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl ShellProfileAdd {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let mode = if self.current {
            profile::ProfileMode::Current
        } else if self.content {
            profile::ProfileMode::Content
        } else {
            // Default: candidate
            profile::ProfileMode::Candidate
        };

        let default_registry = context.default_registry();
        let identifiers = options::Identifier::transform_all(self.packages.clone(), default_registry)?;

        // Auto-install: find_or_install_all handles both already-installed and missing packages
        let platforms = platforms_or_default(&self.platforms);
        let infos = context
            .manager()
            .find_or_install_all(identifiers.clone(), platforms)
            .await?;

        let inputs: Vec<profile::ProfileAddInput> = identifiers
            .into_iter()
            .zip(infos)
            .map(|(identifier, info)| {
                // For content mode, bake the digest into the identifier for direct
                // object store resolution without re-querying the index.
                let identifier = if mode == profile::ProfileMode::Content {
                    identifier.clone_with_digest(info.identifier.digest())
                } else {
                    identifier
                };
                profile::ProfileAddInput {
                    identifier,
                    mode,
                    content_path: info.content,
                }
            })
            .collect();

        let outcomes = context.manager().profile().add_all(inputs)?;

        let entries: Vec<_> = self
            .packages
            .iter()
            .zip(outcomes)
            .map(|(raw, outcome)| {
                let id_str = raw.to_string();
                let (status, previous_mode) = match outcome {
                    profile::AddOutcome::Added => (ProfileAddedStatus::Added, None),
                    profile::AddOutcome::Updated { previous_mode } => {
                        let changed = (previous_mode != mode).then_some(previous_mode);
                        (ProfileAddedStatus::Updated, changed)
                    }
                };
                ProfileAddedEntry::new(id_str, mode, status, previous_mode)
            })
            .collect();

        context.api().report(&ProfileAdded::new(entries))?;
        Ok(ExitCode::SUCCESS)
    }
}
