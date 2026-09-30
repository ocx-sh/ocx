// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Patch domain — descriptor types, flat matcher, and blob persistence primitive
//! (`adr_infrastructure_patches.md`); the config layer is `ocx_config::patch`.

pub mod descriptor;
pub mod error;
pub mod matcher;
pub mod persistence;
pub mod snapshot;

pub use descriptor::{
    CompanionEntry, PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE, PATCH_MANIFEST_ARTIFACT_TYPE, PatchDescriptor,
    PatchDescriptorVersion, PatchRule,
};
pub use error::PatchError;
pub use matcher::glob_match;
pub use persistence::{
    FetchedDescriptorBlobs, PersistedDigests, fetch_patch_descriptor_blobs, persist_patch_descriptor,
    probe_patch_descriptor_digest,
};
pub use snapshot::{PATCH_SNAPSHOT_FILE, PatchSnapshot, SnapshotVersion};
