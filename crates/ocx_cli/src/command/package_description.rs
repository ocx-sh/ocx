// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package description` - dispatcher for the catalog-description pair.
//!
//! `push` and `pull` are a writer and a reader of one registry-side object,
//! the `__ocx.desc` tag: README, logo, title, summary and keywords. They are
//! spelled with the tier's own transport verbs — the same `push`/`pull` that
//! move a package — because moving a description to and from a registry is
//! literally what they do.
//!
//! The variants carry no doc comment on purpose: clap renders a variant's own
//! doc as the subcommand's help and ignores the argument struct's when one is
//! present, so the user-facing text lives with the flags it describes, in
//! `package_description_push.rs` and `package_description_pull.rs`.

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
