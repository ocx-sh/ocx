// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::oci;
use crate::package::metadata::dependency::{
    Dependencies, Dependency, DependencyError, DependencyName, default_dependency_name,
};
use crate::package::metadata::visibility::Visibility;

use super::AuthoringError;

/// A dependency in authoring (sidecar) form.
///
/// Unlike the published [`Dependency`], the identifier's digest is optional:
/// a tag-only identifier declares "resolve me at `ocx package create` time".
/// `create` resolves it against the selected index for its `--platform` and
/// attaches the winning platform manifest's digest to the identifier itself,
/// so the projection to the published form
/// ([`AuthoringDependency::to_published`]) is a straight pass-through.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[expect(
    clippy::manual_non_exhaustive,
    reason = "the private unit field is a serde rejection hook, not an extensibility marker"
)]
pub struct AuthoringDependency {
    /// OCX identifier with a required explicit registry. The digest is
    /// optional in the authoring form: absent means "pin me at
    /// `ocx package create` time". The tag is advisory once a digest is
    /// present.
    pub identifier: oci::Identifier,

    /// Controls how this dependency's environment variables propagate.
    /// Default: `sealed` — no env contribution.
    #[serde(default)]
    pub visibility: Visibility,

    /// Optional name for this dependency used in `${deps.NAME.installPath}`
    /// interpolation. Defaults to the last path segment of the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<DependencyName>,

    /// Rejection sentinel for the retired per-platform `platforms` pin map.
    /// Never carries a value — see [`reject_retired_platforms`].
    #[serde(
        rename = "platforms",
        default,
        skip_serializing,
        deserialize_with = "reject_retired_platforms"
    )]
    #[schemars(skip)]
    #[expect(dead_code, reason = "write-only: the deserializer is the whole point")]
    retired_platforms: (),
}

/// Refuses a dependency still carrying the retired `platforms` pin map.
///
/// The silent-drift case this exists for: serde would ignore the map, the
/// dependency would read as tag-only, and `ocx package create` would re-resolve
/// the mutable tag — swapping out the digest the publisher had locked without
/// saying a word. Rejected by name; unknown *future* keys stay tolerated.
fn reject_retired_platforms<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<(), D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer)?;
    Err(serde::de::Error::custom(
        "the dependency `platforms` pin map is no longer supported; a dependency now carries its manifest \
         digest directly on the identifier (`registry/repo:tag@sha256:...`) — move the pin there to keep it, \
         or drop the field and re-run `ocx package create` to re-resolve it",
    ))
}

impl AuthoringDependency {
    /// Returns the interpolation name for this dependency (explicit `name`
    /// or a slugified form of the repository basename). Mirrors
    /// [`Dependency::name`] — OCI repository grammar permits characters
    /// (notably `.`, e.g. a repository named `open.jdk`) the slug grammar
    /// does not, so the basename is sanitized via
    /// [`default_dependency_name`] rather than asserted. Never panics.
    pub fn name(&self) -> DependencyName {
        if let Some(name) = &self.name {
            return name.clone();
        }
        default_dependency_name(self.identifier.name())
    }

    /// `true` when this dependency carries a digest — i.e. `ocx package
    /// create` has already resolved it, or the publisher pinned it by hand.
    pub fn is_pinned(&self) -> bool {
        self.identifier.digest().is_some()
    }

    /// Returns the digest pin, when present.
    pub fn pinned(&self) -> Option<oci::PinnedIdentifier> {
        self.identifier
            .digest()
            .is_some()
            .then(|| oci::PinnedIdentifier::try_from(self.identifier.clone()).expect("digest presence checked above"))
    }

    /// Projects this dependency to its published form.
    ///
    /// # Errors
    ///
    /// [`AuthoringError::UnpinnedDependency`] when the dependency carries no
    /// digest — the published form has no digest-less shape to project into.
    pub fn to_published(&self) -> Result<Dependency, AuthoringError> {
        Ok(Dependency {
            identifier: self.pinned().ok_or_else(|| AuthoringError::UnpinnedDependency {
                identifier: Box::new(self.identifier.clone()),
            })?,
            visibility: self.visibility,
            name: self.name.clone(),
        })
    }
}

