// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The on-disk layout: three-tier CAS, symlink namespace, package
//! materialisation, shim blobs, local code signing.
//!
//! Everything under `$OCX_HOME` that is not the index lives here. The three
//! content-addressed tiers — raw blobs, extracted layers, assembled packages —
//! plus the mutable symlink namespace on top of them, the ephemeral state and
//! temp roots, the toolchain link tree, and the two mechanisms that materialise
//! a package's content into a tree a user can execute: hardlinks and, on
//! Windows, the committed launcher shim blobs.
//!
//! Five modules, because the crate root is minted rather than promoted from one
//! of them: [`file_structure`] is the composite root and the four others are the
//! primitives it and its callers reach for directly.
//!
//! The crate knows nothing about registries, manifests or the package
//! *metadata* it stores — it is handed paths and bytes. That is why it sits
//! below `ocx_index` and `ocx_package` in `scripts/crate_map.toml` and depends
//! only on `ocx_config` (where `$OCX_HOME` is resolved), `ocx_oci` (digests and
//! layer layouts, the vocabulary of what it stores) and `ocx_util`.

pub mod codesign;
pub mod file_structure;
pub mod hardlink;
pub mod reference_manager;
pub mod shim;
