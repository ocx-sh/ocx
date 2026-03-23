# ADR: Package Variants

## Metadata

**Status:** Accepted
**Date:** 2026-03-22
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** api | data | integration
**Supersedes:** N/A
**Superseded By:** N/A

## Context

OCX currently treats OCI tags as flat version strings (`3.28.1`, `3.28`, `latest`). There is no concept of a package having multiple **variants** — builds with different software-level characteristics such as optimization profiles, feature sets, or embedded dependencies.

### Variant vs. Platform

This distinction is critical:

- **Platform** (os/arch) defines **where** a binary runs. A `linux/amd64` binary cannot run on `darwin/arm64`. OCX already handles this via multi-platform OCI image indexes — one tag, multiple platform entries in the manifest. Platform selection is automatic based on the host.
- **Variant** defines **how** a binary was built — optimization profiles (`pgo`, `pgo.lto`), feature toggles (`freethreaded`), size trade-offs (`slim`), or bundled dependency choices. All variants for a given platform run on the same host; they differ in performance, size, or capability. Variant selection is a **user choice**, not auto-detected.

**Real-world example — python-build-standalone (Astral):**
- Platform: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin` (auto-selected)
- Variants: `pgo.lto` (fastest, default), `pgo` (fast), `debug` (debuggable), `freethreaded` (experimental GIL-free)

Note: libc choice (`musl` vs `glibc`) is a **platform concern** in OCX, not a variant — it determines binary compatibility. Future work may extend `Platform` to include libc, but that is out of scope for this ADR.

### Why Variants Are Needed

Without variant support, users must create separate packages with variant-encoded names (e.g., `python-pgo`, `python-debug`), losing the semantic connection between them and duplicating mirror configurations.

Docker Hub has a convention for this (`python:3.12-slim`, `node:22-alpine`), but uses variant-suffix format (`<version>-<variant>`), which creates a **parsing ambiguity** with semver prereleases (`3.12.5-alpha` vs `3.12.5-slim` — syntactically indistinguishable).

OCX adopts a **variant-prefix** format (`<variant>-<version>`) instead, which is unambiguous because variants start with a letter and versions start with a digit:

- `python:3.12` is the **default variant** (e.g., `pgo.lto`), identical in content to `python:pgo.lto-3.12`
- `python:debug-3.12` is an explicit non-default variant
- `python:debug` is the rolling tag for the latest debug variant
- `latest` refers to the latest version of the default variant

### How Docker Tags Work

Docker tags are purely a naming convention — the OCI distribution spec has no native variant concept. Tags like `python:3.12` and `python:3.12-bookworm` are simply two tags pointing to the **same manifest digest**. Docker uses suffix format (`<version>-<variant>`), but OCX uses **prefix format** (`<variant>-<version>`) to avoid ambiguity with semver prereleases:

| Pattern | Example | Meaning |
|---------|---------|---------|
| `<version>` | `python:3.12` | Latest build of default variant at version 3.12 |
| `<variant>-<version>` | `python:debug-3.12` | Explicit variant at version 3.12 |
| `<variant>` | `python:debug` | Latest version of the debug variant |
| `latest` | `python:latest` | Latest version of the default variant |

Rolling tags cascade within their variant track: `debug-3.12.5_b1` → `debug-3.12.5` → `debug-3.12` → `debug-3` → `debug`.

## Decision Drivers

- Users need to publish multiple builds of the same tool with different software characteristics (optimization profiles, feature sets, size trade-offs)
- The tag naming must be compatible with existing OCI registries (pure convention, no spec extensions)
- The cascade/rolling version system must work per-variant
- A default variant must exist so that `cmake:3.28` continues to work without specifying a variant
- Mirror configs must declare variants without duplicating the entire spec
- CI workflows computing cascade tags must be variant-aware

## Industry Context & Research

**Research artifacts:** `research_package_variants.md`, `research_oci_variants.md`

### Cross-Ecosystem Survey

Ten package management systems were analyzed for variant modeling:

| System | Variant Encoding | Parsing Ambiguity | Key Insight |
|--------|-----------------|-------------------|-------------|
| Docker/OCI | Tag suffix `<version>-<variant>` | High: `-rc1` vs `-slim` | De facto standard but structurally ambiguous |
| Homebrew | No software variants (removed ~2019) | None | Simplicity won over flexibility; CI cost of variants was too high |
| Nix | Attribute paths `pkgs.foo.withBar` | None (typed) | Maximally precise but requires Nix's evaluation model |
| conda | Build string with SHA1 hash | Medium (opaque) | Cautionary tale: opaque hashes make variant discovery painful ([conda#6439](https://github.com/conda/conda/issues/6439)) |
| Conan 2.x | Content-addressed SHA1 | None (no human name) | Theoretically clean but requires tooling to navigate |
| Cargo | Feature flags (source only) | None | Additive features produce 2^N combinations; not viable for pre-built binaries |
| Python wheels | Filename tags (PEP 425) | Low (structured) | No software variant concept; PyTorch uses separate indexes as workaround |
| Spack | Spec string `+mpi ~cuda` | Low (explicit sigils) | Most rigorous model; formal ASP solver ([arxiv:2509.07728](https://arxiv.org/html/2509.07728)) |
| vcpkg | Triplet + feature set | None | Triplet = platform, features = variant — explicitly orthogonal |
| python-build-standalone | Filename `{triple}-{flavor}` | Low (structured) | Closest precedent: two-axis platform × variant for pre-built binaries |

### OCI Spec Status

The OCI specification has **no native concept of software variants**:

- **`platform.variant`** — CPU microarchitecture only (ARM v6/v7/v8, x86-64 v1/v2/v3). No runtime implements selection on non-CPU values. ([image-spec v1.1.1](https://github.com/opencontainers/image-spec/blob/v1.1.1/image-index.md))
- **`platform.features`** — Reserved since 2019 ([moby/moby#38715](https://github.com/moby/moby/issues/38715)). No spec progress, no runtime support.
- **OCI v1.1 Referrers API** — For subordinate artifacts (SBOMs, signatures), not parallel variant selection.
- **Active discussion: [image-spec#1216](https://github.com/opencontainers/image-spec/issues/1216)** — Proposes annotation-based platform metadata for non-image artifacts. ORAS implemented under `land.oras.artifact.platform.*` namespace ([oras#1538](https://github.com/oras-project/oras/issues/1538)). OCI has not adopted it.

### The Suffix Ambiguity Problem

Docker's suffix format (`<version>-<variant>`) creates a parsing ambiguity with semver prereleases that the community acknowledges but has not solved. [Concourse's registry-image-resource](https://github.com/concourse/registry-image-resource) documents this explicitly: "Variants and pre-releases both use the same syntax (e.g., `1.2.3-alpine` is technically valid syntax for a Semver prerelease), so resources will only consider prerelease data starting with `alpha`, `beta`, or `rc` as a proper prerelease, treating anything else as a variant." This confirms the ambiguity is real and requires heuristic workarounds.

### Key Insights

1. **Platform and variant are orthogonal axes.** Platform (os/arch/libc) determines binary compatibility and is auto-selected. Variant (optimization profile, features, size) is a user choice. Docker conflates both in tag suffixes. python-build-standalone keeps them separate. OCX formalizes this distinction.
2. **Variant-prefix format is novel but well-justified.** No existing tool uses `<variant>-<version>`, but the parsing clarity (letter vs digit boundary) eliminates the ambiguity that plagues Docker's suffix format.
3. **"Default variant = simplest identifier" is universal.** Every ecosystem reserves the bare identifier for the most common variant (Docker: unadorned version, Nix: `default` attribute, Spack: declared defaults). OCX follows this pattern.
4. **Cascade-per-variant-track is an OCX innovation.** No existing ecosystem combines rolling version tags with per-variant tracks.
5. **Every variant must concretize to exactly one value.** Spack's formal ASP model validates OCX's requirement for exactly one `default: true` variant — without a declared default, variant selection is ambiguous.
6. **Named variants beat opaque hashes.** conda's SHA1-based build strings are widely criticized for making variant discovery impossible without extracting package internals. OCX's human-readable named variants avoid this.

## Considered Options

### Option 1: Tag Convention with Variant Prefix (`<variant>-<version>`)

**Description:** Tags are `<variant>-<version>` for explicit variants, `<version>` for the default variant. The default variant's version tags are **aliases** (same manifest digest) to the explicit variant tag. Variant metadata stored in package annotations. Differs from Docker's suffix convention to avoid prerelease ambiguity.

| Pros | Cons |
|------|------|
| Unambiguous parsing (variant starts with letter, version starts with digit) | Differs from Docker's suffix convention |
| Zero OCI spec extensions needed | Requires careful cascade logic per variant track |
| Default variant = unadorned version tags (backward-compatible) | Mirror specs need variant declarations |
| Rolling tags work naturally per variant | More tags per package (one set per variant) |
| No conflict with semver prerelease (`-alpha`) or build (`_timestamp`) | |

### Option 2: Separate Repositories Per Variant

**Description:** Each variant gets its own OCI repository (e.g., `python`, `python-debug`, `python-freethreaded`).

| Pros | Cons |
|------|------|
| Simple — no tag format changes | Loses semantic connection between variants |
| Each repo has clean version space | Duplicates mirror configs |
| No cascade changes needed | Users must discover variant repos manually |
| | No central "which variants exist?" query |

### Option 3: OCI Annotations + Index Manifest Grouping

**Description:** Use OCI image index annotations to group variants. Each variant is an entry in the image index manifest with a custom `ocx.variant` annotation, rather than encoding variants in tags.

| Pros | Cons |
|------|------|
| Semantically clean — variant is structured metadata | No registry UI understands custom annotations |
| Single tag per version, variants inside manifest | Breaks `docker pull`-style tooling |
| | Conflicts with platform (os/arch) entries in image index |
| | Can't use standard tag-based selection |

### Option 4: Metadata/Configuration-Based Variant Selection

**Description:** Store variant information in OCX package metadata or manifest annotations, and select variants by inspecting metadata rather than tag names. The tag would be `python:3.12`, and OCX would download the OCI image index, inspect each manifest's annotations to find the requested variant, then pull the matching manifest.

| Pros | Cons |
|------|------|
| Separates variant metadata from naming | **O(N) manifest downloads** — must inspect each manifest to find the variant |
| Could support richer variant metadata | OCI image index descriptors do NOT include annotations from referenced manifests |
| | Cannot filter at the index level — variant info is only in individual manifests |
| | Breaks the 1:1 tag-to-artifact mapping that makes Docker UX simple |
| | Requires custom OCX tooling for every operation; standard OCI tools cannot select variants |

**Why this was rejected:** The critical technical constraint is that **OCI image index entries carry `platform` fields but NOT manifest annotations**. To find a variant by annotation, OCX would need to download every manifest referenced by the index — one HTTP request per platform per variant. For a package with 4 platforms × 3 variants, that's 12 manifest downloads just to find the right one, vs. a single tag pull with Option 1. This is both slower and more complex, with no compensating benefit.

### Option 5: Separate Mirror Configuration Files Per Variant

**Description:** Use OCX's existing `extends` mechanism to create one mirror YAML file per variant, with a shared base configuration.

| Pros | Cons |
|------|------|
| No new mirror spec fields needed | **Quadratic file proliferation**: (LTS version tracks) × (variants) = many files |
| Each file is self-contained | `extends` does shallow merge — entire `assets` block replaced, not merged |
| Clear separation of concerns | No single-file overview of all variants for a package |
| | Variant relationship (which is the default?) must be encoded elsewhere |

**Why this was rejected:** For a package like Python with 3 variants and 3 LTS tracks (3.11, 3.12, 3.13), this produces 9+ configuration files where inline variants in a single file per track would produce 3 files. The `extends` mechanism's shallow merge means each variant file must duplicate all non-overridden fields, losing the inheritance benefit. The inline `variants:` approach keeps all variant declarations co-located and the default designation explicit.

## Decision Outcome

**Chosen Option:** Option 1 — Tag Convention with Variant Prefix

**Rationale:** The variant-prefix format (`<variant>-<version>`) provides unambiguous parsing — variants start with a letter, versions start with a digit — eliminating the prerelease/variant conflict inherent in Docker's suffix format. It requires no OCI spec extensions and works with every existing registry. It maintains backward compatibility (packages without variants continue to work). The main complexity is in cascade logic, which is already well-architected in OCX and can be extended per-variant.

**Why tag convention over metadata-based selection:** The 1:1 correspondence between "the tag you specify" and "the artifact you get" is the simplest possible UX. Tag-based selection is O(1) — one HTTP request to pull the tag. Metadata-based selection (Option 4) would require downloading every manifest in the image index to inspect annotations, because OCI image index entries do NOT carry annotations from their referenced manifests. For automation consumers (CI/CD, Bazel, GitHub Actions), a deterministic tag identifier (`python:debug-3.12`) is the natural interface — it matches Docker's mental model and works with every OCI tool.

**Why inline variants over separate files:** The inline `variants:` section in the mirror spec keeps all variant declarations co-located in a single file per package (or per LTS track). The alternative — separate mirror YAML files per variant using `extends` — creates quadratic file proliferation for packages with multiple LTS tracks × multiple variants, and OCX's `extends` mechanism does shallow merge (entire top-level keys replaced, not merged), making inheritance coarse and requiring duplication.

### Consequences

**Positive:**
- Familiar convention for anyone who has used Docker
- Backward-compatible — existing packages without variants are unaffected
- Registries, UIs, and tooling all work without modification
- Default variant tags (`cmake:3.28`) resolve exactly as today

**Negative:**
- More tags pushed per package (each variant has its own cascade chain)
- Mirror specs become slightly more complex (variant declarations)
- The `Version` type must become variant-aware for cascade computation

**Risks:**
- Tag explosion for packages with many variants × many versions. Mitigation: most tools have 1-3 variants.
- No parsing ambiguity risk with variant-prefix format (variants start with `[a-z]`, versions start with `[0-9]`).

## Technical Details

### Tag Format

```
<version>                      # default variant (alias to <default_variant>-<version>)
<variant>-<version>            # explicit variant
<variant>                      # latest version of variant (rolling)
latest                         # latest version of default variant (rolling)
```

**Parsing rule:** The first `-` that is followed by a digit marks the boundary between variant prefix and version. If the tag starts with a digit, there is no variant (default variant).

**Examples for `python` with default variant `pgo.lto`:**

| Tag | Points to | Rolling? |
|-----|-----------|----------|
| `pgo.lto-3.12.5_b1` | Build-tagged pgo.lto | No (pinned) |
| `pgo.lto-3.12.5` | Rolling patch for pgo.lto | Yes |
| `pgo.lto-3.12` | Rolling minor for pgo.lto | Yes |
| `pgo.lto-3` | Rolling major for pgo.lto | Yes |
| `pgo.lto` | Latest pgo.lto | Yes |
| `debug-3.12.5_b1` | Build-tagged debug | No (pinned) |
| `debug-3.12.5` | Rolling patch for debug | Yes |
| `debug-3.12` | Rolling minor for debug | Yes |
| `debug` | Latest debug | Yes |
| `3.12.5_b1` | = `pgo.lto-3.12.5_b1` (alias) | No |
| `3.12.5` | = `pgo.lto-3.12.5` (alias) | Yes |
| `3.12` | = `pgo.lto-3.12` (alias) | Yes |
| `3` | = `pgo.lto-3` (alias) | Yes |
| `latest` | = `pgo.lto` (alias) | Yes |

**Key rule:** Tags without a variant prefix always refer to the **default variant**. They share the same manifest digest as the explicit default variant tag.

### Variant Naming Rules

- Must match `[a-z][a-z0-9.]*` (lowercase, starts with letter, no `-` or `_`)
- Must not start with a digit (this is what makes parsing unambiguous)
- Must not be `latest` (reserved — always means "latest default variant")
- Separator between variant and version is `-` (hyphen)
- No `-` within variant names (the first `-<digit>` marks the version boundary)
- Multi-word variants use `.` as separator: `pgo.lto`, `no.opt`

### Rejected Tag Patterns

- **`<variant>-latest`** (e.g., `debug-latest`) — **explicitly rejected and must produce an error**. The bare variant name (`debug`) already means "latest version of this variant." Allowing `debug-latest` would create redundancy and confusion (is `debug-latest` the same as `debug`? what about `debug-latest-3.12`?). The word `latest` is reserved and must never appear as a version component after a variant prefix.
- **`latest-<version>`** — rejected because `latest` as a variant name is reserved.

### Version Type Changes

The `Version` struct gains an optional variant:

```rust
pub struct Version {
    major: u32,
    rest: Option<(u32, MinorRest)>,
    variant: Option<String>,  // NEW: e.g., "pgo.lto", "debug", "slim"
}
```

**Display format:** `debug-3.12.5` (variant prepended before version, separated by `-`). Version without variant displays as before: `3.12.5`.

**Parse rule:** If the tag starts with `[a-z]`, split at the first `-` followed by `[0-9]` — left is variant, right is version. If the tag starts with `[0-9]`, it has no variant (default). Examples:
- `debug-3.12.5-alpha_b1` → variant=`debug`, version=`3.12.5-alpha_b1`
- `3.12.5-alpha_b1` → variant=None, version=`3.12.5-alpha_b1`
- `pgo.lto-3.12.5_b1` → variant=`pgo.lto`, version=`3.12.5_b1`

**Important:** The `Version` struct always requires at least a `major` version number. Variant-only tags (e.g., `debug` without any version) are NOT representable as `Version` objects — they are handled at the `Tag` layer (see below).

**Ordering:** The `Ord` implementation must include the variant field. Variants sort lexicographically before version comparison: `None` (default variant) sorts before `Some("debug")`, which sorts before `Some("pgo.lto")`. This ensures that BTreeSet ranges in the cascade algorithm correctly partition versions by variant track.

### Tag Type Changes

The `Tag` enum as implemented:

```rust
pub enum Tag {
    Latest,                    // latest version of default variant
    Internal(InternalTag),     // __ocx.desc, __ocx.* (forward-compatible)
    Version(Version),          // 3.12.5, debug-3.12.5, etc. (always has major)
    Canonical(String),         // sha256:...
    Other(String),             // bare variant names, custom tags
}

