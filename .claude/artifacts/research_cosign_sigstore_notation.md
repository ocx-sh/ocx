# Research: Cosign / Sigstore / Notation (Tech Axis)

**Date:** 2026-04-19
**Scope:** Phase 2 research for OCI referrers discovery (ocx verify / ocx sbom).
**Axis:** Technology / libraries.

## TL;DR

- **Ship cosign keyless first.** The `sigstore` Rust crate (v0.13.0, Oct 2025) provides keyless verification with offline-capable TUF trust root via `sigstore-trust-root`. It is pre-1.0 with unstable API, but it is the only production-viable Rust option and is actively maintained by the Sigstore org.
- **Notation/Notary v2 is Go-only.** There is no Rust crate for Notation signature verification as of April 2026. Rust→Go FFI cost is prohibitive for a lightweight CLI. Defer Notation to Phase 3 at the earliest.
- **SBOM parsing:** Use `cyclonedx-bom` v0.8.1 (official CycloneDX org, March 2025) for CycloneDX 1.3–1.5 JSON parsing. Treat `spdx-rs` as a stretch goal — it last released September 2023, supports only SPDX 2.x, and has 7 GitHub stars. `ocx sbom` v1 should target CycloneDX JSON only.
- **SPDX 3.0 is not ready for tooling.** SPDX 2.3 remains the dominant deployed version; most tools have not yet shipped SPDX 3.0 parsers. OCX should accept both 2.3 and 3.0 SPDX on input but normalise to a display-level struct, not a full round-trip parser.
- **Bundle format detection matters.** Cosign is mid-migration from legacy `application/vnd.dev.cosign.artifact.sig.v1+json` (stored as OCI tag) to the new `application/vnd.dev.sigstore.bundle.v0.3+json` (stored as OCI referrer). OCX must detect and handle both during a multi-year transition window.
- **Cosign is dominant in the Kubernetes/CNCF ecosystem** (5.8k GitHub stars, PyPI and Homebrew adopting Sigstore). Notation serves a narrower enterprise-PKI niche. OCX's target users (CI/CD, GitHub Actions) are overwhelmingly in the cosign camp.

---

## 1. Cosign via `sigstore` Crate

### Library Identity

| Field | Value |
|-------|-------|
| Crate name | `sigstore` |
| Current version | 0.13.0 |
| Released | October 16, 2025 |
| License | Apache-2.0 |
| Maintainer | Sigstore org (official) |
| Repository | https://github.com/sigstore/sigstore-rs |
| Docs | https://docs.rs/sigstore/latest/sigstore/ |
| Downloads (monthly avg) | ~49,000/month |
| Reverse deps | 16 crates (12 direct) |
| Maturity statement | "under active development and will not be considered stable until the 1.0 release" |
| Documentation coverage | 36.97% — sparse, examples are primary API guide |

Companion crate `sigstore-trust-root` v0.6.4 handles TUF root material parsing independently of the main crate.

### API Surface for Verification

The `sigstore::cosign` module exposes:

```
ClientBuilder → Client
Client::verify_oci_reference(auth, image, constraints) → Vec<SignatureLayer>
Client::verify_blob(input, constraints) → SignatureLayer
CosignCapabilities trait — testable mock surface
SignatureLayer — verified signature data
Constraint trait — custom business logic hook
```

`sigstore::cosign::bundle::SignedArtifactBundle` handles Rekor-produced bundle files. The `payload` submodule includes DSSE envelope types **but attestation verification is explicitly not yet implemented** — the README states: "The crate does not handle verification of attestations yet."

Three verification modes are supported:
1. Key-based verification (ECDSA/RSA public key)
2. Keyless verification (Fulcio OIDC certificate chain)
3. Rekor transparency log bundle verification

### TUF Trust Root / Offline Mode

`sigstore-trust-root` v0.6.4 provides:
- `TrustedRoot::production()` — embedded production trust material, **no network required**
- `TrustedRoot::staging()` — embedded staging trust material
- `TrustedRoot::from_tuf()` — async fetch from Sigstore's TUF repository (requires `tuf` feature flag)

This satisfies OCX's offline-first requirement: the embedded production root allows verification without network access, at the cost of not picking up root rotations until the crate is updated. For `ocx verify` in offline mode, `TrustedRoot::production()` is the correct path.

