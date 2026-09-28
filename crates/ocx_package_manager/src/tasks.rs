// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// `pub(crate)` unless the CLI names a module by path, or internal `pub` items leak into the
// public API and trip `clippy::new_without_default`.
pub(crate) mod attest;
pub(crate) mod auto_verify;
pub(crate) mod clean;
pub(crate) mod common;
pub(crate) mod deselect;
pub(crate) mod find;
pub(crate) mod find_or_install;
pub(crate) mod find_symlink;
pub(crate) mod garbage_collection;
pub(crate) mod inspect;
pub(crate) mod install;
pub(crate) mod layer_staging;
pub(crate) mod lazy_advisory;
pub(crate) mod managed_config;
pub(crate) mod materialize_lazy;
pub mod patch_discovery;
pub(crate) mod patch_publish;
pub(crate) mod patch_sync;
pub(crate) mod patch_test;
pub(crate) mod prepare_lazy;
pub(crate) mod pull;
pub(crate) mod pull_local;
pub(crate) mod purge;
// `pub`: `ocx_cli` constructs the `RenderRequest` that `render_toolchain` takes.
pub mod render_toolchain;
pub(crate) mod resolve;
pub(crate) mod resolve_subject;
pub(crate) mod sbom;
pub(crate) mod select;
pub(crate) mod sign;
pub(crate) mod toolchain_names;
pub(crate) mod uninstall;
pub(crate) mod update_check;
pub(crate) mod verify;
