// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Package identity, metadata, versioning, cascade, authoring and publication.

pub mod publisher;

pub mod bin_scan;
pub mod bundle;
pub mod cascade;
pub mod concrete_version;
pub mod dependency_pinning;
pub mod description;
pub mod error;
pub mod info;
pub mod install_info;
pub mod install_status;
pub mod launch;
pub mod libc_lint;
pub mod metadata;
pub mod prune;
pub mod resolved_package;
pub mod tag;
pub mod upgrade_target;
pub mod version;

pub use install_info::InstallInfo;
