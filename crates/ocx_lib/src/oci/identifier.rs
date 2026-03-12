// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;

use serde::{Deserialize, Serialize};

use error::{IdentifierError, IdentifierErrorKind};

use super::{Digest, native};

const OCX_SH_REGISTRY: &str = "ocx.sh";

pub const DEFAULT_REGISTRY: &str = OCX_SH_REGISTRY;

const MAX_REPOSITORY_LENGTH: usize = 255;

const DOCKER_HUB_DOMAINS: &[&str] = &["docker.io", "index.docker.io"];

/// A parsed OCI identifier with registry, repository, optional tag, and optional digest.
///
/// Unlike `oci_spec::Reference`, this type does not inject `"latest"` when no tag
/// is present, does not default to `docker.io`, and provides structured parse errors
/// via [`IdentifierError`].
///
/// Conversion to `native::Reference` (for OCI transport calls) is available via
/// `From<&Identifier>`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Identifier {
    registry: String,
    repository: String,
    tag: Option<String>,
    digest: Option<Digest>,
}

impl Identifier {
    /// Creates an identifier from explicit repository and registry strings.
    ///
    /// No parsing is performed — the values are taken as-is.
    /// The resulting identifier has no tag and no digest.
    pub fn new_registry(repository: impl Into<String>, registry: impl Into<String>) -> Self {
        Self {
            registry: registry.into(),
            repository: repository.into(),
            tag: None,
            digest: None,
        }
    }

    /// Parses an identifier string, using `default_registry` for inputs that
    /// do not contain an explicit registry (e.g. `"cmake:3.28"` or `"myorg/tool"`).
    ///
    /// If the input already contains a registry (detected by a `.` or `:` in the
    /// first path segment, or `"localhost"`), the default is ignored.
    pub fn parse_with_default_registry(s: &str, default_registry: &str) -> Result<Self, IdentifierError> {
        parse(s, default_registry)
    }

    /// Returns a new identifier with the given tag, dropping any existing digest.
    ///
    /// The digest is dropped because changing the tag semantically creates a
    /// different reference — the old digest no longer applies.
    /// Any `+` in the tag is normalized to `_` (OCI tags do not allow `+`).
    pub fn clone_with_tag(&self, tag: impl Into<String>) -> Self {
        Self {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            tag: Some(normalize_tag(tag.into())),
            digest: None,
        }
    }

    /// Returns a new identifier with the given digest, preserving the existing tag.
    pub fn clone_with_digest(&self, digest: Digest) -> Self {
        Self {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            tag: self.tag.clone(),
            digest: Some(digest),
        }
    }

    /// Returns the registry hostname (and optional port), e.g. `"ghcr.io"` or `"localhost:5000"`.
    pub fn registry(&self) -> &str {
        &self.registry
    }

    /// Returns the repository path within the registry, e.g. `"library/ubuntu"` or `"cmake"`.
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Returns the last segment of the repository as the package name.
    ///
    /// For `"myorg/cmake"` this returns `Some("cmake")`.
    pub fn name(&self) -> Option<String> {
        self.repository.split('/').next_back().map(|s| s.to_string())
    }

    /// Returns the tag if one was explicitly provided, or `None` otherwise.
    ///
    /// Unlike `oci_spec::Reference`, this does **not** inject `"latest"` when
    /// no tag is present. Use [`tag_or_latest`](Self::tag_or_latest) when a
    /// fallback to `"latest"` is desired.
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }

    /// Returns the tag if present, or `"latest"` as a default.
    pub fn tag_or_latest(&self) -> &str {
        self.tag.as_deref().unwrap_or("latest")
    }

    /// Returns the content-addressed digest, if any.
    pub fn digest(&self) -> Option<Digest> {
        self.digest.clone()
    }
}

