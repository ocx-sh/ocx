// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;

use serde::{Deserialize, Serialize};

use error::{IdentifierError, IdentifierErrorKind};

use super::{Digest, native};

pub const OCX_SH_REGISTRY: &str = "ocx.sh";
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
/// the `pub(crate)` [`Identifier::canonical_reference`] constructor.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
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

    /// Parses an identifier string that must contain an explicit registry.
    ///
    /// Returns [`IdentifierErrorKind::MissingRegistry`] if the input does not
    /// contain an explicit registry (e.g. `"cmake:3.28"` or `"myorg/tool"`).
    /// Returns [`IdentifierErrorKind::DirectoryTraversal`] if any path segment
    /// is `.` or `..`.
    ///
    /// This parser does **not** inject `"latest"` when the input has no tag
    /// — bare-repo inputs like `"ocx.sh/cmake"` parse to `tag = None`. The
    /// project-config layer ([`crate::project::ProjectConfig::from_toml_str`])
    /// applies its own `:latest` default at the schema boundary so an
    /// `ocx.toml` entry without a tag resolves predictably; that default
    /// does not apply to other [`Identifier::parse`] callers.
    pub fn parse(input: &str) -> Result<Self, IdentifierError> {
        validate_segments(input)?;
        if !has_explicit_registry(input) {
            return Err(IdentifierError {
                input: input.to_string(),
                kind: IdentifierErrorKind::MissingRegistry,
            });
        }
        parse_internal(input, DEFAULT_REGISTRY)
    }

    /// Parses an identifier string, using `default_registry` for inputs that
    /// do not contain an explicit registry (e.g. `"cmake:3.28"` or `"myorg/tool"`).
    ///
    /// If the input already contains a registry (detected by a `.` or `:` in the
    /// first path segment, or `"localhost"`), the default is ignored.
    pub fn parse_with_default_registry(s: &str, default_registry: &str) -> Result<Self, IdentifierError> {
        validate_segments(s)?;
        parse_internal(s, default_registry)
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

    /// Clones with the given digest, preserving the existing tag.
    pub fn clone_with_digest(&self, digest: Digest) -> Self {
        Self {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            tag: self.tag.clone(),
            digest: Some(digest),
        }
    }

    /// Useful for matching entries that differ only by digest (e.g., candidate vs content mode).
    pub fn without_digest(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            tag: self.tag.clone(),
            digest: None,
        }
    }

    /// Strips the tag, preserving registry, repository, and digest.
    pub fn without_tag(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            tag: None,
            digest: self.digest.clone(),
        }
    }

    /// Returns a new identifier with only registry and repository — tag and digest stripped.
    ///
    /// Useful for grouping or deduplicating by package identity regardless of version.
    pub fn without_specifiers(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            tag: None,
            digest: None,
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

    /// Returns the last segment of the repository path as the package name.
    ///
    /// For `"myorg/cmake"` this returns `"cmake"`.
    /// For a single-segment repository like `"python"` this returns `"python"`.
    pub fn name(&self) -> &str {
        self.repository.rsplit('/').next().unwrap_or(&self.repository)
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

    /// Content-addressed digest, if pinned.
    pub fn digest(&self) -> Option<Digest> {
        self.digest.clone()
    }

    /// Builds the **canonical** transport reference for this identifier — host,
    /// repository, tag and digest exactly as stored, with no mirror rewrite.
    ///
    /// This is the push seam. The push path calls it directly because
    /// remote/proxy mirrors are read-only. The read path must **not** call it:
    /// read-path reference construction goes through
    /// [`Client::transport_reference`](crate::oci::Client) /
    /// `transport_registry`, which apply the mirror map.
    ///
    /// There is no PUBLIC bypass — the `From<&Identifier> for native::Reference`
    /// impl is removed, so no external read site has a canonical conversion
    /// symbol to reach for. This method is `pub(crate)`, so it is still callable
    /// in-crate; the discipline that in-crate read paths route through
    /// `Client::transport_reference` / `transport_registry` rather than calling
    /// this directly is enforced by the structural test plus the behavioural
    /// backstop, **not** by the compiler.
    pub(crate) fn canonical_reference(&self) -> native::Reference {
        let registry = self.registry.clone();
        let repository = self.repository.clone();
        match (&self.tag, &self.digest) {
            (Some(tag), Some(digest)) => {
                native::Reference::with_tag_and_digest(registry, repository, tag.clone(), digest.to_string())
            }
            (Some(tag), None) => native::Reference::with_tag(registry, repository, tag.clone()),
            (None, Some(digest)) => native::Reference::with_digest(registry, repository, digest.to_string()),
            (None, None) => native::Reference::with_tag(registry, repository, "latest".into()),
        }
    }
}

/// Returns the canonical, untagged OCX CLI identifier (`ocx.sh/ocx/cli`).
///
/// This is the single source of truth for the well-known self identifier used
/// by self-update, self-setup, and activation. Self-management is opinionated
/// about its registry, so the value is fixed rather than configurable.
///
/// The seam below is a **test-only** override, gated behind `cfg(test)` or the
/// `__testing` Cargo feature so release artifacts physically lack the code
/// path. The override only honors loopback registries.
pub fn ocx_cli_identifier() -> Identifier {
    #[cfg(any(test, feature = "__testing"))]
    {
        if let Ok(spec) = std::env::var("__OCX_SELF_IMAGE")
            && let Some((registry, repository)) = parse_self_image_spec(&spec)
        {
            // Defense-in-depth: even with the seam compiled in, refuse any
            // override that does not point at a loopback registry. Asserts
            // loudly in tests; release builds never link this branch.
            assert!(
                is_loopback_registry(registry),
                "__OCX_SELF_IMAGE override must target a loopback registry; got `{registry}`"
            );
            return Identifier::new_registry(repository, registry);
        }
    }
    Identifier::new_registry("ocx/cli", OCX_SH_REGISTRY)
}

/// Parses the `__OCX_SELF_IMAGE` test-only seam value.
///
/// Format: `<registry>/<repo>` where the first `/` separates registry from
/// repo (registry may contain `:port`, repo may contain further `/` segments).
/// Returns `None` on malformed input.
#[cfg(any(test, feature = "__testing"))]
fn parse_self_image_spec(spec: &str) -> Option<(&str, &str)> {
    spec.split_once('/').filter(|(r, p)| !r.is_empty() && !p.is_empty())
}

/// Loopback-registry check for the `__OCX_SELF_IMAGE` seam.
///
/// Accepts `localhost`, `127.0.0.1`, and the IPv6 loopback `::1` (with or
/// without bracketed `[::1]` host syntax), each with an optional `:port`.
#[cfg(any(test, feature = "__testing"))]
fn is_loopback_registry(registry: &str) -> bool {
    // Bracketed IPv6 form: `[host]` or `[host]:port`. Extract the host inside.
    let host = if let Some(stripped) = registry.strip_prefix('[') {
        match stripped.split_once(']') {
            Some((inner, _)) => inner,
            None => return false,
        }
    } else {
        // Bare host or `host:port`. IPv4 / DNS names never contain `:` so the
        // first `:` always splits host from port.
        registry.split_once(':').map_or(registry, |(host, _)| host)
    };
    host == "localhost" || host == "127.0.0.1" || host == "::1"
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
        parse_internal(value, DEFAULT_REGISTRY)
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
        Identifier::parse(&s).map_err(serde::de::Error::custom)
    }
}

