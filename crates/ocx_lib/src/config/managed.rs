// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Corporate-managed configuration tier (`[managed]`).
//!
//! Home for all `[managed]` settings. Structural twin of
//! [`PatchConfig`](crate::config::patch::PatchConfig): the seed pointer lives in
//! `$OCX_HOME/config.toml`, resolving to a plain `config.toml` payload published
//! as an OCI artifact (domain layer: [`crate::managed_config`]) and merged above
//! the user config every invocation.
//!
//! See `adr_managed_config_tier.md` §"Component contracts" #1 and
//! §"Decision E — required enforcement".

use serde::{Deserialize, Serialize};

/// Configuration for the `[managed]` tier.
///
/// All fields are `Option` so [`Default`] and tier merge work correctly; absent
/// fields fall back to their defaults in [`resolve_managed_config`].
///
/// Like [`PatchConfig`](crate::config::patch::PatchConfig), unknown fields
/// are tolerated (no `deny_unknown_fields`) — fleet forward-compat: a config
/// written for a newer ocx (e.g. a future `[managed]` key) must not brick
/// every older binary in the fleet that reads the same seed. The cost is that
/// a typo'd key silently no-ops; `ocx config update --check` surfaces the
/// tier's effective state for diagnosis.
#[derive(Debug, Default, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ManagedConfig {
    /// OCI reference for the managed-config artifact, e.g.
    /// `"internal.company.com/ocx-config:user"`.
    ///
    /// Required at resolve time; absent → [`resolve_managed_config`] returns
    /// `None` (no managed tier configured).
    pub source: Option<String>,

    /// Fail posture when the snapshot is required but absent.
    ///
    /// - `true` (default) — fail closed: [`ManagedConfigError::SnapshotRequired`]
    ///   until `ocx config update` (or `self setup --managed-config`) syncs one.
    /// - `false` — the tier contributes nothing until synced; a throttle-gated
    ///   stderr hint only (benign-state rule, no per-invocation WARN).
    pub required: Option<bool>,

    /// Background refresh posture. Defaults to [`RefreshPolicy::Notify`].
    pub refresh: Option<RefreshPolicy>,

    /// Background refresh throttle interval, `\d+[smhd]?` (bare = seconds).
    /// Defaults to [`ManagedConfig::DEFAULT_INTERVAL`] (`"1d"`).
    pub interval: Option<String>,

    /// Runtime provenance marker: this tier was declared at the SYSTEM config
    /// scope (`/etc/ocx/config.toml`) AND declared `required = true`, so it is
    /// NON-OVERRIDABLE by any lower tier (mirrors
    /// [`PatchConfig::system_locked`](crate::config::patch::PatchConfig::system_locked)).
    ///
    /// Never serialized (read side) — set by the loader via
    /// [`Self::lock_as_system`] after parsing the system-scope file. Skipped on
    /// the write side too: the fence-write path (`ocx self setup
    /// --managed-config`) never persists this flag back to disk.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
}

/// Background refresh posture for the `[managed]` tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefreshPolicy {
    /// Drift silently triggers a full fetch + persist + swap — but only via the
    /// background tick, whose activation gate is narrow: it fires only on an
    /// interactive terminal (stderr is a TTY), outside CI, and online (see
    /// `app::managed_config_check`). CI and other automation hosts never see
    /// the tick run; they must refresh the snapshot with an explicit
    /// `ocx config update`.
    Apply,
    /// Drift prints a stderr advisory ("run `ocx config update`"); content is
    /// never fetched by the background tick.
    Notify,
    /// The background tick is skipped entirely; only `ocx config update`
    /// refreshes the snapshot.
    Manual,
}

impl std::fmt::Display for RefreshPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Apply => "apply",
            Self::Notify => "notify",
            Self::Manual => "manual",
        })
    }
}

/// Fully resolved form of [`ManagedConfig`] with defaults applied and the
/// source parsed into a canonical [`oci::Identifier`](crate::oci::Identifier).
///
/// Produced by [`resolve_managed_config`], called at `Context::try_init`
/// (mirrors [`resolve_patch_config`](crate::config::patch::resolve_patch_config)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedManagedConfig {
    /// The managed-config artifact reference.
    pub source: crate::oci::Identifier,
    /// Whether an absent/mismatched snapshot fails closed.
    pub required: bool,
    /// Background refresh posture.
    pub refresh: RefreshPolicy,
    /// Background refresh throttle interval.
    pub interval: std::time::Duration,
    /// Whether this tier originates at the SYSTEM scope as a non-overridable
    /// required tier (mirrors
    /// [`ResolvedPatchConfig::system_required`](crate::config::patch::ResolvedPatchConfig::system_required)).
    pub system_required: bool,
}

