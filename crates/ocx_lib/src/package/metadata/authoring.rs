// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Authoring-form package metadata — the `-metadata.json` sidecar a publisher
//! edits and `ocx package create` compiles.
//!
//! The authoring form is a superset of the published [`Metadata`]: dependency
//! identifiers may omit the digest (tag-only, resolved by `create`), and each
//! dependency may carry a per-platform `platforms` pin map. Projection to the
//! published form ([`AuthoringMetadata::to_published`]) collapses each
//! dependency to a single manifest-digest pin and strips the sidecar-only
//! per-dependency pin map **by construction** — the published dependency type
//! simply does not have it.
//!
//! Every published `metadata.json` parses as authoring metadata (subset
//! compatibility), so `ocx package push` reads one type.
//!
//! A bundle targets exactly one platform per `ocx package create`/`push`
//! invocation (`adr_platform_model_unification.md` D5) — there is no
//! bundle-level target-*set*. `create`'s `--platform` (default
//! `Platform::current()`) is the platform every dependency is resolved
//! against, and it is recorded on [`AuthoringBundle::platform`]; `push` and
//! `package test` read that recorded value back
//! ([`AuthoringMetadata::resolve_platform`]) instead of defaulting
//! independently, so the platform label a bundle is published or tested
//! under can never decouple from what its pins were resolved against.
//!
//! ADR: `adr_dependency_manifest_pinning.md`, `adr_platform_model_unification.md`.

pub mod dependency;

pub use dependency::{AuthoringDependencies, AuthoringDependency};

use serde::{Deserialize, Serialize};

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::oci;

use super::Metadata;
use super::binary::Binaries;
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

    /// The platform `ocx package create` resolved dependencies against for
    /// this bundle (canonical grammar string, e.g. `linux/amd64` or `any` —
    /// see [`oci::Platform`]'s `Display`). `ocx package push` and
    /// `ocx package test` default to this value and reject an explicit
    /// `--platform` that disagrees with it, so the platform label a bundle
    /// is published under always matches the platform its dependency pins
    /// were resolved against. Authoring-sidecar-only — stripped at publish.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "platform_field")]
    #[schemars(schema_with = "platform_field_schema")]
    pub platform: Option<oci::Platform>,

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

    /// The interface-binaries claim, hand-authored or baked in by `ocx
    /// package create`'s auto-scan step. Mirrors [`Bundle::binaries`] — see
    /// `adr_declared_binaries_metadata.md` §1, §2.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binaries: Option<Binaries>,
}

impl AuthoringMetadata {
    pub fn dependencies(&self) -> &AuthoringDependencies {
        match self {
            AuthoringMetadata::Bundle(bundle) => &bundle.dependencies,
        }
    }

    /// The interface-binaries claim, or `None` if undeclared / not yet
    /// scanned.
    pub fn binaries(&self) -> Option<&Binaries> {
        match self {
            AuthoringMetadata::Bundle(bundle) => bundle.binaries.as_ref(),
        }
    }

    /// The platform `ocx package create` recorded for this bundle, if any.
    pub fn platform(&self) -> Option<&oci::Platform> {
        match self {
            AuthoringMetadata::Bundle(bundle) => bundle.platform.as_ref(),
        }
    }

    /// Returns `self` with the recorded platform set to `platform`.
    ///
    /// Called by `ocx package create` after resolving dependency pins, so
    /// the sidecar always records the platform its pins were resolved
    /// against.
    #[must_use]
    pub fn with_platform(self, platform: oci::Platform) -> Self {
        match self {
            AuthoringMetadata::Bundle(bundle) => AuthoringMetadata::Bundle(AuthoringBundle {
                platform: Some(platform),
                ..bundle
            }),
        }
    }

    /// Returns `self` with the interface-binaries claim set to `binaries`.
    ///
    /// Called by `ocx package create`'s auto-scan step (Auto/Verify modes)
    /// to bake the scanned or authored claim into the sidecar before
    /// `to_published` projects it. See `adr_declared_binaries_metadata.md`
    /// §2.1.
    #[must_use]
    pub fn with_binaries(self, binaries: Binaries) -> Self {
        match self {
            AuthoringMetadata::Bundle(bundle) => AuthoringMetadata::Bundle(AuthoringBundle {
                binaries: Some(binaries),
                ..bundle
            }),
        }
    }

