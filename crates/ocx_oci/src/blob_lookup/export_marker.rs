// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-export.json`: the map from on-disk registry slug to the registry it names, at an export or cache root.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ocx_util::prelude::StringExt as _;
use serde::{Deserialize, Serialize};

use super::LookupError;

/// File name of the marker at an export root or a cache root.
pub const MARKER_FILE: &str = "ocx-export.json";

/// Largest marker read; a marker is a few hundred bytes.
pub const MARKER_CAP: u64 = 64 * 1024;

/// The marker's current layout version, and the one an absent file stands for.
pub const FORMAT_VERSION: u64 = 1;

/// The import safety walk skips `index/locks`, the writer's own lock directory: a registry slugged `locks` would put
/// index content in a tree nothing walks. Compared ASCII case-insensitively, as a case-folding file system would.
const RESERVED_SLUG: &str = "locks";

/// Windows file systems drop trailing dots and spaces, so `locks.` names the `locks` directory.
fn is_reserved_slug(slug: &str) -> bool {
    slug.trim_end_matches(['.', ' ']).eq_ignore_ascii_case(RESERVED_SLUG)
}

/// Unknown fields are tolerated: a later version may add some.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportMarker {
    pub format_version: u64,
    /// Slug to registry as written; every key must equal `to_relaxed_slug(value)`.
    pub registries: BTreeMap<String, String>,
}

impl Default for ExportMarker {
    fn default() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            registries: BTreeMap::new(),
        }
    }
}

/// Why a marker could not be read or extended.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum ExportMarkerError {
    /// Two registries fold to one slug on a case-insensitive file system.
    #[error("registry '{registry}' collides with '{existing}' in the export marker")]
    #[exit(
        DataError,
        slug = "export_dir_invalid",
        summary = "An export directory or archive is not valid"
    )]
    SlugCollision { registry: String, existing: String },
    /// The registry slugs to a directory name the export layout reserves.
    #[error("registry '{registry}' cannot be exported: its directory name '{RESERVED_SLUG}' is reserved")]
    #[exit(
        DataError,
        slug = "export_dir_invalid",
        summary = "An export directory or archive is not valid"
    )]
    ReservedSlug { registry: String },
    /// The marker is not valid JSON, or a key is not the slug of its value.
    #[error("export marker '{}' is invalid: {reason}", path.display())]
    #[exit(
        DataError,
        slug = "export_dir_invalid",
        summary = "An export directory or archive is not valid"
    )]
    Invalid { path: PathBuf, reason: String },
    #[error(transparent)]
    #[exit(delegate)]
    Read(#[from] LookupError),
}

