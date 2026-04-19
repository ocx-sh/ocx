// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-tier toolchain configuration (`ocx.toml`) and lock (`ocx.lock`).

pub mod config;
pub mod error;
pub mod hash;
mod internal;
pub mod lock;

pub use config::ProjectConfig;
pub use error::{Error, ProjectError, ProjectErrorKind};
pub use hash::{DECLARATION_HASH_VERSION, declaration_hash};
pub use lock::{LockMetadata, LockVersion, LockedTool, ProjectLock};
