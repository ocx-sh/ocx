// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// Most task submodules are crate-internal: the public facade is the `pub`
// methods on `impl PackageManager` (re-exported via `package_manager.rs`), not
// the module paths. `tasks` is `pub` only so the CLI can reach the patch
// discovery helpers (`patch_descriptor_id` / `global_descriptor_id` /
// `PatchTagMap`) — every other module stays `pub(crate)` so internal `pub`
// items (e.g. `pull::SetupGroups::new`) do not leak into the crate's public
// API surface (which would trip `clippy::new_without_default`).
pub(crate) mod clean;
pub(crate) mod common;
pub(crate) mod deselect;
pub(crate) mod find;
pub(crate) mod find_or_install;
pub(crate) mod find_symlink;
pub(crate) mod garbage_collection;
pub(crate) mod hook;
pub(crate) mod inspect;
pub(crate) mod install;
pub(crate) mod layer_staging;
pub mod patch_discovery;
pub(crate) mod patch_publish;
pub(crate) mod patch_sync;
pub(crate) mod patch_test;
pub(crate) mod pull;
pub(crate) mod pull_local;
pub(crate) mod purge;
pub(crate) mod resolve;
pub(crate) mod select;
pub(crate) mod uninstall;
pub(crate) mod update_check;
