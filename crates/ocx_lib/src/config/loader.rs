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

use std::collections::HashMap;
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

/// Test-only seam redirecting [`ConfigLoader::system_path`] away from
/// `/etc/ocx/config.toml`, so the SYSTEM-scope lock path is exercisable
/// without root. Gated per the `__OCX_*` seam convention in
/// `subsystem-tests.md` — absent from release builds, never forwarded to
/// child processes, not user-facing configuration.
#[cfg(any(test, feature = "__testing"))]
pub const SYSTEM_CONFIG_OVERRIDE: &str = "__OCX_TESTING_SYSTEM_CONFIG";

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
    /// The discovered tiers alone — compiled-in defaults plus system, user and
    /// `$OCX_HOME`, WITHOUT the explicit `OCX_CONFIG` / `--config` overlay.
    /// This is what the managed tier folds onto.
    pub base: Config,
    /// The explicit `OCX_CONFIG` / `--config` overlay alone — the tier that
    /// merges on top of the managed fold.
    ///
    /// `base` and `overlay` are exposed as the pair, not as the pre-merged
    /// `local_only`, so a caller can reproduce the adoption fold order for a
    /// payload of its own: `base` -> payload -> `overlay`. `ocx config test`
    /// is that caller.
    pub overlay: Config,
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
    /// What the managed-config snapshot actually contributed to `merged` —
    /// the `required` gate's input, reported by the tier that did the folding
    /// rather than re-derived by the caller. A snapshot whose identity matches
    /// but whose payload does not parse is
    /// [`PayloadUnusable`](crate::config::managed::ManagedSnapshotState::PayloadUnusable):
    /// it is on disk, and none of it is in `merged`.
    pub managed_snapshot_state: crate::config::managed::ManagedSnapshotState,

    /// Every `config.toml` path this pass could have read, in fold order and
    /// **including ones that do not exist** (A-13).
    ///
    /// A consent grant can be added to any of them, so the per-prompt watch set
    /// stats this recorded list — presence, mtime and size — instead of
    /// re-deriving it or, worse, re-parsing config every prompt. A tier file
    /// that did not exist becoming present is a change, which is why absent
    /// candidates are recorded too.
    pub config_tier_paths: Vec<PathBuf>,
}

/// Configuration loader. Stateless namespace for the discovery and loading
/// pipeline.
pub struct ConfigLoader;

impl ConfigLoader {
    /// Top-level entry: build the ordered path list, load, and merge.
    ///
    /// Layering (lowest → highest precedence):
    /// 1. compiled-in defaults ([`Self::builtin_defaults`]) — never pruned
    /// 2. system / user / `$OCX_HOME` tiers — under `OCX_NO_CONFIG=1` the user
    ///    and `$OCX_HOME` tiers are skipped and the system tier is reduced to
    ///    its locked sections (see [`Self::retain_system_locked_sections`])
    /// 3. managed-config snapshot (identity-gated; also suppressed by
    ///    `OCX_NO_CONFIG=1`)
    /// 4. `OCX_CONFIG` — if set and non-empty
    /// 5. `--config FILE` (via [`ConfigInputs::explicit_path`])
    ///
    /// Explicit paths (both env-var and CLI) always load if set; they layer
    /// on top of the discovered chain (or on top of an operator lock, or on
    /// top of nothing, if `OCX_NO_CONFIG=1` pruned it). Empty `OCX_CONFIG=""`
    /// is treated as unset — an escape hatch so users can disable ambient
    /// env-var config without unsetting it.
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

        // `OCX_NO_CONFIG=1` prunes ambient configuration, not operator policy:
        // the system file still loads so its locked sections survive, and is
        // filtered to exactly those sections below.
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

        // Precedence (ADR Decision A): the managed tier folds in AFTER the
        // discovered chain (system → user → home) but BELOW `OCX_CONFIG` and
        // `--config`, so the explicit tiers must merge on top of the managed
        // fold — never underneath it. The explicit overlay is loaded once and
        // applied to both views; merging is per-section last-wins, so folding
        // the overlay as one pre-merged `Config` is equivalent to folding its
        // files individually.
        // The filter applies to the discovered file tiers only: the compiled-in
        // tier is not ambient host state, so `OCX_NO_CONFIG` does not prune it
        // (see `builtin_defaults`) — and its entry is unlocked, so folding it in
        // first would hand it to the filter to drop.
        let mut discovered_config = Self::load_and_merge(&discovered).await?;
        if no_config {
            Self::retain_system_locked_sections(&mut discovered_config);
        }
        let mut base = Self::builtin_defaults();
        base.merge(discovered_config);
        let overlay = Self::load_and_merge(&explicit_paths).await?;

        let mut local_only = base.clone();
        local_only.merge(overlay.clone());

        // The fold resolves its effective source from `local_only` (base +
        // overlay) so a `[managed].source` declared ONLY in `OCX_CONFIG`/
        // `--config` still activates the payload merge — see
        // `fold_managed_tier`'s doc comment. Merge order is unchanged: the
        // payload folds onto `base`, and `overlay` is applied on top of that
        // afterward, so explicit tiers still beat payload values.
        let (mut merged, managed_config_snapshot, resolved_managed_config, managed_snapshot_state) =
            Self::fold_managed_tier(base.clone(), &local_only).await?;
        merged.merge(overlay.clone());

