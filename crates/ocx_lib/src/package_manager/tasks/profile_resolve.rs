// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::{
    log, oci,
    package::metadata::{self, env::conflict},
    prelude::SerdeExt,
    profile::{ProfileEntry, ProfileMode},
};

use super::super::PackageManager;

/// A successfully resolved profile entry with its content path and metadata.
#[derive(Debug)]
pub struct ResolvedProfileEntry {
    /// The original identifier string from the profile manifest.
    pub identifier: String,
    /// The resolution mode used.
    pub mode: ProfileMode,
    /// The resolved content path (object store or symlink depending on mode).
    pub content_path: PathBuf,
    /// The package metadata loaded from the object store.
    pub metadata: metadata::Metadata,
}

/// The result of resolving a single profile entry.
#[derive(Debug)]
pub enum ProfileEntryResolution {
    /// The entry was successfully resolved.
    Resolved(ResolvedProfileEntry),
    /// The entry could not be resolved (broken symlink, missing object, etc.).
    Broken {
        identifier: String,
        mode: ProfileMode,
        path: Option<PathBuf>,
        reason: String,
    },
}

impl ProfileEntryResolution {
    fn broken(entry: &ProfileEntry, path: Option<PathBuf>, reason: impl Into<String>) -> Self {
        Self::Broken {
            identifier: entry.identifier.clone(),
            mode: entry.mode,
            path,
            reason: reason.into(),
        }
    }
}

impl PackageManager {
    /// Resolves all profile entries to their content paths and metadata.
    ///
    /// Returns one `ProfileEntryResolution` per input entry, preserving order.
    /// Entries that cannot be resolved (missing symlinks, purged objects, etc.)
    /// return `Broken` rather than failing the whole operation.
    ///
    /// Intentionally sequential — profiles are small (typically 5-15 entries) and
    /// most entries hit the fast path (local symlink + disk metadata read). The
    /// rare content-mode fallback only queries the local index.
    pub async fn resolve_profile_all(&self, entries: &[ProfileEntry]) -> Vec<ProfileEntryResolution> {
        let mut results = Vec::with_capacity(entries.len());

        for entry in entries {
            results.push(self.resolve_profile_entry(entry).await);
        }

        results
    }

    /// Resolves profile entries and detects env var conflicts across resolved entries.
    ///
    /// Returns the successfully resolved entries plus any constant env var conflicts.
    /// Broken entries are silently skipped (with log warnings).
    pub async fn resolve_profile_env(
        &self,
        entries: &[ProfileEntry],
    ) -> (Vec<ResolvedProfileEntry>, Vec<conflict::Conflict>) {
        let resolutions = self.resolve_profile_all(entries).await;
        let mut resolved = Vec::new();
        let mut conflicts = Vec::new();
        let mut tracker = conflict::ConstantTracker::new();

        for resolution in resolutions {
            match resolution {
                ProfileEntryResolution::Resolved(entry) => {
                    // Track constant env vars for conflict detection
                    if let Some(env) = entry.metadata.env() {
                        for var in env {
                            if let metadata::env::modifier::Modifier::Constant(c) = &var.modifier {
                                let resolved_value =
                                    c.value.replace("${installPath}", &entry.content_path.to_string_lossy());
                                if let Some(conflict) = tracker.track(&entry.identifier, &var.key, &resolved_value) {
                                    conflicts.push(conflict);
                                }
                            }
                        }
                    }
                    resolved.push(entry);
                }
                ProfileEntryResolution::Broken { identifier, reason, .. } => {
                    log::warn!("Skipping profile entry `{}`: {}", identifier, reason);
                }
            }
        }

        (resolved, conflicts)
    }

    async fn resolve_profile_entry(&self, entry: &ProfileEntry) -> ProfileEntryResolution {
        let default_registry = self.default_registry();
        let identifier = match oci::Identifier::parse_with_default_registry(&entry.identifier, default_registry) {
            Ok(id) => id,
            Err(e) => {
                return ProfileEntryResolution::broken(entry, None, format!("invalid identifier: {e}"));
            }
        };

        match entry.mode {
            ProfileMode::Candidate => {
                let path = self.file_structure().installs.candidate(&identifier);
                self.resolve_symlink_entry(entry, &path).await
            }
            ProfileMode::Current => {
                let path = self.file_structure().installs.current(&identifier);
                self.resolve_symlink_entry(entry, &path).await
            }
            ProfileMode::Content => self.resolve_content_entry(entry, &identifier).await,
        }
    }

    async fn resolve_symlink_entry(
        &self,
        entry: &ProfileEntry,
        symlink_path: &std::path::Path,
    ) -> ProfileEntryResolution {
        if !symlink_path.exists() {
            return ProfileEntryResolution::broken(
                entry,
                Some(symlink_path.to_path_buf()),
                format!("{} symlink does not exist", entry.mode),
            );
        }

        match self.load_metadata_for_content(symlink_path) {
            Ok(metadata) => ProfileEntryResolution::Resolved(ResolvedProfileEntry {
                identifier: entry.identifier.clone(),
                mode: entry.mode,
                content_path: symlink_path.to_path_buf(),
                metadata,
            }),
            Err(e) => ProfileEntryResolution::broken(entry, Some(symlink_path.to_path_buf()), e.to_string()),
        }
    }

    async fn resolve_content_entry(
        &self,
        entry: &ProfileEntry,
        identifier: &oci::Identifier,
    ) -> ProfileEntryResolution {
        // If we have a stored digest, try to resolve directly from the object store
        if let Some(digest_str) = &entry.content_digest
            && let Ok(digest) = oci::Digest::try_from(digest_str.clone())
        {
            let id_with_digest = identifier.clone_with_digest(digest);
            if let Ok(content_path) = self.file_structure().objects.content(&id_with_digest)
                && content_path.exists()
            {
                return match self.load_metadata_for_content(&content_path) {
                    Ok(metadata) => ProfileEntryResolution::Resolved(ResolvedProfileEntry {
                        identifier: entry.identifier.clone(),
                        mode: entry.mode,
                        content_path,
                        metadata,
                    }),
                    Err(e) => ProfileEntryResolution::broken(entry, Some(content_path), e.to_string()),
                };
            }
        }

        // Fallback: use find() to resolve via index (backward compat for entries without digest)
        let mut platforms = Vec::new();
        if let Some(platform) = oci::Platform::current() {
            platforms.push(platform);
        }
        platforms.push(oci::Platform::any());
        match self.find(identifier, platforms).await {
            Ok(info) => ProfileEntryResolution::Resolved(ResolvedProfileEntry {
                identifier: entry.identifier.clone(),
                mode: entry.mode,
                content_path: info.content,
                metadata: info.metadata,
            }),
            Err(e) => ProfileEntryResolution::broken(entry, None, e.to_string()),
        }
    }

    fn load_metadata_for_content(&self, content_path: &std::path::Path) -> crate::Result<metadata::Metadata> {
        let metadata_path = self.file_structure().objects.metadata_for_content(content_path)?;
        metadata::Metadata::read_json_from_path(&metadata_path)
    }
}