/// On-disk snapshot of the managed-config tier, persisted as two sibling files
/// under `$OCX_HOME/state/managed-config/`:
///
/// - `snapshot.json` — this struct's metadata (`source`/`tag`/`digest`/
///   `fetched_at`), written by `serde`;
/// - `config.toml` — the raw payload bytes, held here in [`Self::config`] but
///   `#[serde(skip)]`'d out of the JSON so the payload stays a readable,
///   greppable file instead of an escaped string inside the metadata.
///
/// A config-tier data model: the loader identity-gates it against `source`
/// before folding the `config` payload, and the `required` gate compares it
/// here (see [`snapshot_matches_source`]). The domain layer
/// ([`crate::managed_config`]) writes both files (payload first, metadata last)
/// and reads them back via `persist_managed_config` /
/// `read_managed_config_snapshot_at`, and re-exports this type for API
/// stability.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ManagedConfigSnapshot {
    /// The managed-config source this snapshot was fetched from, in its
    /// canonical (normalized) `Display` form — including tag/digest. The
    /// loader compares this against the effective source before merging.
    pub source: String,
    /// The tag component of the source at persist time (snapshot format v2).
    ///
    /// Optional for backward readability of v1 snapshots (`#[serde(default)]`
    /// — absent = `None`, refreshed on the next drift sync; no migration).
    /// Surfaced by `ocx config update --check` so operators can see which
    /// floating tag the snapshot tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The top-level manifest digest (the image-index digest for a package
    /// pushed by `ocx config push`) of the fetched package at persist time —
    /// the tier's drift identity.
    pub digest: crate::oci::Digest,
    /// ISO-8601 UTC timestamp of the fetch that produced this snapshot.
    pub fetched_at: String,
    /// The raw TOML text of the fetched `config.toml` payload, with any
    /// `[managed]` section already stripped by `persist_managed_config`
    /// (ADR Decision I: a remote payload can never redirect/loosen the tier
    /// that fetched it). The loader parses this string directly and folds it
    /// into the accumulator once the source identity check passes.
    ///
    /// Persisted as the readable sibling `config.toml`, NOT embedded in
    /// `snapshot.json`: `#[serde(skip)]` keeps it out of the metadata file, and
    /// `read_managed_config_snapshot_at` repopulates it from the sibling on load.
    /// A missing/unreadable sibling makes the whole snapshot read as absent
    /// (the reader never yields a snapshot with an empty `config`).
    #[serde(skip)]
    pub config: String,
}

/// Identity gate (v2) between a snapshot's provenance and an effective
/// managed-config source.
///
/// Two clauses, both required:
///
/// 1. **Repository identity** — the snapshot's source and the effective
///    source name the same `registry/repository`
///    ([`Identifier::without_specifiers`](crate::oci::Identifier::without_specifiers)
///    equality). Tags float: a snapshot fetched at `:user-1.4.2` still
///    satisfies a seed tracking `:user` (version pins via `ocx config update
///    <VERSION>` and cascade tags both move within one repository).
/// 2. **Digest pin** — when the effective source itself carries a digest
///    (`…@sha256:<hex>`, a digest-pinned seed), the snapshot's content digest
///    must equal that pin (fail-closed immutability assertion).
///
/// A cross-repository snapshot never matches (CI cache-poison defense). A
/// parse failure on the snapshot side is a non-match. Shared by the loader's
/// identity gate ([`crate::config::loader`]), [`resolve_managed_config`]'s
/// `required` gate, the background tick, and the CLI's gated snapshot
/// accessor so they can never drift.
#[must_use]
pub fn snapshot_matches_source(snapshot: &ManagedConfigSnapshot, source: &crate::oci::Identifier) -> bool {
    crate::oci::Identifier::parse_with_default_registry(&snapshot.source, crate::oci::DEFAULT_REGISTRY).is_ok_and(
        |snapshot_source| {
            snapshot_source.without_specifiers() == source.without_specifiers()
                && source.digest().is_none_or(|pin| snapshot.digest == pin)
        },
    )
}

/// Canonical [`oci::Identifier`](crate::oci::Identifier) equality between two
/// raw managed-config source strings (both parsed with the default registry; a
/// parse failure on either side is a non-match).
///
/// Used by the system-lock env-override guard in [`resolve_target`]: a
/// system-locked `[managed]` seed accepts an `OCX_MANAGED_CONFIG` override only
/// when it canonicalizes to the same source.
#[must_use]
fn sources_canonically_eq(left: &str, right: &str) -> bool {
    matches!(
        (
            crate::oci::Identifier::parse_with_default_registry(left, crate::oci::DEFAULT_REGISTRY),
            crate::oci::Identifier::parse_with_default_registry(right, crate::oci::DEFAULT_REGISTRY),
        ),
        (Ok(left_id), Ok(right_id)) if left_id == right_id
    )
}

/// Error variants raised while resolving [`ManagedConfig`] or parsing its
/// fields.
///
/// Every variant classifies to `ExitCode::ConfigError` (78) — a managed-config
/// resolution failure is always a configuration problem (bad seed, bad env
/// override, or a required snapshot that never synced), never a data or network
/// fault in its own right.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManagedConfigError {
    /// The `source` field is present but empty — a no-op managed tier that
    /// would silently skip the whole feature.
    #[error("managed config source is empty")]
    EmptySource,

    /// The `source` value (seed or `OCX_MANAGED_CONFIG` override) is not a
    /// valid OCI identifier.
    #[error("managed config source '{value}' is not a valid OCI identifier")]
    InvalidSource {
        /// The offending source string.
        value: String,
        /// The underlying identifier parse failure.
        #[source]
        source: crate::oci::identifier::error::IdentifierError,
    },

    /// The `interval` field is not a valid `\d+[smhd]?` duration.
    #[error("managed config interval '{value}' is not a valid duration")]
    InvalidInterval {
        /// The offending interval string.
        value: String,
    },

    /// `required = true` (the default) and no snapshot exists whose provenance
    /// matches the effective `source` — identical online and offline.
    #[error("managed config snapshot required for source '{effective_source}' but absent; run `ocx config update`")]
    SnapshotRequired {
        /// The effective managed-config source that has no matching snapshot.
        ///
        /// Named `effective_source`, not `source`, so `thiserror` does not
        /// treat this field as the variant's `#[source]` error (it is an
        /// [`oci::Identifier`](crate::oci::Identifier), not an error type).
        effective_source: crate::oci::Identifier,
    },

    /// An explicit `ocx self setup --managed-config` value was refused because
    /// the `[managed]` tier is system-locked (declared `required` at the SYSTEM
    /// scope): a lock only tightens, so the flag can neither clear the tier nor
    /// redirect it. Only a value matching the locked source may proceed.
    #[error("managed config is system-locked to '{locked}'; --managed-config cannot clear or redirect it")]
    SystemLockedOverride {
        /// The locked source the explicit value must match (or be omitted).
        locked: String,
    },
}