pub enum InternalTag {
    Description,               // __ocx.desc
    Unknown(String),           // forward-compatible for future internal tags
}
```

**Parse order for `Tag::from()`:**
1. `"latest"` → `Tag::Latest`
2. `"__ocx.*"` → `Tag::Internal(InternalTag::Description | Unknown)`
3. Version-parseable (digit-first or variant-prefixed `<variant>-<digit>...`) → `Tag::Version`
4. Canonical digest (`sha256:...`) → `Tag::Canonical`
5. Anything else → `Tag::Other` (includes bare variant names like `"debug"`, `"canary"`)

**Design decision:** Bare variant names (e.g., `"debug"`) fall into `Tag::Other` rather than a dedicated variant. The `Tag` enum is purely syntactic — variant semantics are determined at a higher layer (mirror spec, package annotations) where declared variants are known. `"canary"` is not reserved — it's a valid variant name like any other.

**Validation:** `"latest"` is the only reserved variant prefix — `latest-3.12` returns `None` from `Version::parse()`. Tags like `debug-latest` also return `None` (no digit after `-`).

**Cascade behavior:** Cascading happens **within the same variant track**. `debug-3.12.5_b1` cascades to `debug-3.12.5` → `debug-3.12` → `debug-3` → `debug`. It does NOT cascade across variants. Versions with different variants never block each other in the cascade algorithm.

**The `parent()` function** preserves the variant field at every level. `debug-3.12.5.parent()` → `debug-3.12`, not `3.12`. The cascade terminal for a variant track is the variant-only rolling tag (`debug`), which corresponds to `Tag::VariantRolling("debug")` — not a `Version` object but a separate tag type.

**Cascade filtering:** The `decompose()` function must filter `others` to the same variant track before computing blockers. When using BTreeSet ranges, the variant field in `Ord` ensures that `debug-3.12` and `pgo-3.12` sort into separate regions, but explicit variant-track filtering is still required for the `latest_blockers` computation.

**Default variant aliasing:** When a version is pushed for the default variant, the cascade also produces **unadorned** version tags. For example, pushing `pgo.lto-3.12.5_b1` for the default variant cascades to:
1. `pgo.lto-3.12.5`, `pgo.lto-3.12`, `pgo.lto-3`, `pgo.lto` (variant track)
2. `3.12.5`, `3.12`, `3`, `latest` (default variant aliases)

### Cascade Changes

```
cascade_for_variant(version, variant, is_default, others) -> (tags, is_latest)
```

The `decompose` function is extended to produce tags within a single variant track. For the default variant, a second pass produces alias tags without the variant prefix.

```
                    ┌───────────────────────────┐
                    │ pgo.lto-3.12.5_b1         │  build-tagged (pinned)
                    └──────────┬────────────────┘
                               │ cascade
              ┌────────────────┼────────────────┐
              ▼                ▼                ▼
    ┌────────────────┐  ┌──────────────┐  ┌──────────────┐
    │ pgo.lto-3.12.5 │  │ 3.12.5       │  │ (alias)      │
    └──────┬─────────┘  └──────┬───────┘  └──────────────┘
           │ cascade           │ cascade (default only)
           ▼                   ▼
    ┌────────────────┐  ┌──────────────┐
    │ pgo.lto-3.12   │  │ 3.12         │
    └──────┬─────────┘  └──────┬───────┘
           │                   │
           ▼                   ▼
    ┌────────────────┐  ┌──────────────┐
    │ pgo.lto-3      │  │ 3            │
    └──────┬─────────┘  └──────┬───────┘
           │                   │
           ▼                   ▼
    ┌────────────────┐  ┌──────────────┐
    │ pgo.lto        │  │ latest       │
    └────────────────┘  └──────────────┘