/// Reads `dir/ocx-export.json` (blocking, 64 KiB cap, checked against the slug rule); `None` when absent, which
/// means layout 1.
///
/// # Errors
///
/// [`ExportMarkerError::Invalid`] for malformed JSON, an unknown `format_version`, or a key that is not its value's slug
/// or not a usable directory name.
pub fn read_marker(dir: &Path) -> Result<Option<ExportMarker>, ExportMarkerError> {
    let path = dir.join(MARKER_FILE);
    let invalid = |reason: String| ExportMarkerError::Invalid {
        path: path.clone(),
        reason,
    };
    let bytes = match ocx_util::fs::read_bounded(&path, MARKER_CAP) {
        Ok(bytes) => bytes,
        Err(ocx_util::fs::BoundedReadError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(ocx_util::fs::BoundedReadError::Io { source, .. }) => {
            return Err(LookupError::Io { path, source }.into());
        }
        Err(refusal) => return Err(invalid(refusal.into_io_error().to_string())),
    };
    let marker: ExportMarker = serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
    if marker.format_version != FORMAT_VERSION {
        return Err(invalid(format!(
            "format_version {} is not supported (this ocx reads {FORMAT_VERSION})",
            marker.format_version
        )));
    }
    for (slug, registry) in &marker.registries {
        if is_reserved_slug(slug) {
            return Err(invalid(format!(
                "'{RESERVED_SLUG}' is reserved and cannot be a registry slug"
            )));
        }
        // `..` is left to the index store's containment guard, which refuses it as `repository_escapes_index_home`.
        if slug != ".." && !super::registry_slug_is_safe(slug) {
            return Err(invalid(format!("'{slug}' is not a usable directory name")));
        }
        if *slug != registry.to_relaxed_slug() {
            return Err(invalid(format!("'{slug}' is not the slug of '{registry}'")));
        }
    }
    Ok(Some(marker))
}

/// Writes `dir/ocx-export.json` atomically, replacing the previous marker. Blocking.
///
/// # Errors
///
/// Disk failure.
pub fn write_marker(dir: &Path, marker: &ExportMarker) -> Result<(), LookupError> {
    let path = dir.join(MARKER_FILE);
    let io = |source| LookupError::Io {
        path: path.clone(),
        source,
    };
    let mut bytes = serde_json::to_vec_pretty(marker).map_err(|error| io(std::io::Error::other(error)))?;
    bytes.push(b'\n');
    let mut temp = super::writer::umask_temp_in(dir).map_err(io)?;
    std::io::Write::write_all(&mut temp, &bytes).map_err(io)?;
    ocx_util::fs::persist_temp_file(temp, &path).map_err(io)
}

/// Records `registry` in `marker`; idempotent for a registry already present.
///
/// # Errors
///
/// [`ExportMarkerError::SlugCollision`] when another registry shares the slug after ASCII case folding;
/// [`ExportMarkerError::ReservedSlug`] when the slug is the reserved `locks`, which [`read_marker`] refuses.
pub fn merge_registry(marker: &mut ExportMarker, registry: &str) -> Result<(), ExportMarkerError> {
    let slug = registry.to_relaxed_slug();
    if is_reserved_slug(&slug) {
        return Err(ExportMarkerError::ReservedSlug {
            registry: registry.to_string(),
        });
    }
    for (existing_slug, existing) in &marker.registries {
        if existing_slug.eq_ignore_ascii_case(&slug) && existing != registry {
            return Err(ExportMarkerError::SlugCollision {
                registry: registry.to_string(),
                existing: existing.clone(),
            });
        }
    }
    marker.registries.insert(slug, registry.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_is_layout_1() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_marker(dir.path()).unwrap().is_none());
        assert_eq!(ExportMarker::default().format_version, 1);
    }

    #[test]
    fn case_fold_collision() {
        let mut marker = ExportMarker::default();
        merge_registry(&mut marker, "reg.test").unwrap();
        merge_registry(&mut marker, "reg.test").unwrap();
        let error = merge_registry(&mut marker, "Reg.test").expect_err("folds to the same slug");
        assert!(matches!(error, ExportMarkerError::SlugCollision { .. }), "{error:?}");
    }

    #[test]
    fn merging_a_registry_slugged_locks_is_refused() {
        for registry in ["locks", "LOCKS"] {
            let mut marker = ExportMarker::default();
            let error = merge_registry(&mut marker, registry).expect_err("import would refuse the marker");
            assert!(matches!(error, ExportMarkerError::ReservedSlug { .. }), "{error:?}");
            assert!(
                error.to_string().contains("'locks'"),
                "names the reserved slug: {error}"
            );
            assert!(marker.registries.is_empty(), "a refused registry is not recorded");
        }
    }

    #[test]
    fn slug_must_match_value() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MARKER_FILE),
            br#"{"format_version":1,"registries":{"wrong_slug":"localhost:5000"}}"#,
        )
        .unwrap();
        let error = read_marker(dir.path()).expect_err("key is not the slug of its value");
        assert!(matches!(error, ExportMarkerError::Invalid { .. }), "{error:?}");
    }

    #[test]
    fn a_registry_slugged_locks_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MARKER_FILE),
            br#"{"format_version":1,"registries":{"locks":"locks"}}"#,
        )
        .unwrap();
        let error = read_marker(dir.path()).expect_err("the walk skips index/locks");
        assert!(matches!(error, ExportMarkerError::Invalid { .. }), "{error:?}");
    }

    #[test]
    fn merging_a_registry_whose_slug_names_locks_on_windows_is_refused() {
        for registry in ["locks.", "locks..", "LOCKS."] {
            let mut marker = ExportMarker::default();
            let error = merge_registry(&mut marker, registry).expect_err("same directory as locks on Windows");
            assert!(
                matches!(error, ExportMarkerError::ReservedSlug { .. }),
                "{registry}: {error:?}"
            );
        }
    }

    fn marker_with_slug(slug: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MARKER_FILE),
            format!(r#"{{"format_version":1,"registries":{{"{slug}":"{slug}"}}}}"#),
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_marker_slug_naming_the_walked_root_itself_is_refused() {
        for slug in [".", ""] {
            let error = read_marker(marker_with_slug(slug).path()).expect_err("unsafe slug");
            assert!(
                matches!(error, ExportMarkerError::Invalid { .. }),
                "{slug:?}: {error:?}"
            );
        }
    }

    #[test]
    fn a_traversal_marker_slug_is_left_to_the_index_containment_guard() {
        assert!(
            read_marker(marker_with_slug("..").path()).is_ok(),
            "`..` must reach the index store's guard, which owns its error slug"
        );
    }

    #[test]
    fn unknown_format_version_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MARKER_FILE), br#"{"format_version":2,"registries":{}}"#).unwrap();
        let error = read_marker(dir.path()).expect_err("a later layout is not guessed at");
        assert!(matches!(error, ExportMarkerError::Invalid { .. }), "{error:?}");
    }

    #[test]
    fn unknown_fields_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MARKER_FILE),
            br#"{"format_version":1,"registries":{"localhost_5000":"localhost:5000"},"added_later":true}"#,
        )
        .unwrap();
        let marker = read_marker(dir.path()).unwrap().expect("present");
        assert_eq!(marker.registries["localhost_5000"], "localhost:5000");
    }
}
