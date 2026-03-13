# ADR: Custom OCI Identifier (Replace oci_spec::Reference internals)

## Metadata

**Status:** Proposed
**Date:** 2026-03-12
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** api
**Supersedes:** N/A
**Superseded By:** N/A

## Context

`ocx_lib::oci::Identifier` wraps `oci_spec::distribution::Reference` (re-exported through `oci-client`). The upstream `Reference` was designed for Docker/container workflows and has three behaviors that conflict with OCX's semantics:

1. **Silent `latest` injection.** When neither tag nor digest is present, `Reference::try_from` forces `tag = Some("latest")`. OCX needs to distinguish "user said `cmake`" (no tag) from "user said `cmake:latest`" (explicit tag). Currently compensated with a `has_explicit_tag()` pre-parse and an `explicit_tag: bool` field.

2. **Docker Hub domain defaulting.** `split_domain()` defaults bare names to `docker.io` and prepends `library/` for single-segment repos. OCX's default registry is `ocx.sh`, and it never wants Docker Hub expansion. Currently compensated by running `prepend_domain()` before parsing, which adds the correct registry so `split_domain` sees a domain and leaves it alone.

3. **Opaque parse errors.** `oci_spec::ParseError` provides category-level errors (`ReferenceInvalidFormat`, `NameTooLong`) but no input context. OCX currently maps all parse errors to `Error::Undefined` — the user gets no indication of what failed.

These workarounds are well-tested and functional. The question is whether they should remain workarounds or become first-class design.

## Decision Drivers

- **Correctness**: Tag presence/absence is semantically meaningful in OCX (determines whether `index.select()` returns `Ambiguous` vs looks up a specific tag). The `explicit_tag` workaround is correct but fragile — any new path that constructs an `Identifier` must remember to set it.
- **Error quality**: Users get `Error::Undefined` for parse failures. A custom parser can produce `"invalid identifier 'cmake:3.28@bad': unsupported digest format"`.
- **Simplicity**: OCX's identifier grammar is a strict subset of Docker's. No `library/` prefix, no `index.docker.io` aliasing, no mirror registry support. A simpler grammar means a simpler parser.
- **Maintenance**: The `oci_spec` regex is 400+ characters and requires a 10MB size limit. OCX can validate with straightforward string splitting.
- **Coupling**: `identifier.reference` is `pub(crate)` and accessed in 11 places in `oci::client.rs`. Every access crosses the abstraction boundary.

## Considered Options

### Option A: Custom internal storage, Reference at transport boundary

**Description:** Replace `Identifier`'s internal `reference: native::Reference` field with plain fields (`registry`, `repository`, `tag: Option`, `digest: Option`). Implement OCX's own `FromStr` parser. Add a `to_reference()` method that constructs a `native::Reference` for the `OciTransport` trait boundary. The `OciTransport` trait itself remains unchanged.

