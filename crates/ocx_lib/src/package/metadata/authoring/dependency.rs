// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::oci;
use crate::package::metadata::dependency::{Dependency, DependencyError, DependencyName, default_dependency_name};
use crate::package::metadata::visibility::Visibility;
use crate::project::lookup_host_leaf;

use super::AuthoringError;

/// A dependency in authoring (sidecar) form.
///
/// Unlike the published [`Dependency`], the identifier's digest is optional:
/// a tag-only identifier declares "resolve me at `ocx package create` time".
/// The optional `platforms` map carries per-platform manifest pins (key =
/// [`Platform::lock_key`](crate::oci::Platform::lock_key), the lock V2
/// encoding) for packages whose dependency ships platform-specific manifests.
///
/// Projection to the published form ([`AuthoringDependency::pin_for`])
/// collapses the map to a single [`PinnedIdentifier`](oci::PinnedIdentifier);
/// the sidecar-only `platforms` field is stripped by construction because the
/// published type has no such field.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AuthoringDependency {
    /// OCX identifier with a required explicit registry. The digest is
    /// optional in the authoring form: absent means "pin me at
    /// `ocx package create` time". The tag is advisory once a digest or
    /// platforms map is present.
    pub identifier: oci::Identifier,

    /// Controls how this dependency's environment variables propagate.
    /// Default: `sealed` — no env contribution.
    #[serde(default)]
    pub visibility: Visibility,

    /// Optional name for this dependency used in `${deps.NAME.installPath}`
    /// interpolation. Defaults to the last path segment of the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<DependencyName>,

    /// Per-platform manifest pins written by `ocx package create` when the
    /// dependency ships platform-specific manifests. Key = platform lock key
    /// (e.g. `linux/amd64`, `any`), value = that platform's manifest digest.
    /// Authoring sidecar only — stripped at publish.
    ///
    /// Deserialization rejects duplicate JSON keys (see
    /// [`deserialize_platforms`]) instead of silently keeping the last value
    /// — the same registry-data-unsafe class of bug fixed for
    /// [`Entrypoints`](crate::package::metadata::entrypoint::Entrypoints)'s
    /// custom `MapAccess` deserializer.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_platforms"
    )]
    pub platforms: Option<BTreeMap<String, oci::Digest>>,
}

