// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_package::publisher::Publisher;
use ocx_package_manager::managed_config::{ManagedConfigPublishOptions, publish_managed_config};

use crate::command::deprecated;
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
    #[clap(long = "cascade")]
    cascade: bool,

    // 0.7 removal: the `-c` spelling of `--cascade`.
    #[clap(id = deprecated::CONFIG_PUSH_C.arg_id(), short = 'c', hide = true)]
    deprecated_c: bool,

    /// Platform entry written into the package index. Defaults to `any`.
    ///
    /// `ocx config update` only consumes the platform-independent `any`
    /// entry; keep the default unless you know the consumer differs.
    #[clap(short, long, default_value = "any")]
    platform: ocx_oci::Platform,

    /// The config file to publish (its content is staged as `config.toml`).
    config: std::path::PathBuf,
}

impl ConfigPushArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.as_target(context.default_registry())?;

        let publisher = Publisher::new(context.remote_client()?.clone());
        publisher.ensure_auth(&identifier).await?;

        // Resolved before the push so no fallible step runs once the publish has landed.
        let named = self.identifier.with_domain(context.default_registry())?;

        let outcome = publish_managed_config(
            &publisher,
            &identifier,
            &self.config,
            ManagedConfigPublishOptions {
                cascade: self.cascade || self.deprecated_c,
                platform: self.platform.clone(),
            },
        )
        .await?;

        // The reported digest is the operator's TOFU signal for a digest-pinned seed.
        context
            .api()
            .report(&crate::api::data::push::PushReport::from_outcome(named, outcome))?;

        Ok(ExitCode::SUCCESS)
    }
}
