// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::RwLock;

use crate::oci;
use crate::utility::singleflight;

/// Max keys a tag group admits.
///
/// **Sized for the run, not copied from `chained_index.rs`'s `1024`.** That
/// number is for a group whose keys are one refresh's identifiers; these live
/// as long as the maps beside them — for the process, shared across every
/// `box_clone` — and accrue one key per repository (tags) or per tag
/// (digests) a sync walks. At `1024` a sync against a larger registry answers
/// `CapacityExceeded` → `TempFail(75)` on **successes** alone, an exit code
/// promising a retry that nothing inside the process can make succeed.
///
/// A memory backstop, never a throughput limit: the maps these groups front
/// are themselves unbounded, so any run that could reach this ceiling has
/// already stored strictly more beside it.
const TAG_SINGLEFLIGHT_MAX_KEYS: usize = 1 << 20;

/// How long a coalesced caller blocks for the leader's registry call.
///
/// **Derived from the transport underneath, not copied from a sibling group.**
/// The leader's shortest path is two sequential registry requests — the auth
/// exchange [`Client::ensure_auth`](crate::oci::Client) makes when the registry
/// challenges, then the tags listing or manifest `HEAD` itself — and a tags
/// listing paginates, so it is more than two on any repository with a page's
/// worth of tags. Each of those is bounded by
/// [`REGISTRY_READ_TIMEOUT`](crate::oci::client::REGISTRY_READ_TIMEOUT), so at
/// one read timeout a waiter expires while the leader is still inside a call it
/// is entitled to make: `TempFail(75)` for every caller but one, manufactured by
/// the coalescing out of a slow-but-honest link the uncoalesced path would have
/// waited out. Four covers the auth exchange, the call, and pagination headroom
/// — the same shape as `ocx_index.rs`'s `SOURCE_SINGLEFLIGHT_TIMEOUT`, which
/// derives from that transport's per-attempt cap for the same reason.
///
/// **This bounds the waiter, not the leader.** Unlike the index transport, the
/// OCI client has no total deadline at all: `read_timeout` is a per-frame idle
/// bound on the response body and a hard deadline only on everything before the
/// first body frame, so a peer delivering one frame just inside it runs
/// indefinitely. No multiple of it can promise the leader finishes first. The
/// timeout is here so a genuinely wedged leader cannot stall the CLI forever;
/// sizing it above one request's bound is what stops it firing on calls that are
/// merely slow.
const TAG_SINGLEFLIGHT_TIMEOUT: Duration = oci::client::REGISTRY_READ_TIMEOUT.saturating_mul(4);

// The ordering this constant exists for, checked at compile time rather than
// left to a comment: a waiter that gave up before one request's own bound could
// elapse would turn a merely-slow link into a spurious `TempFail(75)` for every
// caller but one — the opposite of what the coalescing is for. `saturating_mul`
// rather than `from_secs(.as_secs() * 4)` because the latter silently discards
// any sub-second component the source bound might grow.
const _: () = assert!(
    TAG_SINGLEFLIGHT_TIMEOUT.as_secs() > oci::client::REGISTRY_READ_TIMEOUT.as_secs(),
    "the coalescing timeout must exceed one registry request's own bound"
);

/// Internal cache for index data.
/// This is shared across all instances of the index, and is used to avoid redundant file reads.
pub struct Cache {
    repositories: RwLock<HashMap<String, Vec<String>>>,
    tags: RwLock<HashMap<oci::Identifier, Vec<String>>>,
    tag_digests: RwLock<HashMap<oci::Identifier, oci::Digest>>,
    /// Coalesces concurrent misses on [`Self::get_tags`]. Beside the map it
    /// guards, never around it — see [`SharedCache`] on why there is no outer
    /// lock, which a group wrapping the whole struct would reintroduce.
    tag_group: singleflight::Group<oci::Identifier, Vec<String>>,
    /// Coalesces concurrent misses on [`Self::get_tag_digest`], same shape.
    tag_digest_group: singleflight::Group<oci::Identifier, oci::Digest>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            repositories: RwLock::default(),
            tags: RwLock::default(),
            tag_digests: RwLock::default(),
            tag_group: singleflight::Group::new(TAG_SINGLEFLIGHT_MAX_KEYS, TAG_SINGLEFLIGHT_TIMEOUT),
            tag_digest_group: singleflight::Group::new(TAG_SINGLEFLIGHT_MAX_KEYS, TAG_SINGLEFLIGHT_TIMEOUT),
        }
    }
}

impl Cache {
    /// The coalescing group guarding [`Self::get_tags`]' misses.
    pub fn tag_group(&self) -> &singleflight::Group<oci::Identifier, Vec<String>> {
        &self.tag_group
    }

    /// The coalescing group guarding [`Self::get_tag_digest`]' misses.
    pub fn tag_digest_group(&self) -> &singleflight::Group<oci::Identifier, oci::Digest> {
        &self.tag_digest_group
    }

    pub async fn get_repositories(&self, registry: &str) -> Option<Vec<String>> {
        self.repositories.read().await.get(registry).cloned()
    }

    pub async fn set_repositories(&self, registry: String, repositories: Vec<String>) {
        self.repositories.write().await.insert(registry, repositories);
    }

    pub async fn get_tags(&self, identifier: &oci::Identifier) -> Option<Vec<String>> {
        self.tags.read().await.get(identifier).cloned()
    }

    pub async fn set_tags(&self, identifier: oci::Identifier, tags: Vec<String>) {
        self.tags.write().await.insert(identifier, tags);
    }

    pub async fn get_tag_digest(&self, identifier: &oci::Identifier) -> Option<oci::Digest> {
        self.tag_digests.read().await.get(identifier).cloned()
    }

    pub async fn set_tag_digest(&self, identifier: &oci::Identifier, digest: oci::Digest) {
        self.tag_digests.write().await.insert(identifier.clone(), digest);
    }
}

/// Shared handle to the in-memory [`OciIndex`](super::OciIndex) cache.
///
/// The inner fields of [`Cache`] are each independently guarded by a
/// `tokio::sync::RwLock`, so the outer handle is a plain `Arc<Cache>` — no
/// outer lock. Wrapping the whole struct in an outer lock would force a
/// writer to hold that outer write-guard across an inner `.await` (`set_tags`
/// / `set_tag_digest` / `set_repositories`); `tokio::sync::RwLock` allows
/// this in principle, but it blocks every other reader for the entire
/// suspension window. `Arc<Cache>` avoids the contention at the type level —
/// readers and writers only contend on the specific sub-map they touch, and
/// no guard is ever held across an `.await` on a foreign lock.
pub type SharedCache = std::sync::Arc<Cache>;
