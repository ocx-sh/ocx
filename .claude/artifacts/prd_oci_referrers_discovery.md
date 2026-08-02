# PRD: OCI Referrers Discovery — `ocx verify` and `ocx sbom`

<!--
Product Requirements Document
Filename: artifacts/prd_oci_referrers_discovery.md
Owner: Architect (/architect)
Handoff to: Builder (/builder), QA Engineer (/qa-engineer)
Related Skills: writing-prds, decomposing-tasks, requirements-analysis
-->

## Overview

**Status:** Amended for Slice 2 (external discovery + SBOM) — trust-policy scope removed; Slice 1 ships signing per `prd_oci_referrers_signing_v1.md`
**Author:** mherwig (via architect worker, auto-mode; amended `/swarm-plan max` 2026-04-19)
**Date:** 2026-04-19
**Version:** 2.0 (Slice 2 amendment)
**GitHub Issue:** [#24 — feat: OCI referrers API for signature and SBOM discovery](https://github.com/ocx-sh/ocx/issues/24)
**PR-FAQ:** [`pr_faq_oci_referrers_discovery.md`](./pr_faq_oci_referrers_discovery.md) (amended for Slice 2)
**ADR:** [`adr_oci_referrers_discovery_v2.md`](./adr_oci_referrers_discovery_v2.md) (active Slice 2 design); [`adr_oci_referrers_discovery.md`](./adr_oci_referrers_discovery.md) is **superseded** and retained for historical record only
**Slice 1 PRD:** [`prd_oci_referrers_signing_v1.md`](./prd_oci_referrers_signing_v1.md) — the separate sign + verify product record for Slice 1
**Implementation Plan:** [`../state/plans/plan_slice2_external_discovery.md`](../state/plans/plan_slice2_external_discovery.md)
**Stakeholders:** OCX maintainers; CI/CD platform teams consuming OCX; OCX security-sensitive adopters (enterprise, regulated industries)

> **Amendment summary (2026-04-19):** The original PRD bundled `ocx verify` + `ocx sbom` + trust-policy TOML into a single release. On user feedback (2026-04-18) the design was split into two deliverable slices: **Slice 1** ships signing + enforcing verify (see `prd_oci_referrers_signing_v1.md`); **Slice 2** — this PRD — ships external-signature discovery (legacy cosign tag-based) and `ocx sbom` on top of Slice 1. Trust-policy TOML is deferred to v3+ with exit codes 78/79 reserved. `level = "skip"` is not in either slice. `--require-*` / `--distribution-spec` flags are removed; exit code 79 is the programmatic "nothing attached" signal.

## Problem Statement

OCX turns any OCI registry into a cross-platform binary distributor. But OCX consumers today — GitHub Actions runs, Bazel rulesets, Python orchestration scripts, devcontainer features — install OCX packages with no way to answer the two questions supply-chain security forces every serious automation owner to answer in 2026:

1. **Who published this binary?** (Signature verification — is the artifact authentic?)
2. **What is inside this binary?** (SBOM discovery — what dependencies and CVEs travel with it?)

The OCI Image Spec 1.1 defines a referrers graph: auxiliary manifests (signatures, SBOMs, attestations) attached to a subject artifact by digest. Tooling that already writes into this graph is everywhere (cosign v3, syft, trivy, oras attach); registries that serve the graph are most of the ecosystem (Harbor, Quay, ECR, ACR, Artifactory, Zot). OCX packages published today **already carry** cosign-signed referrers and syft-attested SBOMs — because cosign and syft don't care that it's an OCX package; they just see an OCI artifact.

What is missing is an **OCX-side consumer**. Without it, OCX users must:

- Shell out to `oras discover`, `cosign verify-blob`, or `crane manifest` — each with different JSON shapes, exit codes, flag styles
- Reinvent the fallback-tag handling logic themselves to support GHCR and Docker Hub (which don't yet implement the Referrers API)
- Write glue code to thread an OCX reference through a separate tool's identifier format
- Accept that `ocx install` may silently pull a tampered binary — there is no in-flow way to check

The problem is urgent:

- **EU Cyber Resilience Act** enforcement is scaling through 2026: products with digital elements must document provenance or block sale in the EU single market.
- **US Executive Order 14028** (2021, still accreting secondary rules) mandates federal software contractors ship SBOMs with each release.
- **Platform controllers** (Kyverno, Flux, GitHub Attestations, Sigstore Policy Controller) all require signed+attested artifacts by default; unsigned packages fail policy.

OCX's primary users — automation tools — are the exact population these regulations hit first. Without native verify/sbom commands, OCX becomes the odd supply-chain gap inside an otherwise-compliant pipeline.

### Evidence

**Quantitative Evidence:**

- OCI 1.1 Referrers API shipped February 2024; 8 of the 10 most popular registries (by container-image traffic) implement it as of April 2026 — Harbor 2.9+, Quay 3.11+, ECR, ACR, Artifactory, Zot 2.0+, registry:2 3.0-beta, GitLab 17+. The two holdouts — GHCR and Docker Hub — host the majority of open-source OCX package traffic (GHCR is the default for GitHub Actions-published packages).
- `sigstore-rs` v0.13.0 downloads averaged ~49,000/month in Q1 2026 — evidence that Rust-native cosign verification is a production-ready, production-chosen path.
- Cosign bug #4641 (wrong `artifactType` in fallback-index descriptors) has been open since January 2026 with no upstream fix — meaning any OCX-side fallback reader that trusts the descriptor silently misses GHCR signatures.
- Issue #24 has been prioritized since the `/swarm-plan` max-tier review of the product roadmap; supply-chain features have been a stated roadmap item in parent ADR `adr_oci_artifact_enrichment.md` since 2026-03-12.

**Qualitative Evidence:**

- Product-context rule (`product-context.md`) identifies "Private distribution first-class" and "Backend-first design" as OCX differentiators. Private distribution without authenticity verification is a degraded value proposition in the post-SolarWinds / post-xz-utils world.
- Competitive context: Homebrew has no signatures. apt/dnf have signed repos but no per-artifact SBOM. mise/asdf have neither. Nix has content-addressed storage but no signature discovery command. The closest comparable is `cosign verify` — but it's a container-image tool that doesn't know about OCX package identifiers. Shipping native verify/sbom commands is a clear differentiator row.
- Verification Honesty (`quality-core.md`) demands evidence-backed claims; shipping a tool that claims "distributes any pre-built binary" without any way to prove provenance of those binaries is a claim OCX can't back up with evidence.

## Goals & Success Metrics

| Goal | Metric | Target (12 months from release) |
|------|--------|-------------------------------|
| Enable OCX users to discover signatures on any OCX package with one command | `ocx verify` subcommand shipped; JSON output schema v1 frozen | v1 shipped |
| Cover the OCX-published ecosystem regardless of registry | `ocx verify` works against GHCR, Docker Hub, Harbor, Quay, ECR, ACR, Artifactory, Zot, registry:2 | 9+ of 9 tested registries |
| Not break any existing user | `ocx install` behavior unchanged | Zero regression reports attributable to this feature |
| Set up the ecosystem for auto-verify in v3+ without surface breakage | JSON envelope frozen at `schema_version: 1`; `--no-cache` + exit-code taxonomy stable across slices | No breaking CLI change when v3+ enforcement / trust-policy-file support lands (trust-policy dropped from both slices per superseded-ADR rejection; exit codes 78/79 reserved) |
| Machine consumption is first-class | Exit code matrix, JSON schema, stable text output | GitHub Actions integration path documented |
| Adoption signal | % of `ocx install` invocations preceded by `ocx verify` in published GitHub Actions workflows | ≥20% of public workflows using OCX within 12 months |

## User Stories

<!--
INVEST check:
- Independent: each story is a single CLI pathway
- Negotiable: acceptance criteria are testable but implementation-agnostic
- Valuable: each story answers a real question users ask today
- Estimable: scoped to a single command + output
- Small: each is one acceptance test fixture or less
- Testable: criteria map directly to pytest scenarios in the plan
-->

> **Slice 2 user-story scope:** Slice 1 (see `prd_oci_referrers_signing_v1.md`) covers the CI pipeline author signing + verifying OCX-native packages. Slice 2 user stories below add **external-signature discovery** (verify accepts legacy cosign tag-based signatures made by other tools) and **SBOM discovery** (new `ocx sbom` command).

### Persona 1: CI pipeline author consuming mixed-provenance packages

- **As a** GitHub Actions workflow author consuming a GHCR-hosted package signed with cosign outside OCX (e.g. a third-party tool's release), **I want** `ocx verify --certificate-identity X --certificate-oidc-issuer Y ghcr.io/vendor/tool:v1` to exit 0 when a legacy cosign `sha256-<digest>.sig` tag is present, **so that** I don't have to install a second tool just to verify third-party packages.
  - Acceptance: exit 0; JSON `signature_format: "cosign_legacy_v1"`, `discovery_method: "legacy_sig_tag"`.

- **As a** workflow author, **I want** `ocx verify` to silently auto-detect the format (Sigstore bundle v0.3 vs legacy cosign), **so that** my scripts don't have to branch per-publisher.
  - Acceptance: no new flags; same `ocx verify` surface as Slice 1.

- **As a** Bazel ruleset author, **I want** `ocx sbom ghcr.io/org/tool:v1 --download sbom.json`, **so that** my build rule can pin an SBOM artifact for downstream CVE scanning.
  - Acceptance: file written; bytes match layer digest; JSON report printed on stdout with `downloaded_to: "sbom.json"`, `format: "cyclonedx"|"spdx_2_3"`, `component_count >= 1`, `schema_version: 1`.

- **As a** Python orchestration script author, **I want** stable exit codes matching BSD sysexits.h, **so that** I can branch on specific failure reasons without parsing stderr.
  - Acceptance (Slice 2 reuses Slice 1 taxonomy — no new variants): 0 (success), 64 (usage), 65 (data/parse error / malformed SBOM / malformed legacy bundle / unsupported SBOM format), 69 (registry 5xx), 74 (`--download` I/O failure), 75 (429), 77 (403), 79 (no SBOM referrers / no matching signatures), 80 (401 / legacy identity mismatch / legacy issuer mismatch), 81 (offline + cache miss), 82 (Rekor unavailable).

### Persona 2: Security engineer / compliance auditor

- **As a** security engineer at a regulated org, **I want** `ocx sbom` to parse CycloneDX and SPDX 2.3 SBOMs into a stable JSON summary (name, version, component count, license histogram, toolchain), **so that** compliance reports can be generated from `ocx sbom --format json` without jq piping.
  - Acceptance: JSON output has `schema_version: 1` at root; `format ∈ {"cyclonedx", "spdx_2_3", "in_toto_wrapper"}`; `component_count` numeric; `license_histogram` object with SPDX expressions as keys.

- **As a** security engineer, **I want** `ocx sbom` to error cleanly on SPDX 3.0 and CycloneDX 1.6 inputs, **so that** I know when the tool can't parse what's attached and the compliance pipeline should surface the limitation.
  - Acceptance: exit 65; JSON `error_kind: "sbom_unsupported_format"`; remediation text names the format + version and points at the tracking issue.

- **As a** security engineer, **I want** clear documentation of what `ocx verify` does and does not verify in Slice 2, **so that** I don't assume a verification guarantee that isn't there.
  - Acceptance: `ocx verify --help` states "verifies Sigstore bundle v0.3 AND legacy cosign `.sig` tag signatures; other formats (Notation, DSSE) are discovered but not verified — verify them out-of-band"; user-guide section mirrors this.

### Persona 3: Platform engineer running in offline CI

- **As a** platform engineer running in air-gapped CI, **I want** `ocx sbom --offline <ref>` to succeed from cache when a prior online run populated `~/.ocx/blobs/<registry>/.referrers/`, **so that** air-gapped pipelines work.
  - Acceptance: online run populates cache with 24h CI TTL; subsequent offline run exits 0 and hits neither the capability cache probe nor the Referrers API; offline run with no cache exits 81.

- **As a** platform engineer debugging a stale cache, **I want** a single `--no-cache` flag that bypasses **both** the capability cache AND the referrer-index cache, **so that** "force fresh" is one flag, not two.
  - Acceptance: `--no-cache` documented as bypassing both; transport call counter asserts a fresh network round trip.

### Persona 4: OCX-package publisher using mixed tooling

- **As an** OCX package publisher using cosign 2.x to sign packages (not `ocx package sign`), **I want** consumers' `ocx verify` to find my signatures via the legacy `.sig` tag path, **so that** my existing cosign pipelines don't need to migrate.
  - Acceptance: legacy cosign signatures verified via `oci/verify/legacy_cosign.rs`; manifest-walk fallback works on GHCR and Docker Hub; cosign bug #4641 (wrong fallback-index `artifactType`) correctly classified via per-manifest fetch.

## Requirements

### Functional Requirements

Slice 2 FRs only — see `prd_oci_referrers_signing_v1.md` for Slice 1's `ocx package sign` / `ocx verify` FRs (which are extended here, not duplicated).

| ID | Requirement | Priority | Notes |
|----|-------------|----------|-------|
| FR-S2-1 | `ocx verify` auto-discovers legacy cosign tag-based signatures (`sha256-<digest>.sig` tag + `application/vnd.dev.cosign.artifact.sig.v1+json` manifest) alongside Sigstore bundle v0.3 | Must Have | Transparent; no new flag |
| FR-S2-2 | `ocx verify` applies the same `--certificate-identity` / `--certificate-oidc-issuer` match + same Rekor SET verification to legacy bundles as to v0.3 bundles | Must Have | Identity + issuer + chain verified against Slice 1 trust root |
| FR-S2-3 | `ocx verify` JSON output includes `signature_format ∈ {sigstore_bundle_v0_3, cosign_legacy_v1}` and `discovery_method ∈ {referrers_api, fallback_tag, legacy_sig_tag}` | Must Have | Additive to Slice 1 JSON; `schema_version` stays `1` |
| FR-S2-4 | `ocx verify` on GHCR / Docker Hub uses manifest-walk fallback and defensively classifies each referrer by its own manifest body (cosign #4641 mitigation) | Must Have | Correctness requirement per research §3 |
| FR-S2-5 | When both a v0.3 bundle (via Referrers API) AND a legacy `.sig` tag exist, `ocx verify` verifies both and reports both in JSON array; exit 0 iff at least one matches | Must Have | Standard cosign semantics |
| FR-S2-6 | New `ocx sbom <REFERENCE>` command discovers SBOM referrers (CycloneDX, SPDX 2.3, in-toto-wrapped) for the per-platform manifest | Must Have | Core Slice 2 deliverable |
| FR-S2-7 | `ocx sbom` parses CycloneDX 1.3–1.5 via `cyclonedx-bom = "=0.8.1"` into a stable `SbomSummary` DTO (name, version, component count, license histogram, tool) | Must Have | 1.6 returns exit 65 with `error_kind: sbom_unsupported_format` |
| FR-S2-8 | `ocx sbom` parses SPDX 2.3 (and 2.2 backward-compat) via `spdx-rs = "=0.5.5"`; SPDX 3.0 JSON-LD returns exit 65 `sbom_unsupported_format` | Must Have | No Rust 3.0 parser exists (Apr 2026) |
| FR-S2-9 | `ocx sbom --download <PATH>` writes **raw layer bytes** (pre-parse) to `PATH`; `--download -` streams to stdout and **suppresses** the plain/JSON summary entirely (nothing to stdout or stderr on happy path) | Must Have | Invariant preserved from superseded ADR §Invariant 5 |
| FR-S2-10 | `ocx sbom --prefer cyclonedx|spdx` selects preferred format when multiple SBOMs are attached; falls back to "first in referrer order" if `--prefer` format not present | Should Have | Default: `cyclonedx` |
| FR-S2-11 | `ocx sbom --platform OS/ARCH` selects the per-platform manifest before SBOM lookup | Must Have | Symmetric with `ocx install --platform` |
| FR-S2-12 | `ocx sbom` on subject with no SBOM referrers exits 79 with JSON `error_kind: sbom_no_referrers_found` | Must Have | Symmetric with `ocx verify` no-match semantics |
| FR-S2-13 | New referrer-index cache at `~/.ocx/blobs/<registry>/.referrers/<repo>/<subject-digest>.json` with 1h interactive / 24h CI TTL | Must Have | Distinct from Slice 1 capability cache |
| FR-S2-14 | `--no-cache` flag (on both `ocx sbom` and `ocx verify`) bypasses BOTH the referrer-index cache AND the Slice-1 capability cache | Must Have | Single-flag "force fresh" semantics |
| FR-S2-15 | `ocx sbom --offline` with a warm referrer-index cache + warm SBOM blob in content-addressed store exits 0; cold cache exits 81 | Must Have | Offline-first principle |
| FR-S2-16 | JSON output for `ocx sbom` includes `schema_version: 1` at root — same envelope as Slice 1, no version bump | Must Have | Decision S2-D; see ADR v2 |
| FR-S2-17 | New `error_kind` values added to Slice 1's error envelope: `sbom_no_referrers_found`, `sbom_parse_error`, `sbom_unsupported_format`, `sbom_download_io_error`, `legacy_cosign_bundle_malformed`, `legacy_cosign_identity_mismatch`, `legacy_cosign_issuer_mismatch` | Must Have | See ADR v2 §Error-Kind Additions |
| FR-S2-18 | `ocx install` behavior is unchanged — no implicit verify, no implicit SBOM fetch, no file-presence side-effects | Must Have | Regression test required |
| FR-S2-19 | Text output for `ocx sbom` obeys the single-table rule: one `print_table` call for the main summary; license histogram rendered as an indented sub-block (not a second table) | Must Have | `subsystem-cli-api.md` compliance |
| FR-S2-20 | User-guide documentation page updated to cover `ocx sbom` + legacy cosign support under `ocx verify` | Must Have | Security features without docs are block-tier per `quality-core.md` |
| FR-S2-21 | In-toto-wrapped SBOMs (`application/vnd.in-toto+json` layer) are DSSE-unwrapped and recursively classified by predicate type; the wrapped SBOM is then parsed by the format-appropriate parser | Should Have | Deep predicate validation is deferred to v3+ |

### Non-Functional Requirements

Slice 2 NFRs. Slice 1's NFRs (binary-size, Rekor behavior, sigstore pin) are cited where extended.

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-S2-1 | `ocx sbom` latency | Cold cache < 400 ms p50 (two round trips: manifest + layer); warm cache (local blob + cache hit) < 50 ms p50 |
| NFR-S2-2 | `ocx verify` latency on legacy-cosign path | ≤ 20 % overhead over v0.3 path (one extra manifest fetch via fallback tag); warm cache < 50 ms p50 |
| NFR-S2-3 | Binary-size impact of adding `cyclonedx-bom` + `spdx-rs` | ≤ +1.8 MB over Slice-1 binary (Slice 1 added ≤ +3.5 MB over pre-feature per `prd_oci_referrers_signing_v1.md`) |
| NFR-S2-4 | Offline-first | `--offline` with valid cache + local blob exits 0 with no network; cold cache exits 81 (OfflineBlocked) |
| NFR-S2-5 | No breaking change to Slice 1 surface | `ocx verify` JSON adds new fields (`signature_format`, `discovery_method`); no existing field removed or renamed |
| NFR-S2-6 | No breaking change to existing `ocx install` surface | `ocx install` diff contains only unchanged code |
| NFR-S2-7 | Exit-code coverage | 100 % of exit codes listed in Persona 1 FR-S2 (0/64/65/69/74/75/77/79/80/81/82) exercised by an acceptance test |
| NFR-S2-8 | Cosign bug #4641 defensive path | Dedicated acceptance test writes a corrupt fallback-index descriptor and asserts correct per-manifest classification |
| NFR-S2-9 | `task verify` gate must pass post-change | Zero clippy regressions; `cargo deny check` clean on `cyclonedx-bom` + `spdx-rs` licenses (both Apache-2.0) |
| NFR-S2-10 | Dep pins | `cyclonedx-bom = "=0.8.1"`, `spdx-rs = "=0.5.5"` (exact) until a deliberate bump PR |
| NFR-S2-11 | No Go FFI, no C build deps, no non-stdlib Python deps in acceptance tests | Verified in review |
| NFR-S2-12 | Schema-stable JSON | `schema_version: 1` persists across Slice 2; new fields are additive; breaking change requires bump to `2` |
| NFR-S2-13 | Referrer-index cache disk footprint | ≤ 100 KB per subject; no unbounded growth during 24h window (size test in acceptance suite) |
| NFR-S2-14 | Fixture-based testing | No live cosign invocation in CI; no live Fulcio/Rekor calls; fixtures committed bytes only |

## Scope

### In Scope (Slice 2)

- `ocx verify` legacy-cosign discovery (additive; same CLI surface as Slice 1)
- New `ocx sbom <REFERENCE>` subcommand (flags: `--platform`, `--download`, `--prefer`, `--no-cache`, `--format`)
- Referrer-index cache at `~/.ocx/blobs/<registry>/.referrers/<repo>/<subject-digest>.json`
- Manifest-walk fallback (reads `sha256-<digest>` fallback tag AND `sha256-<digest>.sig` legacy cosign tag) with per-manifest defensive classification (cosign #4641 mitigation)
- CycloneDX 1.3–1.5 parsing via `cyclonedx-bom = "=0.8.1"`
- SPDX 2.3 (and 2.2 backward-compat) parsing via `spdx-rs = "=0.5.5"`
- In-toto DSSE envelope unwrap + recursion on wrapped SBOM payload
- New `error_kind` values in Slice 1's envelope (no schema bump)
- User-guide updates (`ocx sbom` section, `ocx verify` legacy-cosign note)
- Acceptance tests against `registry:2` with deterministic pre-generated fixtures
- Typed exit codes reused from Slice 1 (no new variants)

### Out of Scope (v3+ or later)

- Writing SBOMs (`ocx package attest` — parent-ADR follow-on)
- DSSE attestation verification (sigstore-rs 0.13 gap; waiting for 0.14+)
- Notation signature verification (no Rust library as of 2026-04)
- Cosign key-based signing and verification
- Auto-verification during `ocx install`
- Trust-policy TOML file (still reserved at exit codes 78/79 per Slice 1 §S1-G)
- CycloneDX 1.6 parsing (`cyclonedx-bom` 0.8.1 tops out at 1.5; `sbom_unsupported_format` exit 65)
- SPDX 3.0 JSON-LD parsing (no Rust parser exists; `sbom_unsupported_format` exit 65)
- In-toto predicate deep parse (SLSA provenance, VEX — Slice 2 only unwraps + classifies, does not validate predicate schema)
- Signature rotation / revocation UX
- `ocx clean --referrer-cache` subcommand (user guide documents `find` one-liner)
- Referrer list pagination (lists < 10 per subject in practice)
- Discovery of GitHub-proprietary Attestations API (use `gh attestation verify`)
- `--require-*` and `--distribution-spec` flags (use exit code 79 as programmatic signal)
- `level = "skip"` trust-policy mode (removed from superseded ADR; both slices are enforcing-only)

## Dependencies

| Dependency | Owner | Status | Risk |
|------------|-------|--------|------|
| Slice 1 landed on `main` | OCX core | **Blocking** — Slice 2 cannot start until Slice 1 is merged | High |
| `cyclonedx-bom = "=0.8.1"` | Upstream (CycloneDX org) | Available; Apache-2.0 | Low — maintained; reintroduced in Slice 2 after being removed from the superseded ADR |
| `spdx-rs = "=0.5.5"` | Upstream | Available; Apache-2.0 | Medium — unmaintained upstream but pinned exactly; 2.3 deserialization path works |
| `sigstore = "=0.13"` | Upstream (Sigstore org) | Already pulled in by Slice 1 | — (reused) |
| `sigstore-trust-root = "=0.6.4"` | Upstream | Already pulled in by Slice 1 | — (reused) |
| `oci-client` patched fork with `pull_referrers` | OCX fork at `external/rust-oci-client` | Already used by Slice 1 | — (reused) |
| `cosign` CLI (maintainer's machine) | OCX mirror team | Needed for fixture regeneration only; not in CI | Low |
| `syft` CLI (maintainer's machine) | OCX mirror team | Optional (CycloneDX fixture regeneration) | Low |

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `sigstore` crate breaking change in a minor release blocks security patch | M | H | Pinned `=0.13`; upgrade is a dedicated PR with its own acceptance-test gate; defer Dependabot on this dep |
| Cosign bug #4641 fixed upstream and OCX defensive parse wastes network | M | L | Defensive parse remains correct post-fix; cost is bounded (1–5 round trips per invocation); acceptance test covers both pre-fix and post-fix shapes |
| GHCR adds Referrers API mid-year and OCX keeps probing | L | L | 7-day per-registry capability cache auto-detects; `--no-cache` escape hatch |
| `ocx sbom` output mistaken for "verified SBOM" — hostile publisher pushes a doctored SBOM referrer that does not reflect the subject bytes | M | H | **SBOM trust gap documented in three places (ADR v2 §Context, §CLI surface, §Risks):** `ocx sbom --help` preamble explicitly says "Does NOT verify the SBOM's signature or that the SBOM matches the subject"; user-guide SBOM page carries a prominent admonition; `ocx verify` documentation reminds operators that verifying the subject does not verify the SBOM. v3+ tracked via GitHub issue for `ocx sbom --verify` (not Slice 2 scope). |
| `spdx-rs` 0.5.5 stalled (last upstream commit 2023-11-27) — Phase 3 acceptance tests surface a parse bug on an SPDX 2.3 edge case | M | M | Pre-selected contingency in ADR v2 §S2-C: switch `sbom/spdx.rs` to `serde-spdx` without re-opening the ADR; `SbomSummary` DTO unchanged; only `sbom/spdx.rs` call site changes. Fallbacks: vendor the 2023-11-27 snapshot under `external/spdx-rs/`, or write a minimal 200-line deserialiser. |
| GitHub Actions attestations on GHCR invisible to `ocx verify` | H | M | Documented limitation: `actions/attest-build-provenance` stores provenance via GitHub's REST Attestations API (not OCI referrers). Users call `gh attestation verify` separately; v2 may add native discovery. Documented in user guide + `ocx verify --help`. |
| `--download -` collides with JSON output on stdout | L | H if collision | Explicit test: `--download -` suppresses JSON report to stderr |
| `ocx verify` mis-sold as "verified" — Slice 2 verifies real (Sigstore v0.3 + legacy cosign) but users may assume more (Notation, DSSE) | L | M | `--help` text explicitly lists what is and is not verified: "verifies Sigstore bundle v0.3 AND legacy cosign `.sig` tag signatures; Notation, DSSE, and GitHub Attestations are discovered-not-verified and must be verified out-of-band"; `signature_format` field in JSON output makes the verified format explicit; user-guide covers. |
| Registry rate-limit on Docker Hub (10/hr anonymous) causes flaky CI | M | M | 24h non-interactive cache TTL; 429 backoff with `Retry-After`; `--no-cache` documented as an anti-pattern for CI |
| `sigstore-rs` TUF root refresh fails in offline mode | L | M | Use offline-safe embedded root; fallback to discovery-only on TUF refresh failure with warning on stderr |
| Fixture flakiness from live Fulcio/Rekor | H | H | Use static bundle test vectors checked into `test/fixtures/cosign/`; no live Fulcio/Rekor calls |
| Fixture tooling (cosign/oras) not in dogfood mirror on execute date | M | M | Fixture prep becomes a 1-day unblocking task; pytest skips with clear reason if absent |
| `ocx install` implicit verification creeps in via well-meaning PR | L | H | Explicit regression test: `install.rs` does not import `ocx_lib::referrer::*`; code review checklist |

## Open Questions

All v1-era open questions that still apply have been resolved under the Slice 2 split. Trust-policy-related questions (Q-PRD-3, Q-PRD-7 mentions of `--require-referrers`) are obsoleted by the removal of those flags.

- [x] **Q-PRD-S2-1 [RESOLVED]:** `--download -`: JSON/text report is **not emitted at all** (not to stdout, not to stderr). Raw SBOM bytes replace stdout; stderr empty on happy path. See ADR v2 §`ocx sbom` CLI surface.
- [x] **Q-PRD-S2-2 [RESOLVED]:** CI TTL heuristic uses **stdin-is-not-a-TTY** as primary detection, `CI=true` as secondary hint; same mechanism as Slice 1's capability cache (single source of truth).
- [x] **Q-PRD-S2-3 [RESOLVED]:** Transport errors take precedence over "no match" errors. Registry 500 → exit 69; 401 → 80; 404 on Referrers API → manifest-walk fallback (not an error). No `--require-*` flag means no exit-65 precedence conflict.
- [x] **Q-PRD-S2-4 [RESOLVED]:** `ocx sbom` empty result is exit 79 (Decision S2-F in ADR v2) — symmetric with `ocx verify` no-match semantics.
- [ ] **Q-PRD-S2-5:** Should `ocx sbom --download` support content-length precheck + disk-full errors with a specific `error_kind_detail`? Currently: `sbom_download_io_error` exit 74 with `io::Error` detail. **[DEFERRED — user experience refinement; v3+]**
- [ ] **Q-PRD-S2-6:** Should `ocx sbom` expose `--verify-bundle-first` to chain verify + sbom in one command (fail-closed if verify fails)? Currently: user chains via shell (`ocx verify && ocx sbom`). **[DEFERRED — shell-chain is fine for Slice 2; v3+ if adoption signal requires]**
- [ ] **Q-PRD-S2-7:** CycloneDX 1.6 support timeline — tracking upstream `cyclonedx-bom` v0.9+; auto-upgrade when available. **[DEFERRED — Slice 3+ PR]**

## Appendix

### Research

- [`research_cosign_sigstore_notation.md`](./research_cosign_sigstore_notation.md) — sigstore-rs viability, Notation Rust gap, DSSE status
- [`research_verify_cli_patterns.md`](./research_verify_cli_patterns.md) — peer-tool CLI warts to avoid, JSON shape precedent
- [`research_oci_referrers_2026.md`](./research_oci_referrers_2026.md) — registry compatibility matrix, cosign bug #4641, per-platform descent rules
- [`discover_referrers_architecture_map.md`](./discover_referrers_architecture_map.md) — OCX module extension seams
- [`discover_oci_client_extension_points.md`](./discover_oci_client_extension_points.md) — `pull_referrers` already upstream
- [`discover_cli_command_conventions.md`](./discover_cli_command_conventions.md) — OCX CLI conventions
- [`discover_test_fixture_referrers.md`](./discover_test_fixture_referrers.md) — registry:2 fixture options

### Competitive Analysis

| Tool | Covers signatures | Covers SBOMs | Handles GHCR | Stable JSON | Exit-code discipline |
|------|:-:|:-:|:-:|:-:|:-:|
| `cosign verify` | Yes | No | Yes (fallback) | NDJSON (wart) | Mixed |
| `cosign attest` | No | Write-only | Yes | N/A | N/A |
| `notation verify` | Yes (Notation only) | No | Yes | Partial | Partial |
| `oras discover` | Discovery | Discovery | Yes (fallback) | Yes | Yes |
| `syft packages` | No | Generate | Yes | Yes | Yes |
| `trivy image` | Scan-based | Yes | Yes | Yes | Yes |
| **`ocx verify` (proposed)** | Cosign keyless | No | Yes (fallback) | Yes (schema v1) | Yes (typed) |
| **`ocx sbom` (proposed)** | No | Discovery + download | Yes (fallback) | Yes (schema v1) | Yes (typed) |

OCX differentiator: **one tool, one identifier format** (OCX reference) for both signature discovery and SBOM discovery, with the OCX offline-first and backend-first principles intact.

---

## Approval

| Role | Name | Date | Status |
|------|------|------|--------|
| Product | | | Pending |
| Engineering | | | Pending |
| Security | | | Pending (recommended reviewer) |

---

## Next Steps & Handoffs

After PRD approval:

1. [x] **Architect Review**: Already in flight — ADR is Proposed.
   - Output: [`adr_oci_referrers_discovery.md`](./adr_oci_referrers_discovery.md)

2. [x] **Implementation Plan**: Drafted.
   - Output: [`../state/plans/plan_oci_referrers_discovery.md`](../state/plans/plan_oci_referrers_discovery.md)

3. [ ] **`/swarm-execute` max 24**: Phase 4 — execute plan via contract-first TDD with builder + tester + reviewer workers.

4. [ ] **Documentation**: Website page `signatures-and-sboms.md` drafted during Phase 4.

5. [ ] **Post-launch**: Track adoption signal (percent of `ocx install` preceded by `ocx verify` in public GHA workflows).

**Related Artifacts**:

- ADR: [`adr_oci_referrers_discovery.md`](./adr_oci_referrers_discovery.md)
- PR-FAQ: [`pr_faq_oci_referrers_discovery.md`](./pr_faq_oci_referrers_discovery.md)
- Implementation Plan: [`../state/plans/plan_oci_referrers_discovery.md`](../state/plans/plan_oci_referrers_discovery.md)

---

## Version History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-04-19 | architect worker (via `/swarm-plan max 24`) | Initial draft synthesizing Discover + Research phases and ADR decisions |
| 2.0 | 2026-04-19 | architect worker (Slice 2 amendment, `/swarm-plan max`) | Split v1 into Slice 1 (sign + verify) + Slice 2 (this doc — external discovery + SBOM). Removed: trust-policy TOML, `--require-*`, `--distribution-spec`, `level = "skip"` scope. Added: SBOM discovery persona + FRs (FR-S2-6..S2-21), legacy-cosign verify persona + FRs (FR-S2-1..S2-5), referrer-index cache FR-S2-13. Dependencies updated (`cyclonedx-bom` + `spdx-rs` activated; Slice 1 is blocking). ADR pointer changed to `adr_oci_referrers_discovery_v2.md`. Implementation plan pointer changed to `plan_slice2_external_discovery.md`. |