```

### Identifier Changes

The `Identifier` struct remains unchanged — it stores the full tag string including variant prefix. Variant parsing is a concern of the `Version` type, not `Identifier`. The `Identifier` is a transport-layer type; `Version` is the semantic-layer type.

New helper on `Identifier`:

```rust
impl Identifier {
    /// Extracts a Version (with optional variant) from the tag.
    pub fn version(&self) -> Option<Version> {
        self.tag().and_then(Version::parse)
    }
}
```

### Mirror Spec Changes

```yaml
name: python
target:
  registry: ocx.sh
  repository: python

source:
  type: github_release
  owner: astral-sh
  repo: python-build-standalone
  tag_pattern: "^(?P<version>\\d+\\.\\d+\\.\\d+)\\+\\d+$"

# NEW: variant declarations
variants:
  - name: pgo.lto
    default: true
    assets:
      linux/amd64:
        - "cpython-.*-x86_64-unknown-linux-gnu-pgo\\+lto-.*\\.tar\\.zst"
      darwin/arm64:
        - "cpython-.*-aarch64-apple-darwin-pgo\\+lto-.*\\.tar\\.zst"
  - name: debug
    assets:
      linux/amd64:
        - "cpython-.*-x86_64-unknown-linux-gnu-debug-.*\\.tar\\.zst"
      darwin/arm64:
        - "cpython-.*-aarch64-apple-darwin-debug-.*\\.tar\\.zst"
  - name: freethreaded
    assets:
      linux/amd64:
        - "cpython-.*-x86_64-unknown-linux-gnu-freethreaded\\+pgo\\+lto-.*\\.tar\\.zst"