impl std::fmt::Display for Identifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.registry, self.repository)?;
        if let Some(tag) = &self.tag {
            write!(f, ":{tag}")?;
        }
        if let Some(digest) = &self.digest {
            write!(f, "@{digest}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Identifier {
    type Err = IdentifierError;

    fn from_str(value: &str) -> Result<Self, IdentifierError> {
        parse(value, DEFAULT_REGISTRY)
    }
}

impl Serialize for Identifier {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Identifier {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<Identifier>().map_err(serde::de::Error::custom)
    }
}

// ── Conversion to native::Reference ──────────────────────────────────

impl From<&Identifier> for native::Reference {
    fn from(id: &Identifier) -> Self {
        let reg = id.registry.clone();
        let repo = id.repository.clone();
        match (&id.tag, &id.digest) {
            (Some(t), Some(d)) => native::Reference::with_tag_and_digest(reg, repo, t.clone(), d.to_string()),
            (Some(t), None) => native::Reference::with_tag(reg, repo, t.clone()),
            (None, Some(d)) => native::Reference::with_digest(reg, repo, d.to_string()),
            (None, None) => native::Reference::with_tag(reg, repo, "latest".into()),
        }
    }
}

// ── Conversion from native::Reference ────────────────────────────────

impl TryFrom<native::Reference> for Identifier {
    type Error = IdentifierError;

    fn try_from(reference: native::Reference) -> Result<Self, IdentifierError> {
        let registry = reference.registry().to_string();
        let input = reference.to_string();

        if DOCKER_HUB_DOMAINS.iter().any(|d| registry == *d) {
            return Err(IdentifierError {
                input,
                kind: IdentifierErrorKind::DockerHubDefault,
            });
        }

        let repository = reference.repository().to_string();
        let tag = reference.tag().map(|t| normalize_tag(t.to_string()));
        let digest = match reference.digest() {
            Some(d) => {
                let d_str = d.to_string();
                Some(Digest::try_from(d_str).map_err(|_| IdentifierError {
                    input: input.clone(),
                    kind: IdentifierErrorKind::DigestInvalidFormat,
                })?)
            }
            None => None,
        };

        Ok(Self {
            registry,
            repository,
            tag,
            digest,
        })
    }
}

// ── Parser ───────────────────────────────────────────────────────────

fn parse(input: &str, default_registry: &str) -> Result<Identifier, IdentifierError> {
    if input.is_empty() {
        return Err(IdentifierError {
            input: String::new(),
            kind: IdentifierErrorKind::Empty,
        });
    }

    // Split off digest portion (everything after '@').
    let (name_part, digest) = match input.split_once('@') {
        Some((name, digest_str)) => (name, Some(parse_digest(input, digest_str)?)),
        None => (input, None),
    };

    // Split tag from the name portion.
    // We need to find the tag in the last path segment only (after the last '/'),
    // so that registry ports like `localhost:5000` are not mistaken for tags.
    let (name_without_tag, tag) = split_tag(name_part);

    // Prepend default domain if needed.
    let full_name = prepend_domain(name_without_tag, default_registry);

    // Split registry from repository.
    let (registry, repository) = split_registry_repository(&full_name).ok_or_else(|| IdentifierError {
        input: input.to_string(),
        kind: IdentifierErrorKind::InvalidFormat,
    })?;

    // Validate repository.
    if repository.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(IdentifierError {
            input: input.to_string(),
            kind: IdentifierErrorKind::UppercaseRepository,
        });
    }
    if repository.len() > MAX_REPOSITORY_LENGTH {
        return Err(IdentifierError {
            input: input.to_string(),
            kind: IdentifierErrorKind::RepositoryTooLong,
        });
    }

    Ok(Identifier {
        registry,
        repository,
        tag,
        digest,
    })
}

/// Parses a digest string like `sha256:abcdef...` into a `Digest`.
fn parse_digest(input: &str, digest_str: &str) -> Result<Digest, IdentifierError> {
    let (algo, hex) = digest_str.split_once(':').ok_or_else(|| IdentifierError {
        input: input.to_string(),
        kind: IdentifierErrorKind::DigestInvalidFormat,
    })?;

    let expected_len = match algo {
        "sha256" => 64,
        "sha384" => 96,
        "sha512" => 128,
        _ => {
            return Err(IdentifierError {
                input: input.to_string(),
                kind: IdentifierErrorKind::DigestUnsupported(algo.to_string()),
            });
        }
    };

    if hex.len() != expected_len {
        return Err(IdentifierError {
            input: input.to_string(),
            kind: IdentifierErrorKind::DigestInvalidLength {
                algorithm: algo.to_string(),
                expected: expected_len,
                actual: hex.len(),
            },
        });
    }

    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(IdentifierError {
            input: input.to_string(),
            kind: IdentifierErrorKind::DigestInvalidFormat,
        });
    }

    match algo {
        "sha256" => Ok(Digest::Sha256(hex.to_string())),
        "sha384" => Ok(Digest::Sha384(hex.to_string())),
        "sha512" => Ok(Digest::Sha512(hex.to_string())),
        _ => unreachable!(),
    }
}

