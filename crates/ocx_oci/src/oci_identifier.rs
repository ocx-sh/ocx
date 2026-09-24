// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use super::package_ref::error::{IdentifierError, IdentifierErrorKind};
use super::{Digest, PackageRef, PinnedPackageRef, native};

const DOCKER_HUB_DOMAINS: &[&str] = &["docker.io", "index.docker.io"];

/// A **physical** OCI location: the registry and repository a request dials.
///
/// [`PackageRef`] is the package name a user, a lock or package metadata
/// spells; it is logical and may be served by an index that points somewhere
/// else entirely (`ocx.sh/cmake` → `ghcr.io/ocx-contrib/cmake`). Every
/// [`Client`](crate::Client) read and write takes this type instead, so a
/// package name cannot reach the wire without being routed first — dialling
/// the logical host is the defect class of ocx#504, and here it does not
/// compile.
///
/// There is no conversion from or to [`PackageRef`]: no `From`, `Into`,
/// `AsRef`, `Deref` or `FromStr`. The ways to obtain one are:
///
/// - routing a package identifier through the index (`ocx_index::Index::route`,
///   `route_for_dial`, `route_local`), which is the normal path;
/// - [`parse_target`](Self::parse_target), for a write target the user named
///   as a location (`--to`, `-i`, a managed-config source);
/// - [`parse_repository_pointer`](Self::parse_repository_pointer), for an index
///   root's `oci://host/path` pointer;
/// - [`passthrough`](Self::passthrough) and [`from_parts`](Self::from_parts),
///   for registry-backed identity inside the index and for fixtures.
///
/// A workspace ratchet (`oci_identifier_mint_ratchet`) pins every non-test call
/// site of those four constructors, so a new one is a reviewed decision.
///
/// Serializes to the same `registry/repository[:tag][@digest]` string as
/// [`PackageRef`], for reports. It deliberately does not deserialize: nothing
/// OCX persists names a physical location in that grammar.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct OciIdentifier(PackageRef);

impl OciIdentifier {
    /// Parses a location the user named as a **write target** — `--to`, `-i`,
    /// a managed-config source — using `default_registry` when the input names
    /// no registry.
    ///
    /// The same grammar as [`PackageRef::parse_with_default_registry`]. The
    /// input is taken as a location, not a package name: nothing routes it.
    ///
    /// # Errors
    ///
    /// [`IdentifierError`] exactly as the identifier grammar raises it.
    pub fn parse_target(input: &str, default_registry: &str) -> Result<Self, IdentifierError> {
        PackageRef::parse_with_default_registry(input, default_registry).map(Self)
    }

    /// Parses an index root's `repository` pointer (`oci://host/path`).
    ///
    /// Strict (`adr_index_indirection.md` C3): the `oci://` scheme is a wire
    /// contract, so a missing or unknown scheme or an empty host or path is
    /// refused. `host/path` is then re-parsed through the identifier grammar
    /// and must round-trip **exactly** — the lowercase, character-class,
    /// traversal and length checks every package identifier passes guard this
    /// pointer too, and demanding the parsed registry and repository equal the
    /// split host and path, with no tag and no digest, refuses a smuggled tag
    /// (`repo:x`), digest (`repo@sha256:…`), whitespace, control character,
    /// uppercase segment or stray colon. Host allowlisting stays index-side
    /// governance; the SSRF floor runs where the pointer is dereferenced.
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

    /// The location of a **registry-backed** package: the package identifier
    /// is its own location.
    ///
    /// For the index internals that decide a name is not rewritten. Anything
    /// else wants `ocx_index::Index::route`, which reaches this only after
    /// establishing that no index serves the name.
    pub fn passthrough(identifier: &PackageRef) -> Self {
        Self(identifier.clone())
    }

    /// A location from explicit repository and registry strings, with no tag
    /// and no digest. No parsing is performed.
    ///
    /// For locations that arrive already split — a mirror spec's target — and
    /// for fixtures.
    pub fn from_parts(repository: impl Into<String>, registry: impl Into<String>) -> Self {
        Self(PackageRef::new_registry(repository, registry))
    }

    /// This location at `logical`'s version: its tag and its digest, each
    /// carried when present and dropped when absent.
    ///
    /// Derives rather than mints — the receiver is already a location. The
    /// tag goes on first because [`clone_with_tag`](Self::clone_with_tag)
    /// drops a digest.
    #[must_use]
    pub fn at_version_of(self, logical: &PackageRef) -> Self {
        let mut location = self.without_specifiers();
        if let Some(tag) = logical.tag() {
            location = location.clone_with_tag(tag);
        }
        if let Some(digest) = logical.digest() {
            location = location.clone_with_digest(digest);
        }
        location
    }

