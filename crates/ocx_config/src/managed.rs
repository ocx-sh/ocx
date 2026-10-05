// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Corporate-managed configuration tier (`[managed]`): a seed pointer resolving to a published
//! `config.toml` payload merged above the user config. See `adr_managed_config_tier.md`.

use serde::{Deserialize, Serialize};

use crate::refresh::{IntervalError, RefreshPolicy, parse_interval};

// No `deny_unknown_fields`, for the fleet forward-compat reason stated on `crate::Config`.
/// Configuration for the `[managed]` tier.
///
/// Unknown keys are tolerated, so a typo'd key silently does nothing;
/// `ocx config update --check` surfaces the tier's effective state for diagnosis.
#[derive(Debug, Default, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ManagedConfig {
    /// OCI reference for the managed-config artifact, e.g.
    /// `"internal.company.com/ocx-config:user"`.
    ///
    /// Absent → no managed tier configured.
    pub source: Option<String>,

    /// Fail posture when the snapshot is required but absent.
    ///
    /// - `true` (default) — fail closed: a config error (exit 78) until `ocx config update`
    ///   (or `self setup --managed-config`) syncs one.
    /// - `false` — the tier contributes nothing until synced; a throttle-gated
    ///   stderr hint only.
    // An unsynced optional tier is a benign state: never a per-invocation WARN.
    pub required: Option<bool>,

    /// Background refresh posture. Defaults to `notify`.
    ///
    /// `apply` swaps in a drifted snapshot, `notify` advises `ocx config update`, `manual`
    /// refreshes only on `ocx config update`. CI never runs the background check. An unknown
    /// value is ignored with a warning, so the default applies.
    #[serde(default, deserialize_with = "deserialize_refresh")]
    pub refresh: Option<RefreshPolicy>,

    /// Background refresh throttle interval, `\d+[smhd]?` (bare = seconds).
    /// Defaults to `"1d"`.
    pub interval: Option<String>,

    /// Set by the loader on a SYSTEM-scope tier whose effective `required` is true.
    ///
    /// Skipped on write too, or `ocx self setup --managed-config` persists the lock to disk.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
}

fn deserialize_refresh<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<RefreshPolicy>, D::Error> {
    crate::refresh::deserialize_lenient(deserializer, "[managed] refresh")
}

/// Fully resolved [`ManagedConfig`], with defaults applied and the source parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedManagedConfig {
    /// The artifact's registry location, fetched from exactly here, never routed through an index.
    pub source: ocx_oci::OciIdentifier,
    /// Whether an absent/mismatched snapshot fails closed.
    pub required: bool,
    /// Background refresh posture.
    pub refresh: RefreshPolicy,
    /// Background refresh throttle interval.
    pub interval: std::time::Duration,
    /// Whether this is a non-overridable SYSTEM-scope required tier.
    pub system_required: bool,
}

/// On-disk snapshot of the managed-config tier: this metadata in `snapshot.json`, the payload
/// ([`Self::config`]) in a sibling `config.toml`.
///
/// Its payload folds only after [`snapshot_matches_source`] passes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ManagedConfigSnapshot {
    /// The source this snapshot was fetched from, in canonical `Display` form with tag and digest.
    pub source: String,
    /// The source's tag at persist time; absent in older snapshots until the next drift sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The top-level manifest digest at persist time, the tier's drift identity.
    pub digest: ocx_oci::Digest,
    /// ISO-8601 UTC timestamp of the fetch that produced this snapshot.
    pub fetched_at: String,
    /// The raw payload TOML, `[managed]` already stripped; persisted as the sibling `config.toml`.
    #[serde(skip)]
    pub config: String,
}

