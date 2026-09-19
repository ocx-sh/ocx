// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The managed-config tier: its on-disk layout and its domain operations.
//!
//! The corporate managed-configuration feature (`adr_managed_config_tier.md`,
//! v2 amendment). Inside `ocx_lib` this was **two** modules that could not
//! share a name — `config::managed_config` for the layout and
//! `managed_config` for the operations. Both are `ocx_config`, and a crate has
//! one `managed_config`, so they are one module here. The halves are still
//! distinct and stay named as such: [`paths`] is the layout, the rest is the
//! domain layer, and the data model they both serve is [`crate::managed`]
//! (`ManagedConfig` / `ResolvedManagedConfig` / `resolve_managed_config`).
//!
//! ## Module layout
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`paths`] | [`ManagedConfigPaths`] — where the snapshot and the pause file live |
//! | [`persistence`] | [`fetch_managed_config`], [`persist_managed_config`], their error taxonomies |
//! | [`pause`] | [`read_pause`], [`write_pause`], [`clear_pause`] (`ocx config update --pause/--resume`) |
//!
//! Building and pushing the package — `ocx config push` and `ocx config
//! test` — is `ocx_package_manager::managed_config`: it composes a package and
//! signs it, which this tier must not be able to name. Not a doc link: that
//! crate sits above this one and naming it here would be the very edge the
//! sentence forbids.
//!
//! ## Wire shape (v2 — config-as-package)
//!
//! Managed config is an **ordinary ocx package** whose content is a single
//! `config.toml` (published by `ocx config push`, i.e.
//! `ocx_package_manager::managed_config`): image
//! index with an `any/any` entry → image manifest → tar+gzip layer. No custom
//! artifact type. Fetch caps: [`MAX_MANAGED_CONFIG_BYTES`] enforced three
//! ways (declared size, streamed bytes, decompressed bytes) — see
//! [`persistence`].
//!
//! ## Snapshot format (Decision D; v2 adds optional `tag`)
//!
//! A **single file** `state/managed-config/snapshot.json` —
//! `{source, digest, fetched_at, config: "<raw TOML string>"}` — one
//! temp+rename write is atomic, closing both the two-file crash window and the
//! concurrent-writer torn-state race a content/provenance sidecar pair would
//! have. See `ManagedConfigSnapshot`.

pub mod paths;
pub mod pause;
pub mod persistence;
#[cfg(any(test, feature = "__testing"))]
pub mod test_support;

pub use paths::ManagedConfigPaths;

/// The snapshot type is a **config-tier data model** owned by
/// [`crate::managed`]; re-exported here for API stability so consumers of the
/// domain layer keep reaching it at `managed_config::ManagedConfigSnapshot`.
pub use crate::managed::ManagedConfigSnapshot;
pub use pause::{MAX_PAUSE_INTERVAL, ManagedConfigPause, clear_pause, read_pause, write_pause};
pub use persistence::{
    FetchedManagedConfig, ManagedConfigFetchError, ManagedConfigPersistError, ManagedConfigUpdateError,
    fetch_managed_config, persist_managed_config, probe_managed_config_digest, read_managed_config_snapshot,
    read_managed_config_snapshot_at,
};

/// Maximum allowed size (bytes) for the managed-config payload — enforced on
/// the declared layer size, the streamed blob bytes, AND the decompressed
/// archive bytes (fetch side), plus the publish-side payload validation.
/// Mirrors the existing local `config.toml` size cap (`MAX_CONFIG_SIZE` in
/// `loader.rs`).
pub const MAX_MANAGED_CONFIG_BYTES: u64 = 64 * 1024;
