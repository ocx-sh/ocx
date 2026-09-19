// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Package identity, metadata, versioning, cascade, authoring and publication.
//!
//! What a package *is* — the metadata document and its validation, the version
//! grammar, the binary and libc scans, the description artifact — and the write
//! half that puts one in a registry ([`publisher`]). The crate root is the
//! promoted `package` module, so a type keeps the path its callers already
//! spell minus the `ocx_lib::package::` prefix.

pub mod publisher;

pub mod bin_scan;
pub mod bundle;
pub mod cascade;
pub mod dependency_pinning;
pub mod description;
pub mod error;
pub mod info;
pub mod install_info;
pub mod install_status;
pub mod libc_lint;
pub mod metadata;
pub mod resolved_package;
pub mod tag;
pub mod version;

pub use install_info::InstallInfo;