/// Identity gate: the snapshot names the same `registry/repository` as `source` (tags float), and
/// a digest-pinned `source` requires the snapshot's digest to equal the pin.
///
/// A cross-repository snapshot never matches (CI cache-poison defense).
#[must_use]
pub fn snapshot_matches_source(snapshot: &ManagedConfigSnapshot, source: &ocx_oci::OciIdentifier) -> bool {
    ocx_oci::OciIdentifier::parse_target(&snapshot.source, ocx_oci::DEFAULT_REGISTRY).is_ok_and(|snapshot_source| {
        snapshot_source.without_specifiers() == source.without_specifiers()
            && source.digest().is_none_or(|pin| snapshot.digest == pin)
    })
}

/// Canonical [`ocx_oci::PackageRef`] equality of two raw source strings; a parse failure is a
/// non-match.
#[must_use]
fn sources_canonically_eq(left: &str, right: &str) -> bool {
    matches!(
        (
            ocx_oci::PackageRef::parse_with_default_registry(left, ocx_oci::DEFAULT_REGISTRY),
            ocx_oci::PackageRef::parse_with_default_registry(right, ocx_oci::DEFAULT_REGISTRY),
        ),
        (Ok(left_id), Ok(right_id)) if left_id == right_id
    )
}

/// Errors raised while resolving [`ManagedConfig`] or parsing its fields.
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
        source: ocx_oci::package_ref::error::IdentifierError,
    },

    /// The `interval` field is not a valid `\d+[smhd]?` duration.
    // Interpolated, not `#[source]`: the classifier answers at this variant, and a source would
    // print the interval twice in the `{:#}` chain.
    #[error("managed config {0}")]
    InvalidInterval(IntervalError),

    /// `required = true` (the default) and no snapshot exists whose provenance
    /// matches the effective `source` — identical online and offline.
    #[error("managed config snapshot required for source '{effective_source}' but absent; run `ocx config update`")]
    SnapshotRequired {
        /// The effective managed-config source that has no matching snapshot.
        effective_source: ocx_oci::OciIdentifier,
    },

    /// `required = true` and an identity-matching snapshot is on disk, but its payload does not
    /// parse as a [`Config`](crate::Config), so none of its settings apply.
    #[error(
        "managed config snapshot for source '{effective_source}' is present but its payload is not a usable \
         config; re-sync with `ocx config update`"
    )]
    SnapshotUnusable {
        /// The effective managed-config source whose snapshot payload failed to parse.
        effective_source: ocx_oci::OciIdentifier,
    },

    /// An explicit `--managed-config` value would clear or redirect a system-locked tier.
    #[error("managed config is system-locked to '{locked}'; --managed-config cannot clear or redirect it")]
    SystemLockedOverride {
        /// The locked source the explicit value must match (or be omitted).
        locked: String,
    },
}

impl ManagedConfig {
    /// Default `required` value (fail-closed).
    pub const DEFAULT_REQUIRED: bool = true;

    /// Default `refresh` value.
    pub const DEFAULT_REFRESH: RefreshPolicy = RefreshPolicy::Notify;

    /// Default `interval` value.
    pub const DEFAULT_INTERVAL: &'static str = crate::refresh::DEFAULT_INTERVAL;

    /// Mark this tier as system-locked when its effective `required` is true, for the reason on
    /// [`PatchConfig::lock_as_system`](crate::patch::PatchConfig::lock_as_system).
    pub fn lock_as_system(&mut self) {
        if self.required.unwrap_or(Self::DEFAULT_REQUIRED) {
            self.system_locked = true;
        }
    }

