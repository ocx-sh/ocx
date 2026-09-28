// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The on-disk layout: three-tier CAS, symlink namespace, package
//! materialisation, shim blobs, local code signing.

pub mod codesign;
pub mod file_structure;
pub mod hardlink;
pub mod reference_manager;
pub mod shim;
