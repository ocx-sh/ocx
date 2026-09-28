// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use super::package_ref::error::{IdentifierError, IdentifierErrorKind};
use super::{Digest, PackageRef, PinnedPackageRef, native};

const DOCKER_HUB_DOMAINS: &[&str] = &["docker.io", "index.docker.io"];

/// A **physical** OCI location: the registry and repository a request dials.
///
/// No conversion to or from [`PackageRef`] exists, or a package name reaches the
/// wire unrouted; obtain one from `ocx_index::Index::route*` or the constructors
/// below, each call site pinned by `oci_identifier_mint_ratchet`. Never
/// deserializes: nothing OCX persists names a physical location.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct OciIdentifier(PackageRef);

impl OciIdentifier {
    /// Parses a location the user named as a write target (`--to`, `-i`, a
    /// managed-config source); nothing routes it.
    ///
    /// # Errors
    ///
    /// [`IdentifierError`] as [`PackageRef::parse_with_default_registry`] raises it.
    pub fn parse_target(input: &str, default_registry: &str) -> Result<Self, IdentifierError> {
        PackageRef::parse_with_default_registry(input, default_registry).map(Self)
    }

    /// Parses an index root's `repository` pointer (`oci://host/path`), strictly
    /// (`adr_index_indirection.md` C3).
    ///
    /// `host/path` must round-trip exactly through the identifier grammar, or a
    /// remote-authored pointer smuggles a tag, digest or stray character in.
    ///
    /// # Errors
    ///
    /// [`IdentifierError`] of kind [`IdentifierErrorKind::InvalidFormat`] whose
    /// `input` is `value`.
    pub fn parse_repository_pointer(value: &str) -> Result<Self, IdentifierError> {
        let malformed = || IdentifierError::new(value, IdentifierErrorKind::InvalidFormat);
        let rest = value.strip_prefix("oci://").ok_or_else(malformed)?;
        let (host, path) = rest.split_once('/').ok_or_else(malformed)?;
        if host.is_empty() || path.is_empty() {
            return Err(malformed());
        }
        let parsed = PackageRef::parse_with_default_registry(rest, host).map_err(|_| malformed())?;
        if parsed.registry() != host
            || parsed.repository() != path
            || parsed.tag().is_some()
            || parsed.digest().is_some()
        {
            return Err(malformed());
        }
        Ok(Self(parsed))
    }

    /// The package identifier as its own location — only once the index has
    /// established nothing rewrites it; anything else wants `Index::route`.
    pub fn passthrough(identifier: &PackageRef) -> Self {
        Self(identifier.clone())
    }

    /// A location from explicit repository and registry strings, unparsed and unversioned.
    pub fn from_parts(repository: impl Into<String>, registry: impl Into<String>) -> Self {
        Self(PackageRef::new_registry(repository, registry))
    }

    /// This location at `logical`'s tag and digest, each dropped when absent.
    #[must_use]
    pub fn at_version_of(self, logical: &PackageRef) -> Self {
        let mut location = self.without_specifiers();
        // Tag first: `clone_with_tag` drops a digest.
        if let Some(tag) = logical.tag() {
            location = location.clone_with_tag(tag);
        }
        if let Some(digest) = logical.digest() {
            location = location.clone_with_digest(digest);
        }
        location
    }

    /// [`at_version_of`](Self::at_version_of) for a pinned package identifier.
    #[must_use]
    pub fn at_pin_of(self, pinned: &PinnedPackageRef) -> PinnedOciIdentifier {
        self.at_version_of(pinned.as_identifier()).pinned_at(pinned.digest())
    }

    /// This location pinned at `digest`, keeping its tag as advisory.
    #[must_use]
    pub fn pinned_at(&self, digest: Digest) -> PinnedOciIdentifier {
        PinnedOciIdentifier {
            identifier: self.clone_with_digest(digest.clone()),
            digest,
        }
    }

    pub fn registry(&self) -> &str {
        self.0.registry()
    }

    pub fn repository(&self) -> &str {
        self.0.repository()
    }

    pub fn tag(&self) -> Option<&str> {
        self.0.tag()
    }

    pub fn tag_or_latest(&self) -> &str {
        self.0.tag_or_latest()
    }

    pub fn digest(&self) -> Option<Digest> {
        self.0.digest()
    }

    /// Drops any existing digest.
    #[must_use]
    pub fn clone_with_tag(&self, tag: impl Into<String>) -> Self {
        Self(self.0.clone_with_tag(tag))
    }

    #[must_use]
    pub fn clone_with_digest(&self, digest: Digest) -> Self {
        Self(self.0.clone_with_digest(digest))
    }

    #[must_use]
    pub fn without_digest(&self) -> Self {
        Self(self.0.without_digest())
    }

    #[must_use]
    pub fn without_tag(&self) -> Self {
        Self(self.0.without_tag())
    }

    #[must_use]
    pub fn without_specifiers(&self) -> Self {
        Self(self.0.without_specifiers())
    }