### Bundle Format: v0.3 vs Legacy

Two formats exist in the wild simultaneously:

| Format | MediaType | Storage mechanism | Cosign version |
|--------|-----------|-------------------|----------------|
| **Legacy** | `application/vnd.dev.cosign.artifact.sig.v1+json` | OCI tag `sha256-{digest}.sig` | cosign < 2.0 |
| **New bundle** | `application/vnd.dev.sigstore.bundle.v0.3+json` | OCI referrer (OCI v1.1 `subject`) | cosign >= 2.6 |

The new bundle format consolidates: X.509 certificate, Rekor log entry, RFC 3161 timestamp, and message signature/DSSE envelope into a single JSON artifact. Cosign v3.0.6 (April 2026, current) defaults to producing and consuming bundles aligned with all Sigstore SDKs.

**OCX implication:** `ocx verify` must detect both mediaTypes when querying referrers. The legacy format stores signatures as OCI tags (not referrers), which means:
- Referrers API query (`GET /v2/{name}/referrers/{digest}?artifactType=application/vnd.sh.ocx.signature.v1`) finds only new-format signatures
- Legacy format requires tag-based lookup (`sha256-{digest}.sig`) as a fallback probe
- This complexity is a strong argument for using OCX's own `application/vnd.sh.ocx.signature.v1` wrapper that internally normalises both formats

### DSSE Attestation Support

The `sigstore-rs` crate includes DSSE envelope types in `sigstore::cosign::payload` but attestation verification (SLSA provenance, SBOM predicates) is **not yet implemented**. The README explicitly states this gap. DSSE attestation support is in the Go implementation (`sigstore-go`) and Python SDK but has not landed in the Rust crate as of v0.13.0.

**Recommendation for OCX Phase 1:** Do not implement attestation verification. `ocx verify` should verify message signatures (the common case) only. File an internal issue for attestation follow-up.

### Pre-1.0 API Risk

The crate has had 10 breaking changes across 19 releases. At v0.13.0, breaking changes between minor versions are expected. OCX should pin to an exact minor version (`sigstore = "=0.13"`) and plan for a deliberate upgrade cycle. The alternative — vendoring the verification logic inline — is not justified given the cryptographic complexity.

---

## 2. Notation (CNCF Notary)

### Library Availability Assessment

As of April 2026, there is **no Rust crate for Notation/Notary v2 signature verification** on crates.io or in any official CNCF repository. The Notary Project's entire implementation stack is Go:

| Repository | Language | Status |
|------------|----------|--------|
| `notaryproject/notation` | Go | v1.3.0, February 2025 |
| `notaryproject/notation-go` | Go | v1.3.0, February 2025 — library for Go integration |
| `notaryproject/notation-core-go` | Go | v1.2.0, February 2025 — core spec implementation |

The Notary Project specification is open — any language can implement it — but no community Rust implementation exists.

### Rust→Go FFI Cost Analysis

Implementing Notation verification in OCX via Go FFI (`cgo` through Rust FFI boundary) is **not a viable path**:

1. Binary size: linking the Go runtime adds ~8–10 MB minimum to the OCX binary, conflicting with OCX's "standalone binary" principle
2. Build complexity: `cgo` requires a C compiler at build time, breaking `cross`/`cargo-dist` cross-compilation for musl targets
3. Maintenance cost: a Go runtime bundled inside a Rust binary creates two GC heaps, two memory models, and opaque crash modes
4. Async incompatibility: Go's goroutine scheduler and Tokio's executor cannot trivially coexist

A pure-Rust implementation of Notation verification is theoretically possible (the signature envelope is JWS, parseable with `jsonwebtoken`), but would require implementing the full Notation trust store model, certificate chain validation, and revocation checks — a multi-week effort with no upstream maintenance.

### Adoption Signal

