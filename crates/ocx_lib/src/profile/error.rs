// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// An error that occurred while loading or saving a profile manifest.
#[derive(Debug)]
pub enum ProfileError {
    /// The manifest file could not be read or written.
    Io(PathBuf, std::io::Error),
    /// The manifest JSON could not be parsed or serialized.
    Json(PathBuf, serde_json::Error),
    /// The manifest version is not supported by this version of ocx.
    UnsupportedVersion {
        path: PathBuf,
        version: u32,
        supported: u32,
    },
    /// The profile manifest is locked by another process.
    Locked(PathBuf),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileError::Io(path, error) => {
                write!(f, "profile manifest I/O error for '{}': {}", path.display(), error)
            }
            ProfileError::Json(path, error) => {
                write!(f, "profile manifest JSON error for '{}': {}", path.display(), error)
            }
            ProfileError::UnsupportedVersion {
                path,
                version,
                supported,
            } => write!(
                f,
                "unsupported profile manifest version {} in '{}' (supported: {}). \
                 A newer version of ocx may be required.",
                version,
                path.display(),
                supported
            ),
            ProfileError::Locked(path) => {
                write!(f, "profile manifest '{}' is locked by another process", path.display())
            }
        }
    }
}

impl std::error::Error for ProfileError {}
