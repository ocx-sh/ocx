// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Authoring-form package metadata — the `-metadata.json` sidecar a publisher
//! edits and `ocx package create` compiles.
//!
//! The authoring form is a superset of the published [`Metadata`]: dependency
//! identifiers may omit the digest (tag-only, resolved by `create`), each
//! dependency may carry a per-platform `platforms` pin map, and the bundle may
//! carry a `platforms` target set. Projection to the published form
//! ([`AuthoringMetadata::to_published`]) collapses each dependency to a single
//! manifest-digest pin and strips the sidecar-only fields **by construction**
//! — the published types simply do not have them.
//!
//! Every published `metadata.json` parses as authoring metadata (subset
//! compatibility), so `ocx package push` reads one type.
//!
//! ADR: `adr_dependency_manifest_pinning.md`.

pub mod dependency;
pub mod target_platforms;

pub use dependency::{AuthoringDependencies, AuthoringDependency};
pub use target_platforms::{TargetPlatforms, TargetPlatformsError};

use serde::{Deserialize, Serialize};

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::oci;

use super::Metadata;
use super::bundle::{Bundle, Version};
use super::dependency::{Dependencies, DependencyError};
use super::entrypoint::Entrypoints;
use super::env;

/// OCX package metadata in authoring (sidecar) form.
///
/// Superset of the published [`Metadata`]: dependency digests are optional
/// and sidecar-only fields (`platforms`) are allowed. See the module docs.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthoringMetadata {
    Bundle(AuthoringBundle),
}

/// Bundle package metadata in authoring form.
///
/// Same shape as the published [`Bundle`] plus the sidecar-only `platforms`
/// target set and authoring-form dependencies.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AuthoringBundle {
    /// The version of the bundle metadata format.
    pub version: Version,

    /// Number of leading path components to strip when extracting the bundle.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub strip_components: Option<u8>,

    /// Environment variables the package contributes.
    #[serde(skip_serializing_if = "env::Env::is_empty", default)]
    pub env: env::Env,

    /// Ordered list of package dependencies in authoring form (digest
    /// optional; per-platform pin maps allowed). Array order defines the
    /// environment import order.
    #[serde(skip_serializing_if = "AuthoringDependencies::is_empty", default)]
    pub dependencies: AuthoringDependencies,

    /// Named entrypoints that `ocx package install` generates launchers for.
    #[serde(skip_serializing_if = "Entrypoints::is_empty", default)]
    pub entrypoints: Entrypoints,

    /// The package's target-platform set, written by `ocx package create`
    /// when `--platform` is given. Authoring sidecar only — stripped at
    /// publish. `ocx package push` fans out to every platform in this set.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub platforms: Option<TargetPlatforms>,
}

impl AuthoringMetadata {
    pub fn dependencies(&self) -> &AuthoringDependencies {
        match self {
            AuthoringMetadata::Bundle(bundle) => &bundle.dependencies,
        }
    }

    /// The embedded target-platform set, when `ocx package create` wrote one.
    pub fn target_platforms(&self) -> Option<&TargetPlatforms> {
        match self {
            AuthoringMetadata::Bundle(bundle) => bundle.platforms.as_ref(),
        }
    }

    /// `true` when every dependency carries a digest or a non-empty
    /// platforms map — i.e. push can project without network resolution.
    pub fn is_fully_pinned(&self) -> bool {
        self.dependencies().iter().all(AuthoringDependency::is_pinned)
    }

    /// Projects the authoring metadata into the published [`Metadata`] for
    /// `platform`.
    ///
    /// Each dependency collapses to a single [`oci::PinnedIdentifier`] via
    /// [`AuthoringDependency::pin_for`]; sidecar-only fields (`platforms` on
    /// the bundle and per-dependency pin maps) are stripped by construction.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError`] when a dependency is unpinned or its pin
    /// map has no key covering `platform`.
    pub fn to_published(&self, platform: &oci::Platform) -> Result<Metadata, AuthoringError> {
        match self {
            AuthoringMetadata::Bundle(bundle) => {
                let dependencies = bundle
                    .dependencies
                    .iter()
                    .map(|dep| dep.to_published(platform))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Metadata::Bundle(Bundle {
                    version: bundle.version,
                    strip_components: bundle.strip_components,
                    env: bundle.env.clone(),
                    dependencies: Dependencies::new(dependencies)?,
                    entrypoints: bundle.entrypoints.clone(),
                }))
            }
        }
    }
}

/// Errors projecting authoring metadata to the published form.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthoringError {
    /// A dependency has neither a digest nor a platforms pin map.
    #[error(
        "dependency '{identifier}' is not pinned to a manifest digest; run `ocx package create --platform <PLATFORM>` to resolve it"
    )]
    UnpinnedDependency { identifier: Box<oci::Identifier> },
    /// A dependency's pin map has no key covering the projected platform.
    #[error(
        "dependency '{identifier}' has no manifest pin covering platform '{platform}'; re-run `ocx package create` for this platform"
    )]
    MissingPlatformPin {
        identifier: Box<oci::Identifier>,
        platform: String,
    },
    /// Projected dependencies violate the published-form invariants.
    #[error(transparent)]
    Dependency(#[from] DependencyError),
}

