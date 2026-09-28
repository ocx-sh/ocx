// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package description` - dispatcher for the catalog-description pair.
//!
//! The variants carry no doc comment: clap would render it in place of the argument struct's help.

use std::process::ExitCode;

use clap::Subcommand;

/// Dispatcher for `ocx package description`.
#[derive(Subcommand)]
pub enum DescriptionGroup {
    Push(super::package_description_push::PackageDescriptionPush),
    Pull(super::package_description_pull::PackageDescriptionPull),
}

impl DescriptionGroup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            DescriptionGroup::Push(push) => push.execute(context).await,
            DescriptionGroup::Pull(pull) => pull.execute(context).await,
        }
    }
}
