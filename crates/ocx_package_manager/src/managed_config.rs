// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Building the managed-config package — `ocx config push` and `ocx config test`.
//!
//! Publishing a managed config is a **package operation**, not a configuration
//! one: it reads a candidate `config.toml`, bundles it as an ordinary ocx
//! package, optionally signs it and pushes it to a registry. Doing that needs
//! [`ocx_package`], [`ocx_package::publisher`] and `ocx_oci::{sign, verify}` at
//! once — three tiers the layer that merely *parses* `[managed]` must never
//! name. So these two modules sit beside `tasks::patch_publish`, among the
//! other operations that compose a package, rather than beside the snapshot
//! reader they feed.
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`publish`] | [`validate_managed_config_payload`], [`publish_managed_config`] (`ocx config push`) |
//! | [`preview`] | [`preview_managed_config`] (`ocx config test`) — validate + merge preview, writes nothing |
//!
//! The rest of the tier stays below: the wire and snapshot data model is
//! `config::managed`, the on-disk layout is `config::managed_config::paths`
//! (both inside the crate-private `config` module), and the fetch/persist/pause half —
//! together with the [`MAX_MANAGED_CONFIG_BYTES`](ocx_config::managed_config::MAX_MANAGED_CONFIG_BYTES)
//! cap both halves enforce — is [`ocx_config::managed_config`].

pub mod preview;
pub mod publish;

pub use preview::{ManagedConfigPreview, preview_managed_config};
pub use publish::{
    ManagedConfigPublishError, ManagedConfigPublishOptions, publish_managed_config, read_candidate_payload,
    validate_managed_config_payload,
};
