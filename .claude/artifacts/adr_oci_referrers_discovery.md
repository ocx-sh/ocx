# ADR: OCI Referrers Discovery for `ocx verify` and `ocx sbom`

## Metadata

**Status:** Superseded by [`adr_oci_referrers_discovery_v2.md`](./adr_oci_referrers_discovery_v2.md) (2026-04-19)
**Date:** 2026-04-19
**Deciders:** mherwig (architect), Phase 4 review panel (pending)
**GitHub Issue:** [#24 — feat: OCI referrers API for signature and SBOM discovery](https://github.com/ocx-sh/ocx/issues/24)
**Related PRD:** [`prd_oci_referrers_discovery.md`](./prd_oci_referrers_discovery.md) (amended for Slice 2 scope; see `adr_oci_referrers_discovery_v2.md` for the superseding design)
**Related PR-FAQ:** [`pr_faq_oci_referrers_discovery.md`](./pr_faq_oci_referrers_discovery.md) (amended for Slice 2 scope)
**Implementation Plan:** [`../state/plans/plan_slice2_external_discovery.md`](../state/plans/plan_slice2_external_discovery.md) (replaces the original `plan_oci_referrers_discovery.md`; the single-track plan never shipped)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, Tokio, `thiserror` / `anyhow`, BSD sysexits exit codes)
**Domain Tags:** oci, cli, security, supply-chain
**Supersedes:** N/A
**Superseded By:** [`adr_oci_referrers_discovery_v2.md`](./adr_oci_referrers_discovery_v2.md) — split into Slice 1 (sign + verify via Slice-1 ADR) and Slice 2 (external discovery + SBOM). The `level = "skip"` trust-policy half-product that this ADR shipped was rejected by the user on 2026-04-18; see v2 §"What changed from the superseded ADR" for the decision deltas.

> **Historical record only.** This ADR is retained for provenance. Do not implement against it. The active design is in `adr_oci_referrers_signing_v1.md` (Slice 1) + `adr_oci_referrers_discovery_v2.md` (Slice 2).

## Relationship to Parent ADR

This ADR **extends**, does not supersede, [`adr_oci_artifact_enrichment.md`](./adr_oci_artifact_enrichment.md) (2026-03-12, Proposed). That parent ADR proposes the hybrid mechanism (annotations + `_desc` tag + referrers) and scopes 5 implementation phases; this ADR is the **read-side** of Phases 3–5 — specifically the consumer commands `ocx verify` and `ocx sbom`. The push-side (`ocx package sign` / `ocx package attest`) remains scoped to the parent ADR and is explicitly out of scope here.

Parent-ADR decisions this ADR inherits without re-opening:

- Media type `application/vnd.sh.ocx.signature.v1` (OCX signature wrapper) and `application/vnd.sh.ocx.sbom.v1` (OCX SBOM wrapper) as OCX-native referrer `artifactType` values.
- Subject-targeting rule: signatures and SBOMs `subject` field targets a **per-platform `ImageManifest` digest**, never an `ImageIndex` digest.
- The parent ADR's "no tag fallback" stance — which this ADR **splits** on the read path only, with rationale below.

Parent-ADR decisions this ADR **adjusts**:

- "No tag fallback" applies to the push path only. The read path (this ADR) implements tag-fallback reading. See §Decision Outcome → Open Question 3 and §Alternatives.

## Context

OCX v0 publishes multi-platform binary packages to OCI registries. Consumers today trust the registry's transport authentication and the immutability of content-addressed blobs — but they have no way to answer the two questions that modern supply-chain policies demand:

1. **Who published this binary?** (Signature verification)
2. **What is inside this binary?** (SBOM discovery)

The OCI Image Spec 1.1 (2024-02-15) and Distribution Spec 1.1 define a referrers graph: auxiliary manifests (signatures, SBOMs, attestations) can be attached to a subject by digest and discovered via `GET /v2/{name}/referrers/{digest}`. The ecosystem — cosign v3, syft, trivy, oras attach, Quay, Harbor, ECR, ACR, Artifactory, Zot, registry:2 — already writes referrers in this shape. OCX packages can be signed by `cosign sign` and have SBOMs attached by `syft attest` today, with zero OCX cooperation. What is missing is an **OCX-side consumer** that discovers those referrers in the flow where OCX users live: at install time, in CI, next to the package identifier.

Issue #24 asks for that consumer: two CLI subcommands — `ocx verify <ref>` and `ocx sbom <ref>` — that read the referrers graph on an OCX package and surface the results in a form usable by GitHub Actions, Bazel rules, and Python orchestration scripts.

The write-side (OCX signing, attesting, attaching SBOMs) is explicitly deferred. This ADR is only about reading.

## Decision Drivers

- **Supply-chain pressure is now table stakes.** EU Cyber Resilience Act effective dates and US Executive Order 14028 have pushed signatures + SBOMs from "nice-to-have" to compliance prerequisite for enterprise adoption. OCX's primary user — automation (GHA, Bazel, CI) — has already standardized on cosign for container images.
- **OCX is backend-first.** Exit codes, JSON output with schema versioning, offline-first behavior, and absence of interactive prompts are load-bearing. Machine consumption comes before human affordance.
- **CLI shapes are one-way doors.** `ocx verify` flag surfaces, JSON output schemas, exit-code mappings, and trust-policy shape are effectively frozen the moment a user scripts them into CI. Research (`research_verify_cli_patterns.md`) highlights four peer-tool warts (cosign NDJSON, cosign stdout/stderr mix, notation missing `--format json`, oras renaming `manifests` → `referrers`) that OCX must avoid from v1.
- **GHCR and Docker Hub have no Referrers API.** Research (`research_oci_referrers_2026.md`) confirms these two remain unsupported as of April 2026, with no public roadmap. Together they host the majority of open-source package traffic. A pure-API implementation silently misses all cosign-signed packages on GHCR — a show-stopper for usefulness.
- **Cosign v3 is the signing default in OCX's ecosystem.** The CNCF/Kubernetes toolchain (Kyverno, Flux, Policy Controller, GitHub Attestations) is cosign-native. Notation serves an enterprise-PKI niche whose users can run `notation verify` separately.
- **No production Rust library exists for Notation.** Research (`research_cosign_sigstore_notation.md`) confirms all Notary Project code is Go. Rust→Go FFI costs 8–10 MB binary size, a C compiler at build time, and async-runtime incompatibility. A pure-Rust rewrite is months. `sigstore-rs` v0.13.0 gives OCX a credible cosign-keyless verification path today.
- **Auto-verify on install is a trust-model decision.** Turning on signature verification inside `ocx install` would either (a) open OCX to friction the moment a registry lacks the Referrers API, or (b) silently pass unverified packages. Either default is wrong for some users. The decision must be explicit and not a hidden side-effect of a flag default.

## Industry Context & Research

**Research artifacts:**

- Tech axis: [`research_cosign_sigstore_notation.md`](./research_cosign_sigstore_notation.md) — `sigstore` Rust crate v0.13.0 (released October 2024; still latest as of April 2026), `sigstore-trust-root` v0.6.4, `cyclonedx-bom` v0.8.1; Notation Rust gap; DSSE attestation unsupported (verified current April 2026: no 0.14 release; DSSE remains unimplemented).
- Patterns axis: [`research_verify_cli_patterns.md`](./research_verify_cli_patterns.md) — cosign NDJSON wart, notation trust-policy shape, `oras discover` exit-on-empty, `--distribution-spec` escape hatch, `schema_version` discipline. **Footnote:** §6 registry support matrix in this patterns-axis artifact contains known-incorrect entries for GHCR, Docker Hub, and registry:2; the authoritative registry-capability source is `research_oci_referrers_2026.md` §2.
- Domain axis: [`research_oci_referrers_2026.md`](./research_oci_referrers_2026.md) — GHCR/Docker Hub still lack Referrers API; cosign bug #4641 (fallback-tag `artifactType` wrong); per-platform descent required; `OCI-Filters-Applied` is SHOULD not MUST (client re-filter required); registry capability matrix 8/10 major registries.

**Trending approaches (2026):**