    /// Merge `other` into `self`: `other`'s `Some` values win, unless `self` is system-locked.
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

/// Resolves `[managed]` with defaults applied and `required` enforced against `snapshot`; `None`
/// when no source is configured. `env_override` (`OCX_MANAGED_CONFIG`) overrides the source.
///
/// # Errors
///
/// [`ManagedConfigError::EmptySource`], [`ManagedConfigError::InvalidSource`] or
/// [`ManagedConfigError::InvalidInterval`] for a bad field; [`ManagedConfigError::SnapshotRequired`]
/// or [`ManagedConfigError::SnapshotUnusable`] for a `required` tier whose snapshot is not applied.
pub fn resolve_managed_config(
    config: &crate::Config,
    env_override: Option<&str>,
    snapshot: Option<&ManagedConfigSnapshot>,
) -> Result<Option<ResolvedManagedConfig>, ManagedConfigError> {
    let Some(resolved) = resolve_target(config, env_override)? else {
        return Ok(None);
    };

    let state = ManagedSnapshotState::classify(snapshot, &resolved.source);
    Ok(Some(enforce_required_snapshot(resolved, state)?))
}

/// What the on-disk snapshot actually contributed to the merged config, the input to the
/// `required` gate.
///
/// Identity alone is not enough: the loader drops an unparsable payload, so gating on identity
/// would fail open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedSnapshotState {
    /// No readable snapshot, or one whose provenance names a different source.
    Unmatched,
    /// Identity matched, but the payload did not parse as a
    /// [`Config`](crate::Config) and was therefore not merged.
    PayloadUnusable,
    /// Identity matched and the payload was merged into the effective config.
    Applied,
}

impl ManagedSnapshotState {
    /// Classifies a snapshot against `source` as the loader would: identity gate, then payload parse.
    #[must_use]
    pub fn classify(snapshot: Option<&ManagedConfigSnapshot>, source: &ocx_oci::OciIdentifier) -> Self {
        let Some(snapshot) = snapshot.filter(|snap| snapshot_matches_source(snap, source)) else {
            return Self::Unmatched;
        };
        if toml::from_str::<crate::Config>(&snapshot.config).is_ok() {
            Self::Applied
        } else {
            Self::PayloadUnusable
        }
    }
}

/// Applies the `required` gate to an already-resolved target: a `required` tier whose snapshot
/// did not reach the merged config fails closed.
///
/// # Errors
///
/// When `resolved.required`: [`ManagedConfigError::SnapshotRequired`] for
/// [`ManagedSnapshotState::Unmatched`], [`ManagedConfigError::SnapshotUnusable`] for
/// [`ManagedSnapshotState::PayloadUnusable`].
pub fn enforce_required_snapshot(
    resolved: ResolvedManagedConfig,
    state: ManagedSnapshotState,
) -> Result<ResolvedManagedConfig, ManagedConfigError> {
    if !resolved.required {
        return Ok(resolved);
    }
    match state {
        ManagedSnapshotState::Applied => Ok(resolved),
        ManagedSnapshotState::Unmatched => Err(ManagedConfigError::SnapshotRequired {
            effective_source: resolved.source,
        }),
        ManagedSnapshotState::PayloadUnusable => Err(ManagedConfigError::SnapshotUnusable {
            effective_source: resolved.source,
        }),
    }
}

/// Resolves the effective target without the required-snapshot gate, for callers that create or
/// inspect the snapshot (`ocx config update`, the background refresh).
///
/// # Errors
///
/// As [`resolve_managed_config`], minus the two snapshot errors.
pub fn resolve_managed_target(
    config: &crate::Config,
    env_override: Option<&str>,
) -> Result<Option<ResolvedManagedConfig>, ManagedConfigError> {
    resolve_target(config, env_override)
}

