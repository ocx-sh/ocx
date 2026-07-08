// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    managed_config::{ManagedConfigPublishOptions, publish_managed_config},
    oci,
    publisher::Publisher,
};

use crate::options;

/// Arguments for `ocx config push`.
#[derive(Parser)]
pub struct ConfigPushArgs {
    /// Identifier under which the config is published (e.g. `corp/ocx-config:user-1.4.2`).
    #[clap(short = 'i', long = "identifier", required = true)]
    identifier: options::Identifier,

    /// Update rolling variant tags derived from the version tag.
    ///
    /// Pushing `user-1.4.2` also updates `user-1.4`, `user-1`, and `user`, so
    /// fleets adopting a shorter tag pick up the new version automatically.
    #[clap(long = "cascade", short = 'c')]
    cascade: bool,

    /// The repository does not exist in the registry yet.
    ///
    /// Skips checks that require an existing repository (such as listing
    /// current tags for `--cascade`).
    #[clap(long = "new", short = 'n')]
    new: bool,

    /// Platform entry written into the package index. Defaults to `any/any`.
    ///
    /// `ocx config update` only consumes the platform-independent `any/any`
    /// entry; keep the default unless you know the consumer differs.
    #[clap(short, long, default_value = "any/any")]
    platform: oci::Platform,

    /// The config file to publish (its content is staged as `config.toml`).
    config: std::path::PathBuf,
}

impl ConfigPushArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        let publisher = Publisher::new(context.remote_client()?.clone());
        publisher.ensure_auth(&identifier).await?;

        let outcome = publish_managed_config(
            &publisher,
            &identifier,
            &self.config,
            ManagedConfigPublishOptions {
                cascade: self.cascade,
                new: self.new,
                platform: self.platform.clone(),
            },
        )
        .await?;

        // The reported digest doubles as the operator's TOFU signal: it is
        // the value a digest-pinned seed or `ocx config update --check`
        // compares against.
        context.api().report(&crate::api::data::push::PushReport::new(
            identifier.to_string(),
            outcome.manifest_digest.to_string(),
            outcome.cascade_tags,
            outcome.layer_counts,
        ))?;

        Ok(ExitCode::SUCCESS)
    }
}