    /// This location at `pinned`'s version — [`at_version_of`](Self::at_version_of)
    /// for a pinned package identifier, whose digest makes the result pinned
    /// too.
    #[must_use]
    pub fn at_pin_of(self, pinned: &PinnedPackageRef) -> PinnedOciIdentifier {
        self.at_version_of(pinned.as_identifier()).pinned_at(pinned.digest())
    }

    /// This location pinned at `digest`, keeping its tag as advisory — the
    /// blob or manifest a content read addresses inside this repository.
    #[must_use]
    pub fn pinned_at(&self, digest: Digest) -> PinnedOciIdentifier {
        PinnedOciIdentifier {
            identifier: self.clone_with_digest(digest.clone()),
            digest,
        }
    }

    /// Returns the registry hostname (and optional port), e.g. `"ghcr.io"` or `"localhost:5000"`.
    pub fn registry(&self) -> &str {
        self.0.registry()
    }

    /// Returns the repository path within the registry.
    pub fn repository(&self) -> &str {
        self.0.repository()
    }

    /// Returns the tag if one was explicitly provided.
    pub fn tag(&self) -> Option<&str> {
        self.0.tag()
    }

    /// Returns the tag if present, or `"latest"` as a default.
    pub fn tag_or_latest(&self) -> &str {
        self.0.tag_or_latest()
    }

    /// Content-addressed digest, if pinned.
    pub fn digest(&self) -> Option<Digest> {
        self.0.digest()
    }

    /// Returns a new location with the given tag, dropping any existing digest.
    #[must_use]
    pub fn clone_with_tag(&self, tag: impl Into<String>) -> Self {
        Self(self.0.clone_with_tag(tag))
    }

    /// Clones with the given digest, preserving the existing tag.
    #[must_use]
    pub fn clone_with_digest(&self, digest: Digest) -> Self {
        Self(self.0.clone_with_digest(digest))
    }

    /// Strips the digest, preserving registry, repository and tag.
    #[must_use]
    pub fn without_digest(&self) -> Self {
        Self(self.0.without_digest())
    }

    /// Strips the tag, preserving registry, repository and digest.
    #[must_use]
    pub fn without_tag(&self) -> Self {
        Self(self.0.without_tag())
    }

    /// Returns a new location with only registry and repository.
    #[must_use]
    pub fn without_specifiers(&self) -> Self {
        Self(self.0.without_specifiers())
    }

    /// Builds the **canonical** transport reference for this location — host,
    /// repository, tag and digest exactly as stored, with no mirror rewrite.
    ///
    /// This is the push seam. The read path must **not** call it: read-path
    /// reference construction goes through
    /// [`Client::transport_reference`](crate::Client::transport_reference) /
    /// `transport_registry`, which apply the mirror map. That discipline is
    /// enforced by review and the behavioural backstop, not the compiler.
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

    /// The location a transport reference names.
    ///
    /// Crate-private on purpose: a `native::Reference` built outside this
    /// crate is unvalidated text, and a public conversion would be a fifth,
    /// unratcheted way to mint a location.
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

/// Builds the synthetic source reference a cross-repository blob mount needs.
///
/// A mount names its source repository in the `from=` query parameter of an
/// upload POST addressed at the *target* repository, and `oci_client`'s
/// `mount_blob` reads only `repository()` off this value — the registry and tag
/// are never sent. `"latest"` is therefore an inert placeholder, and the
/// registry is carried solely to keep the reference well-formed.
///
/// It lives beside [`OciIdentifier::canonical_reference`] because it is push-path
/// construction: mounting happens during a push, and the push path is
/// mirror-free by design (remote/proxy mirrors are read-only). Building it here
/// rather than in the transport keeps `native::Reference` construction inside
/// the two seam files the mirror-invariant gate allows.
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

/// An [`OciIdentifier`] guaranteed to carry a digest — what a content read
/// (`pull_manifest`, `pull_blob`, `pull_layer`) addresses.
///
/// Deliberately **no serde**: a physical pin has no business in a lock file
/// or in package metadata, which name the package ([`PinnedPackageRef`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PinnedOciIdentifier {
    identifier: OciIdentifier,
    digest: Digest,
}

impl PinnedOciIdentifier {
    /// Returns the digest. Always present by construction.
    pub fn digest(&self) -> Digest {
        self.digest.clone()
    }

    /// Returns a copy with the digest replaced. The tag (if any) is preserved.
    #[must_use]
    pub fn clone_with_digest(&self, digest: Digest) -> Self {
        Self {
            identifier: self.identifier.clone_with_digest(digest.clone()),
            digest,
        }
    }

    /// Returns a borrow of the inner [`OciIdentifier`].
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