// ── Conversion to native::Reference ──────────────────────────────────
//
// There is deliberately NO `impl From<&Identifier> for native::Reference`.
// ADR `adr_oci_registry_mirror.md` Axis B1: the canonical
// `Identifier → native::Reference` conversion is the `pub(crate)`
// `Identifier::canonical_reference` method (push path, mirror-free); the read
// path builds references through `Client::transport_reference` /
// `transport_registry`, which apply the mirror map.
//
// Removing the blanket `From` impl closes the PUBLIC bypass: no external caller
// has a canonical conversion symbol to reach for. It does NOT make an in-crate
// read-path leak a compile error — `canonical_reference` stays `pub(crate)` and
// callable in-crate. The "read paths route through the transport seams"
// invariant is enforced by the structural test plus the behavioural backstop,
// not by the compiler.

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

/// Validates that no path segment is `.` or `..` (directory traversal defence).
fn validate_segments(input: &str) -> Result<(), IdentifierError> {
    let name_part = input.split_once('@').map_or(input, |(name, _)| name);
    for segment in name_part.split('/') {
        let dir_name = segment.split_once(':').map_or(segment, |(name, _)| name);
        if dir_name == "." || dir_name == ".." {
            return Err(IdentifierError {
                input: input.to_string(),
                kind: IdentifierErrorKind::DirectoryTraversal,
            });
        }
    }
    Ok(())
}

/// Checks whether the input contains an explicit registry in the first path segment.
fn has_explicit_registry(input: &str) -> bool {
    let name_part = input.split_once('@').map_or(input, |(name, _)| name);
    match name_part.split_once('/') {
        None => false,
        Some((first, _)) => first.contains('.') || first.contains(':') || first == "localhost",
    }
}