    /// The canonical transport reference, with no mirror rewrite: the push seam.
    ///
    /// Reads must use [`Client::transport_reference`](crate::Client::transport_reference)
    /// instead, or they bypass the mirror map; no compiler check enforces this.
    pub fn canonical_reference(&self) -> native::Reference {
        let registry = self.registry().to_string();
        let repository = self.repository().to_string();
        match (self.tag(), self.digest()) {
            (Some(tag), Some(digest)) => {
                native::Reference::with_tag_and_digest(registry, repository, tag.to_string(), digest.to_string())
            }
            (Some(tag), None) => native::Reference::with_tag(registry, repository, tag.to_string()),
            (None, Some(digest)) => native::Reference::with_digest(registry, repository, digest.to_string()),
            (None, None) => native::Reference::with_tag(registry, repository, "latest".into()),
        }
    }

    /// Crate-private: public, it would be an unratcheted way to mint a location from unvalidated text.
    pub(crate) fn from_native(reference: native::Reference) -> Result<Self, IdentifierError> {
        let registry = reference.registry().to_string();
        let input = reference.to_string();
        if DOCKER_HUB_DOMAINS.iter().any(|domain| registry == *domain) {
            return Err(IdentifierError::new(input, IdentifierErrorKind::DockerHubDefault));
        }
        let mut identifier = PackageRef::new_registry(reference.repository(), registry);
        if let Some(tag) = reference.tag() {
            identifier = identifier.clone_with_tag(tag);
        }
        if let Some(digest) = reference.digest() {
            let digest = Digest::try_from(digest.to_string())
                .map_err(|_| IdentifierError::new(input, IdentifierErrorKind::DigestInvalidFormat))?;
            identifier = identifier.clone_with_digest(digest);
        }
        Ok(Self(identifier))
    }
}

impl std::fmt::Display for OciIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for OciIdentifier {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

/// The source reference of a cross-repository blob mount; `mount_blob` reads only
/// its repository, so `"latest"` is an inert placeholder.
// Kept in this file: the mirror-invariant gate allows `native::Reference` construction in two seam files only.
pub(crate) fn mount_source_reference(registry: &str, source_repository: &str) -> native::Reference {
    native::Reference::with_tag(registry.to_string(), source_repository.to_string(), "latest".into())
}

impl schemars::JsonSchema for OciIdentifier {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("OciIdentifier")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "OCI registry location in the format 'registry/repository[:tag][@digest]'."
        })
    }
}

/// An [`OciIdentifier`] guaranteed to carry a digest — what a content read addresses.
///
/// No serde: a lock file or metadata names the package ([`PinnedPackageRef`]), never a physical pin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PinnedOciIdentifier {
    identifier: OciIdentifier,
    digest: Digest,
}

impl PinnedOciIdentifier {
    pub fn digest(&self) -> Digest {
        self.digest.clone()
    }

    #[must_use]
    pub fn clone_with_digest(&self, digest: Digest) -> Self {
        Self {
            identifier: self.identifier.clone_with_digest(digest.clone()),
            digest,
        }
    }

    pub fn as_oci_identifier(&self) -> &OciIdentifier {
        &self.identifier
    }
}

impl std::ops::Deref for PinnedOciIdentifier {
    type Target = OciIdentifier;

    fn deref(&self) -> &Self::Target {
        &self.identifier
    }
}

impl From<PinnedOciIdentifier> for OciIdentifier {
    fn from(pinned: PinnedOciIdentifier) -> Self {
        pinned.identifier
    }
}

impl TryFrom<OciIdentifier> for PinnedOciIdentifier {
    type Error = PinnedOciIdentifierError;

    fn try_from(identifier: OciIdentifier) -> Result<Self, Self::Error> {
        match identifier.digest() {
            Some(digest) => Ok(Self { identifier, digest }),
            None => Err(PinnedOciIdentifierError { identifier }),
        }
    }
}

impl std::fmt::Display for PinnedOciIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.identifier.fmt(f)
    }
}

/// A pinned OCI location requires a digest but none was present.
#[derive(Debug, thiserror::Error)]
#[error("pinned OCI location requires a digest: {identifier}")]
pub struct PinnedOciIdentifierError {
    pub identifier: OciIdentifier,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> Digest {
        Digest::Sha256("a".repeat(64))
    }

    // ── parse_repository_pointer (C3 round-trip) ─────────────────────────

    #[test]
    fn repository_pointer_parses_host_and_path() {
        let location = OciIdentifier::parse_repository_pointer("oci://ghcr.io/ocx-contrib/cmake").unwrap();
        assert_eq!(location.registry(), "ghcr.io");
        assert_eq!(location.repository(), "ocx-contrib/cmake");
        assert_eq!(location.tag(), None);
        assert_eq!(location.digest(), None);
    }

    #[test]
    fn repository_pointer_accepts_a_port() {
        let location = OciIdentifier::parse_repository_pointer("oci://localhost:5000/cmake").unwrap();
        assert_eq!(location.registry(), "localhost:5000");
        assert_eq!(location.repository(), "cmake");
    }

