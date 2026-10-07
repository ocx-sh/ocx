// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Configuration discovery and loading; the CWD comes in via [`ConfigInputs`], never from ambient
//! state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures::future::join_all;
use tokio::io::AsyncReadExt;

use crate::{Config, error::ConfigSource, error::Error, error::Result};

/// Upper bound for a single config file.
pub const MAX_CONFIG_SIZE: u64 = 64 * 1024;

/// The project file's name, shared by the CWD walk and `--project <directory>`, or `--project .`
/// and a bare walk would answer differently for the same directory.
const PROJECT_FILE_NAME: &str = "ocx.toml";

/// Inputs to config discovery; the loader reads no ambient state beyond these.
pub struct ConfigInputs<'a> {
    /// `--config FILE` CLI flag (highest priority among explicit paths).
    pub explicit_path: Option<&'a Path>,
    /// `--project <FILE>` CLI flag (highest priority among project-tier sources).
    pub explicit_project_path: Option<&'a Path>,
    /// CWD for the project-tier walk. Pass `None` to disable the walk.
    pub cwd: Option<&'a Path>,
}

/// Result of [`ConfigLoader::load_with_local_view`]: the fully merged config and its views.
///
/// The managed-config fetch builds its client from `local_only`, or its own payload could
/// redirect the route that fetches it.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// Every tier, the managed payload included.
    pub merged: Config,
    /// Every tier except the managed payload.
    pub local_only: Config,
    /// Compiled-in defaults plus system, user and `$OCX_HOME`, without the explicit overlay; the
    /// managed tier folds onto this.
    pub base: Config,
    /// The explicit `OCX_CONFIG` / `--config` overlay, merged on top of the managed fold; kept apart
    /// from `base` so `ocx config test` can fold its own payload as `base` -> payload -> `overlay`.
    pub overlay: Config,
    /// The managed snapshot read from disk, before the identity gate, so a consumer's own identity
    /// check sees a mismatched snapshot rather than an absent one; `None` when absent or unreadable.
    pub managed_config_snapshot: Option<crate::managed::ManagedConfigSnapshot>,
    /// The managed target the fold resolved from the local-only view, or `None` when unconfigured
    /// or unresolvable (the caller re-resolves to surface a malformed seed).
    pub resolved_managed_config: Option<crate::managed::ResolvedManagedConfig>,
    /// What the managed snapshot actually contributed to `merged`, the `required` gate's input.
    pub managed_snapshot_state: crate::managed::ManagedSnapshotState,

    /// Every `config.toml` path this pass could have read, in fold order, absent ones included,
    /// since a tier file appearing is a change the per-prompt watch set must see.
    pub config_tier_paths: Vec<PathBuf>,

    /// The tier whose `extra_ca_certs` / `extra_ca_certs_pem` survived into `merged`, recorded per
    /// fold, never inferred from the value, so a refusal names the file that set it.
    pub extra_ca_certs_tier: Option<crate::ConfigTier>,
    /// The same record for `local_only` — identical to
    /// [`Self::extra_ca_certs_tier`] unless the managed payload set a key.
    pub extra_ca_certs_tier_local: Option<crate::ConfigTier>,
}

/// Stateless namespace for the discovery and loading pipeline.
pub struct ConfigLoader;

impl ConfigLoader {
    /// Top-level entry: discover, load, and merge.
    ///
    /// Precedence, lowest first: compiled-in defaults; system, user and `$OCX_HOME`; the
    /// identity-gated managed snapshot; `OCX_CONFIG` if non-empty; [`ConfigInputs::explicit_path`].
    /// `OCX_NO_CONFIG=1` skips the user, `$OCX_HOME` and managed tiers and cuts the system tier to
    /// its locked sections.
    ///
    /// # Errors
    /// Returns an error on missing explicit files, I/O failure, or TOML
    /// parse failure.
    pub async fn load(inputs: ConfigInputs<'_>) -> Result<Config> {
        Ok(Self::load_with_local_view(inputs).await?.merged)
    }

    /// Like [`Self::load`], but returning every view in [`LoadedConfig`].
    ///
    /// # Errors
    /// Same as [`Self::load`].
    pub async fn load_with_local_view(inputs: ConfigInputs<'_>) -> Result<LoadedConfig> {
        let no_config = ocx_env::OCX_NO_CONFIG.bool_or(false).unwrap_or(false);
        let raw_env_config_file = ocx_env::OCX_CONFIG.get_raw().and_then(|value| value.into_string().ok());
        if raw_env_config_file.as_deref() == Some("") {
            log::debug!("OCX_CONFIG is set to empty string — skipped via escape hatch");
        }
        let env_config_file = raw_env_config_file.filter(|s| !s.is_empty());

        // First, so a missing `--project` or `OCX_PROJECT` fails before anything else is read.
        let _project_path = Self::project_path(inputs.cwd, inputs.explicit_project_path).await?;

        // `OCX_NO_CONFIG=1` prunes ambient config, not operator policy: the system file still loads.
        let discovered: Vec<PathBuf> = if no_config {
            Self::existing_candidates(vec![Self::system_path()]).await?
        } else {
            Self::discover_paths().await?
        };
        let mut explicit_paths: Vec<PathBuf> = Vec::new();
        if let Some(env_path) = env_config_file {
            explicit_paths.push(PathBuf::from(env_path));
        }
        if let Some(explicit) = inputs.explicit_path {
            explicit_paths.push(explicit.to_path_buf());
        }

        // Under `OCX_NO_CONFIG` the recorded extra-CA tier is `System` exactly when the filter keeps
        // the pair, so it needs no reset.
        let (mut discovered_config, discovered_extra_ca_tier) = Self::load_and_merge_recording(&discovered).await?;
        if no_config {
            Self::retain_system_locked_sections(&mut discovered_config);
        }
        // After the `OCX_NO_CONFIG` filter, or the unlocked compiled-in entry is dropped.
        let mut base = Self::builtin_defaults();
        base.merge(discovered_config);
        // The explicit overlay merges on top of the managed fold, never under it.
        let (overlay, overlay_extra_ca_tier) = Self::load_and_merge_recording(&explicit_paths).await?;
        // The overlay folds from an empty accumulator, so the lock is checked here, against `base`.
        let overlay_extra_ca_tier = overlay_extra_ca_tier
            .filter(|_| !Self::system_lock_drops_extra_ca(&base, &overlay, crate::ConfigTier::Explicit));
        let extra_ca_certs_tier_local = overlay_extra_ca_tier.or(discovered_extra_ca_tier);

        let mut local_only = base.clone();
        local_only.merge(overlay.clone());

        // Resolved from `local_only`, so an overlay-only `[managed].source` still folds its payload.
        let (
            mut merged,
            managed_config_snapshot,
            resolved_managed_config,
            managed_snapshot_state,
            managed_extra_ca_tier,
        ) = Self::fold_managed_tier(base.clone(), &local_only).await?;
        merged.merge(overlay.clone());
        let extra_ca_certs_tier = overlay_extra_ca_tier
            .or(managed_extra_ca_tier)
            .or(discovered_extra_ca_tier);

        // Candidates, not survivors: a grant added to a not-yet-existing tier file is a change.
        let mut config_tier_paths: Vec<PathBuf> = if no_config {
            vec![Self::system_path()]
        } else {
            Self::tier_candidates()
        };
        config_tier_paths.extend(explicit_paths);

        Ok(LoadedConfig {
            merged,
            local_only,
            base,
            overlay,
            managed_config_snapshot,
            resolved_managed_config,
            managed_snapshot_state,
            config_tier_paths,
            extra_ca_certs_tier,
            extra_ca_certs_tier_local,
        })
    }

    /// The compiled-in base tier: routes `ocx.sh` through
    /// [`DEFAULT_INDEX_BASE_URL`](crate::index::DEFAULT_INDEX_BASE_URL) as config, not a downstream
    /// special case, so every override applies. Not gated on `OCX_NO_CONFIG`.
    fn builtin_defaults() -> Config {
        Config {
            registries: Some(HashMap::from([(
                ocx_oci::OCX_SH_REGISTRY.to_string(),
                crate::RegistryConfig {
                    index: Some(crate::index::DEFAULT_INDEX_BASE_URL.to_string()),
                    index_is_compiled_default: true,
                    ..Default::default()
                },
            )])),
            ..Default::default()
        }
    }

    /// The managed snapshot's path, or `None` under `OCX_NO_CONFIG=1`.
    fn managed_snapshot_candidate() -> Option<PathBuf> {
        if ocx_env::OCX_NO_CONFIG.bool_or(false).unwrap_or(false) {
            return None;
        }
        let ocx_home = crate::home::default_ocx_root()?;
        Some(crate::managed_config::ManagedConfigPaths::for_ocx_home(&ocx_home).snapshot_file())
    }

    /// Identity-gated, one-hop-stripped merge of the managed snapshot onto `accumulator`; zero
    /// network. Every absence is a silent no-op; the tuple feeds the [`LoadedConfig`] fields.
    async fn fold_managed_tier(
        accumulator: Config,
        local_only: &Config,
    ) -> Result<(
        Config,
        Option<crate::managed::ManagedConfigSnapshot>,
        Option<crate::managed::ResolvedManagedConfig>,
        crate::managed::ManagedSnapshotState,
        Option<crate::ConfigTier>,
    )> {
        use crate::managed::ManagedSnapshotState;

        // From `local_only`, overlay included, or an overlay-only seed arms `required` while its
        // payload never folds here.
        let env_override = if ocx_env::OCX_NO_CONFIG.bool_or(false).unwrap_or(false) {
            None
        } else {
            ocx_env::OCX_MANAGED_CONFIG.get()
        };
        // Shared with the `required` gate, or a locked source A plus `OCX_MANAGED_CONFIG=B` skips
        // the fold here while `required` reports A satisfied.
        let Some(resolved) = crate::managed::resolve_managed_target(local_only, env_override.as_deref())
            .ok()
            .flatten()
        else {
            return Ok((accumulator, None, None, ManagedSnapshotState::Unmatched, None));
        };

        // Only now, so a non-managed user never pays the stat.
        let Some(candidate) = Self::managed_snapshot_candidate() else {
            return Ok((accumulator, None, Some(resolved), ManagedSnapshotState::Unmatched, None));
        };
        let Some(snapshot) = crate::managed_config::read_managed_config_snapshot_at(&candidate).await else {
            return Ok((accumulator, None, Some(resolved), ManagedSnapshotState::Unmatched, None));
        };

        // Never apply a snapshot fetched under another identity, even when not `required`.
        let identity_matches = crate::managed::snapshot_matches_source(&snapshot, &resolved.source);
        if !identity_matches {
            log::debug!(
                "managed-config snapshot source does not match the effective source '{}'; treating as absent",
                resolved.source
            );
            return Ok((
                accumulator,
                Some(snapshot),
                Some(resolved),
                ManagedSnapshotState::Unmatched,
                None,
            ));
        }

        let payload = Self::strip_managed_update(&snapshot.config, &resolved.source);
        let mut parsed: Config = match Self::parse_config_stripping_refused_consent(&payload, &resolved.source) {
            Ok(parsed) => parsed,
            Err(source) => {
                // Not benign: WARN even when not `required`, where nothing else would report it.
                log::warn!(
                    "managed-config snapshot for '{}' is not a usable config and was not applied; re-sync with \
                     `ocx config update` ({source})",
                    resolved.source
                );
                return Ok((
                    accumulator,
                    Some(snapshot),
                    Some(resolved),
                    ManagedSnapshotState::PayloadUnusable,
                    None,
                ));
            }
        };
        // One-hop: a remote payload can never redirect or loosen the tier that fetched it.
        if parsed.managed.take().is_some() {
            log::warn!(
                "managed-config payload for '{}' contained a [managed] section; stripped before merge (a remote \
                 payload can never redirect the tier that fetched it)",
                resolved.source
            );
        }

        // Reached only if `strip_managed_update` could not re-serialize; a payload never sets `[update]`.
        parsed.update = None;
        Self::guard_managed_sigstore_trust(&mut parsed, &resolved.source);
        Self::guard_managed_shell_consent(&mut parsed, &resolved.source);
        Self::stamp_shell_tier(&mut parsed, crate::ConfigTier::Managed);
        let extra_ca_certs_tier = (Self::sets_extra_ca_certs(&parsed)
            && !Self::system_lock_drops_extra_ca(&accumulator, &parsed, crate::ConfigTier::Managed))
        .then_some(crate::ConfigTier::Managed);

        let mut accumulator = accumulator;
        accumulator.merge(parsed);
        Ok((
            accumulator,
            Some(snapshot),
            Some(resolved),
            ManagedSnapshotState::Applied,
            extra_ca_certs_tier,
        ))
    }

    /// `payload` without its `update` key, removed from the raw table before the typed parse.
    ///
    /// `[update]` is personal: a payload that could set `self = "apply"` would replace the fleet's
    /// binaries, and a shape this ocx cannot read must not make the rest of the payload unusable.
    fn strip_managed_update<'a>(payload: &'a str, source: &ocx_oci::OciIdentifier) -> std::borrow::Cow<'a, str> {
        let Ok(mut table) = toml::from_str::<toml::Table>(payload) else {
            return std::borrow::Cow::Borrowed(payload);
        };
        if table.remove("update").is_none() {
            return std::borrow::Cow::Borrowed(payload);
        }
        log::debug!(
            "managed-config payload for '{source}' contained an [update] section; ignored ([update] is read from local config.toml only)"
        );
        // A table that just parsed always serializes; on failure the typed parse still tolerates `[update]`.
        toml::to_string(&table).map_or(std::borrow::Cow::Borrowed(payload), std::borrow::Cow::Owned)
    }

    /// Strips the `[trust]` values a remote payload is not entitled to set.
    ///
    /// A path form names the publisher's disk, on a fleet machine absent or an unrelated local file
    /// read on every verification.
    fn guard_managed_sigstore_trust(parsed: &mut Config, source: &ocx_oci::OciIdentifier) {
        // `extra_ca_certs_pem` is honoured even unpinned: it bypasses no signature verification
        // (`adr_managed_config_tier.md`), and pinning it would break every unpinned fleet's CA rollout.
        if parsed.extra_ca_certs.take().is_some() {
            log::warn!(
                "managed-config payload for '{source}' set extra_ca_certs to a local path; ignored (a remote payload \
                 cannot name a path on this machine — publish with `ocx config push`, which inlines the file as \
                 extra_ca_certs_pem)"
            );
        }
        let Some(trust) = parsed.trust.as_mut() else {
            return;
        };
        // `key_pem` stays: it travels inline, bounded, and names no file.
        for policy in &mut trust.policy {
            for signer in &mut policy.signers {
                // `unusable`: removing the path left the entry with no key at all.
                let (dropped, unusable) = match signer {
                    ocx_trust::SignerSpec::Key(matcher) => {
                        let dropped = matcher.key.take().is_some();
                        (dropped, dropped && matcher.key_pem.is_none())
                    }
                    _ => (false, false),
                };
                if !dropped {
                    continue;
                }
                log::warn!(
                    "managed-config payload for '{source}' declares a [[trust.policy]] key signer by path; \
                     ignored (a remote payload cannot name a file on this machine — publish the key inline \
                     as `key_pem`)"
                );
                if unusable {
                    // A keyless `KeyMatcher` fails `validate_signers` and takes the whole policy down;
                    // `Unknown` narrows instead.
                    *signer = ocx_trust::SignerSpec::Unknown;
                }
            }
        }
        let Some(sigstore) = trust.sigstore.as_mut() else {
            return;
        };
        if sigstore.trusted_root.take().is_some() {
            log::warn!(
                "managed-config payload for '{source}' set [trust.sigstore] trusted_root to a local path; ignored (a                  remote payload cannot name a path on this machine — publish with `ocx config push`, which inlines                  the file as trusted_root_json)"
            );
        }
        // Unpinned, whoever can move the tag could swap the trust root or repoint `fulcio_url`, which
        // receives the OIDC identity token.
        if source.digest().is_none() {
            if sigstore.trusted_root_json.take().is_some() {
                log::warn!(
                    "managed-config payload for '{source}' carries [trust.sigstore] trusted_root_json but the [managed]                      source is not digest-pinned; ignored (pin the source to a digest so the trust root cannot be                      swapped by whoever can move the tag)"
                );
            }
            for (field, name) in [
                (&mut sigstore.fulcio_url, "fulcio_url"),
                (&mut sigstore.rekor_url, "rekor_url"),
            ] {
                if field.take().is_some() {
                    log::warn!(
                        "managed-config payload for '{source}' sets [trust.sigstore] {name} but the [managed] source is                          not digest-pinned; ignored (pin the source to a digest so the Sigstore endpoints cannot be                          repointed by whoever can move the tag)"
                    );
                }
            }
        }
    }

    /// Discover the existing system, user and home config files, lowest precedence first.
    ///
    /// A symlinked or unreadable candidate is skipped with a warning, or a writable tier path could
    /// link any readable file and leak it through a parse error.
    ///
    /// # Errors
    /// Returns [`Error::SystemConfig`] when the SYSTEM candidate exists but
    /// cannot be consulted.
    pub async fn discover_paths() -> std::result::Result<Vec<PathBuf>, Error> {
        Self::existing_candidates(Self::tier_candidates()).await
    }

    /// The discovered-tier candidates, lowest precedence first, before any filesystem check.
    fn tier_candidates() -> Vec<PathBuf> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        candidates.push(Self::system_path());
        if let Some(user) = Self::user_path() {
            candidates.push(user);
        }
        if let Some(home) = Self::home_path() {
            candidates.push(home);
        }
        candidates
    }

    /// Filesystem half of [`Self::discover_paths`], shared with the `OCX_NO_CONFIG` path so the
    /// system candidate gets the same checks there.
    ///
    /// # Errors
    /// Returns [`Error::SystemConfig`] when the SYSTEM candidate exists but
    /// cannot be consulted.
    async fn existing_candidates(candidates: Vec<PathBuf>) -> std::result::Result<Vec<PathBuf>, Error> {
        // `symlink_metadata`, not `try_exists`, or the symlink branch never sees the link.
        let system = Self::system_path();
        let checks = join_all(candidates.iter().map(tokio::fs::symlink_metadata)).await;
        let mut kept = Vec::with_capacity(candidates.len());
        for (path, result) in candidates.into_iter().zip(checks) {
            // Anything but absence is fatal here: skipping it skips every lock `apply_system_locks`
            // would clamp. Compared as `load_and_merge` does, so both agree which file it is.
            let is_system = path == system;
            match result {
                Ok(meta) if meta.file_type().is_symlink() => {
                    if is_system {
                        return Err(Error::SystemConfig {
                            path,
                            source: std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "system config file must not be a symlink",
                            ),
                        });
                    }
                    log::warn!(
                        "skipping symlinked config candidate {} (discovered-tier config files must not be symlinks)",
                        path.display()
                    );
                }
                Ok(_) => kept.push(path),
                // Silent on every tier: no `/etc/ocx/config.toml` is the ordinary case.
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    if is_system {
                        return Err(Error::SystemConfig { path, source });
                    }
                    log::warn!("skipping unreadable config candidate {}: {source}", path.display());
                }
            }
        }
        Ok(kept)
    }

    /// Resolve the project-tier `ocx.toml`: `--project` > `OCX_PROJECT` > CWD walk > `None`, with
    /// no `$OCX_HOME/ocx.toml` fallback.
    ///
    /// `OCX_NO_PROJECT=1` prunes the env var and the walk, never the flag
    /// (`adr_project_toolchain_config.md` § Amendment G).
    ///
    /// # Errors
    /// [`Error::FileNotFound`] when an explicit source names a missing path; a walk miss is
    /// `Ok(None)`.
    pub async fn project_path(
        cwd: Option<&Path>,
        explicit: Option<&Path>,
    ) -> std::result::Result<Option<PathBuf>, crate::error::Error> {
        // Tiers 1–2: an explicit selection.
        if let Some(path) = Self::explicit_project(explicit) {
            return Self::resolve_explicit_project_path(&path).await;
        }

        if ocx_env::OCX_NO_PROJECT.bool_or(false).unwrap_or(false) {
            return Ok(None);
        }

        // Tier 3: CWD walk.
        let walk_result = match cwd {
            Some(start) => {
                // Normalized, or a relative ceiling (`<cwd>/..`) equals no walk level and never fires.
                let ceiling = ocx_env::OCX_CEILING_PATH
                    .get()
                    .map(|value| ocx_util::fs::path::lexical_normalize(&start.join(value)));
                Self::walk_for_project_file(start, ceiling.as_deref()).await
            }
            None => None,
        };
        Ok(walk_result)
    }

    /// The explicit project selection in effect: `--project` outranks `OCX_PROJECT`, which
    /// `OCX_NO_PROJECT=1` prunes and an empty value unsets.
    ///
    /// Lets a caller given `None` by [`Self::project_path`] name the directory it looked in.
    #[must_use]
    pub fn explicit_project(flag: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = flag {
            return Some(path.to_path_buf());
        }
        if ocx_env::OCX_NO_PROJECT.bool_or(false).unwrap_or(false) {
            return None;
        }
        let raw_env = ocx_env::OCX_PROJECT
            .get_raw()
            .and_then(|value| value.into_string().ok());
        if raw_env.as_deref() == Some("") {
            log::debug!("OCX_PROJECT is set to empty string — skipped via escape hatch");
        }
        raw_env.filter(|s| !s.is_empty()).map(PathBuf::from)
    }

    /// Resolve an explicit project path, following symlinks: a file under any name, or a directory's
    /// `ocx.toml`, where none is `Ok(None)` (no project), not `FileNotFound`.
    ///
    /// Requiring a regular file would fail `--project .` and every toolchain trampoline.
    async fn resolve_explicit_project_path(path: &Path) -> std::result::Result<Option<PathBuf>, crate::error::Error> {
        match tokio::fs::metadata(path).await {
            Ok(meta) if meta.file_type().is_file() => Ok(Some(path.to_path_buf())),
            Ok(meta) if meta.is_dir() => {
                let candidate = path.join(PROJECT_FILE_NAME);
                match tokio::fs::metadata(&candidate).await {
                    Ok(meta) if meta.file_type().is_file() => Ok(Some(candidate)),
                    Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Ok(_) => Err(Error::Io {
                        path: candidate,
                        tier: ConfigSource::Project,
                        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
                    }),
                    Err(source) => Err(Error::Io {
                        path: candidate,
                        tier: ConfigSource::Project,
                        source,
                    }),
                }
            }
            Ok(_) => {
                // A device or FIFO fails now, or the defect surfaces only at parse time.
                Err(Error::Io {
                    path: path.to_path_buf(),
                    tier: ConfigSource::Project,
                    source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
                })
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Err(Error::FileNotFound {
                path: path.to_path_buf(),
                tier: ConfigSource::Project,
            }),
            // Any other I/O error fails too, never a phantom success for an explicit caller.
            Err(source) => Err(Error::Io {
                path: path.to_path_buf(),
                tier: ConfigSource::Project,
                source,
            }),
        }
    }

    /// Walk up from `start` for `ocx.toml`, stopping at the first hit, a `.git` boundary, or
    /// `ceiling`; a symlinked candidate is skipped.
    ///
    /// `$OCX_HOME/ocx.toml` is skipped, not a boundary: it is the global manifest
    /// (`adr_global_toolchain_tier.md`), and a project enclosing `$OCX_HOME` must stay findable.
    async fn walk_for_project_file(start: &Path, ceiling: Option<&Path>) -> Option<PathBuf> {
        // Normalized, or a relative `OCX_HOME` equals no walk level and the skip never fires.
        let ocx_home =
            crate::home::default_ocx_root().map(|root| ocx_util::fs::path::lexical_normalize(&start.join(root)));
        let mut current = start;
        loop {
            // The candidate check precedes the `.git` gate: an `ocx.toml` at the repo root wins.
            let candidate = current.join(PROJECT_FILE_NAME);
            let (git_present, candidate_meta) =
                tokio::join!(Self::has_git_dir(current), tokio::fs::symlink_metadata(&candidate),);

            match &candidate_meta {
                // Debug, not warn: a working directory under `$OCX_HOME` is ordinary.
                Ok(meta) if meta.file_type().is_file() && ocx_home.as_deref() == Some(current) => {
                    log::debug!(
                        "skipping global toolchain manifest {} during the CWD walk (reachable only via --global/OCX_GLOBAL)",
                        candidate.display()
                    );
                }
                Ok(meta) if meta.file_type().is_file() => {
                    return Some(candidate);
                }
                Ok(meta) if meta.file_type().is_symlink() => {
                    log::warn!(
                        "skipping symlinked project candidate {} (CWD walk rejects symlinks; use --project or OCX_PROJECT to opt in)",
                        candidate.display()
                    );
                }
                Ok(_) => {
                    // A non-regular `ocx.toml` reads as absent.
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    log::warn!(
                        "skipping unreadable project candidate {}: {source}",
                        candidate.display()
                    );
                }
            }

            if git_present {
                return None;
            }

            // After probing: the ceiling bounds going above it, not reading at it.
            if let Some(ceiling) = ceiling
                && current == ceiling
            {
                return None;
            }

            match current.parent() {
                Some(parent) => current = parent,
                None => return None,
            }
        }
    }

    /// Whether `dir/.git` exists as any entry, file or symlink included, without following it.
    ///
    /// Fail-closed: an I/O error other than `NotFound` counts as a boundary.
    async fn has_git_dir(dir: &Path) -> bool {
        match tokio::fs::symlink_metadata(dir.join(".git")).await {
            Ok(_) => true,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => false,
            Err(source) => {
                log::warn!(
                    "treating {}/.git as a repository boundary due to I/O error: {source}",
                    dir.display()
                );
                true
            }
        }
    }

    /// Load and merge config files, lowest precedence first.
    ///
    /// # Errors
    /// Returns an error if any file is missing, unreadable, exceeds
    /// [`MAX_CONFIG_SIZE`], or contains invalid TOML.
    pub async fn load_and_merge<P: AsRef<Path>>(paths: &[P]) -> Result<Config> {
        Ok(Self::load_and_merge_recording(paths).await?.0)
    }

    /// Whether `contribution` sets either extra-CA key, the pair [`Config::merge`] replaces whole.
    fn sets_extra_ca_certs(contribution: &Config) -> bool {
        contribution.extra_ca_certs.is_some() || contribution.extra_ca_certs_pem.is_some()
    }

    /// Whether `accumulator`'s system lock drops the extra-CA pair `contribution` sets, warning once
    /// when it does; the warning never names a value, since a pasted bundle is not for the log.
    fn system_lock_drops_extra_ca(accumulator: &Config, contribution: &Config, tier: crate::ConfigTier) -> bool {
        let dropped = accumulator.extra_ca_certs_system_locked && Self::sets_extra_ca_certs(contribution);
        if dropped {
            log::warn!(
                "ignoring extra_ca_certs / extra_ca_certs_pem from {tier}: the pair is locked by {}; edit the system \
                 tier or ask its owner",
                Self::system_path().display()
            );
        }
        dropped
    }

    /// [`Self::load_and_merge`], also recording the last tier in `paths` that
    /// set `extra_ca_certs` / `extra_ca_certs_pem`.
    ///
    /// # Errors
    /// Same as [`Self::load_and_merge`].
    async fn load_and_merge_recording<P: AsRef<Path>>(paths: &[P]) -> Result<(Config, Option<crate::ConfigTier>)> {
        let mut config = Config::default();
        let mut extra_ca_certs_tier = None;
        for path in paths {
            let path = path.as_ref();
            // Stat first: on Windows, opening a directory fails "Access is denied" before `is_file()`.
            if let Ok(meta) = tokio::fs::metadata(path).await
                && !meta.file_type().is_file()
            {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    tier: ConfigSource::Config,
                    source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "config path is not a regular file"),
                });
            }
            let file = match tokio::fs::File::open(path).await {
                Ok(f) => f,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    return Err(Error::FileNotFound {
                        path: path.to_path_buf(),
                        tier: ConfigSource::Config,
                    });
                }
                Err(source) => {
                    return Err(Error::Io {
                        path: path.to_path_buf(),
                        tier: ConfigSource::Config,
                        source,
                    });
                }
            };
            let metadata = file.metadata().await.map_err(|source| Error::Io {
                path: path.to_path_buf(),
                tier: ConfigSource::Config,
                source,
            })?;
            if !metadata.file_type().is_file() {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    tier: ConfigSource::Config,
                    source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "config path is not a regular file"),
                });
            }
            if metadata.len() > MAX_CONFIG_SIZE {
                return Err(Error::FileTooLarge {
                    path: path.to_path_buf(),
                    size: metadata.len(),
                    limit: MAX_CONFIG_SIZE,
                });
            }
            // Bounded, or a zero-length synthetic file (`/proc/self/mem`) bypasses the cap.
            let mut contents = String::new();
            let mut taken = file.take(MAX_CONFIG_SIZE + 1);
            taken.read_to_string(&mut contents).await.map_err(|source| Error::Io {
                path: path.to_path_buf(),
                tier: ConfigSource::Config,
                source,
            })?;
            if contents.len() as u64 > MAX_CONFIG_SIZE {
                return Err(Error::FileTooLarge {
                    path: path.to_path_buf(),
                    size: contents.len() as u64,
                    limit: MAX_CONFIG_SIZE,
                });
            }
            let mut parsed: Config =
                Self::parse_config_stripping_refused_consent(&contents, path.display()).map_err(|source| {
                    Error::Parse {
                        path: path.to_path_buf(),
                        source: Box::new(source),
                    }
                })?;
            if let Some(update) = parsed.update.as_mut() {
                update.set_origin(path);
            }
            // Only the system file locks; it folds in first, so every lower tier sees its locks.
            if path == Self::system_path().as_path() {
                Self::apply_system_locks(&mut parsed);
            }
            Self::anchor_relative_paths(&mut parsed, path);
            Self::guard_extra_ca_certs_ambiguity(&parsed, path)?;
            let tier = Self::tier_for_path(path);
            Self::stamp_shell_tier(&mut parsed, tier);
            if Self::sets_extra_ca_certs(&parsed) && !Self::system_lock_drops_extra_ca(&config, &parsed, tier) {
                extra_ca_certs_tier = Some(tier);
            }
            config.merge(parsed);
        }
        Ok((config, extra_ca_certs_tier))
    }

    /// Refuse one file declaring both `extra_ca_certs` and `extra_ca_certs_pem`; across tiers,
    /// [`Config::merge`]'s XOR applies instead.
    ///
    /// # Errors
    /// Returns [`Error::AmbiguousExtraCaCerts`] when `parsed` sets both keys.
    fn guard_extra_ca_certs_ambiguity(parsed: &Config, path: &Path) -> Result<()> {
        if parsed.extra_ca_certs.is_some() && parsed.extra_ca_certs_pem.is_some() {
            return Err(Error::AmbiguousExtraCaCerts {
                path: path.to_path_buf(),
            });
        }
        Ok(())
    }

    /// Lock the system-scope config's lockable sections and extra-CA pair before it merges, so each
    /// ignores every lower tier, the managed payload included.
    fn apply_system_locks(parsed: &mut Config) {
        // Only if set here, or an operator declaring no root freezes lower tiers out of adding one.
        parsed.extra_ca_certs_system_locked = Self::sets_extra_ca_certs(parsed);
        // Conditional, inside `PatchConfig::lock_as_system`.
        if let Some(patches) = parsed.patches.as_mut() {
            patches.lock_as_system();
        }
        // `[registry]` and each `[registries.<name>]` entry lock unconditionally.
        if let Some(registry) = parsed.registry.as_mut() {
            registry.lock_as_system();
        }
        if let Some(registries) = parsed.registries.as_mut() {
            for entry in registries.values_mut() {
                entry.lock_as_system();
            }
        }
        // Each mirror entry locks only the role(s) it declares (`adr_index_indirection.md`).
        if let Some(mirrors) = parsed.mirrors.as_mut() {
            for mirror in mirrors.values_mut() {
                mirror.lock_as_system();
            }
        }
        // Required-gated like `[patches]`, or the home tier's fence could loosen a required seed.
        if let Some(managed) = parsed.managed.as_mut() {
            managed.lock_as_system();
        }
        // A locked policy pins its scopes' specificity: a lower tier may join its ANY-of set, never
        // outbid it narrower.
        if let Some(trust) = parsed.trust.as_mut() {
            trust.lock_as_system();
        }
        // Unconditional and per block: `dir`, `name` and `required` clamp together.
        if let Some(records) = parsed.records.as_mut() {
            records.lock_as_system();
        }
    }

    /// Which tier `path` belongs to; anything but a discovered candidate is explicit.
    fn tier_for_path(path: &Path) -> crate::ConfigTier {
        use crate::ConfigTier;

        if path == Self::system_path().as_path() {
            return ConfigTier::System;
        }
        if Self::user_path().is_some_and(|user| path == user.as_path()) {
            return ConfigTier::User;
        }
        if Self::home_path().is_some_and(|home| path == home.as_path()) {
            return ConfigTier::Home;
        }
        ConfigTier::Explicit
    }

    /// Record which tier set `[shell] hook` / `completions`, only where this file set the scalar,
    /// so `ShellConfig::merge` never credits a silent tier.
    fn stamp_shell_tier(parsed: &mut Config, tier: crate::ConfigTier) {
        let Some(shell) = parsed.shell.as_mut() else {
            return;
        };
        if shell.hook.is_some() {
            shell.hook_tier = Some(tier);
        }
        if shell.completions.is_some() {
            shell.completions_tier = Some(tier);
        }
    }

    /// Parse one `config.toml` payload, dropping a refused `[shell.consent]` table rather than
    /// failing the whole file, which would take `[registries]`, `[mirrors]` and `[[trust.policy]]`
    /// down with it (`adr_shell_env_overhaul.md` § Rationale from code: ocx_config).
    ///
    /// # Errors
    ///
    /// The original `toml` error for any other failure, for a refused table that withdraws a grant,
    /// and for an ill-typed table, the operator's own typo.
    fn parse_config_stripping_refused_consent(
        text: &str,
        origin: impl std::fmt::Display,
    ) -> std::result::Result<Config, toml::de::Error> {
        let refusal = match toml::from_str::<Config>(text) {
            Ok(parsed) => return Ok(parsed),
            Err(refusal) => refusal,
        };
        // Edit the parsed table, never the text: string surgery can mangle a neighbouring section.
        let Ok(mut table) = toml::from_str::<toml::Table>(text) else {
            return Err(refusal);
        };
        // Dropping a withdrawal widens: another tier's `include` would stand unopposed.
        if Self::consent_table_withdraws(&table) {
            return Err(refusal);
        }
        // Only a refusal earns the strip, never a typo; `namespaces = 123` also parses once removed.
        if !Self::consent_table_shape_is_readable(&table) {
            return Err(refusal);
        }
        let removed = table
            .get_mut("shell")
            .and_then(toml::Value::as_table_mut)
            .and_then(|shell| shell.remove("consent"))
            .is_some();
        if !removed {
            return Err(refusal);
        }
        let Ok(mut parsed) = toml::Value::Table(table).try_into::<Config>() else {
            return Err(refusal);
        };
        let reason = format!(
            "[shell.consent] in {origin} was refused and dropped; every other section of that file still applies \
             ({refusal}) — a consent grant fails closed, so nothing activates through it until the table is fixed"
        );
        log::warn!("{reason}");
        parsed
            .shell
            .get_or_insert_with(crate::ShellConfig::default)
            .consent_strip_reason = Some(reason);
        Ok(parsed)
    }

    /// Whether the raw `[shell.consent]` carries a non-empty `namespaces.exclude`.
    ///
    /// An unreadable `exclude` counts as a withdrawal: it is exactly a newer ocx's narrowing.
    fn consent_table_withdraws(table: &toml::Table) -> bool {
        let Some(exclude) = table
            .get("shell")
            .and_then(toml::Value::as_table)
            .and_then(|shell| shell.get("consent"))
            .and_then(toml::Value::as_table)
            .and_then(|consent| consent.get("namespaces"))
            .and_then(toml::Value::as_table)
            .and_then(|namespaces| namespaces.get("exclude"))
        else {
            return false;
        };
        exclude.as_array().is_none_or(|patterns| !patterns.is_empty())
    }

    /// Whether the raw `[shell.consent]` has the TOML shape `ShellConsent` expects, separating a
    /// refusal from the operator's typo, which owes them the error.
    ///
    /// Shape, never policy: re-validating patterns here would drift from the real validator.
    fn consent_table_shape_is_readable(table: &toml::Table) -> bool {
        let Some(consent) = table
            .get("shell")
            .and_then(toml::Value::as_table)
            .and_then(|shell| shell.get("consent"))
            .and_then(toml::Value::as_table)
        else {
            return false;
        };
        let is_string_list = |value: &toml::Value| {
            value
                .as_array()
                .is_some_and(|items| items.iter().all(toml::Value::is_str))
        };
        if consent.get("paths").is_some_and(|paths| !is_string_list(paths)) {
            return false;
        }
        match consent.get("namespaces") {
            None => true,
            Some(toml::Value::String(_)) => true,
            Some(toml::Value::Table(spec)) => ["include", "exclude"]
                .iter()
                .all(|key| spec.get(*key).is_none_or(is_string_list)),
            Some(_) => false,
        }
    }

    /// Strip the `[shell.consent]` grant from a managed payload whose source is not digest-pinned,
    /// or whoever can move the tag can swap it (`adr_shell_env_overhaul.md`).
    ///
    /// The reason is recorded on the payload too, since the shims discard stderr.
    fn guard_managed_shell_consent(parsed: &mut Config, source: &ocx_oci::OciIdentifier) {
        use crate::shell::{ConsentScopeSpec, ShellConsent};
        use ocx_trust::ScopeSpec;

        if source.digest().is_some() {
            return;
        }
        let Some(shell) = parsed.shell.as_mut() else {
            return;
        };
        let Some(consent) = shell.consent.take() else {
            return;
        };
        // `exclude` is kept unpinned: dropping it would leave another tier's `include` unopposed, and a
        // tag mover can only take a grant away with it.
        let carve_outs = consent
            .namespaces
            .as_ref()
            .map(|namespaces| namespaces.exclude().to_vec())
            .unwrap_or_default();
        let kept = if carve_outs.is_empty() {
            String::new()
        } else {
            let clause = format!(
                "; its namespaces exclude list ({}) was kept, since a withdrawal can only ever narrow",
                carve_outs.join(", ")
            );
            // `include` equals `exclude`, never empty: an empty `include` is a catch-all, while this
            // grants nothing alone and its `exclude` still opposes other tiers.
            shell.consent = Some(ShellConsent {
                paths: Vec::new(),
                namespaces: Some(ConsentScopeSpec(ScopeSpec::Set {
                    include: carve_outs.clone(),
                    exclude: carve_outs,
                })),
            });
            clause
        };
        let reason = format!(
            "managed-config payload for '{source}' carries [shell.consent] but the [managed] source is not \
             digest-pinned; the grant was ignored{kept} (pin the source to a digest so an activation grant cannot be \
             added by whoever can move the tag)"
        );
        log::warn!("{reason}");
        shell.consent_strip_reason = Some(reason);
    }

    /// Fold a project-tier contribution into `accumulator` without its `[shell]`, `[records]` or `[update]`.
    ///
    /// Stripped here, not left to `ProjectConfig`'s `deny_unknown_fields`: `[shell]` consent from a
    /// repository would let a clone consent to itself, and `[records]` would let it redirect the
    /// operator's audit sink.
    pub fn fold_project_tier(accumulator: &mut Config, mut project_contribution: Config) {
        if project_contribution.shell.take().is_some() {
            log::warn!(
                "a project-tier config declared [shell]; stripped before merge (shell integration and its activation \
                 consent are configured in config.toml, never in a project file)"
            );
        }
        if project_contribution.records.take().is_some() {
            log::warn!(
                "a project-tier config declared [records]; stripped before merge (the execution-record sink is \
                 operator configuration in config.toml, never a repository's to redirect)"
            );
        }
        if project_contribution.update.take().is_some() {
            log::warn!(
                "a project-tier config declared [update]; stripped before merge (update checks are a personal setting \
                 in config.toml, never a repository's to switch on)"
            );
        }
        accumulator.merge(project_contribution);
    }

    /// Resolve every relative path a config file declares against that file's directory, so a
    /// daemon, a CI runner and a shell name the same file.
    ///
    /// All four keys anchor in this one seam, or they drift.
    fn anchor_relative_paths(parsed: &mut Config, config_path: &Path) {
        let Some(dir) = config_path.parent() else {
            return;
        };
        if let Some(records) = parsed.records.as_mut()
            && let Some(sink) = records.dir.as_ref()
            && sink.is_relative()
        {
            records.dir = Some(dir.join(sink));
        }
        parsed.anchor_relative_extra_ca_certs(dir);
        let Some(trust) = parsed.trust.as_mut() else {
            return;
        };
        if let Some(sigstore) = trust.sigstore.as_mut() {
            sigstore.anchor_relative_root(dir);
        }
        for policy in &mut trust.policy {
            policy.anchor_relative_keys(dir);
        }
    }

    /// The `OCX_NO_CONFIG=1` counterpart to [`Self::apply_system_locks`]: keep only the sections the
    /// lock pass clamped.
    ///
    /// Pruning the system tier whole would let a CI job drop a locked `[records]` sink with exit 0
    /// (`adr_exec_resolution_record.md`).
    fn retain_system_locked_sections(config: &mut Config) {
        // Exhaustive, so a section added to `Config` cannot reach here without a decision.
        let Config {
            registry,
            registries,
            mirrors,
            patches,
            managed,
            update,
            trust,
            shell,
            records,
            toolchain_dir,
            extra_ca_certs,
            extra_ca_certs_pem,
            extra_ca_certs_system_locked,
        } = config;

        *patches = patches.take().filter(|patches| patches.system_locked);
        *registry = registry.take().filter(|registry| registry.system_locked);
        *records = records.take().filter(|records| records.system_locked);
        // Dropped even when locked: the flag skips the snapshot read, so a `required` tier would fail
        // every hermetic invocation.
        *managed = None;
        // `[shell]` has no locks: all of it is ambient host configuration.
        *shell = None;
        // Not lockable: `[update]` is a personal setting, never operator policy.
        *update = None;
        // No lock either; a hermetic child still inherits its parent's root via `OCX_TOOLCHAIN_DIR`.
        *toolchain_dir = None;
        // A locked pair survives, or a hermetic CI job drops out of the corporate CA with exit 69.
        if !*extra_ca_certs_system_locked {
            *extra_ca_certs = None;
            *extra_ca_certs_pem = None;
        }
        if let Some(entries) = registries.as_mut() {
            entries.retain(|_, entry| entry.system_locked);
        }
        *registries = registries.take().filter(|entries| !entries.is_empty());
        if let Some(entries) = mirrors.as_mut() {
            entries.retain(|_, mirror| mirror.registry_system_locked || mirror.index_system_locked);
        }
        *mirrors = mirrors.take().filter(|entries| !entries.is_empty());
        // Per policy entry, since policies array-append across tiers; `[trust.sigstore]` as a whole.
        if let Some(declared) = trust.as_mut() {
            declared.policy.retain(|policy| policy.system_locked);
            declared.sigstore = declared.sigstore.take().filter(|sigstore| sigstore.system_locked);
        }
        *trust = trust
            .take()
            .filter(|declared| !declared.policy.is_empty() || declared.sigstore.is_some());
    }

    /// System config: `/etc/ocx/config.toml`, redirectable through `__OCX_TESTING_SYSTEM_CONFIG` in test
    /// builds only.
    pub fn system_path() -> PathBuf {
        #[cfg(any(test, feature = "__testing"))]
        if let Some(path) = ocx_env::__OCX_TESTING_SYSTEM_CONFIG
            .get_raw()
            .and_then(|value| value.into_string().ok())
        {
            return PathBuf::from(path);
        }
        PathBuf::from("/etc/ocx/config.toml")
    }

    /// User config: `$XDG_CONFIG_HOME/ocx/config.toml` or
    /// `~/.config/ocx/config.toml` (via `dirs::config_dir`).
    pub fn user_path() -> Option<PathBuf> {
        #[cfg(any(test, feature = "__testing"))]
        if ocx_env::overrides::is_hermetic() {
            return hermetic_config_dir().map(|d| d.join("ocx").join("config.toml"));
        }
        dirs::config_dir().map(|d| d.join("ocx").join("config.toml"))
    }

    /// `$OCX_HOME/config.toml`, falling back to `~/.ocx/config.toml`.
    // Only through `default_ocx_root`, the one definition of the `$OCX_HOME` default.
    pub fn home_path() -> Option<PathBuf> {
        crate::home::default_ocx_root().map(|d| d.join("config.toml"))
    }

    /// `$OCX_HOME/sigstore/trusted-root.json`, the trust root verification finds with no flag or
    /// config.
    // Not under `state/`, which the tool may discard: this is a durable operator asset.
    pub fn home_sigstore_trusted_root_path() -> Option<PathBuf> {
        crate::home::default_ocx_root().map(|d| d.join("sigstore").join("trusted-root.json"))
    }
}