    /// Resolves the effective platform for `ocx package push` / `ocx
    /// package test`: defaults to the platform `ocx package create`
    /// recorded in the sidecar; an explicit override (`explicit`) is
    /// accepted only when it equals the recorded platform.
    ///
    /// # Errors
    ///
    /// [`AuthoringError::MissingRecordedPlatform`] when the sidecar has no
    /// recorded platform — this cannot happen for a bundle that went
    /// through `ocx package create`, so an absent field is malformed input.
    /// [`AuthoringError::PlatformMismatch`] when `explicit` disagrees with
    /// the recorded platform.
    pub fn resolve_platform(&self, explicit: Option<&oci::Platform>) -> Result<oci::Platform, AuthoringError> {
        let Some(recorded) = self.platform() else {
            return Err(AuthoringError::MissingRecordedPlatform);
        };
        match explicit {
            Some(explicit) if explicit == recorded => Ok(recorded.clone()),
            Some(explicit) => Err(AuthoringError::PlatformMismatch {
                requested: explicit.to_string(),
                recorded: recorded.to_string(),
            }),
            None => Ok(recorded.clone()),
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
    /// [`AuthoringDependency::pin_for`]; the sidecar-only per-dependency pin
    /// map is stripped by construction.
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
                    binaries: bundle.binaries.clone(),
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
    /// A dependency's pin map has two or more keys that tie at the maximum
    /// D1 compatibility score for the projected platform — the pin is
    /// genuinely ambiguous, not missing. Mirrors
    /// [`crate::package::dependency_pinning::DependencyPinningError::AmbiguousPlatform`]
    /// (the fresh-resolve counterpart at `ocx package create` time); this
    /// variant covers the same condition read back from an already-pinned
    /// sidecar `platforms` map.
    #[error(
        "dependency '{identifier}' has an ambiguous manifest pin for platform '{platform}' ({} tied candidates: {}); edit the sidecar `platforms` map to keep a single winner",
        candidates.len(),
        candidates.join(", ")
    )]
    AmbiguousPlatformPin {
        identifier: Box<oci::Identifier>,
        platform: String,
        candidates: Vec<String>,
    },
    /// Projected dependencies violate the published-form invariants.
    #[error(transparent)]
    Dependency(#[from] DependencyError),
    /// An explicit `--platform` passed to `ocx package push` / `ocx package
    /// test` disagrees with the platform `ocx package create` recorded in
    /// the sidecar.
    #[error(
        "requested platform '{requested}' does not match the platform '{recorded}' recorded by `ocx package create`; \
         drop --platform to publish under '{recorded}', or re-run `ocx package create --platform {requested}`"
    )]
    PlatformMismatch { requested: String, recorded: String },
    /// The sidecar carries no recorded platform. `ocx package create`
    /// always writes one, so an absent field means the sidecar predates it
    /// or was hand-authored without ever running `create` — either way,
    /// malformed input for `push`/`test`.
    #[error(
        "metadata sidecar has no recorded platform; run `ocx package create --platform <PLATFORM>` to establish one"
    )]
    MissingRecordedPlatform,
}

impl ClassifyExitCode for AuthoringError {
    fn classify(&self) -> Option<ExitCode> {
        // Malformed / incomplete metadata is input-data trouble: DataError (65).
        Some(ExitCode::DataError)
    }
}

/// Serializes [`AuthoringBundle::platform`] as its canonical grammar string
/// (D2) — the same encoding used for `ocx.lock` / dependency pin-map keys —
/// rather than [`oci::Platform`]'s own `Serialize`/`Deserialize`, which goes
/// through the OCI JSON object shape (`{"os":...,"architecture":...}`).
mod platform_field {
    use std::str::FromStr;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::oci::Platform;

