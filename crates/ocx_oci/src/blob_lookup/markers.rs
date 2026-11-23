// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Referrer markers: empty files naming the referrers a blob carries, so a signature verifies from a directory.
//!
//! `<blob dir>/referrers/<alg>-<hex>` per referrer, and `referrers/.complete` once the whole listing was read.
//! Files are empty and written atomically; a marker proves nothing until its manifest is read and checked.

use std::path::{Path, PathBuf};

use crate::Digest;

use super::LookupError;

/// File name that marks a subject's marker listing as complete.
pub const COMPLETE_FILE: &str = ".complete";

/// The candidates one root holds for a subject.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferrerMarkers {
    pub digests: Vec<Digest>,
    /// True when any root holds `referrers/.complete`.
    pub complete: bool,
}

fn referrers_dir(root: &Path, registry: &str, subject: &Digest) -> PathBuf {
    super::blob_dir_path(root, registry, subject).join("referrers")
}

/// Creates the empty file `name` in the subject's marker directory; an existing one is success.
fn touch(root: &Path, registry: &str, subject: &Digest, name: &str) -> Result<(), LookupError> {
    if !super::registry_slug_is_safe(registry) {
        return Ok(());
    }
    let dir = referrers_dir(root, registry, subject);
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| LookupError::Io { path, source }
    };
    super::writer::refuse_symlinks(root, &dir)?;
    std::fs::create_dir_all(&dir).map_err(io(&dir))?;
    let path = dir.join(name);
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(io(&path)(error)),
    }
}

/// Writes the marker for `referrer` under `subject`; idempotent. Blocking.
///
/// # Errors
///
/// Disk failure, or a symlinked ancestor.
pub fn write_marker(root: &Path, registry: &str, subject: &Digest, referrer: &Digest) -> Result<(), LookupError> {
    touch(root, registry, subject, &referrer.to_string().replace(':', "-"))
}

/// Writes `referrers/.complete` for `subject`; idempotent. Blocking.
///
/// # Errors
///
/// As [`write_marker`].
pub fn write_complete(root: &Path, registry: &str, subject: &Digest) -> Result<(), LookupError> {
    touch(root, registry, subject, COMPLETE_FILE)
}

/// Reads at most `cap` marker names of `subject`, deduplicated and sorted; a name that is not a digest is skipped
/// with a warning. Blocking.
///
/// # Errors
///
/// A marker directory that exists but cannot be listed.
pub fn read_markers(root: &Path, registry: &str, subject: &Digest, cap: usize) -> Result<ReferrerMarkers, LookupError> {
    let mut markers = ReferrerMarkers::default();
    if !super::registry_slug_is_safe(registry) {
        return Ok(markers);
    }
    let dir = referrers_dir(root, registry, subject);
    let io = |source| LookupError::Io {
        path: dir.clone(),
        source,
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(markers),
        Err(error) => return Err(io(error)),
    };
    let mut seen = 0usize;
    for entry in entries {
        let name = entry.map_err(io)?.file_name();
        if name == COMPLETE_FILE {
            continue;
        }
        if seen == cap {
            log::warn!(
                "'{}' lists more than {cap} referrer markers; the rest are ignored",
                dir.display()
            );
            break;
        }
        seen += 1;
        let parsed = name
            .to_str()
            .and_then(|name| name.split_once('-'))
            .and_then(|(algorithm, hex)| Digest::try_from(format!("{algorithm}:{hex}")).ok());
        match parsed {
            Some(digest) => markers.digests.push(digest),
            None => log::warn!("skipping referrer marker {name:?} in '{}': not a digest", dir.display()),
        }
    }
    // Checked by name, not by listing, so a truncated listing still sees it.
    markers.complete = std::fs::symlink_metadata(dir.join(COMPLETE_FILE)).is_ok_and(|meta| meta.is_file());
    markers.digests.sort();
    markers.digests.dedup();
    Ok(markers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_lookup::blob_dir_path;
    use crate::blob_lookup::test_support::sha256;

    const REG: &str = "registry.test";

    #[test]
    fn write_marker_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let (subject, referrer) = (sha256(b"subject"), sha256(b"referrer"));

        write_marker(root.path(), REG, &subject, &referrer).unwrap();
        write_marker(root.path(), REG, &subject, &referrer).unwrap();

        let listed = read_markers(root.path(), REG, &subject, 16).unwrap();
        assert_eq!(listed.digests, vec![referrer.clone()]);
        let marker = blob_dir_path(root.path(), REG, &subject)
            .join("referrers")
            .join(referrer.to_string().replace(':', "-"));
        assert_eq!(std::fs::metadata(marker).unwrap().len(), 0, "a marker is an empty file");
    }

    #[test]
    fn write_complete_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let subject = sha256(b"subject");

        assert!(!read_markers(root.path(), REG, &subject, 16).unwrap().complete);
        write_complete(root.path(), REG, &subject).unwrap();
        write_complete(root.path(), REG, &subject).unwrap();
        let listed = read_markers(root.path(), REG, &subject, 16).unwrap();
        assert!(listed.complete);
        assert!(listed.digests.is_empty(), "`.complete` is not a candidate");
    }

    #[test]
    fn invalid_name_skipped() {
        let root = tempfile::tempdir().unwrap();
        let (subject, referrer) = (sha256(b"subject"), sha256(b"referrer"));
        write_marker(root.path(), REG, &subject, &referrer).unwrap();
        let dir = blob_dir_path(root.path(), REG, &subject).join("referrers");
        std::fs::write(dir.join("not-a-digest"), b"").unwrap();
        std::fs::write(dir.join("sha256-short"), b"").unwrap();

        let listed = read_markers(root.path(), REG, &subject, 16).unwrap();
        assert_eq!(listed.digests, vec![referrer]);
    }

    #[test]
    fn cap_enforced() {
        let root = tempfile::tempdir().unwrap();
        let subject = sha256(b"subject");
        for index in 0..5u8 {
            write_marker(root.path(), REG, &subject, &sha256(&[index])).unwrap();
        }

        let listed = read_markers(root.path(), REG, &subject, 3).unwrap();
        assert_eq!(listed.digests.len(), 3);
    }
}
