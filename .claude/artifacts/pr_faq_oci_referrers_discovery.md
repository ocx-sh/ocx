# PR-FAQ: OCX 0.X — External Signature Discovery + `ocx sbom` (Slice 2)

<!--
Press Release / Frequently Asked Questions
Filename: artifacts/pr_faq_oci_referrers_discovery.md
Owner: Architect (/architect)
Handoff to: Architect (/architect) and reviewer panel for feasibility
Related Skills: writing-pr-faqs, brainstorming, requirements-analysis

Amazon Working Backwards methodology — written as if already launched.
-->

## Overview

**Status:** Amended for Slice 2 scope (2026-04-19) — signing and enforcing verify already shipped in Slice 1; see [`pr_faq_oci_referrers_signing_v1.md`](./pr_faq_oci_referrers_signing_v1.md)
**Author:** mherwig (via architect worker, auto-mode; amended `/swarm-plan max` 2026-04-19)
**Date:** 2026-04-19
**GitHub Issue:** [#24 — feat: OCI referrers API for signature and SBOM discovery](https://github.com/ocx-sh/ocx/issues/24)
**Slice 1 PR-FAQ:** [`pr_faq_oci_referrers_signing_v1.md`](./pr_faq_oci_referrers_signing_v1.md) — sign + verify (Sigstore bundle v0.3)
**Active ADR:** [`adr_oci_referrers_discovery_v2.md`](./adr_oci_referrers_discovery_v2.md)
**Superseded ADR:** [`adr_oci_referrers_discovery.md`](./adr_oci_referrers_discovery.md) — historical only

> **Amendment summary (2026-04-19):** The original PR-FAQ bundled signing, verify, SBOM, and trust-policy TOML into a single launch. On user feedback the project was split into two deliverable slices. **Slice 1 has shipped**: `ocx package sign` (Sigstore bundle v0.3) + `ocx verify` (enforcing; no `skip` mode). **Slice 2 — this document — now ships**: external-signature discovery (legacy cosign tag-based `.sig` signatures from tools other than `ocx`) and `ocx sbom` (CycloneDX 1.3–1.5 + SPDX 2.3). `level = "skip"` never existed in either shipped slice; trust-policy TOML remains deferred.

---

# PRESS RELEASE

## OCX `ocx verify` Accepts Third-Party Signatures; New `ocx sbom` Command Parses Attached SBOMs

**San Francisco — 2026-Qx-XX** — OCX, the first general-purpose binary package manager built on the OCI distribution specification, today extended its Slice-1 supply-chain verification (`ocx package sign` + `ocx verify`, shipped earlier this quarter) with two additions for consuming mixed-provenance packages and doing compliance work. `ocx verify` now auto-discovers legacy cosign `.sig` tag-based signatures made by tools other than `ocx` — meaning any cosign-signed package on GHCR, Docker Hub, or any private registry is now verifiable with `ocx verify` out of the box, no flag required. Separately, the brand-new `ocx sbom <REFERENCE>` command discovers SBOMs (CycloneDX 1.3–1.5 or SPDX 2.3) already attached as OCI referrers and parses them into a stable JSON summary — ready to feed compliance reports, CVE scanners, or EU Cyber Resilience Act documentation pipelines.

### The Problem (post-Slice 1)

Slice 1 shipped a clean OCX-signs → OCX-verifies loop. What it didn't solve: OCX users routinely pull packages they didn't publish — a vendor's CLI hosted on GHCR, a build-tool release signed with cosign 2.x against a corporate registry, a base image signed before Slice 1 existed. Those packages carry **legacy cosign tag-based signatures** (`sha256-<digest>.sig` tag + `application/vnd.dev.cosign.artifact.sig.v1+json` media type), not Sigstore bundle v0.3 referrers, because cosign still writes the legacy format on registries without full Referrers API support — and that's still most of the public container-registry traffic (GHCR and Docker Hub both lack Referrers API as of April 2026). The Slice-1 `ocx verify` returned "no signatures" on those packages, defeating the use case.

Parallel problem: compliance teams running on OCX have been shelling out to `oras discover` + `cosign download sbom` + `syft convert` + `jq` to assemble an SBOM answer. That's four tools, four JSON shapes, four exit-code conventions, and a fragile pipeline script per package.

### The Solution (Slice 2)

With this release, `ocx verify` auto-detects both Sigstore bundle v0.3 referrers (Slice 1) AND legacy cosign tag-based signatures (Slice 2), verifying both with the same Fulcio+Rekor trust chain and the same `--certificate-identity` / `--certificate-oidc-issuer` match that Slice 1 already requires. If a package has both formats (cosign v3 writes both for compatibility), `ocx verify` checks both and reports both in the JSON audit trail.

`ocx sbom ghcr.io/vendor/tool:v1.0 --download sbom.json` discovers SBOM referrers attached to any OCX package, parses CycloneDX or SPDX 2.3 into a stable `schema_version: 1` summary (format, spec version, tool, root component, component count, license histogram), and optionally writes the raw SBOM bytes to a file — or to stdout for piping into vulnerability scanners. Exit codes match Slice 1's BSD sysexits.h taxonomy with zero new variants, so automation scripts branch on `$?` without parsing stderr.

### How It Works

- **`ocx verify pkg:1.0 --certificate-identity X --certificate-oidc-issuer Y`** — Same surface as Slice 1. Slice 2 adds format auto-detection: Sigstore bundle v0.3 (Slice 1 native) AND legacy cosign `.sig` tag (Slice 2 new). Both enforced against the same Fulcio+Rekor trust chain. JSON now includes `signature_format` + `discovery_method` fields alongside the Slice-1 shape; `schema_version` stays at `1`.
- **`ocx sbom pkg:1.0 --download sbom.json`** — New in Slice 2. Finds SBOM referrers attached to the per-platform manifest, parses CycloneDX 1.3–1.5 or SPDX 2.3 into a stable JSON summary (format, components, licenses, tool), and writes the raw SBOM bytes to a file. `--download -` streams bytes to stdout for pipe composition.
- **Works on any registry** — The OCI 1.1 Referrers API is used where available (Harbor, Quay, ECR, ACR, Artifactory, Zot, registry:2, GitLab 17+); a transparent manifest-walk fallback covers Docker Hub and GHCR. Choice is auto-detected and cached per registry (Slice 1 capability cache; 7-day TTL).
- **Cosign #4641 defensive** — Cosign has a known bug where the fallback-index descriptor reports the wrong `artifactType`. `ocx verify` fetches each referrer manifest individually and classifies by the manifest body — never by the fallback-index descriptor — so legacy-cosign signatures on GHCR / Docker Hub are correctly identified.
- **New referrer-index cache** — Slice 2 adds `~/.ocx/blobs/<registry>/.referrers/<repo>/<subject-digest>.json` with 1h interactive / 24h CI TTL. `--no-cache` bypasses both this new cache AND Slice 1's capability cache in one flag.
- **No new infrastructure** — Same registry you run for container images. Same embedded Sigstore production trust root from Slice 1; no new key server, no new crypto dependencies. `cyclonedx-bom = "=0.8.1"` + `spdx-rs = "=0.5.5"` are the only new crates.
- **Offline-first** — Populated caches survive the whole CI run; `ocx sbom --offline` and `ocx verify --offline` succeed from cache without touching the network; cold cache exits 81 (`OfflineBlocked`).
- **Trust-policy TOML still deferred** — Both Slice 1 and Slice 2 stay flag-based. Exit codes 78 (ConfigError) and 79 (NotFound for a specified-missing file) remain reserved for v3+ when trust-policy TOML lands.

### Quote from Project Lead

> "OCX's promise is that any OCI registry can be a serious binary distributor. That promise doesn't hold if the binary has no verifiable provenance. With `ocx verify` and `ocx sbom`, OCX users inherit the entire cosign and syft ecosystem without writing a line of glue code — and they get native, offline-capable verification in a single Rust binary with no runtime dependencies. Compliance stops being a thirty-line shell pipeline and becomes one command."
>
> — Michael Herwig, OCX maintainer

### Quote from Customer (anticipated)

> "Our pipeline used to pull `jq` just to parse the output of three different supply-chain CLIs. With `ocx verify && ocx install`, we replaced 200 lines of CI script with two commands. Our EU CRA audit report now points at a single JSON schema instead of a custom collation layer."
>
> — Platform team at a European SaaS company (anticipated early adopter)

### Getting Started

Install OCX as usual (`curl -fsSL get.ocx.sh | bash` or via `cargo install`). Run `ocx verify --certificate-identity <id> --certificate-oidc-issuer <issuer> <your-package>:<tag>` against any OCX package you already consume (including packages signed with legacy cosign from any vendor). Pipe SBOMs to your scanner: `ocx sbom <your-package>:<tag> --download - | trivy sbom -`. Documentation: [`website/src/docs/signatures-and-sboms.md`](../../website/src/docs/signatures-and-sboms.md) (shipping with the release).

---

# INTERNAL FAQ

## Strategic Questions

### Why should we build this now?

Three forces converge in 2026:

1. **Regulatory pressure.** EU Cyber Resilience Act enforcement scales through the year; US Executive Order 14028 secondary rules mandate SBOMs on federal software contracts. OCX users in regulated industries need a native path to compliance data, not a bolt-on.
2. **Ecosystem readiness.** Cosign v3 ships with OCI 1.1 referrer bundles as the default output. `sigstore-rs` v0.13.0 is actively maintained and gives OCX a credible Rust-native verification path with ~49k monthly downloads. 8 of 10 major registries support the Referrers API as of April 2026.
3. **Competitive window.** Every peer package manager (Homebrew, apt, mise, asdf, even Nix) lacks a first-class per-artifact signature + SBOM surface. Shipping this feature is a clean differentiator row in the product-context matrix — one that aligns with OCX's existing "backend-first" and "private-first" principles.

Waiting another year means users either adopt `oras discover` + `cosign verify` as the workaround standard (reducing the incremental value of `ocx verify`) or they churn to alternatives that already solve the problem.

### What is the target market size?

| Metric | Value | Source |
|--------|-------|--------|
| TAM — automation tools requiring supply-chain metadata | "every CI platform with supply-chain policy" — effectively every GitHub Actions, GitLab CI, Bazel, and Buildkite user running regulated workflows | Analyst estimates of 2026 DevSecOps tooling market: $10B+ |
| SAM — OCX-addressable users needing binary distribution + verification in one tool | Subset of that market that distributes pre-built binaries across OSes; OCX-specific differentiator is "one tool for both" | Estimated low hundreds of millions in annual DevSecOps tooling spend attributable to binary-distribution use cases |
| SOM — OCX's 12-month realistic capture | Users already inside the OCX ecosystem + inbound from "I hit the GHCR referrers gap" searches | Proxy metric: % of `ocx install` invocations preceded by `ocx verify` in public GHA workflows (target ≥20% in 12 months) |

### Who are the competitors?

| Competitor | Strengths | Weaknesses | Our Differentiation |
|------------|-----------|------------|---------------------|
| `cosign verify` | Dominant signing tool; Kubernetes-native | Container-image-shaped; NDJSON output; no OCX identifier awareness; no SBOM; no stable JSON schema | OCX-reference-aware; stable `schema_version`; one identifier format across verify and sbom |
| `notation verify` | Enterprise-PKI focus; Azure/AWS tutorials | No Rust library; Notation trust model is separate from cosign; limited adoption outside enterprise | Cosign-native; single binary; no Go FFI |
| `oras discover` | Covers both API and fallback-tag discovery; good JSON | Discovery only — no verification; a CLI from a different mental model | OCX-integrated; verification bundled; exits meaningfully on require-checks |
| `syft packages` + `trivy image` | Strong SBOM and CVE story | Two tools for one question; user must wire them together; no OCX reference format | Single command; outputs already consumable by the OCX install pipeline |
| `docker trust inspect` | Docker-native | Docker Content Trust legacy; deprecated path | N/A — different trust model |
| Homebrew | Massive user base | No signatures, no SBOMs, no private binaries | OCX has all three |

### What are the key risks?

| Risk | Likelihood | Impact | Mitigation |
|------|:-:|:-:|------------|
| `sigstore` crate breaking change blocks security patch | M | H | Pin from Slice 1 (`=0.13`); Slice 2 adds no new sigstore API surface, so upgrade exposure stays bounded |
| Cosign bug #4641 defensive parse becomes pure overhead post-fix | M | L | Parse remains correct; cost bounded; acceptance-test covers both pre- and post-fix shapes |
| CycloneDX / SPDX schema drift across spec versions breaks the summary | M | M | Pin `cyclonedx-bom = "=0.8.1"` + `spdx-rs = "=0.5.5"`; supported version range documented in `--help` and user guide |
| Legacy cosign bundles fail to verify because cosign embedded intermediate chain differs from Sigstore v0.3 bundle shape | M | H | Slice-2 verifier normalizes to the same `sigstore-rs` trust-chain call; acceptance-test fixtures include both cosign 2.x and cosign 3.x outputs |
| Flaky live Fulcio/Rekor in tests | H | H | Static cosign bundle test vectors; zero live Fulcio/Rekor calls in CI (inherited from Slice 1) |
| Registry rate-limit hits on Docker Hub | M | M | 24h CI cache TTL on the new referrer-index cache; `Retry-After` backoff; `--no-cache` documented as anti-pattern |
| `ocx install` implicit verification creeps in via well-meaning PR | L | H | Regression test + code-review checklist (carried from Slice 1) |
| Referrer-index cache corruption after interrupted write | L | M | Atomic write-to-temp-then-rename; SHA-256 self-check on read; corrupt entries treated as cache-miss |

### What does success look like?

| Timeframe | Metric | Target |
|-----------|--------|--------|
| Launch (Day 1) | `ocx verify` + `ocx sbom` shipped; documentation page live; all acceptance tests green | Yes |
| 30 days | First public GHA workflow using `ocx verify && ocx install` | ≥5 public workflows |
| 90 days | % of `ocx install` invocations in tracked OCX installs preceded by `ocx verify` | ≥10% |
| 1 year | Same | ≥20% |
| 1 year | Registry coverage reports (bug reports + successful verifications across all 10 tracked registries) | 9+ of 10 registries with successful external user reports |
| 1 year | Breaking changes to the v1 JSON schema | 0 (drives trust in `schema_version` stability) |

### What resources are required?

| Resource | Estimate | Notes |
|----------|----------|-------|
| Engineering | 1 engineer × 3 weeks (stub + test + impl + review); 1 week cushion for fixture tooling | Split across `/swarm-execute` workers |
| Design / UX | 0.5 day (text output review, JSON naming review) | Built into review loop |
| Documentation | 1 day for `signatures-and-sboms.md` + user-guide wiring | Part of Phase 4 |
| Infrastructure | None (reuses registry:2 docker-compose fixture) | Confirmed in research |
| External dependencies | `sigstore` `=0.13`, `sigstore-trust-root` `=0.6.4`, `cyclonedx-bom` `=0.8.1` | License review in `deny.toml` |

## Technical Questions

### Is this technically feasible?

Yes — with one explicit trade-off. Cosign keyless verification via `sigstore-rs` is production-ready; Notation has no Rust library, so v1 ships discovery-only for Notation-format referrers. DSSE attestation verification is unsupported by `sigstore-rs` v0.13.0 and is discovery-only in v1. Both are documented limitations with clean forward-compatibility paths.

### What are the technical dependencies?

- `sigstore = "=0.13"` (pinned; pre-1.0 churn)
- `sigstore-trust-root = "=0.6.4"`
- `cyclonedx-bom = "=0.8.1"` (pass-through only in v1)
- Patched `oci-client` (already has `pull_referrers` at `external/rust-oci-client/src/client.rs:1659`)
- registry:2 via docker-compose (existing fixture)
- `cosign` / `oras` / `syft` CLIs via OCX dogfood mirror (for fixture prep; tests skip with clear reason if absent)

### What's the estimated timeline?

| Phase | Duration | Deliverable |
|-------|----------|-------------|
| Phase 1 — Stubs | 2 days | Compiling stub tree; `cargo check` green |
| Phase 2 — Architecture review | 0.5 day | Reviewer sign-off on stubs vs. ADR |
| Phase 3 — Specification tests | 4 days | Failing unit + acceptance tests |
| Phase 4 — Implementation | 6 days | Tests green; `task verify` passes |
| Phase 5 — Review-fix loop | 2 days | Tier-max loop convergence + Codex cross-model pass |
| Documentation | 1 day | User-guide page + changelog |

Total: ~3 weeks end-to-end with a 1-week cushion for fixture tooling and review-fix iterations.

---

# EXTERNAL FAQ

## Customer Questions

### What is `ocx verify`?

`ocx verify <reference>` enforces signature validation on OCX packages. Shipped in Slice 1 for Sigstore bundle v0.3 (OCX-native); extended in Slice 2 to also accept legacy cosign tag-based signatures (`sha256-<digest>.sig` tag + `application/vnd.dev.cosign.artifact.sig.v1+json`) written by cosign 2.x or cosign 3.x against any OCI registry. Both formats are verified against the same Fulcio + Rekor trust chain with the same `--certificate-identity` and `--certificate-oidc-issuer` match requirements. Exit codes follow BSD sysexits.h for scriptability; no `skip` mode exists.

### What is `ocx sbom`?

`ocx sbom <reference>` (new in Slice 2) discovers SBOM referrers attached to the per-platform manifest of an OCX package and parses them into a stable JSON summary (`schema_version: 1`). Supported formats: CycloneDX 1.3–1.5 (JSON) and SPDX 2.3 (JSON). With `--download <path>`, it writes the raw SBOM bytes to a file; `--download -` streams to stdout for pipe composition with a vulnerability scanner or compliance report generator. `ocx sbom` is discovery + parsing only — it does not verify signatures on the SBOM itself (that's `ocx verify`'s job).

### Who is this for?

Primarily: authors of CI/CD pipelines, Bazel rules, devcontainer features, and Python orchestration scripts who consume OCX packages and must prove the provenance and contents of each binary. Secondarily: platform engineers running internal OCX distribution on private registries who need a single, native tool for verification. Not aimed at interactive end-users typing commands at a terminal — OCX remains a backend tool.

### How much does it cost?

| Tier | Price | Includes |
|------|-------|----------|
| OCX (all editions) | Free / open source | `ocx verify`, `ocx sbom`, full OCX CLI |
| Infrastructure | Zero OCX infrastructure cost | Uses your existing OCI registry; cosign keyless uses the public Sigstore production root; no subscriptions |

### How do I get started?

1. Install OCX (`curl -fsSL get.ocx.sh | bash` or via `cargo install`).
2. Verify any OCX package (including ones signed with cosign from any vendor): `ocx verify --certificate-identity <CI identity URI> --certificate-oidc-issuer <OIDC issuer> ghcr.io/your-org/your-tool:v1.0`.
3. Fetch an SBOM: `ocx sbom ghcr.io/your-org/your-tool:v1.0 --download sbom.json`.
4. Guard CI installs: `ocx verify --certificate-identity ... --certificate-oidc-issuer ... pkg:v1 && ocx install pkg:v1` — `ocx verify` exits non-zero on any verification failure, so `&&` blocks the install.

### What makes this different from `cosign verify` or `oras discover`?

- **Single identifier format.** `cosign verify` and `oras discover` speak container-image identifier format; they don't know about OCX package references. `ocx verify pkg:1.0` and `ocx sbom pkg:1.0` work exactly like `ocx install pkg:1.0` — same reference, same resolver, same auth.
- **Stable, versioned JSON.** `ocx verify --format json` and `ocx sbom --format json` both emit `schema_version: 1`. We commit to not breaking it without a schema bump. Peer tools have historically shipped NDJSON or unstable output.
- **Works on GHCR and Docker Hub.** Auto-fallback to the `sha256-<digest>` tag scheme covers the two registries that still lack the Referrers API in 2026. Users don't have to know. Both the capability cache (Slice 1) and the referrer-index cache (Slice 2) persist the probe result.
- **Offline-first.** `ocx verify --offline` and `ocx sbom --offline` succeed from populated caches. No peer tool offers this.
- **Typed exit codes.** Standard BSD sysexits.h mapping (0 success, 64 usage, 65 data, 69 unavailable, 74 I/O, 75 tempfail, 77 noperm, 79 not-found, 80 auth, 81 offline-blocked, 82 Rekor-unavailable, 83 referrers-unsupported) lets scripts branch on specific failure reasons without parsing stderr. **Slice 2 adds zero new exit-code variants** — every code above is defined by Slice 1's enum; Slice 2 only maps new `error_kind` strings onto those existing codes (Architect F6).
- **Native Rust verification.** No subprocess `cosign` invocation — `ocx` is still a single Rust binary with no runtime dependencies.
- **Two verified signature formats, one command.** `ocx verify` transparently accepts Sigstore bundle v0.3 AND legacy cosign tag-based signatures; downstream scripts do not branch on format.

### What signature formats does this release verify?

After Slice 2 ships, `ocx verify` fully verifies:

- **Sigstore bundle v0.3** (`application/vnd.dev.sigstore.bundle.v0.3+json`) — shipped in Slice 1; keyless cosign via `sigstore = "=0.13"`; Fulcio cert chain + Rekor SET + identity + issuer match.
- **Legacy cosign tag-based** (`sha256-<digest>.sig` tag + `application/vnd.dev.cosign.artifact.sig.v1+json`) — shipped in Slice 2; same trust root, same `--certificate-identity` + `--certificate-oidc-issuer` requirement, same Rekor SET enforcement.

Discovered but **not** verified (out of scope):

- **Notation (JWS)** — no Rust library as of April 2026. `ocx verify` does not list Notation referrers in the verification result; users wanting Notation run `notation verify` out-of-band.
- **DSSE attestations** (`application/vnd.dsse.envelope.v1+json`) — sigstore-rs 0.13 gap. SLSA provenance, VEX, deployment annotations fall here. Waiting for sigstore-rs 0.14+.
- **GitHub Actions attestations on GHCR** — `actions/attest-build-provenance` writes to GitHub's REST Attestations API, not the OCI Referrers graph. Use `gh attestation verify`.

`ocx verify` JSON output includes `signature_format: "sigstore_bundle_v0_3" | "cosign_legacy_v1"` on every entry, so audit pipelines can distinguish the two verified paths. There is no `skip` mode in either slice — verification always enforces.

### Does it support my registry?

Yes, including the two problem registries. Supported registries (verified against acceptance tests and community reports):

- **Referrers API path:** Harbor, Quay, ECR, ACR, Artifactory, Zot, registry:2 3.0-beta, GitLab Container Registry 17+
- **Fallback-tag path:** GitHub Container Registry (ghcr.io), Docker Hub (docker.io)

Auto-probe result is persisted in the Slice-1 capability cache; there is no `--distribution-spec` override in Slice 2. If auto-probe gets confused by an unusual proxy configuration, clear the cache with `ocx verify --no-cache` or `ocx sbom --no-cache` and re-run.

### Is my data secure?

- No credentials are transmitted beyond your existing OCI registry authentication (inherited via the same OCX auth chain as `ocx install`).
- Cosign keyless verification uses the embedded Sigstore production TUF root — verifiable and versioned (shipped in Slice 1, reused unchanged in Slice 2).
- `ocx verify` writes the referrer-index cache (JSON) to `~/.ocx/blobs/<registry>/.referrers/<repo>/<subject-digest>.json` and manifest blobs to the existing content-addressed `~/.ocx/blobs/` store. No secrets written.
- Slice 2 adds no new trust-material inputs. There is no trust-policy TOML in this release — see the Slice-1 PR-FAQ for the flag-based verification surface.

### What if I need help?

- User guide: [`website/src/docs/signatures-and-sboms.md`](../../website/src/docs/signatures-and-sboms.md).
- GitHub issues: https://github.com/ocx-sh/ocx/issues
- Discussion board: https://github.com/ocx-sh/ocx/discussions

### Will `ocx install` start verifying automatically in a future release?

It might — but not by silent default and not via file-presence side-effects. A future release may add an explicit `ocx install --verify` flag defaulting to false, then (with notice) flip the default. Existing `ocx install` workflows will not break without warning. See the Slice-1 ADR Decision D3 for the reasoning (unchanged in Slice 2).

### What if a verification fails?

`ocx verify` exits non-zero with a typed BSD sysexits.h code describing the cause — exit 65 when the cert chain or referrer manifest is malformed; exit 80 when `--certificate-identity` or the OIDC issuer does not match the signing cert; exit 82 when the Rekor transparency log is unavailable; exit 79 when no signatures are found. For the full exit-code table see the user guide. The JSON output includes a `verification.reason` field with the machine-readable failure cause. Scripts branch on `$?` without parsing stderr. There is no `skip` mode in Slice 1 or Slice 2; verification is always enforcing.

### Is there a trust-policy file?

Not in Slice 1 or Slice 2. Verification input is explicit flags (`--certificate-identity`, `--certificate-oidc-issuer`, `--trust-root`, `--offline`, `--no-cache`), same as `cosign verify`. Exit codes 78 (`ConfigError`) and 79 (`NotFound for specified-missing file`) remain reserved for the trust-policy-TOML release in v3+; they are live in Slice 2 for other flag-validation paths (missing `--download` destination directory, missing referrer blob in offline mode, empty SBOM component list).

### What is the performance cost?

- **Cold cache:** One or two HTTP round trips for verify (one capability probe, one referrer index fetch), plus 1–N additional manifest fetches on the fallback path for Cosign #4641 defensive classification. `ocx sbom` adds one SBOM blob fetch (typically 8–500 KB). Target latency p50 < 300ms for verify, p50 < 500ms for sbom.
- **Warm cache:** Read from `~/.ocx/blobs/<registry>/.referrers/...` — target p50 < 50ms for verify, p50 < 100ms for sbom (SBOM parse dominates).
- **Offline:** Zero network. Succeeds from populated cache; cold cache exits 81 (`OfflineBlocked`).
- **Binary-size impact (Slice 2 delta):** ≤ +1.5 MB over Slice-1 `ocx` (from `cyclonedx-bom` + `spdx-rs`). Slice 1's `sigstore-rs` footprint is reused unchanged.

---

## Appendix

### Customer Research

- Issue #24 thread: OCX maintainers and early adopters asking for a native verify command.
- Peer-tool wart catalog in [`research_verify_cli_patterns.md`](./research_verify_cli_patterns.md) — what we explicitly avoid reproducing.
- Registry compatibility matrix in [`research_oci_referrers_2026.md`](./research_oci_referrers_2026.md) — which of our users' registries work out-of-the-box vs. via fallback.

### Mockups / Visuals

Text output (`ocx verify`, post-Slice-2, mixed-format example):

```
$ ocx verify --certificate-identity 'https://github.com/example/ci/.github/workflows/release.yml@refs/tags/v3.28' \
             --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
             ghcr.io/example/cmake:3.28
Reference:   ghcr.io/example/cmake:3.28
Resolved:    sha256:aaaa... (linux/amd64)
Subject:     sha256:bbbb... (per-platform manifest)
Discovery:   referrers-tag (auto-probe → fallback; defensive per-manifest classify)

Signatures verified: 2

  [1] sha256:cccc... (1423 B)
      format: sigstore_bundle_v0_3
      discovery: referrers_api  (Slice 1)
      result: VERIFIED (cert identity match + Rekor SET)

  [2] sha256:eeee... (2104 B)
      format: cosign_legacy_v1
      discovery: sha256_tag    (Slice 2)
      result: VERIFIED (cert identity match + Rekor SET)

Other referrers (not verified):
  [3] sha256:dddd... application/vnd.cyclonedx+json (SBOM — see `ocx sbom`)
```

Text output (`ocx sbom`, new in Slice 2):

```
$ ocx sbom ghcr.io/example/cmake:3.28
Reference:   ghcr.io/example/cmake:3.28
Resolved:    sha256:aaaa... (linux/amd64)
Subject:     sha256:bbbb... (per-platform manifest)
SBOM:        sha256:dddd... (8192 B)  format: cyclonedx-json  spec: 1.5  tool: syft 1.14
Root:        pkg:generic/cmake@3.28.3
Components:  412
Licenses:    BSD-3-Clause (187)  MIT (142)  Apache-2.0 (61)  ...
```

JSON shape (`ocx verify --format json`, abbreviated — see ADR v2 §JSON Output Schema for canonical wire format):

```json
{
  "schema_version": 1,
  "reference": "ghcr.io/example/cmake:3.28",
  "resolved_digest": "sha256:aaaa...",
  "subject_digest": "sha256:bbbb...",
  "platform": "linux/amd64",
  "discovery_method": "referrers-tag",
  "signatures": [
    { "digest": "sha256:cccc...", "signature_format": "sigstore_bundle_v0_3", "discovery_method": "referrers_api", "verification": { "result": "verified", "certificate_identity": "https://github.com/...", "certificate_oidc_issuer": "https://token.actions.githubusercontent.com", "rekor_inclusion": "verified" } },
    { "digest": "sha256:eeee...", "signature_format": "cosign_legacy_v1", "discovery_method": "sha256_tag", "verification": { "result": "verified", "certificate_identity": "https://github.com/...", "certificate_oidc_issuer": "https://token.actions.githubusercontent.com", "rekor_inclusion": "verified" } }
  ]
}
```

JSON shape (`ocx sbom --format json`, abbreviated):

```json
{
  "schema_version": 1,
  "reference": "ghcr.io/example/cmake:3.28",
  "resolved_digest": "sha256:aaaa...",
  "subject_digest": "sha256:bbbb...",
  "platform": "linux/amd64",
  "sbom": {
    "digest": "sha256:dddd...",
    "format": "cyclonedx-json",
    "spec_version": "1.5",
    "tool": { "name": "syft", "version": "1.14" },
    "root_component": "pkg:generic/cmake@3.28.3",
    "component_count": 412,
    "license_histogram": { "BSD-3-Clause": 187, "MIT": 142, "Apache-2.0": 61 }
  }
}
```

`signature_format` is a flat discriminant string: `sigstore_bundle_v0_3` (Slice 1) or `cosign_legacy_v1` (Slice 2). `verification.result` is always `verified` on a successful `ocx verify` exit (non-zero exit on any failure); there is no `skip` mode. `ocx sbom.sbom.format` is `cyclonedx-json` or `spdx-json`.

---

## Approval

| Role | Name | Date | Decision |
|------|------|------|----------|
| Product | | | Pending |
| Engineering | | | Pending |
| Leadership | | | Pending |

---

## Next Steps

After Slice-2 PR-FAQ approval:

1. [x] Slice-1 shipped (sign + verify with Sigstore bundle v0.3; see [`pr_faq_oci_referrers_signing_v1.md`](./pr_faq_oci_referrers_signing_v1.md))
2. [x] Slice-2 PRD amended ([`prd_oci_referrers_discovery.md`](./prd_oci_referrers_discovery.md) Version 2.0)
3. [x] Slice-2 ADR drafted ([`adr_oci_referrers_discovery_v2.md`](./adr_oci_referrers_discovery_v2.md); original [`adr_oci_referrers_discovery.md`](./adr_oci_referrers_discovery.md) marked Superseded)
4. [x] Slice-2 implementation plan drafted ([`../state/plans/plan_slice2_external_discovery.md`](../state/plans/plan_slice2_external_discovery.md))
5. [ ] Phase 4 `/swarm-execute max 24` — contract-first TDD execution for Slice 2
