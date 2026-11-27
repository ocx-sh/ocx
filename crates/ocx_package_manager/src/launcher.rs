// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! On-disk launchers: a byte copy of the embedded launcher image per name beside a `.launch` record. See
//! `adr_unified_launcher.md`.

mod body;
pub mod direct;
#[cfg(test)]
mod env_tests;
mod generate;
#[cfg(test)]
mod project_selector_tests;
mod safety;

pub use generate::{binary_pin_value, generate, generate_direct};

/// A `pub(crate)` item inside a private `mod` does not escape it, so the
/// trampoline surface `crate::tasks::render_toolchain` calls needs this
/// re-export; re-exporting one nothing imports is an `unused_imports` error.
pub(crate) use body::{TrampolineTarget, toolchain_record};
pub(crate) use generate::{
    EntryPointDir, entry_point_dir, first_use, host_generation, host_launcher_blob, names_complete,
    trampoline_ocx_binary, write_launcher,
};

use ocx_env_apply::launch::{DeferredRecord, LaunchKind, LaunchRecord, SCHEMA_VERSION};

/// The record of a **deferred** tool's launchers, one for every name of its shim directory: the pinned identifier the
/// first run installs. The unsafe-character check stays at this boundary, [`safety::LauncherSafeString`] being the one
/// validator.
///
/// # Errors
///
/// Returns an error if `identifier`'s rendering contains a character unsafe for a launcher record.
pub(crate) fn deferred_record(identifier: &ocx_oci::PinnedPackageRef) -> Result<LaunchRecord, crate::Error> {
    let identifier = safety::LauncherSafeString::new(identifier.to_string())?;
    Ok(LaunchRecord::Deferred(DeferredRecord {
        kind: LaunchKind::Deferred,
        schema_version: SCHEMA_VERSION,
        identifier: identifier.as_str().to_owned(),
    }))
}
