// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Authoring-form package metadata — the `-metadata.json` sidecar a publisher
//! edits, and `ocx package create`'s **input** format.
//!
//! The sole delta from the published [`Metadata`] is that a dependency
//! identifier may omit its digest — a tag-only identifier means "resolve me at
//! `ocx package create` time". Everything else is byte-identical, so every
//! published `metadata.json` is itself valid authoring input (subset
//! compatibility) and re-running `create` over a compiled sidecar resolves
//! nothing.
//!
//! `create` pins each tag-only dependency against the selected index for its
//! `--platform`, then projects the result with
//! [`AuthoringMetadata::to_published`]. That projection is what gets written
//! beside the bundle and pushed — publishers never hand a registry the
//! authoring form.
//!
//! The platform a bundle was built for is not metadata: on the wire it lives
//! in the OCI image index the registry serves, and between `create` and
//! `push`/`test` in the build receipt written beside the bundle.
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
/// The published [`Metadata`] with one relaxation: dependency digests are
/// optional. See the module docs.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthoringMetadata {
    Bundle(AuthoringBundle),
}

/// Bundle package metadata in authoring form.
///
/// Same shape as the published [`Bundle`], with authoring-form (digest-optional)
/// dependencies.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[expect(
    clippy::manual_non_exhaustive,
    reason = "the private unit field is a serde rejection hook, not an extensibility marker"
)]
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
    /// optional). Array order defines the environment import order.
    #[serde(skip_serializing_if = "AuthoringDependencies::is_empty", default)]
    pub dependencies: AuthoringDependencies,

    /// Named entrypoints that `ocx package install` generates launchers for.
    #[serde(skip_serializing_if = "Entrypoints::is_empty", default)]
    pub entrypoints: Entrypoints,

    /// Rejection sentinel for the retired top-level `platform` key. Never
    /// carries a value — its only job is to make a pre-receipt sidecar fail
    /// loudly instead of having its recorded platform silently ignored. See
    /// [`reject_retired_platform`].
    #[serde(
        rename = "platform",
        default,
        skip_serializing,
        deserialize_with = "reject_retired_platform"
    )]
    #[schemars(skip)]
    #[expect(dead_code, reason = "write-only: the deserializer is the whole point")]
    retired_platform: (),

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

    /// Projects the authoring metadata into the published [`Metadata`].
    ///
    /// Every dependency must carry a digest by now — `ocx package create`
    /// resolves the tag-only ones against the index first, and the published
    /// dependency type has no digest-less form.
    ///
    /// # Errors
    ///
    /// [`AuthoringError::UnpinnedDependency`] when a dependency still has no
    /// digest.
    pub fn to_published(&self) -> Result<Metadata, AuthoringError> {
        match self {
            AuthoringMetadata::Bundle(bundle) => {
                let dependencies = bundle
                    .dependencies
                    .iter()
                    .map(AuthoringDependency::to_published)
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
    /// A dependency carries no digest, so it has no published form.
    #[error(
        "dependency '{identifier}' is not pinned to a manifest digest; run `ocx package create --platform <PLATFORM>` to resolve it"
    )]
    UnpinnedDependency { identifier: Box<oci::Identifier> },
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

/// Refuses a sidecar still carrying the retired top-level `platform` key.
///
/// Serde ignores unknown fields by design (forward-compatibility), which is
/// right for a key nobody has written yet and wrong for one that used to mean
/// something: a pre-receipt sidecar would parse clean, its recorded platform
/// would vanish, and `create` would resolve against whatever `--platform` this
/// invocation happens to pass. Rejected by name — unknown *future* keys stay
/// tolerated.
pub(super) fn reject_retired_platform<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<(), D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer)?;
    Err(serde::de::Error::custom(
        "the `platform` field is no longer part of package metadata; \
         `ocx package create --platform <PLATFORM>` records it in a build receipt beside the bundle instead \
         — drop the field and re-run `ocx package create`",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::Digest;
    use crate::package::metadata::visibility::Visibility;

    fn hex(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn digest(ch: char) -> Digest {
        Digest::Sha256(hex(ch))
    }

    fn parse(json: &str) -> AuthoringMetadata {
        serde_json::from_str(json).expect("authoring metadata parses")
    }

    // ── Parsing: relaxed identifier ──────────────────

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
        // Subset compatibility: a published metadata blob must parse as
        // authoring metadata, so re-running `create` over a compiled sidecar
        // is a no-op resolve.
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
    }

    // ── retired keys are rejected, unknown ones tolerated ──────────────
    //
    // Serde's ignore-unknown default is right for a key nobody has written
    // yet and wrong for one that used to carry meaning: a pre-receipt sidecar
    // would parse clean and lose what it recorded. These three pin both
    // halves of that distinction.

    #[test]
    fn retired_top_level_platform_key_rejected() {
        let err =
            serde_json::from_str::<AuthoringMetadata>(r#"{"type":"bundle","version":1,"platform":"linux/amd64"}"#)
                .expect_err("a pre-receipt sidecar must not parse as if it said nothing");
        let message = err.to_string();
        assert!(message.contains("platform"), "must name the key: {message}");
        assert!(
            message.contains("build receipt"),
            "must say where the platform lives now: {message}"
        );
    }

    #[test]
    fn retired_dependency_platforms_map_rejected() {
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21","platforms":{{"any":"sha256:{}"}}}}]}}"#,
            hex('a')
        );
        let err = serde_json::from_str::<AuthoringMetadata>(&json)
            .expect_err("a pin map must not be silently dropped, leaving the dep to be re-resolved");
        let message = err.to_string();
        assert!(message.contains("platforms"), "must name the key: {message}");
        assert!(
            message.contains("identifier"),
            "must say where the pin belongs now: {message}"
        );
    }

    /// The rejection is exactly-these-keys, not `deny_unknown_fields`: a
    /// sidecar written for a newer ocx must still load on this one.
    #[test]
    fn an_unknown_future_key_still_parses() {
        let metadata = parse(
            r#"{"type":"bundle","version":1,"provenance":{"builder":"acme"},
                "dependencies":[{"identifier":"ocx.sh/java:21","attestation":"x"}]}"#,
        );
        assert_eq!(metadata.dependencies().len(), 1);
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

    // ── to_published projection ────────────────────────

    #[test]
    fn to_published_passes_through_direct_pin() {
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21@sha256:{}"}}]}}"#,
            hex('a')
        );
        let published = parse(&json).to_published().unwrap();
        let dep = published.dependencies().iter().next().unwrap();
        assert_eq!(dep.identifier.digest(), digest('a'));
        assert_eq!(dep.identifier.tag(), Some("21"), "advisory tag preserved");
    }

    #[test]
    fn to_published_unpinned_errors() {
        let metadata = parse(
            r#"{"type":"bundle","version":1,
                "dependencies":[{"identifier":"ocx.sh/java:21"}]}"#,
        );
        let err = metadata.to_published().unwrap_err();
        assert!(
            matches!(err, AuthoringError::UnpinnedDependency { .. }),
            "unexpected: {err}"
        );
    }

    /// The create-writes-the-wire-form contract: what `to_published` produces
    /// is exactly what `ocx package push` will later read back as published
    /// [`Metadata`], and it carries no build-time fields — the platform lives
    /// in the build receipt, never in the sidecar.
    #[test]
    fn to_published_output_reparses_as_published_metadata() {
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21@sha256:{}","visibility":"public"}}]}}"#,
            hex('a')
        );
        let published = parse(&json).to_published().unwrap();
        let serialized = serde_json::to_string(&published).unwrap();

        let reparsed: Metadata = serde_json::from_str(&serialized).expect("projection is valid published metadata");
        assert_eq!(reparsed.dependencies().len(), 1);

        let value: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert!(
            value.get("platform").is_none(),
            "the published sidecar must carry no platform key: {serialized}"
        );
    }

    #[test]
    fn to_published_preserves_visibility_and_name() {
        let json = format!(
            r#"{{"type":"bundle","version":1,
                "dependencies":[{{"identifier":"ocx.sh/java:21@sha256:{}",
                    "visibility":"public","name":"jdk"}}]}}"#,
            hex('a')
        );
        let published = parse(&json).to_published().unwrap();
        let dep = published.dependencies().iter().next().unwrap();
        assert_eq!(dep.visibility, Visibility::PUBLIC);
        assert_eq!(dep.name.as_ref().map(|name| name.as_str()), Some("jdk"));
    }

    // ── binaries: with_binaries builder + to_published projection ───
    //
    // `adr_declared_binaries_metadata.md` §2.1: `with_binaries` is a consuming
    // builder; `to_published`'s struct literal carries
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

        let published = metadata.to_published().unwrap();
        let published_binaries = published
            .binaries()
            .expect("to_published must carry the baked binaries field through");
        assert_eq!(published_binaries.len(), 2);
    }

    #[test]
    fn absent_binaries_stays_absent_through_to_published() {
        let metadata = parse(r#"{"type":"bundle","version":1}"#);
        assert!(metadata.binaries().is_none());

        let published = metadata.to_published().unwrap();
        assert!(
            published.binaries().is_none(),
            "an undeclared binaries field must stay None through to_published, never default to Some(empty)"
        );
    }
}
