# ADR: OCI Artifact Enrichment — Signatures, SBOMs, and Descriptive Metadata

## Metadata

**Status:** Proposed
**Date:** 2026-03-12
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** oci, architecture, supply-chain
**Supersedes:** N/A
**Superseded By:** N/A

## Context

Today, an OCX package stored in an OCI registry consists of exactly two blobs referenced by an `OciImageManifest`:

1. **Config blob** — `application/vnd.sh.ocx.package.v1+json` — the package metadata (env vars, strip_components, version).
2. **Layer blob** — `application/vnd.oci.image.layer.v1.tar+{gzip,xz}` — the compressed binary archive.

These are wrapped in a per-platform `ImageManifest` (with `artifactType: application/vnd.sh.ocx.package.v1`), and multi-platform tags use an `ImageIndex` pointing to multiple such manifests.

This is sufficient for binary distribution, but modern supply-chain and discoverability requirements demand richer metadata:

- **Signatures** — Cryptographic proof that a package was published by a trusted party. Required by enterprise security policies and emerging regulations (EU Cyber Resilience Act, US EO 14028).
- **SBOMs** — Software Bill of Materials (SPDX, CycloneDX) listing dependencies, licenses, and provenance. Increasingly mandated for government procurement and enterprise compliance.
- **Descriptive metadata** — README content, logos, and extended descriptions that registries can surface in their UIs. Today, OCX packages appear as opaque blobs in Docker Hub/GHCR package pages.

The OCI Image Spec v1.1 (released February 2024) introduced first-class support for artifact relationships via the `subject` field and the Referrers API, providing a standards-based mechanism for attaching auxiliary data to existing artifacts.

## Decision Drivers

- OCX positions itself as enterprise-ready; supply-chain security (signatures, SBOMs) is table stakes.
- The OCI v1.1 `subject` + Referrers API is the industry-standard approach used by cosign, Notation, ORAS, and Syft.
- Registry support is broad (ECR, ACR, Harbor, Quay, Zot, registry:2). GHCR lacks the Referrers API but this is accepted as a temporary limitation — referrer features degrade gracefully.
- OCX already sets `artifactType` on manifests, so the foundation is in place.
- Any solution must work with the existing content-addressed storage model and not break offline/snapshot workflows.

## Considered Options

### Option 1: Hybrid — Dedicated Tag for Description + Referrers for Supply-Chain (Recommended)

**Description:** Use three complementary mechanisms, each matched to the artifact's semantics:

1. **OCI annotations** on existing manifests — lightweight metadata (title, license, URLs)
2. **Dedicated `_desc` tag** — repository-level description content (README, logo)
3. **OCI v1.1 referrer artifacts** — per-build supply-chain data (signatures, SBOMs)

This split follows two principles:

- **Match the mechanism to the subject scope.** Descriptions are per-repository (the README for cmake:3.28 and cmake:3.29 is usually identical). Signatures and SBOMs are per-platform-build (a linux/amd64 binary has different deps than darwin/arm64). A dedicated tag is repository-scoped; a referrer's `subject` is digest-scoped.
- **Match the mechanism to the registry requirement.** The `_desc` tag and annotations work on every registry — no Referrers API needed. Signatures and SBOMs use the Referrers API, which is available on most registries (ECR, ACR, Harbor, Quay, Zot, registry:2).

**Subject targeting strategy:**

The OCI `subject` field is always a **digest**, not a tag. This has important implications for what each artifact type should target:

| Artifact | `subject` target | Scope | Why |
|----------|-----------------|-------|-----|
| **Signature** | Per-platform ImageManifest digest | Per-build | Cryptographic binding — the signature covers a specific binary. Signing the ImageIndex doesn't prove the individual platform manifests weren't swapped. |
| **SBOM** | Per-platform ImageManifest digest | Per-build | SBOMs differ per platform — different linked libraries, different build dependencies. |
| **Description** | N/A (dedicated tag, no `subject`) | Per-repository | Platform-independent, version-independent. Avoids orphaned referrers when tags are re-pointed. |

**Registry storage model:**