impl crate::cli::ClassifyExitCode for ManagedConfigError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        Some(crate::cli::ExitCode::ConfigError)
    }
}

impl ManagedConfig {
    /// Default `required` value (fail-closed).
    pub const DEFAULT_REQUIRED: bool = true;

    /// Default `refresh` value.
    pub const DEFAULT_REFRESH: RefreshPolicy = RefreshPolicy::Notify;

    /// Default `interval` value, applied by [`resolve_managed_config`] when
    /// `interval` is absent.
    pub const DEFAULT_INTERVAL: &'static str = "1d";

    /// Mark this tier as system-locked — non-overridable by lower tiers — when
    /// it is `required` (the default is `required = true`).
    ///
    /// Called by the config loader on the system-scope file
    /// (`/etc/ocx/config.toml`) after parsing and before folding higher tiers
    /// in. Mirrors
    /// [`PatchConfig::lock_as_system`](crate::config::patch::PatchConfig::lock_as_system)
    /// exactly, including the "lock on the *effective* required" security
    /// rationale.
    pub fn lock_as_system(&mut self) {
        if self.required.unwrap_or(Self::DEFAULT_REQUIRED) {
            self.system_locked = true;
        }
    }

    /// Merge `other` into `self` field-by-field. `other`'s `Some` values
    /// override `self`'s; `other`'s `None` values do not clobber `self`.
    ///
    /// A system-locked tier (`self.system_locked`) ignores ALL lower-tier
    /// overrides, mirroring
    /// [`PatchConfig::merge`](crate::config::patch::PatchConfig::merge).
    pub fn merge(&mut self, other: ManagedConfig) {
        if self.system_locked {
            return;
        }
        if other.source.is_some() {
            self.source = other.source;
        }
        if other.required.is_some() {
            self.required = other.required;
        }
        if other.refresh.is_some() {
            self.refresh = other.refresh;
        }
        if other.interval.is_some() {
            self.interval = other.interval;
        }
    }
}

/// Resolves the `[managed]` config into a [`ResolvedManagedConfig`], applying
/// defaults, validating fields, and enforcing `required` against `snapshot`.
///
/// - `env_override` — the `OCX_MANAGED_CONFIG` value, if set; overrides
///   `config.managed.source` for this invocation only (never written back).
/// - `snapshot` — the local snapshot (if any) whose provenance the loader
///   already identity-gated before folding its content into `config`. Passed
///   here again so `required` enforcement can distinguish "absent" from
///   "present but ref-mismatched" (both fail the same way — ADR Decision E).
///
/// Returns `None` when no source is configured (no managed tier active) — an
/// **empty** source string is a hard error (same footgun as an empty
/// `[patches].registry` / `[mirrors."<host>"].url`).
///
/// # Errors
///
/// - [`ManagedConfigError::EmptySource`] — `source` present but empty.
/// - [`ManagedConfigError::InvalidSource`] — `source` is not a valid OCI
///   identifier.
/// - [`ManagedConfigError::InvalidInterval`] — `interval` fails
///   [`parse_interval`].
/// - [`ManagedConfigError::SnapshotRequired`] — `required` (effective) is
///   `true` and no snapshot provenance matches the effective source.
pub fn resolve_managed_config(
    config: &crate::config::Config,
    env_override: Option<&str>,
    snapshot: Option<&ManagedConfigSnapshot>,
) -> Result<Option<ResolvedManagedConfig>, ManagedConfigError> {
    let Some(resolved) = resolve_target(config, env_override)? else {
        return Ok(None);
    };

    let snapshot_matches = snapshot.is_some_and(|snap| snapshot_matches_source(snap, &resolved.source));
    Ok(Some(enforce_required_snapshot(resolved, snapshot_matches)?))
}

/// Applies the `required` gate to an already-resolved target: a `required`
/// tier whose snapshot is absent or identity-mismatched (`!snapshot_matches`)
/// fails closed with [`ManagedConfigError::SnapshotRequired`]; every other
/// combination returns the target unchanged.
///
/// Split out so a caller that resolved the target once (e.g. the config loader
/// threading it into `Context::try_init`) can apply the gate without a second
/// [`resolve_managed_target`] — the error type is owned here in `ocx_lib`,
/// where the `#[non_exhaustive]` [`ManagedConfigError`] can be constructed.
///
/// # Errors
///
/// [`ManagedConfigError::SnapshotRequired`] when `resolved.required` and not
/// `snapshot_matches`.
pub fn enforce_required_snapshot(
    resolved: ResolvedManagedConfig,
    snapshot_matches: bool,
) -> Result<ResolvedManagedConfig, ManagedConfigError> {
    if resolved.required && !snapshot_matches {
        return Err(ManagedConfigError::SnapshotRequired {
            effective_source: resolved.source,
        });
    }
    Ok(resolved)
}

/// Resolves the `[managed]` tier's effective target — source, `required`
/// posture, refresh policy, interval — WITHOUT enforcing the
/// required-snapshot gate [`resolve_managed_config`] applies.
///
/// Used by callers whose entire job is to satisfy or inspect that gate —
/// `ocx config update` (the full fetch+persist path and its `--check` probe)
/// and the background-refresh hook — none of which should fail merely because
/// the snapshot they are about to create or refresh does not exist yet.
///
/// # Errors
///
/// Same as [`resolve_managed_config`], except this function never returns
/// [`ManagedConfigError::SnapshotRequired`].
pub fn resolve_managed_target(
    config: &crate::config::Config,
    env_override: Option<&str>,
) -> Result<Option<ResolvedManagedConfig>, ManagedConfigError> {
    resolve_target(config, env_override)
}

