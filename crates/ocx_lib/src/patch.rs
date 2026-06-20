// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Patch domain — descriptor types, flat matcher, and blob persistence primitive.
//!
//! This is the **domain layer** for the infrastructure-patches feature
//! (`adr_infrastructure_patches.md`, milestone #111, Phase 2 / issue #113).
//! It is distinct from the **config layer** in `crates/ocx_lib/src/config/patch.rs`,
//! which defines `PatchConfig`/`ResolvedPatchConfig`/`resolve_patch_config`.
//!
//! ## Module layout
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`descriptor`] | [`PatchDescriptor`], [`PatchRule`], [`PatchDescriptorVersion`], media-type constants, [`CompanionEntry`] |
//! | [`matcher`]    | [`glob_match`] — flat glob (spans `/`, `:`, `@`) |
//! | [`persistence`] | [`persist_patch_descriptor`], [`fetch_patch_descriptor_blobs`], [`PersistedDigests`] |
//! | [`error`]      | [`PatchError`] |
//!
//! ## Phase scope
//!
//! Phase 2 (this module) delivers:
//! - Descriptor types + media-type constants.
//! - Unified flat matcher (no `glob` crate dep).
//! - Persistence primitive (`persist_patch_descriptor`).
//!
//! **Not in scope:** `SitePatchResolver`, compose overlay, discovery wiring,
//! GC root seeding (Phase 3–5). The network fetch (`fetch_patch_descriptor_blobs`)
//! is exercised by acceptance tests once discovery is wired; the pure
//! persistence primitive (`persist_patch_descriptor`) is unit-tested here.

pub mod descriptor;
pub mod error;
pub mod matcher;
pub mod persistence;

pub use descriptor::{
    CompanionEntry, PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE, PATCH_MANIFEST_ARTIFACT_TYPE, PatchDescriptor,
    PatchDescriptorVersion, PatchRule,
};
pub use error::PatchError;
pub use matcher::glob_match;
pub use persistence::{
    FetchedDescriptorBlobs, PersistedDigests, fetch_patch_descriptor_blobs, persist_patch_descriptor,
};
