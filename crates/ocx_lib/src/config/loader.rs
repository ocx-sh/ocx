// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Configuration discovery and loading.
//!
//! Discovery is deliberately separated from loading so that future tiers
//! (e.g., the project-level `ocx.toml` walk in #33) can be added by
//! extending [`ConfigLoader::discover_paths`] without rewriting any other
//! function. CWD is passed in via [`ConfigInputs`] rather than read from
//! the environment, keeping the loader testable without filesystem
//! side effects.

use std::path::{Path, PathBuf};

use futures::future::join_all;
use tokio::io::AsyncReadExt;

use crate::Result;
use crate::config::{Config, error::ConfigSource, error::Error};
use crate::log;

/// Upper bound for a single config file. Config files are expected to be
/// well under 1 KiB; 64 KiB is a generous safety cap that also rules out
/// accidentally pointing `--config` at a multi-megabyte file.
const MAX_CONFIG_SIZE: u64 = 64 * 1024;

/// Inputs to config discovery — captures all caller-provided context so the
/// loader never reads ambient state directly.
pub struct ConfigInputs<'a> {
    /// `--config FILE` CLI flag (highest priority among explicit paths).
    pub explicit_path: Option<&'a Path>,
    /// `--project <FILE>` CLI flag (highest priority among project-tier sources).
    pub explicit_project_path: Option<&'a Path>,
    /// CWD for the project-tier walk (#33). Pass `None` to disable the walk.
    pub cwd: Option<&'a Path>,
}

/// Result of [`ConfigLoader::load_with_local_view`]: the fully merged config
/// alongside the local-only merged config.
///
/// "Local-only" means every discovered/explicit tier that requires no
/// network access (system, user, `$OCX_HOME`, `OCX_CONFIG`, `--config`). The
/// managed-config tier (a network-fetched artifact, folded in by
/// [`ConfigLoader::fold_managed_tier`]) is the one exception layered on top of
/// `local_only` to produce `merged` — the seam exists so the managed-config
/// fetch can build its client from `local_only`'s mirror map (its own payload
/// must not be able to redirect the route used to fetch itself).
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// The fully merged config (every tier, including any future
    /// network-fetched tier layered on top of `local_only`).
    pub merged: Config,
    /// The merged config from local-only tiers.
    pub local_only: Config,
    /// The raw managed-config snapshot [`ConfigLoader::fold_managed_tier`]
    /// read from disk, if any — BEFORE the identity gate (present even when
    /// the snapshot's provenance does not match the effective source, so a
    /// consumer's own identity check, e.g.
    /// [`crate::resolve_managed_config`]'s `required` enforcement, still sees
    /// a mismatched snapshot rather than a silently-absent one). `None` when
    /// no candidate exists (including `OCX_NO_CONFIG=1`, which prunes the
    /// candidate entirely) or the on-disk file is absent/unreadable/malformed.
    /// Exposed so callers (`Context::try_init`) reuse this read instead of
    /// re-reading the same file from disk.
    pub managed_config_snapshot: Option<crate::config::managed::ManagedConfigSnapshot>,
    /// The effective managed-config target [`ConfigLoader::fold_managed_tier`]
    /// resolved (from the local-only view, so the payload cannot redirect the
    /// tier that fetched it), or `None` when no source is configured OR the
    /// resolution errored (the fold swallows resolution errors — the caller
    /// re-resolves to surface a malformed seed). Threaded so `Context::try_init`
    /// reuses this single resolution for the required gate and the snapshot
    /// identity gate instead of resolving the same target two more times.
    pub resolved_managed_config: Option<crate::config::managed::ResolvedManagedConfig>,
}

/// Configuration loader. Stateless namespace for the discovery and loading
/// pipeline.
pub struct ConfigLoader;

impl ConfigLoader {
    /// Top-level entry: build the ordered path list, load, and merge.
    ///
    /// Layering (lowest → highest precedence):
    /// 1. system / user / `$OCX_HOME` tiers — skipped when `OCX_NO_CONFIG=1`
    /// 2. managed-config snapshot (identity-gated; also suppressed by
    ///    `OCX_NO_CONFIG=1`)
    /// 3. `OCX_CONFIG` — if set and non-empty
    /// 4. `--config FILE` (via [`ConfigInputs::explicit_path`])
    ///
    /// Explicit paths (both env-var and CLI) always load if set; they layer
    /// on top of the discovered chain (or on top of nothing, if
    /// `OCX_NO_CONFIG=1` pruned it). Empty `OCX_CONFIG=""` is treated
    /// as unset — an escape hatch so users can disable ambient env-var
    /// config without unsetting it.
    ///
    /// All filesystem I/O uses [`tokio::fs`] so the loader can run inside
    /// the async runtime without blocking a worker thread.
    ///
    /// # Errors
    /// Returns an error on missing explicit files, I/O failure, or TOML
    /// parse failure.
    pub async fn load(inputs: ConfigInputs<'_>) -> Result<Config> {
        Ok(Self::load_with_local_view(inputs).await?.merged)
    }

