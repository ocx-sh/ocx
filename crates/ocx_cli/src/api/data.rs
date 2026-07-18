// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod about;
pub mod catalog;
// ci_export deleted (C4 — handshake §6: ocx ci removed)
pub mod clean;
pub mod config_setup;
pub mod config_update;
pub mod deps;
pub mod env;
pub mod install;
pub mod lock;
pub mod login;
pub mod package_description;
pub mod package_inspect;
pub mod patch_freeze;
pub mod patch_publish;
pub mod patch_sync;
pub mod patch_test;
pub mod patch_why;
pub mod paths;
pub mod pull_dry_run;
pub mod push;
pub mod removed;
pub mod script_run;
pub mod self_setup;
pub mod self_update;
pub mod signature;
pub mod tag;
pub mod verification;
pub mod version;