impl ClassifyExitCode for AuthoringError {
    fn classify(&self) -> Option<ExitCode> {
        // Malformed / incomplete metadata is input-data trouble: DataError (65).
        Some(ExitCode::DataError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::{Digest, Platform};
    use crate::package::metadata::visibility::Visibility;

    fn hex(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn digest(ch: char) -> Digest {
        Digest::Sha256(hex(ch))
    }

    fn platform(value: &str) -> Platform {
        value.parse().expect("platform parses")
    }

    fn parse(json: &str) -> AuthoringMetadata {
        serde_json::from_str(json).expect("authoring metadata parses")
    }

    // ── Parsing: relaxed identifier ──────────────────────────────────

    #[test]
    fn tag_only_dependency_accepted() {
        let metadata = parse(
            r#"{"type":"bundle","version":1,
                "dependencies":[{"identifier":"ocx.sh/java:21"}]}"#,
        );
        let dep = metadata.dependencies().iter().next().unwrap();
        assert_eq!(dep.identifier.tag(), Some("21"));
        assert!(dep.identifier.digest().is_none());
        assert!(!dep.is_pinned());
        assert!(!metadata.is_fully_pinned());
    }

    #[test]
    fn bare_name_dependency_rejected() {
        let err = serde_json::from_str::<AuthoringMetadata>(
            r#"{"type":"bundle","version":1,
                "dependencies":[{"identifier":"java:21"}]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("explicit registry"), "unexpected: {err}");
    }

    #[test]
    fn published_json_parses_as_authoring() {
        // Subset compatibility: a published metadata blob (pinned identifier,
        // no sidecar fields) must parse as authoring metadata.
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21@sha256:{}","visibility":"public"}}]}}"#,
            hex('a')
        );
        let published: Metadata = serde_json::from_str(&json).expect("parses as published");
        let authoring: AuthoringMetadata = serde_json::from_str(&json).expect("parses as authoring");
        assert_eq!(published.dependencies().len(), 1);
        let dep = authoring.dependencies().iter().next().unwrap();
        assert!(dep.is_pinned());
        assert_eq!(dep.visibility, Visibility::PUBLIC);
        assert!(authoring.is_fully_pinned());
    }

    #[test]
    fn duplicate_repository_rejected() {
        let err = serde_json::from_str::<AuthoringMetadata>(
            r#"{"type":"bundle","version":1,
                "dependencies":[{"identifier":"ocx.sh/java:21"},{"identifier":"ocx.sh/java:22"}]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "unexpected: {err}");
    }

    #[test]
    fn duplicate_name_rejected() {
        let err = serde_json::from_str::<AuthoringMetadata>(
            r#"{"type":"bundle","version":1,
                "dependencies":[
                    {"identifier":"ocx.sh/java:21","name":"tool"},
                    {"identifier":"ocx.sh/cmake:3","name":"tool"}]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "unexpected: {err}");
    }

    // ── platforms map + target set parsing ───────────────────────────

    #[test]
    fn platforms_map_and_target_set_roundtrip() {
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21",
                    "platforms":{{"linux/amd64":"sha256:{a}","any":"sha256:{b}"}}}}],
                "platforms":["linux/amd64","any"]}}"#,
            a = hex('a'),
            b = hex('b'),
        );
        let metadata = parse(&json);
        assert!(metadata.is_fully_pinned(), "map-bearing dep counts as pinned");
        let set = metadata.target_platforms().expect("target set present");
        assert_eq!(set.len(), 2);

        let reserialized = serde_json::to_string(&metadata).unwrap();
        let metadata2 = parse(&reserialized);
        let dep = metadata2.dependencies().iter().next().unwrap();
        let map = dep.platforms.as_ref().unwrap();
        assert_eq!(map.get("linux/amd64"), Some(&digest('a')));
        assert_eq!(map.get("any"), Some(&digest('b')));
    }

    #[test]
    fn osf_lock_key_roundtrips() {
        // `;osf=` keys are opaque strings in the map — they must survive
        // serde untouched.
        let key = "linux/amd64;osf=libc.glibc";
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21",
                    "platforms":{{"{key}":"sha256:{a}"}}}}]}}"#,
            a = hex('a'),
        );
        let metadata = parse(&json);
        let reserialized = serde_json::to_string(&metadata).unwrap();
        let metadata2 = parse(&reserialized);
        let dep = metadata2.dependencies().iter().next().unwrap();
        assert_eq!(dep.platforms.as_ref().unwrap().get(key), Some(&digest('a')));
    }

    // ── to_published projection matrix ────────────────────────────────

    fn map_dep(entries: &[(&str, char)]) -> String {
        let map = entries
            .iter()
            .map(|(key, ch)| format!(r#""{key}":"sha256:{}""#, hex(*ch)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21","platforms":{{{map}}}}}]}}"#
        )
    }

    #[test]
    fn to_published_passes_through_direct_pin() {
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21@sha256:{}"}}]}}"#,
            hex('a')
        );
        let metadata = parse(&json);
        let published = metadata.to_published(&platform("linux/amd64")).unwrap();
        let dep = published.dependencies().iter().next().unwrap();
        assert_eq!(dep.identifier.digest(), digest('a'));
        assert_eq!(dep.identifier.tag(), Some("21"), "advisory tag preserved");
    }

    #[test]
    fn to_published_collapses_map_exact_key() {
        let metadata = parse(&map_dep(&[("linux/amd64", 'a'), ("darwin/arm64", 'b')]));
        let published = metadata.to_published(&platform("linux/amd64")).unwrap();
        let dep = published.dependencies().iter().next().unwrap();
        assert_eq!(dep.identifier.digest(), digest('a'));
    }

    #[test]
    fn to_published_base_tier_covers_feature_bearing_platform() {
        // A plain `linux/amd64` map entry covers a feature-bearing projected
        // platform via the base tier (lock V2 semantics).
        let metadata = parse(&map_dep(&[("linux/amd64", 'a')]));
        let published = metadata.to_published(&platform("linux/amd64+libc.glibc")).unwrap();
        let dep = published.dependencies().iter().next().unwrap();
        assert_eq!(dep.identifier.digest(), digest('a'));
    }

    #[test]
    fn to_published_any_tier_is_universal() {
        let metadata = parse(&map_dep(&[("any", 'b')]));
        let published = metadata.to_published(&platform("windows/amd64")).unwrap();
        let dep = published.dependencies().iter().next().unwrap();
        assert_eq!(dep.identifier.digest(), digest('b'));
    }

    #[test]
    fn to_published_feature_bearing_entry_does_not_cover_plain_platform() {
        // Fail-closed: a `libc.glibc`-only pin must NOT cover a plain
        // `linux/amd64` projection (matches install-time can_run semantics).
        let metadata = parse(&map_dep(&[("linux/amd64;osf=libc.glibc", 'a')]));
        let err = metadata.to_published(&platform("linux/amd64")).unwrap_err();
        assert!(
            matches!(err, AuthoringError::MissingPlatformPin { .. }),
            "unexpected: {err}"
        );
    }

    #[test]
    fn to_published_map_miss_errors() {
        let metadata = parse(&map_dep(&[("darwin/arm64", 'b')]));
        let err = metadata.to_published(&platform("linux/amd64")).unwrap_err();
        assert!(
            matches!(err, AuthoringError::MissingPlatformPin { .. }),
            "unexpected: {err}"
        );
    }

    #[test]
    fn to_published_unpinned_errors() {
        let metadata = parse(
            r#"{"type":"bundle","version":1,
                "dependencies":[{"identifier":"ocx.sh/java:21"}]}"#,
        );
        let err = metadata.to_published(&platform("linux/amd64")).unwrap_err();
        assert!(
            matches!(err, AuthoringError::UnpinnedDependency { .. }),
            "unexpected: {err}"
        );
    }

    #[test]
    fn projected_serialization_has_no_sidecar_fields() {
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21",
                    "platforms":{{"any":"sha256:{a}"}}}}],
                "platforms":["any"]}}"#,
            a = hex('a'),
        );
        let metadata = parse(&json);
        let published = metadata.to_published(&Platform::any()).unwrap();
        let serialized = serde_json::to_string(&published).unwrap();
        assert!(
            !serialized.contains("platforms"),
            "published form must carry no sidecar fields: {serialized}"
        );
        // And the projected blob is itself valid published metadata.
        let reparsed: Metadata = serde_json::from_str(&serialized).unwrap();
        assert_eq!(reparsed.dependencies().len(), 1);
    }

    #[test]
    fn to_published_preserves_visibility_and_name() {
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21@sha256:{}",
                    "visibility":"public","name":"jdk"}}]}}"#,
            hex('a')
        );
        let metadata = parse(&json);
        let published = metadata.to_published(&Platform::any()).unwrap();
        let dep = published.dependencies().iter().next().unwrap();
        assert_eq!(dep.visibility, Visibility::PUBLIC);
        assert_eq!(dep.name.as_ref().map(|name| name.as_str()), Some("jdk"));
    }

    #[test]
    fn empty_platforms_map_counts_as_unpinned() {
        let metadata = parse(
            r#"{"type":"bundle","version":1,
                "dependencies":[{"identifier":"ocx.sh/java:21","platforms":{}}]}"#,
        );
        assert!(!metadata.is_fully_pinned(), "empty map must not count as pinned");
    }
}