        // A-13: the candidate list, not the surviving one — `discover_paths`
        // filtered out every tier file that does not exist, and a grant added
        // to one of those is exactly the change the watch set must notice.
        //
        // Under `OCX_NO_CONFIG=1` that list is the system tier alone, matching
        // what the flag now actually reads: the user and `$OCX_HOME` tiers are
        // pruned, but `/etc/ocx/config.toml` still loads for its locked
        // sections, so an operator adding one there changes the resolved config
        // and must expire a cached verdict like any other watched file.
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
        })
    }

    /// The compiled-in base tier — the lowest-precedence layer, below every
    /// file tier.
    ///
    /// Carries exactly one setting: `ocx.sh` resolves through
    /// [`DEFAULT_INDEX_BASE_URL`](crate::oci::index::DEFAULT_INDEX_BASE_URL).
    /// Seeding the marker as config rather than special-casing the namespace
    /// downstream keeps `adr_index_indirection.md` Decision H intact — a
    /// `[registries."<ns>"] index` value stays the SOLE protocol-kind marker,
    /// and every existing override path applies unchanged: a different index,
    /// `index = ""` to revert `ocx.sh` to plain OCI, or a system-scope lock.
    ///
    /// Deliberately NOT gated on `OCX_NO_CONFIG`: that flag prunes ambient
    /// host state (discovered files, the managed snapshot) so a run is
    /// reproducible, and a constant compiled into the binary is exactly the
    /// reproducible part.
    ///
    /// The entry is stamped `index_is_compiled_default` so a consumer can tell
    /// this value apart from an identical one a config file wrote. The CLI's
    /// `build_index_sources` uses that to let an explicit `[mirrors."ocx.sh"]`
    /// entry suppress this default — the one policy question this tier cannot
    /// answer itself, because `[mirrors]` is only fully resolved (config plus
    /// the forwarded `OCX_MIRRORS` env) after the loader has run.
    fn builtin_defaults() -> Config {
        Config {
            registries: Some(HashMap::from([(
                crate::oci::OCX_SH_REGISTRY.to_string(),
                crate::config::RegistryConfig {
                    index: Some(crate::oci::index::DEFAULT_INDEX_BASE_URL.to_string()),
                    index_is_compiled_default: true,
                    ..Default::default()
                },
            )])),
            ..Default::default()
        }
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
        let ocx_home = crate::file_structure::default_ocx_root()?;
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
    /// Every absence path — no candidate, missing/unreadable snapshot,
    /// identity mismatch — is a silent no-op (`accumulator` returned
    /// unchanged, debug log only): a wrong-identity snapshot must never reach
    /// [`Config`], and a benign absent state must not WARN. An
    /// identity-matching snapshot whose payload does not parse is the one
    /// non-benign case and WARNs. Zero network here, ever.
    ///
    /// The fourth tuple element reports which of those happened as a
    /// [`ManagedSnapshotState`](crate::config::managed::ManagedSnapshotState),
    /// so `Context::try_init`'s `required` gate can fail closed on a snapshot
    /// that exists but contributed nothing — an identity check alone reports
    /// such a tier satisfied.
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
        crate::config::managed::ManagedSnapshotState,
    )> {
        use crate::config::managed::ManagedSnapshotState;

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
            return Ok((accumulator, None, None, ManagedSnapshotState::Unmatched));
        };

        // A source resolved — read the snapshot from local state only now, so a
        // non-managed user never pays the stat above.
        let Some(candidate) = Self::managed_snapshot_candidate() else {
            return Ok((accumulator, None, Some(resolved), ManagedSnapshotState::Unmatched));
        };
        let Some(snapshot) = crate::managed_config::read_managed_config_snapshot_at(&candidate).await else {
            // Absent, unreadable, or malformed JSON — treated as absent
            // (benign-state rule, no per-invocation WARN).
            return Ok((accumulator, None, Some(resolved), ManagedSnapshotState::Unmatched));
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
            return Ok((
                accumulator,
                Some(snapshot),
                Some(resolved),
                ManagedSnapshotState::Unmatched,
            ));
        }

        let mut parsed: Config = match Self::parse_config_stripping_refused_consent(&snapshot.config, &resolved.source)
        {
            Ok(parsed) => parsed,
            Err(source) => {
                // NOT the benign-absent case: a snapshot for THIS source is on
                // disk and none of it can be applied. Every unknown section and
                // key is tolerated (see `Config`), and a refused `[shell.consent]`
                // table is dropped on its own by the call above, so reaching here
                // means the payload is genuinely broken — worth a WARN even when
                // `required = false`, where nothing else would report it.
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
                ));
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

        Self::guard_managed_sigstore_trust(&mut parsed, &resolved.source);
        Self::guard_managed_shell_consent(&mut parsed, &resolved.source);
        Self::stamp_shell_tier(&mut parsed, crate::config::ConfigTier::Managed);

        let mut accumulator = accumulator;
        accumulator.merge(parsed);
        Ok((
            accumulator,
            Some(snapshot),
            Some(resolved),
            ManagedSnapshotState::Applied,
        ))
    }

    /// Strips the `[trust]` values a remote payload is not entitled to set,
    /// each for its own reason.
    ///
    /// A `[[trust.policy]]` signer naming its key by path (`key = "/srv/acme.pub"`)
    /// is dropped unconditionally, for the reason `trusted_root` is: the path
    /// names the **publisher's** disk, and on a fleet machine it either does not
    /// exist or resolves to some unrelated local file — which the consumer would
    /// then read, sight unseen, on every verification. `ocx config push` refuses
    /// to publish that form at all; this is the consumer-side half, because the
    /// publish-time check runs on the publisher and a payload can reach a
    /// machine by other routes. `key_pem` travels with the payload and is left
    /// alone: it is bounded by `MAX_MANAGED_CONFIG_BYTES` and names no file.
    ///
    /// `trusted_root` names a path on the **publisher's** disk. On a fleet
    /// machine it either does not exist or — worse — resolves to some unrelated
    /// local file. `ocx config push` inlines it as `trusted_root_json` at
    /// publish time precisely so this case never has to be honoured.
    ///
    /// `trusted_root_json`, `fulcio_url` and `rekor_url` are honoured only
    /// behind a **digest-pinned** `[managed] source`. Otherwise the trust
    /// material arrives over the very channel it exists to verify, and a
    /// registry able to move the tag can swap the CA. The circularity is
    /// broken by a pinned seed, not by policy.
    ///
    /// The two endpoints obey that rule for the same reason the trust root
    /// does, and one sharper: `fulcio_url` is where the OIDC identity token is
    /// sent. `ocx package push --sbom` has no `--fulcio-url` flag to oppose a
    /// config value, so an unpinned payload that could set it would name the
    /// server a signing identity is handed to.
    fn guard_managed_sigstore_trust(parsed: &mut Config, source: &crate::oci::Identifier) {
        let Some(trust) = parsed.trust.as_mut() else {
            return;
        };
        for policy in &mut trust.policy {
            for signer in &mut policy.signers {
                // `dropped` is whether a path form was present at all;
                // `unusable` is whether removing it left the entry with no key
                // of any kind.
                let (dropped, unusable) = match signer {
                    crate::trust::SignerSpec::Key(matcher) => {
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
                    // Blanking the field alone leaves a `KeyMatcher` with
                    // neither `key` nor `key_pem`, which `validate_signers`
                    // refuses by name — taking the **whole** policy down,
                    // sibling keyless signers included, over a diagnostic the
                    // operator never wrote. `Unknown` is the arm that already
                    // means "this build cannot use this signer": it compiles to
                    // no backend, so it narrows, and a policy whose signers all
                    // narrow away is refused as `NoUsableSigner` instead.
                    *signer = crate::trust::SignerSpec::Unknown;
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
    ///
    /// **The SYSTEM candidate is the exception, and best-effort is exactly
    /// wrong for it**: it carries operator policy (every section
    /// [`Self::apply_system_locks`] clamps), so skipping the file skips the
    /// policy with it — silently, on every invocation. A symlinked or otherwise
    /// unreadable `/etc/ocx/config.toml` is therefore
    /// [`Error::SystemConfig`] (exit 78) rather than a warning. Absence stays
    /// silent there too: no system file is the ordinary case.
    ///
    /// # Errors
    /// Returns [`Error::SystemConfig`] when the SYSTEM candidate exists but
    /// cannot be consulted.
    pub async fn discover_paths() -> std::result::Result<Vec<PathBuf>, Error> {
        Self::existing_candidates(Self::tier_candidates()).await
    }

    /// The ordered discovered-tier candidate list (lowest precedence first),
    /// before any filesystem check. Pure so the tier order is testable without
    /// touching `/etc`.
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

    /// Filesystem half of [`Self::discover_paths`]: drop candidates that do
    /// not exist, are symlinks, or are unreadable — except the SYSTEM one,
    /// where everything but absence is fatal (see [`Self::discover_paths`]).
    /// Split out so the `OCX_NO_CONFIG` path can run the same checks over the
    /// system candidate alone — that path exists so a locked section survives
    /// the flag, which it cannot do if the candidate is dropped first.
    ///
    /// # Errors
    /// Returns [`Error::SystemConfig`] when the SYSTEM candidate exists but
    /// cannot be consulted.
    async fn existing_candidates(candidates: Vec<PathBuf>) -> std::result::Result<Vec<PathBuf>, Error> {
        // join_all preserves input order, so the precedence semantics of the
        // candidate list (system → user → $OCX_HOME) are unchanged. Running
        // the symlink_metadata calls concurrently shaves two sequential
        // filesystem round-trips on startup. We use `symlink_metadata` rather
        // than `try_exists` (which follows symlinks) so the symlink-rejection
        // branch can observe the link itself without dereferencing it.
        let system = Self::system_path();
        let checks = join_all(candidates.iter().map(tokio::fs::symlink_metadata)).await;
        let mut kept = Vec::with_capacity(candidates.len());
        for (path, result) in candidates.into_iter().zip(checks) {
            // The SYSTEM candidate is the operator's, and skipping it skips
            // every section `apply_system_locks` would have clamped — so for
            // that one path, anything but plain absence is fatal. Compared the
            // same way `load_and_merge` decides to apply the locks, so the two
            // cannot disagree about which file that is.
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
                // Absence stays silent on every tier: no `/etc/ocx/config.toml`
                // is the ordinary case on nearly every host.
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
                // Absolutized against `start` before the walk: `current` is
                // absolute throughout (it comes from `current_dir()` and only
                // ever moves to `.parent()`), and `Path` equality distinguishes
                // an absolute path from a relative one by its root component,
                // so a relative `OCX_CEILING_PATH` could never equal any level
                // the walk produced — the ceiling silently never fired (#380).
                //
                // The join alone is not enough. A ceiling only ever fires at
                // `start` or one of its ancestors, so every useful relative
                // spelling is `..`-prefixed, and `..` is a component `Path`
                // equality keeps rather than folds — `<cwd>/..` is not equal to
                // `<cwd>`'s parent. `lexical_normalize` folds it without
                // touching the disk, which is what keeps `current` (never
                // canonicalized) comparable.
                //
                // `Path::join` with an absolute value replaces, and normalizing
                // a path that carries no `.`/`..` is the identity, so an
                // absolute ceiling behaves exactly as it did. Empty stays the
                // ignored value it already was, matching the `OCX_PROJECT`
                // escape hatch above; joining it would bound the walk at
                // `start` instead.
                let ceiling = crate::env::var("OCX_CEILING_PATH")
                    .filter(|value| !value.is_empty())
                    .map(|value| crate::utility::fs::path::lexical_normalize(&start.join(value)));
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
            let mut parsed: Config =
                Self::parse_config_stripping_refused_consent(&contents, path.display()).map_err(|source| {
                    Error::Parse {
                        path: path.to_path_buf(),
                        source,
                    }
                })?;
            // C7 enforcement — see [`Self::apply_system_locks`] for the full
            // per-section rationale. Only the system file is locked; it folds
            // in first, so locked sections ignore all lower-tier overrides.
            if path == Self::system_path().as_path() {
                Self::apply_system_locks(&mut parsed);
            }
            Self::anchor_relative_paths(&mut parsed, path);
            Self::stamp_shell_tier(&mut parsed, Self::tier_for_path(path));
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
    /// Covers all seven lockable sections: `[patches]` (lock is conditional
    /// inside `PatchConfig::lock_as_system`), `[registry]` (unconditional),
    /// each `[registries.<name>]` entry (unconditional, per name — closes
    /// the indirection `resolved_default_registry` resolves through), each
    /// `[mirrors."<host>"]` entry (per host, per role — `MirrorConfig::lock_as_system`
    /// locks only the `registry`/`index` role(s) that entry actually declares,
    /// `adr_index_indirection.md` F5b), `[managed]`
    /// (required-gated inside `ManagedConfig::lock_as_system`, like
    /// `[patches]` — a system-scope `required = true` seed must not be
    /// loosenable/clearable by the home tier's fence; ADR Decision G,
    /// criterion 13), `[trust]` (unconditional, per `[[trust.policy]]`
    /// entry — a locked policy pins the specificity level for the scopes it
    /// matches, so a lower tier may only join its ANY-of set at equal
    /// specificity, never outbid it with a narrower scope), and `[records]`
    /// (unconditional, and binary per block — a system-scope section clamps
    /// `dir`, `name` and `required` together, which is what makes recording a
    /// fleet property rather than a wrapper-script convention; a system file
    /// with no `[records]` clamps nothing). Extracted so the section coverage
    /// is unit-testable without writing to `/etc`.
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
        if let Some(trust) = parsed.trust.as_mut() {
            trust.lock_as_system();
        }
        if let Some(records) = parsed.records.as_mut() {
            records.lock_as_system();
        }
    }

    /// Which tier `path` belongs to (C-032).
    ///
    /// The three discovered candidates are compared against the accessors that
    /// produced them; anything else reached `load_and_merge` through
    /// `explicit_paths`, which is the only other caller.
    fn tier_for_path(path: &Path) -> crate::config::ConfigTier {
        use crate::config::ConfigTier;

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

    /// Record which tier set `[shell] hook` / `completions` before this file
    /// merges into the accumulator (C-032).
    ///
    /// Runtime provenance, following the shipped `RegistryDefaults::system_locked`
    /// precedent: `#[serde(skip)]` fields the loader sets after parsing, never
    /// read from disk. Stamped only where the file actually set the scalar, so
    /// `ShellConfig::merge` — which carries the tier alongside the value — never
    /// attributes a decision to a tier that stayed silent.
    fn stamp_shell_tier(parsed: &mut Config, tier: crate::config::ConfigTier) {
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

    /// Parse one `config.toml` payload, dropping a **refused**
    /// `[shell.consent]` table rather than failing the whole file with it.
    ///
    /// The two rules this reconciles both hold, and only together:
    ///
    /// - `arch-principles.md`'s fleet forward-compat row — a payload written
    ///   for a newer ocx "must degrade to its known parts, never fail the whole
    ///   file". One `config.toml` is fleet-wide state, so a refusal that takes
    ///   the file down takes `[registries]`, `[mirrors]` and `[[trust.policy]]`
    ///   with it. On a `required = false` managed tier that silently drops the
    ///   operator's trust pins and falls back to the default registry — a
    ///   commit whose subject is a *narrowing* would widen the effective
    ///   posture on every host at once.
    /// - That row's consent-bearing-table carve-out — dropping an unknown
    ///   *narrowing* key widens trust, so `ShellConsent` refuses instead. The
    ///   carve-out is about the direction of the change, not about which file
    ///   dies: dropping the **whole grant** is the narrowest possible outcome
    ///   **only for a table that grants and does not withdraw**. `exclude` is
    ///   the one thing a `[shell.consent]` table says that TAKES a grant away,
    ///   and it accumulates across tiers ([`ShellConsent::merge`]) against a
    ///   predicate of `covered && !excluded` — so dropping it leaves another
    ///   tier's `include` standing and **widens**, which is the one direction
    ///   the carve-out forbids. A table carrying a non-empty
    ///   `namespaces.exclude` therefore keeps the hard failure.
    ///
    /// [`ShellConsent::merge`]: crate::config::shell::ShellConsent::merge
    ///
    /// So the consent half is stripped structurally and the file survives,
    /// exactly as [`Self::guard_managed_shell_consent`] does for an unpinned
    /// source — same shape, same recorded reason, one tier wider. **Every**
    /// tier, not just the managed one: a discovered tier's refusal is a hard
    /// error on every `ocx` invocation on that host, which is the same
    /// fail-the-file outcome with a smaller blast radius, and the carve-out's
    /// reasoning is tier-independent. The signal is not lost — the reason is
    /// logged AND recorded on the payload, where `ocx about` surfaces it and
    /// the reconciler emits it through the eval'd script (A-21), and the
    /// published JSON schema is where typo detection belongs (same row).
    ///
    /// Only the `[shell.consent]` table may be dropped, and only when removing
    /// it is what makes the file parse, the table withdraws nothing, and the
    /// table is one this ocx **refused** rather than one the operator
    /// mistyped ([`Self::consent_table_shape_is_readable`]): anything else
    /// keeps the original error, spans and all.
    ///
    /// # Errors
    ///
    /// The original `toml` error when the payload is unparseable for any reason
    /// other than a refused `[shell.consent]`; when the refused table carries a
    /// non-empty `namespaces.exclude` — dropping a withdrawal widens, so that
    /// file keeps failing; and when the table is merely ill-typed
    /// (`namespaces = 123`), which is the operator's own typo and owes them the
    /// error rather than a warning.
    fn parse_config_stripping_refused_consent(
        text: &str,
        origin: impl std::fmt::Display,
    ) -> std::result::Result<Config, toml::de::Error> {
        let refusal = match toml::from_str::<Config>(text) {
            Ok(parsed) => return Ok(parsed),
            Err(refusal) => refusal,
        };
        // Re-parse through `toml::Value` rather than editing the text: the
        // table is the library's own model of the file, so removing one key
        // cannot mangle a neighbouring section the way string surgery can.
        let Ok(mut table) = toml::from_str::<toml::Table>(text) else {
            return Err(refusal);
        };
        // Dropping a grant narrows; dropping a WITHDRAWAL widens. `exclude` is
        // the only key that takes a grant away, and it accumulates across
        // tiers, so stripping a table that carries one leaves whatever
        // `include` another tier contributed standing unopposed. That is the
        // one direction the carve-out forbids, so such a file keeps the hard
        // failure it had before the strip existed.
        if Self::consent_table_withdraws(&table) {
            return Err(refusal);
        }
        // A refusal is a judgement about consent; a type error is a typo. Only
        // the first earns the strip, and "removing the table fixed the parse"
        // cannot tell them apart — `namespaces = 123` passes that test too.
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
            .get_or_insert_with(crate::config::ShellConfig::default)
            .consent_strip_reason = Some(reason);
        Ok(parsed)
    }

    /// Whether the raw `[shell.consent]` table in `table` **withdraws** a
    /// grant — i.e. carries a non-empty `namespaces.exclude`.
    ///
    /// Read off the raw [`toml::Table`] rather than a parsed [`ShellConsent`],
    /// because the only tables this question is ever asked of are the ones
    /// that failed to parse. One lookup covers every spelling the config
    /// accepts: an inline `namespaces = { include = [...], exclude = [...] }`,
    /// a `[shell.consent.namespaces]` section header, and the dotted-key form
    /// all normalize to the same nested table. The string form
    /// (`namespaces = "ocx.sh/acme"`) carries no `exclude` and is not a table,
    /// so it answers `false` here.
    ///
    /// An `exclude = []` withdraws nothing and does not block the strip.
    /// Anything else present under that key — a populated list, or a value
    /// shape this ocx cannot read at all — counts as a withdrawal: the
    /// unreadable case is precisely a narrowing written by a newer ocx, which
    /// is what the carve-out exists for.
    ///
    /// [`ShellConsent`]: crate::config::shell::ShellConsent
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

    /// Whether the raw `[shell.consent]` table in `table` has the TOML *shape*
    /// [`ShellConsent`] expects — the test that separates a **refusal** from a
    /// plain **type error**.
    ///
    /// Without it the strip's only question is structural — "did removing this
    /// table make the file parse?" — and `namespaces = 123` answers it exactly
    /// as `namespaces = "ocx.sh/*"` does. The first is the operator's own typo
    /// and owes them exit 78; the second is a judgement this ocx made about a
    /// grant, and is what the carve-out exists to survive. Swallowing the typo
    /// hides a `config.toml` mistake behind a warning on a stderr the shims
    /// discard.
    ///
    /// **Shape, never policy.** This deliberately does not re-run
    /// [`validate_consent_pattern`] or re-check `include`'s emptiness or an
    /// unknown key: a second copy of the validator would drift from the real
    /// one, and every one of those refusals is precisely what still *should*
    /// strip. It asks only what serde would answer with `invalid type` — is
    /// `consent` a table, `paths` a list of strings, `namespaces` a string or a
    /// table, and its `include`/`exclude` lists of strings.
    ///
    /// The typed error is unreachable here: the deserializer hands every
    /// refusal to `serde::de::Error::custom`, which erases
    /// [`ConsentPatternError`] into an opaque [`toml::de::Error`] message, and
    /// `ShellConsent`'s `deny_unknown_fields` refusal is serde's own text that
    /// no marker could reach without hand-writing that derive. Reading the
    /// `toml::Value` variants keeps the discriminator type-level anyway, and
    /// out of the error's prose.
    ///
    /// A key added to `ShellConsent` later and not mirrored here reads as
    /// "shape fine" and strips — the fail-closed direction, and the same
    /// outcome `deny_unknown_fields` already gives it.
    ///
    /// [`ShellConsent`]: crate::config::shell::ShellConsent
    /// [`ConsentPatternError`]: crate::config::shell::ConsentPatternError
    /// [`validate_consent_pattern`]: crate::config::shell::validate_consent_pattern
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
            // The table form, in every spelling — inline, section header and
            // dotted key all normalize to this one.
            Some(toml::Value::Table(spec)) => ["include", "exclude"]
                .iter()
                .all(|key| spec.get(*key).is_none_or(is_string_list)),
            Some(_) => false,
        }
    }

    /// Strip `[shell.consent]` from a managed payload whose `[managed] source`
    /// is not digest-pinned (C-034).
    ///
    /// `[shell] hook` and `completions` are left alone deliberately: they merge
    /// unconditionally in both directions, which is safe only because consent
    /// still gates every project independently. `[shell.consent]` is the half
    /// that grants, so it is honoured only behind a pin — otherwise the consent
    /// material arrives over the very channel it exists to authorise, and
    /// whoever can move the tag can swap it. Same rule, same reason, as
    /// [`Self::guard_managed_sigstore_trust`]'s `trusted_root_json`.
    ///
    /// The reason is recorded on the payload as well as logged: `log::warn!`
    /// goes to a stderr the shell shims discard, so the strip would otherwise
    /// be invisible exactly where it matters. `ocx about` surfaces the recorded
    /// reason, and the reconciler emits it through the eval'd script (A-21).
    ///
    /// The gate is managed-tier-only. A file named by `--config` / `OCX_CONFIG`
    /// is a third consent-bearing channel of the same already-out-of-scope
    /// threat class, and has no `[managed] source` for the pin question to be
    /// asked of at all (A-33).
    ///
    /// **Only the grant is stripped.** `paths` and `namespaces.include` grant;
    /// `namespaces.exclude` **withdraws**, and it accumulates across tiers
    /// ([`ConsentScopeSpec::accumulate`]), so dropping one leaves whatever
    /// `include` another tier — or `OCX_CONSENT_NAMESPACES` — contributed
    /// standing unopposed. That is the one direction this gate exists to
    /// forbid, so the carve-outs survive the strip: honouring them needs no
    /// pin, because whoever moved the tag can only ever take a grant *away*
    /// with them.
    ///
    /// [`Self::parse_config_stripping_refused_consent`] answers the same
    /// asymmetry by refusing the file instead, and the two are not in conflict:
    /// there the table failed to *parse*, so there are no trustworthy patterns
    /// left to keep, and a local `config.toml` can fail closed (exit 78)
    /// without consequence for anyone else. Failing this payload closed would
    /// hand whoever can move the tag a fleet-wide denial of service — the
    /// adversary C-034 models, holding every host's `ocx` hostage — which is
    /// why the managed tier degrades instead of refusing.
    ///
    /// [`ConsentScopeSpec::accumulate`]: crate::config::shell::ConsentScopeSpec::accumulate
    fn guard_managed_shell_consent(parsed: &mut Config, source: &crate::oci::Identifier) {
        use crate::config::shell::{ConsentScopeSpec, ShellConsent};
        use crate::trust::ScopeSpec;

        if source.digest().is_some() {
            return;
        }
        let Some(shell) = parsed.shell.as_mut() else {
            return;
        };
        let Some(consent) = shell.consent.take() else {
            return;
        };
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
            // `ScopeSpec::Set` reads an **empty** `include` as a catch-all, so
            // an exclude-only spec would grant every source it does not carve
            // out — a far wider hole than the one being closed. Seeding
            // `include` with the carve-outs themselves makes the two lists
            // identical, so `covered && !excluded` is false for every source
            // whatever the patterns are, and the spec grants nothing on its
            // own; `accumulate` only ever adds, so that stays true while the
            // `exclude` still opposes another tier's `include`.
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

    /// Fold a project-tier contribution into `accumulator`, without its
    /// `[shell]` or `[records]` sections (C-033).
    ///
    /// **The single entry point for any project-tier fold.** `[shell]` carries
    /// consent, and consent read from a repository's own `ocx.toml` would let a
    /// clone consent to itself — so the section is stripped here, structurally,
    /// rather than relied upon to be unparseable one file over. `ProjectConfig`
    /// refuses a `[shell]` block today only through its `deny_unknown_fields`,
    /// whose own docstring calls it a typo detector; a security property must
    /// not rest on a typo detector nobody records the coupling of.
    ///
    /// `[records]` is stripped for the mirrored reason: the sink is the
    /// operator's, and a repository that could name it could redirect an audit
    /// trail into a directory it also controls — or, by naming an unwritable
    /// one under a `required` posture, refuse every launch inside the checkout.
    /// A cloned repository is untrusted input; where records go is not its call.
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
        accumulator.merge(project_contribution);
    }

    /// Resolve every path a config file declares relative to *that file's*
    /// directory, before it merges into the accumulator.
    ///
    /// Runs per tier, so `/etc/ocx/config.toml` and `$OCX_HOME/config.toml`
    /// each anchor their own values and the answer never depends on the
    /// process working directory — a config read by a daemon, a CI runner and
    /// an interactive shell must name the same file.
    ///
    /// Three keys participate today: `[trust.sigstore] trusted_root`, each
    /// `[[trust.policy]]` signer's `file:`-form `key`, and `[records] dir`. Each
    /// names a location the operator chose beside their own config, so all three
    /// anchor the same way and through the same seam — a second anchoring site
    /// is how they would drift into resolving differently. Every other
    /// path-valued key in the tree is either already absolute by contract or
    /// resolved by its own consumer.
    ///
    /// `--records-dir` and `OCX_RECORDS_DIR` stay **CWD-relative** and never
    /// reach here: a flag and an env var are typed by whoever is standing in a
    /// directory, and anchoring those at a config file's directory would resolve
    /// them somewhere the caller cannot see.
    ///
    /// The **project `ocx.toml`** tier does not pass through here. Its trust
    /// policies are read by `trust::policies_from_ocx_toml`, which takes the
    /// project file's directory as a parameter and applies
    /// [`TrustPolicy::anchor_relative_keys`] itself — same rule, one call site
    /// each, neither able to skip it.
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

    /// The `OCX_NO_CONFIG=1` counterpart to [`Self::apply_system_locks`]: keep
    /// only the sections the lock pass actually clamped, drop the rest.
    ///
    /// `OCX_NO_CONFIG=1` means "ignore ambient configuration", not "ignore
    /// operator policy". Pruning the system tier wholesale made an operator
    /// lock defeatable by one environment variable — a CI job setting the flag
    /// for hermeticity dropped out of a SYSTEM-locked `[records]` sink with
    /// exit 0 and no warning, and, since the flag is not forwarded to child
    /// processes, the two frames of one launch chain could resolve two
    /// different `[records]` policies. This is an integrity control, not a
    /// containment one (ADR `adr_exec_resolution_record.md`, "the lock
    /// protects against error, not malice") — whoever sets `OCX_NO_CONFIG`
    /// could equally not run ocx at all. What it buys is that the accident
    /// stops being silent.
    ///
    /// Everything the system file declares WITHOUT a lock is ordinary
    /// configuration and is pruned along with the user and `$OCX_HOME` tiers,
    /// so the flag keeps its hermetic intent. Explicit tiers (`OCX_CONFIG`,
    /// `--config`) are unaffected: they load as before and merge on top, where
    /// a locked section still ignores them.
    ///
    /// `[managed]` is dropped even when locked. `OCX_NO_CONFIG` suppresses the
    /// snapshot read too ([`Self::managed_snapshot_candidate`]), so a retained
    /// seed could never be satisfied: a `required` tier — the default — would
    /// fail every hermetic invocation rather than enforce anything. The tier
    /// stays fully suppressed, exactly as before this filter existed.
    fn retain_system_locked_sections(config: &mut Config) {
        // Exhaustive destructure on purpose: a section added to `Config` cannot
        // reach the SYSTEM tier without a decision here, the same coverage
        // guarantee `apply_system_locks_covers_every_lockable_section` gives the
        // lock pass — except enforced by the compiler rather than by a test.
        let Config {
            registry,
            registries,
            mirrors,
            patches,
            managed,
            trust,
            shell,
            records,
        } = config;

        *patches = patches.take().filter(|patches| patches.system_locked);
        *registry = registry.take().filter(|registry| registry.system_locked);
        *records = records.take().filter(|records| records.system_locked);
        *managed = None;
        // `[shell]` is the one section `apply_system_locks` clamps nothing in,
        // so nothing in it survives: the hook/completions toggles and the
        // activation consent whitelist are ambient host configuration, and the
        // flag prunes them with the user and `$OCX_HOME` tiers.
        *shell = None;
        if let Some(entries) = registries.as_mut() {
            entries.retain(|_, entry| entry.system_locked);
        }
        *registries = registries.take().filter(|entries| !entries.is_empty());
        if let Some(entries) = mirrors.as_mut() {
            // Per-role lock: `MirrorConfig::lock_as_system` locks every role the
            // entry declares, so an entry with neither role locked declares
            // nothing that survives.
            entries.retain(|_, mirror| mirror.registry_system_locked || mirror.index_system_locked);
        }
        *mirrors = mirrors.take().filter(|entries| !entries.is_empty());
        // `[trust]` locks per `[[trust.policy]]` entry rather than per section,
        // because policies array-append across tiers and the section itself does
        // not survive the fold; `[trust.sigstore]` locks as a whole table.
        if let Some(declared) = trust.as_mut() {
            declared.policy.retain(|policy| policy.system_locked);
            declared.sigstore = declared.sigstore.take().filter(|sigstore| sigstore.system_locked);
        }
        *trust = trust
            .take()
            .filter(|declared| !declared.policy.is_empty() || declared.sigstore.is_some());
    }

    /// System config: `/etc/ocx/config.toml`.
    ///
    /// Redirectable through [`SYSTEM_CONFIG_OVERRIDE`] in test builds only —
    /// the SYSTEM tier is the one tier no test can write to, and every
    /// system-lock behaviour would otherwise be unreachable end to end.
    pub fn system_path() -> PathBuf {
        #[cfg(any(test, feature = "__testing"))]
        if let Some(path) = crate::env::var(SYSTEM_CONFIG_OVERRIDE) {
            return PathBuf::from(path);
        }
        PathBuf::from("/etc/ocx/config.toml")
    }

    /// User config: `$XDG_CONFIG_HOME/ocx/config.toml` or
    /// `~/.config/ocx/config.toml` (via `dirs::config_dir`).
    pub fn user_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("ocx").join("config.toml"))
    }

    /// `$OCX_HOME/config.toml`, falling back to `~/.ocx/config.toml`.
    ///
    /// The directory comes from [`crate::file_structure::default_ocx_root`],
    /// the one definition of the `$OCX_HOME` default — this module used to
    /// carry a second one that resolved the fallback home through a different
    /// API and could name a different directory (#381).
    ///
    /// `None` when `OCX_HOME` is unset and no home directory resolves (e.g. a
    /// service account with no `$HOME`).
    ///
    /// Callers compose well-known children through one of the named
    /// `home_*_path()` accessors here so `$OCX_HOME` path math stays in this
    /// module; code needing a new `$OCX_HOME`-rooted path should add one
    /// rather than join onto the bare directory.
    pub fn home_path() -> Option<PathBuf> {
        crate::file_structure::default_ocx_root().map(|d| d.join("config.toml"))
    }

    /// `$OCX_HOME/sigstore/trusted-root.json`, falling back to
    /// `~/.ocx/sigstore/trusted-root.json`.
    ///
    /// The convention path in the trust-root ladder: drop the file there and
    /// verification finds it with no flag, no env var and no config entry.
    /// Deliberately NOT under `state/` — that subtree is TTL-bound runtime
    /// state the tool writes and may discard, whereas this is a durable
    /// operator-supplied asset nothing but the operator removes.
    pub fn home_sigstore_trusted_root_path() -> Option<PathBuf> {
        crate::file_structure::default_ocx_root().map(|d| d.join("sigstore").join("trusted-root.json"))
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

    /// Point the SYSTEM tier at a path that does not exist.
    ///
    /// `OCX_NO_CONFIG=1` no longer prunes the system tier (its locked sections
    /// survive the flag), so a developer machine that happens to carry a real
    /// `/etc/ocx/config.toml` would otherwise leak into every hermetic-mode
    /// assertion below. Same rationale as `EnvLock::isolate_project_home`.
    fn without_system_config(env: &crate::test::env::EnvLock) {
        env.set(SYSTEM_CONFIG_OVERRIDE, "/nonexistent/ocx-test-system/config.toml");
    }

    /// Point the SYSTEM tier at `content` written into `dir`, so the
    /// `lock_as_system` pass runs against it exactly as it does for
    /// `/etc/ocx/config.toml`.
    fn with_system_config(env: &crate::test::env::EnvLock, dir: &TempDir, content: &str) {
        let path = write_config(dir, "system-config.toml", content);
        env.set(SYSTEM_CONFIG_OVERRIDE, path.to_str().expect("temp path is utf-8"));
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
    // Env-touching tests acquire `crate::test::env::lock()` — a process-wide
    // mutex whose Drop clears all overrides. Overrides route through
    // `crate::env::var`'s `#[cfg(test)]` branch; no `std::env::set_var`, no
    // `unsafe`.

    #[tokio::test]
    async fn load_with_no_config_returns_default() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        without_system_config(&env);
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
        without_system_config(&env);
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
        without_system_config(&env);
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
        without_system_config(&env);
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
        without_system_config(&env);
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
        without_system_config(&env);
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
        without_system_config(&env);
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
        without_system_config(&env);
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

    // ── Built-in base tier ───────────────────────────────────────────────────
    //
    // The compiled-in tier lives below every file tier, so `OCX_NO_CONFIG=1`
    // (which prunes the discovered chain and the managed snapshot, both
    // ambient host state) is the sharpest way to observe it alone.

    fn builtin_ocx_sh_index(config: &Config) -> Option<&str> {
        config
            .registries
            .as_ref()?
            .get(crate::oci::OCX_SH_REGISTRY)?
            .index
            .as_deref()
    }

    #[tokio::test]
    async fn builtin_tier_makes_ocx_sh_index_bearing() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs).await.expect("load should succeed");
        assert_eq!(
            builtin_ocx_sh_index(&config),
            Some(crate::oci::index::DEFAULT_INDEX_BASE_URL),
            "the compiled-in tier must make ocx.sh index-bearing with no config file at all"
        );
    }

    #[tokio::test]
    async fn builtin_tier_leaves_every_other_namespace_plain_oci() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
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
            vec![crate::oci::OCX_SH_REGISTRY],
            "the built-in tier must seed ocx.sh and nothing else"
        );
    }

    #[tokio::test]
    async fn user_index_overrides_the_builtin_ocx_sh_index() {
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "corp.toml",
            "[registries.\"ocx.sh\"]\nindex = \"https://index.corp.example\"\n",
        );
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", path.to_str().unwrap());
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "plain.toml", "[registries.\"ocx.sh\"]\nindex = \"\"\n");
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", path.to_str().unwrap());
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "trusted.toml",
            "[registries.\"ocx.sh\"]\ntrusted_hosts = [\"registry.corp\"]\n",
        );
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", path.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs).await.expect("load should succeed");
        assert_eq!(
            builtin_ocx_sh_index(&config),
            Some(crate::oci::index::DEFAULT_INDEX_BASE_URL)
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        // `$OCX_HOME/config.toml` is the one discovered tier a test can plant
        // (system and user paths are host-absolute).
        std::fs::write(
            dir.path().join("config.toml"),
            "[registries.\"ocx.sh\"]\nindex = \"https://index.corp.example\"\n",
        )
        .unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");
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

        let resolved = crate::trust::resolve(accumulator.trust_policies(), "ghcr.io/acme/tool");
        assert_eq!(
            resolved.len(),
            1,
            "a narrower lower-tier scope must not outbid the system pin after the fold"
        );
        assert_eq!(
            resolved[0].signers.iter().find_map(|signer| match signer {
                crate::trust::SignerSpec::Keyless(keyless) => keyless.identity.as_deref(),
                crate::trust::SignerSpec::Key(_) | crate::trust::SignerSpec::Unknown => None,
            }),
            Some("system-ci")
        );
    }

    /// `local_only` is cloned from the same accumulator as `merged`, so the
    /// built-in tier must show up in both views — the managed-tier fetch
    /// builds its client from `local_only`.
    #[tokio::test]
    async fn builtin_tier_is_present_in_both_loaded_views() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
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
            Some(crate::oci::index::DEFAULT_INDEX_BASE_URL)
        );
        assert_eq!(
            builtin_ocx_sh_index(&loaded.local_only),
            Some(crate::oci::index::DEFAULT_INDEX_BASE_URL)
        );
    }

    // ── Path resolver tests ──────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn system_path_is_etc_ocx_config_toml() {
        // Holds the env lock so a concurrently-running test cannot have the
        // `SYSTEM_CONFIG_OVERRIDE` seam set while this asserts the real path.
        let env = crate::test::env::lock();
        env.remove(SYSTEM_CONFIG_OVERRIDE);
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
        without_system_config(&env);
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");

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
        without_system_config(&env);
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");

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
    fn assert_system_config_error(error: &crate::Error, path: &Path) {
        use crate::cli::{ClassifyExitCode, ExitCode};

        assert!(
            matches!(error, crate::Error::Config(Error::SystemConfig { path: named, .. }) if named == path),
            "expected a fatal system-config error naming {}, got: {error:?}",
            path.display()
        );
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_system_config_is_fatal() {
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let target = write_config(&dir, "fleet.toml", "[records]\ndir = \"/var/log/ocx/records\"\n");
        let link = dir.path().join("system-config.toml");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");
        env.set(SYSTEM_CONFIG_OVERRIDE, link.to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");

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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let target = write_config(&dir, "fleet.toml", "[records]\ndir = \"/var/log/ocx/records\"\n");
        let link = dir.path().join("system-config.toml");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");
        env.set(SYSTEM_CONFIG_OVERRIDE, link.to_str().unwrap());
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");

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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let not_a_dir = write_config(&dir, "not-a-dir", "");
        let candidate = not_a_dir.join("config.toml");
        env.set(SYSTEM_CONFIG_OVERRIDE, candidate.to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");

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
        let env = crate::test::env::lock();
        without_system_config(&env);
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");

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
        let env = crate::test::env::lock();
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
    ///
    /// EC-FS-010, half one of two: the linkfile. The symlink half is
    /// [`project_path_walk_stops_at_a_symlinked_git_entry`].
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

        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        // The decoy the walk must never reach: without a boundary at the
        // checkout root, the ascent finds this and adopts it.
        write_file(&dir.path().join("ocx.toml"), "");
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

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
    ///
    /// EC-FS-013 — this **is** the filesystem-root termination case, and A-11
    /// closes the row by asserting it rather than adding code: with no boundary
    /// and no ceiling the ascent runs out of ancestors, and
    /// [`ConfigLoader::walk_for_project_file`]'s `current.parent()` arm returns
    /// `None` on its own. The bounded timeout is the assertion that matters —
    /// an off-by-one on that arm (re-visiting the root, or ascending into a
    /// path that never shortens) hangs rather than failing, and a hang inside a
    /// per-prompt hook is the shipped bug this guards.
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

    /// EC-FS-013 — starting the walk **at** the filesystem root exercises the
    /// `current.parent() == None` arm on the very first iteration, with no
    /// dependence on what happens to sit between a temp directory and `/`.
    ///
    /// The sibling test above reaches the same arm by ascending, but only on a
    /// host where no ancestor of `TMPDIR` carries a `.git` or an `ocx.toml`;
    /// this one cannot be short-circuited by either. Together they pin A-11's
    /// "the walk's termination at the filesystem root needs no special case —
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

    /// C-001/S-001 (#380): a **relative** `OCX_CEILING_PATH` bounds the walk
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
        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        // ocx.toml above the ceiling — the file an unbounded walk would find.
        write_file(&dir.path().join("ocx.toml"), "");
        let ceiling = dir.path().join("ceiling");
        std::fs::create_dir(&ceiling).unwrap();
        let cwd = ceiling.join("project");
        std::fs::create_dir(&cwd).unwrap();

        // Written relative to `cwd`, the directory the walk starts from.
        env.set("OCX_CEILING_PATH", "..");

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("ceiling-bounded walk should resolve, not error");
        assert_eq!(
            resolved, None,
            "a relative OCX_CEILING_PATH must bound the walk, not be silently ignored"
        );
    }

    /// C-001 (#380): the empty value stays the ignored one it already was.
    ///
    /// Joining it would make the ceiling equal `start` and stop the walk at
    /// cwd — turning the escape hatch every other path-valued `OCX_*` variable
    /// spells the same way into a bound nobody asked for.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_empty_ceiling_does_not_bound_the_walk() {
        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        let outer_project = dir.path().join("ocx.toml");
        write_file(&outer_project, "");
        let cwd = dir.path().join("nested");
        std::fs::create_dir(&cwd).unwrap();

        env.set("OCX_CEILING_PATH", "");

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
        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        let ceiling = dir.path().join("workspace");
        std::fs::create_dir(&ceiling).unwrap();
        let project = ceiling.join("ocx.toml");
        write_file(&project, "");
        env.set("OCX_CEILING_PATH", ceiling.to_str().unwrap());
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

    /// EC-FS-014 — a directory chain at the OS path limit degrades to
    /// "boundary reached", never a raw `ENAMETOOLONG` on the per-prompt path.
    ///
    /// A-11 overrules the register's framing here: this is a test-and-document
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
        let env = crate::test::env::lock();
        let _ocx_home = env.isolate_project_home();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("r");
        std::fs::create_dir(&root).unwrap();
        // The decoy: reachable only if the over-limit level fails to stop the
        // ascent.
        let decoy = root.join("ocx.toml");
        write_file(&decoy, "");
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

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
        without_system_config(&env);
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

        let (folded, _snapshot, _resolved, _state) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
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

        let (folded, snapshot, _resolved, _state) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
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
    /// [`ManagedSnapshotState::PayloadUnusable`](crate::config::managed::ManagedSnapshotState::PayloadUnusable),
    /// so `Context::try_init` can fail a `required` tier closed on it. The
    /// state — not the returned snapshot, which is still `Some` for
    /// `config update --check` — is what the gate reads.
    #[tokio::test]
    async fn managed_snapshot_unparseable_payload_folds_nothing_and_reports_unusable() {
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

        write_managed_snapshot(dir.path(), "corp.example.com/ocx-config:user", "not = [valid toml");

        let accumulator = crate::config::Config {
            managed: Some(crate::config::managed::ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..Default::default()
            }),
            ..crate::config::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, snapshot, _resolved, state) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
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
            crate::config::managed::ManagedSnapshotState::PayloadUnusable,
            "the loader must report what it actually applied, not merely that the identity matched"
        );
    }

    /// The discriminator for the test above: a payload carrying sections and
    /// keys this binary does not know still folds, and its known settings
    /// reach the merged config. `PayloadUnusable` must mean "broken", never
    /// "unfamiliar" — the latter would fail a fleet closed on every rollout.
    #[tokio::test]
    async fn managed_snapshot_payload_from_a_newer_ocx_folds_and_reports_applied() {
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

        write_managed_snapshot(
            dir.path(),
            "corp.example.com/ocx-config:user",
            "[registry]\ndefault = \"corp-registry.example\"\ntimeout = 30\n[toolchain]\nchannel = \"stable\"\n",
        );

        let accumulator = crate::config::Config {
            managed: Some(crate::config::managed::ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..Default::default()
            }),
            ..crate::config::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved, state) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
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
        assert_eq!(state, crate::config::managed::ManagedSnapshotState::Applied);
    }

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
            "[registries.corp]\nindex = \"https://registry.corp.example\"\n",
            "[mirrors]\n\"docker.io\" = \"https://mirror.corp.example\"\n",
            "[managed]\nsource = \"corp/managed-config:stable\"\nrequired = true\n",
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n",
            "signers = [{ kind = \"keyless\", identity = \"ci@acme.example\", oidc_issuer = \"https://iss.example\" }]\n",
            "[records]\ndir = \"/var/log/ocx/records\"\nrequired = true\n",
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

    /// The published guarantee (`reference/execution-records.md`): "no caller
    /// can opt out of a sink the operator has locked at system scope."
    /// `OCX_NO_CONFIG=1` is a caller, and used to be the one way out.
    #[tokio::test]
    async fn no_config_keeps_a_system_locked_records_policy() {
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        env.set("OCX_HOME", home.path().to_str().unwrap());
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
        let sink = absolute_sink("var/log/ocx/records");
        with_system_config(
            &env,
            &dir,
            &format!(
                "[records]\ndir = \"{}\"\nname = \"{{time}}.json\"\nrequired = true\n",
                sink.display()
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        env.set("OCX_HOME", home.path().to_str().unwrap());
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
        let sink = absolute_sink("var/log/ocx/records");
        with_system_config(
            &env,
            &dir,
            &format!("[records]\ndir = \"{}\"\nrequired = true\n", sink.display()),
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
        let env = crate::test::env::lock();
        let home = TempDir::new().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[records]\ndir = \"/tmp/home-tier-records\"\n",
        )
        .unwrap();
        env.set("OCX_HOME", home.path().to_str().unwrap());
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        env.set("OCX_HOME", home.path().to_str().unwrap());
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        env.set("OCX_HOME", home.path().to_str().unwrap());
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");
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
        let mut config: crate::config::Config = toml::from_str(concat!(
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

        ConfigLoader::apply_system_locks(&mut config);
        ConfigLoader::retain_system_locked_sections(&mut config);

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
    }

    /// The filter is lock-driven, not section-driven: the same sections parsed
    /// from a NON-system file (no lock pass) are all pruned.
    #[test]
    fn retain_system_locked_sections_drops_everything_unlocked() {
        let mut config: crate::config::Config = toml::from_str(concat!(
            "[patches]\nregistry = \"patches.corp.example\"\nrequired = true\n",
            "[registry]\ndefault = \"corp\"\n",
            "[registries.corp]\nindex = \"https://registry.corp.example\"\n",
            "[mirrors]\n\"docker.io\" = \"https://mirror.corp.example\"\n",
            "[records]\ndir = \"/var/log/ocx/records\"\nrequired = true\n",
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n",
            "signers = [{ kind = \"keyless\", identity = \"ci@acme.example\", oidc_issuer = \"https://iss.example\" }]\n",
        ))
        .unwrap();

        ConfigLoader::retain_system_locked_sections(&mut config);

        assert!(config.patches.is_none());
        assert!(config.registry.is_none());
        assert!(config.registries.is_none(), "an emptied table collapses to None");
        assert!(config.mirrors.is_none(), "an emptied table collapses to None");
        assert!(config.records.is_none());
        assert!(config.trust.is_none(), "an emptied policy list collapses to None");
    }

    /// The other half of the `[records]` clamp: a SYSTEM file that declares no
    /// `[records]` section locks nothing, so an operator who never opted in
    /// does not silently freeze every lower tier out of configuring a sink.
    #[test]
    fn apply_system_locks_leaves_absent_records_section_unlocked() {
        let mut config: crate::config::Config = toml::from_str("[registry]\ndefault = \"corp\"\n").unwrap();

        ConfigLoader::apply_system_locks(&mut config);

        assert!(
            config.records.is_none(),
            "a system file with no [records] must not synthesize a locked section"
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[registries.corp]\nindex = \"https://malicious-index.example\"\n",
        );

        // Simulate an accumulator that already folded a locked SYSTEM tier:
        // the `[registries.corp]` entry is system-locked (in production via
        // `/etc/ocx/config.toml`'s `lock_as_system` branch).
        let mut corp_entry = crate::config::RegistryConfig {
            index: Some("https://system-locked-index.example".to_string()),
            ..Default::default()
        };
        corp_entry.lock_as_system();
        let mut registries = std::collections::HashMap::new();
        registries.insert("corp".to_string(), corp_entry);
        let accumulator = crate::config::Config {
            registries: Some(registries),
            managed: Some(crate::config::managed::ManagedConfig {
                source: Some("registry.test/managed-config:v1".to_string()),
                required: Some(false),
                ..Default::default()
            }),
            ..crate::config::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved, _state) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
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
            &format!("[records]\ndir = \"{}\"\n", spelled_out.display()),
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
        let crate::trust::SignerSpec::Key(matcher) = &config.trust_policies()[0].signers[0] else {
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
        let crate::trust::SignerSpec::Key(by_path) = &policies[0].signers[0] else {
            panic!("still a key signer");
        };
        assert_eq!(by_path.key.as_deref(), Some(absolute.display().to_string().as_str()));
        let crate::trust::SignerSpec::Key(inline) = &policies[1].signers[0] else {
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

    fn managed_payload_after_guard(payload: &str, source: &str) -> crate::trust::SigstoreTrust {
        let mut parsed: Config = toml::from_str(payload).expect("payload parses");
        let source: crate::oci::Identifier = source.parse().expect("identifier parses");
        ConfigLoader::guard_managed_sigstore_trust(&mut parsed, &source);
        parsed.trust.expect("trust").sigstore.expect("sigstore")
    }

    /// Any well-formed SPKI PEM; the guard strips by *spelling*, never by
    /// whether the material parses, so a real key would prove nothing extra.
    const INLINE_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA\n-----END PUBLIC KEY-----\n";

    fn managed_trust_after_guard(payload: &str, source: &str) -> crate::trust::TrustConfig {
        let mut parsed: Config = toml::from_str(payload).expect("payload parses");
        let source: crate::oci::Identifier = source.parse().expect("identifier parses");
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
            matches!(&trust.policy[0].signers[0], crate::trust::SignerSpec::Unknown),
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
        assert!(matches!(compiled.backends[0], crate::trust::PolicyBackend::Keyless(_)));
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
        let crate::trust::SignerSpec::Key(matcher) = &trust.policy[0].signers[0] else {
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

    // ── C-033 — the project tier can never contribute `[shell]` ─────────────

    /// S-035(b), C-033, EC-CFG-002: a project-tier contribution carrying
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

    /// S-035(a), C-033, EC-CFG-001: `[shell]` written into an `ocx.toml` is a
    /// hard parse error, so the section can never reach `Config` through the
    /// project file at all — this pins the `deny_unknown_fields` door on
    /// `ProjectConfig`; `project::config`'s own
    /// `shell_section_in_ocx_toml_is_refused_by_name` pins the named-refusal
    /// door on the two-pass load path.
    ///
    /// This records the coupling C-033 flags as unrecorded: `ProjectConfig`'s
    /// `#[serde(deny_unknown_fields)]` is documented as a typo detector, and a
    /// security property in a different file rests on it. The strip above is
    /// what makes the property structural; this pins the second door shut.
    #[test]
    fn c033_shell_in_a_project_file_is_refused() {
        use crate::cli::ClassifyExitCode;

        let refused = toml::from_str::<crate::project::ProjectConfig>("[shell]\nhook = true\n");
        assert!(
            refused.is_err(),
            "[shell] in an ocx.toml must be refused; shell integration is configured in config.toml"
        );
        assert_eq!(
            crate::config::error::Error::Parse {
                path: PathBuf::from("ocx.toml"),
                source: refused.expect_err("refused"),
            }
            .classify(),
            Some(crate::cli::ExitCode::ConfigError),
            "a refused project file is a config error (78), the code scripts already case on"
        );
    }

    // ── C-034 / A-32 / A-33 — the managed tier's `[shell]` ──────────────────

    fn managed_shell_after_guard(payload: &str, source: &str) -> Option<crate::config::ShellConfig> {
        let mut parsed: Config = toml::from_str(payload).expect("payload parses");
        let source: crate::oci::Identifier = source.parse().expect("identifier parses");
        ConfigLoader::guard_managed_shell_consent(&mut parsed, &source);
        parsed.shell
    }

    const PINNED_SOURCE: &str =
        "ghcr.io/acme/config@sha256:1111111111111111111111111111111111111111111111111111111111111111";

    /// C-034, S-036, EC-CFG-003(a) — the red half: an unpinned `[managed]
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

    /// C-034, S-036, EC-CFG-003(b) — the green half: a digest-pinned source
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

    /// C-034 + C-032 — the polarity the two tests above cannot see, because
    /// both carry a pure **grant**. `exclude` is the only key that takes a
    /// grant away and it accumulates across tiers, so stripping it leaves an
    /// `include` contributed by another tier (here `OCX_CONSENT_NAMESPACES`,
    /// via the same `ShellConsent::merge` `effective_consent` performs)
    /// standing unopposed — a **widening**, which is the one direction the
    /// C-034 strip exists to forbid. The honest operator who wrote the
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
        effective.merge(crate::config::shell::env_channel(None, Some("ocx.sh/acme,ocx.sh/bad")));
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

    // ── ocx-sh/ocx#344 — a refused consent table is dropped, not fatal ──────

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
        crate::config::shell::ConsentPatternError::WholeRegistry("ocx.sh/*".to_string()).to_string()
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

    /// ocx-sh/ocx#344, `arch-principles.md` fleet forward-compat: a refused
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

    /// ocx-sh/ocx#344, `arch-principles.md`'s consent carve-out: the strip
    /// drops a **grant**, never a **withdrawal**.
    ///
    /// `exclude` is the only thing a `[shell.consent]` table says that TAKES a
    /// grant away, and [`ShellConsent::merge`](crate::config::shell::ShellConsent::merge)
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

    /// Arm 2, ocx-sh/ocx#344: an ordinary **type error** inside
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
    /// [`ManagedSnapshotState::Applied`](crate::config::managed::ManagedSnapshotState::Applied).
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

        // Digest-pinned, so `guard_managed_shell_consent` is not what removes
        // the table — the refusal strip is.
        let source = format!("corp.example.com/ocx-config@sha256:{}", "a".repeat(64));
        write_managed_snapshot(dir.path(), &source, REFUSED_CONSENT_PAYLOAD);

        let accumulator = crate::config::Config {
            managed: Some(crate::config::managed::ManagedConfig {
                source: Some(source),
                ..Default::default()
            }),
            ..crate::config::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved, state) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
            .await
            .expect("fold must succeed");

        assert_eq!(
            state,
            crate::config::managed::ManagedSnapshotState::Applied,
            "a payload whose only fault is a refused consent grant is usable; PayloadUnusable means BROKEN"
        );
        assert_consent_stripped_and_siblings_survive(&folded);
    }

    /// A-33: the digest gate is managed-tier-only. An explicit `--config` /
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

    /// A-32, EC-CFG-006: `--config` / `OCX_CONFIG` outranks the managed tier —
    /// including a digest-pinned one — because the loader folds the managed
    /// tier first and the overlay on top of it.
    ///
    /// Red state: fold the overlay above the managed tier in
    /// [`ConfigLoader::load_with_local_view`] and both assertions flip.
    #[test]
    fn a32_the_explicit_tier_outranks_the_managed_tier_and_records_that() {
        use crate::config::ConfigTier;

        // The shipped order, reproduced: base (discovered) -> managed -> overlay.
        let mut base: Config = toml::from_str("[shell]\nhook = false\n").expect("base parses");
        ConfigLoader::stamp_shell_tier(&mut base, ConfigTier::Home);

        let mut managed: Config = toml::from_str("[shell]\nhook = true\n").expect("managed parses");
        ConfigLoader::guard_managed_shell_consent(
            &mut managed,
            &PINNED_SOURCE.parse::<crate::oci::Identifier>().expect("identifier"),
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
    async fn consent_after_load() -> crate::config::shell::ShellConsent {
        let loaded = ConfigLoader::load_with_local_view(ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        })
        .await
        .expect("load must succeed");
        crate::config::shell::effective_consent(loaded.merged.shell.as_ref())
    }

    /// C-034, EC-CFG-005: a managed `[shell] hook = true` beats the home tier's
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
        use crate::config::ConfigTier;

        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

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
        env.set("OCX_CONFIG", explicit.to_str().unwrap());
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

    /// A-33, EC-CFG-007: `OCX_CONFIG` is a third consent-bearing channel, and
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");
        env.remove(crate::config::shell::OCX_CONSENT_PATHS);
        env.remove(crate::config::shell::OCX_CONSENT_NAMESPACES);

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
        env.set("OCX_CONFIG", explicit.to_str().unwrap());

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

        let consent = crate::config::shell::effective_consent(Some(shell));
        assert_eq!(
            consent.paths,
            vec![PathBuf::from("/explicit/grant")],
            "the OCX_CONFIG grant activates and the unpinned managed grant does not — the gate is managed-tier-only, \
             and an explicit file names a local path the user chose"
        );
    }

    /// A-33, EC-CFG-008: `OCX_NO_CONFIG=1` prunes every config-tier grant and
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");
        env.remove(crate::config::shell::OCX_CONSENT_PATHS);
        env.remove(crate::config::shell::OCX_CONSENT_NAMESPACES);
        std::fs::write(
            dir.path().join("config.toml"),
            "[shell.consent]\npaths = [\"/home/grant\"]\n",
        )
        .unwrap();

        env.remove("OCX_NO_CONFIG");
        assert_eq!(
            consent_after_load().await.paths,
            vec![PathBuf::from("/home/grant")],
            "premise: without the flag the home tier's entry really is a grant"
        );

        env.set("OCX_NO_CONFIG", "1");
        assert!(
            consent_after_load().await.paths.is_empty(),
            "OCX_NO_CONFIG=1 prunes the discovered chain, so every config-tier grant goes with it"
        );

        env.set(crate::config::shell::OCX_CONSENT_PATHS, "/env/grant");
        assert_eq!(
            consent_after_load().await.paths,
            vec![PathBuf::from("/env/grant")],
            "OCX_NO_CONFIG touches neither the explicit tiers nor OCX_CONSENT_*; only OCX_NO_HOOK makes a shell wholly \
             inert"
        );
    }

    /// C-032: the loader stamps the tier a file belongs to, and only where that
    /// file set the scalar.
    #[tokio::test]
    async fn c032_the_loader_stamps_the_tier_that_set_each_scalar() {
        use crate::config::ConfigTier;

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

    // ── A-13 — the recorded config-tier paths ───────────────────────────────

    /// A-13: the watch set stats a list the loader recorded, and it must
    /// include tier files that do NOT exist — a grant added by creating one is
    /// exactly the change an `inert` cache has to expire on.
    #[tokio::test]
    async fn a13_records_every_config_tier_candidate_including_absent_ones() {
        // The env lock, and a SYSTEM candidate this test names itself: the
        // loader reads `SYSTEM_CONFIG_OVERRIDE`, `OCX_CONFIG` and
        // `OCX_NO_CONFIG` from the ambient environment, so without both it
        // could observe a sibling's fixture mid-run — the symlinked system
        // config two tests over turns this into a fatal load, intermittently.
        let env = crate::test::env::lock();
        let dir = TempDir::new().expect("tempdir");
        without_system_config(&env);
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
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

    /// A-13 under `OCX_NO_CONFIG=1`: the recorded list narrows to what the flag
    /// still reads. The system tier stays — it loads for its locked sections, so
    /// an operator adding one changes the resolved config — while the user and
    /// `$OCX_HOME` tiers, which the flag genuinely prunes, drop out.
    #[tokio::test]
    async fn a13_under_no_config_records_the_system_tier_and_nothing_below_it() {
        let env = crate::test::env::lock();
        let dir = TempDir::new().expect("tempdir");
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[records]\ndir = \"/tmp/attacker-sink\"\nrequired = false\n",
        );

        // Simulate an accumulator that already folded a locked SYSTEM tier
        // (in production: `/etc/ocx/config.toml` via `load_and_merge`'s
        // `apply_system_locks` branch) plus a home tier whose `[managed].source`
        // matches the snapshot above.
        let mut records = crate::record::RecordsOptions {
            dir: Some(PathBuf::from("/var/log/ocx/records")),
            required: Some(true),
            ..Default::default()
        };
        records.lock_as_system();
        let accumulator = crate::config::Config {
            records: Some(records),
            managed: Some(crate::config::managed::ManagedConfig {
                source: Some("registry.test/managed-config:v1".to_string()),
                required: Some(false),
                ..Default::default()
            }),
            ..crate::config::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved, _state) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
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
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");
        env.remove("OCX_MANAGED_CONFIG");

        write_managed_snapshot(
            dir.path(),
            "registry.test/managed-config:v1",
            "[records]\ndir = \"/var/log/ocx/fleet\"\n",
        );

        let accumulator = crate::config::Config {
            records: Some(crate::record::RecordsOptions {
                dir: Some(PathBuf::from("/home/dev/records")),
                required: Some(true),
                ..Default::default()
            }),
            managed: Some(crate::config::managed::ManagedConfig {
                source: Some("registry.test/managed-config:v1".to_string()),
                required: Some(false),
                ..Default::default()
            }),
            ..crate::config::Config::default()
        };
        let local_only = accumulator.clone();

        let (folded, _snapshot, _resolved, _state) = ConfigLoader::fold_managed_tier(accumulator, &local_only)
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
}