    pub fn serialize<S: Serializer>(value: &Option<Platform>, serializer: S) -> Result<S::Ok, S::Error> {
        value.as_ref().map(Platform::to_string).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Platform>, D::Error> {
        let raw: Option<String> = Option::deserialize(deserializer)?;
        raw.map(|value| Platform::from_str(&value).map_err(serde::de::Error::custom))
            .transpose()
    }
}

/// Manual JSON Schema for [`AuthoringBundle::platform`] — [`oci::Platform`]
/// has no `schemars::JsonSchema` impl of its own (its derived
/// `Serialize`/`Deserialize` produce the OCI JSON object shape, which would
/// mismatch the canonical-string wire format [`platform_field`] actually
/// writes for this field).
fn platform_field_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "description": "The platform `ocx package create` resolved dependencies against, in the \
                        canonical grammar (`os/arch[/variant][+feature[,feature...]]` \
                        or `any`), e.g. `linux/amd64`. Written by `ocx package create`; read by \
                        `ocx package push` / `ocx package test` as the default publish/test target."
    })
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

    // ── per-dependency platforms map parsing ───────────────────────────

    #[test]
    fn platforms_map_roundtrips() {
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21",
                    "platforms":{{"linux/amd64":"sha256:{a}","any":"sha256:{b}"}}}}]}}"#,
            a = hex('a'),
            b = hex('b'),
        );
        let metadata = parse(&json);
        assert!(metadata.is_fully_pinned(), "map-bearing dep counts as pinned");

        let reserialized = serde_json::to_string(&metadata).unwrap();
        let metadata2 = parse(&reserialized);
        let dep = metadata2.dependencies().iter().next().unwrap();
        let map = dep.platforms.as_ref().unwrap();
        assert_eq!(map.get("linux/amd64"), Some(&digest('a')));
        assert_eq!(map.get("any"), Some(&digest('b')));
    }

    #[test]
    fn feature_bearing_key_roundtrips() {
        let key = "linux/amd64+libc.glibc";
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

    /// D3: a platform-map key that fails to parse as a canonical [`Platform`]
    /// grammar string (the old `lock_key` family's `;osf=` marker syntax, now
    /// deleted) is rejected at deserialize, never silently kept as an opaque
    /// string.
    #[test]
    fn noncanonical_lock_key_syntax_rejected() {
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21",
                    "platforms":{{"linux/amd64;osf=libc.glibc":"sha256:{a}"}}}}]}}"#,
            a = hex('a'),
        );
        let err = serde_json::from_str::<AuthoringMetadata>(&json)
            .expect_err("the old `;osf=` marker syntax must not parse as a canonical Platform key");
        assert!(!err.to_string().is_empty());
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
        // `linux/amd64` projection (`is_compatible` requires the offer's
        // declared os_features to be a subset of what the requirement has —
        // here the requirement declares none).
        let metadata = parse(&map_dep(&[("linux/amd64+libc.glibc", 'a')]));
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
                    "platforms":{{"any":"sha256:{a}"}}}}]}}"#,
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

    // ── binaries: with_binaries builder + to_published projection ───────
    //
    // `adr_declared_binaries_metadata.md` §2.1: `with_binaries` is a
    // consuming builder (exact `with_platform` precedent); `to_published`'s
    // signature is unchanged, its struct literal carries
    // `binaries: bundle.binaries.clone()` verbatim.

    /// Builds a real `Binaries` value through the public parse API (a
    /// bare `Bundle` JSON blob), since `Binaries`'s tuple field is private
    /// to `binary.rs` and cannot be constructed directly from this module.
    fn some_binaries() -> Binaries {
        let bundle: crate::package::metadata::bundle::Bundle =
            serde_json::from_str(r#"{"version":1,"binaries":["cmake","ctest"]}"#).expect("fixture bundle parses");
        bundle.binaries().expect("fixture declares binaries").clone()
    }

    #[test]
    fn with_binaries_bakes_field_and_to_published_carries_it() {
        let metadata = parse(r#"{"type":"bundle","version":1}"#);
        assert!(metadata.binaries().is_none(), "fixture starts undeclared");

        let metadata = metadata.with_binaries(some_binaries());
        assert_eq!(
            metadata.binaries().map(Binaries::len),
            Some(2),
            "with_binaries must bake the claim into the sidecar value"
        );

        let published = metadata.to_published(&Platform::any()).unwrap();
        let published_binaries = published
            .binaries()
            .expect("to_published must carry the baked binaries field through");
        assert_eq!(published_binaries.len(), 2);
    }

    #[test]
    fn absent_binaries_stays_absent_through_to_published() {
        let metadata = parse(r#"{"type":"bundle","version":1}"#);
        assert!(metadata.binaries().is_none());

        let published = metadata.to_published(&Platform::any()).unwrap();
        assert!(
            published.binaries().is_none(),
            "an undeclared binaries field must stay None through to_published, never default to Some(empty)"
        );
    }
}