| Pros | Cons |
|------|------|
| Tag presence is structurally correct (`Option` = no workaround) | Must maintain a custom parser (~50 lines) |
| Parse errors include the raw input and specific reason | Two representations exist (Identifier + Reference) |
| `prepend_domain` becomes part of normal construction, not a pre-parse hack | `to_reference()` conversion at transport boundary (mechanical) |
| No regex dependency for identifier parsing | |
| `mirror_registry` field no longer carried (OCX doesn't use it) | |

### Option B: Custom Identifier AND custom OciTransport signature

**Description:** Same as Option A, but also change `OciTransport` methods to accept `(&str, &str, Option<&str>, Option<&str>)` or a custom transport-level reference type instead of `&native::Reference`.

| Pros | Cons |
|------|------|
| Complete decoupling from oci_spec::Reference | Breaks the natural boundary — transport should speak oci-client types |
| No `to_reference()` conversion needed | `native_transport.rs` must reconstruct Reference internally anyway |
| | More churn for zero behavioral benefit |

### Option C: Status quo (keep wrapping Reference)

**Description:** Keep the current design. Improve error handling for `ParseError` by adding context, but keep `Reference` as internal storage.

| Pros | Cons |
|------|------|
| Zero code change risk | `explicit_tag` workaround remains (fragile for new code paths) |
| Proven in production | Parse errors still lack input context (or require separate wrapping) |
| | Carries unused `mirror_registry` field |
| | Docker Hub defaulting logic runs on every parse (then gets overridden) |

## Decision Outcome

**Chosen Option:** Option A — Custom internal storage, Reference at transport boundary.

**Rationale:** The workarounds work, but they're incidental complexity that will catch future contributors. Tag presence should be structural (`Option<String>`) not behavioral (`bool` flag). The transport boundary is the natural seam — `OciTransport` methods need a `Reference` because `oci-client`'s native transport implementation uses it for URL construction. Converting at that boundary is correct and mechanical.

### Consequences

**Positive:**
- `Identifier::tag()` returns `Option<&str>` naturally — no `explicit_tag` gymnastics
- Parse errors carry the raw input string and a specific reason
- `prepend_domain` becomes part of construction, not a pre-parse step
- Simpler mental model: Identifier is OCX's type, Reference is oci-client's type
- `Deserialize` impl no longer needs `has_explicit_tag()` pre-check

**Negative:**
- `From<&Identifier> for Reference` conversion at ~11 call sites in `client.rs` (mechanical, but present)
- `TryFrom<Reference> for Identifier` is lossy — cannot recover whether `"latest"` tag was explicit or injected by `Reference`; callers must set tag explicitly after conversion if intent matters
- Custom parser must be tested against edge cases (port numbers, nested repos, digest formats)
- `Display` output must remain compatible with current format for JSON serialization stability

**Risks:**
- Parser divergence from OCI spec. Mitigation: OCX's grammar is a subset — test against the `oci_spec` test vectors that apply (skip Docker Hub ones).

## Technical Details

### Architecture

```
User input: "cmake:3.28"
                │
    ┌───────────▼───────────────┐
    │ Identifier::from_str()    │  ← OCX's own parser
    │   or ::from_str_with_     │    (no regex, string splitting)
    │      registry()           │
    └───────────┬───────────────┘
                │
    ┌───────────▼───────────────┐
    │ Identifier {              │
    │   registry: "ocx.sh"     │  ← Plain fields, correct semantics
    │   repository: "cmake"    │
    │   tag: Some("3.28")      │
    │   digest: None            │
    │ }                         │
    └───────────┬───────────────┘
                │ (only when calling OciTransport)
    ┌───────────▼───────────────┐
    │ identifier.to_reference() │  ← Converts to native::Reference
    │ → Reference::with_tag()   │    at the transport boundary
    └───────────┬───────────────┘
                │
    ┌───────────▼───────────────┐
    │ OciTransport methods      │  ← Unchanged, still takes &Reference
    └───────────────────────────┘
```

### API Contract

```rust
/// OCX's own OCI identifier. Does not default tag to "latest".
/// Does not perform Docker Hub domain expansion.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Identifier {
    registry: String,
    repository: String,
    tag: Option<String>,
    digest: Option<Digest>,
}

impl Identifier {
    // ── Constructors ────────────────────────────────────────
    pub fn new(registry: impl Into<String>, repository: impl Into<String>) -> Self;
    pub fn from_str_with_registry(s: &str, registry: &str) -> Result<Self>;

    // ── Accessors (unchanged public API) ────────────────────
    pub fn registry(&self) -> &str;
    pub fn repository(&self) -> &str;
    pub fn name(&self) -> Option<&str>;       // last segment of repository
    pub fn tag(&self) -> Option<&str>;        // None = not provided (no workaround)
    pub fn tag_or_latest(&self) -> &str;      // convenience, returns "latest" if None
    pub fn digest(&self) -> Option<&Digest>;

    // ── Mutation ────────────────────────────────────────────
    pub fn with_tag(&self, tag: impl Into<String>) -> Self;
    pub fn with_digest(&self, digest: Digest) -> Self;
}
```

### Error Type

Currently, identifier-related errors are scattered across the library `Error` enum:

| Current variant | Source | Problem |
|---|---|---|
| `From<oci::ParseError>` → `Error::Undefined` | `Reference::try_from` | Loses all context — raw input, error kind |
| `Error::PackageDigestInvalid(String)` | `oci::Digest::try_from` | Digest is part of identifier parsing |
| `Error::PackageVersionInvalid(String)` | `package::Version::try_from` | Version/tag is part of identifier parsing |

A dedicated `IdentifierError` consolidates these into one type with `From<IdentifierError> for Error` auto-conversion:

```rust
/// Dedicated error type for OCI identifier parsing and conversion.
/// Carries the raw input for actionable error messages.
#[derive(Debug)]
pub struct IdentifierError {
    input: String,
    kind: IdentifierErrorKind,
}

#[derive(Debug)]
pub enum IdentifierErrorKind {
    /// Input is empty.
    Empty,
    /// Repository name contains uppercase characters.
    UppercaseRepository,
    /// Repository name exceeds 255 characters.
    RepositoryTooLong,
    /// Digest algorithm is not supported (sha256/sha384/sha512).
    DigestUnsupported(String),
    /// Digest hex length does not match algorithm.
    DigestInvalidLength { algorithm: String, expected: usize, actual: usize },
    /// Digest format is malformed (missing ':', etc).
    DigestInvalidFormat,
    /// General format error (no valid repository/tag/digest structure).
    InvalidFormat,
    /// Reference carries docker.io defaults — not valid in OCX context.
    DockerHubDefault,
}

impl std::fmt::Display for IdentifierError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // e.g. "invalid identifier 'cmake:3.28@bad': unsupported digest algorithm 'bad'"
        // e.g. "invalid identifier '': identifier must not be empty"
    }
}

impl std::error::Error for IdentifierError {}

// Auto-conversion into the library error type.
impl From<IdentifierError> for crate::Error {
    fn from(e: IdentifierError) -> Self {
        crate::Error::Identifier(e)
    }
}
```

This replaces `Error::PackageDigestInvalid`, `Error::PackageVersionInvalid`, and `From<oci::ParseError> → Error::Undefined` with a single `Error::Identifier(IdentifierError)` variant.

### Trait Implementations

Standard Rust traits for parsing, display, serialization, and native type conversion:

```rust
// ── Parsing ─────────────────────────────────────────────
// FromStr uses DEFAULT_REGISTRY ("ocx.sh") as the default domain.
// The default domain decision lives here and in from_str_with_registry().
impl FromStr for Identifier {
    type Err = IdentifierError;
    // "cmake:3.28"          → registry="ocx.sh", repo="cmake", tag=Some("3.28")
    // "cmake"               → registry="ocx.sh", repo="cmake", tag=None
    // "ghcr.io/org/tool:1"  → registry="ghcr.io", repo="org/tool", tag=Some("1")
}

// ── Display ─────────────────────────────────────────────
impl Display for Identifier {
    // "{registry}/{repository}[:{tag}][@{digest}]"
    // Must match current output for serialization stability.
}

// ── Serde ───────────────────────────────────────────────
impl Serialize for Identifier   { /* delegates to Display */ }
impl Deserialize for Identifier { /* delegates to FromStr — note: no default registry context */ }

// ── Conversion to native::Reference (infallible) ────────
// Identifier → Reference always succeeds: we have all fields.
// Tag defaults to "latest" at this boundary (Reference requires it).
impl From<&Identifier> for native::Reference {
    fn from(id: &Identifier) -> Self {
        match (&id.tag, &id.digest) {
            (Some(tag), Some(digest)) => Reference::with_tag_and_digest(
                id.registry.clone(), id.repository.clone(),
                tag.clone(), digest.to_string(),
            ),
            (Some(tag), None) => Reference::with_tag(
                id.registry.clone(), id.repository.clone(), tag.clone(),
            ),
            (None, Some(digest)) => Reference::with_digest(
                id.registry.clone(), id.repository.clone(), digest.to_string(),
            ),
            (None, None) => Reference::with_tag(
                id.registry.clone(), id.repository.clone(), "latest".into(),
            ),
        }
    }
}

// ── Conversion from native::Reference (fallible) ────────
// Reference → Identifier may fail: Reference may carry docker.io
// defaults or a "latest" tag that should be None. We reject
// References with docker.io registry (they don't belong in OCX).
// Tag is preserved as-is — caller is responsible for intent since
// Reference already injected "latest" if absent.
impl TryFrom<native::Reference> for Identifier {
    type Error = IdentifierError;
    fn try_from(r: native::Reference) -> Result<Self, IdentifierError> {
        // Reject docker.io/library/* — these are Docker Hub defaults
        // that should never appear in OCX.
        // Convert digest string to Digest type (may fail → IdentifierError).
        // Tag: preserve as-is (caller responsible for intent).
    }
}
```

### Default Domain Flow

The default registry decision is made at two explicit points — never silently:

```
CLI layer (options::Identifier)          ocx_lib (oci::Identifier)
─────────────────────────────────        ────────────────────────────────
User types: "cmake:3.28"
         │
         ▼
options::Identifier { raw: "cmake:3.28" }
         │
         │  .with_domain(context.default_registry())
         │  ← domain comes from OCX_DEFAULT_REGISTRY env / "ocx.sh"
         ▼
oci::Identifier::from_str_with_registry("cmake:3.28", "ocx.sh")
         │
         │  prepend_domain("cmake:3.28", "ocx.sh") → "ocx.sh/cmake:3.28"
         │  parse → { registry: "ocx.sh", repo: "cmake", tag: Some("3.28") }
         ▼
oci::Identifier { registry: "ocx.sh", repository: "cmake", tag: Some("3.28") }
```

`FromStr` on `oci::Identifier` also applies a default (`DEFAULT_REGISTRY = "ocx.sh"`), used for:
- Deserialization (JSON/index files that store bare identifiers)
- Direct construction in tests

The `options::Identifier` in `ocx_cli` continues to work unchanged — it stores the raw string and calls `with_domain()` at the CLI boundary, which delegates to `from_str_with_registry()`.

### Parsing Grammar (OCX subset)

```
identifier     = [ registry "/" ] repository [ ":" tag ] [ "@" digest ]
registry       = host [ ":" port ]
host           = domain-component ( "." domain-component )* | "localhost"
port           = [0-9]+
repository     = path-component ( "/" path-component )*
path-component = [a-z0-9]+ ( ( [._-] | "__" ) [a-z0-9]+ )*
tag            = [a-zA-Z0-9_][a-zA-Z0-9._+-]{0,127}
digest         = algorithm ":" hex
algorithm      = "sha256" | "sha384" | "sha512"
```

**Key difference from Docker:** When `registry` is absent, OCX prepends the configured default registry (env `OCX_DEFAULT_REGISTRY`, default `ocx.sh`) — never `docker.io`, never `library/`.

### Domain Detection (from current `prepend_domain`, preserved)

A leading path segment is treated as a registry if any of:
- Contains `.` (domain: `ghcr.io/repo`, `my.registry.com/repo`)
- Contains `:` (port: `localhost:5000/repo`)
- Is literally `localhost`

Otherwise the default registry is prepended: `cmake` → `ocx.sh/cmake`, `myorg/cmake` → `ocx.sh/myorg/cmake`.

## Implementation Plan

### Phase 1: Add custom fields alongside Reference (non-breaking)

1. [ ] Add `registry: String`, `repository: String`, `tag: Option<String>`, `digest: Option<Digest>` fields to `Identifier`
2. [ ] Populate them in all constructors (`from_str`, `from_str_with_registry`, `new_registry`, `clone_with_tag`, `clone_with_digest`)
3. [ ] Implement `From<&Identifier> for native::Reference` (infallible, replaces `to_reference()`)
4. [ ] Implement `TryFrom<native::Reference> for Identifier` (fallible — reject docker.io defaults, handle tag ambiguity)
5. [ ] Switch all accessor methods (`registry()`, `repository()`, `tag()`, `digest()`) to read from new fields
6. [ ] Run `task verify` — all tests pass, behavior unchanged

### Phase 2: Update client.rs call sites

7. [ ] Replace `identifier.reference` accesses in `client.rs` with `native::Reference::from(&identifier)` (or `.into()`)
8. [ ] Replace `identifier.reference.clone()` with `native::Reference::from(&identifier)`
9. [ ] Replace dummy `Reference::with_tag(...)` in `list_repositories` with conversion from a constructed `Identifier`
10. [ ] Run `task verify`

### Phase 3: Error type and custom parser

11. [ ] Add `IdentifierError` struct + `IdentifierErrorKind` enum in `oci/identifier/error.rs` (or `oci/identifier_error.rs`)
12. [ ] Add `Error::Identifier(IdentifierError)` variant to `crate::Error`, implement `From<IdentifierError> for Error`
13. [ ] Write custom `parse_identifier(input, default_registry) -> Result<Identifier, IdentifierError>` (string splitting, no regex)
14. [ ] Replace `FromStr` and `from_str_with_registry` to use custom parser, return `IdentifierError`
15. [ ] Migrate `oci::Digest::try_from` to return `IdentifierError` (digest is part of identifier) — or keep `Digest` independent and convert at the boundary
16. [ ] Remove `reference` field and `explicit_tag` field
17. [ ] Remove `has_explicit_tag()` function
18. [ ] Implement custom `Display` (must match current output: `{registry}/{repository}[:{tag}][@{digest}]`)
19. [ ] Update `Serialize`/`Deserialize` to use custom `Display`/`FromStr`
20. [ ] Remove `Error::PackageDigestInvalid`, `Error::PackageVersionInvalid`, `From<oci::ParseError> for Error`
21. [ ] Run `task verify`

### Phase 4: Test hardening

22. [ ] Port applicable test vectors from `oci_spec::Reference` tests (skip Docker Hub-specific cases like `busybox` → `docker.io/library/busybox`)
23. [ ] Add `IdentifierError` quality tests (verify error messages contain raw input and specific kind)
24. [ ] Add roundtrip tests: `parse → display → parse` identity
25. [ ] Add `From`/`TryFrom` conversion roundtrip tests (`Identifier → Reference → TryFrom → Identifier`)
26. [ ] Verify `options::Identifier` (CLI layer) works unchanged — `with_domain()` still delegates correctly
27. [ ] Run `task verify` + acceptance tests

## Validation

- [ ] All existing unit tests in `identifier.rs` pass unchanged (Phase 1)
- [ ] All acceptance tests pass (each phase)
- [ ] `cargo clippy --workspace` clean
- [ ] Parse error messages include raw input string
- [ ] `Display` output matches current format (no serialization regression)
- [ ] `From<&Identifier> for Reference` produces equivalent `Reference` to what was previously stored
- [ ] `TryFrom<Reference> for Identifier` rejects docker.io defaults, preserves valid references
- [ ] `options::Identifier` (CLI layer) works unchanged — `with_domain()` delegates correctly
- [ ] Conversion roundtrip: `Identifier → Reference → TryFrom → Identifier` preserves all fields (for non-docker.io refs)

## Links

- [oci-spec Reference source](https://github.com/opencontainers/image-spec/) (v0.9.0, `/src/distribution/reference.rs`)
- [Docker distribution normalize.go](https://github.com/distribution/distribution/blob/main/reference/normalize.go) — origin of `split_domain`
- [OCI Distribution Spec](https://github.com/opencontainers/distribution-spec/blob/main/spec.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-12 | mherwig + claude | Initial draft |
