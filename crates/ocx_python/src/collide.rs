// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publish-time collision pre-check across a selected wheel set.
//!
//! A valid resolved lock is collision-free by construction, so OCX's
//! overlap-free prefix-layer union composes a correct `site-packages`. This
//! check is the guard that proves the invariant holds for a concrete wheel set
//! *before* anything is pushed: if two repacked wheels claim the same installed
//! path (post-relocation), the union would be ambiguous — a hard
//! [`CollisionError`], failing before push rather than corrupting the registry.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::repack::RepackedWheel;

/// Verifies that no two wheels in the set share an installed path.
///
/// Compares the `record_paths` of every [`RepackedWheel`]; PEP 420 namespace
/// package directories are shared by design and are not collisions — a wheel's
/// `RECORD` only ever lists files, never bare directories, so two wheels
/// contributing distinct leaf files under the same namespace directory (e.g.
/// `google/cloud/foo/__init__.py` vs `google/cloud/bar/__init__.py`) never
/// produce equal path strings and never collide.
///
/// # Errors
///
/// Returns [`CollisionError::OverlappingPaths`] naming the conflicting path and
/// the two wheels that both claim it.
pub fn check_collisions(wheels: &[RepackedWheel]) -> Result<(), CollisionError> {
    let mut claimed_by: HashMap<&str, &str> = HashMap::new();
    for wheel in wheels {
        for path in &wheel.record_paths {
            match claimed_by.entry(path.as_str()) {
                Entry::Vacant(slot) => {
                    slot.insert(wheel.filename.as_str());
                }
                Entry::Occupied(slot) if *slot.get() != wheel.filename => {
                    return Err(CollisionError::OverlappingPaths {
                        path: path.clone(),
                        first_wheel: (*slot.get()).to_string(),
                        second_wheel: wheel.filename.clone(),
                    });
                }
                Entry::Occupied(_) => {}
            }
        }
    }
    Ok(())
}

/// Errors from the collision pre-check.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CollisionError {
    /// Two wheels claim the same installed path.
    #[error("path '{path}' is claimed by both wheel '{first_wheel}' and wheel '{second_wheel}'")]
    OverlappingPaths {
        /// The colliding installed path.
        path: String,
        /// The first wheel claiming the path.
        first_wheel: String,
        /// The second wheel claiming the path.
        second_wheel: String,
    },
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{CollisionError, check_collisions};
    use crate::repack::RepackedWheel;

    fn wheel(filename: &str, record_paths: &[&str]) -> RepackedWheel {
        RepackedWheel {
            filename: filename.to_string(),
            layer_path: PathBuf::from("/tmp/layer.tar.zst"),
            layer_digest: "sha256:0".to_string(),
            wheel_sha256: "0".to_string(),
            entry_points: Vec::new(),
            record_paths: record_paths.iter().map(|path| (*path).to_string()).collect(),
        }
    }

    #[test]
    fn disjoint_wheels_do_not_collide() {
        let wheels = [
            wheel("a-1.0-py3-none-any.whl", &["a/__init__.py", "a/mod.py"]),
            wheel("b-1.0-py3-none-any.whl", &["b/__init__.py", "b/mod.py"]),
        ];
        assert!(check_collisions(&wheels).is_ok());
    }

    #[test]
    fn identical_file_path_in_two_wheels_errors() {
        let wheels = [
            wheel("a-1.0-py3-none-any.whl", &["shared/module.py"]),
            wheel("b-1.0-py3-none-any.whl", &["shared/module.py"]),
        ];
        let error = check_collisions(&wheels).unwrap_err();
        let CollisionError::OverlappingPaths {
            path,
            first_wheel,
            second_wheel,
        } = error;
        assert_eq!(path, "shared/module.py");
        assert_eq!(first_wheel, "a-1.0-py3-none-any.whl");
        assert_eq!(second_wheel, "b-1.0-py3-none-any.whl");
    }

    #[test]
    fn pep420_namespace_directories_are_not_collisions() {
        let wheels = [
            wheel(
                "google-cloud-foo-1.0-py3-none-any.whl",
                &["google/cloud/foo/__init__.py", "google/cloud/foo/client.py"],
            ),
            wheel(
                "google-cloud-bar-1.0-py3-none-any.whl",
                &["google/cloud/bar/__init__.py", "google/cloud/bar/client.py"],
            ),
        ];
        assert!(check_collisions(&wheels).is_ok());
    }

    #[test]
    fn single_wheel_never_collides() {
        let wheels = [wheel("a-1.0-py3-none-any.whl", &["a/__init__.py", "a/mod.py"])];
        assert!(check_collisions(&wheels).is_ok());
    }
}
