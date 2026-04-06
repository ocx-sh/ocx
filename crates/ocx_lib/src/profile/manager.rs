// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::{file_structure::FileStructure, log, oci, reference_manager::ReferenceManager};

use super::{AddOutcome, ProfileEntry, ProfileError, ProfileManifest, ProfileMode, ProfileSnapshot};

/// Input for a single `add_all` operation.
pub struct ProfileAddInput {
    /// Fully-qualified identifier to add to the profile.
    /// For content-mode entries, this should carry the digest via `clone_with_digest()`.
    pub identifier: oci::Identifier,
    /// Resolution mode for shell startup.
    pub mode: ProfileMode,
    /// The content path (object store content directory) for this package.
    pub content_path: PathBuf,
}

/// Facade for profile mutation operations.
///
/// Peers with `FileStructure` / `Index` / `Client` on `PackageManager`.
/// Handles load-modify-save cycles, symlink creation for profiled packages,
/// and conflict/mode-change warnings.
#[derive(Debug, Clone)]
pub struct ProfileManager {
    file_structure: FileStructure,
}

impl ProfileManager {
    pub fn new(file_structure: FileStructure) -> Self {
        Self { file_structure }
    }

    /// Returns the path to the profile manifest (`$OCX_HOME/profile.json`).
    pub fn manifest_path(&self) -> PathBuf {
        self.file_structure.profile_manifest()
    }

    /// Creates a read-only snapshot of the current profile manifest.
    ///
    /// Silently returns an empty snapshot if the manifest cannot be loaded
    /// (e.g. file missing, unreadable, or malformed). This is intentional —
    /// snapshots are used for best-effort warnings, not correctness-critical paths.
    pub fn snapshot(&self) -> ProfileSnapshot {
        ProfileSnapshot::load(&self.manifest_path())
    }

    /// Loads the profile manifest (non-exclusive). Use for read-only operations
    /// that don't need mutation (e.g. `profile list`, `profile load`).
    pub fn load(&self) -> Result<ProfileManifest, ProfileError> {
        ProfileManifest::load(&self.manifest_path())
    }

    /// Adds one or more packages to the profile manifest.
    ///
    /// For each input:
    /// 1. Creates symlinks (candidate/current) as needed for the chosen mode
    /// 2. Warns about other versions of the same repo already in the profile
    /// 3. Warns on mode changes for existing entries
    /// 4. Saves the manifest atomically
    ///
    /// If symlink creation fails mid-batch, the manifest is saved with all
    /// entries that succeeded before the error is propagated. This prevents
    /// orphan symlinks without corresponding manifest entries.
    ///
    /// Returns one `AddOutcome` per input, preserving order.
    pub fn add_all(&self, inputs: Vec<ProfileAddInput>) -> crate::Result<Vec<AddOutcome>> {
        let manifest_path = self.manifest_path();
        let (mut manifest, _lock) = ProfileManifest::load_exclusive(&manifest_path)?;
        let rm = ReferenceManager::new(self.file_structure.clone());

        let result = self.add_all_inner(&inputs, &mut manifest, &rm);

        // Always save the manifest — even on error — to preserve partial progress.
        // If both the inner operation and save fail, propagate the inner error
        // (the original cause) rather than the save error.
        let save_result = manifest.save(&manifest_path);

        match result {
            Ok(outcomes) => {
                save_result?;
                Ok(outcomes)
            }
            Err(inner_err) => {
                if let Err(save_err) = save_result {
                    log::warn!("Failed to save profile manifest after error: {save_err}");
                }
                Err(inner_err)
            }
        }
    }

    fn add_all_inner(
        &self,
        inputs: &[ProfileAddInput],
        manifest: &mut ProfileManifest,
        rm: &ReferenceManager,
    ) -> crate::Result<Vec<AddOutcome>> {
        let mut outcomes = Vec::with_capacity(inputs.len());

        for input in inputs {
            self.create_symlinks_for_mode(rm, input)?;

            // Warn if another version of the same repo is already in the profile
            for existing in manifest.entries_for_repo(&input.identifier) {
                if existing.identifier != input.identifier {
                    log::warn!(
                        "{} is already in your shell profile as `{}`. \
                         Having multiple versions of the same package may cause PATH conflicts.",
                        input.identifier,
                        existing.identifier,
                    );
                }
            }

            // Content mode requires a digest on the identifier for direct object store resolution.
            if input.mode == ProfileMode::Content && input.identifier.digest().is_none() {
                return Err(ProfileError::ContentModeRequiresDigest {
                    identifier: input.identifier.to_string(),
                }
                .into());
            }

            let outcome = manifest.add(ProfileEntry {
                identifier: input.identifier.clone(),
                mode: input.mode,
            });

            // Warn on mode switch
            if let AddOutcome::Updated { previous_mode } = outcome
                && previous_mode != input.mode
            {
                log::warn!(
                    "{}: mode changed from `{}` to `{}`",
                    input.identifier,
                    previous_mode,
                    input.mode,
                );
            }

            outcomes.push(outcome);
        }

        Ok(outcomes)
    }

    fn create_symlinks_for_mode(&self, rm: &ReferenceManager, input: &ProfileAddInput) -> crate::Result<()> {
        match input.mode {
            ProfileMode::Candidate => {
                let candidate_path = self.file_structure.symlinks.candidate(&input.identifier);
                if !candidate_path.exists() {
                    rm.link(&candidate_path, &input.content_path)?;
                }
            }
            ProfileMode::Current => {
                let candidate_path = self.file_structure.symlinks.candidate(&input.identifier);
                if !candidate_path.exists() {
                    rm.link(&candidate_path, &input.content_path)?;
                }
                let current_path = self.file_structure.symlinks.current(&input.identifier);
                if !current_path.exists() {
                    rm.link(&current_path, &input.content_path)?;
                }
            }
            ProfileMode::Content => {}
        }
        Ok(())
    }

    /// Removes one or more packages from the profile manifest.
    ///
    /// Each identifier string is matched exactly against existing entries.
    /// Callers must resolve identifiers (e.g. via `transform_all`) before
    /// calling this method — short identifiers like `cmake:3.28` will not
    /// match entries stored as `ocx.sh/cmake:3.28`.
    ///
    /// Returns one `bool` per input: `true` if the entry was removed, `false`
    /// if it was already absent.
    pub fn remove_all(&self, identifiers: &[oci::Identifier]) -> crate::Result<Vec<bool>> {
        let manifest_path = self.manifest_path();
        let (mut manifest, _lock) = ProfileManifest::load_exclusive(&manifest_path)?;

        let results: Vec<bool> = identifiers.iter().map(|id| manifest.remove(id)).collect();

        if results.iter().any(|&removed| removed) {
            manifest.save(&manifest_path)?;
        }
        Ok(results)
    }
}
