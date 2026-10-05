// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The project tier: `ocx.toml`/`ocx.lock`, consent, mutation, per-prompt
//! activation, lazy loading.

pub mod activate;
pub mod ladder;
pub mod lazy;

pub mod compose;
pub mod config;
pub mod consent;
mod document;
pub mod env;
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
pub mod toolchain_home;

pub use compose::{
    Origin, PositionalPackage, ResolvedTool, SelectedTool, ToolSource, check_duplicate_selection, compose_tool_set,
    expand_all_keyword, parse_positional, project_env_entries, resolve_selected_tools, select_tool_set,
};
pub use config::{Group, PackageSettings, ProjectConfig, lazy_mode_for_tool, lazy_mode_ladder_for_tool};
pub use env::{EnvValue, ProjectEnv};
pub use error::{Error, ProjectError, ProjectErrorKind};
pub use hash::{DECLARATION_HASH_VERSION, declaration_hash};
pub use hook::{MissingState, ProjectState, load_project_state};
pub use lock::{
    Binding, BoundTool, LockCurrency, LockDrift, LockMetadata, LockVersion, LockedTool, ProjectLock, eager_per_content,
    first_per_content, locked_tool_content_equal,
};
pub use mutate::{
    add_binding_in_memory, binding_key, init_project, init_project_at_default, remove_binding_in_memory,
    retag_binding_in_memory, set_activate,
};
pub use mutation::{ManifestSnapshot, MutationCommit, MutationGuard, StagedMutation};
pub use project_lock::{acquire_project_lock, acquire_project_lock_for_file};
pub use registry::ProjectRegistry;
pub use resolve::{ResolveLockOptions, lookup_host_leaf, resolve_lock, resolve_lock_touched};
pub use toolchain_home::resolve_toolchain_home;

/// The crate's own `Result`, keyed to the tier's own [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Reserved name of the implicit default group (top-level `[tools]`).
pub const DEFAULT_GROUP: &str = internal::DEFAULT_GROUP;

/// Reserved `-g` keyword: the default group plus every named group. Never a
/// declarable group; `[group.all]` and `--group all` are rejected.
pub const ALL_GROUP: &str = internal::ALL_GROUP;
