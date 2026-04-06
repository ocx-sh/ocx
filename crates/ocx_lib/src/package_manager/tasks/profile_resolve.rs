// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::{
    log, oci,
    package::{
        install_info::InstallInfo,
        metadata::{self, env::conflict},
        resolved_package::ResolvedPackage,
    },
    profile::{ProfileEntry, ProfileMode},
};

use super::super::PackageManager;

/// A successfully resolved profile entry with its content path and metadata.
#[derive(Debug)]
pub struct ResolvedProfileEntry {
    /// The fully-qualified OCI identifier from the profile manifest.
    pub identifier: oci::PinnedIdentifier,
    /// The resolution mode used.
    pub mode: ProfileMode,
    /// The resolved content path (object store or symlink depending on mode).
    pub content_path: PathBuf,
    /// The package metadata loaded from the object store.
    pub metadata: metadata::Metadata,
    /// The resolved dependency closure.
    pub resolved: ResolvedPackage,
}

impl From<&ResolvedProfileEntry> for InstallInfo {
    fn from(entry: &ResolvedProfileEntry) -> Self {
        Self {
            identifier: entry.identifier.clone(),
            metadata: entry.metadata.clone(),
            resolved: entry.resolved.clone(),
            content: entry.content_path.clone(),
        }
    }
}

/// The result of resolving a single profile entry.
#[derive(Debug)]
pub enum ProfileEntryResolution {
    /// The entry was successfully resolved.
    Resolved(ResolvedProfileEntry),
    /// The entry could not be resolved (broken symlink, missing object, etc.).
    Broken {
        identifier: oci::Identifier,
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
                                if let Some(conflict) =
                                    tracker.track(&entry.identifier.to_string(), &var.key, &resolved_value)
                                {
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
        match entry.mode {
            ProfileMode::Candidate => {
                let path = self.file_structure().symlinks.candidate(&entry.identifier);
                resolve_symlink_entry(&self.file_structure().packages, entry, &path).await
            }
            ProfileMode::Current => {
                let path = self.file_structure().symlinks.current(&entry.identifier);
                resolve_symlink_entry(&self.file_structure().packages, entry, &path).await
            }
            ProfileMode::Content => resolve_content_entry(self, entry).await,
        }
    }
}

async fn resolve_symlink_entry(
    objects: &crate::file_structure::PackageStore,
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

    match super::common::load_object_data(objects, symlink_path).await {
        Ok((metadata, resolved)) => ProfileEntryResolution::Resolved(ResolvedProfileEntry {
            identifier: resolved.identifier.clone(),
            mode: entry.mode,
            content_path: symlink_path.to_path_buf(),
            metadata,
            resolved,
        }),
        Err(e) => {
            log::debug!("Failed to resolve symlink entry '{}': {}", entry.identifier, e);
            ProfileEntryResolution::broken(entry, Some(symlink_path.to_path_buf()), e.to_string())
        }
    }
}

async fn resolve_content_entry(mgr: &PackageManager, entry: &ProfileEntry) -> ProfileEntryResolution {
    // Digest is on the identifier — resolve directly from the object store.
    if let Ok(pinned) = oci::PinnedIdentifier::try_from(entry.identifier.clone()) {
        let content_path = mgr.file_structure().packages.content(&pinned);
        if content_path.exists() {
            return match super::common::load_object_data(&mgr.file_structure().packages, &content_path).await {
                Ok((metadata, resolved)) => ProfileEntryResolution::Resolved(ResolvedProfileEntry {
                    identifier: pinned,
                    mode: entry.mode,
                    content_path,
                    metadata,
                    resolved,
                }),
                Err(e) => {
                    log::debug!("Failed to resolve content entry '{}': {}", entry.identifier, e);
                    ProfileEntryResolution::broken(entry, Some(content_path), e.to_string())
                }
            };
        }
    }

    // Fallback: use find() to resolve via index (backward compat for entries without digest)
    let mut platforms = Vec::new();
    if let Some(platform) = oci::Platform::current() {
        platforms.push(platform);
    }
    platforms.push(oci::Platform::any());
    match mgr.find(&entry.identifier, platforms).await {
        Ok(info) => ProfileEntryResolution::Resolved(ResolvedProfileEntry {
            identifier: info.identifier,
            mode: entry.mode,
            content_path: info.content,
            metadata: info.metadata,
            resolved: info.resolved,
        }),
        Err(e) => {
            log::debug!("Failed to find entry '{}': {}", entry.identifier, e);
            ProfileEntryResolution::broken(entry, None, e.to_string())
        }
    }
}