- Referrers-as-primary, tags-as-fallback has converged as the industry default; `oras discover` auto-probes transparently.
- Trust-policy-as-file (Notation model) is winning over flag-soup identity assertions (cosign's 15+ `--certificate-*` flags). OCX adopts the file model.
- Discovery vs. enforcement is a design axis: discovery commands exit 0 on empty (oras), enforcement commands exit non-zero (cosign). OCX ships discovery with opt-in enforcement via `--require*` flags.

**Key insight:** The single highest-impact design choice in this ADR is **reading fallback tags on the consumer side** — without it, `ocx verify` returns "no signatures" on GHCR/Docker Hub, which are the two registries where most OCX users' packages live. The parent ADR's "no tag fallback" stance is correct for the push path (writer complexity, concurrent update races) but wrong on the read path (~50 lines of Rust for full ecosystem compatibility).

## Considered Options

This ADR records four one-way-door decisions. Each gets its own "Options Considered" section with at least three alternatives, trade-offs, and the chosen option with rationale.

### Decision A — Fallback-tag stance (read path vs. push path)

The parent ADR decided "no tag fallback." That was written in the push-path context. Does it apply to `ocx verify` / `ocx sbom` on the read path?

#### Options Considered

**Option A1 — Hold the line: no fallback on either path.**

- **Pros:**
  - Single, simple contract: "if your registry doesn't implement the Referrers API, supply-chain features don't work."
  - Zero additional code; zero bug surface.
  - Nudges ecosystem toward adopting the standard.
- **Cons:**
  - `ocx verify cmake:3.28` on any GHCR-hosted cosign-signed package silently returns empty — a failure mode indistinguishable from "no signature exists."
  - GHCR is the default registry for GitHub Actions-published packages (OCX's primary automation target).
  - Docker Hub has the same limitation. Combined, these two registries host the vast majority of public package traffic.
  - Users would install `oras discover` as a workaround anyway — OCX becomes the odd tool that doesn't know how to find signatures.

**Option A2 — Fallback on both paths (read and push).**

- **Pros:**
  - Maximum compatibility on both reading and writing.
- **Cons:**
  - Write-path fallback requires concurrent-write coordination (multiple writers racing to update the `sha256-{digest}` tag — acknowledged by the OCI spec).
  - Permanent second code path with its own bug surface (cosign bug #4641 is an example).
  - Doubles the OCX push-time complexity for a transitional shim.

**Option A3 — Split the stance: fallback on read, no fallback on push.** ← **CHOSEN**

- **Pros:**
  - The costs that justified "no fallback" in the parent ADR (concurrent writer races, cleanup, permanent write-path complexity) exist only on the push side.
  - Read-side fallback is a pure HTTP GET + parse. ~50 lines of Rust. No race conditions.
  - Compatibility with the entire cosign-signed ecosystem on GHCR and Docker Hub.
  - Future-proof: when GHCR adds Referrers API support, OCX auto-detects via probe-first behavior. No code change, no user action.
- **Cons:**
  - Asymmetric behavior between publishing and consuming; must be clearly documented.
  - Fallback path has a known bug (cosign #4641: wrong `artifactType` in fallback index entries). Requires defensive per-manifest classification (see Decision C).

**Decision: Option A3.** The parent ADR's cost calculus applies to the push path only. On the read path, the benefit (full ecosystem compatibility, no silent empty-result failures) clearly outweighs the cost (~50 lines + one known defensive-parse pattern). Research (`research_oci_referrers_2026.md`, §7) pre-answered this question with the same conclusion and documents the defensive-parse requirement.

### Decision B — Signature formats for v1

Which signature formats does `ocx verify` understand in v1? Cosign (keyless + key-based)? Notation? Both? A custom OCX format?

#### Options Considered

**Option B1 — Cosign keyless only (sigstore-rs).** ← **CHOSEN**

- **Pros:**
  - `sigstore` crate v0.13.0 + `sigstore-trust-root` v0.6.4 provide a credible pure-Rust verification path. Offline-capable via embedded TUF production root. ~49k monthly downloads, actively maintained by the Sigstore org.
  - Cosign is dominant in OCX's target user base (CI/CD, GitHub Actions, CNCF toolchain).
  - Cosign v3 (April 2026) defaults to OCI 1.1 referrer-based bundles — aligns with the OCX referrer architecture from the parent ADR.
  - Keyless verification is the path new users take in 2026; key-based is legacy.
- **Cons:**
  - `sigstore-rs` is pre-1.0 (API unstable; 10 breaking changes over 19 releases). Must pin `=0.13` and plan deliberate upgrades.
  - Does NOT yet support DSSE attestations (`cosign attest` envelopes). Attestation verification deferred to a future version.
  - Does not cover `cosign sign --key` scenarios with user-managed keys in v1. Addressed as an extension point.

**Option B2 — Cosign + Notation.**

- **Pros:**
  - Covers the enterprise-PKI niche (Azure ACR, AWS ECR users following Notation best-practice docs).
- **Cons:**
  - No Rust library for Notation exists as of April 2026. `notation-go` is Go-only.
  - Rust→Go FFI adds 8–10 MB to binary size, requires C compiler at build time (breaks musl cross-compilation), creates two GC heaps, Tokio/goroutine runtime conflict.
  - Pure-Rust rewrite of Notation trust store + certificate chain + revocation: multi-week effort with no upstream maintenance.
  - Notation adoption in OCX's target users (GHA, Bazel, Python scripts) is negligible.

**Option B3 — Custom OCX signature format (signs a digest, stores payload in referrer).**

- **Pros:**
  - Minimal dependencies (use `ring` or `ed25519-dalek`).
  - Full control of format and verification logic.
- **Cons:**
  - OCX becomes an island: no `cosign verify` interop, no Kubernetes policy controller interop, no reuse of Sigstore transparency log.
  - Users must learn a fourth signature format (cosign, Notation, in-toto, OCX).
  - Throws away the ecosystem's trust infrastructure (Fulcio, Rekor, TUF) — OCX would need to reinvent all of it.

**Option B4 — Defer signature verification entirely; v1 only discovers + downloads.**

- **Pros:**
  - Ship faster; validate whether users even want verification.
- **Cons:**
  - `ocx verify` that doesn't verify is a broken promise baked into the command name.
  - Exit-code semantics become meaningless ("signature present" vs. "signature valid" are different checks).
  - Trust-policy plumbing has to be added later as a breaking CLI surface change.

**Decision: Option B1.** Confirms the research pre-answer. Cosign keyless is the only viable format given Rust-library constraints and target-user ecosystem. The CLI surface reserves a `--format` flag (see §CLI Contracts) so Notation / custom keys can land in v2 without breaking changes.

Verified current April 2026: no sigstore-rs 0.14 release; DSSE remains unimplemented.

Forward-compatibility plan — dual-format detection by layer `mediaType`:

| Layer `mediaType` | v1 behavior | v2 extension |
|---|---|---|
| `application/vnd.dev.sigstore.bundle.v0.3+json` | Verify via `sigstore` crate | Already supported |
| `application/vnd.dev.cosign.artifact.sig.v1+json` | Classify as "legacy cosign"; discover only, no verify | Add legacy-format verify |
| `application/jose+json` (JWS) | Classify as "notation"; discover only, no verify | Add Notation verify |
| `application/vnd.dsse.envelope.v1+json` | Classify as DSSE; discover-only; no verify | Add DSSE verify (requires sigstore-rs ≥ 0.14 or equivalent) |
| Unknown | Display `mediaType` + size; do not attempt verify | — |

### Decision C — Defensive parsing for cosign fallback-tag bug #4641

On registries without the Referrers API (GHCR, Docker Hub), cosign writes a fallback `sha256-{digest}` tag containing an `OciImageIndex` of referrer descriptors. As of January 2026 (bug #4641, still open), cosign sets `artifactType: application/vnd.oci.empty.v1+json` on every descriptor in that fallback index instead of the real artifactType. Annotations are also absent from fallback descriptors.

If OCX filters by descriptor `artifactType` on the fallback path, every cosign signature on GHCR is silently missed.

#### Options Considered

**Option C1 — Trust the index descriptor `artifactType`; document the known-bad cases.**

- **Pros:**
  - Zero additional round trips.
- **Cons:**
  - Every cosign signature on GHCR and Docker Hub is silently filtered out. Defeats the purpose of Decision A3.
  - User has no visibility into why verification returned empty.

**Option C2 — On the fallback path, fetch every descriptor's manifest and classify by the manifest body's own `artifactType`.** ← **CHOSEN**

- **Pros:**
  - Robust against bug #4641.
  - Typical fallback index has 1–5 descriptors — the N+1 cost is bounded.
  - Gives OCX correct results today and is still correct when cosign fixes the bug (fetch is cheap and the manifest body `artifactType` is authoritative by spec).
- **Cons:**
  - N+1 round trips on the fallback path.
  - Must be implemented carefully to avoid fetching the same manifest twice (cache by digest within the same command invocation).

**Option C3 — Use the Referrers API path only; never fall back.**

- **Pros:**
  - Sidesteps the bug entirely.
- **Cons:**
  - This is Option A1 in disguise — loses GHCR/Docker Hub compatibility.

**Option C4 — Pin cosign version requirement; refuse to read fallback tags written by older cosign.**

- **Pros:**
  - Avoids the bug post-fix.
- **Cons:**
  - OCX cannot tell which cosign version wrote a given tag without fetching the manifest anyway.
  - Pre-fix packages are already in the wild and must be handled.

**Decision: Option C2.** Document the defensive parse in the plan; lock it in with an acceptance test that populates a registry:2 fallback tag with a deliberately-wrong `artifactType` on the index descriptor and verifies OCX classifies it correctly by descending into the manifest. When cosign fixes the bug, no OCX change needed — the manifest body has always been authoritative.

### Decision D — Auto-verify during `ocx install`

Should `ocx install` automatically verify signatures when a trust policy is configured? Or must the user explicitly run `ocx verify` first? This is an open question from the issue that the research axes deliberately did not pre-answer — it is a pure product/trust-model decision.

#### Options Considered

**Option D1 — Auto-verify always, fail-closed.**

- **Pros:**
  - Strongest default security posture.
- **Cons:**
  - On registries without Referrers API, every `ocx install` returns "no signatures found, blocking install" — a breaking change for the entire GHCR/Docker Hub user base on day one.
  - Forces every OCX user to configure a trust policy before they can install anything, even from public mirrors where they have no policy to assert.
  - Trust-on-first-use is not viable for automation (no prompts).

**Option D2 — Auto-verify when a trust policy is configured, otherwise skip.**

- **Pros:**
  - Opt-in via presence of `$OCX_HOME/trust-policy.toml`.
  - Zero friction for users who haven't opted in.
- **Cons:**
  - The action is triggered by file presence, which is an invisible side-effect. A user dropping in a trust-policy file starts gating every install, potentially breaking CI with no command-line change.
  - Mixed behavior: same command, different behavior depending on filesystem state. Reproducibility is degraded.
  - Still requires the policy schema to ship in v1 even though v1 has no verification levels.

**Option D3 — Auto-verify NEVER in v1; require explicit `ocx verify`. Policy configuration is wired but takes effect only when called from `ocx verify` / `ocx sbom` directly.** ← **CHOSEN**

- **Pros:**
  - Command behavior is fully determined by CLI args; no filesystem side-effects gate `ocx install`.
  - Aligns with OCX's backend-first principle: callers (GHA, Bazel) compose `ocx verify && ocx install` explicitly. The `&&` is the policy decision.
  - No breaking change on GHCR/Docker Hub: existing `ocx install` workflows are untouched.
  - Keeps v1 scope tight. Auto-verify is a feature; it can land in v2 with a dedicated `install --verify` flag that defaults to false, then a config option to change the default per profile.
  - Does not foreclose v2 auto-verify — only foreclosures would be (a) making verify a side-effect of a file path, or (b) changing default install behavior silently.
- **Cons:**
  - Users who want always-on verification must wrap `ocx install` in a script. Mitigated by providing a canonical snippet in documentation.
  - Requires OCX users to learn a two-step pattern (`verify` then `install`). Acceptable — they are automation builders, not interactive users.

**Option D4 — Ship a `--verify` flag on `ocx install`, default false; ADR defers default change.**

- **Pros:**
  - Gives users who want auto-verify an explicit path without adding the file-presence side-effect.
  - Can flip the default in a future release.
- **Cons:**
  - Expands v1 scope — `ocx install` gains a new responsibility (referrer discovery, trust-policy resolution, failure path handling) that otherwise lives purely in `ocx verify`.
  - Risks scope creep (install-time verification wants install-time policy → install-time caching → install-time network retries).

**Decision: Option D3.** Keep `ocx install` untouched. v1 ships `ocx verify` and `ocx sbom` as separate commands only. The trust-policy file schema is wired in v1 (see §Trust Policy Shape) but read only by `ocx verify`. An explicit `--verify` flag on `ocx install` is tracked as a v2 extension that will not need a breaking change on the verify command surface.

Rationale for counter-claiming Option D4 (the "moderate" middle ground): auto-verify-as-install-side-effect, even when opt-in via flag, expands the install command's responsibility surface at the moment we are finalizing the verify surface. Decoupling them in v1 lets us ship the verify command on its own merits and learn from real usage before coupling it to install. This is the `/keep it simple` + `/ship it` principle from `quality-core.md`.

## Decision Outcome

**Chosen options:** A3 (split fallback stance: read yes, write no), B1 (cosign keyless only via `sigstore` crate), C2 (defensive per-manifest classification on fallback path), D3 (no auto-verify during install in v1).

### Resolutions of the three issue-open-questions

**Q1 — Should `ocx install` auto-verify signatures if a policy is configured?**

No, not in v1. `ocx install` remains behaviorally identical to today. Verification is an explicit `ocx verify <ref>` invocation. Rationale under Decision D.

**Q2 — Which signature formats ship first — cosign keyless, notation, or both?**

Cosign keyless only. `sigstore` crate v0.13.0 is the only production Rust path; Notation has no Rust library and Rust→Go FFI is prohibitive for a standalone binary. Rationale under Decision B.

**Q3 — Registries without Referrers API — revisit "no fallback" or hold?**

Split the stance: hold on push, revisit on read. The read path implements `sha256-{digest}` tag fallback with defensive per-manifest classification (Decision C) to survive cosign bug #4641. The parent ADR's "no fallback" remains in effect for the push path. Rationale under Decision A.

### CLI Contracts

Both commands use the OCX-standard flag-before-positional ordering per `MEMORY.md::feedback_flags_before_args.md`.

Output format: the `--format <text|json>` flag is a global flag provided by `ContextOptions` / `CommandContext` (see `subsystem-cli.md`). Per-command flag tables below intentionally omit it — it is inherited, not redefined.

```
ocx verify [flags] <reference>

Flags:
  --artifact-type <MEDIA_TYPE>           Filter to a single OCI artifact type.
                                         Default (omitted): all signature types.
                                         Empty string ("") is UsageError (64).
  --require-referrers                    Fail (exit 65) if zero referrers found.
  --min-referrers <N>                    Fail (exit 65) if fewer than N referrers found.
                                         N=0 is accepted and is a no-op (equivalent to omitting).
                                         Non-numeric N is UsageError (64) via clap.
  --require-artifact-type <MEDIA_TYPE>   Repeatable. Fail (exit 65) if any listed type absent.
                                         Empty string ("") is UsageError (64) with message
                                         "artifact-type must be a non-empty media type string".
  --distribution-spec <SPEC>             Force referrers strategy.
                                         Values: v1.1-referrers-api | v1.1-referrers-tag
                                         Default: auto-probe (API first, tag fallback on 404/405).
  --trust-policy <PATH>                  Path to trust-policy TOML. Defaults to
                                         $OCX_HOME/trust-policy.toml when present.
                                         v1: read but verification level defaults to "skip"
                                         unless explicitly set in the policy file.
                                         If <PATH> is explicitly provided and the file does
                                         not exist, exit 79 (NotFound) with message
                                         "trust policy file not found: <PATH>".
  --no-cache                             Bypass cached referrer index for this invocation.
  --platform <OS/ARCH>                   Override auto-detected platform when the reference
                                         is an ImageIndex (multi-platform) and the user wants
                                         verification of a non-host-matching platform.

Positional:
  <reference>                            OCI reference (tag or digest form). Required.

Combination semantics for --require-* flags:

When `--require-referrers`, `--min-referrers N`, and one or more
`--require-artifact-type TYPE` are specified together, all checks are evaluated
against the final (filtered) referrer set. The exit code is 65 for any unmet
check. If multiple checks fail, stderr lists all failures; the `require_checks`
JSON object marks each as true/false independently. Stderr error message
ordering: `require_referrers` first, then `min_referrers`, then each
`require_artifact_type` in the order provided.

Exit codes:
  0   Success (at least one referrer found, OR zero referrers with no --require flags)
  64  UsageError — unknown flag, bad reference syntax
  65  DataError — malformed referrer index, OR --require* unmet
  69  Unavailable — registry unreachable, 5xx, forced --distribution-spec not supported
  74  IoError — local cache read/write failure
  75  TempFail — 429 rate limit after retry backoff exhausted
  77  PermissionDenied — 403
  79  NotFound — reference does not resolve (repo or tag absent);
                  also: --trust-policy <PATH> explicitly given but file missing
  80  AuthError — 401
  81  OfflineBlocked — operation requires network but --offline set

Exit-code precedence (when multiple failure categories would otherwise apply):

Transport errors (69, 75, 79, 80, 81) take precedence over require-check
failures (65). A require-check failure is only emitted when discovery
succeeds but the result set is empty or below threshold. Concretely:

- --distribution-spec v1.1-referrers-api + --require-referrers + registry 404
  on the Referrers API → exit 69 (ReferrersUnsupported), NOT 65.
- Registry 500 + --require-referrers → exit 69 (Unavailable), NOT 65.
- Registry 401 + --require-referrers → exit 80 (AuthError), NOT 65.
- --trust-policy <PATH> given, file missing → exit 79 (NotFound), NOT 78.

UsageError (64) precedes all others: invalid flag values are rejected by
clap before discovery runs.
```

```
ocx sbom [flags] <reference>

Flags:
  --download <PATH>                      Download the selected SBOM content to this path.
                                         Writes raw bytes to stdout if PATH is "-".
                                         When PATH is "-", the JSON/text report is NOT emitted
                                         (not to stdout, not to stderr). Exit codes unchanged.
  --artifact-type <MEDIA_TYPE>           Filter to a specific SBOM artifact type.
                                         Default: all known SBOM types
                                         (application/vnd.cyclonedx+json,
                                          application/spdx+json,
                                          application/vnd.in-toto+json,
                                          application/vnd.sh.ocx.sbom.v1).
  --require                              Fail (exit 65) if no SBOM referrers found.
  --distribution-spec <SPEC>             Same as ocx verify.
  --trust-policy <PATH>                  Same as ocx verify. v1: policy schema accepted but
                                         does not gate SBOM discovery.
  --no-cache                             Bypass cached referrer index.
  --platform <OS/ARCH>                   Same as ocx verify.
  --preference <PRIORITY,...>            Comma-separated artifact_type priority when --download
                                         must pick one SBOM of several. Default:
                                         application/vnd.cyclonedx+json,
                                         application/spdx+json,
                                         application/vnd.in-toto+json.

Positional:
  <reference>                            OCI reference. Required.

Exit codes: same mapping as ocx verify. --require maps to 65 when zero SBOMs found.

CycloneDX 1.6 gap: v1 passes SBOM bytes through unchanged. The pinned
`cyclonedx-bom = "=0.8.1"` crate does not parse CycloneDX 1.6 (released
March 2024); users encountering 1.6 SBOMs receive opaque pass-through only.
EU CRA compliance tooling (BSI TR-03183-2) requires CycloneDX 1.6+ or
SPDX 3.0.1+ — users needing strict schema validation must wrap `ocx sbom
--download` with an external validator until v2 lands native parsing.
```

### JSON Output Schema (v1)

Every JSON payload has `schema_version: 1` at the root. Downstream consumers gate on this. Future breaking changes increment the integer.

`ocx verify --format json` emits:

```json
{
  "schema_version": 1,
  "reference": "ghcr.io/example/cmake:3.28",
  "resolved_digest": "sha256:aaaa...",
  "subject_digest": "sha256:bbbb...",
  "platform": "linux/amd64",
  "registry_method": "referrers-api",
  "referrer_count": 2,
  "referrers": [
    {
      "digest": "sha256:cccc...",
      "artifact_type": "application/vnd.dev.sigstore.bundle.v0.3+json",
      "media_type": "application/vnd.oci.image.manifest.v1+json",
      "size": 1423,
      "annotations": { "dev.sigstore.cosign/signature": "MEYCIQD..." },
      "verification": {
        "attempted": true,
        "result": "skip",
        "reason": "trust policy level 'skip' (v1 default)"
      }
    },
    {
      "digest": "sha256:dddd...",
      "artifact_type": "application/vnd.in-toto+json",
      "media_type": "application/vnd.oci.image.manifest.v1+json",
      "size": 8192,
      "annotations": { "in-toto.io/predicate-type": "https://spdx.dev/Document" },
      "verification": { "attempted": false, "result": "not_a_signature", "reason": null }
    }
  ],
  "require_checks": {
    "require_referrers": false,
    "min_referrers": null,
    "require_artifact_type": []
  }
}
```

`resolved_digest` is the digest of the reference the user asked about (tag-resolved at `ImageIndex` level if applicable). `subject_digest` is the per-platform `ImageManifest` digest that was used to query referrers (may equal `resolved_digest` if the input was already a platform manifest or if the package is `Platform::any()`).

`ocx sbom --format json` emits the same shape with `sboms` replacing `referrers`, plus a `downloaded_to` field.

Canonical schema note: this §JSON Output Schema section is authoritative. Where the PR-FAQ or user-guide include abbreviated JSON mockups, the ADR shape wins on any conflict.

**Canonical type shapes:**

- `classification` is a flat discriminant string with the form `<kind>:<format>` where `<kind>` ∈ `{signature, sbom, in-toto, unknown}`. Full enum of v1 valid values:
  - `signature:cosign-bundle-v03`
  - `signature:cosign-legacy`
  - `signature:notation`
  - `signature:dsse`
  - `sbom:cyclonedx-json`
  - `sbom:spdx-json`
  - `sbom:in-toto-envelope`
  - `sbom:ocx-native`
  - `in-toto` (non-SBOM in-toto predicates)
  - `unknown`
- `verification` is a nested object with required fields `attempted: bool`, `result: "skip" | "passed" | "failed" | "not_applicable"`, and `reason: string | null`.

PR-FAQ JSON mockups are abbreviated for readability (they may omit `attempted` in `verification`); the ADR schema is canonical for the wire format.

Empty-result example for `ocx verify` (still exit 0 without `--require*`):

```json
{
  "schema_version": 1,
  "reference": "ghcr.io/example/cmake:3.28",
  "resolved_digest": "sha256:aaaa...",
  "subject_digest": "sha256:bbbb...",
  "platform": "linux/amd64",
  "registry_method": "referrers-tag",
  "referrer_count": 0,
  "referrers": [],
  "require_checks": { "require_referrers": false, "min_referrers": null, "require_artifact_type": [] }
}
```

Empty-result example for `ocx sbom` (no `--download` flag):

```json
{
  "schema_version": 1,
  "reference": "ghcr.io/example/cmake:3.28",
  "resolved_digest": "sha256:aaaa...",
  "subject_digest": "sha256:bbbb...",
  "platform": "linux/amd64",
  "registry_method": "referrers-tag",
  "sbom_count": 0,
  "sboms": [],
  "downloaded_to": null,
  "require_check": false
}
```

`downloaded_to` is `null` (not omitted) when `--download` is not specified.

### Text Output

Plain output is intended for human inspection. All structured data goes to stdout; diagnostics (warnings, trace logs) go to stderr. Never interleave.

```
$ ocx verify ghcr.io/example/cmake:3.28
Reference:   ghcr.io/example/cmake:3.28
Resolved:    sha256:aaaa... (linux/amd64)
Subject:     sha256:bbbb... (per-platform manifest)
Method:      referrers-api (or: referrers-tag with defensive classify)

Referrers found: 2

  [1] sha256:cccc... (1423 B)
      artifactType: application/vnd.dev.sigstore.bundle.v0.3+json
      Classification: cosign sigstore bundle v0.3
      Verification: skip (trust-policy level 'skip' — discovery only in v1)

  [2] sha256:dddd... (8192 B)
      artifactType: application/vnd.in-toto+json
      predicate-type: https://spdx.dev/Document
      Classification: in-toto SBOM attestation (not a signature, not verified)
```

### Trust Policy Shape (v1 minimal, v2 forward-compatible)

v1 ships a deliberately-minimal TOML schema. Only `version`, named policies with `registry_scopes`, and a single enforcement field (`level = "skip"`) are defined. **All enforcement-specific fields (trust stores, trusted identities, OIDC issuers, Rekor URL, etc.) are explicitly TBD for v2** — keeping them out of v1 avoids baking in field names before the trust model is finalized. Notation uses certificate-based trust stores; cosign keyless uses OIDC issuer + subject pattern + Rekor inclusion proof. The v2 schema will pick the right vocabulary for the format being enforced. v1 does not pre-commit to either.

File location: `$OCX_HOME/trust-policy.toml` (overridable via `--trust-policy <PATH>`).

```toml
# $OCX_HOME/trust-policy.toml  (v1 — minimal; all enforcement fields are v2 TBD)

version = "1"

[[policies]]
name = "default"
registry_scopes = ["*"]

[policies.signature_verification]
level = "skip"   # v1 supports: "skip". v2 will add: "strict" | "permissive" | "audit"
# NOTE: v1 defines NO enforcement fields beyond `level`. Any additional key
# in this table is parsed but ignored with a warning; it will NOT map to a
# v2 field of the same name. v2 enforcement-field vocabulary is deliberately
# left open so it can fit the actual signature format being verified
# (cosign keyless: oidc_issuers, subject_regex, rekor_url;
#  notation: trust_stores, trusted_identities, certificate_subjects;
#  cosign key-based: public_keys). Do NOT assume v1 field names forward.
```

In v1:
- If the file is absent OR `level = "skip"`, `ocx verify` performs discovery only and reports verification results as `skip` per referrer.
- If a user sets `level = "strict"` (or anything other than `"skip"`) in v1, OCX exits with `ConfigError` (78) and a message: "verification levels other than 'skip' are not implemented in OCX v1; use `ocx verify` for discovery-only; track v2 support at issue <TBD>." This deliberate error makes the surface non-foreclosing: scripts that set `level = "strict"` will break loudly until v2 lands, not silently succeed.
- If `--trust-policy <PATH>` is explicitly provided and the file does not exist, OCX exits with `NotFound` (79) per the exit-code precedence rule above — the user requested a specific file, so absence is "data not found" rather than "config misshaped".
- Additional unknown fields in `[policies.signature_verification]` are accepted by the parser with a stderr warning so v1 scripts do not hard-fail if users accidentally pre-write v2-shaped fields. They carry no v1 semantics.

`--trust-policy <PATH>` is wired on both `ocx verify` and `ocx sbom` in v1 to avoid a future breaking flag addition.

### Referrer Cache Layout

Extend the existing file-structure layout (parent ADR §Storage):

```
~/.ocx/
├── blobs/{registry_slug}/{algorithm}/{2hex}/{30hex}/
│   └── data              (existing — individual referrer manifest blobs land here, content-addressed)
│
├── referrers/{registry_slug}/{repo_path_slug}/{subject_digest_slug}.json
│   # Cached Referrers API response body (raw OCI ImageIndex JSON).
│   # Subject-digest-keyed, NOT content-addressed (mutable: new referrers can be added).
│   # Default TTL: 1h in interactive context, 24h in CI (heuristic: stdin is a TTY).
│   # Busted on every `--no-cache` invocation.
│
├── referrers/{registry_slug}/{repo_path_slug}/{subject_digest_slug}.meta.json
│   # { "fetched_at": ISO-8601, "method": "referrers-api"|"referrers-tag",
│   #   "ttl_expires_at": ISO-8601, "source_http_status": 200,
│   #   "oci_filters_applied": bool }
│   # Used to decide whether to re-probe the API, replay the fallback tag, or hit the cache.
│
└── referrers/capabilities/{registry_slug}.json
    # { "supports_referrers_api": bool, "probed_at": ISO-8601, "probe_ttl_days": 7 }
    # Per-registry capability cache — avoids probing the API on every invocation.
```

Rules:

1. Referrer index cached by subject digest, not content digest (the list is mutable).
2. Individual referrer manifest blobs flow through existing `blobs/` (content-addressed, indefinite cache).
3. Registry-capability probe cached for 7 days. `--no-cache` re-probes.
4. The `OCI-Filters-Applied` header is stored in `.meta.json` so re-filter decisions persist across invocations (even though v1 always re-filters client-side — see Invariants).

### Error Taxonomy

Expanded `ClientError` variants (Round 3 Codex pass — was `ReferrersUnsupported` only; Finding 4 widened for the 401/403/429/5xx exit-code matrix):

```rust
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum ClientError {
    // ... existing variants (Authentication, DigestMismatch, ManifestNotFound, BlobNotFound, Registry, Io, ...)

    #[error("referrers API not supported by registry {registry}")]
    ReferrersUnsupported { registry: String },

    #[error("unauthorized: {0}")]
    Unauthorized(String),   // distinct from Authentication (pre-flight) — this is a mid-request 401

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("rate limited by registry{retry_after_suffix}")]
    RateLimited { retry_after: Option<u64> },

    #[error("registry service unavailable ({status}): {reason}")]
    ServiceUnavailable { status: u16, reason: String },
}
```

`ClassifyExitCode` mapping additions:
- `ReferrersUnsupported` → `Unavailable` (69). Fires only when `--distribution-spec v1.1-referrers-api` is explicitly set and the registry returns 404/405. On default auto-probe, the code path transparently falls back to the tag scheme — no error surfaced.
- `Unauthorized` → `AuthError` (80). Distinguished from the pre-existing `Authentication` variant which represents local auth-handshake failure before the request lands.
- `Forbidden` → `PermissionDenied` (77).
- `RateLimited` → `TempFail` (75). `retry_after` parsed from the `Retry-After` header; CLI may display in error message.
- `ServiceUnavailable` → `Unavailable` (69).

Library-side discovery error (expanded in Round 3 per Codex Finding 5 — trust-policy errors split by cause):

```rust
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum ReferrerDiscoveryError {
    #[error("{0}")]
    Client(#[from] ClientError),

    #[error("malformed referrer index for {subject}: {reason}")]
    MalformedIndex { subject: String, reason: String },

    #[error("require check failed: {0}")]
    RequireUnmet(String),

    #[error("verification level {level:?} not supported in OCX v1; use 'skip'")]
    UnsupportedVerificationLevel { level: String },

    #[error("trust policy version {version:?} not supported in OCX v1; use version = \"1\"")]
    UnsupportedTrustPolicyVersion { version: String },

    #[error("failed to parse trust policy {path}: {source}")]
    TrustPolicyParse { path: PathBuf, #[source] source: toml::de::Error },

    #[error("trust policy file not found: {path}")]
    TrustPolicyNotFound { path: PathBuf },

    #[error("trust policy I/O error for {path}: {source}")]
    TrustPolicyIo { path: PathBuf, #[source] source: std::io::Error },

    #[error("referrer cache I/O error for {path}: {source}")]
    CacheIo { path: PathBuf, #[source] source: std::io::Error },

    #[error("network operation blocked by offline mode: {reason}")]
    Offline { reason: String },

    #[error("classification error: {0}")]
    Classify(String),

    #[error("signature verification error: {0}")]
    Verify(String),   // v1: unreachable; only fires when level != Skip, which v1 rejects
}
```

`ClassifyExitCode` mapping:
- `Client` → defers to underlying `ClientError::classify()`
- `MalformedIndex` → `DataError` (65)
- `RequireUnmet` → `DataError` (65)
- `UnsupportedVerificationLevel` → `ConfigError` (78)
- `UnsupportedTrustPolicyVersion` → `ConfigError` (78)
- `TrustPolicyParse` → `ConfigError` (78) — distinct from I/O (malformed TOML vs. missing file)
- `TrustPolicyNotFound` → `NotFound` (79) — explicit `--trust-policy <PATH>` missing (locks Q-PRD-3)
- `TrustPolicyIo` → `IoError` (74) — permission-denied and other I/O; distinct from NotFound
- `CacheIo` → `IoError` (74)
- `Offline` → `OfflineBlocked` (81)
- `Classify` → `DataError` (65)
- `Verify` → `DataError` (65)

### Invariants

1. **`OCI-Filters-Applied` is advisory.** Even if the header is present, the client re-applies the `artifactType` filter on the returned list. (Multiple production registries return unfiltered data regardless.)
2. **Digest required for referrers query.** `Client::list_referrers` takes `&PinnedIdentifier`, not `&Identifier`. The type system prevents tag-only queries from reaching the transport layer.
3. **Per-platform descent is mandatory.** If the input reference resolves to an `ImageIndex`, OCX selects the per-platform `ImageManifest` digest and queries referrers against that digest. Querying against an `ImageIndex` digest always returns empty.
4. **Signature class is determined by layer `mediaType`, not `artifactType`.** The referrer's `artifactType` indicates the kind of referrer (signature/SBOM/custom); the layer inside indicates the format (cosign-bundle, in-toto, JWS). This is why the forward-compatibility plan in Decision B keys off layer mediaType.
5. **Structured stdout, diagnostics on stderr. Never interleave.** Non-negotiable with a single explicit carve-out: when `ocx sbom --download -` is specified, raw SBOM bytes replace the structured report on stdout entirely — the JSON/text report is not emitted at all (not to stdout, not to stderr). Stderr carries only diagnostics (warnings, trace logs) as usual, and stderr is empty on the happy path.
6. **`schema_version: 1` at root of every JSON output.** Non-negotiable. Does not apply in the `--download -` carve-out where no report is emitted.
7. **Fallback classify loop is bounded.** On the fallback-tag path (Decision C2), the defensive per-manifest classify loop processes at most `MAX_FALLBACK_MANIFESTS_TO_CLASSIFY = 20` descriptors. Descriptors above the cap are classified as `Unknown { reason: "fallback index exceeds classification limit; use --distribution-spec v1.1-referrers-api on a supported registry" }` and a warning is emitted on stderr. The cap protects against pathological tag indexes; the Referrers API path has no cap because it supports pagination (deferred to v2).

## Consequences

### Positive

- OCX ships its first supply-chain feature with a clean CLI surface, stable JSON schema, and typed exit codes — ready for GitHub Actions and Bazel integration from v1.
- Compatibility with GHCR and Docker Hub through read-side fallback means OCX works for the majority of open-source package traffic on day one.
- Cosign keyless verification (the dominant model in OCX's target user base) gets a native, pure-Rust implementation with no subprocess calls and offline capability via embedded TUF root.
- Trust-policy schema wired in v1 (even as a stub) avoids a future breaking flag addition when v2 enforcement lands.
- Subsystem boundaries preserved: all registry discovery lives in `ocx_lib::oci`, all CLI glue in `ocx_cli::command::{verify,sbom}`, no CLI concerns leak into the library.
- Contract-first TDD with stubs + reviewer gate before tests is a strong fit for this well-specified surface; acceptance tests against registry:2 + cosign fixtures lock in observable behavior.

### Negative

- `sigstore` crate v0.13.0 is pre-1.0 and has shown 10 breaking changes in 19 releases. OCX pins `=0.13` and accepts a deliberate upgrade cycle.
- DSSE attestation verification is deferred to a future version. Users wanting attestation verification must call `cosign verify-attestation` out-of-band. Documented in README and in `ocx verify --help`.
- Notation users get discovery only (the referrer appears in JSON output with `verification: { attempted: false, reason: "notation format not supported in v1" }`). Verification requires `notation verify` out-of-band. Documented.
- The defensive per-manifest fetch on the fallback path adds N round trips (N = fallback tag index size, bounded to `MAX_FALLBACK_MANIFESTS_TO_CLASSIFY = 20` by Invariant 7; typically 1–5 in practice). Acceptable for a discovery command; cached per invocation.
- Asymmetric push/read fallback stance adds documentation cost. Mitigated by a dedicated user-guide section.
- **GitHub Attestations API is not discovered.** `actions/attest-build-provenance` (GitHub Actions workflow attestation) stores provenance in GHCR via GitHub's proprietary Attestations REST API (`GET /repos/{owner}/{repo}/attestations/{subject_digest}`, versioned 2026-03-10), NOT as OCI referrers or fallback tags. `ocx verify` against a GHA-attested GHCR image will return `referrer_count: 0` even when GitHub-native provenance exists. Users should call `gh attestation verify` separately for GHA-attested packages; v2 may add native discovery of this REST endpoint under a new flag.

### Risks

| Risk | Mitigation |
|---|---|
| `sigstore` crate breaking API in a minor update blocks OCX security patches | Pin `=0.13.x`; acceptance tests cover verification; deliberate bump as a dedicated PR |
| Cosign bug #4641 gets fixed upstream; OCX's defensive parse becomes pure overhead | Defensive parse remains correct post-fix; overhead is 1–5 manifest fetches per invocation (bounded); acceptable cost for correctness over a multi-year transition |
| GHCR adds Referrers API support and OCX keeps probing on every invocation | 7-day per-registry capability cache (see §Cache Layout); auto-detected on probe expiry |
| Trust-policy schema v1 turns out to differ from the v2 shape Notation uses | Schema is explicitly marked "v1" and the architecture reserves the right to bump version numbers; `version = "1"` field in the TOML allows v2 to be gated on a parallel parser |
| User mis-reads "skip" verification level as "verified" | Text output prints `Verification: skip (trust-policy level 'skip' — discovery only in v1)` verbatim; JSON has `result: "skip"` with explicit `reason`; user-guide section calls it out |
| Rate-limit hits on Docker Hub anonymous pulls (10/hr) during CI | 24h cache TTL in non-interactive context; `--no-cache` as escape hatch; 429 handled with `Retry-After` backoff |
| Offline mode semantics for `ocx verify` unclear | `--offline` fails with `OfflineBlocked` (81) unless a valid cached referrer index exists within TTL; documented in user guide |
| ECR push bug #2783 (405 on `oras copy -r` with OCI 1.1 referrer manifests, observed March 2026) | Affects push/copy (cross-registry referrer copy), NOT the read path implemented here. `ocx verify` / `ocx sbom` against ECR is confirmed unaffected. Note captured here for visibility when the write-side plan lands. |

## Technical Details

### Architecture

```
                    ┌────────────────────────────────────────────────────┐
                    │                    ocx_cli                         │
                    │                                                    │
      user ────────▶│  command/verify.rs                                 │
                    │  command/sbom.rs                                   │
                    │       │                                            │
                    │       ▼                                            │
                    │  api/data/verify.rs   (Printable + Serialize)      │
                    │  api/data/sbom.rs     (Printable + Serialize)      │
                    └───────│────────────────────────────────────────────┘
                            │ calls context.referrer_discovery()
                            ▼
                    ┌────────────────────────────────────────────────────┐
                    │                    ocx_lib                         │
                    │                                                    │
                    │  referrer/discovery.rs   ReferrerDiscovery facade  │
                    │  referrer/trust_policy.rs TrustPolicy loader       │
                    │  referrer/cache.rs       Referrer index cache      │
                    │  referrer/fallback.rs    Tag-scheme fallback       │
                    │  referrer/classify.rs    Layer mediaType classify  │
                    │      │                                             │
                    │      ▼                                             │
                    │  oci::Client::list_referrers                       │
                    │      (new public method)                           │
                    │      │                                             │
                    │      ▼                                             │
                    │  OciTransport::list_referrers (new trait method)   │
                    │      │                                             │
                    │      └──▶ NativeTransport ─▶ oci_client::Client    │
                    │                                ::pull_referrers    │
                    │                                (already upstream)  │
                    │                                                    │
                    │  file_structure::ReferrerStore (new)               │
                    │      ~/.ocx/referrers/{registry}/{repo}/           │
                    └────────────────────────────────────────────────────┘
```

### API Contract (library)

**Context wiring.** `ReferrerDiscovery::new` is initialized with a reference to the `oci::Client` held by `Context`, NOT a freshly constructed client. A new accessor `Context::referrer_discovery()` is added that passes the existing shared client through; `ReferrerDiscovery` must not call any `Client::builder()`-shaped constructor. This preserves the single auth session / token cache used by `PackageManager` and aligns with the MEMORY.md cache-coherence refactor ("audit remaining call sites that use `context.remote_client()` directly instead of `default_index`") — `referrer_discovery()` is a new call site that follows the correct pattern from day one.

```rust
// crates/ocx_lib/src/referrer/discovery.rs
pub struct ReferrerDiscovery {
    client: oci::Client,     // shared from Context; NOT a fresh builder
    index: oci::Index,       // shared default_index from Context — routes through ChainMode
    store: ReferrerStore,
    policy: TrustPolicy,
    mode: DiscoveryMode,   // honors ChainMode from --offline / --remote
}

impl ReferrerDiscovery {
    // Takes BOTH the shared client and the shared default_index from Context.
    // Split of concerns (locked by Round 3 Codex pass):
    //   - Tag resolution + per-platform descent  → index.select / index.fetch_manifest
    //     (honors ChainMode; cache-coherent with PackageManager)
    //   - Digest-addressed referrers / fallback tag / blob reads → client
    //     (these are not OCX package manifests; the Index layer does not cache them yet)
    pub fn new(
        client: oci::Client,
        index: oci::Index,
        store: ReferrerStore,
        policy: TrustPolicy,
        mode: DiscoveryMode,
    ) -> Self { /* ... */ }

    pub async fn discover(
        &self,
        identifier: &oci::Identifier,
        filter: ReferrerFilter,
        platform: Option<&oci::Platform>,
    ) -> Result<ReferrerSet, ReferrerDiscoveryError>;
}

pub struct ReferrerFilter {
    pub artifact_type: Option<String>,
    pub well_known_kind: Option<WellKnownKind>,   // WellKnownKind::Signature | Sbom
    pub include_unknown: bool,
}

pub struct ReferrerSet {
    pub reference: oci::Identifier,
    pub resolved_digest: oci::Digest,
    pub subject_digest: oci::Digest,
    pub platform: oci::Platform,
    pub method: RegistryMethod,       // ReferrersApi | ReferrersTag
    pub entries: Vec<ReferrerEntry>,
}

pub struct ReferrerEntry {
    pub digest: oci::Digest,
    pub artifact_type: String,
    pub media_type: String,
    pub size: u64,
    pub annotations: BTreeMap<String, String>,
    pub classification: ReferrerClassification,   // Signature(Kind) | Sbom(Kind) | InToto | Unknown
    pub verification: VerificationOutcome,        // Skipped(reason) | Passed(details) | Failed(reason) | NotApplicable
}
```

### Data Model

Trust policy on disk (see §Trust Policy Shape). Referrer cache on disk (see §Cache Layout). In-memory data types: `ReferrerSet`, `ReferrerEntry` as above.

## Implementation Plan

Detailed plan in [`../state/plans/plan_oci_referrers_discovery.md`](../state/plans/plan_oci_referrers_discovery.md). Summary of phases:

1. **Phase 1 (Stub):** Add `OciTransport::list_referrers`, `Client::list_referrers`, new `ClientError::ReferrersUnsupported`, new `ReferrerDiscoveryError`, `ReferrerDiscovery`/`ReferrerStore`/`TrustPolicy` scaffolding, `Verify`/`Sbom` command shells, report data types. All bodies `unimplemented!()` or `Err(unimplemented)`. Gate: `cargo check` passes.
2. **Phase 2 (Architecture Review):** Reviewer verifies stubs against this ADR (spec-compliance, post-stub).
3. **Phase 3 (Specify):** Unit tests in library crate; acceptance tests in `test/tests/test_verify.py` and `test/tests/test_sbom.py` using `oras` CLI (via OCX dogfood mirror) and HTTP PUT helpers to populate registry:2 fixtures. Tests fail against stubs.
4. **Phase 4 (Implement):** Fill in stubs. Bring up the defensive-parse fallback. Wire trust-policy reader. Ship cosign-keyless classification. `task verify` passes.
5. **Phase 5 (Review-Fix Loop):** Standard canonical review loop per `workflow-swarm.md`.

## Validation

- [ ] Unit tests cover all `ReferrerClassification` branches including bug-#4641 defensive path
- [ ] Acceptance test exists for: happy API path, happy fallback-tag path, empty referrers, malformed referrer index, `--require*` unmet, registry 401/403/404/5xx, offline-blocked, cross-platform query
- [ ] Acceptance test specifically populates a fallback tag with deliberately-wrong `artifactType` on index descriptors (reproduces bug #4641) and verifies OCX classifies correctly
- [ ] Trust-policy v1 stub honored: `level = "skip"` passes, other levels return ConfigError (78)
- [ ] JSON output has `schema_version: 1` in every payload shape (including empty and error-like states)
- [ ] Documentation updated: user-guide section on signatures/SBOMs, split push/read fallback stance, v2 extension points noted

## Links

- Parent: [`adr_oci_artifact_enrichment.md`](./adr_oci_artifact_enrichment.md) — hybrid enrichment mechanism; read side is this ADR, write side remains there
- Adjacent: [`adr_sbom_strategy.md`](./adr_sbom_strategy.md) — OCX-side SBOM generation (cargo-auditable + cargo-cyclonedx at release); frames what SBOM formats `ocx sbom` will encounter
- Research: [`research_cosign_sigstore_notation.md`](./research_cosign_sigstore_notation.md), [`research_verify_cli_patterns.md`](./research_verify_cli_patterns.md), [`research_oci_referrers_2026.md`](./research_oci_referrers_2026.md)
- Discover: [`discover_referrers_architecture_map.md`](./discover_referrers_architecture_map.md), [`discover_oci_client_extension_points.md`](./discover_oci_client_extension_points.md), [`discover_cli_command_conventions.md`](./discover_cli_command_conventions.md), [`discover_test_fixture_referrers.md`](./discover_test_fixture_referrers.md)
- Issue: [#24](https://github.com/ocx-sh/ocx/issues/24)
- External: [OCI Distribution Spec 1.1](https://github.com/opencontainers/distribution-spec/blob/main/spec.md), [sigstore-rs](https://github.com/sigstore/sigstore-rs), [cosign bug #4641](https://github.com/sigstore/cosign/issues/4641)

---

## Round 2 Decisions

This section records adjudications applied after Round 1 of the review panel (spec-compliance, architect trade-off, researcher SOTA). Items listed here are either (a) fixes where two reviewers proposed different remedies and the narrower option was chosen, or (b) contract decisions promoted out of the PRD Open Questions list.

| Ref | Issue | Adjudication | Rationale |
|-----|-------|--------------|-----------|
| Q-PRD-3 | `--trust-policy <PATH>` given but file missing — exit 78 or 79? | **79 (NotFound)** | Spec-compliance reviewer recommended 78 (ConfigError); architect reviewer recommended 79 (NotFound) on the reasoning that a user-supplied path-not-present is data-not-found rather than config-shape. The task prompt instructed to follow the architect recommendation. The rule also generalizes cleanly: 78 stays reserved for "file exists but is malformed / uses unsupported level". |
| Q-PRD-7 | Registry 5xx + `--require-referrers` — exit 69 or 65? | **69 (Unavailable)** | Both reviewers agreed (transport errors preempt require-check failures). Promoted from Open Question to Exit-code precedence rule in §CLI Contracts. |
| `--download -` output routing | Spec-compliance said route JSON to stderr if `--format json`; architect said stderr must be empty | **Raw bytes to stdout, JSON/text report not emitted at all, stderr empty on happy path** | Architect position is stricter and eliminates the "carried structured data on stderr" invariant violation. Task prompt endorsed this option. Recorded as Invariant 5 carve-out. |
| `classification` field | Nested object (ADR full example) vs. flat string (PR-FAQ mockup) | **Flat discriminant string** (`signature:cosign-bundle-v03` etc.) | Spec-compliance flagged the mismatch; the narrower option is to collapse to a flat string (matches PR-FAQ mockup and is simpler to version). Full enum of v1 values pinned in §JSON Output Schema. `verification` remains a nested object. |
| `cyclonedx-bom` dependency | Spec-compliance noted it lacks v1 tests; architect said it's an unspent innovation token in v1 | **Remove from v1 dep set** | Architect remedy is narrower and actionable. Plan updated to drop the dependency; the `SbomFormat::CycloneDxJson` enum variant remains for classification without requiring the crate. v2 adds it when parsing lands. |
| Trust-policy TOML shape | Spec-compliance accepted the Notation-shaped fields; architect said they create a semantic lie for cosign keyless | **Drop `trust_stores` and `trusted_identities` from v1 schema; only `version`, `registry_scopes`, and `level = "skip"` remain** | Architect fix is narrower and more honest about what v1 enforces. v2 field vocabulary stays open so it can match the actual signature format being verified. |
| Bounded fallback classify | Architect flagged unbounded round-trip cost on pathological fallback indexes | **Introduce `MAX_FALLBACK_MANIFESTS_TO_CLASSIFY = 20` cap with warning** | Added as new Invariant 7; plan's Phase 4 Step 4.5 updated. |
| `ReferrerDiscovery` client wiring | Architect flagged hidden duplicate-auth risk | **Explicit note in §API Contract that `ReferrerDiscovery::new` takes the shared `Client` from `Context`, never constructs its own** | Ties to MEMORY.md cache-coherence refactor as a related change. |
| Static cosign bundle validity window | Architect flagged that Fulcio-issued certs expire in ~10 minutes, so committed vectors go stale | **Test fixture uses `sigstore-rs` test-mode flag to skip certificate-validity-window checks (`verify_with_no_certificate_check` or equivalent)** | Alternative "private Fulcio in docker-compose" is heavier; deferred as an infrastructure-investment candidate for v2 hardening. Surfaced in Deferred Findings in the plan. |

All Round 1 actionable findings other than the adjudications above were applied directly; see artifact diffs. Deferred findings are captured in the plan's "Deferred Findings for Handoff" section.

---

## Round 3 Decisions (Codex Cross-Model Pass)

This section records adjudications applied after the Codex cross-model review of the plan artifact (2026-04-19). Codex returned FAIL with 9 Actionable findings; all 9 were applied. Items below record (a) adjudications where Codex and the Claude family reviews disagreed and the narrower/more-executable option won, (b) judgment calls inside individual findings where the plan took an explicit stance, and (c) places where the ADR itself had to be widened to keep plan + ADR in sync.

| Ref | Issue | Adjudication | Rationale |
|-----|-------|--------------|-----------|
| Codex #1 (repo-shape drift) | Claude review passed the plan with paths referring to `oci/error.rs`, `oci/transport/mod.rs`, `command/mod.rs`, `api/data/mod.rs`, free `run(ctx, args)` fns, and `main.rs` dispatch. Codex flagged this as executable-fit drift. | **Rewrite to current tree.** Plan now uses `oci/client/{error,transport,native_transport}.rs`; `crates/ocx_cli/src/command.rs` + `api/data.rs` (flat aggregators, NOT `mod.rs`); `Args`-derived struct with its own `execute(&self, context)` method per `command/install.rs` pattern; dispatch in `App::run` → `Command::execute`; `main.rs` untouched by this feature. Classification lives in `ocx_lib::cli::classify` via `try_downcast!` macro — there is no `ocx_cli/classify.rs`, and new `ClassifyExitCode` impls live on the error types themselves. | Codex's family caught a blind spot that Claude's same-family review rationalized as "implementation detail": flat-aggregator naming and command-struct pattern are load-bearing for the builder. |
| Codex #2 (`Client::resolve_reference` is fictional) | Plan's Step 4.8 algorithm invoked `Client::resolve_reference` for tag→digest resolution; this method does not exist in the codebase. The same step only injected `Client` into `ReferrerDiscovery`, bypassing `default_index` and `ChainMode`. | **Inject both `Client` AND `Index`; split concerns.** Tag resolution + per-platform descent use `Index::select` + `Index::fetch_manifest` (routes through `ChainMode`, cache-coherent with `PackageManager`). Digest-addressed referrers-API / fallback-tag / blob reads use `Client`. ADR §API Contract → `ReferrerDiscovery::new` signature updated to take `index: oci::Index` alongside `client`. | Cache coherence was a known pending refactor (MEMORY.md); wiring this new call site correctly from day one means the refactor can treat it as clean. |
| Codex #3 (cache/offline contract undefined) | Plan said "offline fails with 81" and "`--no-cache` bypasses cache" but never specified TTL policy, read-before-network order, offline-cache-hit behavior, or whether `--no-cache` covers the capability-probe cache as well. | **Explicit algorithm in Step 4.8.** TTL = 1h interactive / 24h CI (heuristic: `stdin.is_terminal()`); capability-probe TTL = 7 days unconditionally. `--no-cache` bypasses **both** reads (referrer-index AND capability-probe) but does NOT disable writes. Offline + cache-hit-within-TTL = exit 0; offline + miss-or-expired = `Offline` error → exit 81. Offline + `--no-cache` = UsageError (64) via clap mutual-exclusion. ADR §Cache Layout already had the 1h/24h / 7-day numbers; Round 3 makes them authoritative and wires them into the plan's tests. | Codex is right that contracts not asserted by tests drift; adding these to the contract makes them builder-safe. |
| Codex #4 (error-taxonomy vs. exit-code matrix) | Plan promised distinct 401/403/429/5xx exit codes but `ClientError` only added `ReferrersUnsupported`. The existing catch-all `Registry(_) → Unavailable` could not satisfy 80/77/75/69 separately. | **Expand `ClientError` with `Unauthorized`, `Forbidden`, `RateLimited { retry_after }`, `ServiceUnavailable { status, reason }`.** Existing `Authentication` variant kept distinct — it represents local auth-handshake failure (pre-flight), whereas `Unauthorized` is a mid-request 401. ADR §Error Taxonomy widened. | Required for the exit-code matrix tests to be runnable. |
| Codex #5 (trust-policy error routing) | Plan routed TOML parse failures to `TrustPolicyIo(std::io::Error)`, conflating missing file (79), malformed TOML (78), and permission-denied I/O (74). | **Split into `TrustPolicyParse`, `TrustPolicyNotFound`, `TrustPolicyIo`, `UnsupportedTrustPolicyVersion`.** Separate `ClassifyExitCode` mappings per variant. Plan test list updated: `load_explicit_path_missing_errors_not_found_79` / `load_invalid_toml_errors` / etc. ADR §Error Taxonomy widened. | Q-PRD-3 demands 79 for path-missing; can't honor that without a dedicated variant. |
| Codex #6 (compile-fail test mechanism) | Plan specified `list_referrers_pinned_identifier_required_type_level` as inline `#[cfg(test)]` — not enforceable by `cargo nextest`, which only runs tests that compile successfully. | **Use `trybuild` UI-test harness.** `crates/ocx_lib/tests/ui.rs` drives `rustc` against `tests/ui/list_referrers_rejects_identifier.rs` and compares stderr to `.stderr` reference file. `trybuild = "1"` added as dev-dependency. Downgrade fallback: if rustc message churn breaks the `.stderr` reference file frequently on nightly, downgrade to architecture-review-only (Phase 2 reviewer scans the signature). | Coverage-by-test-name is not coverage-by-runnable-mechanism. |
| Codex #7 (undefined fixtures) | Plan referenced 401/5xx/malformed-index/spoofed-`OCI-Filters-Applied` test scenarios without defining how the existing `registry:2` harness would produce those responses. | **Concrete designs** for each: NGINX reverse proxy under docker-compose `profiles:` for 401/5xx (returning the required status on `/v2/*/referrers/*` only); raw `requests.put` helper for `malformed_referrer_index` with two seeded scenarios (missing `manifests` field, `digest: "banana"`); NGINX response-header modifier for the spoofed-filter scenario with a unit-test fallback if the NGINX approach proves infeasible. Fallback path for each fixture recorded in the test's docstring so graceful degradation is visible to reviewers. | Infrastructure-investment cost limited to the NGINX configs + docker-compose profile entries; bounded and reviewable. |
| Codex #8 (`cosign sign` live dependency) | `signed_package` fixture called `cosign sign --registry=...` at test time while the plan simultaneously said CI avoids live Fulcio/Rekor. Irreconcilable as written. | **Static cosign bundle + `oras attach`.** No `cosign sign` invocation at test time. Static vectors checked into `test/fixtures/cosign/<name>/`; regeneration is a one-time human-run step documented in `test/fixtures/cosign/scripts/regenerate.sh` (not CI-invoked). `sigstore-rs` test-mode flag (`verify_with_no_certificate_check` or equivalent) gated to `#[cfg(test)]` skips the certificate-validity-window check so static vectors do not age out. Includes an expired-cert vector for the error-path test. Alternative "live Fulcio in docker-compose" remains tracked as DF-16. | Finding 8 was a contradiction between two plan statements; static-vector resolution is consistent with both. |
| Codex #9 (JSON error-reporting mechanism) | Plan required `schema_version` on every JSON branch (including errors), but the existing main-line error path (`classify_error` + stderr + exit) emits nothing to stdout. No specified mechanism to satisfy the assertions. | **Command-level JSON error envelope DTO.** New `VerifyErrorReport { schema_version, reference, error: VerifyErrorEnvelope { kind, message, exit_code } }` + `SbomErrorReport` emitted from `command::execute` when `context.api().is_json()` is true. Command catches `ReferrerDiscoveryError`, routes through `Api::report`, returns the `ExitCode` directly without involving `main.rs`. `--format plain` path bubbles to `main` unchanged. `main.rs` stays untouched. `--download -` carve-out suppresses both success and error envelopes (Invariant 5). | Keeps `main.rs` simple (routing / logging only), keeps the JSON contract deterministic. |

### Items the Codex review got right but the plan's prior rationale was non-trivial — recorded for audit

- Codex Finding 1 (repo-shape): plan's original "plan reads as if repo structure is `command/mod.rs` etc." was a direct consequence of the Claude reviewer family's bias toward pattern-book layouts. The actual tree has used flat aggregator files (`command.rs` with sibling `command/X.rs`) since commit `9a17461` (2026-04-19) as part of the config-layering work. No prior rationale opposes the correction.
- Codex Finding 2 (`Index` injection): the prior plan rationale that "`Client::resolve_reference` is a convenience wrapper" was a hallucination; no such method exists. The Index-first pattern was already documented in `subsystem-oci.md`'s "Cache coherence issue" gotcha, so the correction aligns the plan with long-standing intent.
- Codex Finding 3 (cache contract): the ADR already had the 1h/24h/7-day numbers under §Cache Layout. The plan's prior lack of an algorithm step for them was an oversight, not a rejection — Round 3 simply promotes the numbers from "documented" to "tested".

### Items where the plan took a judgment call inside a Codex-actionable fix

- **Finding 6 downgrade fallback.** `trybuild` is the right mechanism but introduces a dependency that can break on rustc nightly when error messages change. Plan records an explicit downgrade fallback (architecture-review-only) to be taken only if maintenance cost proves prohibitive in practice. Not a pre-decision — a documented escape hatch.
- **Finding 7 spoofed-header fallback.** If the NGINX response-header modifier approach proves unreliable (some NGINX versions reject modifying response headers for proxied 200s), the scenario becomes a library-level unit test using a stubbed HTTP layer. The acceptance-suite slot is then freed. This is recorded in the test's fallback docstring so reviewers see the backup path without hunting.
- **Finding 8 live-Fulcio option.** DF-16 already carried this as infrastructure investment. Round 3 confirms static-vector is v1; live Fulcio remains v2-hardening candidate, not an unshipped prerequisite.

### Items the Codex review did NOT prompt but were tightened opportunistically

- Plan §Files to Modify now explicitly marks `main.rs` as **Untouched** to prevent a builder from editing it out of habit.
- Plan §Files to Modify now lists the `trybuild` setup files (`tests/ui.rs`, `tests/ui/*.rs`, `tests/ui/*.stderr`), the NGINX fixture configs (`test/fixtures/nginx/`), and the cosign regenerator script (`test/fixtures/cosign/scripts/regenerate.sh`) so the builder knows the full surface area.

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-19 | mherwig (via architect worker) | Initial draft synthesizing Discover + Research phases |
| 2026-04-19 | architect (Round 2 fixes) | Applied Round 1 review findings: `--format json` clarified as global flag; exit-code precedence rule; `--require-*` combination semantics; `--require-artifact-type ""` + `--min-referrers 0` + `--trust-policy <PATH>` missing-file behavior specified; `--download -` re-grounded on Invariant 5; `MAX_FALLBACK_MANIFESTS_TO_CLASSIFY` cap added as Invariant 7; trust-policy v1 schema narrowed to `level = "skip"` only; `ReferrerDiscovery` wiring explicitly anchored to shared `Context` client; DSSE classification added to Decision B forward-compat table; GitHub Attestations limitation surfaced; ECR bug #2783 noted; empty `ocx sbom` JSON shape specified; `classification` field formally typed as flat string; patterns-axis research footnote; sigstore-rs v0.13.0 date corrected. See Round 2 Decisions table above. |
| 2026-04-19 | architect (Round 3 Codex cross-model pass) | Applied all 9 Codex actionable findings: §API Contract `ReferrerDiscovery::new` signature widened to take both `client` and `index` with split-of-concerns comment; §Error Taxonomy expanded `ClientError` (added `Unauthorized`/`Forbidden`/`RateLimited`/`ServiceUnavailable`) and `ReferrerDiscoveryError` (split trust-policy errors into `TrustPolicyParse`/`TrustPolicyNotFound`/`TrustPolicyIo`/`UnsupportedTrustPolicyVersion`; added `CacheIo`, `Offline`, `Classify`, `Verify`) with full `ClassifyExitCode` mappings. Added Round 3 Decisions section recording all 9 adjudications, audit notes, and judgment calls. DF-CODEX-1 added to plan's Deferred Findings for the v1 trust-policy scope contradiction (sigstore-crate dep present, verify branch unreachable). |