```
Repository: cmake

Tag: _desc → ImageManifest (description — standalone, no subject)
  artifactType: application/vnd.sh.ocx.description.v1
  config: <empty descriptor>
  layers:
    ├─ readme.md   (application/markdown)
    └─ logo.png    (image/png)

Tag: 3.28 → ImageIndex (multi-platform)
  annotations: { title, description, license, source, vendor }
  └─ Platform linux/amd64 → ImageManifest (sha256:aaa)
       ├─ Config: metadata.json
       ├─ Layer: cmake-3.28.tar.xz
       │
       ├─ Referrer: signature
       │    artifactType: application/vnd.sh.ocx.signature.v1
       │    subject: sha256:aaa
       │    layers: [signature payload]
       │
       └─ Referrer: SBOM
            artifactType: application/vnd.sh.ocx.sbom.v1
            subject: sha256:aaa
            layers: [sbom.spdx.json]

Tag: latest → ImageIndex (same as 3.28 or newer)
  annotations: { title, description, license, source, vendor }
```

**Description discovery (no Referrers API needed):**

```
1. Client wants README for cmake
2. Pull manifest at cmake:_desc → ImageManifest
3. Pull layers → readme.md, logo.png
```

This works on every OCI registry. The `_desc` tag is pushed once and updated independently of package versions.

**Supply-chain artifact discovery (Referrers API):**

```
GET /v2/cmake/referrers/sha256:aaa?artifactType=application/vnd.sh.ocx.signature.v1

→ 200 OK
{
  "mediaType": "application/vnd.oci.image.index.v1+json",
  "manifests": [{
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "digest": "sha256:def...",
    "artifactType": "application/vnd.sh.ocx.signature.v1"
  }]
}
```

**No tag fallback for referrers.** Registries that do not implement the OCI v1.1 Referrers API (notably GHCR as of early 2026) will not support signatures or SBOMs. This is an accepted limitation:

