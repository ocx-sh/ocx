// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The managed-config tier (`adr_managed_config_tier.md`): on-disk layout ([`paths`]) and
//! fetch, persist and pause operations ([`persistence`], [`pause`]) over [`crate::managed`].

pub mod paths;
pub mod pause;
pub mod persistence;
#[cfg(any(test, feature = "__testing"))]
pub mod test_support;

pub use paths::ManagedConfigPaths;

pub use crate::managed::ManagedConfigSnapshot;
pub use pause::{MAX_PAUSE_INTERVAL, ManagedConfigPause, clear_pause, read_pause, write_pause};
pub use persistence::{
    FetchedManagedConfig, ManagedConfigFetchError, ManagedConfigPersistError, ManagedConfigUpdateError,
    fetch_managed_config, persist_managed_config, probe_managed_config_digest, read_managed_config_snapshot,
    read_managed_config_snapshot_at,
};

/// Maximum managed-config payload size in bytes, enforced on the declared layer size, the
/// streamed blob, the decompressed archive and the publish-side payload.
pub const MAX_MANAGED_CONFIG_BYTES: u64 = 64 * 1024;