    #[test]
    fn repository_pointer_refuses_every_malformed_spelling() {
        let digest = format!("sha256:{}", "a".repeat(64));
        for value in [
            "ghcr.io/cmake",
            "https://ghcr.io/cmake",
            "oci://",
            "oci://ghcr.io",
            "oci://ghcr.io/",
            "oci:///cmake",
            "oci://ghcr.io/cmake:3.28",
            &format!("oci://ghcr.io/cmake@{digest}"),
            "oci://ghcr.io/Cmake",
            "oci://ghcr.io/cm ake",
            "oci://ghcr.io/cmake/../evil",
            "oci://ghcr.io/cmake\n",
        ] {
            let error = OciIdentifier::parse_repository_pointer(value).unwrap_err();
            assert_eq!(error.input, value, "error must quote the pointer as given");
            assert!(
                matches!(error.kind, IdentifierErrorKind::InvalidFormat),
                "{value}: {error}"
            );
        }
    }

    // ── parse_target ─────────────────────────────────────────────────────

    #[test]
    fn parse_target_applies_the_default_registry() {
        let location = OciIdentifier::parse_target("cmake:3.28", "registry.corp").unwrap();
        assert_eq!(location.to_string(), "registry.corp/cmake:3.28");
        let explicit = OciIdentifier::parse_target("ghcr.io/cmake:3.28", "registry.corp").unwrap();
        assert_eq!(explicit.registry(), "ghcr.io");
    }

    // ── at_version_of / at_pin_of ────────────────────────────────────────

    #[test]
    fn at_version_of_carries_the_tag_and_the_digest() {
        let logical = PackageRef::new_registry("cmake", "ocx.sh")
            .clone_with_tag("3.28")
            .clone_with_digest(digest());
        let location = OciIdentifier::from_parts("ocx-contrib/cmake", "ghcr.io").at_version_of(&logical);
        assert_eq!(
            location.to_string(),
            format!("ghcr.io/ocx-contrib/cmake:3.28@sha256:{}", "a".repeat(64))
        );
        let untagged = OciIdentifier::from_parts("x", "ghcr.io")
            .clone_with_tag("stale")
            .at_version_of(&logical.without_tag());
        assert_eq!(
            untagged.tag(),
            None,
            "a tag the logical identifier lacks must not survive"
        );
        assert_eq!(untagged.digest(), Some(digest()));
    }

    #[test]
    fn at_pin_of_yields_a_pinned_location() {
        let pinned =
            PinnedPackageRef::try_from(PackageRef::new_registry("cmake", "ocx.sh").clone_with_digest(digest()))
                .unwrap();
        let location = OciIdentifier::from_parts("ocx-contrib/cmake", "ghcr.io").at_pin_of(&pinned);
        assert_eq!(location.digest(), digest());
        assert_eq!(location.registry(), "ghcr.io");
    }

    // ── native::Reference ────────────────────────────────────────────────

    #[test]
    fn canonical_reference_names_the_location_as_stored() {
        let location = OciIdentifier::from_parts("repo", "test.com").clone_with_tag("tag");
        let reference = location.canonical_reference();
        assert_eq!(reference.registry(), "test.com");
        assert_eq!(reference.repository(), "repo");
        assert_eq!(reference.tag(), Some("tag"));
    }

    #[test]
    fn canonical_reference_of_an_untagged_location_is_latest() {
        let reference = OciIdentifier::from_parts("repo", "test.com").canonical_reference();
        assert_eq!(reference.tag(), Some("latest"));
    }

    #[test]
    fn from_native_rejects_docker_hub() {
        let reference = native::Reference::with_tag("docker.io".into(), "library/ubuntu".into(), "latest".into());
        let error = OciIdentifier::from_native(reference).unwrap_err();
        assert!(matches!(error.kind, IdentifierErrorKind::DockerHubDefault));
    }

    #[test]
    fn from_native_accepts_a_custom_registry() {
        let reference = native::Reference::with_tag("test.com".into(), "repo".into(), "1.0".into());
        let location = OciIdentifier::from_native(reference).unwrap();
        assert_eq!(location.registry(), "test.com");
        assert_eq!(location.repository(), "repo");
        assert_eq!(location.tag(), Some("1.0"));
    }

    // ── serde ────────────────────────────────────────────────────────────

    #[test]
    fn serializes_as_the_identifier_string() {
        let location = OciIdentifier::from_parts("repo", "test.com").clone_with_tag("tag");
        assert_eq!(serde_json::to_string(&location).unwrap(), "\"test.com/repo:tag\"");
    }

    // ── PinnedOciIdentifier ──────────────────────────────────────────────

    #[test]
    fn pinned_requires_a_digest() {
        assert!(PinnedOciIdentifier::try_from(OciIdentifier::from_parts("repo", "test.com")).is_err());
        let pinned =
            PinnedOciIdentifier::try_from(OciIdentifier::from_parts("repo", "test.com").clone_with_digest(digest()))
                .unwrap();
        assert_eq!(pinned.digest(), digest());
    }
}