/// [`dirs::config_dir`]'s answer from the hermetic override table; `dirs` reads the process
/// environment itself and would load the developer's own user tier.
#[cfg(any(test, feature = "__testing"))]
fn hermetic_config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return ocx_env::APPDATA
            .get_raw()
            .and_then(|value| value.into_string().ok())
            .map(PathBuf::from);
    }
    if cfg!(target_os = "macos") {
        return ocx_env::home_dir().map(|home| home.join("Library").join("Application Support"));
    }
    ocx_env::XDG_CONFIG_HOME
        .get()
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| ocx_env::home_dir().map(|home| home.join(".config")))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    // ── Helper ───────────────────────────────────────────────────────────────

    fn write_config(dir: &TempDir, filename: &str, content: &str) -> PathBuf {
        let path = dir.path().join(filename);
        std::fs::write(&path, content).expect("write test config");
        path
    }

    /// Point the SYSTEM tier at a path that does not exist.
    ///
    /// `OCX_NO_CONFIG=1` no longer prunes the system tier (its locked sections
    /// survive the flag), so a developer machine that happens to carry a real
    /// `/etc/ocx/config.toml` would otherwise leak into every hermetic-mode
    /// assertion below. Same rationale as `EnvLock::isolate_project_home`.
    fn without_system_config(env: &ocx_env::overrides::EnvLock) {
        env.set(
            &ocx_env::__OCX_TESTING_SYSTEM_CONFIG,
            "/nonexistent/ocx-test-system/config.toml",
        );
    }

    /// Point the SYSTEM tier at `content` written into `dir`, so the
    /// `lock_as_system` pass runs against it exactly as it does for
    /// `/etc/ocx/config.toml`.
    fn with_system_config(env: &ocx_env::overrides::EnvLock, dir: &TempDir, content: &str) {
        let path = write_config(dir, "system-config.toml", content);
        env.set(
            &ocx_env::__OCX_TESTING_SYSTEM_CONFIG,
            path.to_str().expect("temp path is utf-8"),
        );
    }

    // ── load_and_merge tests (Step 3.3) ──────────────────────────────────────

    #[tokio::test]
    async fn load_and_merge_empty_list_returns_default() {
        // Plan: Step 3.3 — empty list → Config::default()
        let result = ConfigLoader::load_and_merge::<PathBuf>(&[]).await;
        let config = result.expect("empty merge should succeed");
        assert!(config.registry.is_none());
    }

    #[tokio::test]
    async fn load_and_merge_single_file() {
        // Plan: Step 3.3 — single file is loaded and parsed
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "single.toml", "[registry]\ndefault = \"single.example\"");
        let config = ConfigLoader::load_and_merge(&[path])
            .await
            .expect("single file merge should succeed");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("single.example")
        );
    }

    #[tokio::test]
    async fn load_and_merge_two_files_second_wins_on_conflict() {
        // Plan: Step 3.3 — two files both setting [registry] default → second wins
        let dir = TempDir::new().unwrap();
        let first = write_config(&dir, "first.toml", "[registry]\ndefault = \"first.example\"");
        let second = write_config(&dir, "second.toml", "[registry]\ndefault = \"second.example\"");
        let config = ConfigLoader::load_and_merge(&[first, second])
            .await
            .expect("two-file merge should succeed");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("second.example"),
            "second (higher precedence) should win"
        );
    }

    #[tokio::test]
    async fn load_and_merge_two_files_only_first_sets_default() {
        // Plan: Step 3.3 — two files, only first sets [registry] default → preserved
        let dir = TempDir::new().unwrap();
        let first = write_config(&dir, "first.toml", "[registry]\ndefault = \"first.example\"");
        let second = write_config(&dir, "second.toml", "");
        let config = ConfigLoader::load_and_merge(&[first, second])
            .await
            .expect("merge should succeed");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("first.example"),
            "first file's value should be preserved when second doesn't override"
        );
    }

    #[tokio::test]
    async fn load_and_merge_missing_file_returns_error() {
        // Plan: Step 3.3 — missing file in list → error (discovery should have filtered)
        let nonexistent = PathBuf::from("/tmp/this-file-does-not-exist-ocx-test-12345.toml");
        let result = ConfigLoader::load_and_merge(&[nonexistent]).await;
        assert!(result.is_err(), "missing file in load_and_merge should be an error");
    }

    #[tokio::test]
    async fn load_and_merge_invalid_toml_returns_parse_error() {
        // Plan: Step 3.3 — file with invalid TOML → Parse error with file path
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "broken.toml", "this is not valid toml =[[[");
        let result = ConfigLoader::load_and_merge(std::slice::from_ref(&path)).await;
        assert!(result.is_err(), "invalid TOML should produce an error");
        let err = result.unwrap_err();
        let err_str = err.to_string();
        assert!(
            err_str.contains("broken.toml"),
            "error message should contain the file path, got: {err_str}"
        );
    }

    #[tokio::test]
    async fn load_and_merge_file_too_large_returns_error() {
        // A file exceeding MAX_CONFIG_SIZE returns FileTooLarge. The bounded
        // read inside load_and_merge also defends against files whose
        // `metadata.len()` is 0 but whose read is unbounded — use a file with
        // real bytes here; the synthetic case is infeasible to simulate in a
        // portable test.
        let dir = TempDir::new().unwrap();
        let content = "x".repeat(MAX_CONFIG_SIZE as usize + 1);
        let path = write_config(&dir, "huge.toml", &content);
        let result = ConfigLoader::load_and_merge(std::slice::from_ref(&path)).await;
        let err = result.expect_err("oversized file should be rejected");
        let err_str = err.to_string();
        assert!(
            err_str.contains("exceeds maximum allowed size"),
            "error should mention size cap, got: {err_str}"
        );
        assert!(
            err_str.contains("huge.toml"),
            "error should contain the file path, got: {err_str}"
        );
    }

    #[tokio::test]
    async fn load_and_merge_rejects_non_regular_file() {
        // Pointing load_and_merge at a directory triggers the is_file() guard.
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_path_buf();
        let result = ConfigLoader::load_and_merge(&[dir_path]).await;
        let err = result.expect_err("directory path should be rejected");
        // Plan Test 3.1.3: assert the exact substring injected at loader.rs:148.
        // The string "config path is not a regular file" lives in the inner
        // std::io::Error source, so walk the full source chain manually via
        // `std::error::Error::source()` and concatenate each Display.
        use std::error::Error as _;
        let mut err_str = format!("{err}");
        let mut cause: Option<&dyn std::error::Error> = err.source();
        while let Some(c) = cause {
            err_str.push_str(": ");
            err_str.push_str(&c.to_string());
            cause = c.source();
        }
        assert!(
            err_str.contains("config path is not a regular file"),
            "error should contain exact substring 'config path is not a regular file', got: {err_str}"
        );
    }

    #[tokio::test]
    async fn load_and_merge_three_files_precedence_order() {
        // Three tiers each setting `[registry] default`: highest wins, the
        // lower two values are fully replaced.
        let dir = TempDir::new().unwrap();
        let low = write_config(&dir, "low.toml", "[registry]\ndefault = \"low.example\"");
        let mid = write_config(&dir, "mid.toml", "[registry]\ndefault = \"mid.example\"");
        let high = write_config(&dir, "high.toml", "[registry]\ndefault = \"high.example\"");
        let config = ConfigLoader::load_and_merge(&[low, mid, high])
            .await
            .expect("three-file merge should succeed");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("high.example"),
            "highest-precedence tier (third file) should win"
        );
    }

    #[tokio::test]
    async fn load_and_merge_registries_merge_across_tiers() {
        // Two tiers with overlapping [registries.shared] entries + one unique
        // entry each. Higher tier wins on the shared key; both unique entries
        // survive.
        let dir = TempDir::new().unwrap();
        let low = write_config(
            &dir,
            "low.toml",
            "[registries.shared]\nindex = \"https://old.example\"\n\n[registries.only_low]\nindex = \"https://low.example\"",
        );
        let high = write_config(
            &dir,
            "high.toml",
            "[registries.shared]\nindex = \"https://new.example\"\n\n[registries.only_high]\nindex = \"https://high.example\"",
        );
        let config = ConfigLoader::load_and_merge(&[low, high])
            .await
            .expect("two-file registries merge should succeed");
        let registries = config.registries.expect("registries should be present");
        assert_eq!(registries.len(), 3);
        assert_eq!(
            registries["shared"].index.as_deref(),
            Some("https://new.example"),
            "higher tier should win on conflicting key"
        );
        assert_eq!(registries["only_low"].index.as_deref(), Some("https://low.example"));
        assert_eq!(registries["only_high"].index.as_deref(), Some("https://high.example"));
    }

    // ── load() orchestration tests ───────────────────────────────────────────
    //
    // Env-touching tests acquire `ocx_env::overrides::lock()` — a process-wide
    // mutex whose Drop clears all overrides, which every `ocx_env` read consults;
    // no `std::env::set_var`, no `unsafe`.

    #[tokio::test]
    async fn load_with_no_config_returns_default() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        without_system_config(&env);
        env.remove(&ocx_env::OCX_CONFIG);
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("OCX_NO_CONFIG=1 should succeed");
        assert!(
            config.registry.is_none(),
            "OCX_NO_CONFIG=1 with no explicit path should return default config"
        );
    }

    #[tokio::test]
    async fn load_with_no_config_and_explicit_path_loads_only_explicit() {
        // OCX_NO_CONFIG=1 with --config → explicit file still loads.
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "hermetic.toml", "[registry]\ndefault = \"hermetic.example\"");
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        without_system_config(&env);
        env.remove(&ocx_env::OCX_CONFIG);
        let inputs = ConfigInputs {
            explicit_path: Some(&path),
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("OCX_NO_CONFIG=1 with --config should load the explicit file");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("hermetic.example"),
            "the explicit file must load even when OCX_NO_CONFIG=1"
        );
    }

    #[tokio::test]
    async fn load_with_no_config_and_env_path_still_loads_env_path() {
        // OCX_NO_CONFIG=1 with OCX_CONFIG → env-var path still loads.
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "env-hermetic.toml",
            "[registry]\ndefault = \"env-hermetic.example\"",
        );
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        without_system_config(&env);
        env.set(&ocx_env::OCX_CONFIG, path.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("OCX_NO_CONFIG=1 with OCX_CONFIG should load the env file");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("env-hermetic.example"),
            "the env-var path must load even when OCX_NO_CONFIG=1"
        );
    }

    #[tokio::test]
    async fn load_with_empty_ocx_config_file_treats_as_unset() {
        // OCX_CONFIG="" is the escape hatch — treated as unset, not an error.
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        without_system_config(&env);
        env.set(&ocx_env::OCX_CONFIG, "");
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("empty OCX_CONFIG should be treated as unset");
        assert!(config.registry.is_none(), "empty OCX_CONFIG must not load anything");
    }

    #[tokio::test]
    async fn load_with_nonexistent_explicit_path_errors() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        without_system_config(&env);
        env.remove(&ocx_env::OCX_CONFIG);
        let nonexistent = PathBuf::from("/tmp/ocx-test-nonexistent-config-99999.toml");
        let inputs = ConfigInputs {
            explicit_path: Some(&nonexistent),
            explicit_project_path: None,
            cwd: None,
        };
        let result = ConfigLoader::load(inputs).await;
        assert!(result.is_err(), "explicit path to missing file should error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("ocx-test-nonexistent-config-99999.toml"),
            "error should contain the path, got: {err}"
        );
    }

    #[tokio::test]
    async fn load_with_ocx_config_file_env_loads_that_file() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "ci.toml", "[registry]\ndefault = \"ci.example\"");
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        without_system_config(&env);
        env.set(&ocx_env::OCX_CONFIG, path.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs).await.expect("OCX_CONFIG should succeed");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("ci.example")
        );
    }

    /// No managed-config tier exists yet, so `load_with_local_view`'s
    /// `merged` and `local_only` views must carry identical content — both
    /// equal to what `load` returns.
    #[tokio::test]
    async fn load_with_local_view_merged_and_local_only_are_identical() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "ci.toml", "[registry]\ndefault = \"ci.example\"");
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        without_system_config(&env);
        env.set(&ocx_env::OCX_CONFIG, path.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let loaded = ConfigLoader::load_with_local_view(inputs)
            .await
            .expect("load_with_local_view should succeed");
        assert_eq!(
            loaded.merged.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("ci.example")
        );
        assert_eq!(
            loaded.local_only.registry.as_ref().and_then(|r| r.default.as_deref()),
            loaded.merged.registry.as_ref().and_then(|r| r.default.as_deref()),
            "merged and local_only must be identical until a managed-config tier exists"
        );
    }

    #[tokio::test]
    async fn load_with_explicit_path_layers_on_top_of_env_path() {
        // Both OCX_CONFIG and --config set → both load; --config (highest
        // file-tier precedence) wins on conflicting scalars.
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let env_file = write_config(&dir, "env.toml", "[registry]\ndefault = \"env.example\"");
        let explicit_file = write_config(&dir, "explicit.toml", "[registry]\ndefault = \"explicit.example\"");
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        without_system_config(&env);
        env.set(&ocx_env::OCX_CONFIG, env_file.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: Some(&explicit_file),
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("both explicit sources should load");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("explicit.example"),
            "--config should layer on top of OCX_CONFIG and win on conflict"
        );
    }

    // ── Built-in base tier ───────────────────────────────────────────────────
    //
    // The compiled-in tier lives below every file tier, so `OCX_NO_CONFIG=1`
    // (which prunes the discovered chain and the managed snapshot, both
    // ambient host state) is the sharpest way to observe it alone.

    fn builtin_ocx_sh_index(config: &Config) -> Option<&str> {
        config
            .registries
            .as_ref()?
            .get(ocx_oci::OCX_SH_REGISTRY)?
            .index
            .as_deref()
    }

    #[tokio::test]
    async fn builtin_tier_makes_ocx_sh_index_bearing() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.remove(&ocx_env::OCX_CONFIG);
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs).await.expect("load should succeed");
        assert_eq!(
            builtin_ocx_sh_index(&config),
            Some(crate::index::DEFAULT_INDEX_BASE_URL),
            "the compiled-in tier must make ocx.sh index-bearing with no config file at all"
        );
    }

    #[tokio::test]
    async fn builtin_tier_leaves_every_other_namespace_plain_oci() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.remove(&ocx_env::OCX_CONFIG);
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs).await.expect("load should succeed");
        let names: Vec<&str> = config
            .registries
            .as_ref()
            .expect("built-in tier seeds a registries table")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            names,
            vec![ocx_oci::OCX_SH_REGISTRY],
            "the built-in tier must seed ocx.sh and nothing else"
        );
    }

    #[tokio::test]
    async fn user_index_overrides_the_builtin_ocx_sh_index() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "corp.toml",
            "[registries.\"ocx.sh\"]\nindex = \"https://index.corp.example\"\n",
        );
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.set(&ocx_env::OCX_CONFIG, path.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs).await.expect("load should succeed");
        assert_eq!(
            builtin_ocx_sh_index(&config),
            Some("https://index.corp.example"),
            "a user-supplied index must beat the compiled-in one"
        );
    }

    /// The documented off-switch: an empty `index` is a declared value, so it
    /// overrides the built-in one, and `build_index_sources` skips an empty
    /// base URL — `ocx.sh` falls back to plain OCI.
    #[tokio::test]
    async fn empty_user_index_disables_the_builtin_ocx_sh_index() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "plain.toml", "[registries.\"ocx.sh\"]\nindex = \"\"\n");
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.set(&ocx_env::OCX_CONFIG, path.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs).await.expect("load should succeed");
        assert_eq!(
            builtin_ocx_sh_index(&config),
            Some(""),
            "index = \"\" must clear the compiled-in index, not be ignored as unset"
        );
    }

    /// A `[registries."ocx.sh"]` entry that sets only `trusted_hosts` must not
    /// erase the built-in `index` — the table merges key-by-key, field-wise.
    #[tokio::test]
    async fn user_entry_without_index_keeps_the_builtin_ocx_sh_index() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "trusted.toml",
            "[registries.\"ocx.sh\"]\ntrusted_hosts = [\"registry.corp\"]\n",
        );
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.set(&ocx_env::OCX_CONFIG, path.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs).await.expect("load should succeed");
        assert_eq!(
            builtin_ocx_sh_index(&config),
            Some(crate::index::DEFAULT_INDEX_BASE_URL)
        );
    }

    /// The tier the feature is actually layered under. Every other test here
    /// sets `OCX_NO_CONFIG=1` and overrides through `OCX_CONFIG` — the
    /// HIGHEST-precedence tier — which leaves position 2 of the documented
    /// order (built-in ▸ discovered ▸ managed ▸ `OCX_CONFIG` ▸ `--config`)
    /// unproven. Inverting the splice in `load_with_local_view` so the
    /// built-in folds ON TOP of the discovered chain passes every one of
    /// them; it fails here, which is the point: a user with an
    /// `[registries."ocx.sh"] index` in a discovered config file would
    /// otherwise be silently routed back to the public index.
    #[tokio::test]
    async fn a_discovered_tier_index_overrides_the_builtin_ocx_sh_index() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        // `$OCX_HOME/config.toml` is the one discovered tier a test can plant
        // (system and user paths are host-absolute).
        std::fs::write(
            dir.path().join("config.toml"),
            "[registries.\"ocx.sh\"]\nindex = \"https://index.corp.example\"\n",
        )
        .unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs).await.expect("load should succeed");
        assert_eq!(
            builtin_ocx_sh_index(&config),
            Some("https://index.corp.example"),
            "a discovered-tier index must beat the compiled-in one — the built-in is the LOWEST tier"
        );
    }

    /// A system-scope `[registries."<ns>"]` entry claims to be
    /// non-overridable, and the compiled-in tier's doc comment now advertises
    /// that as one of its override paths. `Config::merge` reaches every entry
    /// through `map.entry(name).or_default().merge(..)`, so the system tier is
    /// never `self` on the fold that carries it in — the lock only survives
    /// because `RegistryConfig::merge` ADOPTS it from `other`. Without that,
    /// the flag `apply_system_locks` sets is dropped on the first fold and the
    /// user tier (and the untrusted managed payload) can redirect the index.
    #[test]
    fn a_system_locked_registries_entry_survives_the_accumulator_fold() {
        let mut system: Config = toml::from_str("[registries.\"ocx.sh\"]\nindex = \"https://index.corp\"\n")
            .expect("system tier must parse");
        ConfigLoader::apply_system_locks(&mut system);

        // The real fold order: an empty accumulator (or the built-in tier)
        // takes the system tier first, then a lower tier tries to override.
        let mut accumulator = ConfigLoader::builtin_defaults();
        accumulator.merge(system);
        let user: Config = toml::from_str("[registries.\"ocx.sh\"]\nindex = \"https://attacker.example\"\n")
            .expect("user tier must parse");
        accumulator.merge(user);

        assert_eq!(
            builtin_ocx_sh_index(&accumulator),
            Some("https://index.corp"),
            "a system-locked [registries.\"ocx.sh\"] entry must not be overridable by a lower tier"
        );
    }

    /// The trust analogue of the sibling test above, and the seam the
    /// system-tier trust lock depends on. `[trust]` is the one section that
    /// array-appends, so the lock rides on each `[[trust.policy]]` entry and
    /// has to survive `Config::merge`'s `Vec::extend` across the real fold
    /// order — built-in ▸ system ▸ user ▸ managed payload ▸ `--config`
    /// overlay. Both halves are covered on their own
    /// (`lock_as_system_marks_every_declared_policy`,
    /// `system_locked_pin_refuses_a_more_specific_unlocked_entry`); the join
    /// is what this pins. Lose the flag anywhere in the fold and the narrower
    /// lower-tier scope wins by most-specific-wins — the escalation path the
    /// lock closed.
    #[test]
    fn a_system_locked_trust_policy_survives_the_accumulator_fold() {
        let mut system: Config = toml::from_str(
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\nsigners = [{ kind = \"keyless\", identity = \"system-ci\", oidc_issuer = \"iss\" }]\n",
        )
        .expect("system tier must parse");
        ConfigLoader::apply_system_locks(&mut system);

        let mut accumulator = ConfigLoader::builtin_defaults();
        accumulator.merge(system);
        for lower in [
            "[[trust.policy]]\nscope = \"ghcr.io/acme/tool\"\nsigners = [{ kind = \"keyless\", identity = \"user-Y\", oidc_issuer = \"iss\" }]\n",
            "[[trust.policy]]\nscope = \"ghcr.io/acme/tool\"\nsigners = [{ kind = \"keyless\", identity = \"managed-Z\", oidc_issuer = \"iss\" }]\n",
            "[[trust.policy]]\nscope = \"ghcr.io/acme/tool\"\nsigners = [{ kind = \"keyless\", identity = \"overlay-W\", oidc_issuer = \"iss\" }]\n",
        ] {
            accumulator.merge(toml::from_str(lower).expect("lower tier must parse"));
        }

        let resolved = ocx_trust::resolve(accumulator.trust_policies(), "ghcr.io/acme/tool");
        assert_eq!(
            resolved.len(),
            1,
            "a narrower lower-tier scope must not outbid the system pin after the fold"
        );
        assert_eq!(
            resolved[0].signers.iter().find_map(|signer| match signer {
                ocx_trust::SignerSpec::Keyless(keyless) => keyless.identity.as_deref(),
                ocx_trust::SignerSpec::Key(_) | ocx_trust::SignerSpec::Unknown => None,
            }),
            Some("system-ci")
        );
    }

    /// `local_only` is cloned from the same accumulator as `merged`, so the
    /// built-in tier must show up in both views — the managed-tier fetch
    /// builds its client from `local_only`.
    #[tokio::test]
    async fn builtin_tier_is_present_in_both_loaded_views() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.remove(&ocx_env::OCX_CONFIG);
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let loaded = ConfigLoader::load_with_local_view(inputs)
            .await
            .expect("load_with_local_view should succeed");
        assert_eq!(
            builtin_ocx_sh_index(&loaded.merged),
            Some(crate::index::DEFAULT_INDEX_BASE_URL)
        );
        assert_eq!(
            builtin_ocx_sh_index(&loaded.local_only),
            Some(crate::index::DEFAULT_INDEX_BASE_URL)
        );
    }

    // ── Path resolver tests ──────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn system_path_is_etc_ocx_config_toml() {
        // Holds the env lock so a concurrently-running test cannot have the
        // `__OCX_TESTING_SYSTEM_CONFIG` seam set while this asserts the real path.
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::__OCX_TESTING_SYSTEM_CONFIG);
        let path = ConfigLoader::system_path();
        assert_eq!(path, PathBuf::from("/etc/ocx/config.toml"));
    }

    #[test]
    fn user_path_ends_with_ocx_config_toml() {
        if dirs::config_dir().is_none() {
            // Cannot test without a config dir — skip (don't panic)
            return;
        }
        let path = ConfigLoader::user_path();
        assert!(
            path.is_some(),
            "user_path() should return Some when config_dir() is available"
        );
        let path = path.unwrap();
        assert!(
            path.ends_with("ocx/config.toml"),
            "user_path should end with ocx/config.toml, got: {}",
            path.display()
        );
    }

    #[test]
    fn home_path_uses_ocx_home_env_var() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        let path = ConfigLoader::home_path();
        assert!(path.is_some(), "home_path() should return Some when OCX_HOME is set");
        let expected = dir.path().join("config.toml");
        assert_eq!(path.unwrap(), expected);
    }

    // ── discover_paths error handling ────────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn discover_paths_skips_unreadable_candidate() {
        // A candidate whose parent directory lacks search permission causes
        // `try_exists` to return `Err(PermissionDenied)`. The new filter_map
        // branch must log a warning and drop the candidate rather than either
        // including it or failing the whole discovery pass.
        use std::os::unix::fs::PermissionsExt;

        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let locked_home = dir.path().join("locked-home");
        std::fs::create_dir(&locked_home).unwrap();
        let locked_config = locked_home.join("config.toml");
        std::fs::write(&locked_config, "").unwrap();
        // Mode 0o000 strips search permission; stat() on the child fails with
        // PermissionDenied, which is the error kind discover_paths should now
        // log+skip rather than silently collapse.
        std::fs::set_permissions(&locked_home, std::fs::Permissions::from_mode(0o000)).unwrap();

        env.set(&ocx_env::OCX_HOME, locked_home.to_str().unwrap());
        without_system_config(&env);
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);

        let paths = ConfigLoader::discover_paths()
            .await
            .expect("an unreadable candidate outside the SYSTEM tier is skipped, never fatal");

        // Restore permissions so TempDir::drop can clean up even if the
        // assertion below panics.
        std::fs::set_permissions(&locked_home, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            !paths.contains(&locked_config),
            "unreadable candidate must be skipped, got: {paths:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discover_paths_rejects_symlinked_candidate() {
        // Security: a discovered-tier `config.toml` that is a symlink is
        // rejected to prevent a writer with control over one of the
        // tier directories from aiming the link at an arbitrary readable
        // file. Explicit paths (--config, OCX_CONFIG) are out of scope
        // for this check — those are trusted caller input.
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("ocx-home");
        std::fs::create_dir(&home).unwrap();
        // Create a real target file, then symlink `config.toml` → target.
        let target = dir.path().join("target.toml");
        std::fs::write(&target, "[registry]\ndefault = \"symlinked.example\"").unwrap();
        let symlink_path = home.join("config.toml");
        std::os::unix::fs::symlink(&target, &symlink_path).expect("create symlink");

        env.set(&ocx_env::OCX_HOME, home.to_str().unwrap());
        without_system_config(&env);
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);

        let paths = ConfigLoader::discover_paths()
            .await
            .expect("a symlinked candidate outside the SYSTEM tier is skipped, never fatal");

        assert!(
            !paths.contains(&symlink_path),
            "symlinked candidate must be skipped, got: {paths:?}"
        );
    }

    // ── the SYSTEM candidate is not best-effort ──────────────────────────────
    //
    // The user and `$OCX_HOME` tiers carry a caller's own preferences, so a
    // candidate that cannot be read is skipped with a warning and discovery
    // continues. The SYSTEM tier carries operator **policy**: dropping it drops
    // every locked section with it, silently, on every invocation. An operator
    // who symlinks `/etc/ocx/config.toml` at a config-managed fleet file — an
    // ordinary move — would otherwise take the whole fleet out of a locked
    // `[records]` sink with exit 0 and a warning nobody reads.

    /// The exit code a fatal SYSTEM candidate must carry: the operator fixes it
    /// by editing a file, not by clearing an I/O condition.
    #[cfg(unix)]
    fn assert_system_config_error(error: &Error, path: &Path) {
        assert!(
            matches!(error, Error::SystemConfig { path: named, .. } if named == path),
            "expected a fatal system-config error naming {}, got: {error:?}",
            path.display()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_system_config_is_fatal() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let target = write_config(&dir, "fleet.toml", "[records]\ndir = \"/var/log/ocx/records\"\n");
        let link = dir.path().join("system-config.toml");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");
        env.set(&ocx_env::__OCX_TESTING_SYSTEM_CONFIG, link.to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);

        let error = ConfigLoader::load(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect_err("a symlinked SYSTEM candidate must not be skipped with a warning");
        assert_system_config_error(&error, &link);
    }

    /// The same refusal on the `OCX_NO_CONFIG=1` path, which runs the checks
    /// over the system candidate alone: that flag exists so a locked section
    /// survives it, which it cannot do if the candidate is dropped first.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_system_config_is_fatal_under_no_config() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let target = write_config(&dir, "fleet.toml", "[records]\ndir = \"/var/log/ocx/records\"\n");
        let link = dir.path().join("system-config.toml");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");
        env.set(&ocx_env::__OCX_TESTING_SYSTEM_CONFIG, link.to_str().unwrap());
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.remove(&ocx_env::OCX_CONFIG);

        let error = ConfigLoader::load(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect_err("the hermetic path checks the same candidate and must refuse it the same way");
        assert_system_config_error(&error, &link);
    }

    /// Anything other than `NotFound` is fatal too, not only a symlink: an
    /// unreadable `/etc/ocx/config.toml` is a policy file that exists and could
    /// not be consulted. `ENOTDIR` stands in for the class — it needs no
    /// permission games, which a container running as root would defeat.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_system_config_is_fatal() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let not_a_dir = write_config(&dir, "not-a-dir", "");
        let candidate = not_a_dir.join("config.toml");
        env.set(&ocx_env::__OCX_TESTING_SYSTEM_CONFIG, candidate.to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);

        let error = ConfigLoader::load(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect_err("a SYSTEM candidate that cannot be stat'd must not be skipped with a warning");
        assert_system_config_error(&error, &candidate);
    }

    /// The discriminator on the other side: no `/etc/ocx/config.toml` at all is
    /// the ordinary case on nearly every host, and must stay silent.
    #[tokio::test]
    async fn an_absent_system_config_is_not_fatal() {
        let env = ocx_env::overrides::lock();
        without_system_config(&env);
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);

        ConfigLoader::load(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("an absent system config is the ordinary case, not an error");
    }

    /// And the discriminator on the tier axis: the user and `$OCX_HOME` tiers
    /// keep today's best-effort discovery. Only the SYSTEM candidate is fatal,
    /// so the same symlink that refuses above is merely skipped here.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_non_system_candidate_is_still_skipped() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        without_system_config(&env);
        let target = write_config(&dir, "target.toml", "[registry]\ndefault = \"symlinked.example\"");
        let link = dir.path().join("user-config.toml");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        let kept = ConfigLoader::existing_candidates(vec![ConfigLoader::system_path(), link.clone()])
            .await
            .expect("a symlinked user-tier candidate is skipped, never fatal");
        assert!(
            !kept.contains(&link),
            "the candidate must still be dropped, just not fatally; got: {kept:?}"
        );
    }

    #[test]
    fn home_path_fallback_when_ocx_home_unset() {
        // With OCX_HOME removed, home_path() falls back to the shared home resolver.
        // The result is platform-dependent: Some(path ending in .ocx/config.toml)
        // when HOME is set, None otherwise. Both are valid outcomes.
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_HOME);
        let path = ConfigLoader::home_path();
        if let Some(path) = path {
            assert!(
                path.ends_with(".ocx/config.toml"),
                "fallback path should end with .ocx/config.toml, got: {}",
                path.display()
            );
        }
    }

    // ── project_path tests ──────────────────────────────────────────────────
    //
    // The resolver contract:
    //
    //   Precedence: --project > OCX_PROJECT > CWD walk
    //   OCX_NO_PROJECT=1 prunes CWD walk + env var, NOT the explicit flag
    //   OCX_PROJECT="" treated as unset (escape hatch)
    //   Explicit paths follow symlinks; CWD-walk paths reject symlinks
    //   Any basename accepted via flag/env; CWD walk looks for literal `ocx.toml`
    //   CWD walk stops at first ocx.toml, .git/ boundary, or OCX_CEILING_PATH
    //   CWD walk does NOT stop at filesystem root if .git/ is absent
    //   Explicit path escapes OCX_CEILING_PATH
    //   --project <missing> / OCX_PROJECT=<missing> → NotFound (79)
    //
    // Every test below names one plan bullet or one `max`-tier edge case. In
    // Phase 1 (stub), every test fails with the `unimplemented!()` panic on
    // `project_path`; Phase 4 impl flips them to pass.
    //
    // All env-touching tests acquire `ocx_env::overrides::lock()` — the same
    // process-wide mutex used by the `load()` tests above.

    /// Helper: write a file at `path` with the given content.
    fn write_file(path: &std::path::Path, content: &str) {
        std::fs::write(path, content).expect("write test fixture");
    }

    /// Plan bullet: `--project <valid>` → loads the file.
    #[tokio::test]
    async fn project_path_explicit_flag_loads_valid_file() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ocx.toml");
        write_file(&path, "");
        let resolved = ConfigLoader::project_path(None, Some(&path))
            .await
            .expect("valid explicit path should resolve");
        assert_eq!(resolved, Some(path));
    }

    /// Plan bullet: `--project <missing>` → `NotFound` (79).
    #[tokio::test]
    async fn project_path_explicit_flag_missing_returns_not_found() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let missing = PathBuf::from("/tmp/ocx-project-path-test-missing-explicit.toml");
        let err = ConfigLoader::project_path(None, Some(&missing))
            .await
            .expect_err("missing explicit path should be FileNotFound");
        assert!(
            matches!(
                err,
                crate::error::Error::FileNotFound {
                    ref path,
                    tier: crate::error::ConfigSource::Project,
                } if path == &missing,
            ),
            "expected FileNotFound(Project) for missing --project path, got: {err:?}"
        );
    }

    /// Non-`NotFound` I/O error on an explicit path surfaces as `Error::Io`
    /// (exit 74) rather than silently succeeding. `/dev/null/...` returns
    /// `ENOTDIR` on Unix because `/dev/null` is a character device, not a
    /// directory — a stable way to provoke a non-`NotFound` kind without
    /// platform-specific permission gymnastics.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_explicit_io_error_surfaces_as_io() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let bad = PathBuf::from("/dev/null/not-a-real-file.toml");
        let err = ConfigLoader::project_path(None, Some(&bad))
            .await
            .expect_err("non-NotFound I/O error on explicit path must surface");
        assert!(
            matches!(
                err,
                crate::error::Error::Io {
                    ref path,
                    tier: crate::error::ConfigSource::Project,
                    ..
                } if path == &bad,
            ),
            "expected Error::Io(Project) for ENOTDIR on explicit --project path, got: {err:?}"
        );
    }

    /// Plan bullet: `OCX_PROJECT=<valid>` → loads the file.
    #[tokio::test]
    async fn project_path_env_var_loads_valid_file() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("custom-name.toml");
        write_file(&path, "");
        env.set(&ocx_env::OCX_PROJECT, path.to_str().unwrap());
        let resolved = ConfigLoader::project_path(None, None)
            .await
            .expect("env-var path should resolve");
        assert_eq!(resolved, Some(path));
    }

    /// Plan bullet: `OCX_PROJECT=""` → treated as unset.
    ///
    /// With env var treated as unset, and no explicit flag, and a cwd that
    /// has no `ocx.toml` above it up to the ceiling → returns `None`.
    #[tokio::test]
    async fn project_path_empty_env_var_treated_as_unset() {
        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.set(&ocx_env::OCX_PROJECT, "");
        env.remove(&ocx_env::OCX_NO_PROJECT);
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_CEILING_PATH, dir.path().to_str().unwrap());
        let resolved = ConfigLoader::project_path(Some(dir.path()), None)
            .await
            .expect("empty env var should be treated as unset, not an error");
        assert_eq!(
            resolved, None,
            "empty OCX_PROJECT must fall through; with no cwd hit, result should be None"
        );
    }

    /// Plan bullet: `OCX_NO_PROJECT=1` → skips CWD walk + env-var path; returns `None`.
    #[tokio::test]
    async fn project_path_no_project_returns_none() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_PROJECT, "1");
        let dir = TempDir::new().unwrap();
        // Even placing a valid ocx.toml at cwd must not be discovered.
        let cwd_project = dir.path().join("ocx.toml");
        write_file(&cwd_project, "");
        // And a valid env-var path must also be ignored.
        let env_path = dir.path().join("env.toml");
        write_file(&env_path, "");
        env.set(&ocx_env::OCX_PROJECT, env_path.to_str().unwrap());
        let resolved = ConfigLoader::project_path(Some(dir.path()), None)
            .await
            .expect("OCX_NO_PROJECT=1 with no explicit flag must return Ok(None)");
        assert_eq!(
            resolved, None,
            "OCX_NO_PROJECT=1 must prune both CWD walk and env-var path"
        );
    }

    /// Plan bullet: `OCX_NO_PROJECT=1` + `--project <valid>` → still loads.
    #[tokio::test]
    async fn project_path_no_project_does_not_block_explicit_flag() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_PROJECT, "1");
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("flag.toml");
        write_file(&path, "");
        let resolved = ConfigLoader::project_path(None, Some(&path))
            .await
            .expect("OCX_NO_PROJECT=1 must not block --project");
        assert_eq!(resolved, Some(path));
    }

    /// ADR G3: `OCX_NO_PROJECT=1` prunes `OCX_PROJECT` (stricter than
    /// `OCX_NO_CONFIG`, which leaves `OCX_CONFIG` intact). Only `--project`
    /// escapes the kill switch — the env var does not.
    #[tokio::test]
    async fn project_path_no_project_prunes_env_var() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_PROJECT, "1");
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("env-hermetic.toml");
        write_file(&path, "");
        env.set(&ocx_env::OCX_PROJECT, path.to_str().unwrap());
        let resolved = ConfigLoader::project_path(None, None)
            .await
            .expect("OCX_NO_PROJECT=1 must prune the env-var path per ADR G3");
        assert_eq!(resolved, None, "OCX_NO_PROJECT=1 must prune OCX_PROJECT (ADR G3)");
    }

    /// Plan bullet: Precedence `--project` > `OCX_PROJECT` > CWD walk.
    ///
    /// Three files are materialized, one per tier. A single resolver call
    /// that sees all three must return the highest-precedence one.
    #[tokio::test]
    async fn project_path_flag_beats_env_beats_walk_precedence() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();

        // CWD-walk candidate at the root of a throw-away workspace.
        let walk_dir = dir.path().join("workspace");
        std::fs::create_dir(&walk_dir).unwrap();
        let walk_path = walk_dir.join("ocx.toml");
        write_file(&walk_path, "");

        // Env-var candidate.
        let env_path = dir.path().join("env.toml");
        write_file(&env_path, "");
        env.set(&ocx_env::OCX_PROJECT, env_path.to_str().unwrap());

        // Explicit-flag candidate (highest).
        let flag_path = dir.path().join("flag.toml");
        write_file(&flag_path, "");

        let resolved = ConfigLoader::project_path(Some(&walk_dir), Some(&flag_path))
            .await
            .expect("all three tiers present should resolve");
        assert_eq!(
            resolved,
            Some(flag_path),
            "--project must beat OCX_PROJECT and CWD walk"
        );
    }

    /// Max-tier edge case: `--project` takes precedence over `OCX_PROJECT`
    /// when both are set (focused assertion separate from the three-tier test).
    #[tokio::test]
    async fn project_path_explicit_takes_precedence_over_env_when_both_set() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();
        let file_a = dir.path().join("a.toml");
        let file_b = dir.path().join("b.toml");
        write_file(&file_a, "");
        write_file(&file_b, "");
        env.set(&ocx_env::OCX_PROJECT, file_b.to_str().unwrap());
        let resolved = ConfigLoader::project_path(None, Some(&file_a))
            .await
            .expect("both explicit sources should resolve");
        assert_eq!(resolved, Some(file_a), "--project must beat OCX_PROJECT");
    }

    /// Plan bullet: Explicit path escapes `OCX_CEILING_PATH`.
    ///
    /// Ceiling is set above the target file; the explicit path must still
    /// resolve because the ceiling only bounds the CWD walk.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_explicit_escapes_ceiling() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        let ceiling = dir.path().join("ceiling");
        std::fs::create_dir(&ceiling).unwrap();
        env.set(&ocx_env::OCX_CEILING_PATH, ceiling.to_str().unwrap());
        // File lives OUTSIDE the ceiling — must still resolve via --project.
        let outside = dir.path().join("outside.toml");
        write_file(&outside, "");
        let resolved = ConfigLoader::project_path(None, Some(&outside))
            .await
            .expect("explicit path must escape ceiling");
        assert_eq!(resolved, Some(outside));
    }

    /// Plan bullet: Symlink via `--project` → accepted.
    ///
    /// Explicit paths follow symlinks — trusted caller intent.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_explicit_follows_symlink() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.toml");
        write_file(&target, "");
        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");
        let resolved = ConfigLoader::project_path(None, Some(&link))
            .await
            .expect("symlink via --project must be accepted");
        // Either the link path itself or the resolved target is acceptable —
        // the spec says "accepted" without pinning which is returned. Assert
        // that we got a Some and it points at a real file.
        let returned = resolved.expect("should be Some");
        assert!(
            returned == link || returned == target,
            "returned path should be the link or its target, got: {}",
            returned.display()
        );
    }

    /// Plan bullet: Symlink discovered via CWD walk → rejected.
    ///
    /// CWD walk rejects symlinks (matches tier-config discovery symmetry).
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_rejects_symlink() {
        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        // .git/ absent, ceiling bounds the walk to the temp dir.
        env.set(&ocx_env::OCX_CEILING_PATH, dir.path().to_str().unwrap());

        // Put the real ocx.toml outside the walk, then symlink into workspace.
        let target = dir.path().join("real.toml");
        write_file(&target, "");
        let link = workspace.join("ocx.toml");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        let resolved = ConfigLoader::project_path(Some(&workspace), None)
            .await
            .expect("symlinked walk hit should be skipped, not error");
        assert_eq!(
            resolved, None,
            "CWD walk must reject symlinks and return None when only the symlink is found"
        );
    }

    /// Plan bullet: Non-`ocx.toml` basename via `--project` → accepted.
    ///
    /// Matches Cargo `--manifest-path` semantics.
    #[tokio::test]
    async fn project_path_explicit_accepts_any_basename() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("fixture-manifest.project");
        write_file(&path, "");
        let resolved = ConfigLoader::project_path(None, Some(&path))
            .await
            .expect("any basename should be accepted via --project");
        assert_eq!(resolved, Some(path));
    }

    /// Plan bullet: CWD walk with `ocx.toml` at repo root + nested cwd → finds root file.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_finds_root_ocx_toml_from_nested_cwd() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let project = root.join("ocx.toml");
        write_file(&project, "");
        env.set(&ocx_env::OCX_CEILING_PATH, dir.path().to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("walk from nested cwd should succeed");
        assert_eq!(resolved, Some(project), "walk should locate root ocx.toml");
    }

    /// Plan bullet: CWD walk stops at `.git/` boundary.
    ///
    /// A parent `ocx.toml` exists above a `.git/` directory; the walk must
    /// stop at the `.git/` boundary and NOT cross into the parent.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_stops_at_git_boundary() {
        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        // Parent workspace holds an ocx.toml — this must NOT be returned.
        let parent_project = dir.path().join("ocx.toml");
        write_file(&parent_project, "");
        // Inner workspace with a .git/ boundary and a nested cwd.
        let inner = dir.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        std::fs::create_dir(inner.join(".git")).unwrap();
        let nested = inner.join("src");
        std::fs::create_dir(&nested).unwrap();
        env.set(&ocx_env::OCX_CEILING_PATH, dir.path().to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("walk must stop at .git/ boundary");
        assert_eq!(
            resolved, None,
            ".git/ boundary must prevent discovery of parent ocx.toml"
        );
    }

    /// Git worktree `.git` is a *file* (linkfile) pointing at the real git
    /// directory under `worktrees/<name>/`, not a directory. The walk must
    /// still treat the worktree root as a repository boundary — matching
    /// git's own `git-check-ref-format` rule of "any `.git` entry counts".
    ///
    /// EC-FS-010, half one of two: the linkfile. The symlink half is
    /// [`project_path_walk_stops_at_a_symlinked_git_entry`].
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_stops_at_git_worktree_linkfile() {
        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        // Parent workspace holds an ocx.toml — this must NOT be returned.
        let parent_project = dir.path().join("ocx.toml");
        write_file(&parent_project, "");
        // Worktree layout: `.git` is a regular file, not a directory.
        let worktree = dir.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: /some/path/.git/worktrees/wt\n").unwrap();
        let nested = worktree.join("src");
        std::fs::create_dir(&nested).unwrap();
        env.set(&ocx_env::OCX_CEILING_PATH, dir.path().to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("walk must stop at .git linkfile boundary");
        assert_eq!(
            resolved, None,
            ".git file (worktree linkfile) must also act as a repo boundary"
        );
    }

    /// EC-FS-010, half two of two: a **symlinked** `.git` counts exactly like a
    /// real directory and a worktree linkfile (D3:166 — "any `.git` entry
    /// counts").
    ///
    /// Both spellings are asserted in one walk each, and both are
    /// discriminating under a different mutation of [`ConfigLoader::has_git_dir`]:
    ///
    /// - the link **to a real `.git`** reds when the probe narrows to
    ///   directories (`Ok(meta) if meta.is_dir()`), because a symlink's own
    ///   `symlink_metadata` is never `is_dir()`;
    /// - the **dangling** link reds when the probe stops being
    ///   `symlink_metadata` and starts following (`tokio::fs::metadata`),
    ///   because a link to a removed target then reports `NotFound` and the
    ///   walk climbs past the repository root it should have stopped at.
    ///
    /// A dangling `.git` is not a contrivance: `git worktree`'s administrative
    /// directory is pruned out from under a checkout routinely, and the outcome
    /// of guessing "no repository here" is that a *parent* repository's
    /// `ocx.toml` silently becomes this checkout's project.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_stops_at_a_symlinked_git_entry() {
        use std::os::unix::fs::symlink;

        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        // The decoy the walk must never reach: without a boundary at the
        // checkout root, the ascent finds this and adopts it.
        write_file(&dir.path().join("ocx.toml"), "");
        env.set(&ocx_env::OCX_CEILING_PATH, dir.path().to_str().unwrap());

        // (a) `.git` is a symlink to a real git directory living elsewhere.
        let real_git = dir.path().join("elsewhere").join(".git");
        std::fs::create_dir_all(&real_git).unwrap();
        let linked = dir.path().join("linked");
        std::fs::create_dir(&linked).unwrap();
        symlink(&real_git, linked.join(".git")).unwrap();
        let nested = linked.join("src");
        std::fs::create_dir(&nested).unwrap();
        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("a symlinked .git boundary must resolve, not error");
        assert_eq!(
            resolved, None,
            "a `.git` symlink pointing at a real git directory must bound the walk"
        );

        // (b) the same link, dangling — the target was pruned after checkout.
        let dangling = dir.path().join("dangling");
        std::fs::create_dir(&dangling).unwrap();
        symlink(dir.path().join("no-such-git-dir"), dangling.join(".git")).unwrap();
        let nested = dangling.join("src");
        std::fs::create_dir(&nested).unwrap();
        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("a dangling .git symlink must resolve, not error");
        assert_eq!(
            resolved, None,
            "a dangling `.git` symlink is still a `.git` entry and must bound the walk"
        );
    }

    /// Amendment F precedence: `ocx.toml` at the repo root (alongside
    /// `.git/`) must be returned — the `.git/` boundary only prevents
    /// walking UP past the repo root, it does not disqualify a project
    /// file AT that level. Regression guard for the common case.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_finds_ocx_toml_at_git_root_level() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        let project = repo.join("ocx.toml");
        write_file(&project, "");
        let nested = repo.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        env.set(&ocx_env::OCX_CEILING_PATH, dir.path().to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("walk from nested cwd must find ocx.toml at repo root");
        assert_eq!(
            resolved,
            Some(project),
            "a repo root with both .git/ and ocx.toml must resolve to the ocx.toml"
        );
    }

    /// Explicit `--project <dir>` resolves to the `ocx.toml` inside it, and a
    /// directory that holds none is **no project** rather than an error.
    ///
    /// This reverses `project_path_explicit_directory_rejected_as_io`, which
    /// asserted `Error::Io` (exit 74) for the same input. That contract made
    /// `--project .` — and every rendered toolchain trampoline, which re-enters
    /// as `ocx --project '<project root>' exec` — fail before anything ran, and
    /// put the caller's `NoProjectIn` → exit 64 out of reach: only the directory branch
    /// can tell *this directory governs no project* from *this file is missing*.
    #[tokio::test]
    async fn project_path_explicit_directory_resolves_to_its_project_file() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("ocx.toml");
        write_file(&project, "");

        let resolved = ConfigLoader::project_path(None, Some(dir.path()))
            .await
            .expect("a directory holding an ocx.toml must resolve");
        assert_eq!(
            resolved,
            Some(project),
            "explicit --project <dir> must resolve to <dir>/ocx.toml"
        );
    }

    /// The other half of that contract: a named directory with no `ocx.toml` is `None`
    /// — "no project" (the caller's exit 64), never `FileNotFound` (79) or
    /// `Error::Io` (74).
    #[tokio::test]
    async fn project_path_explicit_directory_without_a_project_file_is_none() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();

        let resolved = ConfigLoader::project_path(None, Some(dir.path()))
            .await
            .expect("a directory with no ocx.toml is not an error");
        assert_eq!(
            resolved, None,
            "a directory holding no ocx.toml governs no project, so the answer is None"
        );
    }

    /// A path that is neither a regular file nor a directory still surfaces as
    /// `Error::Io` (exit 74, ADR G9).
    ///
    /// Kept as its own case because a named directory no longer reaches this arm,
    /// and a character device is the remaining reachable input for it — without
    /// this, the arm has no coverage at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_explicit_device_rejected_as_io() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let target = PathBuf::from("/dev/null");
        let err = ConfigLoader::project_path(None, Some(&target))
            .await
            .expect_err("explicit --project pointing at a character device must error");
        assert!(
            matches!(
                err,
                crate::error::Error::Io {
                    ref path,
                    tier: crate::error::ConfigSource::Project,
                    ..
                } if path == &target,
            ),
            "expected Error::Io(Project) for a device on explicit --project path, got: {err:?}"
        );
    }

    /// Plan bullet: No `ocx.toml`, no explicit path → returns `None`.
    #[tokio::test]
    async fn project_path_returns_none_when_no_source() {
        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_CEILING_PATH, dir.path().to_str().unwrap());
        let resolved = ConfigLoader::project_path(Some(dir.path()), None)
            .await
            .expect("no sources should resolve to None, not error");
        assert_eq!(resolved, None);
    }

    /// Max-tier edge case: `OCX_PROJECT=<missing>` → `NotFound`.
    ///
    /// Plan line 69 + Amendment G7 — symmetry with `--project <missing>`.
    /// Plan bullets only test the valid env-var path explicitly.
    #[tokio::test]
    async fn project_path_env_var_missing_file_returns_not_found() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let missing = PathBuf::from("/tmp/ocx-project-path-test-missing-env.toml");
        env.set(&ocx_env::OCX_PROJECT, missing.to_str().unwrap());
        let err = ConfigLoader::project_path(None, None)
            .await
            .expect_err("missing env-var path should be FileNotFound");
        assert!(
            matches!(
                err,
                crate::error::Error::FileNotFound {
                    ref path,
                    tier: crate::error::ConfigSource::Project,
                } if path == &missing,
            ),
            "expected FileNotFound(Project) for missing OCX_PROJECT path, got: {err:?}"
        );
    }

    /// Max-tier edge case: CWD walk with no `.git/`, no `OCX_CEILING_PATH`, no
    /// `ocx.toml` → returns `None` without hanging or erroring.
    ///
    /// Plan line 40 ("Does NOT stop at filesystem root if .git/ is absent")
    /// requires the walk to continue past the common stopping conditions;
    /// this test guards against an infinite loop.
    ///
    /// EC-FS-013 — this **is** the filesystem-root termination case, closed by
    /// asserting it rather than adding code: with no boundary
    /// and no ceiling the ascent runs out of ancestors, and
    /// [`ConfigLoader::walk_for_project_file`]'s `current.parent()` arm returns
    /// `None` on its own. The bounded timeout is the assertion that matters —
    /// an off-by-one on that arm (re-visiting the root, or ascending into a
    /// path that never shortens) hangs rather than failing, and a hang inside a
    /// per-prompt hook is the shipped bug this guards.
    #[tokio::test]
    async fn project_path_walk_without_git_or_ceiling_returns_none() {
        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        // Use a temp dir far inside /tmp; no ocx.toml anywhere on the path
        // to `/`. The resolver must terminate at the filesystem root and
        // return None — never hang, never error.
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let resolved = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ConfigLoader::project_path(Some(&nested), None),
        )
        .await
        .expect("walk must terminate within 5s when no .git/ and no ceiling")
        .expect("walk should resolve to None when nothing is found, not error");
        assert_eq!(resolved, None);
    }

    /// EC-FS-013 — starting the walk **at** the filesystem root exercises the
    /// `current.parent() == None` arm on the very first iteration, with no
    /// dependence on what happens to sit between a temp directory and `/`.
    ///
    /// The sibling test above reaches the same arm by ascending, but only on a
    /// host where no ancestor of `TMPDIR` carries a `.git` or an `ocx.toml`;
    /// this one cannot be short-circuited by either. Together they pin the
    /// ruling "the walk's termination at the filesystem root needs no special case —
    /// assert it, do not add code".
    ///
    /// The ceiling is passed as `None` deliberately: a ceiling would end the
    /// walk one branch earlier and the root arm would never run.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_terminates_at_the_filesystem_root() {
        let root = Path::new("/");
        assert_eq!(root.parent(), None, "the fixture must start where there is no ancestor");
        let resolved = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ConfigLoader::walk_for_project_file(root, None),
        )
        .await
        .expect("a walk starting at the filesystem root must terminate, not loop");
        assert_eq!(
            resolved, None,
            "no ocx.toml at `/` and no ancestor to ascend to must yield None"
        );
    }

    /// Max-tier edge case: `OCX_CEILING_PATH` set above the `ocx.toml` → returns `None`.
    ///
    /// Amendment F — ceiling is the paired bound for the walk. When the
    /// ceiling sits between cwd and the `ocx.toml`, discovery must stop.
    ///
    /// EC-FS-012 half one of two: the ceiling bounds the ascent the same way a
    /// `.git` entry does. The other half — an `ocx.toml` sitting **at** the
    /// ceiling still resolves, because the candidate probe runs before the
    /// ceiling gate — is
    /// [`project_path_walk_finds_ocx_toml_at_the_ceiling_itself`].
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_stops_at_ceiling() {
        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        // ocx.toml at the OUTERMOST level (above the ceiling).
        let outer_project = dir.path().join("ocx.toml");
        write_file(&outer_project, "");
        // Ceiling sits inside the tempdir; walk starts below the ceiling.
        let ceiling = dir.path().join("ceiling");
        std::fs::create_dir(&ceiling).unwrap();
        env.set(&ocx_env::OCX_CEILING_PATH, ceiling.to_str().unwrap());
        let cwd = ceiling.join("project");
        std::fs::create_dir(&cwd).unwrap();

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("ceiling-bounded walk should resolve, not error");
        assert_eq!(
            resolved, None,
            "OCX_CEILING_PATH must bound the walk before reaching outer ocx.toml"
        );
    }

    /// A **relative** `OCX_CEILING_PATH` bounds the walk
    /// exactly as the absolute one in
    /// [`project_path_walk_stops_at_ceiling`] does.
    ///
    /// `current` is absolute throughout the walk, and `Path` equality
    /// distinguishes an absolute path from a relative one by its root
    /// component, so before the join the comparison could never fire and the
    /// walk ran unbounded to the outer `ocx.toml`. The two tests are the same
    /// tree with the same ceiling written two ways, and must answer the same.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_stops_at_a_relative_ceiling() {
        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        // ocx.toml above the ceiling — the file an unbounded walk would find.
        write_file(&dir.path().join("ocx.toml"), "");
        let ceiling = dir.path().join("ceiling");
        std::fs::create_dir(&ceiling).unwrap();
        let cwd = ceiling.join("project");
        std::fs::create_dir(&cwd).unwrap();

        // Written relative to `cwd`, the directory the walk starts from.
        env.set(&ocx_env::OCX_CEILING_PATH, "..");

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("ceiling-bounded walk should resolve, not error");
        assert_eq!(
            resolved, None,
            "a relative OCX_CEILING_PATH must bound the walk, not be silently ignored"
        );
    }

    /// The empty value stays the ignored one it already was.
    ///
    /// Joining it would make the ceiling equal `start` and stop the walk at
    /// cwd — turning the escape hatch every other path-valued `OCX_*` variable
    /// spells the same way into a bound nobody asked for.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_empty_ceiling_does_not_bound_the_walk() {
        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        let outer_project = dir.path().join("ocx.toml");
        write_file(&outer_project, "");
        let cwd = dir.path().join("nested");
        std::fs::create_dir(&cwd).unwrap();

        env.set(&ocx_env::OCX_CEILING_PATH, "");

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("an empty ceiling should resolve, not error");
        assert_eq!(
            resolved,
            Some(outer_project),
            "an empty OCX_CEILING_PATH must stay ignored, not bound the walk at cwd"
        );
    }

    /// EC-FS-012 half two of two: an `ocx.toml` **at** the ceiling still
    /// resolves.
    ///
    /// D3:166 pins the order — the candidate probe runs first and the ceiling
    /// gate only prevents ascending *above* the ceiling, so pointing
    /// `OCX_CEILING_PATH` exactly at a workspace root is a supported way to
    /// pin discovery to it rather than a way to disable it. Move the gate above
    /// the probe and this reds while the sibling
    /// [`project_path_walk_stops_at_ceiling`] stays green — which is why the
    /// pair is needed to describe the contract at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_finds_ocx_toml_at_the_ceiling_itself() {
        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        let ceiling = dir.path().join("workspace");
        std::fs::create_dir(&ceiling).unwrap();
        let project = ceiling.join("ocx.toml");
        write_file(&project, "");
        env.set(&ocx_env::OCX_CEILING_PATH, ceiling.to_str().unwrap());
        let cwd = ceiling.join("crates").join("inner");
        std::fs::create_dir_all(&cwd).unwrap();

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("a walk bounded at the workspace root should resolve, not error");
        assert_eq!(
            resolved,
            Some(project),
            "the candidate probe runs before the ceiling gate, so an ocx.toml AT the ceiling resolves"
        );
    }

    /// Half one of two: a working directory under `$OCX_HOME`
    /// does not adopt the global toolchain manifest as its project.
    ///
    /// `$OCX_HOME/ocx.toml` is the `--global` tier
    /// (item 1, `adr_global_toolchain_tier.md` § Decisions (binding)) and the CWD walk used to
    /// hand it back as an ordinary discovery hit, so every command run from
    /// inside the store — `$OCX_HOME/packages/<x>` — silently acquired the
    /// global toolchain as a project.
    ///
    /// The paired half is
    /// [`project_path_explicit_flag_still_reaches_the_global_manifest`]: a
    /// guard that suppressed the manifest outright, rather than only during
    /// the walk, would pass this assertion alone.
    #[tokio::test]
    async fn walk_does_not_adopt_the_global_toolchain_manifest() {
        let env = ocx_env::overrides::lock();
        let home = env.isolate_project_home();
        write_file(&home.path().join(PROJECT_FILE_NAME), "");
        let cwd = home.path().join("sub");
        std::fs::create_dir(&cwd).unwrap();

        let resolved = ConfigLoader::walk_for_project_file(&cwd, None).await;

        assert_eq!(
            resolved, None,
            "the walk must skip $OCX_HOME's own ocx.toml and keep ascending, not return it"
        );
    }

    /// Half two of two: the skip is scoped to the walk, so the
    /// global manifest stays reachable through an explicit selection.
    ///
    /// The `--global` selector itself is `ProjectConfig::resolve`'s own branch
    /// in `ocx_project`, which joins `<ocx_home>/ocx.toml` without consulting
    /// this loader at all and therefore cannot be exercised from here. The
    /// explicit-path branch is this crate's equivalent evidence: it proves the
    /// guard refuses the *discovery* of that file, not the file.
    #[tokio::test]
    async fn project_path_explicit_flag_still_reaches_the_global_manifest() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let home = env.isolate_project_home();
        let manifest = home.path().join(PROJECT_FILE_NAME);
        write_file(&manifest, "");
        let cwd = home.path().join("sub");
        std::fs::create_dir(&cwd).unwrap();

        let resolved = ConfigLoader::project_path(Some(&cwd), Some(&manifest))
            .await
            .expect("an explicit path naming the global manifest must resolve, not error");

        assert_eq!(
            resolved,
            Some(manifest),
            "the $OCX_HOME guard bounds discovery only — an explicit selection still selects"
        );
    }

    /// The guard is a skip, not a boundary — a real project
    /// **above** `$OCX_HOME` is still found from a working directory below it.
    ///
    /// The mutation this catches is "skip and stop": returning `None` at
    /// `$OCX_HOME` instead of continuing the ascent. That reads as the same
    /// fix and passes [`walk_does_not_adopt_the_global_toolchain_manifest`],
    /// but it makes a project undiscoverable whenever someone points
    /// `OCX_HOME` at a directory inside their checkout.
    #[tokio::test]
    async fn walk_finds_a_project_above_a_nested_ocx_home() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();
        let project_root = dir.path().join("proj");
        let home = project_root.join("home");
        let cwd = home.join("x");
        std::fs::create_dir_all(&cwd).unwrap();
        let project = project_root.join(PROJECT_FILE_NAME);
        write_file(&project, "");
        write_file(&home.join(PROJECT_FILE_NAME), "");
        env.set(&ocx_env::OCX_HOME, home.to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("a walk past a nested $OCX_HOME should resolve, not error");

        assert_eq!(
            resolved,
            Some(project),
            "$OCX_HOME skips its own candidate and keeps walking, so the project above it wins"
        );
    }

    /// `OCX_NO_PROJECT=1` stays a hard `None` — the guard adds a
    /// skip to the walk, it does not give the walk a new way to run.
    ///
    /// Asserted over the tree of
    /// [`walk_finds_a_project_above_a_nested_ocx_home`], which resolves to a
    /// real project without the flag, so the `None` here is the flag's doing
    /// and not an empty fixture.
    #[tokio::test]
    async fn no_project_stays_a_hard_none_above_a_nested_ocx_home() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_PROJECT, "1");
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();
        let project_root = dir.path().join("proj");
        let home = project_root.join("home");
        let cwd = home.join("x");
        std::fs::create_dir_all(&cwd).unwrap();
        write_file(&project_root.join(PROJECT_FILE_NAME), "");
        write_file(&home.join(PROJECT_FILE_NAME), "");
        env.set(&ocx_env::OCX_HOME, home.to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("OCX_NO_PROJECT=1 with no explicit flag must return Ok(None)");

        assert_eq!(resolved, None, "OCX_NO_PROJECT=1 prunes the walk entirely");
    }

    /// A **relative** `$OCX_HOME` still fires the guard.
    ///
    /// `home::default_ocx_root` returns the environment value verbatim, and
    /// `current` is absolute at every level of the walk, so an unjoined
    /// relative root can never equal one — the guard would report as present
    /// and do nothing. Resolved against `start`, which is the same treatment
    /// `OCX_CEILING_PATH` gets and for the same reason.
    ///
    /// Mutation that reds it: drop the `start.join(..)` and normalize the raw
    /// root. Every other `$OCX_HOME` test stays green, because they all spell
    /// the root absolutely.
    #[tokio::test]
    async fn walk_skips_a_relative_ocx_home() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        env.remove(&ocx_env::OCX_CEILING_PATH);
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        let cwd = home.join("sub");
        std::fs::create_dir_all(&cwd).unwrap();
        write_file(&home.join(PROJECT_FILE_NAME), "");
        // `..` from the walk's start is this tree's relative spelling of $OCX_HOME.
        env.set(&ocx_env::OCX_HOME, "..");

        let resolved = ConfigLoader::walk_for_project_file(&cwd, None).await;

        assert_eq!(
            resolved, None,
            "a relative $OCX_HOME resolves against the walk start, so the guard still skips its manifest"
        );
    }

    /// EC-FS-014 — a directory chain at the OS path limit degrades to
    /// "boundary reached", never a raw `ENAMETOOLONG` on the per-prompt path.
    ///
    /// The register's framing is overruled here: this is a test-and-document
    /// gap, not an implementation gap. [`ConfigLoader::has_git_dir`] already
    /// fails closed on any non-`NotFound` I/O error, and an over-limit
    /// `<dir>/.git` probe is exactly that — so the ascent stops at the level
    /// where paths stopped being expressible and the caller gets `Ok(None)`.
    /// The decoy `ocx.toml` above the chain is what makes the assertion
    /// discriminating: flip `has_git_dir`'s non-`NotFound` arm to `false` and
    /// the walk climbs out and adopts it.
    ///
    /// The limit is **discovered, not assumed** — `PATH_MAX` is 4096 on Linux
    /// and 1024 on macOS, and a filesystem may impose its own. The chain grows
    /// until the OS refuses one more single-character level, so a probe for a
    /// five-byte `/.git` child of the deepest directory necessarily exceeds
    /// whatever the real limit turned out to be. The precondition is then
    /// asserted rather than assumed, because a fixture that quietly stopped
    /// producing the error would leave this test green for the wrong reason.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_over_the_os_path_limit_stops_without_erroring() {
        let env = ocx_env::overrides::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("r");
        std::fs::create_dir(&root).unwrap();
        // The decoy: reachable only if the over-limit level fails to stop the
        // ascent.
        let decoy = root.join("ocx.toml");
        write_file(&decoy, "");
        env.set(&ocx_env::OCX_CEILING_PATH, dir.path().to_str().unwrap());

        // Grow in 200-byte strides while they fit, then in single bytes, so the
        // deepest directory sits within one byte of the real limit.
        let mut deep = root.clone();
        for stride in [200_usize, 1] {
            loop {
                let next = deep.join("d".repeat(stride));
                match std::fs::create_dir(&next) {
                    Ok(()) => deep = next,
                    Err(_) => break,
                }
            }
        }
        let probe = std::fs::symlink_metadata(deep.join(".git")).expect_err(
            "the fixture must actually exceed the OS path limit; a `.git` probe under the deepest \
             creatable directory has to fail, or this test proves nothing",
        );
        assert_ne!(
            probe.kind(),
            std::io::ErrorKind::NotFound,
            "the over-limit probe must be an I/O error, not a plain miss: {probe:?}"
        );

        let resolved = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ConfigLoader::project_path(Some(&deep), None),
        )
        .await
        .expect("an over-limit walk must terminate")
        .expect("an over-limit probe must degrade to a boundary, never surface as Err");
        assert_eq!(
            resolved, None,
            "a path at the OS limit fails closed to `boundary reached`, so the ancestor ocx.toml \
             is not adopted"
        );
    }

    /// Max-tier edge case: no ceiling, no `.git/`, deeply nested cwd, `ocx.toml`
    /// many levels up → found.
    ///
    /// Stresses the loop-termination logic: we expect the walk to actually
    /// traverse several levels rather than short-circuiting at one or two.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_no_ceiling_no_git_finds_ocx_toml_many_levels_up() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_PROJECT);
        env.remove(&ocx_env::OCX_PROJECT);
        let dir = TempDir::new().unwrap();
        // Set the ceiling at the tempdir so the test cannot accidentally walk
        // past it into the real filesystem; the `ocx.toml` is placed just
        // below the ceiling so the walk has work to do without leaving the
        // sandbox. This still stresses five levels of parent traversal, which
        // is the point of the test.
        env.set(&ocx_env::OCX_CEILING_PATH, dir.path().to_str().unwrap());
        let root = dir.path().join("r");
        std::fs::create_dir(&root).unwrap();
        let project = root.join("ocx.toml");
        write_file(&project, "");
        let deep = root.join("a").join("b").join("c").join("d").join("e");
        std::fs::create_dir_all(&deep).unwrap();

        let resolved = ConfigLoader::project_path(Some(&deep), None)
            .await
            .expect("deep walk should resolve");
        assert_eq!(
            resolved,
            Some(project),
            "walk must traverse multiple parent levels to find ocx.toml"
        );
    }

    // ── managed-config tier: identity-gated fold (ADR Decision A) ────────────
    //
    // `fold_managed_tier` folds a matching snapshot's payload above the home
    // tier and below `--config`/`OCX_CONFIG`, stripping any embedded
    // `[managed]` section first (see its doc comment for the full contract).
    // Tests below cover both the merge path and the ignore paths (mismatch /
    // hermetic / malformed snapshot).

    /// Writes a managed-config snapshot at the well-known paths under `ocx_home`,
    /// mirroring `persist_managed_config`'s two-file layout: `snapshot.json`
    /// metadata plus the sibling `config.toml` payload.
    fn write_managed_snapshot(ocx_home: &Path, source: &str, config_toml: &str) {
        let path = crate::managed_config::ManagedConfigPaths::for_ocx_home(ocx_home).snapshot_file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let snapshot = serde_json::json!({
            "source": source,
            "digest": format!("sha256:{}", "a".repeat(64)),
            "fetched_at": "2026-07-04T00:00:00Z",
        });
        std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        std::fs::write(
            crate::managed_config::ManagedConfigPaths::toml_beside_snapshot(&path),
            config_toml,
        )
        .unwrap();
    }

    /// Precedence + one-hop-strip (criteria 11): the managed snapshot folds
    /// ABOVE the home tier but BELOW `--config`/`OCX_CONFIG`; its embedded
    /// `[managed]` section (a redirect attempt) is stripped and never
    /// overrides the seed's own `[managed]` values.
    #[tokio::test]
    async fn managed_snapshot_merges_above_home_below_config_and_strips_managed_section() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        std::fs::write(
            dir.path().join("config.toml"),
            "[managed]\nsource = \"registry.test/managed-config:v1\"\nrequired = false\n",
        )
        .unwrap();

        // The payload embeds a hostile [managed] section attempting a redirect
        // (ADR Decision I, one-hop) — it must never override the seed's source.
        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[registry]\ndefault = \"managed-registry\"\n[managed]\nsource = \"hostile.test/other:v1\"\n",
        );

        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let loaded = ConfigLoader::load_with_local_view(inputs)
            .await
            .expect("load must succeed");

        assert_eq!(
            loaded.merged.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("managed-registry"),
            "managed snapshot payload must fold above the home tier"
        );
        assert_eq!(
            loaded.merged.managed.as_ref().and_then(|m| m.source.as_deref()),
            Some("registry.test/managed-config:v1"),
            "the payload's embedded [managed] section must be stripped and never override \
             the seed source (one-hop, ADR Decision I)"
        );
        assert!(
            loaded.local_only.registry.is_none(),
            "the local-only view must exclude the network-sourced managed tier"
        );

        // --config overlay (highest precedence) must still beat the managed tier.
        let overlay_dir = TempDir::new().unwrap();
        let overlay = write_config(
            &overlay_dir,
            "overlay.toml",
            "[registry]\ndefault = \"overlay-registry\"\n",
        );
        let inputs = ConfigInputs {
            explicit_path: Some(&overlay),
            explicit_project_path: None,
            cwd: None,
        };
        let loaded = ConfigLoader::load_with_local_view(inputs)
            .await
            .expect("load must succeed");
        assert_eq!(
            loaded.merged.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("overlay-registry"),
            "--config overlay must beat the managed snapshot"
        );
    }

    /// A managed payload's `[update]` never reaches the merged config, while the local tier's
    /// `[update]` survives the fold: a publisher must not be able to switch on binary replacement.
    ///
    /// Red state: delete both the `strip_managed_update` call and the `parsed.update = None` in
    /// `fold_managed_tier`, and the payload's `self = "apply"` wins over the local `manual`.
    #[tokio::test]
    async fn managed_payload_update_section_is_ignored() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        std::fs::write(
            dir.path().join("config.toml"),
            "[managed]\nsource = \"registry.test/managed-config:v1\"\nrequired = false\n[update]\nself = \"manual\"\n",
        )
        .unwrap();
        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[registry]\ndefault = \"managed-registry\"\n[update]\nself = \"apply\"\ninterval = \"0\"\n",
        );

        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let loaded = ConfigLoader::load_with_local_view(inputs)
            .await
            .expect("load must succeed");

        assert_eq!(
            loaded.merged.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("managed-registry"),
            "the payload must actually fold, or the drop below is untested"
        );
        let update = loaded.merged.update.expect("the local [update] must survive");
        assert_eq!(update.self_policy, Some(Ok(crate::refresh::RefreshPolicy::Manual)));
        assert_eq!(
            update.interval, None,
            "no payload [update] key may reach the merged config"
        );
    }

    /// A malformed `[update]` in a managed payload is dropped before the typed parse, so the rest of
    /// the payload still applies: fleets run mixed ocx versions under one payload.
    #[tokio::test]
    async fn managed_payload_with_a_malformed_update_still_applies() {
        // A bare key must precede the first table header, or TOML files it under that table.
        for payload in [
            "update = 1\n[registry]\ndefault = \"managed-registry\"\n",
            "[registry]\ndefault = \"managed-registry\"\n[update]\ninterval = []\n",
        ] {
            let env = ocx_env::overrides::lock();
            let dir = TempDir::new().unwrap();
            env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
            env.remove(&ocx_env::OCX_CONFIG);
            env.remove(&ocx_env::OCX_NO_CONFIG);
            env.remove(&ocx_env::OCX_MANAGED_CONFIG);

            std::fs::write(
                dir.path().join("config.toml"),
                "[managed]\nsource = \"registry.test/managed-config:v1\"\nrequired = false\n",
            )
            .unwrap();
            write_managed_snapshot(dir.path(), "registry.test/managed-config:v1", payload);

            let inputs = ConfigInputs {
                explicit_path: None,
                explicit_project_path: None,
                cwd: None,
            };
            let loaded = ConfigLoader::load_with_local_view(inputs)
                .await
                .expect("load must succeed");
            assert_eq!(
                loaded.merged.registry.as_ref().and_then(|r| r.default.as_deref()),
                Some("managed-registry"),
                "{payload:?}: the payload must still apply"
            );
            assert!(loaded.merged.update.is_none(), "{payload:?}");
        }
    }

    /// A wrongly typed local `[update]` never fails the load, or every command exits 78.
    #[tokio::test]
    async fn local_malformed_update_never_fails_the_load() {
        for local_update in ["update = 1\n", "[update]\ninterval = 3600\n", "[update]\nself = 5\n"] {
            let env = ocx_env::overrides::lock();
            let dir = TempDir::new().unwrap();
            env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
            env.remove(&ocx_env::OCX_CONFIG);
            env.remove(&ocx_env::OCX_NO_CONFIG);
            env.remove(&ocx_env::OCX_MANAGED_CONFIG);
            std::fs::write(dir.path().join("config.toml"), local_update).unwrap();

            let inputs = ConfigInputs {
                explicit_path: None,
                explicit_project_path: None,
                cwd: None,
            };
            let loaded = ConfigLoader::load_with_local_view(inputs).await;
            assert!(loaded.is_ok(), "{local_update:?}: {:?}", loaded.err());
        }
    }

    /// Criterion 7 (loader-level): a snapshot whose embedded provenance does
    /// not match the effective source must be treated as absent — content
    /// never reaches `Config`, mirrors/registry/patches included.
    #[tokio::test]
    async fn managed_snapshot_source_mismatch_is_never_merged() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        std::fs::write(
            dir.path().join("config.toml"),
            "[managed]\nsource = \"registry.test/managed-config:v1\"\n",
        )
        .unwrap();
        write_managed_snapshot(
            dir.path(),
            "other.test/managed-config:v1",
            "[registry]\ndefault = \"poisoned-registry\"\n",
        );

        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("load must succeed despite the mismatch");
        assert!(
            config.registry.is_none(),
            "a source-mismatched snapshot must never merge its payload, got: {config:?}"
        );
    }

    /// W6: a snapshot whose embedded `config` payload is corrupt TOML must be
    /// treated as absent by the loader fold (debug-logged, never a hard error,
    /// never a partial merge) — same benign-state posture as a corrupt
    /// snapshot file.
    #[tokio::test]
    async fn managed_snapshot_corrupt_embedded_toml_treated_as_absent() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        std::fs::write(
            dir.path().join("config.toml"),
            "[managed]\nsource = \"registry.test/managed-config:v1\"\nrequired = false\n",
        )
        .unwrap();
        // Identity matches, but the embedded payload is not valid TOML.
        write_managed_snapshot(dir.path(), "registry.test/managed-config:v1", "not = [valid");

        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("load must succeed despite the corrupt embedded payload");
        assert!(
            config.registry.is_none(),
            "a corrupt embedded payload must never partially merge, got: {config:?}"
        );
        assert_eq!(
            config.managed.as_ref().and_then(|managed| managed.source.as_deref()),
            Some("registry.test/managed-config:v1"),
            "the seed itself stays intact when the snapshot payload is corrupt"
        );
    }

    /// ADR Decision A: the effective source for the identity gate is env
    /// `OCX_MANAGED_CONFIG` (when set) over the seed's `managed.source`.
    #[tokio::test]
    async fn managed_snapshot_identity_gate_uses_env_override_when_set() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);

        std::fs::write(
            dir.path().join("config.toml"),
            "[managed]\nsource = \"seed.test/managed-config:v1\"\n",
        )
        .unwrap();
        // The snapshot's provenance matches the ENV override, not the seed.
        env.set(&ocx_env::OCX_MANAGED_CONFIG, "override.test/managed-config:v1");
        write_managed_snapshot(
            dir.path(),
            "override.test/managed-config:v1",
            "[registry]\ndefault = \"env-override-registry\"\n",
        );

        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let loaded = ConfigLoader::load_with_local_view(inputs)
            .await
            .expect("load must succeed");
        assert_eq!(
            loaded.merged.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("env-override-registry"),
            "the identity gate must use the env-overridden source, matching the snapshot fetched under it"
        );
    }

    /// Amended post-Codex-gate 2026-07-05 (ADR "Loader integration"): a
    /// `[managed].source` seed declared ONLY in the `--config`/`OCX_CONFIG`
    /// overlay (no home/system/user tier at all) must still activate the
    /// fold — the payload's non-conflicting values become visible in
    /// `merged`, while the overlay's own conflicting value still wins.
    #[tokio::test]
    async fn managed_snapshot_seed_only_in_overlay_still_folds_payload() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);
        // Deliberately no home-tier config.toml — the seed exists ONLY in
        // the --config overlay below.

        write_managed_snapshot(
            dir.path(),
            "overlay-only.test/managed-config:v1",
            "[registry]\ndefault = \"payload-registry\"\n[patches]\nregistry = \"payload-patches.example\"\n",
        );

        let overlay_dir = TempDir::new().unwrap();
        let overlay = write_config(
            &overlay_dir,
            "overlay.toml",
            "[managed]\nsource = \"overlay-only.test/managed-config:v1\"\n[registry]\ndefault = \"overlay-registry\"\n",
        );
        let inputs = ConfigInputs {
            explicit_path: Some(&overlay),
            explicit_project_path: None,
            cwd: None,
        };
        let loaded = ConfigLoader::load_with_local_view(inputs)
            .await
            .expect("load must succeed");

        assert_eq!(
            loaded.merged.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("overlay-registry"),
            "the overlay's own [registry] must still beat the payload's on a conflicting key"
        );
        assert_eq!(
            loaded.merged.patches.as_ref().and_then(|p| p.registry.as_deref()),
            Some("payload-patches.example"),
            "an overlay-only [managed].source must still activate the fold, making the \
             payload's non-conflicting values visible in merged"
        );
    }

    /// A snapshot fetched under the `--config` OVERLAY's source merges even
    /// though the home tier declares a DIFFERENT `[managed].source` — the
    /// fold's identity gate must resolve from `local_only` (base + overlay),
    /// not the base tiers alone.
    #[tokio::test]
    async fn managed_snapshot_overlay_source_overrides_home_seed_for_identity_gate() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        std::fs::write(
            dir.path().join("config.toml"),
            "[managed]\nsource = \"home-seed.test/managed-config:v1\"\n",
        )
        .unwrap();
        write_managed_snapshot(
            dir.path(),
            "overlay-seed.test/managed-config:v1",
            "[registry]\ndefault = \"overlay-seed-registry\"\n",
        );

        let overlay_dir = TempDir::new().unwrap();
        let overlay = write_config(
            &overlay_dir,
            "overlay.toml",
            "[managed]\nsource = \"overlay-seed.test/managed-config:v1\"\n",
        );
        let inputs = ConfigInputs {
            explicit_path: Some(&overlay),
            explicit_project_path: None,
            cwd: None,
        };
        let loaded = ConfigLoader::load_with_local_view(inputs)
            .await
            .expect("load must succeed");

        assert_eq!(
            loaded.merged.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("overlay-seed-registry"),
            "a snapshot provisioned for the OVERLAY's source must merge when the overlay \
             declares a different [managed].source than the home tier"
        );
    }

    /// Sibling to the above: a snapshot fetched under the HOME tier's source
    /// is treated as absent once the overlay declares a DIFFERENT source —
    /// the fold and `resolve_managed_config`'s `required` gate must agree on
    /// which source is effective, or a stale snapshot could silently satisfy
    /// the wrong identity.
    #[tokio::test]
    async fn managed_snapshot_home_seed_source_is_absent_once_overlay_overrides_it() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        std::fs::write(
            dir.path().join("config.toml"),
            "[managed]\nsource = \"home-seed.test/managed-config:v1\"\n",
        )
        .unwrap();
        write_managed_snapshot(
            dir.path(),
            "home-seed.test/managed-config:v1",
            "[registry]\ndefault = \"stale-home-registry\"\n",
        );

        let overlay_dir = TempDir::new().unwrap();
        let overlay = write_config(
            &overlay_dir,
            "overlay.toml",
            "[managed]\nsource = \"overlay-seed.test/managed-config:v1\"\n",
        );
        let inputs = ConfigInputs {
            explicit_path: Some(&overlay),
            explicit_project_path: None,
            cwd: None,
        };
        let loaded = ConfigLoader::load_with_local_view(inputs)
            .await
            .expect("load must succeed despite the mismatch");

        assert!(
            loaded.merged.registry.is_none(),
            "a snapshot fetched under the HOME tier's source must be treated as absent once \
             the overlay declares a different source, got: {:?}",
            loaded.merged.registry
        );
    }

    /// A malformed `snapshot.json` (not valid JSON) must be treated as absent
    /// — the loader never fails the whole config load because of a corrupt
    /// managed-config snapshot (no new loader error variant, ADR Decision A).
    #[tokio::test]
    async fn managed_snapshot_malformed_json_is_treated_as_absent() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        std::fs::write(
            dir.path().join("config.toml"),
            "[managed]\nsource = \"registry.test/managed-config:v1\"\n",
        )
        .unwrap();
        let snapshot_path = crate::managed_config::ManagedConfigPaths::for_ocx_home(dir.path()).snapshot_file();
        std::fs::create_dir_all(snapshot_path.parent().unwrap()).unwrap();
        std::fs::write(&snapshot_path, b"not valid json {{{").unwrap();

        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("a corrupt managed-config snapshot must not fail the whole config load");
        assert!(config.registry.is_none(), "corrupt snapshot must be treated as absent");
    }

    /// Criterion 26: `OCX_NO_CONFIG=1` suppresses the managed-config candidate
    /// AND disables the `OCX_MANAGED_CONFIG` env-override read entirely
    /// (hermetic means hermetic).
    #[tokio::test]
    async fn no_config_suppresses_managed_snapshot_even_with_matching_env_override() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        without_system_config(&env);
        env.remove(&ocx_env::OCX_CONFIG);
        env.set(&ocx_env::OCX_MANAGED_CONFIG, "registry.test/managed-config:v1");

        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[registry]\ndefault = \"should-never-appear\"\n",
        );

        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("OCX_NO_CONFIG=1 must still succeed");
        assert!(
            config.registry.is_none(),
            "OCX_NO_CONFIG=1 must suppress the managed-config candidate even with a matching snapshot"
        );
    }

    /// Criterion 28 (unit-level substitute — mirrors the sanctioned pattern in
    /// `test/tests/test_patches.py::test_launcher_digest_matched_opt_out_respects_system_required`:
    /// `system_locked` is only ever set by the loader after parsing the
    /// SYSTEM-scope `/etc/ocx/config.toml`, which acceptance tests cannot
    /// write without root). A system-locked `[registry]` on the accumulator
    /// must survive a managed-payload redirection attempt: `fold_managed_tier`
    /// reuses `Config::merge`, which already respects `system_locked`.
    #[tokio::test]
    async fn managed_snapshot_cannot_override_system_locked_registry() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[registry]\ndefault = \"malicious-registry.example\"\n",
        );

        // Simulate an accumulator that already folded a locked SYSTEM tier
        // (in production this comes from `/etc/ocx/config.toml` via
        // `load_and_merge`'s `lock_as_system` branch) plus a home tier whose
        // `[managed].source` matches the snapshot above.
        let mut registry = crate::RegistryDefaults {
            default: Some("system-locked-registry.example".to_string()),
            system_locked: false,
        };
        registry.lock_as_system();
        let accumulator = crate::Config {
            registry: Some(registry),
            managed: Some(crate::managed::ManagedConfig {
                source: Some("registry.test/managed-config:v1".to_string()),
                required: Some(false),
                ..Default::default()
            }),
            ..crate::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved, _state, _) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
            .await
            .expect("fold must succeed even against a locked accumulator");
        assert_eq!(
            folded.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("system-locked-registry.example"),
            "a system-locked [registry] must survive a managed-payload redirection attempt"
        );
    }

    /// Regression (Codex-flagged 2026-07-05): the identity gate here must use
    /// the SAME lock-aware source resolution as `resolve_managed_config`'s
    /// `required` gate. Before the fix, this gate resolved a mismatched
    /// `OCX_MANAGED_CONFIG` override directly (ignoring the system lock),
    /// while the required gate (via `resolve_managed_target`) correctly
    /// ignored the override and fell back to the locked seed — so the two
    /// gates disagreed on the effective source. Net effect: the snapshot for
    /// the LOCKED source was compared against the mismatched override,
    /// silently NOT folded, while required-enforcement separately resolved
    /// back to the locked source and reported the same snapshot as
    /// satisfying — a required corporate config tier silently vanished with
    /// no error. This test locks the two gates together: a system-locked
    /// `[managed]` source must still fold its own snapshot even when
    /// `OCX_MANAGED_CONFIG` names a different (mismatched) source.
    #[tokio::test]
    async fn managed_snapshot_system_locked_source_folds_despite_mismatched_env_override() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.set(&ocx_env::OCX_MANAGED_CONFIG, "hostile.test/evil-config:latest");

        write_managed_snapshot(
            dir.path(),
            "system.corp/ocx-config:user",
            "[registry]\ndefault = \"corp-registry.example\"\n",
        );

        // Simulate an accumulator/local-only view that already folded a
        // locked SYSTEM tier (in production: `/etc/ocx/config.toml` via
        // `load_and_merge`'s `lock_as_system` branch).
        let mut managed = crate::managed::ManagedConfig {
            source: Some("system.corp/ocx-config:user".to_string()),
            required: Some(true),
            ..Default::default()
        };
        managed.lock_as_system();
        let accumulator = crate::Config {
            managed: Some(managed),
            ..crate::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, snapshot, _resolved, _state, _) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
            .await
            .expect("fold must succeed");
        assert!(
            snapshot.is_some(),
            "the on-disk snapshot must still be read and returned"
        );
        assert_eq!(
            folded.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("corp-registry.example"),
            "a system-locked [managed] source must fold its own snapshot even when OCX_MANAGED_CONFIG names a \
             mismatched source — the identity gate must ignore the same override resolve_managed_target ignores"
        );
    }

    /// End-to-end half of the required-gate fix: an identity-matching
    /// snapshot whose payload is not valid TOML folds NOTHING and reports
    /// [`ManagedSnapshotState::PayloadUnusable`](crate::managed::ManagedSnapshotState::PayloadUnusable),
    /// so `Context::try_init` can fail a `required` tier closed on it. The
    /// state — not the returned snapshot, which is still `Some` for
    /// `config update --check` — is what the gate reads.
    #[tokio::test]
    async fn managed_snapshot_unparseable_payload_folds_nothing_and_reports_unusable() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        write_managed_snapshot(dir.path(), "corp.example.com/ocx-config:user", "not = [valid toml");

        let accumulator = crate::Config {
            managed: Some(crate::managed::ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..Default::default()
            }),
            ..crate::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, snapshot, _resolved, state, _) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
            .await
            .expect("a broken payload must not fail the load");
        assert!(
            snapshot.is_some(),
            "the snapshot is still surfaced so `config update --check` can diagnose it"
        );
        assert!(
            folded.registry.is_none(),
            "an unparseable payload must contribute nothing to the merged config"
        );
        assert_eq!(
            state,
            crate::managed::ManagedSnapshotState::PayloadUnusable,
            "the loader must report what it actually applied, not merely that the identity matched"
        );
    }

    /// The discriminator for the test above: a payload carrying sections and
    /// keys this binary does not know still folds, and its known settings
    /// reach the merged config. `PayloadUnusable` must mean "broken", never
    /// "unfamiliar" — the latter would fail a fleet closed on every rollout.
    #[tokio::test]
    async fn managed_snapshot_payload_from_a_newer_ocx_folds_and_reports_applied() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        write_managed_snapshot(
            dir.path(),
            "corp.example.com/ocx-config:user",
            "[registry]\ndefault = \"corp-registry.example\"\ntimeout = 30\n[toolchain]\nchannel = \"stable\"\n",
        );

        let accumulator = crate::Config {
            managed: Some(crate::managed::ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..Default::default()
            }),
            ..crate::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved, state, _) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
            .await
            .expect("fold must succeed");
        assert_eq!(
            folded
                .registry
                .as_ref()
                .and_then(|registry| registry.default.as_deref()),
            Some("corp-registry.example"),
            "the settings this binary understands must survive the unknown ones"
        );
        assert_eq!(state, crate::managed::ManagedSnapshotState::Applied);
    }

    /// Regression (review round 2): the `[managed]` lock call was missing
    /// from the system-scope wiring — `ManagedConfig::lock_as_system` existed
    /// but was never invoked, so criterion 13 was unenforced dead code. Pins
    /// that `apply_system_locks` covers every lockable section, so a newly
    /// added lockable section that misses the wiring fails here.
    #[test]
    fn apply_system_locks_covers_every_lockable_section() {
        let mut config: crate::Config = toml::from_str(concat!(
            "[patches]\nregistry = \"patches.corp.example\"\nrequired = true\n",
            "[registry]\ndefault = \"corp\"\n",
            "[registries.corp]\nindex = \"https://registry.corp.example\"\n",
            "[mirrors]\n\"docker.io\" = \"https://mirror.corp.example\"\n",
            "[managed]\nsource = \"corp/managed-config:stable\"\nrequired = true\n",
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n",
            "signers = [{ kind = \"keyless\", identity = \"ci@acme.example\", oidc_issuer = \"https://iss.example\" }]\n",
            "[records]\ndir = \"/var/log/ocx/records\"\nrequired = true\n",
        ))
        .unwrap();
        // A root-level scalar has to precede every table header, so it is
        // spliced in rather than appended.
        config.extra_ca_certs_pem = Some("system".to_string());

        ConfigLoader::apply_system_locks(&mut config);

        assert!(
            config.extra_ca_certs_system_locked,
            "extra_ca_certs / extra_ca_certs_pem must lock when the system file sets either key"
        );
        assert!(
            config.patches.unwrap().system_locked,
            "[patches] (required=true) must lock"
        );
        assert!(config.registry.unwrap().system_locked, "[registry] must lock");
        assert!(
            config.registries.unwrap().values().all(|entry| entry.system_locked),
            "every [registries.<name>] entry must lock"
        );
        assert!(
            config
                .mirrors
                .unwrap()
                .values()
                .all(|mirror| mirror.registry_system_locked && mirror.index_system_locked),
            "every [mirrors.\"<host>\"] entry must lock every role it declares"
        );
        assert!(
            config.managed.unwrap().system_locked,
            "[managed] must lock (criterion 13)"
        );
        assert!(
            config.trust.unwrap().policy.iter().all(|policy| policy.system_locked),
            "every [[trust.policy]] entry must lock"
        );
        assert!(
            config.records.unwrap().system_locked,
            "[records] must lock — a system-scope sink is what makes recording a fleet property"
        );
    }

    // ── OCX_NO_CONFIG vs. the SYSTEM lock ───────────────────────────────────
    //
    // `OCX_NO_CONFIG=1` prunes ambient configuration, not operator policy. The
    // pair of directions below is what makes that claim testable: the locked
    // section must survive the flag, and everything else must still be pruned
    // by it — without the second half these tests would pass against a loader
    // that simply stopped honouring `OCX_NO_CONFIG`.

    /// A sink path this host's `Path::is_absolute` agrees with.
    ///
    /// A POSIX `/var/log/...` is only *root-relative* on Windows, so the
    /// anchoring seam rewrites it there — correctly, but it then stops being the
    /// "operator spelled it out in full" case these tests are about.
    fn absolute_sink(tail: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:/{tail}"))
        } else {
            PathBuf::from(format!("/{tail}"))
        }
    }

    /// A TOML `<key> = <path>` line whose value is escaped **by the TOML
    /// serializer**, never by hand.
    ///
    /// `format!("{key} = \"{}\"", path.display())` is a fixture hand-rolling a
    /// serializer for a format a library already owns, and it is wrong on
    /// Windows: a basic string reads backslash as an escape introducer, so a
    /// real temporary directory — `C:\Users\RUNNER~1\AppData\Local\Temp\…` —
    /// parses as `\U`, `\A` and `\T` and the loader refuses the file with
    /// "too few unicode value digits, expected unicode hexadecimal value".
    /// `toml::Value`'s own `Display` owns the quoting and escaping
    /// (`quality-core.md` § *Don't Own Non-Domain Code*).
    fn toml_path_line(key: &str, path: &Path) -> String {
        let value = toml::Value::from(path.to_str().expect("fixture paths are utf-8"));
        format!("{key} = {value}\n")
    }

    /// The published guarantee (`reference/execution-records.md`): "no caller
    /// can opt out of a sink the operator has locked at system scope."
    /// `OCX_NO_CONFIG=1` is a caller, and used to be the one way out.
    #[tokio::test]
    async fn no_config_keeps_a_system_locked_records_policy() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, home.path().to_str().unwrap());
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.remove(&ocx_env::OCX_CONFIG);
        let sink = absolute_sink("var/log/ocx/records");
        with_system_config(
            &env,
            &dir,
            &format!(
                "[records]\n{}name = \"{{time}}.json\"\nrequired = true\n",
                toml_path_line("dir", &sink)
            ),
        );

        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("OCX_NO_CONFIG=1 must still succeed");

        let records = config
            .records
            .expect("a SYSTEM-locked [records] must survive OCX_NO_CONFIG=1");
        assert!(records.system_locked, "the clamp must reach the resolver");
        assert_eq!(records.dir, Some(sink));
        assert_eq!(records.name.as_deref(), Some("{time}.json"));
        assert_eq!(records.required, Some(true));
    }

    /// The lock outranks the explicit tiers under the flag too — `--config` is
    /// the loudest caller channel there is, and a locked block still wins.
    #[tokio::test]
    async fn no_config_system_locked_records_beats_an_explicit_config_file() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, home.path().to_str().unwrap());
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.remove(&ocx_env::OCX_CONFIG);
        let sink = absolute_sink("var/log/ocx/records");
        with_system_config(
            &env,
            &dir,
            &format!("[records]\n{}required = true\n", toml_path_line("dir", &sink)),
        );
        let caller = write_config(
            &dir,
            "caller.toml",
            "[records]\ndir = \"/tmp/caller\"\nrequired = false\n",
        );

        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: Some(&caller),
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load should succeed");

        let records = config.records.expect("locked [records] must be present");
        assert_eq!(
            records.dir,
            Some(sink),
            "an explicit --config file must not redirect a locked sink"
        );
        assert_eq!(records.required, Some(true), "nor loosen the posture");
    }

    /// Discriminator #1: an *unlocked* `[records]` — one that reached the
    /// config from the `$OCX_HOME` tier rather than from system scope — is
    /// still pruned by the flag, exactly as before.
    #[tokio::test]
    async fn no_config_prunes_an_unlocked_home_tier_records_section() {
        let env = ocx_env::overrides::lock();
        let home = TempDir::new().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[records]\ndir = \"/tmp/home-tier-records\"\n",
        )
        .unwrap();
        env.set(&ocx_env::OCX_HOME, home.path().to_str().unwrap());
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.remove(&ocx_env::OCX_CONFIG);
        without_system_config(&env);

        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load should succeed");

        assert!(
            config.records.is_none(),
            "OCX_NO_CONFIG=1 must still prune an unlocked $OCX_HOME [records] section"
        );
    }

    /// Discriminator #2: the SYSTEM file is filtered, not loaded wholesale.
    /// `[patches] required = false` is the operator explicitly declining to
    /// enforce, so it does not lock — it is ordinary configuration and the flag
    /// still prunes it, while the `[records]` block in the same file survives.
    #[tokio::test]
    async fn no_config_prunes_system_sections_that_did_not_lock() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, home.path().to_str().unwrap());
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.remove(&ocx_env::OCX_CONFIG);
        with_system_config(
            &env,
            &dir,
            concat!(
                "[patches]\nregistry = \"patches.corp.example\"\nrequired = false\n",
                "[records]\ndir = \"/var/log/ocx/records\"\n",
            ),
        );

        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load should succeed");

        assert!(
            config.patches.is_none(),
            "an unlocked system [patches] is ordinary configuration and must still be pruned"
        );
        assert!(
            config.records.is_some(),
            "the locked [records] block in the same file must survive"
        );
    }

    /// `[managed]` stays fully suppressed under the flag even though it locks:
    /// `OCX_NO_CONFIG` also suppresses the snapshot read, so a retained seed
    /// could never be satisfied and a `required` tier (the default) would fail
    /// every hermetic invocation instead of enforcing anything.
    #[tokio::test]
    async fn no_config_still_suppresses_a_system_managed_tier() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, home.path().to_str().unwrap());
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);
        with_system_config(
            &env,
            &dir,
            "[managed]\nsource = \"corp/managed-config:stable\"\nrequired = true\n",
        );

        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load should succeed");

        assert!(
            config.managed.is_none(),
            "a system [managed] seed must stay suppressed under OCX_NO_CONFIG=1"
        );
    }

    /// Every section the lock pass clamps survives the filter (and nothing
    /// else does). The compile-time gate is the exhaustive destructure inside
    /// `retain_system_locked_sections`; this pins the runtime half.
    #[test]
    fn retain_system_locked_sections_keeps_every_locked_section() {
        let mut config: crate::Config = toml::from_str(concat!(
            "[patches]\nregistry = \"patches.corp.example\"\nrequired = true\n",
            "[registry]\ndefault = \"corp\"\n",
            "[registries.corp]\nindex = \"https://registry.corp.example\"\n",
            "[mirrors]\n\"docker.io\" = \"https://mirror.corp.example\"\n",
            "[managed]\nsource = \"corp/managed-config:stable\"\nrequired = true\n",
            "[records]\ndir = \"/var/log/ocx/records\"\nrequired = true\n",
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n",
            "signers = [{ kind = \"keyless\", identity = \"ci@acme.example\", oidc_issuer = \"https://iss.example\" }]\n",
            "[shell]\nhook = true\n",
        ))
        .unwrap();
        // A root-level scalar has to precede every table header, so it is
        // spliced in rather than appended.
        config.toolchain_dir = Some(PathBuf::from("/home/operator/tc"));
        config.extra_ca_certs_pem = Some("system".to_string());

        ConfigLoader::apply_system_locks(&mut config);
        ConfigLoader::retain_system_locked_sections(&mut config);

        assert_eq!(
            config.extra_ca_certs_pem.as_deref(),
            Some("system"),
            "a locked extra_ca_certs_pem must survive (ocx#469)"
        );
        assert!(config.patches.is_some(), "locked [patches] must survive");
        assert!(config.registry.is_some(), "locked [registry] must survive");
        assert!(config.registries.is_some(), "locked [registries.<name>] must survive");
        assert!(config.mirrors.is_some(), "locked [mirrors.\"<host>\"] must survive");
        assert!(config.records.is_some(), "locked [records] must survive");
        assert!(
            config.trust.is_some_and(|trust| trust.policy.len() == 1),
            "a locked [[trust.policy]] entry must survive"
        );
        assert!(config.managed.is_none(), "[managed] is dropped even when locked");
        assert!(
            config.shell.is_none(),
            "[shell] locks nothing, so the flag prunes it like any ambient tier"
        );
        assert!(
            config.toolchain_dir.is_none(),
            "`toolchain_dir` locks nothing either, so the flag prunes it like [shell] (C-016)"
        );
    }

    /// The filter is lock-driven, not section-driven: the same sections parsed
    /// from a NON-system file (no lock pass) are all pruned.
    #[test]
    fn retain_system_locked_sections_drops_everything_unlocked() {
        let mut config: crate::Config = toml::from_str(concat!(
            "[patches]\nregistry = \"patches.corp.example\"\nrequired = true\n",
            "[registry]\ndefault = \"corp\"\n",
            "[registries.corp]\nindex = \"https://registry.corp.example\"\n",
            "[mirrors]\n\"docker.io\" = \"https://mirror.corp.example\"\n",
            "[records]\ndir = \"/var/log/ocx/records\"\nrequired = true\n",
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n",
            "signers = [{ kind = \"keyless\", identity = \"ci@acme.example\", oidc_issuer = \"https://iss.example\" }]\n",
        ))
        .unwrap();
        config.toolchain_dir = Some(PathBuf::from("/home/operator/tc"));
        config.extra_ca_certs = Some(PathBuf::from("/home/operator/corp-ca.pem"));

        ConfigLoader::retain_system_locked_sections(&mut config);

        assert!(
            config.extra_ca_certs.is_none(),
            "an unlocked extra_ca_certs is pruned too (ocx#469)"
        );
        assert!(config.patches.is_none());
        assert!(config.registry.is_none());
        assert!(config.registries.is_none(), "an emptied table collapses to None");
        assert!(config.mirrors.is_none(), "an emptied table collapses to None");
        assert!(config.records.is_none());
        assert!(config.trust.is_none(), "an emptied policy list collapses to None");
        assert!(
            config.toolchain_dir.is_none(),
            "an unlocked `toolchain_dir` is pruned too"
        );
    }

    /// `[update]` is a personal setting with no lock, so a hermetic run keeps none of it.
    #[test]
    fn retain_system_locked_sections_drops_update() {
        let mut config: crate::Config = toml::from_str("[update]\nself = \"manual\"\n").unwrap();
        assert!(config.update.is_some(), "the fixture must carry [update]");

        ConfigLoader::retain_system_locked_sections(&mut config);

        assert!(config.update.is_none());
    }

    /// The other half of the `[records]` clamp: a SYSTEM file that declares no
    /// `[records]` section locks nothing, so an operator who never opted in
    /// does not silently freeze every lower tier out of configuring a sink.
    #[test]
    fn apply_system_locks_leaves_absent_records_section_unlocked() {
        let mut config: crate::Config = toml::from_str("[registry]\ndefault = \"corp\"\n").unwrap();

        ConfigLoader::apply_system_locks(&mut config);

        assert!(
            config.records.is_none(),
            "a system file with no [records] must not synthesize a locked section"
        );
    }

    /// The same "absent locks nothing" half for the extra-CA pair: a
    /// system file that declares neither key leaves every lower tier and
    /// `OCX_EXTRA_CA_CERTS` free to add a root.
    #[test]
    fn apply_system_locks_leaves_absent_extra_ca_certs_unlocked() {
        let mut config: crate::Config = toml::from_str("[registry]\ndefault = \"corp\"\n").unwrap();

        ConfigLoader::apply_system_locks(&mut config);

        assert!(
            !config.extra_ca_certs_system_locked,
            "a system file with neither extra-CA key must not lock the pair"
        );
    }

    /// Same contract as `managed_snapshot_cannot_override_system_locked_registry`,
    /// but for the `[registries.<name>]` entry lock directly. Since §6 removed
    /// `resolved_default_registry`'s indirection through this table entirely,
    /// the entry's own `system_locked` flag is now the only thing protecting
    /// its fields — this pins that a managed payload still cannot override a
    /// system-locked entry's `index` value.
    #[tokio::test]
    async fn managed_snapshot_cannot_override_system_locked_registries_entry() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[registries.corp]\nindex = \"https://malicious-index.example\"\n",
        );

        // Simulate an accumulator that already folded a locked SYSTEM tier:
        // the `[registries.corp]` entry is system-locked (in production via
        // `/etc/ocx/config.toml`'s `lock_as_system` branch).
        let mut corp_entry = crate::RegistryConfig {
            index: Some("https://system-locked-index.example".to_string()),
            ..Default::default()
        };
        corp_entry.lock_as_system();
        let mut registries = std::collections::HashMap::new();
        registries.insert("corp".to_string(), corp_entry);
        let accumulator = crate::Config {
            registries: Some(registries),
            managed: Some(crate::managed::ManagedConfig {
                source: Some("registry.test/managed-config:v1".to_string()),
                required: Some(false),
                ..Default::default()
            }),
            ..crate::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved, _state, _) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
            .await
            .expect("fold must succeed even against a locked accumulator");

        assert_eq!(
            folded.registries.unwrap()["corp"].index.as_deref(),
            Some("https://system-locked-index.example"),
            "a system-locked [registries.<name>] entry must survive a managed-payload redirection attempt"
        );
    }

    // ── [trust.sigstore] anchoring + managed-tier guards ─────────────────────

    #[tokio::test]
    async fn relative_trusted_root_anchors_to_the_declaring_config_dir_not_the_cwd() {
        // The bug this guards is silent: with no anchoring, a relative
        // `trusted_root` resolves against the process working directory, so
        // verification finds the right file whenever the operator happens to
        // run from `/etc/ocx` and mysteriously stops when they cd elsewhere.
        // The tempdir is deliberately NOT the CWD — a test run from inside it
        // would pass either way.
        let dir = TempDir::new().expect("tempdir");
        let path = write_config(
            &dir,
            "config.toml",
            "[trust.sigstore]\ntrusted_root = \"sigstore/trusted-root.json\"\n",
        );

        let config = ConfigLoader::load_and_merge(&[path]).await.expect("load");
        let sigstore = config.trust.expect("trust").sigstore.expect("sigstore");
        let anchored = sigstore.trusted_root.expect("trusted_root");
        assert!(
            anchored.is_absolute(),
            "anchored to an absolute path: {}",
            anchored.display()
        );
        assert_eq!(anchored, dir.path().join("sigstore").join("trusted-root.json"));
    }

    /// `[records] dir` rides the same anchoring seam, and its bug is the same
    /// shape: a relative sink in `/etc/ocx/config.toml` would otherwise resolve
    /// against the process working directory, so an operator's fleet-wide sink
    /// would land in a different place for every directory a build runs from —
    /// scattered records, no error, and a collector reading an empty tree.
    ///
    /// The tempdir is deliberately NOT the CWD: a test run from inside it would
    /// pass either way. An absolute sink is asserted unchanged in the same test,
    /// because the anchoring must not rewrite what the operator fully specified.
    #[tokio::test]
    async fn a_relative_records_dir_anchors_to_the_declaring_config_dir_not_the_cwd() {
        let dir = TempDir::new().expect("tempdir");
        let path = write_config(&dir, "config.toml", "[records]\ndir = \"audit/records\"\n");

        let config = ConfigLoader::load_and_merge(&[path]).await.expect("load");
        let sink = config.records.expect("records").dir.expect("dir");
        assert!(sink.is_absolute(), "anchored to an absolute path: {}", sink.display());
        assert_eq!(sink, dir.path().join("audit").join("records"));

        let spelled_out = absolute_sink("var/log/ocx-records");
        let absolute = write_config(
            &dir,
            "absolute.toml",
            &format!("[records]\n{}", toml_path_line("dir", &spelled_out)),
        );
        let config = ConfigLoader::load_and_merge(&[absolute]).await.expect("load");
        assert_eq!(
            config.records.expect("records").dir.expect("dir"),
            spelled_out,
            "an absolute sink is the operator's final word and must pass through untouched"
        );
    }

    /// The signer-key twin of the test above, riding the same seam. Its bug is
    /// silent in exactly the same way: with no anchoring, a relative
    /// `key = "keys/acme.pub"` in `/etc/ocx/config.toml` resolves against the process
    /// working directory, so verification finds the key whenever the operator
    /// runs from `/etc/ocx` and mysteriously stops when they cd elsewhere. The
    /// tempdir is deliberately NOT the CWD — a test run from inside it would
    /// pass either way.
    ///
    /// Ordinary path resolution, not a containment check: nothing here
    /// restricts where a key may live.
    #[tokio::test]
    async fn a_relative_signer_key_anchors_to_the_declaring_config_dir_not_the_cwd() {
        let dir = TempDir::new().expect("tempdir");
        let path = write_config(
            &dir,
            "config.toml",
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\nsigners = [{ kind = \"key\", key = \"keys/acme.pub\" }]\n",
        );

        let config = ConfigLoader::load_and_merge(&[path]).await.expect("load");
        let ocx_trust::SignerSpec::Key(matcher) = &config.trust_policies()[0].signers[0] else {
            panic!("still a key signer");
        };
        let anchored = std::path::PathBuf::from(matcher.key.as_deref().expect("the key reference survives"));
        assert!(
            anchored.is_absolute(),
            "anchored to an absolute path: {}",
            anchored.display()
        );
        assert_eq!(anchored, dir.path().join("keys").join("acme.pub"));
    }

    /// An absolute reference already names one file, and an inline `key_pem`
    /// names none — rewriting either would corrupt it.
    #[tokio::test]
    async fn an_absolute_signer_key_and_an_inline_pem_survive_loading_unchanged() {
        let dir = TempDir::new().expect("tempdir");
        let absolute = dir.path().join("elsewhere").join("acme.pub");
        let path = write_config(
            &dir,
            "config.toml",
            // The path is quoted by the TOML serializer rather than by the
            // format string: a Windows tempdir is `C:\Users\…`, and `\U` in a
            // basic string is a unicode escape, so an interpolated `"{}"` makes
            // the fixture unparseable on exactly one platform.
            &format!(
                "[[trust.policy]]\nscope = \"a/*\"\nsigners = [{{ kind = \"key\", key = {} }}]\n\
                 [[trust.policy]]\nscope = \"b/*\"\nsigners = [{{ kind = \"key\", key_pem = \"inline\" }}]\n",
                toml::Value::from(absolute.display().to_string())
            ),
        );

        let config = ConfigLoader::load_and_merge(&[path]).await.expect("load");
        let policies = config.trust_policies();
        let ocx_trust::SignerSpec::Key(by_path) = &policies[0].signers[0] else {
            panic!("still a key signer");
        };
        assert_eq!(by_path.key.as_deref(), Some(absolute.display().to_string().as_str()));
        let ocx_trust::SignerSpec::Key(inline) = &policies[1].signers[0] else {
            panic!("still a key signer");
        };
        assert_eq!(inline.key, None, "an inline pem gains no path");
        assert_eq!(inline.key_pem.as_deref(), Some("inline"));
    }

    #[tokio::test]
    async fn absolute_trusted_root_survives_loading_unchanged() {
        let dir = TempDir::new().expect("tempdir");
        let absolute = dir.path().join("elsewhere").join("trusted-root.json");
        let path = write_config(
            &dir,
            "config.toml",
            // Quoted by the TOML serializer, not by the format string — see the
            // signer-key twin above for the Windows path that breaks otherwise.
            &format!(
                "[trust.sigstore]\ntrusted_root = {}\n",
                toml::Value::from(absolute.display().to_string())
            ),
        );

        let config = ConfigLoader::load_and_merge(&[path]).await.expect("load");
        let sigstore = config.trust.expect("trust").sigstore.expect("sigstore");
        assert_eq!(sigstore.trusted_root.as_deref(), Some(absolute.as_path()));
    }

    fn managed_payload_after_guard(payload: &str, source: &str) -> ocx_trust::SigstoreTrust {
        let mut parsed: Config = toml::from_str(payload).expect("payload parses");
        let source =
            ocx_oci::OciIdentifier::parse_target(source, ocx_oci::DEFAULT_REGISTRY).expect("identifier parses");
        ConfigLoader::guard_managed_sigstore_trust(&mut parsed, &source);
        parsed.trust.expect("trust").sigstore.expect("sigstore")
    }

    /// Any well-formed SPKI PEM; the guard strips by *spelling*, never by
    /// whether the material parses, so a real key would prove nothing extra.
    const INLINE_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA\n-----END PUBLIC KEY-----\n";

    fn managed_trust_after_guard(payload: &str, source: &str) -> ocx_trust::TrustConfig {
        let mut parsed: Config = toml::from_str(payload).expect("payload parses");
        let source =
            ocx_oci::OciIdentifier::parse_target(source, ocx_oci::DEFAULT_REGISTRY).expect("identifier parses");
        ConfigLoader::guard_managed_sigstore_trust(&mut parsed, &source);
        parsed.trust.expect("trust")
    }

    /// The consumer-side half of the publish-time refusal: a `key` signer
    /// naming a path in a managed payload names the *publisher's* disk, so the
    /// consumer must never read whatever sits at that path locally.
    ///
    /// The payload carries no `[trust.sigstore]` on purpose — the guard used to
    /// return early when that table was absent, so a policy-only payload would
    /// have walked straight past this strip.
    #[test]
    fn managed_tier_drops_a_key_signer_named_by_path() {
        let trust = managed_trust_after_guard(
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n\
             signers = [{ kind = \"key\", key = \"/home/operator/acme.pub\" }]\n",
            "ghcr.io/acme/config@sha256:1111111111111111111111111111111111111111111111111111111111111111",
        );
        assert!(
            matches!(&trust.policy[0].signers[0], ocx_trust::SignerSpec::Unknown),
            "a key signer left with no key at all must narrow to nothing, not linger as an \
             unsatisfiable KeyMatcher: {:?}",
            trust.policy[0].signers[0]
        );
        assert!(
            trust.policy[0].clone().compile().is_err(),
            "a policy whose only signer narrowed away must be refused, never accepted as trust-anyone"
        );
    }

    /// Dropping the path form must not take the rest of the policy with it.
    ///
    /// Blanking `key` in place leaves a `KeyMatcher` with neither `key` nor
    /// `key_pem`, which `validate_signers` refuses by name — so one legacy
    /// payload would turn a fleet-wide scope into a hard config error on every
    /// covered command, naming a signer the operator never wrote.
    #[test]
    fn managed_tier_drops_only_the_path_signer_and_keeps_its_siblings() {
        let trust = managed_trust_after_guard(
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n\
             signers = [\n\
               { kind = \"key\", key = \"/home/operator/acme.pub\" },\n\
               { kind = \"keyless\", identity = \"ci@acme.example\", oidc_issuer = \"https://token.actions.githubusercontent.com\" },\n\
             ]\n",
            "ghcr.io/acme/config@sha256:1111111111111111111111111111111111111111111111111111111111111111",
        );

        let compiled = trust.policy[0]
            .clone()
            .compile()
            .expect("the keyless sibling still compiles after the key-by-path signer is dropped");
        assert_eq!(compiled.backends.len(), 1, "exactly the keyless backend survives");
        assert!(matches!(compiled.backends[0], ocx_trust::PolicyBackend::Keyless(_)));
    }

    /// The other direction, or the strip above would be indistinguishable from
    /// "managed payloads carry no key signers at all": inline material travels
    /// with the payload, names no file, and is left exactly as published.
    #[test]
    fn managed_tier_keeps_an_inline_key_signer() {
        let trust = managed_trust_after_guard(
            &format!(
                "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n\
                 signers = [{{ kind = \"key\", key_pem = \"\"\"{INLINE_KEY_PEM}\"\"\" }}]\n"
            ),
            "ghcr.io/acme/config@sha256:1111111111111111111111111111111111111111111111111111111111111111",
        );
        let ocx_trust::SignerSpec::Key(matcher) = &trust.policy[0].signers[0] else {
            panic!("still a key signer");
        };
        assert_eq!(
            matcher.key_pem.as_deref(),
            Some(INLINE_KEY_PEM),
            "inline material survives"
        );
    }

    #[test]
    fn managed_tier_ignores_a_path_form_trusted_root() {
        // A fleet payload naming `/home/operator/sigstore/root.json` is naming
        // a path on someone else's disk. `ocx config push` inlines it, so a
        // payload that still carries the path form was not published through
        // the supported route.
        let sigstore = managed_payload_after_guard(
            "[trust.sigstore]\ntrusted_root = \"/home/operator/root.json\"\nrekor_url = \"https://rekor.corp.example\"\n",
            "ghcr.io/acme/config@sha256:1111111111111111111111111111111111111111111111111111111111111111",
        );
        assert_eq!(sigstore.trusted_root, None, "the path form is stripped");
        assert_eq!(
            sigstore.rekor_url.as_deref(),
            Some("https://rekor.corp.example"),
            "the rest of the payload still applies"
        );
    }

    #[test]
    fn managed_tier_ignores_an_inline_trust_root_behind_an_unpinned_source() {
        // Without a digest pin the trust root arrives over the channel it
        // exists to verify: whoever can move the tag can swap the CA.
        let sigstore = managed_payload_after_guard(
            "[trust.sigstore]\ntrusted_root_json = \"{}\"\n",
            "ghcr.io/acme/config:v1",
        );
        assert_eq!(sigstore.trusted_root_json, None);
    }

    /// The endpoints obey the same digest-pin rule as the trust root, and
    /// `fulcio_url` is the sharper case: it names where the OIDC identity
    /// token is sent, and `ocx package push --sbom` has no `--fulcio-url` flag
    /// to oppose a config value.
    ///
    /// Stripping the field to `None` is what makes resolution fall back to the
    /// builtin default — that an absent field yields the builtin is pinned
    /// separately, by the CLI's endpoint-precedence tests.
    #[test]
    fn managed_tier_ignores_sigstore_endpoints_behind_an_unpinned_source() {
        let sigstore = managed_payload_after_guard(
            "[trust.sigstore]\nfulcio_url = \"https://fulcio.attacker.example\"\nrekor_url = \"https://rekor.attacker.example\"\n",
            "ghcr.io/acme/config:v1",
        );
        assert_eq!(
            sigstore.fulcio_url, None,
            "an unpinned payload must not name the server the identity token is sent to"
        );
        assert_eq!(sigstore.rekor_url, None, "same rule for the transparency log");
    }

    #[test]
    fn managed_tier_honours_sigstore_endpoints_behind_a_digest_pin() {
        let sigstore = managed_payload_after_guard(
            "[trust.sigstore]\nfulcio_url = \"https://fulcio.corp.example\"\nrekor_url = \"https://rekor.corp.example\"\n",
            "ghcr.io/acme/config@sha256:1111111111111111111111111111111111111111111111111111111111111111",
        );
        assert_eq!(sigstore.fulcio_url.as_deref(), Some("https://fulcio.corp.example"));
        assert_eq!(
            sigstore.rekor_url.as_deref(),
            Some("https://rekor.corp.example"),
            "a digest-pinned seed breaks the circularity, so the fleet setting applies"
        );
    }

    #[test]
    fn managed_tier_honours_an_inline_trust_root_behind_a_digest_pin() {
        let sigstore = managed_payload_after_guard(
            "[trust.sigstore]\ntrusted_root_json = \"{}\"\n",
            "ghcr.io/acme/config@sha256:1111111111111111111111111111111111111111111111111111111111111111",
        );
        assert_eq!(
            sigstore.trusted_root_json.as_deref(),
            Some("{}"),
            "a digest-pinned seed breaks the circularity, so the payload is honoured"
        );
    }

    // ── `extra_ca_certs` / `extra_ca_certs_pem` ───────────────────────────────

    /// A real self-signed CA (the test stack's Fulcio root), so the inline form
    /// under test is material a fleet consumer could actually seed a client
    /// from — not a placeholder the guard would strip by spelling anyway.
    const EXTRA_CA_CERTS_FIXTURE_PEM: &str = include_str!("../../../test/sigstore/keys/fulcio-ca.crt.pem");

    /// The root-level `extra_ca_certs_pem` line carrying the fixture above.
    /// Literal multi-line string: no escapes, and TOML trims the newline
    /// right after the opening quotes, so the parsed value is byte-identical
    /// to the fixture.
    fn extra_ca_certs_pem_line() -> String {
        format!("extra_ca_certs_pem = '''\n{EXTRA_CA_CERTS_FIXTURE_PEM}'''\n")
    }

    /// `extra_ca_certs = <path>`, quoted by the TOML serializer so a Windows
    /// tempdir (`C:\Users\…`) does not turn into a unicode escape.
    fn extra_ca_certs_path_line(path: &Path) -> String {
        format!("extra_ca_certs = {}\n", toml::Value::from(path.display().to_string()))
    }

    fn managed_config_after_guard(payload: &str, source: &str) -> Config {
        let mut parsed: Config = toml::from_str(payload).expect("payload parses");
        let source =
            ocx_oci::OciIdentifier::parse_target(source, ocx_oci::DEFAULT_REGISTRY).expect("identifier parses");
        ConfigLoader::guard_managed_sigstore_trust(&mut parsed, &source);
        parsed
    }

    const UNPINNED_SOURCE: &str = "ghcr.io/acme/config:v1";

    /// A managed payload's path-form `extra_ca_certs` names a
    /// file on the *publisher's* disk, so the consumer drops it — behind a
    /// digest pin or not, pinning is about provenance and this key is inert
    /// either way — and the rest of the payload applies unchanged. The
    /// "publish with `ocx config push`" warning that accompanies the strip is
    /// not observable here (the crate has no log capture), exactly as for the
    /// `trusted_root` twin above; the strip is what is asserted.
    #[test]
    fn managed_tier_drops_a_path_form_extra_ca_certs_and_keeps_its_siblings() {
        for source in [PINNED_SOURCE, UNPINNED_SOURCE] {
            let config = managed_config_after_guard(
                "extra_ca_certs = \"/home/operator/corp-ca.pem\"\n\n[registry]\ndefault = \"corp.example\"\n",
                source,
            );
            assert_eq!(
                config.extra_ca_certs, None,
                "the path form is stripped from a managed payload ({source})"
            );
            assert_eq!(
                config.registry.as_ref().and_then(|r| r.default.as_deref()),
                Some("corp.example"),
                "the rest of the payload still applies ({source})"
            );
        }
    }

    /// `extra_ca_certs_pem` is honoured from a managed payload
    /// WITHOUT a digest pin — the opposite of `trusted_root_json` a few tests
    /// up, and deliberately so: a CA root never bypasses signature
    /// verification and does nothing without network position, so the
    /// pin rule that guards the Sigstore material does not apply. (The
    /// Sigstore client's own pin-gating of managed roots lives in
    /// `tls::sigstore_extra_roots`, not in this guard.) Asserted unpinned, so a later "fix" that widens the
    /// pin rule to this key reds here.
    #[test]
    fn managed_tier_keeps_extra_ca_certs_pem_from_an_unpinned_source() {
        let config = managed_config_after_guard(&extra_ca_certs_pem_line(), UNPINNED_SOURCE);
        assert_eq!(
            config.extra_ca_certs_pem.as_deref(),
            Some(EXTRA_CA_CERTS_FIXTURE_PEM),
            "inline material travels with the payload and is kept regardless of pinning"
        );
    }

    /// A relative `extra_ca_certs` resolves against the directory of
    /// the file that declares it, never the process working directory — the
    /// same `FileReference::anchored_at` rule as `trusted_root`. The tempdir
    /// is deliberately NOT the CWD, or the test would pass either way.
    #[tokio::test]
    async fn relative_extra_ca_certs_anchors_to_the_declaring_config_dir_not_the_cwd() {
        let dir = TempDir::new().expect("tempdir");
        let path = write_config(&dir, "config.toml", "extra_ca_certs = \"certs/corp-ca.pem\"\n");

        let config = ConfigLoader::load_and_merge(&[path]).await.expect("load");
        let anchored = config.extra_ca_certs.expect("extra_ca_certs");
        assert!(
            anchored.is_absolute(),
            "anchored to an absolute path: {}",
            anchored.display()
        );
        assert_eq!(anchored, dir.path().join("certs").join("corp-ca.pem"));
    }

    /// Anchoring must not rewrite what the operator fully specified.
    #[tokio::test]
    async fn absolute_extra_ca_certs_survives_loading_unchanged() {
        let dir = TempDir::new().expect("tempdir");
        let absolute = dir.path().join("elsewhere").join("corp-ca.pem");
        let path = write_config(&dir, "config.toml", &extra_ca_certs_path_line(&absolute));

        let config = ConfigLoader::load_and_merge(&[path]).await.expect("load");
        assert_eq!(config.extra_ca_certs.as_deref(), Some(absolute.as_path()));
    }

    /// The path key is an ordinary replace across tiers — the
    /// highest tier that sets it wins, and a tier that sets neither key
    /// leaves the lower tier's value standing.
    #[tokio::test]
    async fn extra_ca_certs_path_takes_the_highest_tier_that_sets_it() {
        let dir = TempDir::new().expect("tempdir");
        let low = write_config(&dir, "low.toml", "extra_ca_certs = \"low-ca.pem\"\n");
        let high = write_config(&dir, "high.toml", "extra_ca_certs = \"high-ca.pem\"\n");
        let silent = write_config(&dir, "silent.toml", "[registry]\ndefault = \"corp.example\"\n");

        let config = ConfigLoader::load_and_merge(&[low, high, silent])
            .await
            .expect("three-file merge should succeed");
        assert_eq!(
            config.extra_ca_certs.as_deref(),
            Some(dir.path().join("high-ca.pem").as_path()),
            "the highest tier that SETS the key wins; a silent tier above it changes nothing"
        );
        assert_eq!(config.extra_ca_certs_pem, None);
    }

    /// The inline key follows the same ordinary-replace rule.
    #[tokio::test]
    async fn extra_ca_certs_pem_takes_the_highest_tier_that_sets_it() {
        let dir = TempDir::new().expect("tempdir");
        let low = write_config(&dir, "low.toml", "extra_ca_certs_pem = \"low\"\n");
        let high = write_config(&dir, "high.toml", &extra_ca_certs_pem_line());
        let silent = write_config(&dir, "silent.toml", "[registry]\ndefault = \"corp.example\"\n");

        let config = ConfigLoader::load_and_merge(&[low, high, silent])
            .await
            .expect("three-file merge should succeed");
        assert_eq!(
            config.extra_ca_certs_pem.as_deref(),
            Some(EXTRA_CA_CERTS_FIXTURE_PEM),
            "the highest tier that SETS the key wins; a silent tier above it changes nothing"
        );
        assert_eq!(config.extra_ca_certs, None);
    }

    /// XOR on merge: a higher tier setting `extra_ca_certs_pem`
    /// takes BOTH keys from that tier, so a lower tier's path does not survive
    /// beside it — the merged config never carries both spellings.
    #[tokio::test]
    async fn a_higher_tier_extra_ca_certs_pem_clears_a_lower_tiers_path() {
        let dir = TempDir::new().expect("tempdir");
        let low = write_config(&dir, "low.toml", "extra_ca_certs = \"low-ca.pem\"\n");
        let high = write_config(&dir, "high.toml", &extra_ca_certs_pem_line());

        let config = ConfigLoader::load_and_merge(&[low, high]).await.expect("merge");
        assert_eq!(config.extra_ca_certs_pem.as_deref(), Some(EXTRA_CA_CERTS_FIXTURE_PEM));
        assert_eq!(
            config.extra_ca_certs, None,
            "the lower tier's path must not linger beside the inline form"
        );
    }

    /// The other direction of the XOR, or the test above would be
    /// indistinguishable from "the inline key always wins".
    #[tokio::test]
    async fn a_higher_tier_extra_ca_certs_path_clears_a_lower_tiers_pem() {
        let dir = TempDir::new().expect("tempdir");
        let low = write_config(&dir, "low.toml", &extra_ca_certs_pem_line());
        let high = write_config(&dir, "high.toml", "extra_ca_certs = \"high-ca.pem\"\n");

        let config = ConfigLoader::load_and_merge(&[low, high]).await.expect("merge");
        assert_eq!(
            config.extra_ca_certs.as_deref(),
            Some(dir.path().join("high-ca.pem").as_path())
        );
        assert_eq!(
            config.extra_ca_certs_pem, None,
            "the lower tier's inline form must not linger beside the path"
        );
    }

    /// One file declaring both spellings is ambiguous and is
    /// refused at load — `Error::AmbiguousExtraCaCerts` naming the file, exit
    /// 78. Asserted through `classify()` and, separately, the message text
    /// that tells the operator which key means what.
    #[tokio::test]
    async fn a_file_declaring_both_extra_ca_certs_keys_is_refused_as_ambiguous() {
        let dir = TempDir::new().expect("tempdir");
        let path = write_config(
            &dir,
            "config.toml",
            &format!("extra_ca_certs = \"corp-ca.pem\"\n{}", extra_ca_certs_pem_line()),
        );

        let err = ConfigLoader::load_and_merge(std::slice::from_ref(&path))
            .await
            .expect_err("both spellings in one file must be refused");
        assert!(
            matches!(&err, Error::AmbiguousExtraCaCerts { path: named } if *named == path),
            "the refusal names the ambiguous file; got {err:?}"
        );
        assert!(
            err.to_string().contains("extra_ca_certs names a local file"),
            "the refusal explains which key means what: {err}"
        );
    }

    /// The premise for the refusal above: one spelling alone loads at every
    /// file tier — including the explicit `--config` / `OCX_CONFIG` tier — or
    /// the ambiguity test could pass on a loader that refuses the keys outright.
    #[tokio::test]
    async fn either_extra_ca_certs_key_alone_loads_at_a_file_tier() {
        let dir = TempDir::new().expect("tempdir");
        let by_path = write_config(&dir, "path.toml", "extra_ca_certs = \"corp-ca.pem\"\n");
        let inline = write_config(&dir, "inline.toml", &extra_ca_certs_pem_line());

        let config = ConfigLoader::load_and_merge(&[by_path]).await.expect("path form loads");
        assert_eq!(
            config.extra_ca_certs.as_deref(),
            Some(dir.path().join("corp-ca.pem").as_path())
        );
        let config = ConfigLoader::load_and_merge(&[inline])
            .await
            .expect("inline form loads");
        assert_eq!(config.extra_ca_certs_pem.as_deref(), Some(EXTRA_CA_CERTS_FIXTURE_PEM));
    }

    /// Through the shipped fold, not a hand-rolled merge: a managed
    /// payload's `extra_ca_certs_pem` (unpinned) beats the home tier's
    /// own path, the explicit `OCX_CONFIG` overlay beats the managed tier, and
    /// the local-only view — what the managed-config fetch client is built
    /// from — never sees the managed payload's material.
    #[tokio::test]
    async fn a_managed_extra_ca_certs_pem_beats_the_home_tier_and_yields_to_the_explicit_tier() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);
        without_system_config(&env);

        std::fs::write(
            dir.path().join("config.toml"),
            "extra_ca_certs = \"home-ca.pem\"\n\n[managed]\nsource = \"registry.test/managed-config:v1\"\nrequired = \
             false\n",
        )
        .unwrap();
        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            &extra_ca_certs_pem_line(),
        );

        let loaded = ConfigLoader::load_with_local_view(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load must succeed");
        assert_eq!(
            loaded.merged.extra_ca_certs_pem.as_deref(),
            Some(EXTRA_CA_CERTS_FIXTURE_PEM),
            "the managed tier folds above the home tier"
        );
        assert_eq!(
            loaded.merged.extra_ca_certs, None,
            "XOR: the home tier's path does not survive beside the managed inline form"
        );
        assert_eq!(
            loaded.local_only.extra_ca_certs.as_deref(),
            Some(dir.path().join("home-ca.pem").as_path()),
            "the local-only view keeps the home tier's own root (D-6: the fetch client's trust set)"
        );
        assert_eq!(
            loaded.local_only.extra_ca_certs_pem, None,
            "the local-only view must never carry the managed payload's own CA"
        );

        let overlay_dir = TempDir::new().unwrap();
        let overlay = write_config(&overlay_dir, "overlay.toml", "extra_ca_certs = \"override-ca.pem\"\n");
        let loaded = ConfigLoader::load_with_local_view(ConfigInputs {
            explicit_path: Some(&overlay),
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load must succeed");
        assert_eq!(
            loaded.merged.extra_ca_certs.as_deref(),
            Some(overlay_dir.path().join("override-ca.pem").as_path()),
            "--config / OCX_CONFIG merges above the managed tier"
        );
        assert_eq!(
            loaded.merged.extra_ca_certs_pem, None,
            "XOR against the managed inline form"
        );
    }

    /// Edge case: `OCX_NO_CONFIG=1` prunes an UNLOCKED pair — the home
    /// tier's — like any ambient configuration. The discriminator for the
    /// lock (`extra_ca_certs_system_lock_beats_every_lower_tier` pins the
    /// surviving half): without this, a loader that stopped honouring the
    /// flag for the pair would pass the lock test. The flag-off premise is
    /// asserted first, or "no root" would be indistinguishable from a fixture
    /// that never set one.
    #[tokio::test]
    async fn no_config_prunes_an_unlocked_extra_ca_certs_pair() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);
        without_system_config(&env);
        std::fs::write(dir.path().join("config.toml"), "extra_ca_certs = \"home-ca.pem\"\n").unwrap();

        let load = || async {
            ConfigLoader::load_with_local_view(ConfigInputs {
                explicit_path: None,
                explicit_project_path: None,
                cwd: None,
            })
            .await
            .expect("load must succeed")
            .merged
        };

        env.remove(&ocx_env::OCX_NO_CONFIG);
        let merged = load().await;
        assert_eq!(
            merged.extra_ca_certs.as_deref(),
            Some(dir.path().join("home-ca.pem").as_path()),
            "premise: without the flag the home tier's path is loaded"
        );
        assert!(!merged.extra_ca_certs_system_locked, "premise: nothing locked it");

        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        let merged = load().await;
        assert_eq!(
            merged.extra_ca_certs, None,
            "OCX_NO_CONFIG=1 prunes the home tier's path"
        );
        assert_eq!(merged.extra_ca_certs_pem, None);
    }

    // ── the project tier can never contribute `[shell]` ─────────────────────

    /// EC-CFG-002 — a project-tier contribution carrying
    /// `[shell.consent]` contributes **nothing** — no project-sourced
    /// `shell` key. Deliberately not routed through `ProjectConfig`: this
    /// asserts the explicit strip, not the typo detector one file over.
    ///
    /// Red state: delete the `take()` in
    /// [`ConfigLoader::fold_project_tier`] and `merged.shell` becomes `Some`,
    /// carrying a grant a clone wrote for itself.
    #[test]
    fn c033_a_project_tier_fold_cannot_contribute_shell_consent() {
        let mut merged: Config = toml::from_str("[registry]\ndefault = \"ghcr.io\"\n").expect("base parses");
        let project: Config = toml::from_str(
            "[shell.consent]\npaths = [\"/home/u/clone\"]\nnamespaces = \"ocx.sh/acme\"\n\n[registry]\ndefault = \
             \"ocx.sh\"\n",
        )
        .expect("a project-tier contribution parses as a Config");
        assert!(
            project.shell.is_some(),
            "the fixture must actually carry [shell], or the strip is untested"
        );

        ConfigLoader::fold_project_tier(&mut merged, project);

        assert!(
            merged.shell.is_none(),
            "a project tier must never contribute [shell] — consent read from a repo's own file lets a clone consent \
             to itself"
        );
        assert_eq!(
            merged.resolved_default_registry(),
            Some("ocx.sh"),
            "everything a project tier IS allowed to set must still merge"
        );
    }

    /// A repository must not switch on unattended binary replacement for whoever clones it.
    ///
    /// Red state: delete the `take()` for `update` in [`ConfigLoader::fold_project_tier`].
    #[test]
    fn a_project_tier_fold_cannot_contribute_update() {
        let mut merged: Config = toml::from_str("[update]\nself = \"manual\"\n").expect("base parses");
        let project: Config = toml::from_str("[update]\nself = \"apply\"\n[registry]\ndefault = \"ocx.sh\"\n")
            .expect("a project-tier contribution parses as a Config");
        assert!(project.update.is_some(), "the fixture must carry [update]");

        ConfigLoader::fold_project_tier(&mut merged, project);

        assert_eq!(
            merged.update.as_ref().and_then(|update| update.self_policy.clone()),
            Some(Ok(crate::refresh::RefreshPolicy::Manual))
        );
        assert_eq!(merged.resolved_default_registry(), Some("ocx.sh"));
    }

    /// The `[records]` twin of the strip above, and the same class of defect: a
    /// repository that could contribute `[records]` could redirect an audit
    /// trail into a directory it also controls, or point a `required` posture at
    /// an unwritable one and refuse every launch inside the checkout. Where
    /// records go is the operator's call, and a clone is untrusted input.
    ///
    /// Red state: delete the `take()` for `records` in
    /// [`ConfigLoader::fold_project_tier`] and `merged.records` carries the
    /// repository's sink.
    #[test]
    fn a_project_tier_fold_cannot_contribute_records() {
        let mut merged: Config = toml::from_str("[records]\ndir = \"/var/log/ocx-records\"\n").expect("base parses");
        let project: Config = toml::from_str(
            "[records]\ndir = \"./.ocx/records\"\nrequired = true\n\n[registry]\ndefault = \"ocx.sh\"\n",
        )
        .expect("a project-tier contribution parses as a Config");
        assert!(
            project.records.is_some(),
            "the fixture must actually carry [records], or the strip is untested"
        );

        ConfigLoader::fold_project_tier(&mut merged, project);

        assert_eq!(
            merged
                .records
                .as_ref()
                .expect("the operator's own section survives")
                .dir,
            Some(std::path::PathBuf::from("/var/log/ocx-records")),
            "a project tier must never redirect the execution-record sink"
        );
        assert_eq!(
            merged.resolved_default_registry(),
            Some("ocx.sh"),
            "everything a project tier IS allowed to set must still merge"
        );
    }

    // ── the managed tier's `[shell]` ────────────────────────────────────────

    fn managed_shell_after_guard(payload: &str, source: &str) -> Option<crate::ShellConfig> {
        let mut parsed: Config = toml::from_str(payload).expect("payload parses");
        let source =
            ocx_oci::OciIdentifier::parse_target(source, ocx_oci::DEFAULT_REGISTRY).expect("identifier parses");
        ConfigLoader::guard_managed_shell_consent(&mut parsed, &source);
        parsed.shell
    }

    const PINNED_SOURCE: &str =
        "ghcr.io/acme/config@sha256:1111111111111111111111111111111111111111111111111111111111111111";

    /// EC-CFG-003(a) — the red half: an unpinned `[managed]
    /// source` cannot ship an activation grant. This is the only thing between
    /// an unpinned managed payload and a PATH-front activation on every host in
    /// a fleet.
    ///
    /// Red state: remove the `source.digest().is_some()` early return in
    /// [`ConfigLoader::guard_managed_shell_consent`] and `consent` survives.
    #[test]
    fn c034_managed_shell_consent_is_stripped_behind_an_unpinned_source() {
        let shell = managed_shell_after_guard(
            "[shell]\nhook = true\n\n[shell.consent]\nnamespaces = \"ocx.sh/acme\"\n",
            "ghcr.io/acme/config:v1",
        )
        .expect("[shell] survives — only the consent half is gated");

        assert!(
            shell.consent.is_none(),
            "an unpinned managed payload must not carry an activation grant"
        );
        assert_eq!(
            shell.hook,
            Some(true),
            "hook merges unconditionally in both directions — it grants nothing, and consent still gates every project"
        );
        let reason = shell
            .consent_strip_reason
            .expect("the reason must be recorded, not only logged to a stderr the shims discard");
        assert!(
            reason.contains("digest-pinned"),
            "the recorded reason must name the cause so a rerun is actionable, got: {reason}"
        );
    }

    /// EC-CFG-003(b) — the green half: a digest-pinned source
    /// breaks the circularity, so the same payload is honoured — and nothing is
    /// reported as stripped.
    #[test]
    fn c034_managed_shell_consent_is_honoured_behind_a_digest_pin() {
        let shell = managed_shell_after_guard("[shell.consent]\nnamespaces = \"ocx.sh/acme\"\n", PINNED_SOURCE)
            .expect("[shell] present");
        let consent = shell.consent.expect("a pinned payload keeps its consent table");
        assert!(consent.namespaces.expect("namespaces").matches("ocx.sh/acme"));
        assert!(
            shell.consent_strip_reason.is_none(),
            "nothing was stripped, so nothing may be reported as stripped"
        );
    }

    /// The polarity the two tests above cannot see, because
    /// both carry a pure **grant**. `exclude` is the only key that takes a
    /// grant away and it accumulates across tiers, so stripping it leaves an
    /// `include` contributed by another tier (here `OCX_CONSENT_NAMESPACES`,
    /// via the same `ShellConsent::merge` `effective_consent` performs)
    /// standing unopposed — a **widening**, which is the one direction the
    /// managed-tier strip exists to forbid. The honest operator who wrote the
    /// carve-out must get it.
    ///
    /// Red state, both halves: (a) `take()` the whole table in
    /// [`ConfigLoader::guard_managed_shell_consent`] and the carve-out is gone,
    /// so `ocx.sh/bad` activates; (b) keep the `exclude` under an **empty**
    /// `include` and `ScopeSpec::Set` reads it as a catch-all, so the
    /// standalone assertions below red on a source nobody ever granted.
    #[test]
    fn c034_an_unpinned_managed_payload_keeps_its_namespaces_carve_out() {
        let shell = managed_shell_after_guard(
            concat!(
                "[shell.consent]\n",
                "paths = [\"/srv/fleet\"]\n",
                "namespaces = { include = [\"ocx.sh/acme\"], exclude = [\"ocx.sh/bad\"] }\n",
            ),
            "ghcr.io/acme/config:v1",
        )
        .expect("[shell] survives — only the grant half is gated");

        let consent = shell.consent.clone().expect("the withdrawal survives the strip");
        assert!(
            consent.paths.is_empty(),
            "`paths` grants unconditionally and must not survive an unpinned source"
        );
        let namespaces = consent.namespaces.as_ref().expect("the carve-out survives as a spec");
        assert_eq!(
            namespaces.exclude(),
            ["ocx.sh/bad"],
            "the carve-out must survive verbatim — it is the only key that can take a grant away"
        );
        assert!(
            !namespaces.matches("ocx.sh/acme"),
            "the payload's own `include` is a grant and must not survive the strip"
        );
        assert!(
            !namespaces.matches("ocx.sh/nobody-granted-this"),
            "what survives must grant NOTHING on its own — `ScopeSpec::Set` reads an empty `include` as a catch-all"
        );

        // The reachable attack shape: an include contributed by another channel
        // after the config tiers, exactly as `effective_consent` folds it.
        let mut effective = consent;
        effective.merge(crate::shell::env_channel(None, Some("ocx.sh/acme,ocx.sh/bad")));
        let namespaces = effective.namespaces.expect("the env channel contributes a spec");
        assert!(
            !namespaces.matches("ocx.sh/bad"),
            "an exclude beats an include contributed by another tier — dropping it would widen"
        );
        assert!(
            namespaces.matches("ocx.sh/acme"),
            "positive control: the other tier's grant still stands, so the assertion above is not passing vacuously"
        );

        let reason = shell
            .consent_strip_reason
            .expect("the reason must be recorded, not only logged to a stderr the shims discard");
        assert!(
            reason.contains("digest-pinned"),
            "the recorded reason must name the cause so a rerun is actionable, got: {reason}"
        );
        assert!(
            reason.contains("ocx.sh/bad"),
            "`ocx about` and the reconciler print this reason; it must say WHAT was kept, got: {reason}"
        );
    }

    // ── a refused consent table is dropped, not fatal ───────────────────────

    /// The payload every test below shares: a refused `[shell.consent]` grant
    /// sitting beside the three sections a fleet actually depends on.
    const REFUSED_CONSENT_PAYLOAD: &str = concat!(
        "[registries.\"ocx.sh\"]\nindex = \"https://index.corp.example\"\n",
        "[mirrors]\n\"ghcr.io\" = \"https://mirror.corp.example\"\n",
        "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n",
        "signers = [{ kind = \"keyless\", identity = \"ci@acme.example\", oidc_issuer = \"https://iss.example\" }]\n",
        "[shell]\nhook = true\n",
        "[shell.consent]\nnamespaces = \"ocx.sh/*\"\n",
    );

    /// The refusal `REFUSED_CONSENT_PAYLOAD` must be reported by — derived from
    /// the variant, so rewording the message cannot silently weaken the test.
    fn whole_registry_refusal() -> String {
        crate::shell::ConsentPatternError::WholeRegistry("ocx.sh/*".to_string()).to_string()
    }

    /// Every assertion the strip owes, in one place: no consent, the reason
    /// recorded and naming the class, and every sibling section intact.
    fn assert_consent_stripped_and_siblings_survive(config: &Config) {
        let shell = config
            .shell
            .as_ref()
            .expect("[shell] survives — only the consent half is dropped");
        assert!(
            shell.consent.is_none(),
            "a refused consent table must grant NOTHING; the strip fails closed"
        );
        let reason = shell
            .consent_strip_reason
            .as_deref()
            .expect("the reason must be recorded, not only logged to a stderr the shims discard");
        assert!(
            reason.contains(&whole_registry_refusal()),
            "the recorded reason must name the refusal that caused it, got: {reason}"
        );
        assert_eq!(
            shell.hook,
            Some(true),
            "only the `consent` key is removed — the rest of [shell] is untouched"
        );
        assert_eq!(
            config
                .registries
                .as_ref()
                .and_then(|registries| registries.get("ocx.sh"))
                .and_then(|entry| entry.index.as_deref()),
            Some("https://index.corp.example"),
            "[registries] must survive a refused consent table"
        );
        assert!(
            config
                .mirrors
                .as_ref()
                .is_some_and(|mirrors| mirrors.contains_key("ghcr.io")),
            "[mirrors] must survive a refused consent table"
        );
        assert_eq!(
            config.trust.as_ref().map(|trust| trust.policy.len()),
            Some(1),
            "[[trust.policy]] must survive a refused consent table — dropping an operator's trust pins is the \
             widening this whole strip exists to prevent"
        );
    }

    /// `arch-principles.md` fleet forward-compat: a refused
    /// `[shell.consent]` grant in a DISCOVERED tier drops the grant and nothing
    /// else. Before the strip this was `Error::Parse`, so every `ocx`
    /// invocation on the host exited on a file that is otherwise fine.
    ///
    /// Red state: replace the body of
    /// [`ConfigLoader::parse_config_stripping_refused_consent`] with
    /// `toml::from_str::<Config>(text)` and `load_and_merge` returns
    /// `Error::Parse` instead — `expect` below fails.
    #[tokio::test]
    async fn c344_a_refused_consent_table_is_dropped_and_the_discovered_tier_still_loads() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "config.toml", REFUSED_CONSENT_PAYLOAD);

        let config = ConfigLoader::load_and_merge(std::slice::from_ref(&path))
            .await
            .expect("a refused consent grant must not take the whole tier down with it");

        assert_consent_stripped_and_siblings_survive(&config);
    }

    /// The discriminator: the strip is narrow. A payload broken for any reason
    /// OTHER than its consent table still fails the file, even when a refused
    /// consent table is sitting right next to the real error — so "drop the
    /// consent half" can never decay into "swallow anything".
    ///
    /// Red state: return the second-pass result unconditionally in
    /// [`ConfigLoader::parse_config_stripping_refused_consent`] instead of
    /// falling back to `refusal`, and this stops erroring.
    #[tokio::test]
    async fn c344_the_strip_does_not_rescue_a_file_broken_anywhere_else() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "config.toml",
            "[registry]\ndefault = 12\n[shell.consent]\nnamespaces = \"ocx.sh/*\"\n",
        );

        let error = ConfigLoader::load_and_merge(std::slice::from_ref(&path))
            .await
            .expect_err("a type error outside [shell.consent] must still fail the file");
        let rendered = error.to_string();
        assert!(
            rendered.contains("config.toml"),
            "the surviving error must be the ORIGINAL one, path and all, got: {rendered}"
        );
    }

    /// One fixture, one variable: `exclude_line` is the ONLY difference
    /// between the payloads below, so nothing but the withdrawal can explain a
    /// difference in outcome.
    ///
    /// The refusal is an unknown key **inside** the namespaces table, which is
    /// the case `arch-principles.md`'s consent carve-out was written for — an
    /// operator publishes a narrowing an older fleet host cannot read.
    fn refused_narrowing_payload(exclude_line: &str) -> String {
        format!(
            "[registries.\"ocx.sh\"]\nindex = \"https://index.corp.example\"\n\
             [shell]\nhook = true\n\
             [shell.consent.namespaces]\n\
             include = [\"ocx.sh/acme\", \"ocx.sh/tools\"]\n\
             {exclude_line}\
             require_signed = [\"x\"]\n"
        )
    }

    async fn load_one_config(payload: &str) -> Result<Config> {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "config.toml", payload);
        ConfigLoader::load_and_merge(std::slice::from_ref(&path)).await
    }

    /// `arch-principles.md`'s consent carve-out: the strip
    /// drops a **grant**, never a **withdrawal**.
    ///
    /// `exclude` is the only thing a `[shell.consent]` table says that TAKES a
    /// grant away, and [`ShellConsent::merge`](crate::shell::ShellConsent::merge)
    /// accumulates it across tiers against a `covered && !excluded` predicate.
    /// So stripping a table that carries one leaves whatever `include` another
    /// tier contributed standing unopposed: an operator's fleet-wide narrowing
    /// ("withdraw the compromised org") would become a *grant* on every host
    /// too old to read it, and the attacker is whoever holds the **withdrawn**
    /// org's credential. That is widening, the one direction the carve-out
    /// forbids, so such a file keeps the hard failure it had before the strip
    /// existed.
    ///
    /// Both polarities, because either alone is half a proof: the granting
    /// payloads are the positive control that stops the fix from degenerating
    /// into "never strip".
    ///
    /// Red state: delete the `Self::consent_table_withdraws` early return in
    /// [`ConfigLoader::parse_config_stripping_refused_consent`] and the
    /// withdrawing payload starts loading with its `exclude` silently gone.
    #[tokio::test]
    async fn c344_the_strip_drops_a_grant_but_never_a_withdrawal() {
        let error = load_one_config(&refused_narrowing_payload("exclude = [\"ocx.sh/tools\"]\n"))
            .await
            .expect_err(
                "a refused table carrying a withdrawal must keep failing the file; dropping it would leave another \
                 tier's include standing unopposed",
            );
        // The whole `source()` chain, the way the CLI renders it with `{err:#}`:
        // the outer variant names only the path, and asserting on that alone
        // would be satisfied by a typo in this fixture's inline TOML.
        let mut rendered = error.to_string();
        let mut cause = std::error::Error::source(&error);
        while let Some(current) = cause {
            rendered.push_str(&format!(": {current}"));
            cause = current.source();
        }
        assert!(
            rendered.contains("require_signed"),
            "the surviving error must be the ORIGINAL refusal, not some other parse failure in this fixture, got: \
             {rendered}"
        );

        for (label, exclude_line) in [
            ("no exclude at all", ""),
            ("an empty exclude, which withdraws nothing", "exclude = []\n"),
        ] {
            let config = match load_one_config(&refused_narrowing_payload(exclude_line)).await {
                Ok(config) => config,
                Err(error) => {
                    panic!("a refused table with {label} must still be stripped so the file survives: {error}")
                }
            };
            let shell = config
                .shell
                .as_ref()
                .expect("[shell] survives — only the consent half is dropped");
            assert!(
                shell.consent.is_none(),
                "a refused consent table with {label} must grant NOTHING; the strip fails closed"
            );
            assert!(
                shell.consent_strip_reason.is_some(),
                "the strip must record its reason for {label}, not only log it to a stderr the shims discard"
            );
            assert_eq!(
                shell.hook,
                Some(true),
                "only the `consent` key is removed — the rest of [shell] is untouched ({label})"
            );
            assert!(
                config
                    .registries
                    .as_ref()
                    .is_some_and(|registries| registries.contains_key("ocx.sh")),
                "[registries] must survive the strip ({label}) — rescuing the sibling sections is the point of it"
            );
        }
    }

    /// One fixture, one variable: the `namespaces = …` line is the ONLY
    /// difference between the three arms below, so nothing but that value can
    /// explain a difference in outcome.
    fn consent_namespaces_payload(namespaces_line: &str) -> String {
        format!(
            "[registries.\"ocx.sh\"]\nindex = \"https://index.corp.example\"\n\
             [shell]\nhook = true\n\
             [shell.consent]\n{namespaces_line}\n"
        )
    }

    /// Arm 1, the positive control: a genuine **refusal** — the whole-registry
    /// spelling — still strips. Without it the fix below could degenerate into
    /// "never strip" and every arm would pass.
    ///
    /// Red state: make [`ConfigLoader::consent_table_shape_is_readable`] return
    /// `false` unconditionally and this stops loading.
    #[tokio::test]
    async fn c344_a_refused_pattern_still_strips() {
        let config = load_one_config(&consent_namespaces_payload("namespaces = \"ocx.sh/*\""))
            .await
            .expect("a refused pattern is a judgement about consent, not a broken file");
        let shell = config.shell.as_ref().expect("[shell] survives the strip");
        assert!(
            shell.consent.is_none(),
            "the refused grant must be gone — the strip fails closed"
        );
        assert!(
            shell
                .consent_strip_reason
                .as_ref()
                .is_some_and(|reason| reason.contains(&whole_registry_refusal())),
            "the recorded reason must name the refusal class, got: {:?}",
            shell.consent_strip_reason
        );
    }

    /// Arm 2: an ordinary **type error** inside
    /// `[shell.consent]` is the operator's own typo and keeps exit 78. Removing
    /// the table makes this file parse exactly as it does for arm 1, so the
    /// structural test alone cannot tell the two apart and swallowed this one
    /// behind a warning on a stderr the shims discard.
    ///
    /// Red state: delete the `Self::consent_table_shape_is_readable` early
    /// return in [`ConfigLoader::parse_config_stripping_refused_consent`] and
    /// this payload starts loading successfully.
    #[tokio::test]
    async fn c344_a_plain_type_error_in_the_consent_table_is_not_stripped() {
        let error = load_one_config(&consent_namespaces_payload("namespaces = 123"))
            .await
            .expect_err("an ill-typed consent value is a config error, not a refused grant");
        let mut rendered = error.to_string();
        let mut cause = std::error::Error::source(&error);
        while let Some(current) = cause {
            rendered.push_str(&format!(": {current}"));
            cause = current.source();
        }
        assert!(
            rendered.contains("invalid type"),
            "the surviving error must be the ORIGINAL type error, spans and all, got: {rendered}"
        );
    }

    /// Arm 3, the regression check on the guard that landed before this one:
    /// a refused table carrying a **withdrawal** still fails the file, in the
    /// inline spelling too — dotted, sectioned and inline all normalize to the
    /// same nested table, so `consent_table_withdraws` must catch all three.
    ///
    /// Red state: delete the `Self::consent_table_withdraws` early return in
    /// [`ConfigLoader::parse_config_stripping_refused_consent`] and this
    /// payload starts loading with its `exclude` silently gone.
    #[tokio::test]
    async fn c344_a_withdrawing_inline_table_is_still_not_stripped() {
        let error = load_one_config(&consent_namespaces_payload(
            "namespaces = { include = [\"ocx.sh/acme\"], exclude = [\"ocx.sh/tools\"], require_signed = [\"x\"] }",
        ))
        .await
        .expect_err("dropping a withdrawal widens; such a file keeps the hard failure");
        let mut rendered = error.to_string();
        let mut cause = std::error::Error::source(&error);
        while let Some(current) = cause {
            rendered.push_str(&format!(": {current}"));
            cause = current.source();
        }
        assert!(
            rendered.contains("require_signed"),
            "the surviving error must be the ORIGINAL refusal, not some other failure in this fixture, got: {rendered}"
        );
    }

    /// The fleet half, end to end: an identity-matching managed payload whose
    /// only fault is a refused consent grant folds everything else and reports
    /// [`ManagedSnapshotState::Applied`](crate::managed::ManagedSnapshotState::Applied).
    ///
    /// This is the block-tier case. Before the strip the payload became
    /// `PayloadUnusable`, so with `required = false` an operator's `[mirrors]`
    /// and `[[trust.policy]]` vanished fleet-wide — resolution silently falling
    /// back to the default registry with no trust pins — and with
    /// `required = true` every command on every host failed.
    ///
    /// Red state: same mutation as the discovered-tier test; the fold then
    /// reports `PayloadUnusable` and folds nothing.
    #[tokio::test]
    async fn c344_a_refused_consent_table_in_a_managed_payload_folds_everything_else() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        // Digest-pinned, so `guard_managed_shell_consent` is not what removes
        // the table — the refusal strip is.
        let source = format!("corp.example.com/ocx-config@sha256:{}", "a".repeat(64));
        write_managed_snapshot(dir.path(), &source, REFUSED_CONSENT_PAYLOAD);

        let accumulator = crate::Config {
            managed: Some(crate::managed::ManagedConfig {
                source: Some(source),
                ..Default::default()
            }),
            ..crate::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved, state, _) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
            .await
            .expect("fold must succeed");

        assert_eq!(
            state,
            crate::managed::ManagedSnapshotState::Applied,
            "a payload whose only fault is a refused consent grant is usable; PayloadUnusable means BROKEN"
        );
        assert_consent_stripped_and_siblings_survive(&folded);
    }

    /// The digest gate is managed-tier-only. An explicit `--config` /
    /// `OCX_CONFIG` file has no `[managed] source` for the pin question to be
    /// asked of, and is a third consent-bearing channel of the same
    /// already-out-of-scope threat class.
    ///
    /// Red state: call the gate on the overlay and this grant disappears.
    #[test]
    fn a33_the_digest_gate_does_not_apply_to_the_explicit_tier() {
        let mut merged: Config = Config::default();
        let overlay: Config =
            toml::from_str("[shell.consent]\npaths = [\"/home/u/project\"]\n").expect("overlay parses");
        merged.merge(overlay);
        assert_eq!(
            merged
                .shell
                .expect("shell")
                .consent
                .expect("consent")
                .paths
                .first()
                .map(PathBuf::as_path),
            Some(Path::new("/home/u/project")),
            "an explicit-tier grant is never gated on a [managed] pin"
        );
    }

    /// EC-CFG-006 — `--config` / `OCX_CONFIG` outranks the managed tier —
    /// including a digest-pinned one — because the loader folds the managed
    /// tier first and the overlay on top of it.
    ///
    /// Red state: fold the overlay above the managed tier in
    /// [`ConfigLoader::load_with_local_view`] and both assertions flip.
    #[test]
    fn a32_the_explicit_tier_outranks_the_managed_tier_and_records_that() {
        use crate::ConfigTier;

        // The shipped order, reproduced: base (discovered) -> managed -> overlay.
        let mut base: Config = toml::from_str("[shell]\nhook = false\n").expect("base parses");
        ConfigLoader::stamp_shell_tier(&mut base, ConfigTier::Home);

        let mut managed: Config = toml::from_str("[shell]\nhook = true\n").expect("managed parses");
        ConfigLoader::guard_managed_shell_consent(
            &mut managed,
            &ocx_oci::OciIdentifier::parse_target(PINNED_SOURCE, ocx_oci::DEFAULT_REGISTRY).expect("identifier"),
        );
        ConfigLoader::stamp_shell_tier(&mut managed, ConfigTier::Managed);
        base.merge(managed);
        assert_eq!(
            base.shell.as_ref().and_then(|shell| shell.hook),
            Some(true),
            "a pinned managed tier beats every DISCOVERED tier"
        );

        let mut overlay: Config = toml::from_str("[shell]\nhook = false\n").expect("overlay parses");
        ConfigLoader::stamp_shell_tier(&mut overlay, ConfigTier::Explicit);
        base.merge(overlay);

        let shell = base.shell.expect("shell");
        assert_eq!(
            shell.hook,
            Some(false),
            "the explicit tier still merges on top and wins"
        );
        assert_eq!(
            shell.hook_tier,
            Some(ConfigTier::Explicit),
            "the recorded provenance names the tier that ACTUALLY decided, never a hard-coded 'managed'"
        );
    }

    /// The consent a full load actually grants: the `config.toml` tiers as the
    /// loader folded them, plus the `OCX_CONSENT_*` env channel.
    async fn consent_after_load() -> crate::shell::ShellConsent {
        let loaded = ConfigLoader::load_with_local_view(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load must succeed");
        crate::shell::effective_consent(loaded.merged.shell.as_ref())
    }

    /// EC-CFG-005 — a managed `[shell] hook = true` beats the home tier's
    /// own explicit `false` — asserted through the **shipped fold**, not a
    /// hand-rolled merge order. `hook` grants nothing, so it merges
    /// unconditionally in both directions; that is only safe because
    /// `[shell.consent]` still gates every project independently.
    ///
    /// EC-CFG-006 rides along, because only a full load can carry it: the
    /// sibling unit test hand-rolls the tier order, so its stated red state —
    /// folding the overlay above the managed tier in
    /// [`ConfigLoader::load_with_local_view`] — is unreachable there and the
    /// green is indistinguishable from never having exercised the loader.
    ///
    /// Red state: fold the payload underneath in
    /// [`ConfigLoader::fold_managed_tier`] (`parsed.merge(accumulator)` in place
    /// of `accumulator.merge(parsed)`) and the managed assertions drop to the
    /// home tier's `false`; drop the post-fold `merged.merge(overlay)` and the
    /// `OCX_CONFIG` assertions keep reporting the managed tier.
    #[tokio::test]
    async fn c034_ec_cfg_005_a_managed_hook_beats_the_home_tiers_own_false() {
        use crate::ConfigTier;

        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        // Pinned to the digest `write_managed_snapshot` stamps, so
        // `snapshot_matches_source`'s digest clause is satisfied and the
        // *pinned* half of the managed `[shell]` contract is what runs.
        let source = format!("registry.test/managed-config@sha256:{}", "a".repeat(64));
        std::fs::write(
            dir.path().join("config.toml"),
            format!("[shell]\nhook = false\n\n[managed]\nsource = \"{source}\"\nrequired = false\n"),
        )
        .unwrap();
        write_managed_snapshot(dir.path(), &source, "[shell]\nhook = true\n");

        let loaded = ConfigLoader::load_with_local_view(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load must succeed");

        assert_eq!(
            loaded.local_only.shell.as_ref().and_then(|shell| shell.hook),
            Some(false),
            "the fixture must actually carry the home tier's `false`, or the precedence below is untested"
        );
        let shell = loaded.merged.shell.expect("the managed payload contributes [shell]");
        assert_eq!(
            shell.hook,
            Some(true),
            "the managed tier beats a user's own explicit `hook = false` — the direction the fleet-off rationale does \
             not cover"
        );
        assert_eq!(
            shell.hook_tier,
            Some(ConfigTier::Managed),
            "the recorded provenance must name the tier that actually decided the rung"
        );

        // EC-CFG-006: the one tier the managed fold does NOT beat, through the
        // same load — `OCX_CONFIG` merges after it, and the user chose the file.
        let explicit_dir = TempDir::new().unwrap();
        let explicit = write_config(&explicit_dir, "chosen.toml", "[shell]\nhook = false\n");
        env.set(&ocx_env::OCX_CONFIG, explicit.to_str().unwrap());
        let loaded = ConfigLoader::load_with_local_view(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load must succeed");
        let shell = loaded.merged.shell.expect("shell");
        assert_eq!(
            shell.hook,
            Some(false),
            "OCX_CONFIG merges above the managed fold, so `:326`'s \"beats every file a user can edit\" is false for \
             the explicit tiers"
        );
        assert_eq!(
            shell.hook_tier,
            Some(ConfigTier::Explicit),
            "`ocx shell state` must name the deciding tier, never assert \"managed\""
        );
    }

    /// `LoadedConfig::extra_ca_certs_tier` is the loader's
    /// own record of which tier's key survived the fold, and
    /// `extra_ca_certs_tier_local` the same for the local-only view — TRUE
    /// provenance, never a guess from the value. One `$OCX_HOME`, four
    /// loads through the shipped fold: the home tier alone (the system tier
    /// would lock — `extra_ca_certs_system_lock_beats_every_lower_tier`
    /// owns that) → `Home` in both views; a managed payload setting the key →
    /// `Managed` in the merged view while the local view still names the
    /// discovered tier; an `OCX_CONFIG` overlay on top → `Explicit` in both;
    /// and a system `_pem` under a managed payload whose only key
    /// is the PATH form → `System`: that path is dropped before the record
    /// is taken, so a dropped key never stamps `Managed` over the tier whose
    /// root actually resolves.
    ///
    /// Red states: record the tier unconditionally in
    /// `load_and_merge_recording` (a silent tier stamps itself over the
    /// home tier → the first load reds); return `None` from
    /// `fold_managed_tier` (the second load's merged tier reds); drop the
    /// overlay's `.or(..)` precedence (the third reds); take the managed
    /// record before `guard_managed_sigstore_trust` (the fourth reds).
    #[tokio::test]
    async fn extra_ca_certs_tier_records_the_tier_that_actually_set_the_key() {
        use crate::ConfigTier;

        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);
        without_system_config(&env);
        let load = || async {
            ConfigLoader::load_with_local_view(ConfigInputs {
                explicit_path: None,
                explicit_project_path: None,
                cwd: None,
            })
            .await
            .expect("load must succeed")
        };

        // Home tier alone.
        std::fs::write(dir.path().join("config.toml"), extra_ca_certs_pem_line()).unwrap();
        let loaded = load().await;
        assert_eq!(
            loaded.merged.extra_ca_certs_pem.as_deref(),
            Some(EXTRA_CA_CERTS_FIXTURE_PEM)
        );
        assert_eq!(loaded.extra_ca_certs_tier, Some(ConfigTier::Home));
        assert_eq!(loaded.extra_ca_certs_tier_local, Some(ConfigTier::Home));

        // An unpinned managed payload sets the key: merged names it, local
        // still names the tier the discovered chain folded.
        let source = "registry.test/managed-config:stable";
        std::fs::write(
            dir.path().join("config.toml"),
            format!(
                "{}[managed]\nsource = \"{source}\"\nrequired = false\n",
                extra_ca_certs_pem_line()
            ),
        )
        .unwrap();
        write_managed_snapshot(dir.path(), source, "extra_ca_certs_pem = \"managed\"\n");
        let loaded = load().await;
        assert_eq!(loaded.merged.extra_ca_certs_pem.as_deref(), Some("managed"));
        assert_eq!(loaded.extra_ca_certs_tier, Some(ConfigTier::Managed));
        assert_eq!(
            loaded.local_only.extra_ca_certs_pem.as_deref(),
            Some(EXTRA_CA_CERTS_FIXTURE_PEM),
            "the local view never carries the payload's key"
        );
        assert_eq!(loaded.extra_ca_certs_tier_local, Some(ConfigTier::Home));

        // `OCX_CONFIG` outranks the managed fold, in both views.
        let explicit = write_config(&dir, "chosen.toml", "extra_ca_certs_pem = \"explicit\"\n");
        env.set(&ocx_env::OCX_CONFIG, explicit.to_str().unwrap());
        let loaded = load().await;
        assert_eq!(loaded.merged.extra_ca_certs_pem.as_deref(), Some("explicit"));
        assert_eq!(loaded.extra_ca_certs_tier, Some(ConfigTier::Explicit));
        assert_eq!(loaded.extra_ca_certs_tier_local, Some(ConfigTier::Explicit));

        // A managed payload whose only key is the path form sets
        // nothing once that path is dropped — the record stays with the system
        // tier, whose `_pem` is what resolves. The home tier is silent so the
        // system lock is not what decides this load.
        env.remove(&ocx_env::OCX_CONFIG);
        let system = write_config(&dir, "system.toml", &extra_ca_certs_pem_line());
        env.set(&ocx_env::__OCX_TESTING_SYSTEM_CONFIG, system.to_str().unwrap());
        std::fs::write(
            dir.path().join("config.toml"),
            format!("[managed]\nsource = \"{source}\"\nrequired = false\n"),
        )
        .unwrap();
        write_managed_snapshot(dir.path(), source, "extra_ca_certs = \"/publisher/corp-ca.pem\"\n");
        let loaded = load().await;
        assert_eq!(
            loaded.merged.extra_ca_certs_pem.as_deref(),
            Some(EXTRA_CA_CERTS_FIXTURE_PEM)
        );
        assert_eq!(
            loaded.merged.extra_ca_certs, None,
            "the path form never survives a payload"
        );
        assert_eq!(
            loaded.extra_ca_certs_tier,
            Some(ConfigTier::System),
            "a dropped path form must not record the managed tier as the provenance"
        );
        assert_eq!(loaded.extra_ca_certs_tier_local, Some(ConfigTier::System));
    }

    /// The system tier's pair is locked — it beats the home tier
    /// (switching to the PATH form, so a merge that honoured it would clear
    /// the system `_pem` rather than only replace it), an unpinned managed
    /// payload and `OCX_CONFIG`, in both views, with the tier recorded as
    /// `System` throughout; and it survives `OCX_NO_CONFIG=1`, which prunes
    /// ambient configuration, not operator policy. The lock flag itself
    /// reaches both views, which is what `config::tls::resolve_extra_roots` reads to
    /// ignore `OCX_EXTRA_CA_CERTS`.
    ///
    /// Red states: drop the `Config::merge` guard → the merged `_pem` reads
    /// "explicit"; drop the `retain_system_locked_sections` guard → the
    /// `OCX_NO_CONFIG` load's `_pem` is `None`.
    #[tokio::test]
    async fn extra_ca_certs_system_lock_beats_every_lower_tier() {
        use crate::ConfigTier;

        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);
        with_system_config(&env, &dir, &extra_ca_certs_pem_line());
        let source = "registry.test/managed-config:stable";
        std::fs::write(
            dir.path().join("config.toml"),
            format!("extra_ca_certs = \"home-ca.pem\"\n[managed]\nsource = \"{source}\"\nrequired = false\n"),
        )
        .unwrap();
        write_managed_snapshot(dir.path(), source, "extra_ca_certs_pem = \"managed\"\n");
        let explicit = write_config(&dir, "chosen.toml", "extra_ca_certs_pem = \"explicit\"\n");
        env.set(&ocx_env::OCX_CONFIG, explicit.to_str().unwrap());
        let load = || async {
            ConfigLoader::load_with_local_view(ConfigInputs {
                explicit_path: None,
                explicit_project_path: None,
                cwd: None,
            })
            .await
            .expect("load must succeed")
        };

        let loaded = load().await;
        for (view, config) in [("merged", &loaded.merged), ("local_only", &loaded.local_only)] {
            assert!(
                config.extra_ca_certs_system_locked,
                "{view}: the lock must reach the view"
            );
            assert_eq!(
                config.extra_ca_certs_pem.as_deref(),
                Some(EXTRA_CA_CERTS_FIXTURE_PEM),
                "{view}: the system root must be the one that resolves"
            );
            assert_eq!(
                config.extra_ca_certs, None,
                "{view}: the home tier's path form must not clear the locked pair"
            );
        }
        assert_eq!(loaded.extra_ca_certs_tier, Some(ConfigTier::System));
        assert_eq!(loaded.extra_ca_certs_tier_local, Some(ConfigTier::System));

        // The flag prunes the home tier and the snapshot; the locked pair and
        // its record stay, and `OCX_CONFIG` still cannot outbid them.
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        let loaded = load().await;
        assert!(loaded.merged.extra_ca_certs_system_locked);
        assert_eq!(
            loaded.merged.extra_ca_certs_pem.as_deref(),
            Some(EXTRA_CA_CERTS_FIXTURE_PEM),
            "a system-locked pair must survive OCX_NO_CONFIG=1"
        );
        assert_eq!(
            loaded.extra_ca_certs_tier,
            Some(ConfigTier::System),
            "the record must survive with the pair — a refusal still names the system file"
        );
        assert_eq!(loaded.extra_ca_certs_tier_local, Some(ConfigTier::System));
    }

    /// EC-CFG-007 — `OCX_CONFIG` is a third consent-bearing channel, and
    /// the managed tier's digest gate never reaches it. One load proves both
    /// halves: the unpinned managed payload's grant is stripped, the
    /// `OCX_CONFIG` grant of the same shape is honoured.
    ///
    /// The recorded strip reason is the premise check — without it, an
    /// explicit-only `paths` set is indistinguishable from a managed snapshot
    /// that never loaded at all.
    ///
    /// Red state: call [`ConfigLoader::guard_managed_shell_consent`] on the
    /// overlay too and the explicit grant disappears; delete the gate and
    /// `/managed/grant` joins the set.
    #[tokio::test]
    async fn a33_ec_cfg_007_ocx_config_grants_consent_the_digest_gate_never_reaches() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);
        env.remove(&ocx_env::OCX_CONSENT_PATHS);
        env.remove(&ocx_env::OCX_CONSENT_NAMESPACES);

        // A tag, not a digest: whoever can move it can swap the grant.
        std::fs::write(
            dir.path().join("config.toml"),
            "[managed]\nsource = \"registry.test/managed-config:v1\"\nrequired = false\n",
        )
        .unwrap();
        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[shell.consent]\npaths = [\"/managed/grant\"]\n",
        );

        let explicit_dir = TempDir::new().unwrap();
        let explicit = write_config(
            &explicit_dir,
            "chosen.toml",
            "[shell.consent]\npaths = [\"/explicit/grant\"]\n",
        );
        env.set(&ocx_env::OCX_CONFIG, explicit.to_str().unwrap());

        let loaded = ConfigLoader::load_with_local_view(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load must succeed");
        let shell = loaded.merged.shell.as_ref().expect("both tiers contribute [shell]");
        let reason = shell
            .consent_strip_reason
            .as_deref()
            .expect("the managed payload must have reached the fold and been stripped there");
        assert!(
            reason.contains("digest-pinned"),
            "the recorded reason must name the cause, got: {reason}"
        );

        let consent = crate::shell::effective_consent(Some(shell));
        assert_eq!(
            consent.paths,
            vec![PathBuf::from("/explicit/grant")],
            "the OCX_CONFIG grant activates and the unpinned managed grant does not — the gate is managed-tier-only, \
             and an explicit file names a local path the user chose"
        );
    }

    /// EC-CFG-008 — `OCX_NO_CONFIG=1` prunes every config-tier grant and
    /// leaves the `OCX_CONSENT_*` channel intact. The asymmetry is the point,
    /// so all three states are asserted — including the flag-off premise,
    /// without which "no grant" would be indistinguishable from a fixture that
    /// never granted anything.
    ///
    /// Red state: drop the `no_config` guard on `discovered` in
    /// [`ConfigLoader::load_with_local_view`] and the middle assertion sees the
    /// home tier's grant; prune the env channel alongside it and the last one
    /// goes empty.
    #[tokio::test]
    async fn a33_ec_cfg_008_no_config_prunes_config_tier_grants_but_not_the_env_channel() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);
        env.remove(&ocx_env::OCX_CONSENT_PATHS);
        env.remove(&ocx_env::OCX_CONSENT_NAMESPACES);
        std::fs::write(
            dir.path().join("config.toml"),
            "[shell.consent]\npaths = [\"/home/grant\"]\n",
        )
        .unwrap();

        env.remove(&ocx_env::OCX_NO_CONFIG);
        assert_eq!(
            consent_after_load().await.paths,
            vec![PathBuf::from("/home/grant")],
            "premise: without the flag the home tier's entry really is a grant"
        );

        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        assert!(
            consent_after_load().await.paths.is_empty(),
            "OCX_NO_CONFIG=1 prunes the discovered chain, so every config-tier grant goes with it"
        );

        env.set(&ocx_env::OCX_CONSENT_PATHS, "/env/grant");
        assert_eq!(
            consent_after_load().await.paths,
            vec![PathBuf::from("/env/grant")],
            "OCX_NO_CONFIG touches neither the explicit tiers nor OCX_CONSENT_*; only OCX_NO_HOOK makes a shell wholly \
             inert"
        );
    }

    /// The loader stamps the tier a file belongs to, and only where that
    /// file set the scalar.
    #[tokio::test]
    async fn c032_the_loader_stamps_the_tier_that_set_each_scalar() {
        use crate::ConfigTier;

        let dir = TempDir::new().expect("tempdir");
        let path = write_config(&dir, "config.toml", "[shell]\nhook = true\n");
        let config = ConfigLoader::load_and_merge(&[path]).await.expect("load");

        let shell = config.shell.expect("shell");
        assert_eq!(
            shell.hook_tier,
            Some(ConfigTier::Explicit),
            "a path that is none of the three discovered candidates reached the loader as an explicit tier"
        );
        assert_eq!(
            shell.completions_tier, None,
            "a tier that did not set `completions` must not claim to have decided it"
        );
    }

    // ── the recorded config-tier paths ──────────────────────────────────────

    /// The watch set stats a list the loader recorded, and it must
    /// include tier files that do NOT exist — a grant added by creating one is
    /// exactly the change an `inert` cache has to expire on.
    #[tokio::test]
    async fn a13_records_every_config_tier_candidate_including_absent_ones() {
        // The env lock, and a SYSTEM candidate this test names itself: the
        // loader reads `__OCX_TESTING_SYSTEM_CONFIG`, `OCX_CONFIG` and
        // `OCX_NO_CONFIG` from the ambient environment, so without both it
        // could observe a sibling's fixture mid-run — the symlinked system
        // config two tests over turns this into a fatal load, intermittently.
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().expect("tempdir");
        without_system_config(&env);
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        let explicit = write_config(&dir, "explicit.toml", "[shell]\nhook = true\n");

        let loaded = ConfigLoader::load_with_local_view(ConfigInputs {
            explicit_path: Some(explicit.as_path()),
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load");

        assert!(
            loaded.config_tier_paths.contains(&ConfigLoader::system_path()),
            "the system tier is recorded whether or not it exists today"
        );
        assert!(
            loaded.config_tier_paths.contains(&explicit),
            "the --config override is recorded — it is a consent-bearing channel of its own"
        );
        assert_eq!(
            loaded.config_tier_paths.last(),
            Some(&explicit),
            "the list is in fold order, so the highest-precedence tier is last"
        );
    }

    /// Under `OCX_NO_CONFIG=1`: the recorded list narrows to what the flag
    /// still reads. The system tier stays — it loads for its locked sections, so
    /// an operator adding one changes the resolved config — while the user and
    /// `$OCX_HOME` tiers, which the flag genuinely prunes, drop out.
    #[tokio::test]
    async fn a13_under_no_config_records_the_system_tier_and_nothing_below_it() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().expect("tempdir");
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.remove(&ocx_env::OCX_CONFIG);
        without_system_config(&env);

        let loaded = ConfigLoader::load_with_local_view(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load");

        assert_eq!(
            loaded.config_tier_paths,
            vec![ConfigLoader::system_path()],
            "the system tier is still read under the flag, so it is still watched — and it is the only one"
        );
    }

    /// Same contract as `managed_snapshot_cannot_override_system_locked_registry`,
    /// for `[records]`. A system-scope `[records]` is what lets an operator make
    /// recording a fleet property instead of a wrapper-script convention, so the
    /// managed tier — a payload fetched from a registry, i.e. the one tier that
    /// is not a local file the operator wrote — must be able to neither redirect
    /// the sink nor loosen the fail posture. The clamp is binary and per-block:
    /// `dir`, `name` and `required` are pinned together.
    #[tokio::test]
    async fn managed_snapshot_cannot_override_system_locked_records() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[records]\ndir = \"/tmp/attacker-sink\"\nrequired = false\n",
        );

        // Simulate an accumulator that already folded a locked SYSTEM tier
        // (in production: `/etc/ocx/config.toml` via `load_and_merge`'s
        // `apply_system_locks` branch) plus a home tier whose `[managed].source`
        // matches the snapshot above.
        let mut records = crate::records::RecordsOptions {
            dir: Some(PathBuf::from("/var/log/ocx/records")),
            required: Some(true),
            ..Default::default()
        };
        records.lock_as_system();
        let accumulator = crate::Config {
            records: Some(records),
            managed: Some(crate::managed::ManagedConfig {
                source: Some("registry.test/managed-config:v1".to_string()),
                required: Some(false),
                ..Default::default()
            }),
            ..crate::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved, _state, _) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
            .await
            .expect("fold must succeed even against a locked accumulator");

        let folded_records = folded.records.expect("[records] must survive the fold");
        assert_eq!(
            folded_records.dir,
            Some(PathBuf::from("/var/log/ocx/records")),
            "a system-locked [records] sink must survive a managed-payload redirection attempt"
        );
        assert_eq!(
            folded_records.required,
            Some(true),
            "a system-locked [records] fail posture must not be loosened by a managed payload"
        );
    }

    /// The discriminating half of the pair above. Without a `[records]` arm in
    /// `Config::merge` the payload's section is dropped on the floor, so the
    /// locked-tier test would pass for the wrong reason — the clamp would look
    /// enforced while nothing was ever merged. This pins that an UNLOCKED
    /// `[records]` genuinely folds, which is what makes the locked case a
    /// statement about the lock rather than about a missing merge arm.
    #[tokio::test]
    async fn managed_snapshot_overrides_unlocked_records() {
        let env = ocx_env::overrides::lock();
        let dir = TempDir::new().unwrap();
        env.set(&ocx_env::OCX_HOME, dir.path().to_str().unwrap());
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);

        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[records]\ndir = \"/var/log/ocx/fleet\"\n",
        );

        let accumulator = crate::Config {
            records: Some(crate::records::RecordsOptions {
                dir: Some(PathBuf::from("/home/dev/records")),
                required: Some(true),
                ..Default::default()
            }),
            managed: Some(crate::managed::ManagedConfig {
                source: Some("registry.test/managed-config:v1".to_string()),
                required: Some(false),
                ..Default::default()
            }),
            ..crate::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved, _state, _) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
            .await
            .expect("fold must succeed");

        let folded_records = folded.records.expect("[records] must survive the fold");
        assert_eq!(
            folded_records.dir,
            Some(PathBuf::from("/var/log/ocx/fleet")),
            "an unlocked [records] must let the higher managed tier redirect the sink"
        );
        assert_eq!(
            folded_records.required,
            Some(true),
            "a field the payload leaves unset must not be clobbered"
        );
    }

    // ── `toolchain_dir` across the tiers ──────────────────────────────────────
    //
    // Written from `adr_toolchain_activation.md` § `config.toml` placement key,
    // not from an implementation. Two kinds of row: those that ask only
    // what the loader *merged* (they pin the prune),
    // and those that hand the merged config to `ToolchainRoot::resolve` and
    // assert the root it admits or the refusal it reports.

    /// A `$OCX_HOME` anchor plus a hermetic tier set, for the rows that resolve
    /// a root rather than only merging one — or `None` when this host's
    /// temporary root cannot serve as a containment anchor.
    ///
    /// Through `crate::sandbox_or_skip`, never a bare `TempDir::new()`.
    /// On macOS `$TMPDIR` is under `/var/folders/…`, which canonicalises through
    /// the `/var` → `private/var` symlink to `/private/var/folders/…`, and
    /// `/private` is a live system-location prefix — so pass 2 refuses every root beneath
    /// it with `SystemPrefix` and the `.expect("… is admitted")` calls below
    /// panic on `verify-deep.yml`'s `macos-latest` leg. That helper is at
    /// `crate` scope for exactly this reason: the reasoning was written
    /// once in `config.rs` and this module could not see it.
    fn toolchain_anchor(env: &ocx_env::overrides::EnvLock) -> Option<TempDir> {
        let anchor = crate::sandbox_or_skip()?;
        env.set(&ocx_env::OCX_HOME, anchor.path().to_str().expect("temp path is utf-8"));
        env.remove(&ocx_env::OCX_TOOLCHAIN_DIR);
        env.remove(&ocx_env::OCX_CONFIG);
        env.remove(&ocx_env::OCX_NO_CONFIG);
        env.remove(&ocx_env::OCX_MANAGED_CONFIG);
        without_system_config(env);
        Some(anchor)
    }

    /// A fleet operator's `[managed]` payload carries `toolchain_dir`
    /// into the merged config like any other tier. `fold_managed_tier` parses
    /// the payload as a whole `Config`, so the key needs no separate
    /// allow-listing; this pins that it stays that way.
    #[tokio::test]
    async fn managed_payload_carries_toolchain_dir_into_the_merged_config() {
        let env = ocx_env::overrides::lock();
        let Some(anchor) = toolchain_anchor(&env) else { return };
        let fleet_root = anchor.path().join("fleet");

        std::fs::write(
            anchor.path().join("config.toml"),
            "[managed]\nsource = \"registry.test/managed-config:v1\"\nrequired = false\n",
        )
        .unwrap();
        write_managed_snapshot(
            anchor.path(),
            "registry.test/managed-config:v1",
            &toml_path_line("toolchain_dir", &fleet_root),
        );

        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load should succeed");

        assert_eq!(
            config.toolchain_dir.as_deref(),
            Some(fleet_root.as_path()),
            "a [managed] payload's toolchain_dir must reach the merged config (S-004)"
        );
        let resolved = crate::ToolchainRoot::resolve(&config)
            .expect("a fleet root inside $OCX_HOME is admitted")
            .expect("the payload declared a root");
        assert_eq!(
            resolved.as_path(),
            dunce::canonicalize(anchor.path())
                .expect("canonicalise the anchor")
                .join("fleet"),
            "the fleet-pushed root resolves like any other tier's"
        );
    }

    /// The `[managed]` tier is not a bypass. A fleet-pushed
    /// system location faces the identical refusal a hand-written
    /// `config.toml` would, and reports as the `config.toml` tier because that
    /// is what an operator edits to fix it.
    #[tokio::test]
    async fn managed_payload_toolchain_dir_faces_the_identical_refusal() {
        let env = ocx_env::overrides::lock();
        let Some(anchor) = toolchain_anchor(&env) else { return };

        std::fs::write(
            anchor.path().join("config.toml"),
            "[managed]\nsource = \"registry.test/managed-config:v1\"\nrequired = false\n",
        )
        .unwrap();
        // This host's own system location, not a POSIX literal: `/usr` has no
        // drive prefix on Windows, so `Path::is_absolute` is false there and
        // the value would be refused as `Relative` — a real refusal, but not
        // the system-location one this row is about.
        let system_location = if cfg!(windows) {
            PathBuf::from(ocx_env::SYSTEM_ROOT.get().unwrap_or_else(|| r"C:\Windows".to_string()))
        } else {
            PathBuf::from("/usr")
        };
        write_managed_snapshot(
            anchor.path(),
            "registry.test/managed-config:v1",
            &toml_path_line("toolchain_dir", &system_location),
        );

        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load should succeed");

        let error = crate::ToolchainRoot::resolve(&config)
            .expect_err("a fleet-pushed system location is refused like any other tier's");
        assert!(
            matches!(
                error,
                crate::ToolchainRootError::SystemPrefix {
                    tier: crate::ToolchainRootTier::ConfigFile,
                    ..
                }
            ),
            "the [managed] tier folds into the config.toml chain and is refused as one; got {error}"
        );
    }

    /// `OCX_NO_CONFIG=1` prunes an ambient `toolchain_dir`, and the environment
    /// tier then becomes the effective one.
    ///
    /// Both halves matter: the prune alone would leave a hermetic child with no
    /// root at all, and the shipped continuity story is that a parent ocx
    /// forwards its own resolved root as `OCX_TOOLCHAIN_DIR`.
    #[tokio::test]
    async fn hermetic_mode_prunes_an_ambient_toolchain_dir_and_the_environment_tier_takes_over() {
        let env = ocx_env::overrides::lock();
        let Some(anchor) = toolchain_anchor(&env) else { return };
        let dir = TempDir::new().unwrap();
        with_system_config(
            &env,
            &dir,
            &toml_path_line("toolchain_dir", &anchor.path().join("from-system")),
        );
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.set(
            &ocx_env::OCX_TOOLCHAIN_DIR,
            anchor.path().join("from-env").to_str().expect("temp path is utf-8"),
        );

        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load should succeed");

        assert!(
            config.toolchain_dir.is_none(),
            "toolchain_dir is ambient host configuration and OCX_NO_CONFIG=1 prunes it"
        );
        let resolved = crate::ToolchainRoot::resolve(&config)
            .expect("the environment root is inside $OCX_HOME")
            .expect("the environment tier declared a root");
        assert_eq!(
            resolved.as_path(),
            dunce::canonicalize(anchor.path())
                .expect("canonicalise the anchor")
                .join("from-env"),
            "with the file tier pruned, OCX_TOOLCHAIN_DIR is the effective tier"
        );
    }

    /// `OCX_NO_CONFIG=1` does **not** prune a `toolchain_dir` from an explicit
    /// `--config` / `OCX_CONFIG` file: `retain_system_locked_sections` runs
    /// over the discovered chain only, and the explicit overlay merges
    /// afterwards. So the explicit file still beats the environment tier.
    #[tokio::test]
    async fn hermetic_mode_keeps_an_explicit_config_toolchain_dir() {
        let env = ocx_env::overrides::lock();
        let Some(anchor) = toolchain_anchor(&env) else { return };
        let dir = TempDir::new().unwrap();
        let explicit = write_config(
            &dir,
            "explicit.toml",
            &toml_path_line("toolchain_dir", &anchor.path().join("from-file")),
        );
        env.set(&ocx_env::OCX_NO_CONFIG, "1");
        env.set(
            &ocx_env::OCX_TOOLCHAIN_DIR,
            anchor.path().join("from-env").to_str().expect("temp path is utf-8"),
        );

        let config = ConfigLoader::load(ConfigInputs {
            explicit_path: Some(&explicit),
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load should succeed");

        assert_eq!(
            config.toolchain_dir.as_deref(),
            Some(anchor.path().join("from-file").as_path()),
            "an explicitly named file is not ambient configuration, so the flag does not prune it"
        );
        let resolved = crate::ToolchainRoot::resolve(&config)
            .expect("the explicit root is inside $OCX_HOME")
            .expect("the explicit file declared a root");
        assert_eq!(
            resolved.as_path(),
            dunce::canonicalize(anchor.path())
                .expect("canonicalise the anchor")
                .join("from-file"),
            "the explicit file still beats the environment tier under OCX_NO_CONFIG=1"
        );
    }
}