/// Splits the tag from the name portion. Returns `(name_without_tag, Option<tag>)`.
///
/// Only looks for a `:` in the last path segment (after the last `/`),
/// so registry ports like `localhost:5000` are not mistaken for tags.
fn split_tag(name: &str) -> (&str, Option<String>) {
    let last_slash = name.rfind('/');
    let last_segment = match last_slash {
        Some(pos) => &name[pos + 1..],
        None => name,
    };

    match last_segment.find(':') {
        Some(colon_in_segment) => {
            let colon_pos = match last_slash {
                Some(slash_pos) => slash_pos + 1 + colon_in_segment,
                None => colon_in_segment,
            };
            let tag = &name[colon_pos + 1..];
            (&name[..colon_pos], Some(normalize_tag(tag.to_string())))
        }
        None => (name, None),
    }
}

/// Normalizes `+` to `_` in a tag string.
///
/// OCI tags do not allow `+` (`[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}`).
/// This is the earliest boundary where user input enters the system.
fn normalize_tag(tag: String) -> String {
    tag.replace('+', "_")
}

/// Splits `full_name` into `(registry, repository)`.
///
/// The first segment (before the first `/`) is the registry if it contains
/// a `.` or `:`, or is `"localhost"`. Otherwise the entire string is the
/// repository under the default registry (but that case is handled by
/// `prepend_domain` before this function is called).
fn split_registry_repository(full_name: &str) -> Option<(String, String)> {
    let (first, rest) = full_name.split_once('/')?;
    Some((first.to_string(), rest.to_string()))
}