fn parse_internal(input: &str, default_registry: &str) -> Result<Identifier, IdentifierError> {
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
    // Character-class guard (OCI distribution spec repository grammar,
    // narrowed to what the uppercase check above doesn't already cover): each
    // `/`-segment must be non-empty and contain only lowercase alphanumerics,
    // `.`, `_`, or `-`. Catches garbage input (spaces, punctuation) that would
    // otherwise silently parse into a syntactically well-formed but
    // registry-illegal identifier.
    if repository
        .split('/')
        .any(|segment| segment.is_empty() || !segment.bytes().all(is_repository_segment_byte))
    {
        return Err(IdentifierError {
            input: input.to_string(),
            kind: IdentifierErrorKind::InvalidFormat,
        });
    }

    Ok(Identifier {
        registry,
        repository,
        tag,
        digest,
    })
}

/// Whether `byte` is a legal character inside one `/`-separated repository segment.
fn is_repository_segment_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
}

/// Parses a digest string like `sha256:abcdef...` into a `Digest`.
fn parse_digest(input: &str, digest_str: &str) -> Result<Digest, IdentifierError> {
    Digest::try_from(digest_str).map_err(|_| IdentifierError {
        input: input.to_string(),
        kind: IdentifierErrorKind::DigestInvalidFormat,
    })
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

impl schemars::JsonSchema for Identifier {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Identifier")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "OCI identifier in the format 'registry/repository[:tag][@digest]'."
        })
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

    /// Finding #5: direct coverage for the repository character-class guard
    /// (`is_repository_segment_byte`). The accept side must allow lowercase
    /// alphanumerics plus `.`, `_`, `-` in every `/`-segment; the reject side
    /// must refuse spaces, disallowed punctuation, and empty segments as
    /// `InvalidFormat` (rather than silently parsing registry-illegal input).
    #[test]
    fn repository_char_class_accepts_allowed_boundary() {
        // All allowed special characters across nested segments.
        let id: Identifier = "test.com/foo.bar_baz-qux/a1b2"
            .parse()
            .expect("allowed char classes must parse");
        assert_eq!(id.repository(), "foo.bar_baz-qux/a1b2");
    }

    #[test]
    fn repository_char_class_rejects_space() {
        let err = "test.com/foo bar".parse::<Identifier>().unwrap_err();
        assert!(
            matches!(err.kind, IdentifierErrorKind::InvalidFormat),
            "a space in a repository segment must be rejected as InvalidFormat, got {:?}",
            err.kind
        );
    }

    #[test]
    fn repository_char_class_rejects_disallowed_punctuation() {
        for input in ["test.com/foo!bar", "test.com/foo~bar", "test.com/foo%bar"] {
            let err = input.parse::<Identifier>().unwrap_err();
            assert!(
                matches!(err.kind, IdentifierErrorKind::InvalidFormat),
                "disallowed punctuation in '{input}' must be rejected as InvalidFormat, got {:?}",
                err.kind
            );
        }
    }

    #[test]
    fn repository_char_class_rejects_empty_segment() {
        let err = "test.com/foo//bar".parse::<Identifier>().unwrap_err();
        assert!(
            matches!(err.kind, IdentifierErrorKind::InvalidFormat),
            "an empty repository segment must be rejected as InvalidFormat, got {:?}",
            err.kind
        );
    }

    #[test]
    fn bad_digest_algo_errors() {
        let err = "test.com/repo@md5:abcdef".parse::<Identifier>().unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DigestInvalidFormat));
    }

    #[test]
    fn bad_digest_length_errors() {
        let err = "test.com/repo@sha256:abc".parse::<Identifier>().unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DigestInvalidFormat));
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
        let reference = id.canonical_reference();
        assert_eq!(reference.registry(), "test.com");
        assert_eq!(reference.repository(), "repo");
        assert_eq!(reference.tag(), Some("tag"));
    }

    #[test]
    fn identifier_without_tag_becomes_latest_in_reference() {
        let id: Identifier = "test.com/repo".parse().unwrap();
        let reference = id.canonical_reference();
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

    // ── parse (strict, no default registry) ─────────────────────────────

    #[test]
    fn parse_rejects_bare_name() {
        let err = Identifier::parse("cmake:3.28").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::MissingRegistry));
    }

    #[test]
    fn parse_rejects_org_repo() {
        let err = Identifier::parse("myorg/cmake").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::MissingRegistry));
    }

    #[test]
    fn parse_accepts_explicit_registry() {
        let id = Identifier::parse("ocx.sh/cmake:3.28").unwrap();
        assert_eq!(id.registry(), "ocx.sh");
        assert_eq!(id.repository(), "cmake");
        assert_eq!(id.tag(), Some("3.28"));
    }

    #[test]
    fn parse_accepts_localhost() {
        let id = Identifier::parse("localhost/repo:tag").unwrap();
        assert_eq!(id.registry(), "localhost");
    }

    #[test]
    fn parse_accepts_port() {
        let id = Identifier::parse("localhost:5000/repo:tag").unwrap();
        assert_eq!(id.registry(), "localhost:5000");
    }

    #[test]
    fn parse_rejects_dotdot_traversal() {
        let err = Identifier::parse("ocx.sh/../evil").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DirectoryTraversal));
    }

    #[test]
    fn parse_rejects_dot_traversal() {
        let err = Identifier::parse("ocx.sh/org/./evil").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DirectoryTraversal));
    }

    #[test]
    fn parse_rejects_dotdot_with_tag() {
        let err = Identifier::parse("ocx.sh/..:tag").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DirectoryTraversal));
    }

    #[test]
    fn deserialize_rejects_bare_name() {
        let err = serde_json::from_str::<Identifier>(r#""cmake:3.28""#).unwrap_err();
        assert!(err.to_string().contains("explicit registry"));
    }

    // ── parse_with_default_registry (CLI path) ──────────────────────────

    #[test]
    fn parse_with_default_registry_accepts_bare_name() {
        let id = Identifier::parse_with_default_registry("cmake:3.28", "ocx.sh").unwrap();
        assert_eq!(id.registry(), "ocx.sh");
        assert_eq!(id.repository(), "cmake");
        assert_eq!(id.tag(), Some("3.28"));
    }

    #[test]
    fn parse_with_default_registry_accepts_org_repo() {
        let id = Identifier::parse_with_default_registry("myorg/cmake", "ocx.sh").unwrap();
        assert_eq!(id.registry(), "ocx.sh");
        assert_eq!(id.repository(), "myorg/cmake");
    }

    #[test]
    fn parse_with_default_registry_rejects_dotdot_traversal() {
        let err = Identifier::parse_with_default_registry("../evil/cmake", "ocx.sh").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DirectoryTraversal));
    }

    #[test]
    fn parse_with_default_registry_rejects_dotdot_segment() {
        let err = Identifier::parse_with_default_registry("ocx.sh/../evil", "ocx.sh").unwrap_err();
        assert!(matches!(err.kind, IdentifierErrorKind::DirectoryTraversal));
    }

    #[test]
    fn from_str_uses_default_registry() {
        let id: Identifier = "cmake:3.28".parse().unwrap();
        assert_eq!(id.registry(), DEFAULT_REGISTRY);
        assert_eq!(id.repository(), "cmake");
    }

    // ── ocx_cli_identifier + __OCX_SELF_IMAGE seam ──────────────────────

    /// Without the `__OCX_SELF_IMAGE` env var, the canonical identifier wins.
    #[test]
    fn ocx_cli_identifier_defaults_to_canonical() {
        // Defensive: ensure no leftover env state from a sibling test.
        // SAFETY: tests in this module never read this var concurrently;
        // serial scope of `#[test]` provides the ordering guarantee.
        unsafe { std::env::remove_var("__OCX_SELF_IMAGE") };
        let id = ocx_cli_identifier();
        assert_eq!(id.registry(), OCX_SH_REGISTRY);
        assert_eq!(id.repository(), "ocx/cli");
    }

    /// Parser accepts `<registry>/<repo>` shape with port + multi-segment repo.
    #[test]
    fn parse_self_image_spec_accepts_loopback_with_port() {
        let (registry, repository) = parse_self_image_spec("localhost:5000/ocx/cli").unwrap();
        assert_eq!(registry, "localhost:5000");
        assert_eq!(repository, "ocx/cli");
    }

    /// Empty registry or empty repo → `None`.
    #[test]
    fn parse_self_image_spec_rejects_empty_halves() {
        assert!(parse_self_image_spec("/ocx/cli").is_none());
        assert!(parse_self_image_spec("localhost:5000/").is_none());
        assert!(parse_self_image_spec("no-slash").is_none());
    }

    /// Loopback gate accepts the loopback host set, rejects anything else.
    #[test]
    fn is_loopback_registry_accepts_loopback_set() {
        assert!(is_loopback_registry("localhost"));
        assert!(is_loopback_registry("localhost:5000"));
        assert!(is_loopback_registry("127.0.0.1"));
        assert!(is_loopback_registry("127.0.0.1:443"));
        assert!(is_loopback_registry("[::1]"));
        assert!(is_loopback_registry("[::1]:5000"));
    }

    /// Loopback gate refuses public registries even with the seam compiled in.
    #[test]
    fn is_loopback_registry_rejects_public_hosts() {
        assert!(!is_loopback_registry("ocx.sh"));
        assert!(!is_loopback_registry("ghcr.io"));
        assert!(!is_loopback_registry("registry.example.com:5000"));
        assert!(!is_loopback_registry("192.168.0.1"));
        // Spoof attempts: hostnames that merely embed "localhost".
        assert!(!is_loopback_registry("evil-localhost.example.com"));
        assert!(!is_loopback_registry("localhost.evil.com"));
    }
}
