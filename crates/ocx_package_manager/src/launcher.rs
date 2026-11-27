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

/// Refuses a record the launcher would refuse to read, so a home writer never publishes one. `context` names the
/// path the error reports.
///
/// # Errors
///
/// [`crate::Error::LauncherRecordRefused`] carrying the record's [`LaunchRecord::validate`] failure.
pub(crate) fn validate_for_write(
    record: &LaunchRecord,
    windows: bool,
    context: &std::path::Path,
) -> Result<(), crate::Error> {
    record
        .validate(windows)
        .map_err(|source| crate::Error::LauncherRecordRefused {
            path: context.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use ocx_env_apply::launch::{LaunchKind, LaunchRecord, PackageRecord, RecordError, SCHEMA_VERSION};
    use ocx_exit::{ClassifyErrorKind, ClassifyExitCode, Detail, ExitCode};

    use super::validate_for_write;

    /// A record the launcher would refuse leaves the writer as a data error, with the broken rule as its cause.
    #[test]
    fn a_record_the_launcher_would_refuse_is_a_data_error_whose_source_is_the_record_error() {
        let relative_root = LaunchRecord::Package(PackageRecord {
            kind: LaunchKind::Package,
            schema_version: SCHEMA_VERSION,
            package_root: "relative/root".to_owned(),
        });
        let error = validate_for_write(&relative_root, false, std::path::Path::new("/home/launchers/tool"))
            .expect_err("a relative package root is refused");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
        let Detail::Fixed(entry) = error.kind_detail() else {
            panic!("the variant names its slug directly");
        };
        assert_eq!(entry.slug, "launcher_record_refused");
        let source = error.source().expect("the record error is the source");
        assert_eq!(
            source.downcast_ref::<RecordError>(),
            Some(&RecordError::NotAbsolute("relative/root".to_owned()))
        );
        assert!(
            !error.to_string().contains("relative/root"),
            "the cause is printed by the chain, not twice: {error}"
        );
    }
}