    /// Like [`Self::load`], but also returns the local-only merged view
    /// alongside the fully merged config — see [`LoadedConfig`].
    ///
    /// # Errors
    /// Same as [`Self::load`].
    pub async fn load_with_local_view(inputs: ConfigInputs<'_>) -> Result<LoadedConfig> {
        let no_config = crate::env::flag("OCX_NO_CONFIG", false);
        let raw_env_config_file = crate::env::var("OCX_CONFIG");
        if raw_env_config_file.as_deref() == Some("") {
            log::debug!("OCX_CONFIG is set to empty string — skipped via escape hatch");
        }
        let env_config_file = raw_env_config_file.filter(|s| !s.is_empty());

        // Resolve the project-tier path first so a missing `--project` or
        // `OCX_PROJECT` surfaces as `FileNotFound` (exit 79) before we
        // read anything else. Phase 1 only wires error propagation — the
        // returned path itself is consumed in later phases once the
        // project-config schema lands.
        let _project_path = Self::project_path(inputs.cwd, inputs.explicit_project_path).await?;

        let discovered: Vec<PathBuf> = if no_config {
            Vec::new()
        } else {
            Self::discover_paths().await
        };
        let mut explicit_paths: Vec<PathBuf> = Vec::new();
        if let Some(env_path) = env_config_file {
            explicit_paths.push(PathBuf::from(env_path));
        }
        if let Some(explicit) = inputs.explicit_path {
            explicit_paths.push(explicit.to_path_buf());
        }

        // Precedence (ADR Decision A): the managed tier folds in AFTER the
        // discovered chain (system → user → home) but BELOW `OCX_CONFIG` and
        // `--config`, so the explicit tiers must merge on top of the managed
        // fold — never underneath it. The explicit overlay is loaded once and
        // applied to both views; merging is per-section last-wins, so folding
        // the overlay as one pre-merged `Config` is equivalent to folding its
        // files individually.
        let base = Self::load_and_merge(&discovered).await?;
        let overlay = Self::load_and_merge(&explicit_paths).await?;

        let mut local_only = base.clone();
        local_only.merge(overlay.clone());

        // The fold resolves its effective source from `local_only` (base +
        // overlay) so a `[managed].source` declared ONLY in `OCX_CONFIG`/
        // `--config` still activates the payload merge — see
        // `fold_managed_tier`'s doc comment. Merge order is unchanged: the
        // payload folds onto `base`, and `overlay` is applied on top of that
        // afterward, so explicit tiers still beat payload values.
        let (mut merged, managed_config_snapshot, resolved_managed_config) =
            Self::fold_managed_tier(base, &local_only).await?;
        merged.merge(overlay);

        Ok(LoadedConfig {
            merged,
            local_only,
            managed_config_snapshot,
            resolved_managed_config,
        })
    }

    /// 4th discovery candidate (ADR Decision A): the managed-config snapshot,
    /// folded in after the `home_path()` tier and below `OCX_CONFIG`/
    /// `--config`. Zero network here — the snapshot is read from local state
    /// only.
    ///
    /// `OCX_NO_CONFIG=1` suppresses this candidate entirely (hermetic means
    /// hermetic).
    ///
    /// Path duplication is avoided via the pure associated fn
    /// [`crate::file_structure::StateStore::managed_config_snapshot_path`],
    /// shared by the loader and the store accessor — this never constructs a
    /// [`crate::file_structure::StateStore`].
    fn managed_snapshot_candidate() -> Option<PathBuf> {
        if crate::env::flag("OCX_NO_CONFIG", false) {
            return None;
        }
        let ocx_home = Self::home_dir()?;
        Some(crate::file_structure::StateStore::managed_config_snapshot_path(
            &ocx_home,
        ))
    }

    /// Identity-gated one-hop-strip merge of the managed-config snapshot onto
    /// `accumulator` (ADR Decision A).
    ///
    /// Resolves the effective source locally (`OCX_MANAGED_CONFIG` env
    /// override, else the `[managed].source` seed already folded into
    /// `local_only` — base tiers PLUS the `OCX_CONFIG`/`--config` overlay;
    /// amended post-Codex-gate 2026-07-05, see the ADR "Loader integration"
    /// decision — resolving from the base tiers alone let an overlay-only
    /// seed activate `Context::try_init`'s required-gate without ever folding
    /// its payload here), then merges the snapshot ONLY when its embedded
    /// provenance `source` equals that effective source under canonical
    /// [`crate::oci::Identifier`] equality (tag and digest significant). The
    /// snapshot's embedded TOML is parsed as a [`Config`], its `[managed]`
    /// table is stripped before the merge (one hop — a payload can never
    /// redirect the tier that fetched it; a present `[managed]` is WARNed,
    /// Decision I). Merge order is unaffected: `accumulator` (base only) is
    /// what the payload actually folds onto — the overlay is layered on top
    /// by the caller afterward, so explicit tiers still beat payload values.
    ///
    /// Every absence path — no candidate, missing/unreadable/malformed
    /// snapshot, identity mismatch — is a silent no-op (`accumulator`
    /// returned unchanged, debug log only): a wrong-identity snapshot must
    /// never reach [`Config`], and a benign absent state must not WARN.
    /// Zero network here, ever.
    ///
    /// Also returns the RAW snapshot this call read from disk (before the
    /// identity gate below), so [`Self::load_with_local_view`] can expose it
    /// via [`LoadedConfig::managed_config_snapshot`] and callers avoid a
    /// second read of the same file. The raw value is `Some` even on an
    /// identity mismatch — only a missing candidate or an
    /// absent/unreadable/malformed on-disk file yields `None`.
    ///
    /// The effective [`ResolvedManagedConfig`](crate::config::managed::ResolvedManagedConfig)
    /// target is returned as the third tuple element so `Context::try_init`
    /// reuses this single resolution instead of resolving the same target two
    /// more times. It is `None` when no source is configured OR the resolution
    /// errored (swallowed here — the fold is best-effort; the caller re-resolves
    /// to surface a malformed seed as the authoritative error).
    ///
    /// The target is resolved FIRST so a non-managed user never pays the
    /// `snapshot.json` stat: the file is read only once a source resolves.
    async fn fold_managed_tier(
        accumulator: Config,
        local_only: &Config,
    ) -> Result<(
        Config,
        Option<crate::config::managed::ManagedConfigSnapshot>,
        Option<crate::config::managed::ResolvedManagedConfig>,
    )> {
        // Resolve the effective source LOCALLY: env `OCX_MANAGED_CONFIG`
        // (suppressed by `OCX_NO_CONFIG` — hermetic) over `local_only`'s
        // already-folded `managed.source` (base tiers — system/user/home — PLUS
        // the `OCX_CONFIG`/`--config` overlay).
        //
        // Uses `resolve_managed_target` — the SAME lock-aware resolution
        // `resolve_managed_config`'s `required` gate uses — instead of a raw
        // env-over-seed computation. Regression (Codex-flagged 2026-07-05): a
        // raw resolution here disagreed with the lock-aware one once the
        // system-lock env-override guard landed: a system-locked source A
        // plus a mismatched `OCX_MANAGED_CONFIG=B` made this gate compare the
        // snapshot against B (mismatch, fold skipped) while the required gate
        // separately re-resolved back to A and found the SAME snapshot
        // satisfying — required reported satisfied with the payload silently
        // never folded. Sharing the resolution closes the drift permanently.
        let env_override = if crate::env::flag(crate::env::keys::OCX_NO_CONFIG, false) {
            None
        } else {
            crate::env::var(crate::env::keys::OCX_MANAGED_CONFIG).filter(|value| !value.is_empty())
        };
        let Some(resolved) = crate::config::managed::resolve_managed_target(local_only, env_override.as_deref())
            .ok()
            .flatten()
        else {
            return Ok((accumulator, None, None));
        };

        // A source resolved — read the snapshot from local state only now, so a
        // non-managed user never pays the stat above.
        let Some(candidate) = Self::managed_snapshot_candidate() else {
            return Ok((accumulator, None, Some(resolved)));
        };
        let Some(snapshot) = crate::managed_config::read_managed_config_snapshot_at(&candidate).await else {
            // Absent, unreadable, or malformed JSON — treated as absent
            // (benign-state rule, no per-invocation WARN).
            return Ok((accumulator, None, Some(resolved)));
        };

        // Canonical `oci::Identifier` equality (tag/digest significant) —
        // never applies a snapshot fetched under a different identity, even
        // for `required = false` tiers (CI cache-poison defense). Uses the
        // shared `snapshot_matches_source` predicate so this gate and
        // `resolve_managed_config`'s `required` gate can never drift.
        let identity_matches = crate::config::managed::snapshot_matches_source(&snapshot, &resolved.source);
        if !identity_matches {
            log::debug!(
                "managed-config snapshot source does not match the effective source '{}'; treating as absent",
                resolved.source
            );
            return Ok((accumulator, Some(snapshot), Some(resolved)));
        }

        let mut parsed: Config = match toml::from_str(&snapshot.config) {
            Ok(parsed) => parsed,
            Err(source) => {
                log::debug!("managed-config snapshot payload is not valid TOML, treating as absent: {source}");
                return Ok((accumulator, Some(snapshot), Some(resolved)));
            }
        };
        // ADR Decision I (one-hop): a remote payload can never redirect or
        // loosen the tier that fetched it.
        if parsed.managed.take().is_some() {
            log::warn!(
                "managed-config payload for '{}' contained a [managed] section; stripped before merge (a remote \
                 payload can never redirect the tier that fetched it)",
                resolved.source
            );
        }

        let mut accumulator = accumulator;
        accumulator.merge(parsed);
        Ok((accumulator, Some(snapshot), Some(resolved)))
    }

