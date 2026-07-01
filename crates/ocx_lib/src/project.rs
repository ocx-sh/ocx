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
pub mod mutation;
mod project_lock;
pub mod registry;
pub mod resolve;

pub use compose::{
    Origin, PositionalPackage, ResolvedTool, SelectedTool, ToolSource, compose_tool_set, expand_all_keyword,
    host_leaf_identifier, parse_positional, resolve_selected_tools, select_tool_set,
};
pub use config::{PackageSettings, ProjectConfig};
pub use error::{Error, ProjectError, ProjectErrorKind};
pub use hash::{DECLARATION_HASH_VERSION, declaration_hash};
pub use hook::{MissingState, ProjectState, load_project_state};
pub use lock::{
    LockMetadata, LockVersion, LockedResolution, LockedTool, ProjectLock, ProjectLockV2, resolutions_content_equal,
};
pub use mutate::{
    add_binding, add_binding_in_memory, binding_key, init_project, init_project_at_default, remove_binding,
    remove_binding_in_memory,
};
pub use mutation::{MutationCommit, MutationGuard, StagedMutation};
pub use project_lock::{acquire_project_lock, acquire_project_lock_for_file};
pub use registry::ProjectRegistry;
pub use resolve::{ResolveLockOptions, lookup_host_leaf, resolve_lock, resolve_lock_touched};

/// Reserved group name for the implicit default group (the top-level
/// `[tools]` table in `ocx.toml`, the `"default"` group key in lock
/// entries, and the JSON key in the declaration-hash canonical form).
///
/// Re-exported from the module-private [`internal::DEFAULT_GROUP`] so CLI
/// callers (`exec`, `pull`, `lock`, `update`, `shell-hook`, `hook-env`, …)
/// share a single source of truth instead of each defining a local
/// `const DEFAULT_GROUP: &str = "default"`.
pub const DEFAULT_GROUP: &str = internal::DEFAULT_GROUP;

/// Reserved CLI keyword that expands to the union of the default group and
/// every named group declared in `ocx.toml` when passed to `-g`.
///
/// Re-exported from the module-private [`internal::ALL_GROUP`]. Project-tier
/// commands (`run`, `pull`, `lock`, `update`) accept `-g all` and expand it
/// at the CLI layer via [`compose::expand_all_keyword`] before calling
/// [`compose_tool_set`]. `[group.all]` in `ocx.toml` is rejected at parse
/// time; `--group all` in mutating commands is rejected at mutate time.
pub const ALL_GROUP: &str = internal::ALL_GROUP;