/// Deserializes [`AuthoringDependency::platforms`], rejecting duplicate JSON
/// keys.
///
/// `BTreeMap`'s derived `Deserialize` silently keeps the last value on a
/// duplicate key (`serde_json`'s default `MapAccess` consumption). That is
/// unsafe for a registry-facing pin map edited by hand or by tooling — a
/// duplicate platform key most likely indicates a publisher mistake that
/// should surface as an error, not silently drop a pin.
fn deserialize_platforms<'de, D>(deserializer: D) -> Result<Option<BTreeMap<String, oci::Digest>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct PlatformsVisitor;

    impl<'de> serde::de::Visitor<'de> for PlatformsVisitor {
        type Value = Option<BTreeMap<String, oci::Digest>>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a map of platform lock key to manifest digest, or null")
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_some<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: serde::Deserializer<'de>,
        {
            deserializer.deserialize_map(DuplicateRejectingMapVisitor).map(Some)
        }
    }

    struct DuplicateRejectingMapVisitor;

    impl<'de> serde::de::Visitor<'de> for DuplicateRejectingMapVisitor {
        type Value = BTreeMap<String, oci::Digest>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a map of platform lock key to manifest digest")
        }

        fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
        where
            M: serde::de::MapAccess<'de>,
        {
            let mut entries: BTreeMap<String, oci::Digest> = BTreeMap::new();
            while let Some(key) = map.next_key::<String>()? {
                let value: oci::Digest = map.next_value()?;
                match entries.entry(key) {
                    Entry::Occupied(occupied) => {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate platform key '{}' in dependency platforms map",
                            occupied.key()
                        )));
                    }
                    Entry::Vacant(vacant) => {
                        vacant.insert(value);
                    }
                }
            }
            Ok(entries)
        }
    }

    deserializer.deserialize_option(PlatformsVisitor)
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

    /// `true` when this dependency carries a direct digest pin or a non-empty
    /// platforms map — i.e. `ocx package push` can project it without network
    /// resolution.
    pub fn is_pinned(&self) -> bool {
        self.identifier.digest().is_some() || self.platforms.as_ref().is_some_and(|map| !map.is_empty())
    }

    /// Returns the direct digest pin, when present.
    pub fn pinned(&self) -> Option<oci::PinnedIdentifier> {
        self.identifier
            .digest()
            .is_some()
            .then(|| oci::PinnedIdentifier::try_from(self.identifier.clone()).expect("digest presence checked above"))
    }

    /// Projects this dependency to the single manifest pin for `platform`.
    ///
    /// A direct digest pin is universal (passes through untouched). A
    /// platforms map is consulted via the lock V2 3-tier lookup: exact
    /// [`lock_key`](crate::oci::Platform::lock_key), then
    /// [`base_lock_key`](crate::oci::Platform::base_lock_key), then `"any"`.
    ///
    /// # Errors
    ///
    /// [`AuthoringError::UnpinnedDependency`] when the dependency has neither
    /// a digest nor a platforms map; [`AuthoringError::MissingPlatformPin`]
    /// when the map has no key covering `platform`.
    pub fn pin_for(&self, platform: &oci::Platform) -> Result<oci::PinnedIdentifier, AuthoringError> {
        if let Some(pinned) = self.pinned() {
            return Ok(pinned);
        }
        let Some(map) = self.platforms.as_ref().filter(|map| !map.is_empty()) else {
            return Err(AuthoringError::UnpinnedDependency {
                identifier: Box::new(self.identifier.clone()),
            });
        };
        let Some(digest) = lookup_host_leaf(map, platform) else {
            return Err(AuthoringError::MissingPlatformPin {
                identifier: Box::new(self.identifier.clone()),
                platform: platform.to_string(),
            });
        };
        let pinned_identifier = self.identifier.clone_with_digest(digest.clone());
        Ok(oci::PinnedIdentifier::try_from(pinned_identifier).expect("digest just attached"))
    }

    /// Projects this dependency to its published form for `platform`.
    pub fn to_published(&self, platform: &oci::Platform) -> Result<Dependency, AuthoringError> {
        Ok(Dependency {
            identifier: self.pin_for(platform)?,
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
    /// Maximum number of dependencies permitted in a single sidecar.
    ///
    /// `ocx package push`'s pre-push gate (`verify_dependency_pins`) issues
    /// one authenticated registry GET per unique dependency pin, driven by
    /// this externally-editable sidecar. Bounding the count here bounds
    /// push-time network fan-out, so a maliciously (or accidentally) edited
    /// sidecar cannot be used to sweep thousands of internal hosts/ports
    /// (SSRF/DoS mitigation).
    pub const MAX_DEPENDENCIES: usize = 256;

    pub fn new(entries: Vec<AuthoringDependency>) -> Result<Self, DependencyError> {
        if entries.len() > Self::MAX_DEPENDENCIES {
            return Err(DependencyError::TooManyDependencies {
                count: entries.len(),
                max: Self::MAX_DEPENDENCIES,
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

    fn hex(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn dep(repo_index: usize) -> AuthoringDependency {
        let json = format!(r#"{{"identifier":"example.com/dep{repo_index}:1"}}"#);
        serde_json::from_str(&json).expect("dependency parses")
    }

    // ── H2: dependency count cap (SSRF/DoS mitigation) ────────────────────────

    #[test]
    fn new_accepts_exactly_max_dependencies() {
        let entries: Vec<_> = (0..AuthoringDependencies::MAX_DEPENDENCIES).map(dep).collect();
        assert!(
            AuthoringDependencies::new(entries).is_ok(),
            "the max count itself must be accepted"
        );
    }

    #[test]
    fn new_rejects_more_than_max_dependencies() {
        let entries: Vec<_> = (0..AuthoringDependencies::MAX_DEPENDENCIES + 1).map(dep).collect();
        let err = AuthoringDependencies::new(entries).expect_err("257 distinct deps must be rejected");
        assert!(
            matches!(
                err,
                DependencyError::TooManyDependencies { count, max }
                    if count == AuthoringDependencies::MAX_DEPENDENCIES + 1 && max == AuthoringDependencies::MAX_DEPENDENCIES
            ),
            "expected TooManyDependencies, got: {err}"
        );
        assert_eq!(err.classify(), Some(ExitCode::DataError));
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

    // ── W6: duplicate platforms keys must error, not last-win ─────────────────

    #[test]
    fn platforms_map_rejects_duplicate_keys() {
        let json = format!(
            r#"{{"identifier":"example.com/dep","platforms":{{"linux/amd64":"sha256:{a}","linux/amd64":"sha256:{b}"}}}}"#,
            a = hex('a'),
            b = hex('b'),
        );
        let err = serde_json::from_str::<AuthoringDependency>(&json)
            .expect_err("duplicate platform keys must be rejected, not last-wins");
        assert!(
            err.to_string().contains("duplicate"),
            "expected a duplicate-key error, got: {err}"
        );
    }

    #[test]
    fn platforms_map_accepts_unique_keys() {
        let json = format!(
            r#"{{"identifier":"example.com/dep","platforms":{{"linux/amd64":"sha256:{a}","any":"sha256:{b}"}}}}"#,
            a = hex('a'),
            b = hex('b'),
        );
        let parsed: AuthoringDependency = serde_json::from_str(&json).expect("unique keys must parse");
        assert_eq!(parsed.platforms.as_ref().unwrap().len(), 2);
    }
}
