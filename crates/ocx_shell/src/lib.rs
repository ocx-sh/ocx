// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shell and CI export surface: export generation, per-prompt reconciliation planner, hook emission, CI flavors.
//!
//! Two co-equal subtrees rather than one promoted root: [`shell`] emits what a
//! login shell evaluates, [`ci`] emits what a CI runner's log parser reads, and
//! neither is the other's detail — they share only the export vocabulary
//! underneath.
//!
//! Nothing here may reach `ocx_project` or `ocx_package_manager`. Both depend on
//! this crate — `project::consent` reads [`shell::coexistence`] and
//! [`shell::reconcile`], and `package_manager::activation` sequences them — so an
//! import in this direction closes a cycle. Until WP-32 that rule was a
//! source-text guard over a directory walk, because nothing about the failure was
//! visible to `cargo check`; the extraction turns it into a Cargo error, and the
//! guard was deleted rather than left as decoration.

pub mod ci;
pub mod shell;