/// Guards an explicit `ocx self setup --managed-config <value>` against the
/// system lock (a lock only tightens — mirrors [`resolve_target`]'s env-override
/// posture, but rejects instead of silently ignoring since the flag is explicit
/// intent whose downstream clear/fetch would otherwise corrupt the locked tier).
///
/// `Ok(())` when the tier is unconfigured, unlocked, or `value` canonicalizes to
/// the locked source. A system-locked tier refuses an empty (clear) or
/// non-matching (redirect) `value`.
///
/// # Errors
///
/// - [`ManagedConfigError::SystemLockedOverride`] — locked tier, `value` empty
///   or not the locked source.
/// - Any error from resolving the locked seed (malformed source/interval).
pub fn check_locked_managed_override(config: &crate::config::Config, value: &str) -> Result<(), ManagedConfigError> {
    let Some(locked) = resolve_target(config, None)? else {
        return Ok(());
    };
    if !locked.system_required {
        return Ok(());
    }
    if !value.is_empty() && sources_canonically_eq(value, &locked.source.to_string()) {
        return Ok(());
    }
    Err(ManagedConfigError::SystemLockedOverride {
        locked: locked.source.to_string(),
    })
}

/// Shared core behind [`resolve_managed_config`] and [`resolve_managed_target`]:
/// resolves source/required/refresh/interval, never consulting a snapshot.
fn resolve_target(
    config: &crate::config::Config,
    env_override: Option<&str>,
) -> Result<Option<ResolvedManagedConfig>, ManagedConfigError> {
    // A bare `OCX_MANAGED_CONFIG` env override activates the tier even with no
    // `[managed]` seed at all (the CI-ephemeral recipe) — default every other
    // field from `ManagedConfig::default()` in that case.
    let managed = config.managed.clone().unwrap_or_default();

    // Env override wins over the seed; an empty override is treated as unset
    // (matches the `OCX_CONFIG=""` precedent). A SYSTEM-LOCKED seed
    // (`/etc/ocx/config.toml` `[managed] required = true`) is an exception:
    // locks only tighten, so `OCX_MANAGED_CONFIG` can never redirect a locked
    // tier to a different source. The override is honored only when it
    // canonicalizes to the locked seed's own source; otherwise it is ignored
    // and the locked seed source stands (never a silent resolve/fetch of the
    // injected source — the trust-boundary fix for CWE-15).
    let overridden = env_override.filter(|value| !value.is_empty());
    let source = match overridden {
        Some(value)
            if managed.system_locked
                && !managed
                    .source
                    .as_deref()
                    .is_some_and(|seed| sources_canonically_eq(value, seed)) =>
        {
            crate::log::warn!(
                "ignoring OCX_MANAGED_CONFIG override '{value}': [managed] is system-locked and the override does \
                 not match the locked source"
            );
            managed.source.clone()
        }
        Some(value) => Some(value.to_string()),
        None => managed.source.clone(),
    };
    let Some(source) = source else {
        return Ok(None);
    };
    if source.is_empty() {
        return Err(ManagedConfigError::EmptySource);
    }

    let identifier = crate::oci::Identifier::parse_with_default_registry(&source, crate::oci::DEFAULT_REGISTRY)
        .map_err(|identifier_error| ManagedConfigError::InvalidSource {
            value: source.clone(),
            source: identifier_error,
        })?;

    let required = managed.required.unwrap_or(ManagedConfig::DEFAULT_REQUIRED);
    let refresh = managed.refresh.unwrap_or(ManagedConfig::DEFAULT_REFRESH);
    let interval = parse_interval(managed.interval.as_deref().unwrap_or(ManagedConfig::DEFAULT_INTERVAL))?;

    Ok(Some(ResolvedManagedConfig {
        source: identifier,
        required,
        refresh,
        interval,
        system_required: managed.system_locked,
    }))
}

