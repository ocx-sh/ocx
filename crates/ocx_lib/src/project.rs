// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-tier toolchain configuration (`ocx.toml`) and lock (`ocx.lock`).

pub mod compose;
pub mod config;
pub mod error;
pub mod hash;
pub mod hook;
mod internal;
pub mod lock;
pub mod mutate;
pub mod registry;
pub mod resolve;

pub use compose::{Origin, PositionalPackage, ResolvedTool, compose_tool_set, parse_positional};
pub use config::ProjectConfig;
pub use error::{Error, ProjectError, ProjectErrorKind};
pub use hash::{DECLARATION_HASH_VERSION, declaration_hash};
pub use hook::{AppliedSet, MissingState, ProjectState, collect_applied, load_project_state};
pub use lock::{LockMetadata, LockVersion, LockedTool, ProjectLock};
pub use mutate::{add_binding, binding_key, init_project, remove_binding};
pub use registry::ProjectRegistry;
pub use resolve::{ResolveLockOptions, resolve_lock, resolve_lock_partial};

/// Reserved group name for the implicit default group (the top-level
/// `[tools]` table in `ocx.toml`, the `"default"` group key in lock
/// entries, and the JSON key in the declaration-hash canonical form).
///
/// Re-exported from the module-private [`internal::DEFAULT_GROUP`] so CLI
/// callers (`exec`, `pull`, `lock`, `update`, `shell-hook`, `hook-env`, …)
/// share a single source of truth instead of each defining a local
/// `const DEFAULT_GROUP: &str = "default"`.
pub const DEFAULT_GROUP: &str = internal::DEFAULT_GROUP;
