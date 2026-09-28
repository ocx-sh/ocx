// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! On-disk launcher scripts that wrap `ocx exec` per entrypoint. See
//! `adr_package_entry_points.md`.

mod body;
#[cfg(test)]
mod env_tests;
mod generate;
#[cfg(test)]
mod project_selector_tests;
mod safety;

pub use generate::generate;

/// A `pub(crate)` item inside a private `mod` does not escape it, so the
/// trampoline surface `crate::tasks::render_toolchain` calls needs this
/// re-export. It lists exactly those items: re-exporting one nothing imports is
/// an `unused_imports` error under `-D warnings`. `EXEC_SIDECAR_GLOBAL` stays
/// absent because only [`body::exec_sidecar_body`] spells that literal.
pub(crate) use body::{TrampolineTarget, exec_sidecar_body, unix_trampoline_body};
pub(crate) use generate::trampoline_ocx_binary;

/// The generated Unix shim body for one declared interface name of a
/// **deferred** tool — composed onto `PATH` without its content materialized.
/// Name-independent: `${0##*/}` carries the invoked name, so one
/// rendering serves every name in a shim directory's `bin/`.
///
/// Lets `crate::tasks::prepare_lazy` reach [`body::unix_shim_body`] without a
/// third producer of the `launcher shim` wire token (exactly two are sanctioned),
/// while the unsafe-character check stays at this module's boundary as in
/// [`generate`], keeping [`safety::LauncherSafeString`] the one validator.
///
/// # Errors
///
/// Returns an error if `identifier`'s rendering contains a character unsafe
/// for the launcher template.
pub(crate) fn shim_body(identifier: &ocx_oci::PinnedPackageRef) -> Result<String, crate::Error> {
    let identifier = safety::LauncherSafeString::new(identifier.to_string())?;
    Ok(body::unix_shim_body(&identifier))
}
