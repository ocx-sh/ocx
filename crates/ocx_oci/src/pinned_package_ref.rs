// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use super::{Digest, PackageRef};

/// A validated [`PackageRef`] guaranteed to carry a digest.
///
/// This is used in `resolve.json` to persist the fully resolved dependency
/// graph at install time.  The digest guarantee means consumers never need
/// fallback resolution logic.
///
/// Equality and hashing include all fields (registry, repository, tag, digest).
/// When you need content-identity semantics that ignore the advisory tag, use
/// [`eq_content`](Self::eq_content) for ad-hoc comparisons.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PinnedPackageRef(PackageRef);

impl PinnedPackageRef {
    /// Returns the digest.  Always present by construction.
    pub fn digest(&self) -> Digest {
        self.0.digest().expect("PinnedPackageRef always has a digest")
    }

    /// Content-identity comparison: equal if registry, repository, and digest
    /// match.  The advisory tag is ignored.
    pub fn eq_content(&self, other: &Self) -> bool {
        self.0.registry() == other.0.registry()
            && self.0.repository() == other.0.repository()
            && self.digest() == other.digest()
    }

    /// Returns a copy with the advisory tag stripped.
    ///
    /// Use this before inserting into `HashMap`/`HashSet` when deduplication
    /// should ignore the tag.
    pub fn strip_advisory(&self) -> Self {
        Self(self.0.without_tag())
    }

    /// Returns a copy with the digest replaced. Tag (if any) is preserved.
    pub fn clone_with_digest(&self, digest: Digest) -> Self {
        Self(self.0.clone_with_digest(digest))
    }

    /// Returns a borrow of the inner [`PackageRef`].
    ///
    /// Prefer this over the `Deref` impl plus reference gymnastics
    /// (e.g. `&**pinned`) when callers need an explicit `&PackageRef` —
    /// for example, when constructing `crate::Repository::from(&PackageRef)`.
    pub fn as_identifier(&self) -> &PackageRef {
        &self.0
    }
}

impl std::ops::Deref for PinnedPackageRef {
    type Target = PackageRef;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<PinnedPackageRef> for PackageRef {
    fn from(pinned: PinnedPackageRef) -> Self {
        pinned.0
    }
}

impl TryFrom<PackageRef> for PinnedPackageRef {
    type Error = PinnedIdentifierError;

    fn try_from(id: PackageRef) -> Result<Self, Self::Error> {
        if id.digest().is_none() {
            return Err(PinnedIdentifierError { identifier: id });
        }
        Ok(PinnedPackageRef(id))
    }
}

impl std::fmt::Display for PinnedPackageRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for PinnedPackageRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PinnedPackageRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let id = PackageRef::parse(&s).map_err(serde::de::Error::custom)?;
        PinnedPackageRef::try_from(id).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for PinnedPackageRef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("PinnedPackageRef")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Pinned OCI identifier with a required digest and an optional advisory tag: 'registry/repository[:tag]@digest'.",
            "examples": [
                "ocx.sh/cmake:3.28@sha256:43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9",
                "ocx.sh/cmake@sha256:43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9"
            ]
        })
    }
}

/// A pinned identifier requires a digest but none was present.
#[derive(Debug, thiserror::Error)]
#[error("pinned identifier requires a digest: {identifier}")]
pub struct PinnedIdentifierError {
    pub identifier: PackageRef,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256_hex() -> String {
        "a".repeat(64)
    }

    fn make_id_with_digest() -> PackageRef {
        PackageRef::new_registry("cmake", "example.com").clone_with_digest(Digest::Sha256(sha256_hex()))
    }

    fn make_id_with_tag_and_digest() -> PackageRef {
        PackageRef::new_registry("cmake", "example.com")
            .clone_with_tag("3.28")
            .clone_with_digest(Digest::Sha256(sha256_hex()))
    }

    fn make_id_without_digest() -> PackageRef {
        PackageRef::new_registry("cmake", "example.com").clone_with_tag("3.28")
    }

    #[test]
    fn try_from_with_digest_succeeds() {
        let id = make_id_with_digest();
        let pinned = PinnedPackageRef::try_from(id.clone()).unwrap();
        assert_eq!(pinned.registry(), id.registry());
        assert_eq!(pinned.repository(), id.repository());
        assert_eq!(pinned.digest(), id.digest().unwrap());
    }

    #[test]
    fn try_from_without_digest_fails() {
        let id = make_id_without_digest();
        assert!(PinnedPackageRef::try_from(id).is_err());
    }

    #[test]
    fn try_from_bare_identifier_fails() {
        let id = PackageRef::new_registry("cmake", "example.com");
        assert!(PinnedPackageRef::try_from(id).is_err());
    }

    #[test]
    fn try_from_preserves_tag() {
        let id = make_id_with_tag_and_digest();
        assert!(id.tag().is_some());
        let pinned = PinnedPackageRef::try_from(id).unwrap();
        assert_eq!(pinned.tag(), Some("3.28"));
    }