/// Guards an explicit `ocx self setup --managed-config <value>` against the system lock.
///
/// Refuses where the env override is merely ignored, since the flag's downstream clear or fetch
/// would corrupt the locked tier.
///
/// # Errors
///
/// [`ManagedConfigError::SystemLockedOverride`] for an empty or non-matching `value` on a locked
/// tier, or any error from resolving the locked seed.
pub fn check_locked_managed_override(config: &crate::Config, value: &str) -> Result<(), ManagedConfigError> {
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
    config: &crate::Config,
    env_override: Option<&str>,
) -> Result<Option<ResolvedManagedConfig>, ManagedConfigError> {
    // A bare `OCX_MANAGED_CONFIG` activates the tier even with no `[managed]` seed.
    let managed = config.managed.clone().unwrap_or_default();

    // A system-locked seed honours only an override naming the locked source, or the env could
    // redirect a locked tier (CWE-15).
    let overridden = env_override.filter(|value| !value.is_empty());
    let source = match overridden {
        Some(value)
            if managed.system_locked
                && !managed
                    .source
                    .as_deref()
                    .is_some_and(|seed| sources_canonically_eq(value, seed)) =>
        {
            log::warn!(
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

    let identifier =
        ocx_oci::OciIdentifier::parse_target(&source, ocx_oci::DEFAULT_REGISTRY).map_err(|identifier_error| {
            ManagedConfigError::InvalidSource {
                value: source.clone(),
                source: identifier_error,
            }
        })?;

    let required = managed.required.unwrap_or(ManagedConfig::DEFAULT_REQUIRED);
    let refresh = managed.refresh.unwrap_or(ManagedConfig::DEFAULT_REFRESH);
    let interval = parse_interval(managed.interval.as_deref().unwrap_or(ManagedConfig::DEFAULT_INTERVAL))
        .map_err(ManagedConfigError::InvalidInterval)?;

    Ok(Some(ResolvedManagedConfig {
        source: identifier,
        required,
        refresh,
        interval,
        system_required: managed.system_locked,
    }))
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
        let config: crate::Config = toml::from_str(toml_str).expect("valid [managed] TOML must parse");
        let managed = config.managed.expect("[managed] section must be present");
        assert_eq!(managed.source.as_deref(), Some("internal.company.com/ocx-config:user"));
        assert_eq!(managed.required, Some(true));
        assert_eq!(managed.refresh, Some(RefreshPolicy::Notify));
        assert_eq!(managed.interval.as_deref(), Some("1d"));
    }

    /// An unknown `refresh` posture must not fail the file: a payload written for a newer ocx
    /// would otherwise break every older binary in the fleet.
    #[test]
    fn unknown_refresh_posture_is_ignored_and_the_rest_of_the_section_survives() {
        let config: crate::Config = toml::from_str(
            r#"
            [managed]
            source = "internal.company.com/ocx-config:user"
            refresh = "someday"
        "#,
        )
        .expect("an unknown refresh posture must not fail the parse");
        let managed = config.managed.expect("[managed] section must be present");
        assert_eq!(managed.refresh, None);
        assert_eq!(managed.source.as_deref(), Some("internal.company.com/ocx-config:user"));
    }

    /// Fleet forward-compat (v2 posture flip): unknown `[managed]` fields are
    /// IGNORED — a seed written for a newer ocx must not brick older fleet
    /// binaries reading the same file. Inverts the v1
    /// `managed_config_toml_unknown_field_rejected` test.
    #[test]
    fn managed_config_toml_unknown_field_ignored() {
        let toml_str = "[managed]\nsource = \"r\"\nfuture_field = \"x\"\n";
        let config: crate::Config =
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
        let config: crate::Config = toml::from_str(toml_str).expect("config without [managed] must parse");
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

        let mut system: crate::Config = toml::from_str(system_toml).expect("system config must parse");
        let user: crate::Config = toml::from_str(user_toml).expect("user config must parse");

        system.merge(user);

        let managed = system.managed.expect("merged config must have [managed]");
        assert_eq!(managed.source.as_deref(), Some("user.corp/ocx-config:user"));
        assert_eq!(managed.required, Some(true));
    }

    // ── resolve_managed_config ───────────────────────────────────────────────

    fn managed_snapshot(source: &str, config_toml: &str) -> ManagedConfigSnapshot {
        ManagedConfigSnapshot {
            source: source.to_string(),
            tag: None,
            digest: ocx_oci::Digest::Sha256("a".repeat(64)),
            fetched_at: "2026-07-04T00:00:00Z".to_string(),
            config: config_toml.to_string(),
        }
    }

    #[test]
    fn resolve_managed_config_returns_none_when_managed_absent() {
        let config = crate::Config::default();
        let result = resolve_managed_config(&config, None, None);
        assert!(
            matches!(result, Ok(None)),
            "no [managed] section must yield Ok(None), got {result:?}"
        );
    }

    #[test]
    fn resolve_managed_config_returns_none_when_source_absent() {
        let config = crate::Config {
            managed: Some(ManagedConfig::default()),
            ..crate::Config::default()
        };
        let result = resolve_managed_config(&config, None, None);
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn resolve_managed_config_errors_on_empty_source() {
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some(String::new()),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
        };
        let result = resolve_managed_config(&config, None, None);
        assert!(matches!(result, Err(ManagedConfigError::EmptySource)));
    }

    #[test]
    fn resolve_managed_config_errors_on_invalid_source() {
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("not a valid identifier !!".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
        };
        let result = resolve_managed_config(&config, None, None);
        assert!(matches!(result, Err(ManagedConfigError::InvalidSource { .. })));
    }

    #[test]
    fn resolve_managed_config_errors_on_invalid_interval() {
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                interval: Some("not-a-duration".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
        };
        let result = resolve_managed_config(&config, None, None);
        assert!(matches!(result, Err(ManagedConfigError::InvalidInterval { .. })));
    }

    /// The managed interval message is a user-visible string; wrapping `IntervalError` keeps it.
    #[test]
    fn invalid_interval_message_is_unchanged_by_the_wrapped_error() {
        let error = ManagedConfigError::InvalidInterval(parse_interval("not-a-duration").unwrap_err());
        assert_eq!(
            error.to_string(),
            "managed config interval 'not-a-duration' is not a valid duration"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    /// Env override resolution order: env `OCX_MANAGED_CONFIG` beats the seed.
    #[test]
    fn resolve_managed_config_env_override_wins_over_seed() {
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("seed.example.com/ocx-config:user".to_string()),
                required: Some(false),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
        };
        let resolved = resolve_managed_config(&config, Some("env.example.com/ocx-config:user"), None)
            .expect("valid override must not error")
            .expect("configured source must yield Some");
        assert_eq!(resolved.source.to_string(), "env.example.com/ocx-config:user");
    }

    // Criterion 10: runtime `OCX_MANAGED_CONFIG=""` = unset (matches `OCX_CONFIG=""`).
    #[test]
    fn resolve_managed_config_empty_env_override_treated_as_unset() {
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("seed.example.com/ocx-config:user".to_string()),
                required: Some(false),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
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
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                required: Some(false),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
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
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
        };
        let result = resolve_managed_config(&config, None, None);
        match result {
            Err(ManagedConfigError::SnapshotRequired { effective_source }) => {
                assert_eq!(effective_source.to_string(), "corp.example.com/ocx-config:user");
            }
            other => panic!("expected SnapshotRequired, got {other:?}"),
        }
    }

    /// Regression: a `required` tier whose snapshot matches by IDENTITY but
    /// whose payload does not parse must fail closed.
    ///
    /// The identity gate alone reported such a tier satisfied while the loader
    /// silently dropped the payload — fail-open in the one place an operator
    /// asked to fail closed. `SnapshotUnusable`, not `SnapshotRequired`: the
    /// snapshot is present, so "but absent; run `ocx config update`" would
    /// have described the wrong state.
    #[test]
    fn resolve_managed_config_required_true_unparseable_payload_fails_closed() {
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
        };
        let snap = managed_snapshot("corp.example.com/ocx-config:user", "not = [valid toml");
        match resolve_managed_config(&config, None, Some(&snap)) {
            Err(ManagedConfigError::SnapshotUnusable { effective_source }) => {
                assert_eq!(effective_source.to_string(), "corp.example.com/ocx-config:user");
            }
            other => panic!("expected SnapshotUnusable, got {other:?}"),
        }
    }

    /// The discriminator for the test above: a payload carrying keys this
    /// binary does not know is NOT unusable — it parses, folds, and satisfies
    /// the gate. Without this, "fails closed on an unparseable payload" would
    /// be indistinguishable from "fails closed on anything unfamiliar", which
    /// is exactly the fleet break the tolerance posture exists to prevent.
    #[test]
    fn resolve_managed_config_required_true_unknown_keys_payload_satisfies_gate() {
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
        };
        let snap = managed_snapshot(
            "corp.example.com/ocx-config:user",
            "[registry]\ndefault = \"corp.example.com\"\ntimeout = 30\n[toolchain]\nchannel = \"stable\"\n",
        );
        let resolved = resolve_managed_config(&config, None, Some(&snap))
            .expect("a payload from a newer ocx must satisfy the required gate")
            .expect("configured source must yield Some");
        assert!(resolved.required);
    }

    /// `required = false` never fails on an unusable payload — the posture
    /// governs the gate, not the classification. The tier simply contributes
    /// nothing (the loader WARNs).
    #[test]
    fn enforce_required_snapshot_required_false_tolerates_unusable_payload() {
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                required: Some(false),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
        };
        let snap = managed_snapshot("corp.example.com/ocx-config:user", "not = [valid toml");
        resolve_managed_config(&config, None, Some(&snap))
            .expect("required=false must not fail on an unusable payload")
            .expect("configured source must yield Some");
    }

    /// The three states classify exactly as the loader would act on them.
    #[test]
    fn managed_snapshot_state_classifies_identity_then_payload() {
        let source = gate_source("corp.example.com/ocx-config:user");
        assert_eq!(
            ManagedSnapshotState::classify(None, &source),
            ManagedSnapshotState::Unmatched
        );

        let foreign = managed_snapshot("other.example.com/ocx-config:user", "[registry]\ndefault = \"x\"\n");
        assert_eq!(
            ManagedSnapshotState::classify(Some(&foreign), &source),
            ManagedSnapshotState::Unmatched,
            "a wrong-identity snapshot is unmatched no matter how good its payload is"
        );

        let broken = managed_snapshot("corp.example.com/ocx-config:user", "not = [valid toml");
        assert_eq!(
            ManagedSnapshotState::classify(Some(&broken), &source),
            ManagedSnapshotState::PayloadUnusable
        );

        let good = managed_snapshot("corp.example.com/ocx-config:user", "[registry]\ndefault = \"x\"\n");
        assert_eq!(
            ManagedSnapshotState::classify(Some(&good), &source),
            ManagedSnapshotState::Applied
        );
    }

    // Criterion 8: required=false + absent snapshot -> Ok(Some(resolved)), never fails closed.
    #[test]
    fn resolve_managed_config_required_false_absent_snapshot_returns_ok_some() {
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                required: Some(false),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
        };
        let resolved = resolve_managed_config(&config, None, None)
            .expect("required=false with absent snapshot must not error")
            .expect("configured source must still yield Some");
        assert!(!resolved.required);
    }

    #[test]
    fn resolve_managed_config_snapshot_matching_source_required_true_succeeds() {
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
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
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
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
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("corp.example.com/ocx-config:user".to_string()),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
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

    fn gate_source(reference: &str) -> ocx_oci::OciIdentifier {
        ocx_oci::OciIdentifier::parse_target(reference, ocx_oci::DEFAULT_REGISTRY).unwrap()
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
        let config: crate::Config = toml::from_str(
            "[registry]\ndefault = \"attacker.example.com\"\n[managed]\nsource = \"team/ocx-config:user\"\nrequired = false\n",
        )
        .expect("config must parse");
        let resolved = resolve_managed_config(&config, None, None)
            .expect("valid config must not error")
            .expect("configured source must yield Some");
        assert_eq!(
            resolved.source.registry(),
            ocx_oci::DEFAULT_REGISTRY,
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
        let config = crate::Config {
            managed: Some(managed),
            ..crate::Config::default()
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
        let config = crate::Config {
            managed: Some(managed),
            ..crate::Config::default()
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
        let config = crate::Config {
            managed: Some(ManagedConfig {
                source: Some("seed.example.com/ocx-config:user".to_string()),
                required: Some(false),
                ..ManagedConfig::default()
            }),
            ..crate::Config::default()
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
        let config = crate::Config {
            managed: Some(managed),
            ..crate::Config::default()
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