The CNCF Notary Project completed a second security audit in January 2025 and is on the Incubating track. Notation v1.3.0 ships February 2025. However:
- GitHub stars for `notaryproject/notation`: ~1,200 (vs cosign's 5,800)
- Primary adopters are enterprise registry vendors (ACR, ECR notation plugins) and regulated industries requiring traditional PKI trust models
- Kubernetes ecosystem tooling (Kyverno, Flux, Policy Controller) is almost entirely cosign-based

### Verdict

**Defer Notation to a future phase.** The absence of a Rust library, the FFI cost, and the substantially smaller community footprint in OCX's target user base (CI/CD, GitHub Actions, cloud-native) make Notation a low-ROI investment for the initial `ocx verify` release.

---

## 3. SBOM Parsing (SPDX + CycloneDX)

### CycloneDX Parsing: `cyclonedx-bom`

| Field | Value |
|-------|-------|
| Crate name | `cyclonedx-bom` |
| Current version | 0.8.1 |
| Released | March 19, 2025 |
| Repository | https://github.com/CycloneDX/cyclonedx-rust-cargo |
| Maintainer | CycloneDX org (official) |
| Supported spec versions | CycloneDX 1.3, 1.4, 1.5 |
| Supported formats | JSON and XML |

The `cyclonedx-bom` crate is the **library half** of `cyclonedx-rust-cargo` (the generation tool used in the SBOM ADR). This is important: OCX already depends on the `cargo-cyclonedx` generation tool, and the parsing library is from the same codebase — reducing dependency surface.

Key parsing API:
```rust
Bom::parse_from_json_v1_5(reader) -> Result<Bom>
Bom::parse_from_json_v1_4(reader) -> Result<Bom>
Bom::validate()                   -> ValidationResult
```

The `Bom` struct exposes `components`, `metadata`, `dependencies`, `vulnerabilities`, and `compositions` fields. For `ocx sbom` display, the relevant fields are:
- `bom.metadata.component` — the root component (the package being described)
- `bom.components` — list of dependency components with name, version, purl, licenses
- `bom.metadata.timestamp` — when the SBOM was generated

**Note on CycloneDX 1.6:** The spec released in March 2024 and is not yet supported by `cyclonedx-bom` v0.8.1. This is acceptable for v1 — `ocx sbom` will encounter 1.3–1.5 BOMs from `cargo-cyclonedx` generation.

### SPDX Parsing: `spdx-rs`

| Field | Value |
|-------|-------|
| Crate name | `spdx-rs` |
| Current version | 0.5.5 |
| Released | September 19, 2023 — **30 months without a release** |
| GitHub stars | 7 |
| Maintainer | doubleopen-project (community) |
| SPDX versions | 2.x only (no SPDX 3.0 support) |

The `spdx-rs` crate is effectively unmaintained relative to the pace of the SPDX specification. SPDX 3.0 was finalised in April 2024 and the crate has had no update since September 2023. The 7-star count indicates minimal community adoption.

**Alternative approach for SPDX:** The `serde_spdx` crate (https://docs.rs/serde-spdx) provides a `serde`-compatible SPDX type system. For `ocx sbom` v1, it is sufficient to deserialize SPDX JSON using `serde_json` into a minimal struct that extracts:
- `SPDXID`, `spdxVersion`, `name`, `dataLicense`
- `packages[]` — each with `name`, `versionInfo`, `licenseConcluded`, `SPDXID`
- `relationships[]` — for dependency graph display

This avoids a heavy dependency on a stale crate and is resilient to SPDX 2.3 vs 3.0 schema differences (the display fields are stable across versions).

### SPDX 3.0 Adoption Status

SPDX 3.0 was finalized in April 2024. As of April 2026:
- Most tooling (Syft, cargo-cyclonedx, GitHub Dependency Graph) still emits SPDX 2.3
- SPDX 3.0 uses a JSON-LD graph model incompatible with the 2.x document model — parsers require a rewrite, not an update
- No major Rust SPDX 3.0 parser exists
- Interlynk and other compliance platforms are still implementing SPDX 3.0 support

**Verdict:** OCX should accept SPDX 2.3 input only for v1. Add SPDX 3.0 support only when `cargo-cyclonedx` or another upstream tool actually emits 3.0.

### What `ocx sbom` Should Display (v1)

Based on competitive analysis of `trivy image --sbom`, `cosign download sbom`, and `oras discover`, the minimum valuable display for `ocx sbom <ref>` is:

```
Package:   cmake:3.28 (linux/amd64)
SBOM:      CycloneDX 1.5 JSON
Generated: 2026-03-12T10:30:00Z
Tool:      cargo-cyclonedx 0.5.9

Components (47):
  cmake 3.28.1              (MIT)
  openssl-sys 0.9.102       (MIT OR Apache-2.0)
  tokio 1.43.0              (MIT)
  ... [truncated, use --all to expand]

Summary: 47 components, 3 licenses (MIT, Apache-2.0, OpenSSL)
```

For JSON output (`--format json`): emit the raw SBOM blob or a normalised subset — the orchestrator (GitHub Actions, Bazel) decides how to consume it.

**CVE highlighting in `ocx sbom` v1:** Do not add vulnerability scanning. That requires a CVE database (Grype/OSV integration) and is out of scope. `ocx sbom` v1 is display + extraction, not scanning.

---

## 4. Initial Signature Format Recommendation

### Question from Issue #24

"Which signature formats ship first — cosign keyless, notation, both?"

### Recommendation: Cosign Keyless Only, with OCX Wrapper Media Type

**Ship only cosign keyless verification in Phase 2.** The decision logic:

**1. Rust library availability is the binding constraint.** There is no Rust Notation library. Building cosign verification using `sigstore` v0.13.0 is weeks of work; building Notation verification from scratch is months.

**2. The OCX ADR already uses `application/vnd.sh.ocx.signature.v1` as the referrer `artifactType`.** This is correct: OCX wraps the inner signature format, allowing the inner format to evolve. The referrer is discovered by OCX's own `artifactType`, and OCX then inspects the layer `mediaType` to determine how to verify it. This means OCX is not locked to a single signature format at the OCI layer.

**3. Cosign is dominant in OCX's target user base.** The Kubernetes policy controllers (Kyverno, Flux ImagePolicy, Sigstore Policy Controller), GitHub Actions supply chain hardening workflows, and the broader CNCF toolchain all standardise on cosign. Users of OCX in CI/CD are already using cosign for their container images.

**4. Cosign v3 (April 2026) defaults to the OCI v1.1 referrer-based bundle format**, which aligns perfectly with the OCX referrer architecture decided in the enrichment ADR.

**5. Notation's enterprise PKI niche** is real but not OCX's v1 audience. Enterprise users who need Notation can verify separately via `notation verify` while OCX reads the SBOM referrer directly. This is not a gap — it is a staged rollout.

### Dual-Format Detection Plan (for forward compatibility)

When `ocx verify` inspects a signature referrer's layer, it should check the layer `mediaType`:

| Layer `mediaType` | Action |
|-------------------|--------|
| `application/vnd.dev.sigstore.bundle.v0.3+json` | Parse as Sigstore bundle, verify via `sigstore` crate |
| `application/vnd.dev.cosign.artifact.sig.v1+json` | Legacy cosign format (tag-based), fetch via tag probe |
| `application/jose+json` (JWS envelope) | Future Notation support, display "unsupported format, use `notation verify`" |
| Unknown | Display `mediaType` and raw size, do not attempt verification |

This design is forwards-compatible: Notation support can be added later by filling in the `application/jose+json` branch without touching the referrer discovery layer.

---

## 5. Adoption Signals (Competitive Scan)

### Cosign / Sigstore Ecosystem

| Signal | Data |
|--------|------|
| `sigstore/cosign` GitHub stars | 5,800 (April 2026) |
| Cosign latest version | v3.0.6, April 6, 2026 |
| Sigstore CNCF status | Incubating → Graduated track |
| `sigstore-rs` downloads | ~49,000/month |
| Kubernetes image signing | All official k8s images signed with cosign since 1.24 |
| PyPI adoption | Python packages on PyPI now support Sigstore signatures |
| Homebrew adoption | Homebrew bottles being signed with Sigstore (2024 initiative) |
| GitHub Actions | `actions/attest-sbom` and `sigstore/cosign-installer` are first-class |
| Syft | Creates in-toto SBOM attestations using cosign's library internally |
| Grype | Plans to accept attestation-signed SBOMs for trusted vulnerability scanning |

### Notation / Notary Project

| Signal | Data |
|--------|------|
| `notaryproject/notation` GitHub stars | ~1,200 |
| Latest version | v1.3.0, February 2025 |
| CNCF status | Incubating (second security audit Jan 2025) |
| Primary adopters | ACR (Azure), ECR (AWS notation plugin), Ratify (policy enforcement) |
| Rust library | None |
| Enterprise preference | Azure DevOps, AWS ECR "best practices" docs recommend Notation |

### SBOM Tool Adoption

| Tool | Cosign support | Notation support | Primary format |
|------|---------------|-----------------|----------------|
| Syft | Yes (in-toto attestation via cosign) | No | CycloneDX JSON, SPDX JSON, Syft JSON |
| Grype | Via Syft SBOM | No | Consumes CycloneDX, SPDX |
| Trivy | Yes (OCI referrers + cosign verify) | Partial (plugin) | CycloneDX JSON, SPDX JSON |
| ORAS | Format-agnostic (pushes/pulls) | Format-agnostic | Any OCI layer |
| `cosign attest` | Native | Not supported | In-toto (DSSE) with CycloneDX/SPDX predicate |

### mise / proto / pixi (competing package managers)

None of these tools implement signature verification or SBOM display as of April 2026. OCX would be differentiated by being the first binary package manager to surface supply chain provenance directly in the install workflow.

---

## Citations

- `sigstore` v0.13.0 — https://docs.rs/sigstore/latest/sigstore/ — primary Rust cosign/keyless verification crate
- `sigstore-rs` GitHub — https://github.com/sigstore/sigstore-rs — release history, feature gaps (no attestation verification)
- `sigstore-trust-root` v0.6.4 — https://docs.rs/sigstore-trust-root — offline TUF root support, `TrustedRoot::production()`
- Sigstore Bundle Format v0.3.2 — https://docs.sigstore.dev/about/bundle/ — bundle structure, DSSE envelope spec, mediaType `application/vnd.dev.sigstore.bundle.v0.3+json`
- `sigstore/cosign` GitHub (v3.0.6, 5,800 stars) — https://github.com/sigstore/cosign — adoption signal, OCI 1.1 referrer mode
- Cosign OCI 1.1 referrers status — https://github.com/sigstore/cosign/issues/4335 — incomplete OCI 1.1 support across cosign subcommands (active as of 2025)
- Cosign OCI v1.1 Chainguard post — https://www.chainguard.dev/unchained/building-towards-oci-v1-1-support-in-cosign — referrer-based bundle storage
- `cyclonedx-bom` v0.8.1 — https://github.com/CycloneDX/cyclonedx-rust-cargo/blob/main/cyclonedx-bom/README.md — official CycloneDX Rust parsing library, versions 1.3–1.5
- `cyclonedx-rust-cargo` releases — https://github.com/CycloneDX/cyclonedx-rust-cargo/releases — v0.8.1 March 2025 confirmation
- `spdx-rs` v0.5.5 — https://github.com/doubleopen-project/spdx-rs — last release Sep 2023, 7 stars, SPDX 2.x only
- SPDX 3.0 tooling status — https://spdx.dev/unpacking-the-spdx-3-0-tooling-mini-summit-a-new-era-of-compliance-and-security/ — industry still transitioning, tooling incomplete
- SPDX 2.3 vs 3.0 bridge — https://dev.to/jijo-007/bridging-the-gap-converting-spdx-30-to-23-in-the-software-supply-chain-3omc — widespread 2.3 dependency
- Notary Project v1.3.0 — https://www.cncf.io/blog/2025/02/10/notary-project-announces-notation-v1-3-0-and-tspclient-go-v1-0-0/ — Go-only, no Rust library
- Notation v1.0.0 GitHub — https://github.com/notaryproject/notation — confirmed Go-only implementation
- Notary second security audit — https://www.cncf.io/blog/2025/01/21/notary-project-completes-its-second-audit/ — Incubating CNCF project
- Cosign vs Notation comparison — https://snyk.io/blog/signing-container-images/ — adoption analysis, use-case differentiation
- Syft + Sigstore attestations — https://anchore.com/sbom/creating-sbom-attestations-using-syft-and-sigstore/ — Syft uses cosign library internally
- CNCF OpenSSF Sigstore adoption — https://openssf.org/blog/2024/02/16/scaling-up-supply-chain-security-implementing-sigstore-for-seamless-container-image-signing/ — CNCF-scale adoption signal
- Trivy OCI referrers plugin — https://github.com/aquasecurity/trivy-plugin-referrer — Trivy referrer-based SBOM/scan pushing
- lib.rs sigstore stats — https://lib.rs/crates/sigstore — 49k monthly downloads, 16 reverse deps, #88 cryptography