fn prepend_domain(name: &str, domain: &str) -> String {
    match name.split_once('/') {
        None => format!("{domain}/{name}"),
        Some((left, _)) => {
            if !(left.contains('.') || left.contains(':')) && left != "localhost" {
                format!("{domain}/{name}")
            } else {
                name.into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic parsing ────────────────────────────────────────────────────

    #[test]
    fn parse_bare_name() {
        let id: Identifier = "python".parse().unwrap();
        assert_eq!(id.registry(), DEFAULT_REGISTRY);
        assert_eq!(id.repository(), "python");
        assert_eq!(id.tag(), None);
        assert_eq!(id.tag_or_latest(), "latest");
        assert_eq!(id.digest(), None);
    }

    #[test]
    fn parse_name_with_tag() {
        let id: Identifier = "python:3.12".parse().unwrap();
        assert_eq!(id.registry(), DEFAULT_REGISTRY);
        assert_eq!(id.repository(), "python");
        assert_eq!(id.tag(), Some("3.12"));
        assert_eq!(id.tag_or_latest(), "3.12");
    }

    #[test]
    fn parse_explicit_latest() {
        let id: Identifier = "python:latest".parse().unwrap();
        assert_eq!(id.tag(), Some("latest"));
        assert_eq!(id.tag_or_latest(), "latest");
    }

    #[test]
    fn parse_with_registry() {
        let id: Identifier = "test.com/repo:tag".parse().unwrap();
        assert_eq!(id.registry(), "test.com");
        assert_eq!(id.repository(), "repo");
        assert_eq!(id.tag(), Some("tag"));
    }

    #[test]
    fn parse_registry_with_port() {
        let id: Identifier = "test:5000/repo:tag".parse().unwrap();
        assert_eq!(id.registry(), "test:5000");
        assert_eq!(id.repository(), "repo");
        assert_eq!(id.tag(), Some("tag"));
    }

    #[test]
    fn parse_registry_port_digest_only() {
        let hex = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let input = format!("test:5000/repo@sha256:{hex}");
        let id: Identifier = input.parse().unwrap();
        assert_eq!(id.registry(), "test:5000");
        assert_eq!(id.repository(), "repo");
        assert_eq!(id.tag(), None);
        assert!(id.digest().is_some());
    }

    #[test]
    fn parse_tag_and_digest() {
        let hex = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let input = format!("test:5000/repo:tag@sha256:{hex}");
        let id: Identifier = input.parse().unwrap();
        assert_eq!(id.registry(), "test:5000");
        assert_eq!(id.repository(), "repo");
        assert_eq!(id.tag(), Some("tag"));
        assert!(id.digest().is_some());
    }

    #[test]
    fn parse_nested_repo_no_tag() {
        let id: Identifier = "sub-dom1.foo.com/bar/baz/quux".parse().unwrap();
        assert_eq!(id.registry(), "sub-dom1.foo.com");
        assert_eq!(id.repository(), "bar/baz/quux");
        assert_eq!(id.tag(), None);
    }

    #[test]
    fn parse_complex_nested() {
        let id: Identifier = "b.gcr.io/test.example.com/my-app:tag".parse().unwrap();
        assert_eq!(id.registry(), "b.gcr.io");
        assert_eq!(id.repository(), "test.example.com/my-app");
        assert_eq!(id.tag(), Some("tag"));
    }

    #[test]
    fn parse_org_repo_gets_default_registry() {
        let id: Identifier = "myorg/cmake:3.28".parse().unwrap();
        assert_eq!(id.registry(), DEFAULT_REGISTRY);
        assert_eq!(id.repository(), "myorg/cmake");
        assert_eq!(id.tag(), Some("3.28"));
    }

    // ── parse_with_default_registry ─────────────────────────────────────

    #[test]
    fn parse_with_default_registry_preserves_tag_presence() {
        let bare = Identifier::parse_with_default_registry("python", "localhost:5000").unwrap();
        assert_eq!(bare.tag(), None);
        assert_eq!(bare.tag_or_latest(), "latest");
        assert_eq!(bare.registry(), "localhost:5000");

        let tagged = Identifier::parse_with_default_registry("python:3.12", "localhost:5000").unwrap();
        assert_eq!(tagged.tag(), Some("3.12"));
    }

    #[test]
    fn parse_with_default_registry_ignores_default_when_registry_present() {
        let id = Identifier::parse_with_default_registry("ghcr.io/myorg/tool:1.0", "localhost:5000").unwrap();
        assert_eq!(id.registry(), "ghcr.io");
        assert_eq!(id.repository(), "myorg/tool");
        assert_eq!(id.tag(), Some("1.0"));
    }

    // ── clone_with_tag / clone_with_digest ───────────────────────────────

    #[test]
    fn clone_with_tag_always_explicit() {
        let bare: Identifier = "python".parse().unwrap();
        assert_eq!(bare.tag(), None);

        let tagged = bare.clone_with_tag("3.12");
        assert_eq!(tagged.tag(), Some("3.12"));
    }

    #[test]
    fn clone_with_digest_preserves_fields() {
        let id: Identifier = "test.com/repo:tag".parse().unwrap();
        let digest = Digest::Sha256("a".repeat(64));
        let with_digest = id.clone_with_digest(digest.clone());
        assert_eq!(with_digest.registry(), "test.com");
        assert_eq!(with_digest.repository(), "repo");
        assert_eq!(with_digest.tag(), Some("tag"));
        assert_eq!(with_digest.digest(), Some(digest));
    }

    // ── Error cases ──────────────────────────────────────────────────────

    #[test]
    fn empty_string_errors() {
        let err = "".parse::<Identifier>().unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::Empty));
        assert_eq!(err.input, "");
    }

    #[test]
    fn uppercase_repo_errors() {
        let err = "test.com/Foo".parse::<Identifier>().unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::UppercaseRepository));
    }

    #[test]
    fn bad_digest_algo_errors() {
        let err = "test.com/repo@md5:abcdef".parse::<Identifier>().unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DigestUnsupported(_)));
    }

    #[test]
    fn bad_digest_length_errors() {
        let err = "test.com/repo@sha256:abc".parse::<Identifier>().unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DigestInvalidLength { .. }));
    }

    // ── Display roundtrip ────────────────────────────────────────────────

    #[test]
    fn display_roundtrip() {
        let cases = &[
            "test.com/repo:tag",
            "test:5000/repo:tag",
            "sub-dom1.foo.com/bar/baz/quux",
        ];
        for &input in cases {
            let id: Identifier = input.parse().unwrap();
            let displayed = id.to_string();
            let reparsed: Identifier = displayed.parse().unwrap();
            assert_eq!(id, reparsed, "roundtrip failed for: {input}");
        }
    }

    #[test]
    fn display_with_digest_roundtrip() {
        let hex = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let input = format!("test:5000/repo:tag@sha256:{hex}");
        let id: Identifier = input.parse().unwrap();
        let reparsed: Identifier = id.to_string().parse().unwrap();
        assert_eq!(id, reparsed);
    }

    #[test]
    fn display_bare_name_includes_default_registry() {
        let id: Identifier = "python".parse().unwrap();
        assert_eq!(id.to_string(), format!("{DEFAULT_REGISTRY}/python"));
    }

    // ── Serialize / Deserialize ──────────────────────────────────────────

    #[test]
    fn serde_roundtrip() {
        let id: Identifier = "test.com/repo:tag".parse().unwrap();
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: Identifier = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    // ── From/TryFrom Reference ───────────────────────────────────────────

    #[test]
    fn identifier_to_reference_roundtrip() {
        let id: Identifier = "test.com/repo:tag".parse().unwrap();
        let reference = native::Reference::from(&id);
        assert_eq!(reference.registry(), "test.com");
        assert_eq!(reference.repository(), "repo");
        assert_eq!(reference.tag(), Some("tag"));
    }

    #[test]
    fn identifier_without_tag_becomes_latest_in_reference() {
        let id: Identifier = "test.com/repo".parse().unwrap();
        let reference = native::Reference::from(&id);
        assert_eq!(reference.tag(), Some("latest"));
    }

    #[test]
    fn try_from_reference_rejects_docker_hub() {
        let reference = native::Reference::with_tag("docker.io".into(), "library/ubuntu".into(), "latest".into());
        let err = Identifier::try_from(reference).unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DockerHubDefault));
    }

    #[test]
    fn try_from_reference_accepts_custom_registry() {
        let reference = native::Reference::with_tag("test.com".into(), "repo".into(), "1.0".into());
        let id = Identifier::try_from(reference).unwrap();
        assert_eq!(id.registry(), "test.com");
        assert_eq!(id.repository(), "repo");
        assert_eq!(id.tag(), Some("1.0"));
    }

    // ── new_registry ─────────────────────────────────────────────────────

    #[test]
    fn new_registry_creates_identifier() {
        let id = Identifier::new_registry("cmake", "example.com");
        assert_eq!(id.registry(), "example.com");
        assert_eq!(id.repository(), "cmake");
        assert_eq!(id.tag(), None);
        assert_eq!(id.digest(), None);
    }

    // ── Tag normalization (+  → _) ──────────────────────────────────────

    #[test]
    fn parse_normalizes_plus_to_underscore_in_tag() {
        let id: Identifier = "cmake:3.28.1+20260216".parse().unwrap();
        assert_eq!(id.tag(), Some("3.28.1_20260216"));
    }

    #[test]
    fn parse_preserves_underscore_in_tag() {
        let id: Identifier = "cmake:3.28.1_20260216".parse().unwrap();
        assert_eq!(id.tag(), Some("3.28.1_20260216"));
    }

    #[test]
    fn parse_normalizes_plus_with_registry_port() {
        let id: Identifier = "test:5000/repo:1.0+build".parse().unwrap();
        assert_eq!(id.registry(), "test:5000");
        assert_eq!(id.tag(), Some("1.0_build"));
    }

    #[test]
    fn parse_plus_display_roundtrip() {
        let id1: Identifier = "test.com/repo:3.28.1+b1".parse().unwrap();
        let displayed = id1.to_string();
        let id2: Identifier = displayed.parse().unwrap();
        assert_eq!(id1, id2);
        assert_eq!(id2.tag(), Some("3.28.1_b1"));
    }

    #[test]
    fn clone_with_tag_normalizes_plus() {
        let base: Identifier = "test.com/repo".parse().unwrap();
        let tagged = base.clone_with_tag("3.28.1+b1");
        assert_eq!(tagged.tag(), Some("3.28.1_b1"));
    }
}