# Fields at the top level are defaults inherited by all variants
cascade: true
versions:
  min: "3.12.0"
```

**Inheritance:** Top-level fields (`source`, `cascade`, `versions`, `asset_type`, `build_timestamp`, etc.) serve as defaults inherited by all variants. Each variant can override:
- **`assets`** (required per variant) — the platform-to-regex patterns that select the right build artifacts. This is the primary differentiator between variants.
- **`metadata`** (optional) — a variant may have different environment variables or configuration.
- **`asset_type`** (optional) — if a variant uses a different archive format or strip_components.

Fields that variants **cannot** override: `name`, `target`, `source`, `cascade`, `versions`, `verify`, `concurrency`. These are package-level concerns, not variant-level. If a variant truly needs a different source or version range, it should be a separate package.

If no `variants` key exists, the spec behaves as today (single implicit variant, backward-compatible).

**Exactly one variant must be `default: true`** when variants are declared. The default variant's builds produce both prefixed and unprefixed tags.

### CLI Changes

No immediate CLI changes required. The `install`, `select`, and other commands work with full tags including variant suffixes:

```bash
ocx install python:debug-3.12    # explicit variant
ocx install python:3.12          # default variant (pgo.lto)
ocx install python:debug         # latest debug
ocx install python:freethreaded  # latest freethreaded
```

Future enhancement: `ocx index list python --variants` to show available variants.

### Index Changes

The index lists all tags including variant-prefixed ones. No structural changes to `IndexImpl`. A future `list_variants()` convenience method could parse tags to extract unique variant names, but this is not required for the initial implementation.

### File Structure

No changes to `FileStructure`, `ObjectStore`, or `InstallStore`. Variant information is encoded in the tag, which is already slugified for filesystem paths. For example:
- `debug-3.12` becomes install path `.../candidates/debug-3.12/`
- `3.12` becomes install path `.../candidates/3.12/`

## Implementation Plan

### Phase 1: Version Variant Parsing ✅
1. [x] Extend `Version` struct with `variant: Option<String>`
2. [x] Update `Version::parse()` to detect variant prefix (split at first `-` before digit)
3. [x] Update `Version::Display` to prepend variant (`debug-3.12.5`)
4. [x] Update `Version::parent()` to preserve variant
5. [x] Add `Version::variant()`, `Version::has_variant()` accessors
6. [x] Update `Version::Ord` to sort variant first (enables BTreeSet range isolation)
7. [x] Unit tests for all variant parsing edge cases

### Phase 2: Cascade Variant Awareness ✅
1. [x] Modify `decompose()` to filter `latest_blockers` and prerelease blockers to same variant track
2. [x] Update `resolve_cascade_tags()` to use variant name as terminal tag
3. [x] Extend cascade unit tests for variant scenarios (same-variant blocking, cross-variant non-blocking, mixed sets, prerelease isolation)
4. [x] Extend orchestration tests for variant-aware platform checks
5. [x] Acceptance tests: 9 variant cascade tests covering isolation, cross-variant non-interference, same-version different-variant, blocking, platform preservation, 3-variant independence

**Implementation notes (deviations from original plan):**
- `Tag::Canary` removed — "canary" is not special-cased, just a variant name like any other
- `Tag::LatestVariant` removed — bare variant names fall into `Tag::Other`; variant semantics determined at higher layer
- `Tag::Internal(InternalTag)` added — consolidates `__ocx.*` tag handling with forward-compatible `Unknown` variant
- `Version::with_variant()`, `is_same_variant_track()` not added (YAGNI) — callers use `v.variant() == other.variant()` directly
- `Version::without_variant()` added in Phase 3 for default variant alias cascade
- Default variant alias generation deferred to Phase 3 (needs mirror spec context)

**Phase 3 implementation notes:**
- `MirrorSpec.assets` changed from required to `Option<AssetPatterns>` — mutually exclusive with `variants`
- `EffectiveVariant` provides a unified interface: legacy specs produce one synthetic variant with `name: None`
- `VariantContext { name, is_default }` on `MirrorTask` carries variant info through the pipeline
- `ResolvedVersion.variant: Option<String>` enables variant-aware already-mirrored detection in `filter.rs`
- Already-mirrored check constructs variant-prefixed version for `VersionPlatformMap.has()` while min/max bounds use bare source version (avoids `Ord` issues with variant-first sorting)
- Default variant alias cascade: `push_and_cascade()` performs a second `push_cascade()` with `without_variant()` tag, producing unadorned tags (`3.12.5`, `3.12`, `3`, `latest`)
- OCI registries handle duplicate blob uploads as no-ops (content-addressed), so the second push only creates tag aliases

### Phase 3: Mirror Variant Support ✅
1. [x] Add `variants` field to `MirrorSpec` (with `name`, `default`, overrides)
2. [x] Add spec validation (exactly one default, naming rules)
3. [x] Update `execute_mirror()` orchestrator to iterate over variants
4. [x] Update `push_and_cascade()` to pass variant + default flag
5. [x] Update `MirrorTask` to carry variant context
6. [x] Add mirror spec tests

**Phase 4 implementation notes:**
- `--variants` flag on `index list` parses tags client-side via `Version::parse()`, no manifest fetching needed
- `--platforms` flag fetches manifest for a single tag (specified or `latest`), not all tags
- OCI manifest annotations deferred — variant info is fully recoverable from tag names, no consumer exists yet

### Phase 4: Discovery ✅
1. [x] Add `--variants` flag to `ocx index list` for variant discovery
2. [x] Add `--platforms` flag to `ocx index list` for single-tag platform query
3. [x] Acceptance tests for variant install, select, list workflows (5 tests)

## Validation

- [x] Existing tests pass unchanged (backward compatibility) — Phase 1+2
- [x] Cascade tests cover: same-variant blocking, cross-variant non-blocking — Phase 2
- [x] Mirror spec tests: parse with/without variants, validation (mutual exclusivity, default count, naming rules, duplicates, reserved names), effective_variants with inheritance/overrides — Phase 3
- [x] Filter tests: variant already-mirrored detection, variant-vs-default independence, different variants same version independent, min/max uses bare version — Phase 3
- [x] Version tests: `without_variant()` strips variant, noop for default — Phase 3
- [ ] Cascade tests cover: default alias generation (integration) — Phase 3 (covered by push.rs logic, not separately tested)
- [x] Acceptance tests cover: install variant, rolling tag, select, coexist, `--variants` discovery — Phase 4
- [x] `task verify` passes — Phase 1+2+3+4 (fmt, clippy, build, unit tests, acceptance tests pass; shell lint skipped due to missing shfmt package)

## Links

### Research Artifacts
- `research_package_variants.md` — Cross-ecosystem variant modeling survey (Docker, Homebrew, Nix, conda, Conan, Cargo, Python wheels, Spack, vcpkg, python-build-standalone)
- `research_oci_variants.md` — OCI spec variant support analysis and tag naming conventions

### External References
- [OCI image-spec v1.1.1 image-index.md](https://github.com/opencontainers/image-spec/blob/v1.1.1/image-index.md) — `platform.variant` is CPU-only, `platform.features` is reserved
- [OCI image-spec#1216](https://github.com/opencontainers/image-spec/issues/1216) — Platform metadata for non-image artifacts (annotation-based proposals)
- [ORAS#1538](https://github.com/oras-project/oras/issues/1538) — ORAS annotation-based platform metadata (`land.oras.artifact.platform.*`)
- [moby/moby#38715](https://github.com/moby/moby/issues/38715) — `platform.features` stalled proposal (since 2019)
- [Concourse registry-image-resource](https://github.com/concourse/registry-image-resource) — Documents the semver prerelease/variant suffix ambiguity
- [conda#6439](https://github.com/conda/conda/issues/6439) — Opaque hash build strings as UX problem
- [arxiv:2509.07728](https://arxiv.org/html/2509.07728) — Spack ASP concretization: "every variant must concretize to exactly one value"
- python-build-standalone (Astral): two-axis model — target triple (platform) × build options (variant)

### OCX Source References
- OCX cascade algebra: `crates/ocx_lib/src/package/cascade.rs`
- OCX version type: `crates/ocx_lib/src/package/version.rs`
- OCX tag type: `crates/ocx_lib/src/package/tag.rs`
- OCX mirror spec: `crates/ocx_mirror/src/spec.rs`
- OCX identifier: `crates/ocx_lib/src/oci/identifier.rs`

---

## Design Rationale: Tag Selection as UX Principle

The core UX decision in this ADR is the **1:1 correspondence between OCI tag and installed artifact**. When a user writes `ocx install python:debug-3.12`, the tag `debug-3.12` directly identifies the artifact — no metadata inspection, no solver, no manifest downloading required. This matches Docker's mental model and works with every OCI tool (crane, skopeo, regclient).

This simplicity has a cost: variants must be pre-declared named values, not arbitrary feature combinations. A user cannot say "give me Python with PGO but without LTO" — they can only select from the curated set (`pgo.lto`, `pgo`, `debug`, `freethreaded`). This is intentional. For pre-built binary distribution, the publisher decides which variants to build; the consumer selects from what's available. Spack and Conan offer richer variant models because they can build from source to satisfy arbitrary constraints. OCX distributes pre-built artifacts and therefore matches the python-build-standalone model: a finite set of named flavors.

For OCX's target audience (CI/CD, Bazel, GitHub Actions), deterministic tag identifiers are the natural interface. These consumers want reproducible builds, not variant negotiation. The tag IS the contract.

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-22 | mherwig | Initial draft |
| 2026-03-22 | mherwig | Added cross-ecosystem research, OCI spec analysis, rejected options 4-5, variant-only tag handling, `variant-latest` rejection, `Tag::VariantRolling`, Ord/cascade/parent refinements, mirror spec override boundaries, design rationale |
| 2026-03-22 | mherwig | Phase 3: Mirror variant support — `VariantSpec`, `EffectiveVariant`, variant-aware filter, default alias cascade, `Version::without_variant()` |
| 2026-03-22 | mherwig | Phase 4: `--variants` and `--platforms` flags on `index list`, acceptance tests, user guide Variants section. Annotations deferred (no consumer). |