    #[test]
    fn deref_exposes_identifier_methods() {
        let id = make_id_with_digest();
        let pinned = PinnedPackageRef::try_from(id).unwrap();
        assert_eq!(pinned.registry(), "example.com");
        assert_eq!(pinned.repository(), "cmake");
    }

    #[test]
    fn into_identifier_roundtrip() {
        let id = make_id_with_digest();
        let pinned = PinnedPackageRef::try_from(id.clone()).unwrap();
        let back: PackageRef = pinned.into();
        assert_eq!(back, id);
    }

    #[test]
    fn display_matches_inner_identifier() {
        let id = make_id_with_digest();
        let pinned = PinnedPackageRef::try_from(id.clone()).unwrap();
        assert_eq!(pinned.to_string(), id.to_string());
    }

    #[test]
    fn serde_roundtrip() {
        let id = make_id_with_digest();
        let pinned = PinnedPackageRef::try_from(id).unwrap();
        let json = serde_json::to_string(&pinned).unwrap();
        let deserialized: PinnedPackageRef = serde_json::from_str(&json).unwrap();
        assert_eq!(pinned, deserialized);
    }

    #[test]
    fn json_schema_has_specialized_description() {
        let schema = schemars::schema_for!(PinnedPackageRef);
        let json = serde_json::to_value(&schema).unwrap();
        // schema_for! wraps in a root $ref; the actual definition may be at
        // the top level or under $defs depending on schemars version.
        let description = json
            .get("description")
            .or_else(|| json.pointer("/$defs/PinnedPackageRef/description"))
            .and_then(|v| v.as_str())
            .expect("schema must have a description");
        assert!(
            description.contains("digest"),
            "description should mention digest: {description}"
        );
        assert!(
            description.contains("advisory tag"),
            "description should mention advisory tag: {description}"
        );
    }

    #[test]
    fn equality_includes_tag() {
        let with_tag = PinnedPackageRef::try_from(make_id_with_tag_and_digest()).unwrap();
        let without_tag = PinnedPackageRef::try_from(make_id_with_digest()).unwrap();
        assert_ne!(with_tag, without_tag, "full equality must distinguish tags");
    }

    #[test]
    fn eq_content_ignores_tag() {
        let with_tag = PinnedPackageRef::try_from(make_id_with_tag_and_digest()).unwrap();
        let without_tag = PinnedPackageRef::try_from(make_id_with_digest()).unwrap();
        assert!(with_tag.eq_content(&without_tag));
    }

    #[test]
    fn strip_advisory_enables_content_dedup() {
        use std::collections::HashSet;
        let with_tag = PinnedPackageRef::try_from(make_id_with_tag_and_digest()).unwrap();
        let without_tag = PinnedPackageRef::try_from(make_id_with_digest()).unwrap();
        assert_eq!(with_tag.strip_advisory(), without_tag.strip_advisory());

        let mut set = HashSet::new();
        set.insert(with_tag.strip_advisory());
        assert!(
            !set.insert(without_tag.strip_advisory()),
            "stripped dedup should prevent second insert"
        );
    }

    #[test]
    fn deserialize_rejects_missing_registry() {
        let json = format!(r#""cmake@sha256:{}""#, sha256_hex());
        let err = serde_json::from_str::<PinnedPackageRef>(&json).unwrap_err();
        assert!(err.to_string().contains("explicit registry"));
    }

    #[test]
    fn deserialize_preserves_tag() {
        let json = format!(r#""example.com/cmake:3.28@sha256:{}""#, sha256_hex());
        let pinned: PinnedPackageRef = serde_json::from_str(&json).unwrap();
        assert_eq!(pinned.tag(), Some("3.28"));
        assert_eq!(pinned.repository(), "cmake");
    }

    #[test]
    fn clone_with_digest_replaces_digest_preserves_tag() {
        let pinned = PinnedPackageRef::try_from(make_id_with_tag_and_digest()).unwrap();
        let new_digest = Digest::Sha256("b".repeat(64));
        let replaced = pinned.clone_with_digest(new_digest.clone());
        assert_eq!(replaced.digest(), new_digest);
        assert_eq!(replaced.registry(), pinned.registry());
        assert_eq!(replaced.repository(), pinned.repository());
        assert_eq!(replaced.tag(), pinned.tag());
    }

    #[test]
    fn deserialize_rejects_missing_digest() {
        let hex = sha256_hex();
        let json = format!(r#""example.com/cmake@sha256:{hex}""#);
        let deserialized = serde_json::from_str::<PinnedPackageRef>(&json);
        assert!(deserialized.is_ok());

        let json_no_digest = r#""example.com/cmake""#;
        let err = serde_json::from_str::<PinnedPackageRef>(json_no_digest).unwrap_err();
        assert!(err.to_string().contains("digest"));
    }
}
