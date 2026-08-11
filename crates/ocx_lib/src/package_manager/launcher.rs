// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! On-disk launcher scripts that wrap `ocx exec` per entrypoint. See
//! `adr_package_entry_points.md`.

mod body;
mod generate;
mod safety;

pub use generate::generate;

/// The generated Unix shim body for one declared interface name of a
/// **deferred** tool — a tool composed onto `PATH` without its content being
/// materialized.
///
/// The body is name-independent: `$(basename "$0")` carries the invoked name,
/// so one rendering serves every name in a shim directory's `bin/`.
///
/// Exists so the generation task (`package_manager::tasks::prepare_lazy`) can
/// reach [`body::unix_shim_body`] — C-018 sanctions exactly two producers of
/// the `launcher shim` wire token, and this is not a third one — while the
/// unsafe-character check stays at this module's entry boundary, exactly where
/// [`generate`] applies it. That keeps [`safety::LauncherSafeString`] the one
/// validator for every generated body and out of the caller's vocabulary.
///
/// # Errors
///
/// Returns an error if `identifier`'s rendering contains a character unsafe
/// for the launcher template.
pub(crate) fn shim_body(identifier: &crate::oci::PinnedIdentifier) -> Result<String, crate::Error> {
    let identifier = safety::LauncherSafeString::new(identifier.to_string())?;
    Ok(body::unix_shim_body(&identifier))
}
