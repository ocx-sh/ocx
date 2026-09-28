// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared helpers for the `ocx patch` maintainer subcommands.

use ocx_config::patch::PatchConfig;
use ocx_config::patch::ResolvedPatchConfig;

use crate::error::UsageError;

/// Resolve the effective patch tier, honouring an optional `--registry` override.
///
/// An override retargets the configured tier's host, keeping its `path` and `required`, or builds a default tier when none is configured.
/// Errors with a usage error (exit 64) when the override is blank, or absent with no tier configured.
pub fn effective_patches(
    registry_override: Option<&str>,
    context: &crate::app::Context,
) -> Result<ResolvedPatchConfig, UsageError> {
    resolve_effective_patches(registry_override, context.manager().patches().cloned())
}

fn resolve_effective_patches(
    registry_override: Option<&str>,
    configured: Option<ResolvedPatchConfig>,
) -> Result<ResolvedPatchConfig, UsageError> {
    let Some(registry) = registry_override else {
        return configured.ok_or_else(|| {
            UsageError::new(
                "no patch registry configured; pass --registry <HOST/PATH>, set a [patches] config tier, or OCX_PATCHES",
            )
        });
    };

    let registry = registry.trim();
    if registry.is_empty() {
        return Err(UsageError::new("--registry value must not be empty"));
    }

    Ok(match configured {
        Some(mut tier) => {
            tier.registry = registry.to_string();
            tier
        }
        None => ResolvedPatchConfig {
            registry: registry.to_string(),
            path_template: PatchConfig::DEFAULT_PATH_TEMPLATE.to_string(),
            required: PatchConfig::DEFAULT_REQUIRED,
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_tier() -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            registry: "configured.example/patches".to_string(),
            path_template: "{registry}/custom/{repository}".to_string(),
            required: false,
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
        }
    }

    /// No `--registry` and no configured tier → usage error (exit 64 surface).
    #[test]
    fn no_registry_no_config_is_usage_error() {
        assert!(resolve_effective_patches(None, None).is_err());
    }

    /// No `--registry` but a configured tier → the configured tier verbatim.
    #[test]
    fn no_registry_uses_configured_tier() {
        let resolved = resolve_effective_patches(None, Some(configured_tier())).expect("configured tier");
        assert_eq!(resolved, configured_tier());
    }

    /// `--registry` with no configured tier → a default tier at that registry.
    /// This is the bootstrap-a-new-registry path.
    #[test]
    fn registry_override_without_config_builds_default_tier() {
        let resolved = resolve_effective_patches(Some("registry.corp.example/ocx-patches"), None)
            .expect("override must build a tier");
        assert_eq!(resolved.registry, "registry.corp.example/ocx-patches");
        assert_eq!(resolved.path_template, PatchConfig::DEFAULT_PATH_TEMPLATE);
        assert_eq!(resolved.required, PatchConfig::DEFAULT_REQUIRED);
        assert!(!resolved.system_required);
    }

    /// `--registry` with a configured tier → retarget the host but keep the
    /// configured `path` template and `required` posture.
    #[test]
    fn registry_override_preserves_configured_path_and_required() {
        let resolved = resolve_effective_patches(Some("registry.corp.example/ocx-patches"), Some(configured_tier()))
            .expect("override with config");
        assert_eq!(resolved.registry, "registry.corp.example/ocx-patches");
        assert_eq!(resolved.path_template, "{registry}/custom/{repository}");
        assert!(
            !resolved.required,
            "configured required posture must survive the retarget"
        );
    }

    /// An empty / whitespace `--registry` value is a usage error, not a silent
    /// no-op patch tier (same footgun as an empty `[patches].registry`).
    #[test]
    fn empty_registry_override_is_usage_error() {
        assert!(resolve_effective_patches(Some("   "), None).is_err());
    }
}
