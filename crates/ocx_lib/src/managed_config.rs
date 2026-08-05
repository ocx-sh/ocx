// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Managed-config domain layer — the `[managed]` tier's package plumbing.
//!
//! This is the **domain layer** for the corporate managed-configuration
//! feature (`adr_managed_config_tier.md`, v2 amendment). It is distinct from
//! the **config layer** in `crates/ocx_lib/src/config/managed.rs`, which
//! defines `ManagedConfig`/`ResolvedManagedConfig`/`resolve_managed_config`.
//!
//! ## Module layout
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`persistence`] | [`fetch_managed_config`], [`persist_managed_config`], their error taxonomies |
//! | [`publish`] | [`validate_managed_config_payload`], [`publish_managed_config`] (`ocx config push`) |
//! | [`preview`] | [`preview_managed_config`] (`ocx config test`) — validate + merge preview, writes nothing |
//! | [`pause`] | [`read_pause`], [`write_pause`], [`clear_pause`] (`ocx config update --pause/--resume`) |
//!
//! ## Wire shape (v2 — config-as-package)
//!
//! Managed config is an **ordinary ocx package** whose content is a single
//! `config.toml` (published by `ocx config push`, see [`publish`]): image
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
//! have. See [`ManagedConfigSnapshot`](crate::config::managed::ManagedConfigSnapshot).

pub mod pause;
pub mod persistence;
pub mod preview;
pub mod publish;
#[cfg(test)]
pub(crate) mod test_support;

/// The snapshot type is a **config-tier data model** owned by
/// [`crate::config::managed`]; re-exported here for API stability so consumers
/// of the domain layer keep reaching it at `managed_config::ManagedConfigSnapshot`.
pub use crate::config::managed::ManagedConfigSnapshot;
pub use pause::{MAX_PAUSE_INTERVAL, ManagedConfigPause, clear_pause, read_pause, write_pause};
pub use persistence::{
    FetchedManagedConfig, ManagedConfigFetchError, ManagedConfigPersistError, ManagedConfigUpdateError,
    fetch_managed_config, persist_managed_config, probe_managed_config_digest, read_managed_config_snapshot,
    read_managed_config_snapshot_at,
};
pub use preview::{ManagedConfigPreview, preview_managed_config};
pub use publish::{
    ManagedConfigPublishError, ManagedConfigPublishOptions, publish_managed_config, validate_managed_config_payload,
};

/// Maximum allowed size (bytes) for the managed-config payload — enforced on
/// the declared layer size, the streamed blob bytes, AND the decompressed
/// archive bytes (fetch side), plus the publish-side payload validation.
/// Mirrors the existing local `config.toml` size cap (`MAX_CONFIG_SIZE` in
/// `config/loader.rs`).
pub const MAX_MANAGED_CONFIG_BYTES: u64 = 64 * 1024;