/// Ordered list of authoring-form dependencies.
///
/// Serializes as a JSON array; array position defines the canonical
/// environment import order. Construction and deserialization enforce the
/// same invariants as the published [`Dependencies`](crate::package::metadata::dependency::Dependencies):
/// explicit registry per identifier (via [`oci::Identifier`]'s deserializer),
/// unique `(registry, repository)` pairs, unique explicit names.
#[derive(Debug, Clone, Default)]
pub struct AuthoringDependencies {
    entries: Vec<AuthoringDependency>,
}

impl AuthoringDependencies {
    pub fn new(entries: Vec<AuthoringDependency>) -> Result<Self, DependencyError> {
        // The cap belongs to the published collection — that is the form the
        // pre-push SSRF/DoS gate reads. Applying it here too reports an
        // over-long list at authoring time instead of only on projection.
        if entries.len() > Dependencies::MAX_DEPENDENCIES {
            return Err(DependencyError::TooManyDependencies {
                count: entries.len(),
                max: Dependencies::MAX_DEPENDENCIES,
            });
        }
        let mut seen_ids = HashSet::new();
        let mut seen_names: HashSet<DependencyName> = HashSet::new();
        for dep in &entries {
            if let Some(name) = &dep.name
                && !seen_names.insert(name.clone())
            {
                return Err(DependencyError::DuplicateName { name: name.to_string() });
            }
            let key = (
                dep.identifier.registry().to_string(),
                dep.identifier.repository().to_string(),
            );
            if !seen_ids.insert(key) {
                return Err(DependencyError::DuplicateRepository {
                    repository: format!("{}/{}", dep.identifier.registry(), dep.identifier.repository()),
                });
            }
        }
        Ok(Self { entries })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, AuthoringDependency> {
        self.entries.iter()
    }
}

impl<'a> IntoIterator for &'a AuthoringDependencies {
    type Item = &'a AuthoringDependency;
    type IntoIter = std::slice::Iter<'a, AuthoringDependency>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl Serialize for AuthoringDependencies {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.entries.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AuthoringDependencies {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let entries = Vec::<AuthoringDependency>::deserialize(deserializer)?;
        AuthoringDependencies::new(entries).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for AuthoringDependencies {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("AuthoringDependencies")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Vec<AuthoringDependency>>::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ClassifyExitCode, ExitCode};

    fn dep(repo_index: usize) -> AuthoringDependency {
        let json = format!(r#"{{"identifier":"example.com/dep{repo_index}:1"}}"#);
        serde_json::from_str(&json).expect("dependency parses")
    }

    // ── H2: dependency count cap (SSRF/DoS mitigation) ────────────────────────

    #[test]
    fn new_accepts_exactly_max_dependencies() {
        let entries: Vec<_> = (0..Dependencies::MAX_DEPENDENCIES).map(dep).collect();
        assert!(
            AuthoringDependencies::new(entries).is_ok(),
            "the max count itself must be accepted"
        );
    }

    #[test]
    fn new_rejects_more_than_max_dependencies() {
        let entries: Vec<_> = (0..Dependencies::MAX_DEPENDENCIES + 1).map(dep).collect();
        let err = AuthoringDependencies::new(entries).expect_err("257 distinct deps must be rejected");
        assert!(
            matches!(
                err,
                DependencyError::TooManyDependencies { count, max }
                    if count == Dependencies::MAX_DEPENDENCIES + 1 && max == Dependencies::MAX_DEPENDENCIES
            ),
            "expected TooManyDependencies, got: {err}"
        );
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    /// The cap must hold on the deserialize path too — `ocx package create`
    /// reads the authoring sidecar as bytes, never through
    /// `AuthoringDependencies::new` directly.
    #[test]
    fn deserializing_more_than_max_dependencies_is_rejected() {
        let entries = (0..Dependencies::MAX_DEPENDENCIES + 1)
            .map(|index| format!(r#"{{"identifier":"example.com/dep{index}:1"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let err = serde_json::from_str::<AuthoringDependencies>(&format!("[{entries}]"))
            .expect_err("an over-long dependency array must not deserialize");
        assert!(err.to_string().contains("too many"), "unexpected: {err}");
    }

    // ── H3: name() must not panic on OCI-legal, slug-illegal basenames ────────

    #[test]
    fn name_derives_valid_slug_from_dotted_repository_basename() {
        let dep: AuthoringDependency =
            serde_json::from_str(r#"{"identifier":"example.com/open.jdk:21"}"#).expect("dependency parses");
        // Must not panic; must produce a valid DependencyName.
        let name = dep.name();
        assert_eq!(name.as_str(), "open-jdk");
    }
}