    /// Discover the ordered list of config files to load (lowest precedence
    /// first).
    ///
    /// Returns `[system_path, user_path, home_path]` filtering out `None`
    /// and nonexistent files. The project-tier path is resolved separately
    /// by [`Self::project_path`] because it returns a single
    /// `Option<PathBuf>` rather than joining the tier chain.
    ///
    /// Async because it calls [`tokio::fs::symlink_metadata`] per candidate
    /// path. `NotFound` candidates are silently skipped. Symlinked candidates
    /// are rejected with a warning — an attacker who can write to a
    /// discovered-tier location (`/etc/ocx/config.toml`,
    /// `~/.config/ocx/config.toml`, `$OCX_HOME/config.toml`) could otherwise
    /// point the link at any readable file and surface its contents via a
    /// parse-error message or provoke unexpected side effects on load.
    /// Explicit paths (`--config`, `OCX_CONFIG`) are trusted caller
    /// input and are not subject to this check. Other I/O errors (permission
    /// denied, stale NFS handle, EIO) are logged as warnings and the
    /// candidate is still skipped — discovery never fails the whole process,
    /// but an unreadable `~/.ocx/config.toml` should at least be
    /// *diagnosable*. A race between discovery and read surfaces later as
    /// [`Error::Io`] during [`Self::load_and_merge`].
    pub async fn discover_paths() -> Vec<PathBuf> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        candidates.push(Self::system_path());
        if let Some(user) = Self::user_path() {
            candidates.push(user);
        }
        if let Some(home) = Self::home_path() {
            candidates.push(home);
        }
        // join_all preserves input order, so the precedence semantics of the
        // candidate list (system → user → $OCX_HOME) are unchanged. Running
        // the symlink_metadata calls concurrently shaves two sequential
        // filesystem round-trips on startup. We use `symlink_metadata` rather
        // than `try_exists` (which follows symlinks) so the symlink-rejection
        // branch can observe the link itself without dereferencing it.
        let checks = join_all(candidates.iter().map(tokio::fs::symlink_metadata)).await;
        candidates
            .into_iter()
            .zip(checks)
            .filter_map(|(path, result)| match result {
                Ok(meta) if meta.file_type().is_symlink() => {
                    log::warn!(
                        "skipping symlinked config candidate {} (discovered-tier config files must not be symlinks)",
                        path.display()
                    );
                    None
                }
                Ok(_) => Some(path),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
                Err(source) => {
                    log::warn!("skipping unreadable config candidate {}: {source}", path.display());
                    None
                }
            })
            .collect()
    }

    /// Resolve the project-tier `ocx.toml` path.
    ///
    /// Precedence: `explicit` (from `--project`) > `OCX_PROJECT` env
    /// var > CWD walk > **None**. There is no implicit `$OCX_HOME/ocx.toml`
    /// fallback — the global toolchain is reachable only via the explicit
    /// `--global`/`OCX_GLOBAL` selector handled by
    /// [`crate::project::ProjectConfig::resolve`]
    /// (see adr_global_toolchain_tier.md §Decision 1).
    /// `OCX_NO_PROJECT=1` prunes the walk and the env var
    /// but does NOT prune the explicit flag (trusted caller intent, per
    /// ADR `adr_project_toolchain_config.md` Amendment G3). Empty
    /// `OCX_PROJECT=""` is treated as unset (escape hatch, matches
    /// `OCX_CONFIG=""`).
    ///
    /// The CWD walk stops at the first `ocx.toml`, any `.git/` boundary,
    /// or `OCX_CEILING_PATH`. Discovered (walked) paths reject symlinks;
    /// explicit paths (flag / env) follow symlinks (trusted caller).
    ///
    /// # Errors
    /// Returns [`Error::FileNotFound`] when an explicit source (`--project`
    /// or `OCX_PROJECT`) names a path that does not exist (exit 79).
    /// CWD-walk misses return `Ok(None)` — the walk treats absence as a
    /// non-event, unlike explicit caller intent.
    pub async fn project_path(
        cwd: Option<&Path>,
        explicit: Option<&Path>,
    ) -> std::result::Result<Option<PathBuf>, crate::config::error::Error> {
        // Tier 1: explicit `--project` flag — highest priority. Follows
        // symlinks (trusted caller intent). Missing file → FileNotFound.
        // Not gated by `OCX_NO_PROJECT` (Amendment G3).
        if let Some(path) = explicit {
            return Self::resolve_explicit_project_path(path).await;
        }

        // `OCX_NO_PROJECT=1` prunes BOTH env-var and CWD-walk lookups
        // (Amendment G3). This diverges from `OCX_NO_CONFIG` + explicit
        // env-var tier-config behavior deliberately: the project file is
        // a single, unique source, not a composed tier chain — so "turn
        // project discovery off entirely" is the useful kill switch.
        let no_project = crate::env::flag("OCX_NO_PROJECT", false);
        if no_project {
            return Ok(None);
        }

        // Tier 2: `OCX_PROJECT` env var. Empty string is the escape
        // hatch (matches `OCX_CONFIG=""` pattern in `load()` above).
        let raw_env = crate::env::var("OCX_PROJECT");
        if raw_env.as_deref() == Some("") {
            log::debug!("OCX_PROJECT is set to empty string — skipped via escape hatch");
        }
        if let Some(env_path) = raw_env.filter(|s| !s.is_empty()) {
            return Self::resolve_explicit_project_path(Path::new(&env_path)).await;
        }

        // Tier 3: CWD walk.
        let walk_result = match cwd {
            Some(start) => {
                let ceiling = crate::env::var("OCX_CEILING_PATH").map(PathBuf::from);
                Self::walk_for_project_file(start, ceiling.as_deref()).await
            }
            None => None,
        };
        // No implicit `$OCX_HOME/ocx.toml` fallback: the global toolchain
        // is reachable only via the explicit `--global`/`OCX_GLOBAL`
        // selector handled in `ProjectConfig::resolve`. CWD-walk miss is a
        // hard `None` (see adr_global_toolchain_tier.md §Decision 1).
        Ok(walk_result)
    }

    /// Resolve an explicit project-tier path (from `--project` or
    /// `OCX_PROJECT`). Explicit paths follow symlinks and must resolve
    /// to a regular file.
    async fn resolve_explicit_project_path(
        path: &Path,
    ) -> std::result::Result<Option<PathBuf>, crate::config::error::Error> {
        // `tokio::fs::metadata` follows symlinks (trusted caller intent, G5).
        match tokio::fs::metadata(path).await {
            Ok(meta) if meta.file_type().is_file() => Ok(Some(path.to_path_buf())),
            Ok(_) => {
                // Path exists but is not a regular file (directory, device,
                // FIFO). Phase 1 discards the resolved path, so if we
                // returned `Ok(Some(path))` here the defect would silently
                // slip through discovery and surface only at parse time.
                // Surface now as `Error::Io` (exit 74, ADR G9).
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
            // Other I/O errors (permission denied, stale NFS handle, EIO).
            // Phase 1 discards the resolved path, so if we returned
            // `Ok(Some(path))` here the error would silently vanish. Surface
            // as `Error::Io` (exit 74) so an explicit caller gets a
            // diagnosable failure instead of a phantom success (ADR G9).
            Err(source) => Err(Error::Io {
                path: path.to_path_buf(),
                tier: ConfigSource::Project,
                source,
            }),
        }
    }

    /// Walk up from `start`, looking for `ocx.toml`. Stops at the first hit,
    /// any `.git/` boundary, or `ceiling` (if set). Returns `None` if the
    /// walk exhausts the filesystem root without finding a project file.
    ///
    /// Symlinks discovered at the walk step are rejected (G5): the candidate
    /// is skipped and the walk continues upward. `.git/` detection uses
    /// `symlink_metadata` as well so a `.git` symlink does not silently
    /// weaken the boundary check. A `.git` file (git worktree linkfile) also
    /// counts as a boundary — we match git's own "any `.git` entry" rule.
    async fn walk_for_project_file(start: &Path, ceiling: Option<&Path>) -> Option<PathBuf> {
        let mut current = start;
        loop {
            // Probe `.git/` and `ocx.toml` concurrently; `tokio::join!` just
            // overlaps the two stat round-trips. Precedence at each level
            // (Amendment F): a valid `ocx.toml` at the current level wins
            // over the `.git/` boundary — the boundary only prevents walking
            // UP past the repo root, not accepting a project file AT the
            // repo root. The `.git/` gate therefore fires AFTER the
            // candidate-hit check but before ascending.
            let candidate = current.join("ocx.toml");
            let (git_present, candidate_meta) =
                tokio::join!(Self::has_git_dir(current), tokio::fs::symlink_metadata(&candidate),);

            match &candidate_meta {
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
                    // Directory or other non-regular file named `ocx.toml` —
                    // treat as absent. A weird mount point shouldn't derail
                    // discovery or cause silent success.
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    log::warn!(
                        "skipping unreadable project candidate {}: {source}",
                        candidate.display()
                    );
                }
            }

            // No valid hit at this level — respect the repo boundary before
            // ascending so we don't walk into a parent repository.
            if git_present {
                return None;
            }

            // Ceiling check runs AFTER probing the current directory so a
            // ceiling that points exactly at a workspace containing
            // `ocx.toml` still discovers it. The ceiling bounds the walk
            // from going ABOVE it, not from reading AT it.
            if let Some(ceiling) = ceiling
                && current == ceiling
            {
                return None;
            }

            match current.parent() {
                Some(parent) => current = parent,
                // Reached the filesystem root without a hit.
                None => return None,
            }
        }
    }

    /// Returns `true` if `dir/.git` exists as any filesystem entry (directory,
    /// file — the git worktree "linkfile" case — or symlink).
    ///
    /// Fail-closed on non-`NotFound` I/O errors: if the filesystem reports
    /// `PermissionDenied` or `EIO` for `.git`, we cannot *disprove* the
    /// presence of a repository boundary, and silently letting the walk
    /// cross into the parent would weaken the boundary check. The safer
    /// default is to assume the boundary exists.
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

    /// Load and merge an ordered list of config files (lowest precedence
    /// first). Missing files at this stage are an error — discovery should
    /// have filtered them. Async I/O via [`tokio::fs`] + parse + merge.
    ///
    /// Accepts any slice of `AsRef<Path>` so callers can pass `&[PathBuf]`,
    /// `&[&Path]`, or a borrowed single path without forced allocation.
    ///
    /// # Errors
    /// Returns an error if any file is missing, unreadable, exceeds
    /// [`MAX_CONFIG_SIZE`], or contains invalid TOML.
    pub async fn load_and_merge<P: AsRef<Path>>(paths: &[P]) -> Result<Config> {
        let mut config = Config::default();
        for path in paths {
            let path = path.as_ref();
            // Reject a non-regular path (e.g. a directory) with a consistent
            // message on every platform *before* opening. On Windows,
            // `File::open` on a directory fails with a generic "Access is
            // denied" that never reaches the `is_file()` guard below; on Unix
            // the open succeeds and the guard fires. Stat-first makes the
            // rejection uniform across platforms.
            if let Ok(meta) = tokio::fs::metadata(path).await
                && !meta.file_type().is_file()
            {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    tier: ConfigSource::Config,
                    source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "config path is not a regular file"),
                }
                .into());
            }
            let file = match tokio::fs::File::open(path).await {
                Ok(f) => f,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    return Err(Error::FileNotFound {
                        path: path.to_path_buf(),
                        tier: ConfigSource::Config,
                    }
                    .into());
                }
                Err(source) => {
                    return Err(Error::Io {
                        path: path.to_path_buf(),
                        tier: ConfigSource::Config,
                        source,
                    }
                    .into());
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
                }
                .into());
            }
            if metadata.len() > MAX_CONFIG_SIZE {
                return Err(Error::FileTooLarge {
                    path: path.to_path_buf(),
                    size: metadata.len(),
                    limit: MAX_CONFIG_SIZE,
                }
                .into());
            }
            // Bounded read so synthetic files whose `metadata.len()` is 0 but whose read is
            // unbounded (e.g. /proc/self/mem, /proc/self/maps on Linux) can't bypass the size
            // cap and hang/exhaust memory. The `metadata.len()` pre-check above still fast-paths
            // normal oversized files without reading any bytes.
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
                }
                .into());
            }
            let mut parsed: Config = toml::from_str(&contents).map_err(|source| Error::Parse {
                path: path.to_path_buf(),
                source,
            })?;
            // C7 enforcement — see [`Self::apply_system_locks`] for the full
            // per-section rationale. Only the system file is locked; it folds
            // in first, so locked sections ignore all lower-tier overrides.
            if path == Self::system_path().as_path() {
                Self::apply_system_locks(&mut parsed);
            }
            config.merge(parsed);
        }
        Ok(config)
    }

    /// C7 enforcement: lock every lockable section of a system-scope config
    /// (`/etc/ocx/config.toml`) as non-overridable BEFORE it merges into the
    /// accumulator. The system tier folds in first, so a locked section then
    /// ignores all lower-tier overrides (including an untrusted
    /// managed-config payload).
    ///
    /// Covers all five lockable sections: `[patches]` (lock is conditional
    /// inside `PatchConfig::lock_as_system`), `[registry]` (unconditional),
    /// each `[registries.<name>]` entry (unconditional, per name — closes
    /// the indirection `resolved_default_registry` resolves through), each
    /// `[mirrors."<host>"]` entry (per host, per role — `MirrorConfig::lock_as_system`
    /// locks only the `registry`/`index` role(s) that entry actually declares,
    /// `adr_index_indirection.md` F5b), and `[managed]`
    /// (required-gated inside `ManagedConfig::lock_as_system`, like
    /// `[patches]` — a system-scope `required = true` seed must not be
    /// loosenable/clearable by the home tier's fence; ADR Decision G,
    /// criterion 13). Extracted so the section coverage is unit-testable
    /// without writing to `/etc`.
    fn apply_system_locks(parsed: &mut Config) {
        if let Some(patches) = parsed.patches.as_mut() {
            patches.lock_as_system();
        }
        if let Some(registry) = parsed.registry.as_mut() {
            registry.lock_as_system();
        }
        if let Some(registries) = parsed.registries.as_mut() {
            for entry in registries.values_mut() {
                entry.lock_as_system();
            }
        }
        if let Some(mirrors) = parsed.mirrors.as_mut() {
            for mirror in mirrors.values_mut() {
                mirror.lock_as_system();
            }
        }
        if let Some(managed) = parsed.managed.as_mut() {
            managed.lock_as_system();
        }
    }

    /// System config: `/etc/ocx/config.toml`.
    pub fn system_path() -> PathBuf {
        PathBuf::from("/etc/ocx/config.toml")
    }

    /// User config: `$XDG_CONFIG_HOME/ocx/config.toml` or
    /// `~/.config/ocx/config.toml` (via `dirs::config_dir`).
    pub fn user_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("ocx").join("config.toml"))
    }

    /// `$OCX_HOME` directory, falling back to `~/.ocx`.
    ///
    /// Single resolver shared by [`Self::home_path`] (returns
    /// `<dir>/config.toml`). Returns `None` when `OCX_HOME` is unset and
    /// `dirs::home_dir()` cannot resolve a home directory (e.g., a service
    /// account with no `$HOME`).
    ///
    /// Kept private: callers compose well-known children through one of
    /// the named `home_*_path()` accessors above so `$OCX_HOME` path math
    /// stays in this module. External code that needs a `$OCX_HOME`-rooted
    /// path should add (or extend) such an accessor rather than reach for
    /// the bare directory.
    fn home_dir() -> Option<PathBuf> {
        if let Some(ocx_home) = crate::env::var("OCX_HOME") {
            return Some(PathBuf::from(ocx_home));
        }
        dirs::home_dir().map(|h| h.join(".ocx"))
    }

    /// `$OCX_HOME/config.toml`, falling back to `~/.ocx/config.toml`.
    pub fn home_path() -> Option<PathBuf> {
        Self::home_dir().map(|d| d.join("config.toml"))
    }
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
            "[registries.shared]\nurl = \"old.example\"\n\n[registries.only_low]\nurl = \"low.example\"",
        );
        let high = write_config(
            &dir,
            "high.toml",
            "[registries.shared]\nurl = \"new.example\"\n\n[registries.only_high]\nurl = \"high.example\"",
        );
        let config = ConfigLoader::load_and_merge(&[low, high])
            .await
            .expect("two-file registries merge should succeed");
        let registries = config.registries.expect("registries should be present");
        assert_eq!(registries.len(), 3);
        assert_eq!(
            registries["shared"].url.as_deref(),
            Some("new.example"),
            "higher tier should win on conflicting key"
        );
        assert_eq!(registries["only_low"].url.as_deref(), Some("low.example"));
        assert_eq!(registries["only_high"].url.as_deref(), Some("high.example"));
    }

    // ── load() orchestration tests ───────────────────────────────────────────
    //
    // Env-touching tests acquire `crate::test::env::lock()` — a process-wide
    // mutex whose Drop clears all overrides. Overrides route through
    // `crate::env::var`'s `#[cfg(test)]` branch; no `std::env::set_var`, no
    // `unsafe`.

    #[tokio::test]
    async fn load_with_no_config_returns_default() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "hermetic.toml", "[registry]\ndefault = \"hermetic.example\"");
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "env-hermetic.toml",
            "[registry]\ndefault = \"env-hermetic.example\"",
        );
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", path.to_str().unwrap());
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
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", "");
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
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "ci.toml", "[registry]\ndefault = \"ci.example\"");
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", path.to_str().unwrap());
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "ci.toml", "[registry]\ndefault = \"ci.example\"");
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", path.to_str().unwrap());
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let env_file = write_config(&dir, "env.toml", "[registry]\ndefault = \"env.example\"");
        let explicit_file = write_config(&dir, "explicit.toml", "[registry]\ndefault = \"explicit.example\"");
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", env_file.to_str().unwrap());
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

    // ── Path resolver tests ──────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn system_path_is_etc_ocx_config_toml() {
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
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

        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let locked_home = dir.path().join("locked-home");
        std::fs::create_dir(&locked_home).unwrap();
        let locked_config = locked_home.join("config.toml");
        std::fs::write(&locked_config, "").unwrap();
        // Mode 0o000 strips search permission; stat() on the child fails with
        // PermissionDenied, which is the error kind discover_paths should now
        // log+skip rather than silently collapse.
        std::fs::set_permissions(&locked_home, std::fs::Permissions::from_mode(0o000)).unwrap();

        env.set("OCX_HOME", locked_home.to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");

        let paths = ConfigLoader::discover_paths().await;

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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("ocx-home");
        std::fs::create_dir(&home).unwrap();
        // Create a real target file, then symlink `config.toml` → target.
        let target = dir.path().join("target.toml");
        std::fs::write(&target, "[registry]\ndefault = \"symlinked.example\"").unwrap();
        let symlink_path = home.join("config.toml");
        std::os::unix::fs::symlink(&target, &symlink_path).expect("create symlink");

        env.set("OCX_HOME", home.to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");

        let paths = ConfigLoader::discover_paths().await;

        assert!(
            !paths.contains(&symlink_path),
            "symlinked candidate must be skipped, got: {paths:?}"
        );
    }

    #[test]
    fn home_path_fallback_when_ocx_home_unset() {
        // With OCX_HOME removed, home_path() falls back to `dirs::home_dir()`.
        // The result is platform-dependent: Some(path ending in .ocx/config.toml)
        // when HOME is set, None otherwise. Both are valid outcomes.
        let env = crate::test::env::lock();
        env.remove("OCX_HOME");
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
    // Plan Phase 1 (plan_project_toolchain.md, lines 77–93) defines the
    // resolver contract:
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
    // All env-touching tests acquire `crate::test::env::lock()` — the same
    // process-wide mutex used by the `load()` tests above.

    /// Helper: write a file at `path` with the given content.
    fn write_file(path: &std::path::Path, content: &str) {
        std::fs::write(path, content).expect("write test fixture");
    }

    /// Plan bullet: `--project <valid>` → loads the file.
    #[tokio::test]
    async fn project_path_explicit_flag_loads_valid_file() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT");
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
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
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT");
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let missing = PathBuf::from("/tmp/ocx-project-path-test-missing-explicit.toml");
        let err = ConfigLoader::project_path(None, Some(&missing))
            .await
            .expect_err("missing explicit path should be FileNotFound");
        assert!(
            matches!(
                err,
                crate::config::error::Error::FileNotFound {
                    ref path,
                    tier: crate::config::error::ConfigSource::Project,
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
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT");
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let bad = PathBuf::from("/dev/null/not-a-real-file.toml");
        let err = ConfigLoader::project_path(None, Some(&bad))
            .await
            .expect_err("non-NotFound I/O error on explicit path must surface");
        assert!(
            matches!(
                err,
                crate::config::error::Error::Io {
                    ref path,
                    tier: crate::config::error::ConfigSource::Project,
                    ..
                } if path == &bad,
            ),
            "expected Error::Io(Project) for ENOTDIR on explicit --project path, got: {err:?}"
        );
    }

    /// Plan bullet: `OCX_PROJECT=<valid>` → loads the file.
    #[tokio::test]
    async fn project_path_env_var_loads_valid_file() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("custom-name.toml");
        write_file(&path, "");
        env.set("OCX_PROJECT", path.to_str().unwrap());
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
        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.set("OCX_PROJECT", "");
        env.remove("OCX_NO_PROJECT");
        let dir = TempDir::new().unwrap();
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());
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
        let env = crate::test::env::lock();
        env.set("OCX_NO_PROJECT", "1");
        let dir = TempDir::new().unwrap();
        // Even placing a valid ocx.toml at cwd must not be discovered.
        let cwd_project = dir.path().join("ocx.toml");
        write_file(&cwd_project, "");
        // And a valid env-var path must also be ignored.
        let env_path = dir.path().join("env.toml");
        write_file(&env_path, "");
        env.set("OCX_PROJECT", env_path.to_str().unwrap());
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
        let env = crate::test::env::lock();
        env.set("OCX_NO_PROJECT", "1");
        env.remove("OCX_PROJECT");
        env.remove("OCX_CEILING_PATH");
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
        let env = crate::test::env::lock();
        env.set("OCX_NO_PROJECT", "1");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("env-hermetic.toml");
        write_file(&path, "");
        env.set("OCX_PROJECT", path.to_str().unwrap());
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
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();

        // CWD-walk candidate at the root of a throw-away workspace.
        let walk_dir = dir.path().join("workspace");
        std::fs::create_dir(&walk_dir).unwrap();
        let walk_path = walk_dir.join("ocx.toml");
        write_file(&walk_path, "");

        // Env-var candidate.
        let env_path = dir.path().join("env.toml");
        write_file(&env_path, "");
        env.set("OCX_PROJECT", env_path.to_str().unwrap());

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
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let file_a = dir.path().join("a.toml");
        let file_b = dir.path().join("b.toml");
        write_file(&file_a, "");
        write_file(&file_b, "");
        env.set("OCX_PROJECT", file_b.to_str().unwrap());
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
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        let ceiling = dir.path().join("ceiling");
        std::fs::create_dir(&ceiling).unwrap();
        env.set("OCX_CEILING_PATH", ceiling.to_str().unwrap());
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
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        env.remove("OCX_CEILING_PATH");
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
        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        // .git/ absent, ceiling bounds the walk to the temp dir.
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

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
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        env.remove("OCX_CEILING_PATH");
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
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let project = root.join("ocx.toml");
        write_file(&project, "");
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

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
        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
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
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

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
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_stops_at_git_worktree_linkfile() {
        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
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
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("walk must stop at .git linkfile boundary");
        assert_eq!(
            resolved, None,
            ".git file (worktree linkfile) must also act as a repo boundary"
        );
    }

    /// Amendment F precedence: `ocx.toml` at the repo root (alongside
    /// `.git/`) must be returned — the `.git/` boundary only prevents
    /// walking UP past the repo root, it does not disqualify a project
    /// file AT that level. Regression guard for the common case.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_finds_ocx_toml_at_git_root_level() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        let project = repo.join("ocx.toml");
        write_file(&project, "");
        let nested = repo.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("walk from nested cwd must find ocx.toml at repo root");
        assert_eq!(
            resolved,
            Some(project),
            "a repo root with both .git/ and ocx.toml must resolve to the ocx.toml"
        );
    }

    /// Explicit `--project <dir>` must not silently succeed — non-file
    /// targets surface as `Error::Io` (exit 74, ADR G9) rather than being
    /// accepted as a valid project-file path.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_explicit_directory_rejected_as_io() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT");
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let target = dir.path().to_path_buf();
        let err = ConfigLoader::project_path(None, Some(&target))
            .await
            .expect_err("explicit --project pointing at a directory must error");
        assert!(
            matches!(
                err,
                crate::config::error::Error::Io {
                    ref path,
                    tier: crate::config::error::ConfigSource::Project,
                    ..
                } if path == &target,
            ),
            "expected Error::Io(Project) for directory on explicit --project path, got: {err:?}"
        );
    }

    /// Plan bullet: No `ocx.toml`, no explicit path → returns `None`.
    #[tokio::test]
    async fn project_path_returns_none_when_no_source() {
        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());
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
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let missing = PathBuf::from("/tmp/ocx-project-path-test-missing-env.toml");
        env.set("OCX_PROJECT", missing.to_str().unwrap());
        let err = ConfigLoader::project_path(None, None)
            .await
            .expect_err("missing env-var path should be FileNotFound");
        assert!(
            matches!(
                err,
                crate::config::error::Error::FileNotFound {
                    ref path,
                    tier: crate::config::error::ConfigSource::Project,
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
    #[tokio::test]
    async fn project_path_walk_without_git_or_ceiling_returns_none() {
        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        env.remove("OCX_CEILING_PATH");
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

    /// Max-tier edge case: `OCX_CEILING_PATH` set above the `ocx.toml` → returns `None`.
    ///
    /// Amendment F — ceiling is the paired bound for the walk. When the
    /// ceiling sits between cwd and the `ocx.toml`, discovery must stop.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_stops_at_ceiling() {
        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        // ocx.toml at the OUTERMOST level (above the ceiling).
        let outer_project = dir.path().join("ocx.toml");
        write_file(&outer_project, "");
        // Ceiling sits inside the tempdir; walk starts below the ceiling.
        let ceiling = dir.path().join("ceiling");
        std::fs::create_dir(&ceiling).unwrap();
        env.set("OCX_CEILING_PATH", ceiling.to_str().unwrap());
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

    /// Max-tier edge case: no ceiling, no `.git/`, deeply nested cwd, `ocx.toml`
    /// many levels up → found.
    ///
    /// Stresses the loop-termination logic: we expect the walk to actually
    /// traverse several levels rather than short-circuiting at one or two.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_no_ceiling_no_git_finds_ocx_toml_many_levels_up() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        // Set the ceiling at the tempdir so the test cannot accidentally walk
        // past it into the real filesystem; the `ocx.toml` is placed just
        // below the ceiling so the walk has work to do without leaving the
        // sandbox. This still stresses five levels of parent traversal, which
        // is the point of the test.
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());
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
        let path = crate::file_structure::StateStore::managed_config_snapshot_path(ocx_home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let snapshot = serde_json::json!({
            "source": source,
            "digest": format!("sha256:{}", "a".repeat(64)),
            "fetched_at": "2026-07-04T00:00:00Z",
        });
        std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        std::fs::write(
            crate::file_structure::StateStore::managed_config_toml_path_for_snapshot(&path),
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

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

    /// Criterion 7 (loader-level): a snapshot whose embedded provenance does
    /// not match the effective source must be treated as absent — content
    /// never reaches `Config`, mirrors/registry/patches included.
    #[tokio::test]
    async fn managed_snapshot_source_mismatch_is_never_merged() {
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");

        std::fs::write(
            dir.path().join("config.toml"),
            "[managed]\nsource = \"seed.test/managed-config:v1\"\n",
        )
        .unwrap();
        // The snapshot's provenance matches the ENV override, not the seed.
        env.set("OCX_MANAGED_CONFIG", "override.test/managed-config:v1");
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

        std::fs::write(
            dir.path().join("config.toml"),
            "[managed]\nsource = \"registry.test/managed-config:v1\"\n",
        )
        .unwrap();
        let snapshot_path = crate::file_structure::StateStore::managed_config_snapshot_path(dir.path());
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
        env.set("OCX_MANAGED_CONFIG", "registry.test/managed-config:v1");

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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[registry]\ndefault = \"malicious-registry.example\"\n",
        );

        // Simulate an accumulator that already folded a locked SYSTEM tier
        // (in production this comes from `/etc/ocx/config.toml` via
        // `load_and_merge`'s `lock_as_system` branch) plus a home tier whose
        // `[managed].source` matches the snapshot above.
        let mut registry = crate::config::RegistryDefaults {
            default: Some("system-locked-registry.example".to_string()),
            system_locked: false,
        };
        registry.lock_as_system();
        let accumulator = crate::config::Config {
            registry: Some(registry),
            managed: Some(crate::config::managed::ManagedConfig {
                source: Some("registry.test/managed-config:v1".to_string()),
                required: Some(false),
                ..Default::default()
            }),
            ..crate::config::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.set("OCX_MANAGED_CONFIG", "hostile.test/evil-config:latest");

        write_managed_snapshot(
            dir.path(),
            "system.corp/ocx-config:user",
            "[registry]\ndefault = \"corp-registry.example\"\n",
        );

        // Simulate an accumulator/local-only view that already folded a
        // locked SYSTEM tier (in production: `/etc/ocx/config.toml` via
        // `load_and_merge`'s `lock_as_system` branch).
        let mut managed = crate::config::managed::ManagedConfig {
            source: Some("system.corp/ocx-config:user".to_string()),
            required: Some(true),
            ..Default::default()
        };
        managed.lock_as_system();
        let accumulator = crate::config::Config {
            managed: Some(managed),
            ..crate::config::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, snapshot, _resolved) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
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

    /// Same contract as `managed_snapshot_cannot_override_system_locked_registry`,
    /// but through the `[registries.<name>].url` indirection:
    /// `Config::resolved_default_registry` resolves `[registry] default`
    /// through the NAMED `[registries.<name>]` entry's `url`, so a lock on
    /// Regression (review round 2): the `[managed]` lock call was missing
    /// from the system-scope wiring — `ManagedConfig::lock_as_system` existed
    /// but was never invoked, so criterion 13 was unenforced dead code. Pins
    /// that `apply_system_locks` covers every lockable section, so a newly
    /// added lockable section that misses the wiring fails here.
    #[test]
    fn apply_system_locks_covers_every_lockable_section() {
        let mut config: crate::config::Config = toml::from_str(concat!(
            "[patches]\nregistry = \"patches.corp.example\"\nrequired = true\n",
            "[registry]\ndefault = \"corp\"\n",
            "[registries.corp]\nurl = \"registry.corp.example\"\n",
            "[mirrors]\n\"docker.io\" = \"https://mirror.corp.example\"\n",
            "[managed]\nsource = \"corp/managed-config:stable\"\nrequired = true\n",
        ))
        .unwrap();

        ConfigLoader::apply_system_locks(&mut config);

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
    }

    /// `RegistryDefaults` alone leaves that indirection open — a managed
    /// payload could redirect the entry instead of the default name. This
    /// pins that the entry-level lock (`RegistryConfig::system_locked`)
    /// closes it too.
    #[tokio::test]
    async fn managed_snapshot_cannot_override_system_locked_registries_entry() {
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[registries.corp]\nurl = \"malicious-registry.example\"\n",
        );

        // Simulate an accumulator that already folded a locked SYSTEM tier:
        // both `[registry] default = "corp"` and `[registries.corp].url` are
        // system-locked (in production via `/etc/ocx/config.toml`'s
        // `lock_as_system` branch).
        let mut registry = crate::config::RegistryDefaults {
            default: Some("corp".to_string()),
            system_locked: false,
        };
        registry.lock_as_system();
        let mut corp_entry = crate::config::RegistryConfig {
            url: Some("system-locked-registry.example".to_string()),
            index: None,
            system_locked: false,
        };
        corp_entry.lock_as_system();
        let mut registries = std::collections::HashMap::new();
        registries.insert("corp".to_string(), corp_entry);
        let accumulator = crate::config::Config {
            registry: Some(registry),
            registries: Some(registries),
            managed: Some(crate::config::managed::ManagedConfig {
                source: Some("registry.test/managed-config:v1".to_string()),
                required: Some(false),
                ..Default::default()
            }),
            ..crate::config::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
            .await
            .expect("fold must succeed even against a locked accumulator");

        assert_eq!(
            folded.resolved_default_registry(),
            Some("system-locked-registry.example"),
            "a system-locked [registries.<name>] entry must survive a managed-payload redirection attempt"
        );
    }
}
