// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! On-disk launcher scripts that wrap `ocx exec` per entrypoint. See
//! `adr_package_entry_points.md`.

mod body;
mod generate;
mod pathext;
mod safety;

pub use generate::generate;
pub use pathext::{LAUNCHER_EXT, emplace_pathext, includes_launcher};
pub(crate) use safety::LauncherSafeString;