- GHCR is the only major registry without the Referrers API; an [active community discussion](https://github.com/orgs/community/discussions/163029) suggests support is coming.
- Supply-chain features are enrichment, not core functionality. OCX's primary value (install, version-switch, env) works on every registry regardless.
- Description and annotations work on every registry — the most user-visible enrichment (README, logo, title) is universally available.
- The tag fallback scheme (`sha256-{digest}` tags) adds significant complexity: concurrent writers competing to update a shared index tag, race conditions, cleanup, and a permanent second code path. This cost is not justified for a transitional compatibility shim.
- OCX detects support at runtime — if the Referrers API returns 404/405, a clear message is surfaced: "this registry does not support OCI referrers."
- When GHCR adds support, OCX users get supply-chain features automatically with no code changes.

**Pros:**
- Description works on **every** registry — no Referrers API required for the most user-visible enrichment
- Signatures/SBOMs use standards-based OCI v1.1 — interoperable with cosign, Notation, ORAS, Syft, Grype
- Decoupled lifecycles — description updated independently of versions, supply-chain data attached per-build
- No orphaned referrers — `_desc` tag is stable, not affected by version re-tagging
- Single code path — no tag fallback complexity for referrers
- `oras discover` and `cosign verify` work against OCX packages out of the box

**Cons:**
- One reserved tag (`_desc`) per repository — minor tag namespace cost
- Supply-chain features unavailable on registries without Referrers API (GHCR) until they add support
- Referrer graphs can grow unbounded without garbage collection policy
- Two discovery mechanisms (tag pull for description, Referrers API for supply-chain) — but each is simple individually

### Option 2: Embed Everything in the Package Manifest

**Description:** Add signature, SBOM, and readme as additional layers in the existing `ImageManifest`, distinguished by layer media type.

```json
{
  "artifactType": "application/vnd.sh.ocx.package.v1",
  "config": { "mediaType": "application/vnd.sh.ocx.package.v1+json", ... },
  "layers": [
    { "mediaType": "application/vnd.oci.image.layer.v1.tar+xz", ... },
    { "mediaType": "application/vnd.sh.ocx.signature.v1+pem", ... },
    { "mediaType": "application/spdx+json", ... },
    { "mediaType": "application/markdown", ... }
  ]
}
```

**Pros:**
- Simple — single manifest, single fetch
- No referrer API dependency
- Works on every registry

**Cons:**
- **Coupling** — cannot add a signature after publication without re-pushing the entire manifest (new digest = breaks all existing references)
- **Violation of OCI conventions** — the ecosystem (cosign, Notation, Syft) expects referrer-based attachment; this would be non-interoperable
- **Bloated pulls** — clients that only want the binary must download or skip-range past extra layers
- **Backwards-incompatible** — existing OCX clients validate `layers.len() == 1`; changing this breaks all deployed versions
- **No graph semantics** — cannot sign an SBOM independently

### Option 3: Annotations-Only (Descriptive Metadata)

**Description:** Use OCI annotations on the existing manifests/index for descriptive data, and delegate signatures/SBOMs to external tooling (cosign, Notation).

```json
{
  "annotations": {
    "org.opencontainers.image.title": "CMake",
    "org.opencontainers.image.description": "Cross-platform build system generator",
    "org.opencontainers.image.version": "3.28.1",
    "org.opencontainers.image.licenses": "BSD-3-Clause",
    "org.opencontainers.image.source": "https://github.com/Kitware/CMake",
    "org.opencontainers.image.documentation": "https://cmake.org/documentation/",
    "org.opencontainers.image.vendor": "Kitware",
    "sh.ocx.readme-url": "https://github.com/Kitware/CMake/blob/master/README.rst",
    "sh.ocx.logo-url": "https://cmake.org/wp-content/uploads/2023/08/CMake-Mark-1.svg"
  }
}
```

**Pros:**
- Zero infrastructure change — annotations are already supported
- Registries (Docker Hub, GHCR) surface standard OCI annotations in their UIs today
- No additional blobs or manifests
- Compatible with all registries

**Cons:**
- Annotations are strings only — cannot embed binary data (logos) or large content (full README)
- URLs for readme/logo require external hosting and create a dependency
- Does not solve signatures or SBOMs
- Annotation values are limited in size (no formal limit in spec, but registries impose practical limits)

## Decision

**Use Option 1 (Hybrid) — three complementary mechanisms matched to artifact semantics:**

| Data | Mechanism | Scope | Requires Referrers API |
|------|-----------|-------|----------------------|
| Title, description, version, license, source URL, vendor | OCI annotations on ImageManifest/ImageIndex | Per-version | **No** |
| README + logo | Dedicated `_desc` tag with multi-layer manifest | Per-repository | **No** |
| Signature | Referrer artifact (`subject` → per-platform ImageManifest) | Per-build | **Yes** |
| SBOM | Referrer artifact (`subject` → per-platform ImageManifest) | Per-build | **Yes** |

## Proposed Media Types

### OCX Artifact Types (for `artifactType` field)

| Artifact | `artifactType` | Storage |
|----------|----------------|---------|
| Package (existing) | `application/vnd.sh.ocx.package.v1` | Tagged manifest |
| Description (README + logo) | `application/vnd.sh.ocx.description.v1` | `_desc` tag |
| Signature | `application/vnd.sh.ocx.signature.v1` | Referrer |
| SBOM | `application/vnd.sh.ocx.sbom.v1` | Referrer |

### Layer Media Types

| Content | Layer `mediaType` |
|---------|-------------------|
| Binary archive (existing) | `application/vnd.oci.image.layer.v1.tar+{gzip,xz}` |
| SPDX SBOM | `application/spdx+json` |
| CycloneDX SBOM | `application/vnd.cyclonedx+json` |
| README | `application/markdown` |
| Logo (PNG) | `image/png` |
| Logo (SVG) | `image/svg+xml` |
| Signature payload | TBD — depends on signing scheme (cosign, Notation, or custom) |

### OCI Annotations (on ImageManifest and ImageIndex)

| Annotation Key | Example Value |
|----------------|---------------|
| `org.opencontainers.image.title` | `CMake` |
| `org.opencontainers.image.description` | `Cross-platform build system generator` |
| `org.opencontainers.image.version` | `3.28.1` |
| `org.opencontainers.image.licenses` | `BSD-3-Clause` |
| `org.opencontainers.image.source` | `https://github.com/Kitware/CMake` |
| `org.opencontainers.image.documentation` | `https://cmake.org/documentation/` |
| `org.opencontainers.image.vendor` | `Kitware` |
| `org.opencontainers.image.created` | `2026-03-12T10:30:00Z` |

## Implementation Phases

### Phase 1: Annotations (Low Effort, Immediate Value)

Add OCI annotation support to `ocx package push`. Works on every registry.

1. Extend `metadata.json` schema with optional `annotations` field (or accept a `--annotation KEY=VALUE` CLI flag).
2. Set `org.opencontainers.image.*` annotations on both the per-platform `ImageManifest` and the multi-platform `ImageIndex`.
3. Display annotations in `ocx index list --with-platforms` and `ocx info` output.

**Impact:** Package pages on Docker Hub/GHCR immediately show title, description, and links. Zero registry-side requirements.

### Phase 2: Description via `_desc` Tag (No Referrers API Required)

Repository-level README and logo. Works on every registry.

1. **Client layer** — Add `push_description(identifier, layers)` to `oci::Client`. Constructs an ImageManifest with `artifactType: application/vnd.sh.ocx.description.v1`, empty config, and layers for readme/logo. Pushes to `_desc` tag.
2. **CLI publish** — `ocx package describe <repo> --readme README.md --logo logo.png` pushes/overwrites the `_desc` tag.
3. **CLI consume** — `ocx package info <repo>` pulls the `_desc` manifest, fetches layers, displays README content.
4. **Index integration** — `ocx index catalog --with-descriptions` shows which repos have a `_desc` tag.

### Phase 3: Referrer Infrastructure (Requires Referrers API)

Build the OCI v1.1 referrer plumbing for supply-chain artifacts.

1. **Client layer** — Add `push_referrer(subject_digest, artifact_type, layers)` and `list_referrers(digest, artifact_type_filter)` to `oci::Client`.
2. **Registry detection** — Probe for Referrers API support; surface clear message when unsupported ("this registry does not support OCI referrers; signatures and SBOMs are unavailable").
3. **Local storage** — Extend the index store to cache referrer metadata (`$OCX_HOME/index/{registry}/referrers/{digest}.json`).
4. **CLI** — Add `ocx artifact attach` and `ocx artifact list` commands.

### Phase 4: SBOM Support

1. `ocx package push --sbom sbom.spdx.json` — pushes an SBOM referrer with `subject` pointing to the per-platform ImageManifest.
2. `ocx artifact list <pkg>` — shows attached SBOMs per platform.
3. `ocx artifact pull <pkg> --type sbom` — fetches the SBOM content.
4. Interop: verify that `cosign verify-attestation` and `grype` can consume OCX-published SBOMs.

### Phase 5: Signature Support

1. Design the signing scheme (cosign-compatible vs custom).
2. `ocx package sign <pkg>` — signs a per-platform ImageManifest and pushes a signature referrer.
3. `ocx package verify <pkg>` — verifies signatures via referrer discovery.
4. Consider integration with Sigstore (keyless signing via OIDC) for open-source packages.

## Impact on Existing Architecture

### OCI Client (`oci::Client`)

New methods needed:
- `push_description(identifier, layers)` — push a description manifest to the `_desc` tag (no `subject`, no Referrers API)
- `pull_description(identifier)` — pull manifest + layers from `_desc` tag
- `push_referrer(subject_digest, artifact_type, layers)` — push a manifest with `subject` field (Phase 3)
- `list_referrers(digest, artifact_type_filter)` → `Vec<OciDescriptor>` — query Referrers API (Phase 3)
- `pull_referrer(digest)` — fetch a specific referrer manifest and its layers (Phase 3)

The `OciTransport` trait needs corresponding low-level methods. The `NativeTransport` implementation must handle:
- Referrers API endpoint (`/v2/{name}/referrers/{digest}`) — Phase 3
- Registry capability detection (404/405 → unsupported, surface user-facing message) — Phase 3

### `oci-client` Patch (`external/rust-oci-client`)

The patched `oci-client` already has `OciImageManifest` with `subject: Option<OciDescriptor>` — this field exists but is unused by OCX today. The `push_manifest` method should already handle the `subject` field correctly.

Additions needed:
- Helper to construct the empty config descriptor (`application/vnd.oci.empty.v1+json`, digest of `{}`)
- Referrers API endpoint call (new method) — Phase 3

### File Structure

New paths under `$OCX_HOME/index/`:
```
index/{registry}/referrers/{digest}.json    # cached referrer index
```

New paths under `$OCX_HOME/objects/` (if referrer content is cached locally):
```
objects/{registry}/{repo}/{referrer_digest}/
  ├─ content/        # e.g., README.md, sbom.json
  └─ metadata.json   # referrer artifact metadata
```

### Metadata Schema

The existing `metadata.json` (Bundle struct) gains an optional `annotations` map:

```rust
pub struct Bundle {
    pub version: Version,
    pub strip_components: Option<u8>,
    pub env: Env,
    pub annotations: Option<BTreeMap<String, String>>,  // NEW
}
```

### CLI Commands

New commands:
- `ocx package describe <repo> --readme <file> --logo <file>` — push/update `_desc` tag (Phase 2)
- `ocx package info <repo>` — pull and display description from `_desc` tag (Phase 2)
- `ocx artifact attach <pkg> --type <artifact_type> <file>...` — attach referrer artifacts (Phase 3)
- `ocx artifact list <pkg>` — list referrers (tree view, like `oras discover`) (Phase 3)
- `ocx artifact pull <pkg> --type <artifact_type>` — fetch specific referrer content (Phase 3)

Extended existing commands:
- `ocx package push --sbom <file>` — convenience flag for SBOM attachment (Phase 4)
- `ocx package push --annotation KEY=VALUE` — set annotations (Phase 1)
- `ocx index catalog --with-descriptions` — show which repos have a `_desc` tag (Phase 2)

## Registry Compatibility Matrix

| Registry | Annotations | Description (`_desc` tag) | Referrers API (signatures, SBOMs) |
|----------|-------------|---------------------------|-----------------------------------|
| Docker Hub | Yes | Yes | Limited |
| GHCR | Yes | Yes | **No** |
| Amazon ECR | Yes | Yes | Yes |
| Azure ACR | Yes | Yes | Yes |
| Google AR | Yes | Yes | Yes |
| Harbor 2.x | Yes | Yes | Yes |
| Red Hat Quay | Yes | Yes | Yes |
| Zot | Yes | Yes | Yes |
| registry:2 | Yes | Yes | Yes (v2.7+) |

**Accepted limitation:** GHCR and Docker Hub users get annotations and descriptions (Phases 1-2) but not referrer-based supply-chain features (signatures, SBOMs) until those registries implement the Referrers API. Core package management (install, version-switch, env) is unaffected. OCX detects unsupported registries at runtime and surfaces a clear message.

## Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| GHCR delays Referrers API indefinitely | Signatures/SBOMs unavailable for GHCR-hosted packages | Annotations and descriptions still work; core functionality unaffected; tag fallback can be added later if needed |
| `_desc` tag collision with user tags | Publisher accidentally overwrites description | `_` prefix is uncommon for version tags; document reserved tag convention; validate on push |
| Referrer graph grows unbounded | Registry storage costs, slow discovery | Implement `artifactType` filtering; consider GC policy for old referrers |
| Signing scheme fragmentation (cosign vs Notation vs custom) | Ecosystem confusion | Start with cosign compatibility (largest installed base); add Notation later |
| `oci-client` patch divergence | Upstream changes conflict with referrer additions | Contribute referrer support upstream; minimize patch surface |
| Offline mode complexity | Referrer data not available offline | Cache referrer index in local index store during `ocx index update`; `_desc` can be cached in local index |

## References

- [OCI Image Spec v1.1 — Manifest](https://github.com/opencontainers/image-spec/blob/main/manifest.md)
- [OCI Image Spec v1.1 — Annotations](https://github.com/opencontainers/image-spec/blob/main/annotations.md)
- [OCI Distribution Spec v1.1 — Referrers API](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#listing-referrers)
- [ORAS — Artifact Concepts](https://oras.land/docs/concepts/artifact/)
- [ORAS — Reference Types](https://oras.land/docs/concepts/reftypes/)
- [Cosign Signature Spec](https://github.com/sigstore/cosign/blob/main/specs/SIGNATURE_SPEC.md)
- [Chainguard — Building towards OCI v1.1 support in cosign](https://www.chainguard.dev/unchained/building-towards-oci-v1-1-support-in-cosign)
- [Chainguard — Intro to OCI Reference Types](https://www.chainguard.dev/unchained/intro-to-oci-reference-types)
- [AWS ECR OCI 1.1 Support](https://aws.amazon.com/blogs/opensource/diving-into-oci-image-and-distribution-1-1-support-in-amazon-ecr/)
- [Red Hat Quay Referrers API](https://www.redhat.com/en/blog/announcing-open-container-initiativereferrers-api-quayio-step-towards-enhanced-security-and-compliance)
- [ORAS Compatible Registries](https://oras.land/docs/compatible_oci_registries/)