/// Parses an interval string of the form `\d+[smhd]?` (bare digits = seconds;
/// `s`/`m`/`h`/`d` suffix scales the value) into a [`Duration`](std::time::Duration).
///
/// Used to resolve [`ManagedConfig::interval`] into
/// [`ResolvedManagedConfig::interval`], and shared by the background-refresh
/// throttle (mirrors the flat glob matcher precedent in `patch::matcher` —
/// small, dependency-free, unit-tested parser rather than a crate).
///
/// # Errors
///
/// Returns [`ManagedConfigError::InvalidInterval`] when `value` does not match
/// the grammar.
pub fn parse_interval(value: &str) -> Result<std::time::Duration, ManagedConfigError> {
    let invalid = || ManagedConfigError::InvalidInterval {
        value: value.to_string(),
    };

    if value.is_empty() {
        return Err(invalid());
    }

    let (digits, suffix) = match value.chars().last() {
        Some(last) if last.is_ascii_alphabetic() => (&value[..value.len() - 1], Some(last)),
        _ => (value, None),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let count: u64 = digits.parse().map_err(|_| invalid())?;
    let multiplier = match suffix {
        None | Some('s') => 1,
        Some('m') => 60,
        Some('h') => 3_600,
        Some('d') => 86_400,
        Some(_) => return Err(invalid()),
    };
    Ok(std::time::Duration::from_secs(count.saturating_mul(multiplier)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ManagedConfig defaults ────────────────────────────────────────────────

    #[test]
    fn managed_config_default_is_all_none() {
        let cfg = ManagedConfig::default();
        assert!(cfg.source.is_none());
        assert!(cfg.required.is_none());
        assert!(cfg.refresh.is_none());
        assert!(cfg.interval.is_none());
        assert!(!cfg.system_locked);
    }

    #[test]
    fn managed_config_default_required_constant_is_true() {
        const _: () = assert!(ManagedConfig::DEFAULT_REQUIRED, "DEFAULT_REQUIRED must be true");
    }

    #[test]
    fn managed_config_default_refresh_constant_is_notify() {
        assert_eq!(ManagedConfig::DEFAULT_REFRESH, RefreshPolicy::Notify);
    }

    #[test]
    fn managed_config_default_interval_constant_value() {
        assert_eq!(ManagedConfig::DEFAULT_INTERVAL, "1d");
    }

    // ── ManagedConfig TOML parsing ────────────────────────────────────────────

    #[test]
    fn managed_config_toml_full_block_parses() {
        let toml_str = r#"
            [managed]
            source = "internal.company.com/ocx-config:user"
            required = true
            refresh = "notify"
            interval = "1d"
        "#;
        let config: crate::config::Config = toml::from_str(toml_str).expect("valid [managed] TOML must parse");
        let managed = config.managed.expect("[managed] section must be present");
        assert_eq!(managed.source.as_deref(), Some("internal.company.com/ocx-config:user"));
        assert_eq!(managed.required, Some(true));
        assert_eq!(managed.refresh, Some(RefreshPolicy::Notify));
        assert_eq!(managed.interval.as_deref(), Some("1d"));
    }

    /// Fleet forward-compat (v2 posture flip): unknown `[managed]` fields are
    /// IGNORED — a seed written for a newer ocx must not brick older fleet
    /// binaries reading the same file. Inverts the v1
    /// `managed_config_toml_unknown_field_rejected` test.
    #[test]
    fn managed_config_toml_unknown_field_ignored() {
        let toml_str = "[managed]\nsource = \"r\"\nfuture_field = \"x\"\n";
        let config: crate::config::Config =
            toml::from_str(toml_str).expect("[managed] with unknown fields must parse (fleet forward-compat)");
        assert_eq!(
            config.managed.expect("[managed] present").source.as_deref(),
            Some("r"),
            "known fields still parse alongside ignored unknown ones"
        );
    }

    #[test]
    fn no_managed_section_yields_none() {
        let toml_str = "[registry]\ndefault = \"ocx.sh\"\n";
        let config: crate::config::Config = toml::from_str(toml_str).expect("config without [managed] must parse");
        assert!(config.managed.is_none());
    }

    // ── ManagedConfig::merge ──────────────────────────────────────────────────

    #[test]
    fn managed_config_merge_none_in_higher_does_not_clobber_lower() {
        let mut lower = ManagedConfig {
            source: Some("corp.example.com/ocx-config:user".to_string()),
            required: Some(true),
            refresh: Some(RefreshPolicy::Apply),
            interval: Some("1d".to_string()),
            system_locked: false,
        };
        let higher = ManagedConfig::default();
        lower.merge(higher);
        assert_eq!(lower.source.as_deref(), Some("corp.example.com/ocx-config:user"));
        assert_eq!(lower.required, Some(true));
        assert_eq!(lower.refresh, Some(RefreshPolicy::Apply));
        assert_eq!(lower.interval.as_deref(), Some("1d"));
    }

    #[test]
    fn managed_config_merge_some_in_higher_overrides_lower() {
        let mut lower = ManagedConfig {
            source: Some("old.example.com/ocx-config:user".to_string()),
            required: Some(false),
            refresh: Some(RefreshPolicy::Manual),
            interval: None,
            system_locked: false,
        };
        let higher = ManagedConfig {
            source: Some("new.example.com/ocx-config:user".to_string()),
            required: Some(true),
            refresh: Some(RefreshPolicy::Notify),
            interval: Some("6h".to_string()),
            system_locked: false,
        };
        lower.merge(higher);
        assert_eq!(lower.source.as_deref(), Some("new.example.com/ocx-config:user"));
        assert_eq!(lower.required, Some(true));
        assert_eq!(lower.refresh, Some(RefreshPolicy::Notify));
        assert_eq!(lower.interval.as_deref(), Some("6h"));
    }

    // ── system-locked tier ────────────────────────────────────────────────────

    #[test]
    fn lock_as_system_locks_required_default_and_explicit_true() {
        let mut explicit_true = ManagedConfig {
            required: Some(true),
            ..ManagedConfig::default()
        };
        explicit_true.lock_as_system();
        assert!(explicit_true.system_locked);

        let mut not_required = ManagedConfig {
            required: Some(false),
            ..ManagedConfig::default()
        };
        not_required.lock_as_system();
        assert!(!not_required.system_locked);

        let mut default_required = ManagedConfig {
            source: Some("corp.example.com/ocx-config:user".to_string()),
            ..ManagedConfig::default()
        };
        default_required.lock_as_system();
        assert!(
            default_required.system_locked,
            "absent required defaults to true and MUST lock"
        );
    }

    #[test]
    fn merge_system_locked_ignores_lower_tier() {
        let mut system = ManagedConfig {
            source: Some("system.corp/ocx-config:user".to_string()),
            required: Some(true),
            ..ManagedConfig::default()
        };
        system.lock_as_system();
        assert!(system.system_locked);

        let user = ManagedConfig {
            source: Some("user.corp/ocx-config:user".to_string()),
            required: Some(false),
            refresh: Some(RefreshPolicy::Manual),
            interval: Some("1s".to_string()),
            system_locked: false,
        };
        system.merge(user);

        assert_eq!(system.source.as_deref(), Some("system.corp/ocx-config:user"));
        assert_eq!(system.required, Some(true));
        assert!(system.system_locked, "lock flag stays sticky after merge");
    }

    // ── Config::merge wiring ──────────────────────────────────────────────────

    #[test]
    fn config_merge_managed_last_wins() {
        let system_toml = "[managed]\nsource = \"system.corp/ocx-config:user\"\nrequired = true\n";
        let user_toml = "[managed]\nsource = \"user.corp/ocx-config:user\"\n";

        let mut system: crate::config::Config = toml::from_str(system_toml).expect("system config must parse");
        let user: crate::config::Config = toml::from_str(user_toml).expect("user config must parse");

        system.merge(user);

        let managed = system.managed.expect("merged config must have [managed]");
        assert_eq!(managed.source.as_deref(), Some("user.corp/ocx-config:user"));
        assert_eq!(managed.required, Some(true));
    }

    // ── parse_interval ────────────────────────────────────────────────────────

    #[test]
    fn parse_interval_bare_digits_is_seconds() {
        assert_eq!(parse_interval("30").unwrap(), std::time::Duration::from_secs(30));
    }

    #[test]
    fn parse_interval_seconds_suffix() {
        assert_eq!(parse_interval("45s").unwrap(), std::time::Duration::from_secs(45));
    }

    #[test]
    fn parse_interval_minutes_suffix() {
        assert_eq!(parse_interval("5m").unwrap(), std::time::Duration::from_secs(5 * 60));
    }

    #[test]
    fn parse_interval_hours_suffix() {
        assert_eq!(parse_interval("2h").unwrap(), std::time::Duration::from_secs(2 * 3600));
    }

    #[test]
    fn parse_interval_days_suffix() {
        assert_eq!(parse_interval("1d").unwrap(), std::time::Duration::from_secs(86_400));
    }

    #[test]
    fn parse_interval_default_constant_parses_to_one_day() {
        assert_eq!(
            parse_interval(ManagedConfig::DEFAULT_INTERVAL).unwrap(),
            std::time::Duration::from_secs(86_400)
        );
    }

    /// "0" is bare digits with no suffix (seconds), and zero seconds is a
    /// valid (if degenerate) duration — pins the current behavior rather than
    /// rejecting it, since nothing about the grammar excludes zero.
    #[test]
    fn parse_interval_zero_is_valid_zero_duration() {
        assert_eq!(parse_interval("0").unwrap(), std::time::Duration::ZERO);
    }

    /// A digit count that cannot fit `u64` fails at the `str::parse::<u64>()`
    /// step itself (before the day-multiplier `saturating_mul`), so it
    /// gracefully yields `InvalidInterval` rather than panicking.
    #[test]
    fn parse_interval_rejects_overflow_magnitude_value() {
        assert!(matches!(
            parse_interval("99999999999999999999d"),
            Err(ManagedConfigError::InvalidInterval { .. })
        ));
    }

    #[test]
    fn parse_interval_rejects_empty_string() {
        assert!(matches!(
            parse_interval(""),
            Err(ManagedConfigError::InvalidInterval { .. })
        ));
    }

    #[test]
    fn parse_interval_rejects_garbage() {
        assert!(matches!(
            parse_interval("garbage"),
            Err(ManagedConfigError::InvalidInterval { .. })
        ));
    }

    #[test]
    fn parse_interval_rejects_negative() {
        assert!(matches!(
            parse_interval("-5"),
            Err(ManagedConfigError::InvalidInterval { .. })
        ));
    }

    #[test]
    fn parse_interval_rejects_unknown_suffix() {
        assert!(matches!(
            parse_interval("5x"),
            Err(ManagedConfigError::InvalidInterval { .. })
        ));
    }

    #[test]
    fn parse_interval_rejects_trailing_garbage_after_suffix() {
        assert!(matches!(
            parse_interval("5ss"),
            Err(ManagedConfigError::InvalidInterval { .. })
        ));
    }

    // ── resolve_managed_config ───────────────────────────────────────────────

    fn managed_snapshot(source: &str, config_toml: &str) -> ManagedConfigSnapshot {
        ManagedConfigSnapshot {
            source: source.to_string(),
            tag: None,
            digest: crate::oci::Digest::Sha256("a".repeat(64)),
            fetched_at: "2026-07-04T00:00:00Z".to_string(),
            config: config_toml.to_string(),
        }
    }

    #[test]
    fn resolve_managed_config_returns_none_when_managed_absent() {
        let config = crate::config::Config::default();
        let result = resolve_managed_config(&config, None, None);
        assert!(
            matches!(result, Ok(None)),
            "no [managed] section must yield Ok(None), got {result:?}"
        );
    }

    #[test]
    fn resolve_managed_config_returns_none_when_source_absent() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig::default()),
            ..crate::config::Config::default()
        };
        let result = resolve_managed_config(&config, None, None);
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn resolve_managed_config_errors_on_empty_source() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some(String::new()),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let result = resolve_managed_config(&config, None, None);
        assert!(matches!(result, Err(ManagedConfigError::EmptySource)));
    }

    #[test]
    fn resolve_managed_config_errors_on_invalid_source() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some("not a valid identifier !!".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let result = resolve_managed_config(&config, None, None);
        assert!(matches!(result, Err(ManagedConfigError::InvalidSource { .. })));
    }

    #[test]
    fn resolve_managed_config_errors_on_invalid_interval() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                interval: Some("not-a-duration".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let result = resolve_managed_config(&config, None, None);
        assert!(matches!(result, Err(ManagedConfigError::InvalidInterval { .. })));
    }

    /// Env override resolution order: env `OCX_MANAGED_CONFIG` beats the seed.
    #[test]
    fn resolve_managed_config_env_override_wins_over_seed() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some("seed.example.com/ocx-config:user".to_string()),
                required: Some(false),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let resolved = resolve_managed_config(&config, Some("env.example.com/ocx-config:user"), None)
            .expect("valid override must not error")
            .expect("configured source must yield Some");
        assert_eq!(resolved.source.to_string(), "env.example.com/ocx-config:user");
    }

    // Criterion 10: runtime `OCX_MANAGED_CONFIG=""` = unset (matches `OCX_CONFIG=""`).
    #[test]
    fn resolve_managed_config_empty_env_override_treated_as_unset() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some("seed.example.com/ocx-config:user".to_string()),
                required: Some(false),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let resolved = resolve_managed_config(&config, Some(""), None)
            .expect("empty override must not error")
            .expect("seed source must still resolve");
        assert_eq!(
            resolved.source.to_string(),
            "seed.example.com/ocx-config:user",
            "empty env override must fall back to the seed source, not be treated as configured"
        );
    }

    #[test]
    fn resolve_managed_config_applies_defaults_when_omitted() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                required: Some(false),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let resolved = resolve_managed_config(&config, None, None)
            .expect("valid config must not error")
            .expect("configured source must yield Some");
        assert_eq!(
            resolved.refresh,
            RefreshPolicy::Notify,
            "refresh must default to Notify"
        );
        assert_eq!(
            resolved.interval,
            std::time::Duration::from_secs(86_400),
            "interval must default to 1 day"
        );
    }

    // Criterion 6 (unit-level): default required=true + no snapshot at all -> SnapshotRequired.
    #[test]
    fn resolve_managed_config_required_true_absent_snapshot_errors_snapshot_required() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let result = resolve_managed_config(&config, None, None);
        match result {
            Err(ManagedConfigError::SnapshotRequired { effective_source }) => {
                assert_eq!(effective_source.to_string(), "corp.example.com/ocx-config:user");
            }
            other => panic!("expected SnapshotRequired, got {other:?}"),
        }
    }

    // Criterion 8: required=false + absent snapshot -> Ok(Some(resolved)), never fails closed.
    #[test]
    fn resolve_managed_config_required_false_absent_snapshot_returns_ok_some() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                required: Some(false),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let resolved = resolve_managed_config(&config, None, None)
            .expect("required=false with absent snapshot must not error")
            .expect("configured source must still yield Some");
        assert!(!resolved.required);
    }

    #[test]
    fn resolve_managed_config_snapshot_matching_source_required_true_succeeds() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let snap = managed_snapshot("corp.example.com/ocx-config:user", "[registry]\ndefault = \"x\"\n");
        let resolved = resolve_managed_config(&config, None, Some(&snap))
            .expect("matching snapshot must not error")
            .expect("configured source must yield Some");
        assert_eq!(resolved.source.to_string(), "corp.example.com/ocx-config:user");
    }

    /// Criterion 7 (unit-level): a snapshot fetched for a different source must
    /// never satisfy `required` for the CURRENT effective source — even though
    /// a snapshot file physically exists on disk (CI cache-poison defense).
    #[test]
    fn resolve_managed_config_snapshot_source_mismatch_treated_as_absent() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let snap = managed_snapshot("other.example.com/ocx-config:user", "[registry]\ndefault = \"x\"\n");
        let result = resolve_managed_config(&config, None, Some(&snap));
        assert!(
            matches!(result, Err(ManagedConfigError::SnapshotRequired { .. })),
            "a source-mismatched snapshot must be treated as absent, got {result:?}"
        );
    }

    /// Gate v2 (inverts the v1 `..._tag_vs_digest_mismatch_treated_as_absent`
    /// test): identity is `registry/repository` — tags and digests float
    /// within one repository, so a snapshot recorded under a digest reference
    /// still satisfies a tag-tracking seed for the same repository.
    #[test]
    fn resolve_managed_config_snapshot_same_repo_different_specifier_matches() {
        let hex = "b".repeat(64);
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let snap = managed_snapshot(
            &format!("corp.example.com/ocx-config@sha256:{hex}"),
            "[registry]\ndefault = \"x\"\n",
        );
        let resolved = resolve_managed_config(&config, None, Some(&snap))
            .expect("same-repository snapshot must satisfy the required gate (gate v2: specifiers float)")
            .expect("configured source must yield Some");
        assert_eq!(resolved.source.to_string(), "corp.example.com/ocx-config:user");
    }

    // ── snapshot_matches_source (gate v2) ─────────────────────────────────────

    fn gate_source(reference: &str) -> crate::oci::Identifier {
        crate::oci::Identifier::parse_with_default_registry(reference, crate::oci::DEFAULT_REGISTRY).unwrap()
    }

    /// Gate v2 clause 1: tags float within one repository — a snapshot
    /// persisted after `ocx config update user-1.4.2` (version pin) still
    /// matches a seed tracking `:user`.
    #[test]
    fn snapshot_matches_source_tag_float_matches() {
        let snap = managed_snapshot("corp.example.com/ocx-config:user-1.4.2", "");
        assert!(snapshot_matches_source(
            &snap,
            &gate_source("corp.example.com/ocx-config:user")
        ));
    }

    /// Cross-repository (or cross-registry) snapshots never match — the CI
    /// cache-poison defense survives the v2 tag-float loosening.
    #[test]
    fn snapshot_matches_source_cross_repo_rejected() {
        let snap = managed_snapshot("corp.example.com/other-config:user", "");
        assert!(!snapshot_matches_source(
            &snap,
            &gate_source("corp.example.com/ocx-config:user")
        ));

        let cross_registry = managed_snapshot("other.example.com/ocx-config:user", "");
        assert!(!snapshot_matches_source(
            &cross_registry,
            &gate_source("corp.example.com/ocx-config:user")
        ));
    }

    /// Gate v2 clause 2: a digest-pinned seed binds — the snapshot's content
    /// digest must equal the pin.
    #[test]
    fn snapshot_matches_source_digest_pin_binds() {
        // `managed_snapshot` records digest sha256:aaaa… — the matching pin.
        let pin_hex = "a".repeat(64);
        let snap = managed_snapshot("corp.example.com/ocx-config:user", "");
        assert!(snapshot_matches_source(
            &snap,
            &gate_source(&format!("corp.example.com/ocx-config@sha256:{pin_hex}"))
        ));
    }

    /// A digest-pinned seed fails closed against a snapshot carrying any
    /// other digest — tag floats never loosen an explicit pin.
    #[test]
    fn snapshot_matches_source_digest_pin_rejects_other_digest() {
        let other_pin = "c".repeat(64);
        let snap = managed_snapshot("corp.example.com/ocx-config:user", "");
        assert!(!snapshot_matches_source(
            &snap,
            &gate_source(&format!("corp.example.com/ocx-config@sha256:{other_pin}"))
        ));
    }

    /// Seed registry pin: a registry-less `[managed].source` resolves against
    /// the BUILT-IN default registry, never the configured `[registry].default`
    /// — the managed tier cannot have its own trust root redirected by the
    /// config it is about to replace.
    #[test]
    fn resolve_target_registry_less_source_uses_built_in_default_registry() {
        let config: crate::config::Config = toml::from_str(
            "[registry]\ndefault = \"attacker.example.com\"\n[managed]\nsource = \"team/ocx-config:user\"\nrequired = false\n",
        )
        .expect("config must parse");
        let resolved = resolve_managed_config(&config, None, None)
            .expect("valid config must not error")
            .expect("configured source must yield Some");
        assert_eq!(
            resolved.source.registry(),
            crate::oci::DEFAULT_REGISTRY,
            "the seed must resolve against the built-in default registry, not [registry].default"
        );
    }

    /// Finding #1 regression: a SYSTEM-LOCKED `[managed]` source must not be
    /// redirected by an `OCX_MANAGED_CONFIG` env override pointing at a
    /// different source. The override is ignored and resolution stays on the
    /// locked seed source (never a silent resolve/fetch of the injected source
    /// — CWE-15, locks only tighten).
    #[test]
    fn resolve_target_system_locked_ignores_mismatched_env_override() {
        let mut managed = ManagedConfig {
            source: Some("system.corp/ocx-config:user".to_string()),
            required: Some(true),
            ..ManagedConfig::default()
        };
        managed.system_locked = true;
        let config = crate::config::Config {
            managed: Some(managed),
            ..crate::config::Config::default()
        };
        let resolved = resolve_managed_target(&config, Some("hostile.test/evil-config:latest"))
            .expect("locked tier with a mismatched override must still resolve to the locked seed")
            .expect("the locked seed source must yield Some");
        assert_eq!(
            resolved.source.to_string(),
            "system.corp/ocx-config:user",
            "a system-locked source must not be redirected by OCX_MANAGED_CONFIG"
        );
    }

    /// A system-locked seed still accepts an `OCX_MANAGED_CONFIG` override that
    /// canonicalizes to the same source (identity match, not a redirection).
    #[test]
    fn resolve_target_system_locked_accepts_matching_env_override() {
        let mut managed = ManagedConfig {
            source: Some("system.corp/ocx-config:user".to_string()),
            required: Some(true),
            ..ManagedConfig::default()
        };
        managed.system_locked = true;
        let config = crate::config::Config {
            managed: Some(managed),
            ..crate::config::Config::default()
        };
        let resolved = resolve_managed_target(&config, Some("system.corp/ocx-config:user"))
            .expect("a matching override must not error")
            .expect("configured source must yield Some");
        assert_eq!(resolved.source.to_string(), "system.corp/ocx-config:user");
    }

    /// An UNLOCKED seed (the default) still honors an env override that points
    /// at a different source — the lock guard only tightens the locked case.
    #[test]
    fn resolve_target_unlocked_seed_still_honors_env_override() {
        let config = crate::config::Config {
            managed: Some(ManagedConfig {
                source: Some("seed.example.com/ocx-config:user".to_string()),
                required: Some(false),
                ..ManagedConfig::default()
            }),
            ..crate::config::Config::default()
        };
        let resolved = resolve_managed_target(&config, Some("env.example.com/ocx-config:user"))
            .expect("unlocked override must not error")
            .expect("configured source must yield Some");
        assert_eq!(
            resolved.source.to_string(),
            "env.example.com/ocx-config:user",
            "an unlocked seed must still allow env-override redirection"
        );
    }

    #[test]
    fn resolve_managed_config_carries_system_required() {
        let mut managed = ManagedConfig {
            source: Some("corp.example.com/ocx-config:user".to_string()),
            required: Some(false),
            ..ManagedConfig::default()
        };
        managed.system_locked = true;
        let config = crate::config::Config {
            managed: Some(managed),
            ..crate::config::Config::default()
        };
        let resolved = resolve_managed_config(&config, None, None)
            .expect("valid config must not error")
            .expect("configured source must yield Some");
        assert!(
            resolved.system_required,
            "system_locked must propagate to system_required"
        );
    }
}
