// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! On-disk launcher scripts that wrap `ocx exec` per entrypoint. See
//! `adr_package_entry_points.md`.

mod body;
mod generate;
mod safety;

pub use generate::generate;

/// R-W18(a) — a `pub(crate)` item inside a private `mod` does not escape it,
/// so WP-6's trampoline surface is unreachable without a re-export here.
///
/// Exactly the four items `package_manager::tasks::render_toolchain` calls, and
/// no more: re-exporting an item nothing imports is an `unused_imports` error
/// under `-D warnings`, so this line grows with its consumers rather than ahead
/// of them. `EXEC_SIDECAR_GLOBAL` is deliberately still absent — the renderer
/// never spells that literal, [`body::exec_sidecar_body`] does.
pub(crate) use body::{TrampolineTarget, exec_sidecar_body, unix_trampoline_body};
pub(crate) use generate::trampoline_ocx_binary;

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
